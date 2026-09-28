//! browse: a lynx-like text web browser.
//!
//! Pages are fetched with `webclient` (HTTP/1.1, HTTPS with TLS 1.2/1.3,
//! cookies, redirects), parsed by `html`, laid out by `textlayout` and
//! drawn with ANSI escapes. Links and form fields are navigated with the
//! arrow keys; forms can be filled in and submitted (GET, POST,
//! multipart). `browse --portal` opens a captive-portal login page and
//! reports when Internet access works. No JavaScript, no CSS layout.

#![no_std]
#![no_main]

extern crate alloc;

mod form;
mod load;
mod term;

use load::{LoadError, Loaded, Loader};
use rustos_rt::prelude::*;
use rustos_rt::{fs, io, time};
use term::{goto, out, Key, Term};
use textlayout::{field_text, Field, FieldKind, Options, Page, Style, Target};
use webclient::httpc::cookie::Context;
use webclient::httpc::{Request, Url};
use webclient::portal;

rustos_rt::entry!(main);

const USAGE: &str = "usage: browse [-k] [-dump|-source] [-width N] [--portal] [URL]";

/// One document in the history.
struct Entry {
    loaded: Loaded,
    doc: html::Document,
    page: Page,
    fields: Vec<Field>,
    top: usize,
    sel: Option<usize>,
    source: bool,
    width: usize,
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
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Render loaded content as a page.
fn layout(l: &Loaded, width: usize, source: bool) -> (html::Document, Page) {
    let src = if source || !l.mime.contains("html") {
        let title = if source { format!("Source of {}", l.url) } else { l.url.to_string() };
        format!("<title>{}</title><pre>{}</pre>", escape(&title), escape(&l.text))
    } else {
        l.text.clone()
    };
    let doc = html::parse(&src);
    let page = textlayout::render(&doc, &Options { width, number_links: true });
    (doc, page)
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
    let mut codes: Vec<&str> = Vec::new();
    match target {
        Target::Link(_) => {
            codes.push("36");
            if !style.dim {
                codes.push("4");
            }
        }
        Target::Field(_) => codes.push("32"),
        _ => {
            if style.heading {
                codes.push("33");
            }
            if style.dim {
                codes.push("36");
            }
        }
    }
    if style.bold || style.heading {
        codes.push("1");
    }
    if (style.underline || style.italic) && !matches!(target, Target::Link(_)) {
        codes.push("4");
    }
    if selected {
        codes.push("7");
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
        // Re-lay out after a terminal resize.
        if let Some(e) = self.hist.get_mut(self.cur) {
            if e.width != cols {
                let fields = e.fields.clone();
                let (doc, page) = layout(&e.loaded, cols, e.source);
                e.doc = doc;
                e.page = page;
                if e.page.fields.len() == fields.len() {
                    for (a, b) in e.page.fields.iter_mut().zip(fields) {
                        a.value = b.value;
                        a.checked = b.checked;
                        a.selected = b.selected;
                    }
                }
                e.fields = e.page.fields.clone();
                e.width = cols;
            }
        }
        let body = rows.saturating_sub(2);
        let mut s = String::from("\x1b[H");
        let Some(e) = self.entry() else {
            out("\x1b[H\x1b[2J");
            return;
        };
        let title = if e.page.title.is_empty() { e.loaded.url.to_string() } else { e.page.title.clone() };
        let pos = if e.page.lines.len() > body {
            format!(" ({}/{})", e.top / body.max(1) + 1, e.page.lines.len().div_ceil(body.max(1)))
        } else {
            String::new()
        };
        let head = format!(" {}{}", title, pos);
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
                    let text = textlayout::truncate(&text, cols - col);
                    col += textlayout::text_width(&text);
                    s.push_str(&sgr(span.style, span.target, sel == Some(span.target) && !matches!(span.target, Target::None)));
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
                Some(Target::Link(i)) => {
                    let href = &e.page.links[i].href;
                    match self.resolve(href) {
                        Some(u) => u.to_string(),
                        None => href.clone(),
                    }
                }
                Some(Target::Field(i)) => describe_field(&e.fields[i]),
                _ => String::from("Arrows: move  Enter: follow  Left: back  g: go  /: search  h: help  q: quit"),
            }
        };
        s.push_str(&goto(rows - 1, 0));
        s.push_str(&format!("\x1b[0;7m{}\x1b[0m", pad(&status, cols.saturating_sub(1))));
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

