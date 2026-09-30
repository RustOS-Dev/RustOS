// jsd runtime: messaging with the browser, requests, timers, console.
"use strict";
(function (G) {
  const H = G.__host;
  delete G.__host;
  const J = G.__jsd = {};
  J.inbox = [];        // messages that arrived during a synchronous request
  J.ops = [];          // DOM mutations for the next batch
  J.out = [];          // other messages for the next batch
  J.pending = new Map(); // async request id -> [resolve, reject]
  J.seq = 1;
  J.handlers = {};     // message type -> handler
  J.host = H;

  J.send = function (msg) { H.send(JSON.stringify(msg)); };
  J.queue = function (msg) { J.out.push(msg); };

  // A synchronous request: wait for its reply, keeping other messages.
  J.rpc = function (m, a) {
    const id = J.seq++;
    J.flushOps();
    J.send({ t: "rpc", id, m, a: a === undefined ? null : a });
    for (;;) {
      const line = H.recv();
      const msg = JSON.parse(line);
      if (msg.t === "reply" && msg.id === id) {
        if (msg.error !== undefined && msg.error !== null) throw new Error(msg.error);
        return msg.v;
      }
      J.inbox.push(msg);
    }
  };

  // An asynchronous request: a promise settled by the reply.
  J.rpcAsync = function (m, a) {
    const id = J.seq++;
    J.flushOps();
    J.send({ t: "rpc", id, m, a: a === undefined ? null : a, async: true });
    return new Promise((resolve, reject) => J.pending.set(id, [resolve, reject]));
  };

  J.flushOps = function () {
    if (J.ops.length) {
      const ops = J.ops;
      J.ops = [];
      J.send({ t: "mut", ops });
    }
  };

  // Called by the host at the end of every task.
  G.__flush = function () {
    if (J.onFlush) J.onFlush();
    J.flushOps();
    for (const m of J.out.splice(0)) J.send(m);
    if (J.inbox.length) {
      // Handle messages that arrived during synchronous requests.
      H.setTimer(-1, 0);
    }
    J.send({ t: "idle" });
  };

  G.__onMessage = function (line) {
    let msg;
    try { msg = JSON.parse(line); } catch (e) { return; }
    J.dispatch(msg);
  };

  J.dispatch = function (msg) {
    if (msg.t === "reply") {
      const p = J.pending.get(msg.id);
      if (p) {
        J.pending.delete(msg.id);
        if (msg.error !== undefined && msg.error !== null) p[1](new TypeError(msg.error));
        else p[0](msg.v);
      }
      return;
    }
    const h = J.handlers[msg.t];
    if (h) h(msg);
  };

  // ---- timers ----
  const timers = new Map(); // id -> {fn, args, interval, ms}
  let nextTimer = 1;
  function callTimer(t) {
    if (typeof t.fn === "function") t.fn.apply(G, t.args);
    else (0, eval)(String(t.fn));
  }
  G.__onTimer = function (id) {
    if (id === -1) {
      for (const m of J.inbox.splice(0)) J.dispatch(m);
      return;
    }
    const t = timers.get(id);
    if (!t) return;
    if (t.interval) H.setTimer(id, t.ms);
    else timers.delete(id);
    try { callTimer(t); } catch (e) { G.__onError(e); }
  };
  function addTimer(fn, ms, args, interval) {
    const id = nextTimer++;
    ms = Math.max(0, Number(ms) || 0);
    if (interval) ms = Math.max(ms, 4);
    timers.set(id, { fn, args, interval, ms });
    H.setTimer(id, ms);
    return id;
  }
  function clearTimer(id) {
    id = Number(id);
    if (timers.delete(id)) H.clearTimer(id);
  }
  G.setTimeout = (fn, ms, ...args) => addTimer(fn, ms, args, false);
  G.setInterval = (fn, ms, ...args) => addTimer(fn, ms, args, true);
  G.clearTimeout = clearTimer;
  G.clearInterval = clearTimer;
  G.queueMicrotask = (fn) => { Promise.resolve().then(() => fn()); };
  J.frameMs = 100;
  G.requestAnimationFrame = (fn) => addTimer(() => fn(G.performance.now()), J.frameMs, [], false);
  G.cancelAnimationFrame = clearTimer;
  G.requestIdleCallback = (fn) => addTimer(() => fn({ didTimeout: false, timeRemaining: () => 10 }), 1, [], false);
  G.cancelIdleCallback = clearTimer;

  // ---- console and errors ----
  function fmt(v) {
    if (typeof v === "string") return v;
    if (v instanceof Error) return v.name + ": " + v.message;
    try { return JSON.stringify(v) ?? String(v); } catch (e) { return String(v); }
  }
  const logs = {};
  function logger(level) {
    return (...args) => J.queue({ t: "console", level, text: args.map(fmt).join(" ") });
  }
  G.console = {
    log: logger("log"), info: logger("info"), warn: logger("warn"), error: logger("error"),
    debug: logger("debug"), trace: logger("debug"), dir: logger("log"), table: logger("log"),
    group: logger("log"), groupCollapsed: logger("log"), groupEnd() {},
    time(l) { logs[l] = H.now(); }, timeEnd(l) { logger("log")(l + ": " + (H.now() - (logs[l] || 0)) + "ms"); },
    assert(c, ...a) { if (!c) logger("error")("Assertion failed", ...a); },
    count: logger("log"),
  };
  G.__onError = function (e) {
    let text;
    if (e instanceof Error) text = e.name + ": " + e.message + (e.stack ? "\n" + e.stack : "");
    else text = "Uncaught " + fmt(e);
    J.queue({ t: "console", level: "error", text });
    if (G.dispatchEvent && G.ErrorEvent) {
      try { G.dispatchEvent(new G.ErrorEvent("error", { message: String(e && e.message || e), error: e })); } catch (x) {}
    }
  };

  G.performance = {
    now: () => H.now() - J.t0,
    timeOrigin: Date.now(),
    mark() {}, measure() {}, getEntriesByType: () => [], getEntriesByName: () => [],
  };
  J.t0 = H.now();
  J.random32 = H.random;
})(globalThis);
