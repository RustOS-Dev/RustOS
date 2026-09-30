//! browse: a lynx-like text web browser.
//!
//! Pages are fetched with `webclient` (HTTP/1.1, HTTPS with TLS 1.2/1.3,
//! cookies, redirects), parsed by `html`, styled by `css` (the page's
//! `<style>` and `<link rel=stylesheet>` sheets, with `@import`), laid out
//! by `layout` in character cells and drawn with ANSI escapes. Links and
//! form fields are navigated with the arrow keys; forms can be filled in
//! and submitted (GET, POST, multipart). Page scripts run in a helper
//! process, `jsd` (QuickJS), which owns the live DOM and sends its changes
//! back (see script.rs). `browse --portal` opens a
//! captive-portal login page and reports when Internet access works.

#![no_std]
#![no_main]

extern crate alloc;

mod form;
mod gfx;
mod js;
mod load;
mod media;
mod script;
mod sockets;
mod term;

use alloc::collections::BTreeMap;
use html::NodeId;
use jsproto::Json;
use layout::{DomState, Field, FieldKind, Page, SheetSource, Style, Target, field_text};
use load::{LoadError, Loaded, Loader};
use rustos_rt::io::{POLLIN, PollFd};
use rustos_rt::prelude::*;
use rustos_rt::{fs, io, time};
use script::{Action, Override, Sessions, Ui};
use term::{Key, Term, goto, out};
use webclient::httpc::cookie::Context;
use webclient::httpc::{Request, Url};
use webclient::portal;

rustos_rt::entry!(main);

const USAGE: &str = "usage: browse [-k] [-nocss] [-nojs] [-dump|-source] [-width N] [-g] [-dump-png FILE [-size WxH] [-full]] [--portal] [URL]";

/// Author CSS is applied (off with `-nocss`).
static mut USE_CSS: bool = true;
/// Page scripts run (off with `-nojs`, toggled with K).
static mut USE_JS: bool = true;

fn use_css() -> bool {
    // SAFETY: set once in main before any page is loaded.
    unsafe { USE_CSS }
}

fn use_js() -> bool {
    // SAFETY: single-threaded; changed only between page loads.
    unsafe { USE_JS }
}

/// One document in the history.
pub struct Entry {
    loaded: Loaded,
    /// Author style sheets (fetched when the page's set of sheets changes).
    sheets: Vec<String>,
    /// The sheet sources the sheets were fetched for.
    sheet_key: String,
    doc: html::Document,
    page: Page,
    fields: Vec<Field>,
    top: usize,
    sel: Option<usize>,
    source: bool,
    width: usize,
    /// Control state from the user and scripts, by element.
    overrides: BTreeMap<NodeId, Override>,
    /// The document changed since the last layout.
    dirty: bool,
    /// The page's scripts, while it is shown.
    script: Option<script::Script>,
    /// WebSocket/EventSource connections of the page's scripts.
    sockets: Vec<sockets::Conn>,
    referrer: String,
}

impl Entry {
    fn new(loaded: Loaded, doc: html::Document, width: usize) -> Entry {
        Entry {
            loaded,
            sheets: Vec::new(),
            sheet_key: String::new(),
            doc,
            page: Page::default(),
            fields: Vec::new(),
            top: 0,
            sel: None,
            source: false,
            width,
            overrides: BTreeMap::new(),
            dirty: true,
            script: None,
            sockets: Vec::new(),
            referrer: String::new(),
        }
    }

    fn scripting(&self) -> bool {
        self.script.as_ref().is_some_and(|s| !s.js.dead)
    }

    /// Record a control change by the user.
    fn set_override(&mut self, i: usize, f: impl FnOnce(&mut Field, &mut Override)) {
        let node = self.fields[i].node;
        let o = self.overrides.entry(node).or_default();
        f(&mut self.fields[i], o);
    }
}

struct App {
    term: Term,
    loader: Loader,
    hist: Vec<Entry>,
    cur: usize,
    msg: String,
    search: String,
    number: String,
    portal: bool,
    refreshes: u32,
    sessions: Sessions,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn document(l: &Loaded, source: bool) -> html::Document {
    let src = if source || !l.mime.contains("html") {
        let title = if source {
            format!("Source of {}", l.url)
        } else {
            l.url.to_string()
        };
        format!(
            "<title>{}</title><pre>{}</pre>",
            escape(&title),
            escape(&l.text)
        )
    } else {
        l.text.clone()
    };
    html::parse(&src)
}

fn render_opts(width: usize, scripting: bool) -> layout::Options {
    let rows = if io::isatty(1) {
        rustos_rt::term::size(1).0 as usize
    } else {
        24
    };
    layout::Options {
        width: width.max(20),
        height: rows.max(10),
        author_css: use_css(),
        scripting,
        ..layout::Options::default()
    }
}

/// Fetch the author style sheets of `doc` (`<link rel=stylesheet>`
/// resolved against the base URL, `@import`s inlined before the sheet
/// that imports them).
fn fetch_sheets(
    loader: &mut Loader,
    l: &Loaded,
    doc: &html::Document,
    width: usize,
) -> Vec<String> {
    if !use_css() || !l.mime.contains("html") {
        return Vec::new();
    }
    let base = doc
        .base
        .as_deref()
        .and_then(|b| l.url.join(b).ok())
        .unwrap_or_else(|| l.url.clone());
    let device = layout::cell_device(&render_opts(width, false));
    let mut out = Vec::new();
    let mut budget = 16; // at most this many downloads per page
    for src in layout::sheet_sources(doc, &device) {
        match src {
            SheetSource::Inline(css) => add_sheet(loader, &base, css, &mut out, &mut budget, 0),
            SheetSource::Link(href) => {
                if let Some((css, url)) = fetch_css(loader, &base, &href, &mut budget) {
                    add_sheet(loader, &url, css, &mut out, &mut budget, 0);
                }
            }
        }
    }
    out
}

fn fetch_css(
    loader: &mut Loader,
    base: &Url,
    href: &str,
    budget: &mut u32,
) -> Option<(String, Url)> {
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    let url = base.join(href).ok()?;
    let ctx = Context {
        same_site: url.host_str() == base.host_str(),
        top_level_safe: false,
    };
    match loader.fetch(Request::get(url), ctx) {
        Ok(r) if r.status < 400 && !r.mime.contains("html") => Some((r.text, r.url)),
        _ => None,
    }
}

fn add_sheet(
    loader: &mut Loader,
    base: &Url,
    css: String,
    out: &mut Vec<String>,
    budget: &mut u32,
    depth: u32,
) {
    if depth < 3 {
        for href in layout::imports_of(&css) {
            if let Some((t, u)) = fetch_css(loader, base, &href, budget) {
                add_sheet(loader, &u, t, out, budget, depth + 1);
            }
        }
    }
    out.push(css);
}

/// Lay out a document with its style sheets.
fn render(doc: &html::Document, sheets: &[String], width: usize) -> Page {
    layout::render_with(doc, &render_opts(width, false), sheets, &mut |_| None)
}

/// A key for the page's set of style sheets (to refetch when scripts
/// change it).
fn sheet_key(doc: &html::Document, width: usize) -> String {
    let device = layout::cell_device(&render_opts(width, false));
    let mut k = String::new();
    for s in layout::sheet_sources(doc, &device) {
        match s {
            SheetSource::Inline(t) => k.push_str(&t),
            SheetSource::Link(h) => k.push_str(&h),
        }
        k.push('\u{1}');
    }
    k
}

/// Element state for layout: script click handlers and control state.
pub(crate) fn dom_state(e: &Entry) -> DomState {
    let mut state = DomState::default();
    if let Some(s) = &e.script {
        state.clickable = s.clickable.clone();
        // Inline onclick handlers.
        for n in e.doc.descendants(0) {
            if e.doc.attr(n, "onclick").is_some() || e.doc.attr(n, "onmousedown").is_some() {
                state.clickable.insert(n);
            }
        }
    }
    for (&n, o) in &e.overrides {
        if let Some(c) = o.checked {
            state.checked.push((n, c));
        }
        if let Some(v) = &o.value {
            state.values.push((n, v.clone()));
        }
        if let Some(i) = o.selected {
            state.selected.push((n, i));
        }
    }
    state
}

/// Lay out an entry's document again (after script changes, a resize or
/// control changes), keeping control state.
pub(crate) fn relayout(e: &mut Entry, loader: &mut Loader) {
    if !e.source {
        let key = sheet_key(&e.doc, e.width);
        if key != e.sheet_key {
            e.sheets = fetch_sheets(loader, &e.loaded, &e.doc, e.width);
            e.sheet_key = key;
        }
    }
    let scripting = e.scripting();
    let state = dom_state(e);
    let opts = layout::Options {
        state,
        ..render_opts(e.width, scripting)
    };
    let sheets: &[String] = if e.source { &[] } else { &e.sheets };
    e.page = layout::render_with(&e.doc, &opts, sheets, &mut |_| None);
    e.fields = e.page.fields.clone();
    for f in &mut e.fields {
        if let Some(o) = e.overrides.get(&f.node) {
            if let Some(v) = &o.value {
                f.value = v.clone();
            }
            if let Some(c) = o.checked {
                f.checked = c;
            }
            if let Some(s) = o.selected {
                if s < f.options.len() {
                    f.selected = s;
                }
            }
        }
    }
    e.dirty = false;
}

/// Dialogs in batch mode: alerts go to stderr, confirm says yes, prompt
/// takes the default.
struct BatchUi;

impl Ui for BatchUi {
    fn alert(&mut self, text: &str) {
        eprintln!("alert: {}", text);
    }
    fn confirm(&mut self, text: &str) -> bool {
        eprintln!("confirm: {} -> yes", text);
        true
    }
    fn prompt(&mut self, text: &str, default: &str) -> Option<String> {
        eprintln!("prompt: {} -> {}", text, default);
        Some(default.to_string())
    }
}

/// Dialogs on the terminal.
struct TermUi<'a> {
    term: &'a mut Term,
}

