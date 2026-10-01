/* musl-libctest: a small libc conformance run (string, stdio, malloc,
 * pthread, time, signals (siginfo, ucontext, sigaltstack, per-thread
 * masks), processes, files, sockets, poll/epoll). */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <netinet/in.h>
#include <poll.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <ucontext.h>
#include <unistd.h>

static int passed, failed;
#define CHECK(name, cond) do { if (cond) passed++; else { failed++; printf("FAIL %s (%s:%d, errno %d)\n", name, __FILE__, __LINE__, errno); } } while (0)

static int cmp_int(const void *a, const void *b) { return *(const int *)a - *(const int *)b; }

static void strings(void) {
    char buf[64];
    CHECK("strlen", strlen("hello") == 5);
    CHECK("strcmp", strcmp("abc", "abd") < 0);
    CHECK("strstr", strcmp(strstr("hay needle hay", "needle"), "needle hay") == 0);
    snprintf(buf, sizeof buf, "%d-%s-%.2f-%x", 42, "x", 3.14159, 255);
    CHECK("snprintf", strcmp(buf, "42-x-3.14-ff") == 0);
    int a; char s[16]; double d;
    CHECK("sscanf", sscanf("7 word 2.5", "%d %15s %lf", &a, s, &d) == 3 && a == 7 && !strcmp(s, "word") && d == 2.5);
    CHECK("strtod", fabs(strtod("1e3", 0) - 1000.0) < 1e-9);
    CHECK("strtol", strtol("-0x10", 0, 16) == -16);
    int v[] = {5, 3, 9, 1};
    qsort(v, 4, sizeof(int), cmp_int);
    CHECK("qsort", v[0] == 1 && v[3] == 9);
    CHECK("sqrt", fabs(sqrt(2.0) - 1.41421356) < 1e-6);
}