    fn scroll_to_line(&mut self, line: usize) {
        let body = self.body_rows();
        if let Some(e) = self.hist.get_mut(self.cur) {
            if line < e.top || line >= e.top + body {
                e.top = line.saturating_sub(body / 3);
            }
        }
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
                    out(&format!("{}\x1b[0;7m{}\x1b[0m", goto(rows - 1, 0), pad(&prompt, cols.saturating_sub(1))));
                    if matches!(self.term.key(-1), Some(Key::Char('y')) | Some(Key::Char('Y'))) {
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
        let (doc, page) = layout(&loaded, width, false);
        let status = loaded.status;
        let fields = page.fields.clone();
        let mut entry = Entry {
            loaded,
            doc,
            page,
            fields,
            top: 0,
            sel: None,
            source: false,
            width,
        };
        if let Some(f) = fragment {
            if let Some(&(_, line)) = entry.page.anchors.iter().find(|(n, _)| *n == f) {
                entry.top = line;
            }
        }
        // First focusable on the first screen.
        let body = self.body_rows();
        entry.sel = focusables(&entry.page).iter().position(|x| x.0 >= entry.top && x.0 < entry.top + body);
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
        self.msg = if status >= 400 { format!("HTTP error {}", status) } else { String::new() };
        if self.portal {
            self.check_portal();
        }
        self.auto_refresh();
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
        let same_site = self
            .entry()
            .is_none_or(|e| webclient::httpc::client::site(e.loaded.url.host_str()) == webclient::httpc::client::site(url.host_str()));
        self.refreshes = 0;
        self.open(Request::get(url), Context { same_site, top_level_safe: true }, true);
    }

    /// Follow `<meta http-equiv=refresh>` after a short countdown.
    fn auto_refresh(&mut self) {
        let Some(e) = self.entry() else { return };
        let Some((secs, target)) = e.doc.refresh.clone() else { return };
        if secs > 15 || self.refreshes >= 5 {
            if self.msg.is_empty() {
                self.msg = format!("This page refreshes after {} s to {}", secs, target);
            }
            return;
        }
        let url = if target.is_empty() { Some(e.loaded.url.clone()) } else { self.resolve(&target) };
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
        let Some(e) = self.hist.get_mut(self.cur) else { return };
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
            Some(n) if visible(f[n].0, e.top) || (down && f[n].0 < e.top + 2 * body) || (!down && f[n].0 + body >= e.top) => {
                e.sel = Some(n);
                let line = f[n].0;
                if !visible(line, e.top) {
                    e.top = if down { line.saturating_sub(body - 1) } else { line };
                }
            }
            _ => {
                let max_top = e.page.lines.len().saturating_sub(body);
                e.top = if down { (e.top + body).min(max_top) } else { e.top.saturating_sub(body) };
                e.sel = f.iter().position(|x| visible(x.0, e.top));
            }
        }
    }

    fn page(&mut self, down: bool) {
        let body = self.body_rows();
        if let Some(e) = self.hist.get_mut(self.cur) {
            let max_top = e.page.lines.len().saturating_sub(body);
            e.top = if down { (e.top + body).min(max_top) } else { e.top.saturating_sub(body) };
            let top = e.top;
            e.sel = focusables(&e.page).iter().position(|x| x.0 >= top && x.0 < top + body);
        }
    }

    fn activate(&mut self) {
        match self.selected_target() {
            Some(Target::Link(i)) => {
                let href = self.hist[self.cur].page.links[i].href.clone();
                match self.resolve(&href) {
                    Some(u) => self.navigate(u),
                    None => self.show_message(&format!("Bad link: {}", href)),
                }
            }
            Some(Target::Field(i)) => self.field(i),
            _ => {}
        }
    }