impl TermUi<'_> {
    fn show(&mut self, text: &str, hint: &str) {
        let (rows, cols) = self.term.size();
        let t = text.replace('\n', " ");
        out(&format!(
            "{}\x1b[0;7m{}\x1b[0m",
            goto(rows - 1, 0),
            pad(&format!("{} {}", t, hint), cols.saturating_sub(1))
        ));
    }
}

impl Ui for TermUi<'_> {
    fn alert(&mut self, text: &str) {
        self.show(text, "[press a key]");
        self.term.key(-1);
    }
    fn confirm(&mut self, text: &str) -> bool {
        self.show(text, "(y/N)");
        matches!(
            self.term.key(-1),
            Some(Key::Char('y')) | Some(Key::Char('Y'))
        )
    }
    fn prompt(&mut self, text: &str, default: &str) -> Option<String> {
        let (rows, cols) = self.term.size();
        term::edit_line(self.term, rows - 1, cols, text, default, false)
    }
}

/// Selectable things (links and visible fields) in screen order.
fn focusables(p: &Page) -> Vec<(usize, usize, Target)> {
    let mut v: Vec<(usize, usize, Target)> = Vec::new();
    for (i, l) in p.links.iter().enumerate() {
        if let Some((r, c)) = l.pos {
            v.push((r, c, Target::Link(i)));
        }
    }
    for (i, f) in p.fields.iter().enumerate() {
        if let Some((r, c)) = f.pos {
            v.push((r, c, Target::Field(i)));
        }
    }
    v.sort_by_key(|x| (x.0, x.1));
    v
}

fn sgr(style: Style, target: Target, selected: bool) -> String {
    let mut codes: Vec<String> = Vec::new();
    let mut push = |c: &str| codes.push(String::from(c));
    match target {
        Target::Link(_) => {
            push("36");
            if !style.dim {
                push("4");
            }
        }
        Target::Field(_) => push("32"),
        _ => {
            if let Some((r, g, b)) = style.fg {
                codes.push(format!("38;2;{};{};{}", r, g, b));
            } else if style.heading {
                push("33");
            } else if style.dim {
                push("36");
            }
        }
    }
    if let Some((r, g, b)) = style.bg {
        codes.push(format!("48;2;{};{};{}", r, g, b));
    }
    let mut push = |c: &str| codes.push(String::from(c));
    if style.bold || style.heading {
        push("1");
    }
    if style.italic && !matches!(target, Target::Link(_)) {
        push("3");
    }
    if style.underline && !matches!(target, Target::Link(_)) {
        push("4");
    }
    if style.strike {
        push("9");
    }
    if selected {
        push("7");
    }
    format!("\x1b[0;{}m", codes.join(";"))
}

impl App {
    fn entry(&self) -> Option<&Entry> {
        self.hist.get(self.cur)
    }

    fn body_rows(&self) -> usize {
        self.term.size().0.saturating_sub(2)
    }

    fn selected_target(&self) -> Option<Target> {
        let e = self.entry()?;
        let f = focusables(&e.page);
        e.sel.and_then(|s| f.get(s)).map(|x| x.2)
    }

    fn draw(&mut self) {
        let (rows, cols) = self.term.size();
        // Re-lay out after a terminal resize or script changes.
        if let Some(e) = self.hist.get_mut(self.cur) {
            if e.width != cols {
                e.width = cols;
                e.dirty = true;
                if let Some(s) = e.script.as_mut() {
                    s.js.send(&Json::obj([
                        ("t", Json::from("resize")),
                        ("width", Json::from(cols * 8)),
                        ("height", Json::from(rows * 16)),
                    ]));
                }
            }
            if e.dirty {
                relayout(e, &mut self.loader);
                let n = focusables(&e.page).len();
                if e.sel.is_some_and(|s| s >= n) {
                    e.sel = None;
                }
            }
        }
        let body = rows.saturating_sub(2);
        let mut s = String::from("\x1b[H");
        let Some(e) = self.entry() else {
            out("\x1b[H\x1b[2J");
            return;
        };
        let title = if e.page.title.is_empty() {
            e.loaded.url.to_string()
        } else {
            e.page.title.clone()
        };
        let pos = if e.page.lines.len() > body {
            format!(
                " ({}/{})",
                e.top / body.max(1) + 1,
                e.page.lines.len().div_ceil(body.max(1))
            )
        } else {
            String::new()
        };
        let js = if e.scripting() { " [JS]" } else { "" };
        let head = format!(" {}{}{}", title, pos, js);
        s.push_str(&format!("\x1b[0;7m{}\x1b[0m", pad(&head, cols)));
        let sel = self.selected_target();
        for i in 0..body {
            s.push_str(&goto(i + 1, 0));
            if let Some(line) = e.page.lines.get(e.top + i) {
                let mut col = 0;
                let mut field_done: Vec<usize> = Vec::new();
                for span in &line.spans {
                    if col >= cols {
                        break;
                    }
                    let text = match span.target {
                        Target::Field(fi) => {
                            if field_done.contains(&fi) {
                                continue;
                            }
                            field_done.push(fi);
                            field_text(&e.fields[fi])
                        }
                        _ => span.text.clone(),
                    };
                    let text = layout::truncate(&text, cols - col);
                    col += layout::text_width(&text);
                    s.push_str(&sgr(
                        span.style,
                        span.target,
                        sel == Some(span.target) && !matches!(span.target, Target::None),
                    ));
                    s.push_str(&text);
                }
            }
            s.push_str("\x1b[0m\x1b[K");
        }
        // Status line: message, or what is selected.
        let status = if !self.msg.is_empty() {
            self.msg.clone()
        } else if !self.number.is_empty() {
            format!("Link number: {}", self.number)
        } else {
            match sel {
                Some(Target::Link(i)) if e.page.links[i].href.is_empty() => format!(
                    "[*] {} - press Enter to click (script)",
                    e.page.links[i].text
                ),
                Some(Target::Link(i)) => {
                    let href = &e.page.links[i].href;
                    match self.resolve(href) {
                        Some(u) => u.to_string(),
                        None => href.clone(),
                    }
                }
                Some(Target::Field(i)) => describe_field(&e.fields[i]),
                _ => String::from(
                    "Arrows: move  Enter: follow  Left: back  g: go  /: search  h: help  q: quit",
                ),
            }
        };
        s.push_str(&goto(rows - 1, 0));
        s.push_str(&format!(
            "\x1b[0;7m{}\x1b[0m",
            pad(&status, cols.saturating_sub(1))
        ));
        out(&s);
    }

