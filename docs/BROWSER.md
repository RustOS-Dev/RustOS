# browse — the text web browser

`browse` (also installed as `lynx` and `www`) is a text-mode web browser
in the spirit of lynx. It is meant for reading documentation, downloading
files and — importantly on a laptop — logging into Wi-Fi networks that
put a web login page ("captive portal") in front of the Internet.

```
browse [-k] [-nocss] [-dump|-source] [-width N] [--portal] [URL]
```

| Option | Meaning |
|--------|---------|
| `URL` | page to open (`example.com`, `https://…`, `file:///etc`, `/storage`) |
| `-dump` | print the rendered page and a list of links, then exit |
| `-source` | print the page source, then exit |
| `-width N` | layout width for `-dump` |
| `-k` | accept any TLS certificate |
| `-nocss` | ignore the page's style sheets (the built-in HTML styles only) |
| `--portal` | open the captive-portal login page (see below) |

Without a URL the built-in help page opens. When the output is not a
terminal `browse` behaves like `-dump`.

## Keys

| Key | Action |
|-----|--------|
| Down, Tab / Up, Shift-Tab | next / previous link or form field |
| Right, Enter | follow the link; edit, toggle or press the field |
| Left, Backspace, `u` | back |
| Space, PgDn, `+` / `b`, PgUp, `-` | page down / up |
| Home, End | top / bottom |
| `g` / `G` | open a URL / edit the current URL |
| `12` Enter | follow link number 12 |
| `/`, `n` | search, next match |
| `r`, Ctrl-R | reload |
| `\` | show the HTML source |
| `=` | page information: URL, status, TLS version and cipher, cookies, headers |
| `d` | download the selected link (to `/storage/Downloads`) |
| `c` | cookies (delete per domain) |
| `h`, `?` | help |
| `q`, Ctrl-C | quit |

Links are numbered (`[3]`), highlighted in colour and shown with their
target in the status line. Form fields are drawn as widgets:
`[text____]`, `[****__]` (password), `[X]`/`[ ]` (check box), `(*)`/`( )`
(radio button), `[choice v]` (drop-down list), `[ Submit ]`.

* **Text fields** open an editor on the bottom line (Enter accepts, Esc
  cancels, Ctrl-A/E/K/U/W work). Text areas open a full-screen editor
  (Ctrl-X done).
* **Drop-down lists** open a menu (Up/Down, Enter).
* **Buttons** submit their form (GET, POST urlencoded, multipart, or
  text/plain, with `formaction`/`formmethod` honoured). Pressing Enter in
  a text field submits a form that has no submit button.

## What it supports

* HTTP/1.1 with keep-alive, redirects, chunked and gzip/deflate bodies;
  HTTPS with TLS 1.2 and 1.3 (certificates checked against the CA bundle;
  on a certificate problem `browse` asks whether to continue anyway, which
  some portals with self-signed certificates need).
* Cookies (RFC 6265, including `SameSite`), persisted for sessions across
  runs in `/storage/etc/cookies.txt`.
* HTML: headings, paragraphs, lists, definition lists, block quotes,
  preformatted text, tables, images as `[alt text]`, frames and iframes as
  placeholders, `<meta http-equiv=refresh>` (followed after a short
  countdown that any key cancels), `<base>`, fragment links, UTF-8 /
  Latin-1 / windows-1252 pages, IPv6 addresses (`http://[2001:db8::1]/`).
* CSS: the page's `<style>` elements and `<link rel=stylesheet>` sheets
  (with `@import`, `@media`, `@supports`, `@layer`, nesting, custom
  properties) are applied and the page is laid out with the CSS box model
  — block and inline flow, floats, flexbox, grid, tables, positioning —
  in character cells (one column = 8 px, one row = 16 px). Colors are
  shown in 24-bit color where the page sets a background; text colors on
  the terminal's own background are kept only when readable. See
  [CSS.md](CSS.md).