    fn field(&mut self, i: usize) {
        let (rows, cols) = self.term.size();
        let f = self.hist[self.cur].fields[i].clone();
        if f.disabled {
            self.show_message("This field is disabled");
            return;
        }
        let label = if f.label.is_empty() { f.name.clone() } else { f.label.clone() };
        match f.kind {
            FieldKind::Text | FieldKind::Password | FieldKind::File => {
                if f.readonly {
                    self.show_message("This field is read-only");
                    return;
                }
                let prompt = format!("{}:", if label.is_empty() { "Text" } else { &label });
                if let Some(v) = term::edit_line(&mut self.term, rows - 1, cols, &prompt, &f.value, f.kind == FieldKind::Password) {
                    self.hist[self.cur].fields[i].value = v;
                    // Implicit submission: a form without a submit button.
                    if let Some(fi) = f.form {
                        let has_button = self.hist[self.cur]
                            .fields
                            .iter()
                            .any(|x| x.form == Some(fi) && matches!(x.kind, FieldKind::Submit | FieldKind::Image));
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
                    self.hist[self.cur].fields[i].value = v;
                }
            }
            FieldKind::Checkbox => {
                let c = &mut self.hist[self.cur].fields[i].checked;
                *c = !*c;
            }
            FieldKind::Radio => {
                for (k, x) in self.hist[self.cur].fields.iter_mut().enumerate() {
                    if x.kind == FieldKind::Radio && x.name == f.name && x.form == f.form {
                        x.checked = k == i;
                    }
                }
            }
            FieldKind::Select => {
                if let Some(n) = choose(&mut self.term, &label, &f) {
                    self.hist[self.cur].fields[i].selected = n;
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
                        *x = e.page.fields[k].clone();
                    }
                }
            }
            FieldKind::Button => self.show_message("This button needs JavaScript (not supported)"),
            FieldKind::Hidden => {}
        }
    }

    fn submit(&mut self, fi: usize, submitter: Option<usize>) {
        let Some(base) = self.base() else { return };
        let e = &self.hist[self.cur];
        match form::submission(&e.page.forms, &e.fields, fi, submitter, &base) {
            Ok(req) => {
                let same_site = webclient::httpc::client::site(req.url.host_str()) == webclient::httpc::client::site(e.loaded.url.host_str());
                let safe = req.method == "GET";
                self.refreshes = 0;
                self.open(req, Context { same_site, top_level_safe: safe }, true);
            }
            Err(err) => self.show_message(&format!("Cannot submit: {}", err)),
        }
    }

    fn prompt_url(&mut self, initial: &str) {
        let (rows, cols) = self.term.size();
        if let Some(u) = term::edit_line(&mut self.term, rows - 1, cols, "URL to open:", initial, false) {
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
            match term::edit_line(&mut self.term, rows - 1, cols, "Search for:", &self.search.clone(), false) {
                Some(s) if !s.is_empty() => self.search = s,
                _ => return,
            }
        }
        let needle = self.search.to_lowercase();
        let Some(e) = self.hist.get_mut(self.cur) else { return };
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
             <dt>Size<dd>{} lines, {} links, {} forms, {} fields</dl><h2>Response headers</h2><pre>",
            escape(&e.loaded.url.to_string()),
            escape(&e.page.title),
            e.loaded.status,
            escape(&e.loaded.mime),
            if e.loaded.tls.is_empty() { String::from("not encrypted") } else { escape(&e.loaded.tls) },
            escape(&host),
            cookies,
            e.loaded.redirects,
            e.page.lines.len(),
            e.page.links.len(),
            e.page.forms.len(),
            e.page.fields.len()
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
        let (doc, page) = layout(&loaded, width, false);
        let fields = page.fields.clone();
        self.hist.truncate(self.cur + 1);
        self.hist.push(Entry { loaded, doc, page, fields, top: 0, sel: None, source: false, width });
        self.cur = self.hist.len() - 1;
    }

    fn download(&mut self) {
        let Some(Target::Link(i)) = self.selected_target() else {
            self.show_message("Select a link to download first");
            return;
        };
        let href = self.hist[self.cur].page.links[i].href.clone();
        let Some(url) = self.resolve(&href) else { return };
        let dir = if fs::is_dir("/storage") { "/storage/Downloads" } else { "/tmp" };
        let _ = fs::create_dir_all(dir);
        let name = {
            let n = url.file_name();
            if n.is_empty() { String::from("download.html") } else { n }
        };
        let (rows, cols) = self.term.size();
        let Some(path) = term::edit_line(&mut self.term, rows - 1, cols, "Save as:", &format!("{}/{}", dir, name), false) else {
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
            let Some(k) = self.term.key(-1) else { break };
            self.msg.clear();
            if let Key::Char(c @ '0'..='9') = k {
                self.number.push(c);
                continue;
            }
            if !self.number.is_empty() && matches!(k, Key::Enter | Key::Char('g')) {
                let n: usize = self.number.parse().unwrap_or(0);
                self.number.clear();
                let target = self.entry().and_then(|e| e.page.links.get(n.wrapping_sub(1)).map(|l| l.href.clone()));
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
                        self.cur -= 1;
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
                    let u = self.entry().map(|e| e.loaded.url.to_string()).unwrap_or_default();
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
                    let width = self.term.size().1;
                    if let Some(e) = self.hist.get_mut(self.cur) {
                        e.source = !e.source;
                        let (doc, page) = layout(&e.loaded, width, e.source);
                        e.doc = doc;
                        e.page = page;
                        e.fields = e.page.fields.clone();
                        e.top = 0;
                        e.sel = None;
                    }
                }
                Key::Char('=') => self.info(),
                Key::Char('d') => self.download(),
                Key::Char('c') => self.navigate(Url::parse("about:cookies").unwrap()),
                Key::Char('h') | Key::Char('?') => self.navigate(Url::parse("about:help").unwrap()),
                Key::Ctrl('l') => out("\x1b[2J"),
                _ => {}
            }
        }
        self.loader.save_cookies();
    }
}