    fn base(&self) -> Option<Url> {
        let e = self.entry()?;
        let u = e.loaded.url.clone();
        match &e.doc.base {
            Some(b) => u.join(b).ok().or(Some(u)),
            None => Some(u),
        }
    }

    fn resolve(&self, href: &str) -> Option<Url> {
        self.base()?.join(href).ok()
    }

    fn show_message(&mut self, m: &str) {
        self.msg = m.to_string();
    }

    // -----------------------------------------------------------------
    // Loading
    // -----------------------------------------------------------------

    fn open(&mut self, req: Request, ctx: Context, push: bool) {
        let target = req.url.to_string();
        self.show_message(&format!("Loading {} ...", target));
        self.draw();
        let url = req.url.clone();
        let fragment = url.fragment.clone();
        let result = loop {
            match self.loader.fetch(req.clone(), ctx) {
                Err(LoadError::Certificate(host, why)) => {
                    let (rows, cols) = self.term.size();
                    let prompt = format!("Certificate problem: {}. Continue anyway? (y/N)", why);
                    out(&format!(
                        "{}\x1b[0;7m{}\x1b[0m",
                        goto(rows - 1, 0),
                        pad(&prompt, cols.saturating_sub(1))
                    ));
                    if matches!(
                        self.term.key(-1),
                        Some(Key::Char('y')) | Some(Key::Char('Y'))
                    ) {
                        self.loader.client.connector.trusted_hosts.push(host);
                        continue;
                    }
                    break Err(format!("Not loaded: server certificate {}", why));
                }
                Err(LoadError::Other(e)) => break Err(e),
                Ok(l) => break Ok(l),
            }
        };
        let loaded = match result {
            Ok(l) => l,
            Err(e) => {
                self.show_message(&format!("Error: {}", e));
                return;
            }
        };
        let width = self.term.size().1;
        let doc = document(&loaded, false);
        let status = loaded.status;
        let referrer = self
            .entry()
            .map(|e| e.loaded.url.to_string())
            .unwrap_or_default();
        self.leave_page();
        let mut entry = Entry::new(loaded, doc, width);
        entry.referrer = referrer;
        if push {
            self.hist.truncate(self.cur + 1);
            self.hist.push(entry);
            self.cur = self.hist.len() - 1;
        } else if self.cur < self.hist.len() {
            self.hist[self.cur] = entry;
        } else {
            self.hist.push(entry);
            self.cur = self.hist.len() - 1;
        }
        self.msg = if status >= 400 {
            format!("HTTP error {}", status)
        } else {
            String::new()
        };
        self.start_page(fragment);
        if self.portal {
            self.check_portal();
        }
        self.auto_refresh();
    }

    /// Stop the scripts of the page being left.
    fn leave_page(&mut self) {
        let App {
            term,
            loader,
            hist,
            cur,
            sessions,
            ..
        } = self;
        if let Some(e) = hist.get_mut(*cur) {
            if e.script.is_some() {
                script::unload(e, loader, sessions, &mut TermUi { term });
                e.script = None;
                e.sockets.clear();
            }
        }
    }

    /// Lay out the current page, running its scripts first.
    fn start_page(&mut self, fragment: Option<String>) {
        let (rows, cols) = self.term.size();
        let body = self.body_rows();
        {
            let App {
                term,
                loader,
                hist,
                cur,
                sessions,
                ..
            } = self;
            let e = &mut hist[*cur];
            e.width = cols;
            if use_js()
                && !e.source
                && e.loaded.mime.contains("html")
                && e.loaded.url.scheme != "about"
                && js::available()
            {
                let referrer = e.referrer.clone();
                if script::start(e, &referrer, cols, rows) {
                    // Until the load event (or 10 s).
                    script::pump(
                        e,
                        loader,
                        sessions,
                        &mut TermUi { term },
                        &mut |m| m.str("t") == Some("loaded"),
                        false,
                        10_000,
                    );
                    if let Some(s) = e.script.as_mut() {
                        s.loaded = true;
                    }
                    script::pump(
                        e,
                        loader,
                        sessions,
                        &mut TermUi { term },
                        &mut |_| false,
                        true,
                        300,
                    );
                }
            }
            relayout(e, loader);
            e.top = 0;
            if let Some(f) = fragment {
                if let Some(&(_, line)) = e.page.anchors.iter().find(|(n, _)| *n == f) {
                    e.top = line;
                }
            }
            // First focusable on the first screen.
            e.sel = focusables(&e.page)
                .iter()
                .position(|x| x.0 >= e.top && x.0 < e.top + body);
        }
        self.after_script();
    }

    /// Carry out what the page's scripts asked for.
    fn after_script(&mut self) {
        loop {
            let Some(e) = self.hist.get_mut(self.cur) else {
                return;
            };
            let Some(s) = e.script.as_mut() else { return };
            if let Some(n) = s.notice.take() {
                self.msg = n;
            }
            if let Some(y) = s.scroll.take() {
                e.top = (y / 16.0).max(0.0) as usize;
            }
            if let Some(node) = s.focus.take() {
                if e.dirty {
                    relayout(e, &mut self.loader);
                }
                let f = focusables(&e.page);
                let want = |t: &Target| match *t {
                    Target::Link(i) => e.page.links[i].node == node,
                    Target::Field(i) => e.fields[i].node == node,
                    _ => false,
                };
                if let Some(k) = f.iter().position(|x| want(&x.2)) {
                    e.sel = Some(k);
                    let line = f[k].0;
                    let body = self.term.size().0.saturating_sub(2);
                    if line < e.top || line >= e.top + body {
                        e.top = line.saturating_sub(body / 3);
                    }
                }
            }
            let Some(s) = e.script.as_mut() else { return };
            if s.actions.is_empty() {
                return;
            }
            let a = s.actions.remove(0);
            s.actions.clear();
            match a {
                Action::Navigate { url, replace } => {
                    let same_site = webclient::httpc::client::site(e.loaded.url.host_str())
                        == webclient::httpc::client::site(url.host_str());
                    self.refreshes = 0;
                    if url.fragment.is_some()
                        && url.without_fragment() == e.loaded.url.without_fragment()
                    {
                        self.navigate(url);
                    } else {
                        self.open(
                            Request::get(url),
                            Context {
                                same_site,
                                top_level_safe: true,
                            },
                            !replace,
                        );
                    }
                }
                Action::Submit(req) => {
                    let same_site = webclient::httpc::client::site(req.url.host_str())
                        == webclient::httpc::client::site(e.loaded.url.host_str());
                    let safe = req.method == "GET";
                    self.refreshes = 0;
                    self.open(
                        req,
                        Context {
                            same_site,
                            top_level_safe: safe,
                        },
                        true,
                    );
                }
                Action::Go(d) => {
                    let to = self.cur as i64 + d as i64;
                    if to >= 0 && (to as usize) < self.hist.len() {
                        self.go_to(to as usize);
                    }
                }
                Action::Close => self.msg = String::from("The page asked to close this window"),
            }
        }
    }