* `file:` URLs and directory listings, `about:help`, `about:cookies`.

* JavaScript (QuickJS-ng, in a helper process per page): the DOM, events,
  forms, `fetch`/XHR, `localStorage`, timers, modules, WebSocket and
  EventSource. Elements with click handlers are selectable like links
  (marked `[*]`); `J` opens the JavaScript console, `K` turns scripts
  off and on, `-nojs` starts with them off. See [JAVASCRIPT.md](JAVASCRIPT.md).

Not supported: images and video in text mode (see graphical mode), frames.

## Graphical mode (`browse -g`)

`browse -g URL` draws pages on the framebuffer (`/dev/fb0`), with the
console switched to graphics mode (`KDSETMODE KD_GRAPHICS`).

**Rendering**
- The same CSS engine and layout as text mode, laid out in pixels.
- Text uses the bundled DejaVu fonts (sans, serif, mono; regular and bold, in `/usr/share/fonts/dejavu`), rasterized with anti-aliasing by fontdue, with kerning.
- `@font-face` web fonts in TTF/OTF and WOFF 1.0 format are loaded.
- Pages are painted by `crates/paint`:
  - backgrounds, including linear gradients and images with `cover`/`contain`/repeat;
  - borders with rounded corners and dotted, dashed and double styles;
  - box shadows, `opacity` and overflow clipping;
  - text decorations, list bullets, and drawn form controls.
- Images: PNG, JPEG, GIF (first frame) and BMP, scaled with bilinear filtering.
- `<canvas>` elements show what scripts drew with the 2D context. That context is a software rasterizer in jsd: rectangles, paths, arcs and Bézier curves, strokes, gradients, transforms, `get`/`putImageData` and canvas-to-canvas `drawImage`. Text on a canvas is measured but not drawn.

**Keys**
- Tab/Down and Shift-Tab/Up move between links and form fields.
- Enter follows or activates the selected one. Text fields are edited in the status bar.
- Space/PgDn and b/PgUp scroll by a page; j/k scroll by lines; Home and End jump to the top and bottom.
- `g` opens a URL; Left goes back; `r` reloads; `q` quits.

**Mouse:** the pointer comes from `/dev/input/mice`, and a click activates what is under it.

**Screenshots:** `browse -dump-png FILE [-size WxH] [-full] URL` renders a page, with its scripts and images, into a PNG without a screen. `-full` captures the whole page height rather than just the viewport.

## Captive portals

1. Join the network: `wifi connect "Hotel Wi-Fi"`. After the connection
   `wifi` checks for a portal and prints
   `captive portal detected: … (log in with 'browse --portal')`.
   (`netcheck` runs the same check at any time.)
2. Run `browse --portal`. It opens the login page — the one the network
   announced (DHCP option 114, DHCPv6 or router advertisement, RFC 8910),
   the one found by the probe (`/run/portal`), or it probes now.
3. Fill in the form and submit. After every page `browse` checks the
   connection again and shows **Internet access OK** once you are through.

The probe fetches `http://connectivitycheck.gstatic.com/generate_204`;
another URL and expected status can be set in `/etc/portal.conf` (or
`/storage/etc/portal.conf`):

```
url=http://captive.example.org/check
expect=204
```

## Implementation

| Part | Where |
|------|-------|
| URL parsing and resolution | `crates/weburl` |
| HTTP client, cookies, gzip, forms | `crates/http` (package `httpc`) |
| TLS | `crates/nettls` (rustls + RustCrypto) |
| HTML parser | `crates/html` |
| CSS (parsing, selectors, cascade, computed style) | `crates/css` |
| Layout (box tree, flow, flex, grid, tables) and the cell renderer | `crates/layout` |
| Sockets, CA bundle, portal detection | `userland/webclient` |
| The program | `userland/browser` |

The pure crates have host unit tests; the `browser` and `captive-portal`
boot scenarios drive the real program against a scripted web server
(`tools/test-web-server.py`).
