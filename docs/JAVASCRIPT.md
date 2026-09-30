# JavaScript in the browser

`browse` runs page scripts with [QuickJS-ng](https://github.com/quickjs-ng/quickjs)
in a helper process, `jsd` (`/usr/libexec/jsd`). Each page gets its own
jsd process. jsd owns the page's live DOM, and the browser keeps a copy that
it lays out.

```
 browse (Rust)                               jsd (C + QuickJS + lib/*.js)
 ─────────────                               ───────────────────────────
 fetch, parse (crates/html) ── init {tree} ──▶ builds the DOM, runs scripts
 layout (crates/layout)     ◀── mut {ops} ──── DOM mutations after each task
 cookies, TLS, CORS, storage ◀── rpc ───────── fetch, cookie, storage, rects…
 user actions ──────────── event/input ──────▶ dispatch, default actions
                           ◀── navigate/submit/eventDone
```

## Why a separate process

- **Networking and security stay in Rust.** jsd reaches the network,
  cookies and storage only by asking the browser. The browser enforces
  the same-origin policy and CORS on each request.
- **A crashing or looping script cannot hang the browser.**
  - The browser only waits for jsd with timeouts.
  - jsd stops any single script or callback after 10 s, using QuickJS's
    interrupt handler; the error is logged to the page console.
  - The memory limit is 256 MiB.
- **Isolation.** jsd starts with only its two pipes: stderr goes to
  `/dev/null` and every other file descriptor is closed before exec. The
  QuickJS `std`/`os` modules are not loaded.

## Protocol

The two processes exchange one JSON object per line over a pipe pair. The
field `t` is the message type. The codec and DOM helpers are in
`crates/jsproto`.

| Direction | Message | Meaning |
|-----------|---------|---------|
| browse → jsd | `init` | Starts the page. Carries the URL, the referrer, the viewport in CSS px, the parsed tree as `[id, tag, attrs, children]` and the next free node id. |
| browse → jsd | `event` | A user action on a node: `click`, `focus`, `blur`, `submit` or a key. jsd answers `eventDone` with `cancelled`. |
| browse → jsd | `input` | The user changed a control's `value`, `checked` state or select `index`. jsd updates the control and fires `input` and `change`. |
| browse → jsd | `reply` | The answer to one of jsd's requests. |
| browse → jsd | `ws`, `es` | WebSocket and EventSource events. |
| browse → jsd | `resize`, `scroll`, `unload`, `eval` | Viewport and navigation events, and code typed in the console. |
| jsd → browse | `mut` | DOM operations: `create`, `insert`, `remove`, `attr`, `data`, `value`, `checked`, `selected`. |
| jsd → browse | `rpc` | A request: `fetch`, `fetchScript`, `parseFragment`, `parseDocument`, `cookie`, `setCookie`, `storageLoad`, `rects`, `hitTest`, `computedStyle`, `matchMedia`, `cssSupports`, `cssRules`, `alert`, `confirm`, `prompt`. |
| jsd → browse | `navigate`, `submit`, `history`, `fragment`, `go` | Navigation. A form submission carries its entry list; the browser encodes and sends it. |
| jsd → browse | `console`, `clickable`, `focus`, `invalid`, `storageSet`, `wsOpen`/`wsSend`/`wsClose`, `esOpen`/`esClose` | Console output, elements with click listeners, focus changes, validation messages, storage writes, and socket commands. |
| jsd → browse | `idle`, `loaded` | End of a task, and the page's load event. |

Node ids are shared. The browser's ids index `html::Document::nodes`. New
nodes get ids from jsd's counter, which starts at the browser's next id.
HTML fragments (`innerHTML`, `DOMParser`) are parsed by the browser with
the same parser.

## Supported web platform

The library is `userland/jsd/lib/*.js`, embedded in jsd at build time.

**DOM**
- `Node`, `Element`, `Document`, `DocumentFragment`, `Text`, `Comment` and `Attr`, with tree mutation, `ChildNode` and `ParentNode` methods.
- Live `childNodes`, `children` and `getElementsBy*`.
- `querySelector(All)`, `matches` and `closest` (Selectors 4 including `:is`, `:where`, `:not`, `:has` and `:nth-*`).
- `classList`, `dataset`, `style` (a `CSSStyleDeclaration` over the style attribute), `attributes`.
- `innerHTML`, `outerHTML`, `insertAdjacent*`, `textContent`, `innerText`, `cloneNode`, `compareDocumentPosition`.
- `template`, `MutationObserver`, `TreeWalker`, `NodeIterator`, a basic `Range`.
- `DOMParser`, `XMLSerializer`, `document.implementation`.
- Custom elements (`customElements.define`, lifecycle callbacks, `observedAttributes`).
- Elements with an id, and named forms and images, are window properties.

**HTML**
- Reflected attributes for common elements.
- Forms:
  - `value`, `checked`, `selectedIndex`, `options` and `form.elements`, including `form.name` access;
  - `submit()`, `requestSubmit()`, `reset()`;
  - `FormData`;
  - constraint validation: `required`, `pattern`, `min`/`max`/`step`, `minlength`/`maxlength`, and `email`/`url`/`number` types, with `checkValidity`, `reportValidity`, `setCustomValidity` and `validity`.
- `dialog`, `details`, `label` activation, `Image` and `Audio` constructors.

**Events**
- `EventTarget` with capture and bubble, `once`, `passive` and `signal`.
- `Event`, `UIEvent`, `MouseEvent`, `KeyboardEvent`, `InputEvent`, `FocusEvent`, `SubmitEvent`, `CustomEvent` and others.
- Inline `on*` attributes, compiled with the element, form and document scope chain.
- `DOMContentLoaded`, `load`, `readystatechange`, `hashchange`, `popstate`, `beforeunload`, `pagehide` and `unload`.

**Window**
- `location`: its setters navigate; `assign`, `replace` and `reload`.
- `history`: `pushState` and `replaceState` update the browser's URL; `back` and `go` are supported.
- `navigator`, `screen`, `alert`/`confirm`/`prompt` (shown on the status line), `open`, `getComputedStyle`, `matchMedia`, `scrollTo`.
- `requestAnimationFrame` at 10 Hz in text mode.
- `setTimeout`, `setInterval`, `queueMicrotask`, `structuredClone`, `ResizeObserver`, and an `IntersectionObserver` that reports every element as visible.

**Network**
- `fetch` with `Request`, `Response`, `Headers` and `AbortController`.
- `XMLHttpRequest`, synchronous and asynchronous.
- `URL` and `URLSearchParams`, using a WHATWG URL parser written in JS.
- `Blob`, `File`, `FileReader` and object URLs.
- `WebSocket` (RFC 6455, run by the browser) and `EventSource`.

**Storage**
- `localStorage`, stored per origin in `/storage/var/browser/localstorage/`, or under `/tmp` without a storage partition.
- `sessionStorage`, kept for as long as the browser runs.
- `document.cookie`. HttpOnly cookies are neither visible nor settable.

**Utilities**
- `TextEncoder`/`TextDecoder` (UTF-8, UTF-16, windows-1252) and `atob`/`btoa`.
- `crypto.getRandomValues`, `crypto.randomUUID`, `crypto.subtle.digest` (SHA-1/256/384/512) and HMAC `sign`/`verify`.
- A small English-only `Intl`: `NumberFormat`, `DateTimeFormat`, `Collator`, `PluralRules`, `RelativeTimeFormat` and `ListFormat`.

### Script loading

- Classic scripts block parsing and run in document order. `defer` scripts run before `DOMContentLoaded`; `async` scripts run when they arrive.
- Module scripts (`type=module`, `import()`) load through the browser, with import maps.
- `document.write` during parsing inserts its markup after the running script, and scripts it writes run in order.
- Scripts inserted by other scripts run when connected. Scripts inserted with `innerHTML` do not run.
- `<noscript>` is hidden while scripts run.

## Security model

- **Same-origin policy and CORS.**
  - `fetch`/XHR to another origin needs `Access-Control-Allow-Origin` in the response.
  - Non-simple requests are preflighted with `OPTIONS`.
  - `credentials: "include"` needs `Access-Control-Allow-Credentials`.
  - `no-cors` responses are opaque.
  - Only CORS-safelisted and exposed headers reach the page, and `Set-Cookie` never does.
- **Cookies** are sent by the browser's jar with the same SameSite rules as navigations.
- **Mixed content.** HTTPS pages cannot fetch, load scripts from, or open sockets to `http:`/`ws:` URLs.
- **Isolation.** Pages cannot read each other's DOM, since each page has its own process. Frames are not loaded.
- **Not supported:** CSP headers. Scripts cannot reach files or processes.

## Using it

- Scripts are on by default. `browse -nojs` or the `K` key turns them off, and `K` reloads the page.
- The title bar shows `[JS]` while a page's scripts run.
- Elements with click listeners or `onclick` can be selected like links. They are marked `[*]`, and Enter clicks them.
- `J` opens the JavaScript console. It shows `console.*` output and errors, and evaluates expressions you type.
- `browse -dump` runs scripts until the load event and then until they have been quiet for 300 ms (at most 5 s). It follows script navigations and form submissions, up to five, and prints script errors to stderr.

## Tests

- **`crates/jsproto/tests/jsd.rs`**
  - Runs a host build of jsd (`userland/jsd/host-build.sh QUICKJS_SRC OUT`, then set `JSD_BIN=OUT/jsd`) against a minimal browser.
  - Covers the DOM, script order, `document.write`, modules, forms and validation, fetch/XHR, storage, timers, location/history, URL/encoding/crypto, MutationObserver and custom elements.
- **Scenario `browser-js`**
  - Pages from `tools/test-web-server.py` (`/js/...`): script rendering, `<noscript>`, XHR, localStorage persistence, a timer redirect, `document.write`, modules with an import map, WebSocket echo and EventSource.
  - Also a script-validated `fetch` login and a click-handler element, both driven with keys, and the console.
- **Scenario `captive-portal-js`:** a portal whose Connect button is a `<span>` with a click listener. The listener calls `fetch` and then sets `location.href`, and `browse --portal` then reports Internet access.

## Limits

- Frames and `postMessage` between them are not supported.
- There is no `<canvas>` in text mode (see M20 for the graphical browser).
- The following are not implemented: IndexedDB, service workers, WebRTC, WebAssembly, Web Workers and CSP.
- Layout queries answer in CSS pixels of the character-cell layout, where a cell is 8×16 px.