    /// Show history entry `i` (scripts start again, as on a fresh load
    /// from the cache).
    fn go_to(&mut self, i: usize) {
        self.leave_page();
        self.cur = i;
        let (top, sel) = (self.hist[i].top, self.hist[i].sel);
        let e = &mut self.hist[i];
        if !e.source && e.loaded.mime.contains("html") {
            e.doc = document(&e.loaded, false);
            e.overrides.clear();
        }
        e.dirty = true;
        self.start_page(None);
        if let Some(e) = self.hist.get_mut(self.cur) {
            if self.cur == i {
                e.top = top;
                e.sel = sel;
            }
        }
    }

    /// Something arrived from the page's scripts or sockets.
    fn service_page(&mut self) {
        let App {
            term,
            loader,
            hist,
            cur,
            sessions,
            ..
        } = self;
        let Some(e) = hist.get_mut(*cur) else { return };
        sockets::service(e);
        script::drain(e, loader, sessions, &mut TermUi { term });
        self.after_script();
    }

    /// Wait for a key while serving the page's scripts.
    fn next_key(&mut self) -> Option<Key> {
        loop {
            if self.term.has_pending() {
                return self.term.key(-1);
            }
            let mut fds = vec![PollFd {
                fd: 0,
                events: POLLIN,
                revents: 0,
            }];
            if let Some(e) = self.entry() {
                if let Some(s) = e.script.as_ref().filter(|s| !s.js.dead) {
                    if s.js.has_buffered() {
                        self.service_page();
                        self.draw();
                        continue;
                    }
                    fds.push(PollFd {
                        fd: s.js.fd(),
                        events: POLLIN,
                        revents: 0,
                    });
                }
                for fd in sockets::fds(e) {
                    fds.push(PollFd {
                        fd,
                        events: POLLIN,
                        revents: 0,
                    });
                }
            }
            if fds.len() == 1 {
                return self.term.key(-1);
            }
            let _ = io::poll(&mut fds, -1);
            if fds[0].revents != 0 {
                return self.term.key(-1);
            }
            self.service_page();
            self.draw();
        }
    }

    /// Send a user event to the page's scripts; Some(cancelled) when they
    /// ran.
    fn js_event(&mut self, kind: &str, node: NodeId) -> Option<bool> {
        let App {
            term,
            loader,
            hist,
            cur,
            sessions,
            ..
        } = self;
        let e = hist.get_mut(*cur)?;
        if !e.scripting() {
            return None;
        }
        let r = script::event(e, loader, sessions, &mut TermUi { term }, kind, node);
        // Focusing the element clicked: it is already selected.
        if let Some(s) = e.script.as_mut() {
            if s.focus == Some(node) {
                s.focus = None;
            }
        }
        if e.dirty {
            relayout(e, loader);
        }
        r
    }

    /// The JavaScript console: messages, and code to evaluate.
    fn console(&mut self) {
        let (rows, cols) = self.term.size();
        loop {
            let lines: Vec<String> = self
                .entry()
                .and_then(|e| e.script.as_ref())
                .map(|s| s.console.clone())
                .unwrap_or_default();
            let mut s = format!(
                "\x1b[H\x1b[0;7m{}\x1b[0m",
                pad(
                    " JavaScript console (Enter an expression; empty line: back)",
                    cols
                )
            );
            let body = rows.saturating_sub(2);
            let first = lines.len().saturating_sub(body);
            for k in 0..body {
                s.push_str(&goto(k + 1, 0));
                s.push_str("\x1b[0m\x1b[K");
                if let Some(l) = lines.get(first + k) {
                    let color = if l.starts_with("error") {
                        "\x1b[31m"
                    } else if l.starts_with("warn") {
                        "\x1b[33m"
                    } else {
                        ""
                    };
                    s.push_str(color);
                    s.push_str(&layout::truncate(l, cols));
                    s.push_str("\x1b[0m");
                }
            }
            out(&s);
            let Some(code) = term::edit_line(&mut self.term, rows - 1, cols, ">", "", false) else {
                break;
            };
            if code.trim().is_empty() {
                break;
            }
            let App {
                term,
                loader,
                hist,
                cur,
                sessions,
                ..
            } = self;
            let Some(e) = hist.get_mut(*cur) else { break };
            let v = script::eval(e, loader, sessions, &mut TermUi { term }, &code);
            if let Some(sc) = e.script.as_mut() {
                sc.console.push(format!("> {}", code));
                sc.console.push(format!("< {}", v));
            } else {
                self.msg = v;
                break;
            }
        }
        self.after_script();
    }

    fn navigate(&mut self, url: Url) {
        // Same document, different fragment: just scroll.
        if let Some(e) = self.entry() {
            if url.fragment.is_some() && url.without_fragment() == e.loaded.url.without_fragment() {
                let f = url.fragment.clone().unwrap();
                if let Some(&(_, line)) = e.page.anchors.iter().find(|(n, _)| *n == f) {
                    self.hist[self.cur].top = line;
                    return;
                }
            }
        }
        let same_site = self.entry().is_none_or(|e| {
            webclient::httpc::client::site(e.loaded.url.host_str())
                == webclient::httpc::client::site(url.host_str())
        });
        self.refreshes = 0;
        self.open(
            Request::get(url),
            Context {
                same_site,
                top_level_safe: true,
            },
            true,
        );
    }

    /// Follow `<meta http-equiv=refresh>` after a short countdown.
    fn auto_refresh(&mut self) {
        let Some(e) = self.entry() else { return };
        let Some((secs, target)) = e.doc.refresh.clone() else {
            return;
        };
        if secs > 15 || self.refreshes >= 5 {
            if self.msg.is_empty() {
                self.msg = format!("This page refreshes after {} s to {}", secs, target);
            }
            return;
        }
        let url = if target.is_empty() {
            Some(e.loaded.url.clone())
        } else {
            self.resolve(&target)
        };
        let Some(url) = url else { return };
        self.msg = format!("Refreshing to {} in {} s (any key cancels)", url, secs);
        self.draw();
        if self.term.key((secs * 1000).max(50) as i32).is_some() {
            self.msg = String::from("Refresh cancelled");
            return;
        }
        self.refreshes += 1;
        self.open(Request::get(url), Context::USER, true);
    }

    fn check_portal(&mut self) {
        let m = self.msg.clone();
        self.show_message("Checking Internet access ...");
        self.draw();
        match portal::check(4000) {
            portal::Status::Online => {
                self.portal = false;
                self.msg = String::from("Internet access OK - you are logged in. Press q to quit.");
            }
            _ => self.msg = m,
        }
    }

    // -----------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------

    fn move_sel(&mut self, down: bool) {
        let body = self.body_rows();
        let Some(e) = self.hist.get_mut(self.cur) else {
            return;
        };
        let f = focusables(&e.page);
        let visible = |line: usize, top: usize| line >= top && line < top + body;
        let next = match (e.sel, down) {
            (None, true) => f.iter().position(|x| x.0 >= e.top),
            (None, false) => f.iter().rposition(|x| x.0 < e.top + body),
            (Some(s), true) => (s + 1 < f.len()).then_some(s + 1),
            (Some(s), false) => s.checked_sub(1),
        };
        match next {
            // Jump only within reach; otherwise scroll a page first.
            Some(n)
                if visible(f[n].0, e.top)
                    || (down && f[n].0 < e.top + 2 * body)
                    || (!down && f[n].0 + body >= e.top) =>
            {
                e.sel = Some(n);
                let line = f[n].0;
                if !visible(line, e.top) {
                    e.top = if down {
                        line.saturating_sub(body - 1)
                    } else {
                        line
                    };
                }
            }
            _ => {
                let max_top = e.page.lines.len().saturating_sub(body);
                e.top = if down {
                    (e.top + body).min(max_top)
                } else {
                    e.top.saturating_sub(body)
                };
                e.sel = f.iter().position(|x| visible(x.0, e.top));
            }
        }
    }

