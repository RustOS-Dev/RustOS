/*
 * jsd: the JavaScript helper of the `browse` web browser.
 *
 * One jsd process runs the scripts of one page. It talks to the browser
 * over its stdin/stdout: one JSON message per line. The page's DOM lives
 * here (implemented in JavaScript, lib/*.js, embedded at build time as
 * jsd_lib.h); the browser keeps a copy that it lays out, updated from the
 * mutation batches jsd sends after every task. Networking, cookies,
 * storage and layout queries are requests to the browser.
 *
 * Page scripts get no file or process access: the QuickJS std/os modules
 * are not loaded. The host exposes only `__host`:
 *   send(line)        write one message line to the browser
 *   recv()            block for the next line from the browser
 *   now()             monotonic milliseconds
 *   setTimer(id, ms)  call __onTimer(id) after ms (replaces a timer id)
 *   nextTimer()       ms until the next timer is due, -1 for none
 *   clearTimer(id)
 *   random()          a random 32-bit integer (for crypto.getRandomValues)
 *   evalScript(code, url)          run a classic script
 *   evalModule(code, url)          run a module (imports via __resolve /
 *                                  __loadModule in JS)
 *   log(str)          write to stderr (debugging)
 */
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/random.h>
#include <time.h>
#include <unistd.h>

#include "quickjs.h"
#include "jsd_lib.h"

#define MAX_TIMERS 4096
#define SCRIPT_BUDGET_MS 10000

static JSRuntime *rt;
static JSContext *ctx;

struct timer {
    int64_t id;
    double due;
};
static struct timer timers[MAX_TIMERS];
static int ntimers;

static double deadline; /* interrupt scripts running past this (ms) */

static double now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

/* ---- line I/O ---------------------------------------------------------- */

static char *inbuf;
static size_t inlen, incap;
static int in_eof;

/* Read more input; returns 0 at EOF. */
static int fill(void)
{
    if (incap - inlen < 65536) {
        incap = incap ? incap * 2 : 262144;
        inbuf = realloc(inbuf, incap);
        if (!inbuf)
            exit(3);
    }
    ssize_t n;
    do {
        n = read(0, inbuf + inlen, incap - inlen);
    } while (n < 0 && errno == EINTR);
    if (n <= 0) {
        in_eof = 1;
        return 0;
    }
    inlen += n;
    return 1;
}

/* Next complete line (without the newline), malloc'd; NULL if none yet. */
static char *take_line(void)
{
    char *nl = memchr(inbuf, '\n', inlen);
    if (!nl)
        return NULL;
    size_t len = nl - inbuf;
    char *line = malloc(len + 1);
    memcpy(line, inbuf, len);
    line[len] = 0;
    memmove(inbuf, nl + 1, inlen - len - 1);
    inlen -= len + 1;
    return line;
}

static int take_line_available(void)
{
    return inlen > 0 && memchr(inbuf, '\n', inlen) != NULL;
}

static void write_all(int fd, const char *p, size_t n)
{
    while (n > 0) {
        ssize_t w = write(fd, p, n);
        if (w < 0) {
            if (errno == EINTR)
                continue;
            exit(0); /* the browser went away */
        }
        p += w;
        n -= w;
    }
}

/* ---- host functions ---------------------------------------------------- */

static JSValue h_send(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    size_t len;
    const char *s = JS_ToCStringLen(c, &len, argv[0]);
    if (!s)
        return JS_EXCEPTION;
    write_all(1, s, len);
    write_all(1, "\n", 1);
    JS_FreeCString(c, s);
    return JS_UNDEFINED;
}

static JSValue h_recv(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    for (;;) {
        char *line = take_line();
        if (line) {
            JSValue v = JS_NewString(c, line);
            free(line);
            return v;
        }
        if (in_eof || !fill())
            exit(0);
    }
}

static JSValue h_now(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    return JS_NewFloat64(c, now_ms());
}

static JSValue h_set_timer(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    int64_t id;
    double ms;
    if (JS_ToInt64(c, &id, argv[0]) || JS_ToFloat64(c, &ms, argv[1]))
        return JS_EXCEPTION;
    if (ms < 0 || ms != ms)
        ms = 0;
    for (int i = 0; i < ntimers; i++) {
        if (timers[i].id == id) {
            timers[i].due = now_ms() + ms;
            return JS_UNDEFINED;
        }
    }
    if (ntimers < MAX_TIMERS) {
        timers[ntimers].id = id;
        timers[ntimers].due = now_ms() + ms;
        ntimers++;
    }
    return JS_UNDEFINED;
}

static JSValue h_clear_timer(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    int64_t id;
    if (JS_ToInt64(c, &id, argv[0]))
        return JS_EXCEPTION;
    for (int i = 0; i < ntimers; i++) {
        if (timers[i].id == id) {
            timers[i] = timers[--ntimers];
            break;
        }
    }
    return JS_UNDEFINED;
}

