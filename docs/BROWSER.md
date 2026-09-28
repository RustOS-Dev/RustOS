# browse — the text web browser

`browse` (also installed as `lynx` and `www`) is a text-mode web browser
in the spirit of lynx. It is meant for reading documentation, downloading
files and — importantly on a laptop — logging into Wi-Fi networks that
put a web login page ("captive portal") in front of the Internet.

```
browse [-k] [-dump|-source] [-width N] [--portal] [URL]
```

| Option | Meaning |
|--------|---------|
| `URL` | page to open (`example.com`, `https://…`, `file:///etc`, `/storage`) |
| `-dump` | print the rendered page and a list of links, then exit |
| `-source` | print the page source, then exit |
| `-width N` | layout width for `-dump` |
| `-k` | accept any TLS certificate |
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
  preformatted text, tables in columns (or one cell after another when
  the screen is too narrow), images as `[alt text]`, frames and iframes
  as links, `<meta http-equiv=refresh>` (followed after a short countdown
  that any key cancels), `<base>`, fragment links, UTF-8 / Latin-1 /
  windows-1252 pages, IPv6 addresses (`http://[2001:db8::1]/`).
* `file:` URLs and directory listings, `about:help`, `about:cookies`.

Not supported: JavaScript, CSS layout, images, video. Pages that only work
with JavaScript show their `<noscript>` content and whatever forms they
contain.

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
| Text layout | `crates/textlayout` |
| Sockets, CA bundle, portal detection | `userland/webclient` |
| The program | `userland/browser` |

The pure crates have host unit tests; the `browser` and `captive-portal`
boot scenarios drive the real program against a scripted web server
(`tools/test-web-server.py`).