    fn page(&mut self, down: bool) {
        let body = self.body_rows();
        if let Some(e) = self.hist.get_mut(self.cur) {
            let max_top = e.page.lines.len().saturating_sub(body);
            e.top = if down {
                (e.top + body).min(max_top)
            } else {
                e.top.saturating_sub(body)
            };
            let top = e.top;
            e.sel = focusables(&e.page)
                .iter()
                .position(|x| x.0 >= top && x.0 < top + body);
        }
    }

    fn activate(&mut self) {
        match self.selected_target() {
            Some(Target::Link(i)) => {
                let link = self.hist[self.cur].page.links[i].clone();
                // With scripts, the click runs the page's handlers and
                // the link's default action (a navigate message).
                if self.js_event("click", link.node).is_some() {
                    self.after_script();
                    return;
                }
                if link.href.is_empty() {
                    self.show_message("This needs JavaScript");
                    return;
                }
                match self.resolve(&link.href) {
                    Some(u) => self.navigate(u),
                    None => self.show_message(&format!("Bad link: {}", link.href)),
                }
            }
            Some(Target::Field(i)) => {
                self.field(i);
                self.after_script();
            }
            _ => {}
        }
    }

    /// Tell the page's scripts the user changed a control.
    fn js_input(&mut self, node: NodeId, what: Json) {
        let App {
            term,
            loader,
            hist,
            cur,
            sessions,
            ..
        } = self;
        let Some(e) = hist.get_mut(*cur) else { return };
        if !e.scripting() {
            return;
        }
        script::input(e, node, what);
        script::pump(
            e,
            loader,
            sessions,
            &mut TermUi { term },
            &mut |_| false,
            true,
            500,
        );
        if e.dirty {
            relayout(e, loader);
        }
    }

    fn field(&mut self, i: usize) {
        let (rows, cols) = self.term.size();
        let f = self.hist[self.cur].fields[i].clone();
        if f.disabled {
            self.show_message("This field is disabled");
            return;
        }
        let label = if f.label.is_empty() {
            f.name.clone()
        } else {
            f.label.clone()
        };
        // With scripts: the click (focus, handlers, and for buttons and
        // check boxes the default action) happens in the page.
        if let Some(cancelled) = self.js_event("click", f.node) {
            if cancelled {
                return;
            }
            let node = f.node;
            let find = |e: &Entry| e.fields.iter().position(|x| x.node == node);
            match f.kind {
                FieldKind::Text | FieldKind::Password | FieldKind::File | FieldKind::Textarea => {
                    if f.readonly {
                        return;
                    }
                    let cur = self
                        .entry()
                        .and_then(|e| find(e).map(|k| e.fields[k].value.clone()))
                        .unwrap_or(f.value.clone());
                    let v = if f.kind == FieldKind::Textarea {
                        edit_textarea(&mut self.term, &label, &cur)
                    } else {
                        let prompt = format!("{}:", if label.is_empty() { "Text" } else { &label });
                        term::edit_line(
                            &mut self.term,
                            rows - 1,
                            cols,
                            &prompt,
                            &cur,
                            f.kind == FieldKind::Password,
                        )
                    };
                    if let Some(v) = v {
                        if let Some(k) = self.entry().and_then(find) {
                            self.hist[self.cur].set_override(k, |f, o| {
                                f.value = v.clone();
                                o.value = Some(v.clone());
                            });
                        }
                        self.js_input(node, Json::obj([("value", Json::from(v.as_str()))]));
                        if f.kind != FieldKind::Textarea && f.form.is_some() {
                            let e = &self.hist[self.cur];
                            let has_button = e.fields.iter().any(|x| {
                                x.form == f.form
                                    && matches!(x.kind, FieldKind::Submit | FieldKind::Image)
                            });
                            if !has_button {
                                self.js_event("submit", node);
                                return;
                            }
                        }
                        self.move_sel(true);
                    }
                }
                FieldKind::Select => {
                    let cur = self
                        .entry()
                        .and_then(|e| find(e).map(|k| e.fields[k].clone()))
                        .unwrap_or(f.clone());
                    if let Some(n) = choose(&mut self.term, &label, &cur) {
                        if let Some(k) = self.entry().and_then(find) {
                            self.hist[self.cur].set_override(k, |f, o| {
                                f.selected = n;
                                o.selected = Some(n);
                            });
                        }
                        self.js_input(node, Json::obj([("index", Json::from(n))]));
                    }
                }
                _ => {} // toggled/submitted by the page
            }
            return;
        }
        match f.kind {
            FieldKind::Text | FieldKind::Password | FieldKind::File => {
                if f.readonly {
                    self.show_message("This field is read-only");
                    return;
                }
                let prompt = format!("{}:", if label.is_empty() { "Text" } else { &label });
                if let Some(v) = term::edit_line(
                    &mut self.term,
                    rows - 1,
                    cols,
                    &prompt,
                    &f.value,
                    f.kind == FieldKind::Password,
                ) {
                    self.hist[self.cur].set_override(i, |f, o| {
                        f.value = v.clone();
                        o.value = Some(v);
                    });
                    // Implicit submission: a form without a submit button.
                    if let Some(fi) = f.form {
                        let has_button = self.hist[self.cur].fields.iter().any(|x| {
                            x.form == Some(fi)
                                && matches!(x.kind, FieldKind::Submit | FieldKind::Image)
                        });
                        if !has_button {
                            self.submit(fi, None);
                            return;
                        }
                    }
                    self.move_sel(true);
                }
            }
            FieldKind::Textarea => {
                if let Some(v) = edit_textarea(&mut self.term, &label, &f.value) {
                    self.hist[self.cur].set_override(i, |f, o| {
                        f.value = v.clone();
                        o.value = Some(v);
                    });
                }
            }
            FieldKind::Checkbox => {
                self.hist[self.cur].set_override(i, |f, o| {
                    f.checked = !f.checked;
                    o.checked = Some(f.checked);
                });
            }
            FieldKind::Radio => {
                let e = &mut self.hist[self.cur];
                for k in 0..e.fields.len() {
                    let x = &e.fields[k];
                    if x.kind == FieldKind::Radio && x.name == f.name && x.form == f.form {
                        e.set_override(k, |x, o| {
                            x.checked = k == i;
                            o.checked = Some(k == i);
                        });
                    }
                }
            }
            FieldKind::Select => {
                if let Some(n) = choose(&mut self.term, &label, &f) {
                    self.hist[self.cur].set_override(i, |f, o| {
                        f.selected = n;
                        o.selected = Some(n);
                    });
                }
            }
            FieldKind::Submit | FieldKind::Image => match f.form {
                Some(fi) => self.submit(fi, Some(i)),
                None => self.show_message("This button is not in a form"),
            },
            FieldKind::Reset => {
                let e = &mut self.hist[self.cur];
                for (k, x) in e.fields.iter_mut().enumerate() {
                    if x.form == f.form {
                        e.overrides.remove(&x.node);
                        *x = e.page.fields[k].clone();
                    }
                }
            }
            FieldKind::Button => self.show_message("This button needs JavaScript"),
            FieldKind::Hidden => {}
        }
    }

    fn submit(&mut self, fi: usize, submitter: Option<usize>) {
        let Some(base) = self.base() else { return };
        let e = &self.hist[self.cur];
        match form::submission(&e.page.forms, &e.fields, fi, submitter, &base) {
            Ok(req) => {
                let same_site = webclient::httpc::client::site(req.url.host_str())
                    == webclient::httpc::client::site(e.loaded.url.host_str());
                let safe = req.method == "GET";
                self.refreshes = 0;
                self.open(
                    req,
                    Context {
                        same_site,
                        top_level_safe: safe,
                    },
                    true,
                );
            }
            Err(err) => self.show_message(&format!("Cannot submit: {}", err)),
        }
    }