fn pad(s: &str, w: usize) -> String {
    let mut t = textlayout::truncate(s, w);
    for _ in textlayout::text_width(&t)..w {
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
    let name = if f.label.is_empty() { &f.name } else { &f.label };
    format!("{} {} - press Enter to {}", what, name, match f.kind {
        FieldKind::Submit | FieldKind::Image => "submit",
        FieldKind::Checkbox | FieldKind::Radio => "toggle",
        FieldKind::Select => "choose",
        _ => "edit",
    })
}

/// Pop-up menu for a `<select>`.
fn choose(t: &mut Term, label: &str, f: &Field) -> Option<usize> {
    let (rows, cols) = t.size();
    let h = f.options.len().min(rows.saturating_sub(4)).max(1);
    let w = f.options.iter().map(|o| textlayout::text_width(&o.label)).max().unwrap_or(10).max(textlayout::text_width(label)).min(cols - 4) + 4;
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
        let mut s = format!("{}\x1b[0;7m{}\x1b[0m", goto(r0 - 1, c0), pad(&format!(" {}", label), w));
        for k in 0..h {
            let i = first + k;
            let text = f.options.get(i).map_or(String::new(), |o| format!("  {}", o.label));
            let style = if i == sel { "\x1b[0;7;32m" } else { "\x1b[0;32m" };
            s.push_str(&format!("{}{}{}\x1b[0m", goto(r0 + k, c0), style, pad(&text, w)));
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
        let mut s = format!("\x1b[H\x1b[0;7m{}\x1b[0m", pad(&format!(" Editing: {}   (Ctrl-X: done, Esc: cancel)", label), cols));
        for k in 0..body {
            let text: String = lines.get(top + k).map(|l| l.iter().collect()).unwrap_or_default();
            s.push_str(&format!("{}\x1b[K{}", goto(k + 1, 0), textlayout::truncate(&text, cols - 1)));
        }
        s.push_str(&goto(r - top + 1, c.min(cols - 1)));
        out(&s);
        match t.key(-1) {
            None | Some(Key::Esc) | Some(Key::Ctrl('c')) => break None,
            Some(Key::Ctrl('x')) => break Some(lines.iter().map(|l| l.iter().collect::<String>()).collect::<Vec<_>>().join("\n")),
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

fn batch(url: Url, insecure: bool, source: bool, width: usize) -> i32 {
    let mut loader = Loader::new(insecure);
    match loader.fetch(Request::get(url), Context::USER) {
        Ok(l) => {
            if source {
                print!("{}", l.text);
            } else {
                let (doc, mut page) = layout(&l, width, false);
                // References are listed as absolute URLs.
                let base = doc.base.as_deref().and_then(|b| l.url.join(b).ok()).unwrap_or_else(|| l.url.clone());
                for link in &mut page.links {
                    if let Ok(u) = base.join(&link.href) {
                        link.href = u.to_string();
                    }
                }
                print!("{}", textlayout::dump(&page));
            }
            loader.save_cookies();
            if l.status >= 400 { 1 } else { 0 }
        }
        Err(LoadError::Certificate(_, e)) => {
            eprintln!("browse: server certificate {} (use -k to continue anyway)", e);
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
    let mut target: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-k" | "--insecure" => insecure = true,
            "-dump" | "--dump" => dump = true,
            "-source" | "--source" => source = true,
            "--portal" | "-portal" => portal_mode = true,
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
                println!("browse: no captive portal found ({})", portal::describe(&portal::check(5000)));
                return 1;
            }
        },
        (None, false) => Url::parse("about:help").unwrap(),
    };
    if dump || source || !io::isatty(1) {
        let w = if width > 0 { width } else if io::isatty(1) { rustos_rt::term::size(1).1 as usize } else { 80 };
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
    };
    out("\x1b[2J");
    app.navigate(url);
    app.run();
    0
}