/* nextTimer(): milliseconds until the next timer is due, or -1 if none. */
static JSValue h_next_timer(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    if (ntimers == 0)
        return JS_NewInt64(c, -1);
    double next = timers[0].due;
    for (int i = 1; i < ntimers; i++) {
        if (timers[i].due < next)
            next = timers[i].due;
    }
    double left = next - now_ms();
    return JS_NewInt64(c, left > 0 ? (int64_t)left : 0);
}

static JSValue h_random(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    uint32_t r = 0;
    if (getrandom(&r, sizeof r, 0) != sizeof r)
        r = (uint32_t)(now_ms() * 7919.0);
    return JS_NewUint32(c, r);
}

static JSValue h_log(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    const char *s = JS_ToCString(c, argv[0]);
    if (s) {
        fprintf(stderr, "jsd: %s\n", s);
        JS_FreeCString(c, s);
    }
    return JS_UNDEFINED;
}

/* Report the pending exception to the page (window.onerror, console). */
static void report_exception(JSContext *c)
{
    JSValue exc = JS_GetException(c);
    JSValue global = JS_GetGlobalObject(c);
    JSValue fn = JS_GetPropertyStr(c, global, "__onError");
    if (JS_IsFunction(c, fn)) {
        JSValue r = JS_Call(c, fn, global, 1, &exc);
        if (JS_IsException(r))
            JS_FreeValue(c, JS_GetException(c));
        JS_FreeValue(c, r);
    }
    JS_FreeValue(c, fn);
    JS_FreeValue(c, global);
    JS_FreeValue(c, exc);
}

static JSValue h_eval_script(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    size_t len;
    const char *code = JS_ToCStringLen(c, &len, argv[0]);
    const char *url = JS_ToCString(c, argv[1]);
    if (!code || !url)
        return JS_EXCEPTION;
    deadline = now_ms() + SCRIPT_BUDGET_MS;
    JSValue r = JS_Eval(c, code, len, url, JS_EVAL_TYPE_GLOBAL);
    JS_FreeCString(c, code);
    JS_FreeCString(c, url);
    if (JS_IsException(r)) {
        report_exception(c);
        return JS_FALSE;
    }
    JS_FreeValue(c, r);
    return JS_TRUE;
}

static JSValue h_eval_module(JSContext *c, JSValueConst this, int argc, JSValueConst *argv)
{
    size_t len;
    const char *code = JS_ToCStringLen(c, &len, argv[0]);
    const char *url = JS_ToCString(c, argv[1]);
    if (!code || !url)
        return JS_EXCEPTION;
    deadline = now_ms() + SCRIPT_BUDGET_MS;
    JSValue r = JS_Eval(c, code, len, url, JS_EVAL_TYPE_MODULE);
    JS_FreeCString(c, code);
    JS_FreeCString(c, url);
    if (JS_IsException(r)) {
        report_exception(c);
        return JS_FALSE;
    }
    JS_FreeValue(c, r);
    return JS_TRUE;
}

/* ---- modules ----------------------------------------------------------- */

/* Resolve `name` against the importing module's URL with JS __resolve. */
static char *normalize(JSContext *c, const char *base, const char *name, void *opaque)
{
    JSValue global = JS_GetGlobalObject(c);
    JSValue fn = JS_GetPropertyStr(c, global, "__resolve");
    JSValue args[2] = { JS_NewString(c, base), JS_NewString(c, name) };
    JSValue r = JS_Call(c, fn, global, 2, args);
    JS_FreeValue(c, args[0]);
    JS_FreeValue(c, args[1]);
    JS_FreeValue(c, fn);
    JS_FreeValue(c, global);
    if (JS_IsException(r))
        return NULL;
    const char *s = JS_ToCString(c, r);
    JS_FreeValue(c, r);
    if (!s)
        return NULL;
    char *out = js_strdup(c, s);
    JS_FreeCString(c, s);
    return out;
}

/* Fetch a module's source with JS __loadModule (a request to the
   browser) and compile it. */
static JSModuleDef *load_module(JSContext *c, const char *name, void *opaque)
{
    JSValue global = JS_GetGlobalObject(c);
    JSValue fn = JS_GetPropertyStr(c, global, "__loadModule");
    JSValue arg = JS_NewString(c, name);
    JSValue src = JS_Call(c, fn, global, 1, &arg);
    JS_FreeValue(c, arg);
    JS_FreeValue(c, fn);
    JS_FreeValue(c, global);
    if (JS_IsException(src))
        return NULL;
    size_t len;
    const char *code = JS_ToCStringLen(c, &len, src);
    JS_FreeValue(c, src);
    if (!code)
        return NULL;
    JSValue f = JS_Eval(c, code, len, name, JS_EVAL_TYPE_MODULE | JS_EVAL_FLAG_COMPILE_ONLY);
    JS_FreeCString(c, code);
    if (JS_IsException(f))
        return NULL;
    JSModuleDef *m = JS_VALUE_GET_PTR(f);
    JS_FreeValue(c, f);
    return m;
}