    fn prompt_url(&mut self, initial: &str) {
        let (rows, cols) = self.term.size();
        if let Some(u) = term::edit_line(
            &mut self.term,
            rows - 1,
            cols,
            "URL to open:",
            initial,
            false,
        ) {
            if u.trim().is_empty() {
                return;
            }
            match Url::from_user_input(&u) {
                Ok(url) => self.navigate(url),
                Err(e) => self.show_message(&format!("Bad URL: {}", e)),
            }
        }
    }

    fn search(&mut self, again: bool) {
        let (rows, cols) = self.term.size();
        if !again || self.search.is_empty() {
            match term::edit_line(
                &mut self.term,
                rows - 1,
                cols,
                "Search for:",
                &self.search.clone(),
                false,
            ) {
                Some(s) if !s.is_empty() => self.search = s,
                _ => return,
            }
        }
        let needle = self.search.to_lowercase();
        let Some(e) = self.hist.get_mut(self.cur) else {
            return;
        };
        let start = if again { e.top + 1 } else { e.top };
        let found = (start..e.page.lines.len())
            .chain(0..start)
            .find(|&i| e.page.lines[i].text().to_lowercase().contains(&needle));
        match found {
            Some(l) => {
                e.top = l;
                let top = e.top;
                e.sel = focusables(&e.page).iter().position(|x| x.0 >= top);
                self.msg = format!("Found '{}' (n: next)", self.search);
            }
            None => self.msg = format!("'{}' not found", self.search),
        }
    }

    fn info(&mut self) {
        let Some(e) = self.entry() else { return };
        let host = e.loaded.url.host_str().to_string();
        let cookies = self
            .loader
            .client
            .jar
            .matching(&e.loaded.url, time::now(), Context::USER)
            .len();
        let mut html = format!(
            "<title>Page information</title><h1>Page information</h1><dl>\
             <dt>URL<dd>{}<dt>Title<dd>{}<dt>Status<dd>{}<dt>Content type<dd>{}\
             <dt>Security<dd>{}<dt>Cookies for {}<dd>{}<dt>Redirects<dd>{}\
             <dt>Size<dd>{} lines, {} links, {} forms, {} fields<dt>Scripts<dd>{}</dl><h2>Response headers</h2><pre>",
            escape(&e.loaded.url.to_string()),
            escape(&e.page.title),
            e.loaded.status,
            escape(&e.loaded.mime),
            if e.loaded.tls.is_empty() {
                String::from("not encrypted")
            } else {
                escape(&e.loaded.tls)
            },
            escape(&host),
            cookies,
            e.loaded.redirects,
            e.page.lines.len(),
            e.page.links.len(),
            e.page.forms.len(),
            e.page.fields.len(),
            if e.scripting() {
                "running"
            } else if use_js() {
                "not running"
            } else {
                "off (-nojs, K)"
            }
        );
        for (k, v) in &e.loaded.headers {
            html.push_str(&format!("{}: {}\n", escape(k), escape(v)));
        }
        html.push_str("</pre>");
        self.show_generated(html);
    }

    fn show_generated(&mut self, html: String) {
        let width = self.term.size().1;
        let loaded = Loaded {
            url: Url::parse("about:blank").unwrap(),
            status: 200,
            mime: String::from("text/html"),
            text: html,
            headers: Vec::new(),
            tls: String::new(),
            redirects: 0,
        };
        let doc = document(&loaded, false);
        self.leave_page();
        let mut e = Entry::new(loaded, doc, width);
        relayout(&mut e, &mut self.loader);
        self.hist.truncate(self.cur + 1);
        self.hist.push(e);
        self.cur = self.hist.len() - 1;
    }

    fn download(&mut self) {
        let Some(Target::Link(i)) = self.selected_target() else {
            self.show_message("Select a link to download first");
            return;
        };
        let href = self.hist[self.cur].page.links[i].href.clone();
        let Some(url) = self.resolve(&href) else {
            return;
        };
        let dir = if fs::is_dir("/storage") {
            "/storage/Downloads"
        } else {
            "/tmp"
        };
        let _ = fs::create_dir_all(dir);
        let name = {
            let n = url.file_name();
            if n.is_empty() {
                String::from("download.html")
            } else {
                n
            }
        };
        let (rows, cols) = self.term.size();
        let Some(path) = term::edit_line(
            &mut self.term,
            rows - 1,
            cols,
            "Save as:",
            &format!("{}/{}", dir, name),
            false,
        ) else {
            return;
        };
        self.show_message(&format!("Downloading {} ...", url));
        self.draw();
        match self.loader.download(url, &path) {
            Ok(n) => self.msg = format!("Saved {} ({} bytes)", path, n),
            Err(e) => self.msg = format!("Download failed: {}", e),
        }
    }

    fn run(&mut self) {
        loop {
            self.draw();
            let Some(k) = self.next_key() else { break };
            self.msg.clear();
            if let Key::Char(c @ '0'..='9') = k {
                self.number.push(c);
                continue;
            }
            if !self.number.is_empty() && matches!(k, Key::Enter | Key::Char('g')) {
                let n: usize = self.number.parse().unwrap_or(0);
                self.number.clear();
                let target = self
                    .entry()
                    .and_then(|e| e.page.links.get(n.wrapping_sub(1)).map(|l| l.href.clone()));
                match target.and_then(|h| self.resolve(&h)) {
                    Some(u) => self.navigate(u),
                    None => self.show_message(&format!("No link number {}", n)),
                }
                continue;
            }
            self.number.clear();
            match k {
                Key::Char('q') | Key::Char('Q') | Key::Ctrl('c') => break,
                Key::Down | Key::Tab => self.move_sel(true),
                Key::Up | Key::BackTab => self.move_sel(false),
                Key::Right | Key::Enter => self.activate(),
                Key::Left | Key::Backspace | Key::Char('u') => {
                    if self.cur > 0 {
                        let to = self.cur - 1;
                        self.go_to(to);
                    } else {
                        self.show_message("Already at the first page");
                    }
                }
                Key::PageDown | Key::Char(' ') | Key::Char('+') => self.page(true),
                Key::PageUp | Key::Char('b') | Key::Char('-') => self.page(false),
                Key::Home => {
                    if let Some(e) = self.hist.get_mut(self.cur) {
                        e.top = 0;
                        e.sel = None;
                    }
                }
                Key::End => {
                    let body = self.body_rows();
                    if let Some(e) = self.hist.get_mut(self.cur) {
                        e.top = e.page.lines.len().saturating_sub(body);
                        e.sel = None;
                    }
                }
                Key::Char('g') => self.prompt_url(""),
                Key::Char('G') => {
                    let u = self
                        .entry()
                        .map(|e| e.loaded.url.to_string())
                        .unwrap_or_default();
                    self.prompt_url(&u);
                }
                Key::Char('/') => self.search(false),
                Key::Char('n') => self.search(true),
                Key::Char('r') | Key::Ctrl('r') => {
                    if let Some(u) = self.entry().map(|e| e.loaded.url.clone()) {
                        self.open(Request::get(u), Context::USER, false);
                    }
                }
                Key::Char('\\') => {
                    let source = !self.entry().is_some_and(|e| e.source);
                    self.leave_page();
                    if let Some(e) = self.hist.get_mut(self.cur) {
                        e.source = source;
                        e.doc = document(&e.loaded, e.source);
                        e.overrides.clear();
                        e.dirty = true;
                    }
                    self.start_page(None);
                }
                Key::Char('J') => self.console(),
                Key::Char('K') => {
                    // SAFETY: single-threaded, between page loads.
                    unsafe { USE_JS = !USE_JS };
                    let on = use_js();
                    let to = self.cur;
                    if !self.hist.is_empty() {
                        self.go_to(to);
                    }
                    self.msg = String::from(if on {
                        "JavaScript on"
                    } else {
                        "JavaScript off"
                    });
                }
                Key::Char('=') => self.info(),
                Key::Char('d') => self.download(),
                Key::Char('c') => self.navigate(Url::parse("about:cookies").unwrap()),
                Key::Char('h') | Key::Char('?') => self.navigate(Url::parse("about:help").unwrap()),
                Key::Ctrl('l') => out("\x1b[2J"),
                _ => {}
            }
        }
        self.leave_page();
        self.loader.save_cookies();
    }
}