static void memory(void) {
    char *p = malloc(100);
    CHECK("malloc", p != 0);
    memset(p, 'x', 100);
    p = realloc(p, 1 << 20);           /* large: mmap/mremap paths */
    CHECK("realloc large", p && p[99] == 'x');
    p = realloc(p, 4 << 20);
    CHECK("realloc larger", p && p[0] == 'x');
    free(p);
    void *m = mmap(0, 65536, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK("mmap", m != MAP_FAILED);
    CHECK("munmap", munmap(m, 65536) == 0);
    void **many = malloc(1000 * sizeof(void *));
    for (int i = 0; i < 1000; i++) many[i] = malloc(i * 13 + 1);
    for (int i = 0; i < 1000; i++) free(many[i]);
    free(many);
    CHECK("malloc churn", 1);
}

static void files(void) {
    FILE *f = fopen("/tmp/libctest.txt", "w+");
    CHECK("fopen", f != 0);
    fprintf(f, "line one\nline two\n");
    rewind(f);
    char line[32];
    CHECK("fgets", fgets(line, sizeof line, f) && !strcmp(line, "line one\n"));
    fclose(f);
    struct stat st;
    CHECK("stat", stat("/tmp/libctest.txt", &st) == 0 && st.st_size == 18);
    int fd = open("/tmp/libctest.txt", O_RDONLY);
    char b[4] = {0};
    CHECK("pread", pread(fd, b, 3, 5) == 3 && !memcmp(b, "one", 3));
    close(fd);
    DIR *dir = opendir("/tmp");
    int found = 0;
    struct dirent *e;
    while (dir && (e = readdir(dir)))
        if (!strcmp(e->d_name, "libctest.txt")) found = 1;
    if (dir) closedir(dir);
    CHECK("readdir", found);
    CHECK("unlink", unlink("/tmp/libctest.txt") == 0 && access("/tmp/libctest.txt", F_OK) != 0);
    char cwd[256];
    CHECK("getcwd", getcwd(cwd, sizeof cwd) != 0);
}

static volatile sig_atomic_t got;
static void on_usr1(int s) { got = s; }
static jmp_buf jb;

static void signals_and_processes(void) {
    struct sigaction sa = {0};
    sa.sa_handler = on_usr1;
    sigaction(SIGUSR1, &sa, 0);
    raise(SIGUSR1);
    CHECK("sigaction+raise", got == SIGUSR1);
    if (!setjmp(jb)) longjmp(jb, 1); else CHECK("setjmp/longjmp", 1);
    int p[2];
    CHECK("pipe", pipe(p) == 0);
    pid_t pid = fork();
    if (pid == 0) {
        close(p[0]);
        write(p[1], "child", 5);
        _exit(3);
    }
    close(p[1]);
    char b[8] = {0};
    CHECK("read from child", read(p[0], b, 8) == 5 && !strcmp(b, "child"));
    int st;
    CHECK("waitpid", waitpid(pid, &st, 0) == pid && WIFEXITED(st) && WEXITSTATUS(st) == 3);
    close(p[0]);
}

/* SA_SIGINFO handlers see musl's siginfo_t and ucontext_t over the
 * kernel's rt_sigframe. */
static sigjmp_buf segv_jb;
static char *alt_base;
static volatile int segv_code, segv_on_alt;
static void *volatile segv_addr;
static void on_segv(int s, siginfo_t *si, void *ctx) {
    char here;
    (void)s; (void)ctx;
    segv_code = si->si_code;
    segv_addr = si->si_addr;
    segv_on_alt = &here >= alt_base && &here < alt_base + 65536;
    siglongjmp(segv_jb, 1);
}
static volatile int usr2_code, usr2_pid;
static volatile long usr2_rip;
static void on_usr2(int s, siginfo_t *si, void *ctx) {
    ucontext_t *uc = ctx;
    (void)s;
    usr2_code = si->si_code;
    usr2_pid = si->si_pid;
    usr2_rip = uc->uc_mcontext.gregs[REG_RIP];
}
static pthread_t waiter_thread;
static void *sigwaiter(void *arg) {
    sigset_t set;
    int sig = 0;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    *(int *)arg = sigwait(&set, &sig) == 0 ? sig : -1;
    return 0;
}

static void siginfo_and_altstack(void) {
    stack_t ss = {.ss_sp = alt_base = malloc(65536), .ss_size = 65536};
    CHECK("sigaltstack", sigaltstack(&ss, 0) == 0);
    struct sigaction sa = {0};
    sa.sa_sigaction = on_segv;
    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
    sigaction(SIGSEGV, &sa, 0);
    char *page = mmap(0, 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    /* Volatile accesses only between sigsetjmp and the fault: nothing the
     * compiler could move before the faulting store. */
    static volatile int not_faulted;
    if (!sigsetjmp(segv_jb, 1)) {
        *(volatile char *)(page + 5) = 1;
        not_faulted = 1;
    }
    CHECK("write to read-only page faults", !not_faulted);
    CHECK("SIGSEGV siginfo (SEGV_ACCERR, si_addr)", segv_code == SEGV_ACCERR && segv_addr == page + 5);
    CHECK("SIGSEGV handler on sigaltstack", segv_on_alt);
    signal(SIGSEGV, SIG_DFL);
    munmap(page, 4096);
    ss.ss_flags = SS_DISABLE;
    sigaltstack(&ss, 0);

    sa.sa_sigaction = on_usr2;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGUSR2, &sa, 0);
    pthread_kill(pthread_self(), SIGUSR2);
    CHECK("pthread_kill siginfo (SI_TKILL, si_pid, uc_mcontext)",
          usr2_code == SI_TKILL && usr2_pid == getpid() && usr2_rip != 0);
    signal(SIGUSR2, SIG_DFL);

    /* Per-thread masks: the thread blocks SIGUSR1 (inherited) and takes it
     * with sigwait; the main thread does not block it. */
    sigset_t set, old;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    pthread_sigmask(SIG_BLOCK, &set, &old);
    int got_sig = 0;
    pthread_create(&waiter_thread, 0, sigwaiter, &got_sig);
    pthread_sigmask(SIG_SETMASK, &old, 0);
    sigset_t now;
    pthread_sigmask(SIG_SETMASK, 0, &now);
    CHECK("pthread_sigmask is per thread", !sigismember(&now, SIGUSR1));
    struct timespec d = {0, 20 * 1000000};
    nanosleep(&d, 0);
    got = 0;
    pthread_kill(waiter_thread, SIGUSR1);
    pthread_join(waiter_thread, 0);
    CHECK("sigwait in a thread takes pthread_kill'd signal", got_sig == SIGUSR1 && got == 0);
}

static void *adder(void *arg) { *(int *)arg += 1; return 0; }

static void threads_and_time(void) {
    int x = 0;
    pthread_t t;
    CHECK("pthread_create", pthread_create(&t, 0, adder, &x) == 0);
    CHECK("pthread_join", pthread_join(t, 0) == 0 && x == 1);
    struct timespec a, b, d = {0, 20 * 1000000};
    clock_gettime(CLOCK_MONOTONIC, &a);
    nanosleep(&d, 0);
    clock_gettime(CLOCK_MONOTONIC, &b);
    long ms = (b.tv_sec - a.tv_sec) * 1000 + (b.tv_nsec - a.tv_nsec) / 1000000;
    CHECK("nanosleep", ms >= 19 && ms < 1000);
    time_t now = time(0);
    struct tm tm;
    CHECK("gmtime", gmtime_r(&now, &tm) != 0 && tm.tm_year >= 70);
}

static void sockets_and_polling(void) {
    int srv = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(18777);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    int one = 1;
    setsockopt(srv, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    CHECK("bind", bind(srv, (struct sockaddr *)&addr, sizeof addr) == 0);
    CHECK("listen", listen(srv, 4) == 0);
    int c = socket(AF_INET, SOCK_STREAM, 0);
    CHECK("connect", connect(c, (struct sockaddr *)&addr, sizeof addr) == 0);
    int a = accept(srv, 0, 0);
    CHECK("accept", a >= 0);
    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN, .data.fd = a};
    epoll_ctl(ep, EPOLL_CTL_ADD, a, &ev);
    send(c, "ping", 4, 0);
    struct epoll_event out;
    CHECK("epoll_wait", epoll_wait(ep, &out, 1, 2000) == 1 && out.data.fd == a);
    char b[8] = {0};
    CHECK("recv", recv(a, b, 8, 0) == 4 && !strcmp(b, "ping"));
    struct pollfd pfd = {.fd = c, .events = POLLOUT};
    CHECK("poll", poll(&pfd, 1, 1000) == 1 && (pfd.revents & POLLOUT));
    close(ep); close(a); close(c); close(srv);
}

int main(void) {
    strings();
    memory();
    files();
    signals_and_processes();
    siginfo_and_altstack();
    threads_and_time();
    sockets_and_polling();
    printf("libctest: %d passed, %d failed\n", passed, failed);
    return failed != 0;
}