/* ---- main loop --------------------------------------------------------- */

static int interrupt(JSRuntime *r, void *opaque)
{
    return deadline > 0 && now_ms() > deadline;
}

static void run_jobs(void)
{
    JSContext *c;
    for (;;) {
        deadline = now_ms() + SCRIPT_BUDGET_MS;
        int r = JS_ExecutePendingJob(rt, &c);
        if (r <= 0) {
            if (r < 0)
                report_exception(c);
            break;
        }
    }
}

/* Call global function `name` with one argument (string or number). */
static void call_global(const char *name, JSValue arg)
{
    JSValue global = JS_GetGlobalObject(ctx);
    JSValue fn = JS_GetPropertyStr(ctx, global, name);
    deadline = now_ms() + SCRIPT_BUDGET_MS;
    JSValue r = JS_Call(ctx, fn, global, 1, &arg);
    if (JS_IsException(r))
        report_exception(ctx);
    JS_FreeValue(ctx, r);
    JS_FreeValue(ctx, fn);
    JS_FreeValue(ctx, global);
    JS_FreeValue(ctx, arg);
    run_jobs();
    /* End of a task: send DOM mutations and other batched output. */
    global = JS_GetGlobalObject(ctx);
    fn = JS_GetPropertyStr(ctx, global, "__flush");
    r = JS_Call(ctx, fn, global, 0, NULL);
    if (JS_IsException(r))
        report_exception(ctx);
    JS_FreeValue(ctx, r);
    JS_FreeValue(ctx, fn);
    JS_FreeValue(ctx, global);
    deadline = 0;
}

static void add_fn(JSValue obj, const char *name, JSCFunction *f, int n)
{
    JS_SetPropertyStr(ctx, obj, name, JS_NewCFunction(ctx, f, name, n));
}

int main(int argc, char **argv)
{
    size_t mem = 256u << 20;
    if (argc > 1)
        mem = (size_t)atol(argv[1]) << 20;
    rt = JS_NewRuntime();
    JS_SetMemoryLimit(rt, mem);
    JS_SetMaxStackSize(rt, 1 << 20);
    JS_SetInterruptHandler(rt, interrupt, NULL);
    JS_SetModuleLoaderFunc(rt, normalize, load_module, NULL);
    ctx = JS_NewContext(rt);

    JSValue global = JS_GetGlobalObject(ctx);
    JSValue host = JS_NewObject(ctx);
    add_fn(host, "send", h_send, 1);
    add_fn(host, "recv", h_recv, 0);
    add_fn(host, "now", h_now, 0);
    add_fn(host, "setTimer", h_set_timer, 2);
    add_fn(host, "clearTimer", h_clear_timer, 1);
    add_fn(host, "nextTimer", h_next_timer, 0);
    add_fn(host, "random", h_random, 0);
    add_fn(host, "log", h_log, 1);
    add_fn(host, "evalScript", h_eval_script, 2);
    add_fn(host, "evalModule", h_eval_module, 2);
    JS_SetPropertyStr(ctx, global, "__host", host);
    JS_FreeValue(ctx, global);

    /* The DOM and web APIs. */
    JSValue r = JS_Eval(ctx, jsd_lib, sizeof(jsd_lib) - 1, "jsd:lib", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(r)) {
        JSValue e = JS_GetException(ctx);
        const char *s = JS_ToCString(ctx, e);
        fprintf(stderr, "jsd: library failed: %s\n", s ? s : "?");
        return 2;
    }
    JS_FreeValue(ctx, r);

    for (;;) {
        char *line;
        while ((line = take_line())) {
            call_global("__onMessage", JS_NewString(ctx, line));
            free(line);
        }
        /* Timers due? */
        double t = now_ms();
        int fired = 0;
        for (int i = 0; i < ntimers; i++) {
            if (timers[i].due <= t) {
                int64_t id = timers[i].id;
                timers[i] = timers[--ntimers];
                call_global("__onTimer", JS_NewInt64(ctx, id));
                fired = 1;
                break; /* the list may have changed */
            }
        }
        if (fired || take_line_available())
            continue;
        if (in_eof)
            break;
        /* Sleep until input or the next timer. */
        double next = -1;
        for (int i = 0; i < ntimers; i++)
            if (next < 0 || timers[i].due < next)
                next = timers[i].due;
        int timeout = next < 0 ? -1 : (int)(next - now_ms() + 1);
        if (timeout < 0 && next >= 0)
            timeout = 0;
        struct pollfd p = { .fd = 0, .events = POLLIN };
        int n = poll(&p, 1, timeout);
        if (n > 0 && !fill())
            break;
    }
    return 0;
}