fn pad(s: &str, w: usize) -> String {
    let mut t = layout::truncate(s, w);
    for _ in layout::text_width(&t)..w {
        t.push(' ');
    }
    t
}

fn describe_field(f: &Field) -> String {
    let what = match f.kind {
        FieldKind::Text => "Text field",
        FieldKind::Password => "Password field",
        FieldKind::Checkbox => "Check box",
        FieldKind::Radio => "Radio button",
        FieldKind::Select => "Choice list",
        FieldKind::Textarea => "Text area",
        FieldKind::Submit | FieldKind::Image => "Submit button",
        FieldKind::Reset => "Reset button",
        FieldKind::Button => "Button (JavaScript)",
        FieldKind::File => "File (enter a path)",
        FieldKind::Hidden => "Hidden",
    };
    let name = if f.label.is_empty() {
        &f.name
    } else {
        &f.label
    };
    format!(
        "{} {} - press Enter to {}",
        what,
        name,
        match f.kind {
            FieldKind::Submit | FieldKind::Image => "submit",
            FieldKind::Checkbox | FieldKind::Radio => "toggle",
            FieldKind::Select => "choose",
            _ => "edit",
        }
    )
}

/// Pop-up menu for a `<select>`.
fn choose(t: &mut Term, label: &str, f: &Field) -> Option<usize> {
    let (rows, cols) = t.size();
    let h = f.options.len().min(rows.saturating_sub(4)).max(1);
    let w = f
        .options
        .iter()
        .map(|o| layout::text_width(&o.label))
        .max()
        .unwrap_or(10)
        .max(layout::text_width(label))
        .min(cols - 4)
        + 4;
    let (r0, c0) = ((rows - h) / 2, (cols - w) / 2);
    let mut sel = f.selected;
    let mut first = 0;
    loop {
        if sel < first {
            first = sel;
        }
        if sel >= first + h {
            first = sel + 1 - h;
        }
        let mut s = format!(
            "{}\x1b[0;7m{}\x1b[0m",
            goto(r0 - 1, c0),
            pad(&format!(" {}", label), w)
        );
        for k in 0..h {
            let i = first + k;
            let text = f
                .options
                .get(i)
                .map_or(String::new(), |o| format!("  {}", o.label));
            let style = if i == sel {
                "\x1b[0;7;32m"
            } else {
                "\x1b[0;32m"
            };
            s.push_str(&format!(
                "{}{}{}\x1b[0m",
                goto(r0 + k, c0),
                style,
                pad(&text, w)
            ));
        }
        out(&s);
        match t.key(-1)? {
            Key::Up => sel = sel.saturating_sub(1),
            Key::Down => sel = (sel + 1).min(f.options.len().saturating_sub(1)),
            Key::PageUp => sel = sel.saturating_sub(h),
            Key::PageDown => sel = (sel + h).min(f.options.len().saturating_sub(1)),
            Key::Enter | Key::Right => return Some(sel),
            Key::Esc | Key::Left | Key::Ctrl('c') | Key::Char('q') => return None,
            _ => {}
        }
    }
}

/// Full-screen editor for `<textarea>` (Ctrl-X saves, Esc cancels).
fn edit_textarea(t: &mut Term, label: &str, initial: &str) -> Option<String> {
    let mut lines: Vec<Vec<char>> = initial.split('\n').map(|l| l.chars().collect()).collect();
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    let (mut r, mut c) = (lines.len() - 1, lines.last().unwrap().len());
    let mut top = 0;
    out("\x1b[?25h");
    let result = loop {
        let (rows, cols) = t.size();
        let body = rows.saturating_sub(2);
        if r < top {
            top = r;
        }
        if r >= top + body {
            top = r + 1 - body;
        }
        let mut s = format!(
            "\x1b[H\x1b[0;7m{}\x1b[0m",
            pad(
                &format!(" Editing: {}   (Ctrl-X: done, Esc: cancel)", label),
                cols
            )
        );
        for k in 0..body {
            let text: String = lines
                .get(top + k)
                .map(|l| l.iter().collect())
                .unwrap_or_default();
            s.push_str(&format!(
                "{}\x1b[K{}",
                goto(k + 1, 0),
                layout::truncate(&text, cols - 1)
            ));
        }
        s.push_str(&goto(r - top + 1, c.min(cols - 1)));
        out(&s);
        match t.key(-1) {
            None | Some(Key::Esc) | Some(Key::Ctrl('c')) => break None,
            Some(Key::Ctrl('x')) => {
                break Some(
                    lines
                        .iter()
                        .map(|l| l.iter().collect::<String>())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            Some(Key::Enter) => {
                let rest = lines[r].split_off(c);
                lines.insert(r + 1, rest);
                r += 1;
                c = 0;
            }
            Some(Key::Backspace) => {
                if c > 0 {
                    c -= 1;
                    lines[r].remove(c);
                } else if r > 0 {
                    let cur = lines.remove(r);
                    r -= 1;
                    c = lines[r].len();
                    lines[r].extend(cur);
                }
            }
            Some(Key::Left) => {
                if c > 0 {
                    c -= 1;
                } else if r > 0 {
                    r -= 1;
                    c = lines[r].len();
                }
            }
            Some(Key::Right) => {
                if c < lines[r].len() {
                    c += 1;
                } else if r + 1 < lines.len() {
                    r += 1;
                    c = 0;
                }
            }
            Some(Key::Up) => {
                r = r.saturating_sub(1);
                c = c.min(lines[r].len());
            }
            Some(Key::Down) => {
                r = (r + 1).min(lines.len() - 1);
                c = c.min(lines[r].len());
            }
            Some(Key::Home) => c = 0,
            Some(Key::End) => c = lines[r].len(),
            Some(Key::Char(ch)) => {
                lines[r].insert(c, ch);
                c += 1;
            }
            _ => {}
        }
    };
    out("\x1b[?25l");
    result
}

/// The captive-portal login page: announced, remembered, or probed.
fn portal_url() -> Option<Url> {
    if let Ok(u) = fs::read_to_string(portal::STATE_FILE) {
        if let Ok(url) = Url::parse(u.trim()) {
            return Some(url);
        }
    }
    if let Some((_, _, u)) = portal::announced() {
        return Url::parse(&u).ok();
    }
    match portal::check(5000) {
        portal::Status::Portal(Some(u)) => Url::parse(&u).ok(),
        _ => None,
    }
}

/// Run a page's scripts in batch mode: until the load event, then while
/// they stay busy (timers, requests; at most `settle_ms`). Returns what
/// they asked for (a navigation or a form submission).
fn batch_scripts(
    e: &mut Entry,
    loader: &mut Loader,
    sessions: &mut Sessions,
    settle_ms: u64,
) -> Option<Action> {
    if !script::start(e, "", e.width, 24) {
        return None;
    }
    let mut ui = BatchUi;
    let start = rustos_rt::time::millis();
    script::pump(
        e,
        loader,
        sessions,
        &mut ui,
        &mut |m| m.str("t") == Some("loaded"),
        false,
        10_000,
    );
    // Keep serving timers, requests and sockets until quiet for 300 ms.
    let mut quiet_since = rustos_rt::time::millis();
    loop {
        let now = rustos_rt::time::millis();
        if now - start > settle_ms || now - quiet_since > 300 {
            break;
        }
        if e.script.as_ref().is_some_and(|s| !s.actions.is_empty()) {
            break;
        }
        let mut fds = Vec::new();
        if let Some(s) = e.script.as_ref().filter(|s| !s.js.dead) {
            fds.push(PollFd {
                fd: s.js.fd(),
                events: POLLIN,
                revents: 0,
            });
        } else {
            break;
        }
        for fd in sockets::fds(e) {
            fds.push(PollFd {
                fd,
                events: POLLIN,
                revents: 0,
            });
        }
        let buffered = e.script.as_ref().is_some_and(|s| s.js.has_buffered());
        if buffered || io::poll(&mut fds, 50).unwrap_or(0) > 0 {
            sockets::service(e);
            script::drain(e, loader, sessions, &mut ui);
            quiet_since = rustos_rt::time::millis();
        }
    }
    let s = e.script.as_mut()?;
    for l in &s.console {
        if l.starts_with("error") || l.starts_with("warn") || l.starts_with("log") {
            eprintln!("js {}", l);
        }
    }
    let a = if s.actions.is_empty() {
        None
    } else {
        Some(s.actions.remove(0))
    };
    a
}

fn batch(url: Url, insecure: bool, source: bool, width: usize) -> i32 {
    let mut loader = Loader::new(insecure);
    let mut sessions = Sessions::default();
    let mut req = Request::get(url);
    let mut ctx = Context::USER;
    // Script navigations are followed (up to 5).
    for _ in 0..6 {
        let l = match loader.fetch(req.clone(), ctx) {
            Ok(l) => l,
            Err(LoadError::Certificate(_, e)) => {
                eprintln!(
                    "browse: server certificate {} (use -k to continue anyway)",
                    e
                );
                return 1;
            }
            Err(LoadError::Other(e)) => {
                eprintln!("browse: {}", e);
                return 1;
            }
        };
        if source {
            print!("{}", l.text);
            loader.save_cookies();
            return if l.status >= 400 { 1 } else { 0 };
        }
        let status = l.status;
        let doc = document(&l, false);
        let mut e = Entry::new(l, doc, width);
        let mut next = None;
        if use_js()
            && e.loaded.mime.contains("html")
            && e.loaded.url.scheme != "about"
            && js::available()
        {
            next = batch_scripts(&mut e, &mut loader, &mut sessions, 5000);
        }
        let from = e.loaded.url.clone();
        match next {
            Some(Action::Navigate { url, .. })
                if url.without_fragment() != from.without_fragment() =>
            {
                eprintln!("browse: script navigated to {}", url);
                ctx = Context {
                    same_site: webclient::httpc::client::site(url.host_str())
                        == webclient::httpc::client::site(from.host_str()),
                    top_level_safe: true,
                };
                req = Request::get(url);
                continue;
            }
            Some(Action::Submit(r)) => {
                eprintln!("browse: script submitted a form to {}", r.url);
                ctx = Context {
                    same_site: webclient::httpc::client::site(r.url.host_str())
                        == webclient::httpc::client::site(from.host_str()),
                    top_level_safe: r.method == "GET",
                };
                req = r;
                continue;
            }
            _ => {}
        }
        relayout(&mut e, &mut loader);
        let mut page = e.page.clone();
        // References are listed as absolute URLs.
        let base = e
            .doc
            .base
            .as_deref()
            .and_then(|b| e.loaded.url.join(b).ok())
            .unwrap_or_else(|| e.loaded.url.clone());
        for link in &mut page.links {
            if link.href.is_empty() {
                link.href = String::from("(script)");
            } else if let Ok(u) = base.join(&link.href) {
                link.href = u.to_string();
            }
        }
        // Field values as scripts left them.
        page.fields = e.fields.clone();
        print!("{}", layout::dump(&page));
        drop(e);
        loader.save_cookies();
        return if status >= 400 { 1 } else { 0 };
    }
    eprintln!("browse: too many script navigations");
    1
}

#[allow(dead_code)]
fn batch_plain(url: Url, insecure: bool, width: usize) -> i32 {
    let mut loader = Loader::new(insecure);
    match loader.fetch(Request::get(url), Context::USER) {
        Ok(l) => {
            let doc = document(&l, false);
            let sheets = fetch_sheets(&mut loader, &l, &doc, width);
            let page = render(&doc, &sheets, width);
            print!("{}", layout::dump(&page));
            if l.status >= 400 { 1 } else { 0 }
        }
        Err(LoadError::Certificate(_, e)) => {
            eprintln!(
                "browse: server certificate {} (use -k to continue anyway)",
                e
            );
            1
        }
        Err(LoadError::Other(e)) => {
            eprintln!("browse: {}", e);
            1
        }
    }
}

fn main(args: Vec<String>) -> i32 {
    let mut insecure = false;
    let mut dump = false;
    let mut source = false;
    let mut portal_mode = false;
    let mut width = 0usize;
    let mut graphical = false;
    let mut png: Option<String> = None;
    let mut size = (800u32, 600u32);
    let mut full = false;
    let mut target: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-k" | "--insecure" => insecure = true,
            // SAFETY: before any page is loaded (single-threaded).
            "-nocss" | "--nocss" => unsafe { USE_CSS = false },
            // SAFETY: as above.
            "-nojs" | "--nojs" => unsafe { USE_JS = false },
            "-dump" | "--dump" => dump = true,
            "-source" | "--source" => source = true,
            "--portal" | "-portal" => portal_mode = true,
            "-g" | "--graphical" => graphical = true,
            "-full" | "--full" => full = true,
            "-dump-png" | "--dump-png" => {
                i += 1;
                png = args.get(i).cloned();
            }
            "-size" | "--size" => {
                i += 1;
                if let Some((w, h)) = args.get(i).and_then(|s| s.split_once('x')) {
                    size = (
                        w.parse().unwrap_or(800).clamp(64, 4096),
                        h.parse().unwrap_or(600).clamp(64, 4096),
                    );
                }
            }
            "-width" | "--width" => {
                i += 1;
                width = args.get(i).and_then(|w| w.parse().ok()).unwrap_or(0);
            }
            "-h" | "--help" => {
                println!("{}", USAGE);
                return 0;
            }
            a if a.starts_with('-') => {
                eprintln!("browse: unknown option {}\n{}", a, USAGE);
                return 2;
            }
            a => target = Some(a.to_string()),
        }
        i += 1;
    }
    let url = match (&target, portal_mode) {
        (Some(t), _) => match Url::from_user_input(t) {
            Ok(u) => u,
            Err(e) => {
                eprintln!("browse: {}: {}", t, e);
                return 2;
            }
        },
        (None, true) => match portal_url() {
            Some(u) => u,
            None => {
                println!(
                    "browse: no captive portal found ({})",
                    portal::describe(&portal::check(5000))
                );
                return 1;
            }
        },
        (None, false) => Url::parse("about:help").unwrap(),
    };
    if let Some(out) = png {
        return gfx::dump_png(url, insecure, &out, size.0, size.1, full);
    }
    if graphical {
        return gfx::run(url, insecure);
    }
    if dump || source || !io::isatty(1) {
        let w = if width > 0 {
            width
        } else if io::isatty(1) {
            rustos_rt::term::size(1).1 as usize
        } else {
            80
        };
        return batch(url, insecure, source, w);
    }
    let mut app = App {
        term: Term::enter(),
        loader: Loader::new(insecure),
        hist: Vec::new(),
        cur: 0,
        msg: String::new(),
        search: String::new(),
        number: String::new(),
        portal: portal_mode,
        refreshes: 0,
        sessions: Sessions::default(),
    };
    out("\x1b[2J");
    app.navigate(url);
    app.run();
    0
}
