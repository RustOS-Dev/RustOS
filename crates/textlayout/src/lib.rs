//! Text-mode layout of HTML documents for the `browse` browser.
//!
//! [`render`] walks an [`html::Document`] and produces a [`Page`]: wrapped
//! lines of styled spans, plus the links, form fields, forms and anchors
//! found on the page with their screen positions. Blocks, lists, headings,
//! preformatted text and tables (column layout, falling back to a linear
//! layout when too narrow) are supported. Form controls are rendered as
//! fixed-width widgets (`[text____]`, `[X]`, `(*)`, `[choice v]`,
//! `[ Submit ]`) whose text the browser regenerates with
//! [`field_text`] as the user edits them.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use html::{Document, NodeId, NodeKind};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub underline: bool,
    pub italic: bool,
    pub heading: bool,
    /// Link numbers and other decoration.
    pub dim: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    None,
    Link(usize),
    Field(usize),
    /// Zero-width marker for a fragment identifier.
    Anchor(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    pub target: Target,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn width(&self) -> usize {
        self.spans.iter().map(|s| text_width(&s.text)).sum()
    }
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Link {
    pub href: String,
    pub text: String,
    /// First screen position (line, column).
    pub pos: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Password,
    Hidden,
    Checkbox,
    Radio,
    Select,
    Textarea,
    Submit,
    Image,
    Reset,
    Button,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub kind: FieldKind,
    /// The `type` attribute as written (`email`, `search`, ...).
    pub input_type: String,
    pub name: String,
    pub value: String,
    pub checked: bool,
    pub options: Vec<SelectOption>,
    pub selected: usize,
    /// Owning form (index into [`Page::forms`]).
    pub form: Option<usize>,
    /// Display width of the value area.
    pub size: usize,
    pub disabled: bool,
    pub readonly: bool,
    pub id: Option<String>,
    /// Text of an associated `<label>`, or the placeholder.
    pub label: String,
    /// Submit-button overrides.
    pub formaction: Option<String>,
    pub formmethod: Option<String>,
    pub formenctype: Option<String>,
    pub pos: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    pub action: String,
    /// `get` or `post`.
    pub method: String,
    pub enctype: String,
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Page {
    pub title: String,
    pub lines: Vec<Line>,
    pub links: Vec<Link>,
    pub fields: Vec<Field>,
    pub forms: Vec<Form>,
    /// Fragment identifiers and their line.
    pub anchors: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub width: usize,
    /// Prefix links with `[n]` as lynx does.
    pub number_links: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            width: 80,
            number_links: true,
        }
    }
}

/// Columns a character occupies (wide East Asian characters take two;
/// combining marks and zero-width characters none).
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    if u < 0x300 {
        return 1;
    }
    if (0x300..0x370).contains(&u) || (0x200B..=0x200F).contains(&u) || u == 0xFEFF {
        return 0;
    }
    if (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE4F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x1F300..=0x1FAFF).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u)
    {
        return 2;
    }
    1
}

pub fn text_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Cut `s` to at most `w` columns.
pub fn truncate(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if used + cw > w {
            break;
        }
        used += cw;
        out.push(c);
    }
    out
}

fn pad(s: &str, w: usize, fill: char) -> String {
    let mut out = truncate(s, w);
    for _ in text_width(&out)..w {
        out.push(fill);
    }
    out
}

/// Display text of a form field with its current state.
pub fn field_text(f: &Field) -> String {
    match f.kind {
        FieldKind::Hidden => String::new(),
        FieldKind::Text | FieldKind::File => {
            let shown = if f.kind == FieldKind::File {
                if f.value.is_empty() { String::from("(no file)") } else { f.value.clone() }
            } else if f.value.is_empty() && !f.label.is_empty() {
                // Placeholder-ish hint is not shown; keep underscores.
                String::new()
            } else {
                // Show the end of long values (where the cursor is).
                let w = text_width(&f.value);
                if w > f.size {
                    let skip = w - f.size;
                    let mut acc = 0;
                    f.value
                        .chars()
                        .skip_while(|c| {
                            let r = acc < skip;
                            acc += char_width(*c);
                            r
                        })
                        .collect()
                } else {
                    f.value.clone()
                }
            };
            format!("[{}]", pad(&shown, f.size, '_'))
        }
        FieldKind::Password => {
            let n = f.value.chars().count().min(f.size);
            let mut s: String = core::iter::repeat_n('*', n).collect();
            s = pad(&s, f.size, '_');
            format!("[{}]", s)
        }
        FieldKind::Checkbox => String::from(if f.checked { "[X]" } else { "[ ]" }),
        FieldKind::Radio => String::from(if f.checked { "(*)" } else { "( )" }),
        FieldKind::Select => {
            let label = f.options.get(f.selected).map_or("", |o| o.label.as_str());
            format!("[{} v]", pad(label, f.size, ' '))
        }
        FieldKind::Textarea => {
            let first = f.value.lines().next().unwrap_or("");
            let more = f.value.lines().count() > 1;
            let shown = if more { format!("{}…", first) } else { first.to_string() };
            format!("[{}]", pad(&shown, f.size, '_'))
        }
        FieldKind::Submit | FieldKind::Reset | FieldKind::Button | FieldKind::Image => {
            let label = if !f.label.is_empty() {
                f.label.clone()
            } else if !f.value.is_empty() {
                f.value.clone()
            } else {
                String::from(match f.kind {
                    FieldKind::Reset => "Reset",
                    FieldKind::Button => "Button",
                    _ => "Submit",
                })
            };
            format!("[ {} ]", label)
        }
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

struct ListCtx {
    ordered: bool,
    next: i64,
}

struct Ctx<'a> {
    doc: &'a Document,
    opts: Options,
    width: usize,
    out: Vec<Line>,
    cur: Vec<Span>,
    cur_w: usize,
    indent: usize,
    /// Indent for the current line only (hanging list markers).
    first_indent: Option<usize>,
    space: bool,
    blank: usize,
    started: bool,
    style: Style,
    link: Option<usize>,
    pre: usize,
    lists: Vec<ListCtx>,
    form: Option<usize>,
    links: Vec<Link>,
    fields: Vec<Field>,
    forms: Vec<Form>,
    anchors: Vec<String>,
    /// `<label for>` texts, applied after layout.
    labels: Vec<(String, String)>,
}

fn attr<'d>(doc: &'d Document, id: NodeId, name: &str) -> Option<&'d str> {
    doc.attr(id, name)
}

impl<'a> Ctx<'a> {
    fn avail(&self) -> usize {
        self.width.saturating_sub(self.indent).max(10)
    }

    fn flush_line(&mut self) {
        let spans = core::mem::take(&mut self.cur);
        self.out.push(Line { spans });
        self.cur_w = 0;
        self.space = false;
        self.first_indent = None;
    }

    fn start_line_if_needed(&mut self) {
        if self.cur.is_empty() {
            if self.started && self.blank > 0 {
                for _ in 0..self.blank {
                    self.out.push(Line::default());
                }
            }
            self.blank = 0;
            self.started = true;
            let ind = self.first_indent.unwrap_or(self.indent);
            if ind > 0 {
                self.cur.push(Span {
                    text: " ".repeat(ind),
                    style: Style::default(),
                    target: Target::None,
                });
            }
            self.cur_w = ind;
        }
    }

    /// End the current line; request `blank` empty lines before the next
    /// content (collapsing with earlier requests).
    fn block(&mut self, blank: usize) {
        if !self.cur.is_empty() {
            self.flush_line();
        }
        self.blank = self.blank.max(blank);
        self.space = false;
    }

    fn hard_break(&mut self) {
        if self.cur.is_empty() {
            self.start_line_if_needed();
        }
        self.flush_line();
    }

    fn push_span(&mut self, text: String, style: Style, target: Target) {
        self.cur_w += text_width(&text);
        // Merge with the previous span when possible.
        if let Some(last) = self.cur.last_mut() {
            if last.style == style && last.target == target && !matches!(target, Target::Anchor(_)) {
                last.text.push_str(&text);
                return;
            }
        }
        self.cur.push(Span { text, style, target });
    }

    fn target(&self) -> Target {
        self.link.map_or(Target::None, Target::Link)
    }

    /// Add an unbreakable unit (a word, a widget).
    fn atom(&mut self, text: &str, style: Style, target: Target) {
        let w = text_width(text);
        self.start_line_if_needed();
        let sp = usize::from(self.space && self.cur_w > self.first_indent.unwrap_or(self.indent));
        if self.cur_w + sp + w > self.width.max(self.indent + 10) && self.cur_w > self.first_indent.unwrap_or(self.indent) {
            self.flush_line();
            self.start_line_if_needed();
        } else if sp == 1 {
            // The space takes the style of the text it joins, so headings
            // stay underlined and multi-word links one highlighted unit.
            let t = self.target();
            self.push_span(String::from(" "), style, t);
        }
        // Words longer than a line are split.
        if self.cur_w + w > self.width && w > self.avail() {
            let mut rest = text;
            while !rest.is_empty() {
                let room = self.width.saturating_sub(self.cur_w).max(1);
                let part = truncate(rest, room);
                let part = if part.is_empty() { rest.chars().next().unwrap().to_string() } else { part };
                rest = &rest[part.len()..];
                self.push_span(part, style, target);
                if !rest.is_empty() {
                    self.flush_line();
                    self.start_line_if_needed();
                }
            }
        } else {
            self.push_span(text.to_string(), style, target);
        }
        self.space = false;
    }

    fn text(&mut self, t: &str) {
        if self.pre > 0 {
            let mut first = true;
            for line in t.split('\n') {
                if !first {
                    self.hard_break();
                }
                first = false;
                if line.is_empty() {
                    continue;
                }
                self.start_line_if_needed();
                let expanded = line.replace('\t', "        ").replace('\r', "");
                let style = self.style;
                let target = self.target();
                self.push_span(expanded, style, target);
            }
            return;
        }
        if t.starts_with(|c: char| c.is_whitespace()) {
            self.space = true;
        }
        let ends_ws = t.ends_with(|c: char| c.is_whitespace());
        let mut words = t.split_whitespace().peekable();
        while let Some(w) = words.next() {
            let (style, target) = (self.style, self.target());
            self.atom(w, style, target);
            if words.peek().is_some() {
                self.space = true;
            }
        }
        if ends_ws {
            self.space = true;
        }
        // Text inside a link names it.
        if let Some(l) = self.link {
            let link = &mut self.links[l];
            if !link.text.is_empty() && t.starts_with(char::is_whitespace) {
                link.text.push(' ');
            }
            link.text.push_str(&html::collapse_ws(t));
        }
    }

    fn anchor(&mut self, name: &str) {
        self.start_line_if_needed();
        let i = self.anchors.len();
        self.anchors.push(name.to_string());
        self.cur.push(Span {
            text: String::new(),
            style: Style::default(),
            target: Target::Anchor(i),
        });
    }

    fn with_style(&mut self, id: NodeId, f: impl FnOnce(&mut Style)) {
        let saved = self.style;
        f(&mut self.style);
        self.children(id);
        self.style = saved;
    }

    fn children(&mut self, id: NodeId) {
        let kids = self.doc.nodes[id].children.clone();
        for c in kids {
            self.node(c);
        }
    }

    fn node(&mut self, id: NodeId) {
        let doc = self.doc;
        let (tag, _) = match &doc.nodes[id].kind {
            NodeKind::Text(t) => {
                let t = t.clone();
                self.text(&t);
                return;
            }
            NodeKind::Document => {
                self.children(id);
                return;
            }
            NodeKind::Element { tag, attrs } => (tag.as_str(), attrs),
        };
        if let Some(i) = attr(doc, id, "id") {
            self.anchor(i);
        }
        if attr(doc, id, "hidden").is_some() || attr(doc, id, "style").is_some_and(|s| {
            let s = s.replace(' ', "").to_ascii_lowercase();
            s.contains("display:none") || s.contains("visibility:hidden")
        }) {
            // Hidden form inputs still count.
            for d in doc.descendants(id) {
                if doc.tag(d) == "input" {
                    self.input(d);
                }
            }
            return;
        }
        match tag {
            "head" | "script" | "style" | "template" | "title" | "meta" | "link" | "base" | "svg"
            | "math" | "canvas" | "object" | "embed" | "audio" | "video" | "source" | "track"
            | "param" | "datalist" | "map" | "area" => {}
            "br" => self.hard_break(),
            "wbr" => {}
            "hr" => {
                self.block(0);
                self.start_line_if_needed();
                let w = self.avail().min(self.width);
                let dim = Style { dim: true, ..Style::default() };
                self.push_span("─".repeat(w), dim, Target::None);
                self.block(0);
            }
            "p" | "address" | "figure" | "fieldset" | "center" | "main" | "article" | "section"
            | "header" | "footer" | "nav" | "aside" | "hgroup" | "figcaption" | "details" | "dialog" => {
                let blank = usize::from(matches!(tag, "p" | "figure" | "fieldset" | "details" | "dialog"));
                self.block(blank);
                self.children(id);
                self.block(blank);
            }
            "div" | "caption" | "noscript" | "legend" | "summary" | "option" | "optgroup" => {
                self.block(0);
                match tag {
                    "legend" | "summary" | "caption" => self.with_style(id, |s| s.bold = true),
                    _ => self.children(id),
                }
                self.block(0);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.block(1);
                let big = matches!(tag, "h1" | "h2");
                self.with_style(id, |s| {
                    s.bold = true;
                    s.heading = true;
                    s.underline |= big;
                });
                self.block(1);
            }
            "pre" | "listing" | "xmp" | "plaintext" => {
                self.block(1);
                self.pre += 1;
                // A newline right after <pre> is ignored.
                let kids = doc.nodes[id].children.clone();
                for (k, c) in kids.into_iter().enumerate() {
                    if k == 0 {
                        if let NodeKind::Text(t) = &doc.nodes[c].kind {
                            let t = t.strip_prefix('\n').unwrap_or(t).to_string();
                            self.text(&t);
                            continue;
                        }
                    }
                    self.node(c);
                }
                self.pre -= 1;
                self.block(1);
            }
            "blockquote" => {
                self.block(1);
                self.indent += 4;
                self.children(id);
                self.indent -= 4;
                self.block(1);
            }
            "ul" | "ol" | "menu" | "dir" => {
                let nested = !self.lists.is_empty();
                self.block(usize::from(!nested));
                let start = attr(doc, id, "start").and_then(|s| s.trim().parse().ok()).unwrap_or(1);
                self.lists.push(ListCtx {
                    ordered: tag == "ol",
                    next: start,
                });
                self.children(id);
                self.lists.pop();
                self.block(usize::from(!nested));
            }
            "li" => {
                self.block(0);
                let depth = self.lists.len();
                let marker = match self.lists.last_mut() {
                    Some(l) if l.ordered => {
                        if let Some(v) = attr(doc, id, "value").and_then(|v| v.trim().parse().ok()) {
                            l.next = v;
                        }
                        let m = format!("{}. ", l.next);
                        l.next += 1;
                        m
                    }
                    _ => String::from(["* ", "+ ", "- "][depth.saturating_sub(1) % 3]),
                };
                let base = self.indent;
                self.first_indent = Some(base);
                self.start_line_if_needed();
                let dim = Style { bold: true, ..Style::default() };
                self.push_span(marker.clone(), dim, Target::None);
                self.indent = base + text_width(&marker);
                self.space = false;
                self.children(id);
                self.indent = base;
                self.block(0);
            }
            "dl" => {
                self.block(1);
                self.children(id);
                self.block(1);
            }
            "dt" => {
                self.block(0);
                self.with_style(id, |s| s.bold = true);
                self.block(0);
            }
            "dd" => {
                self.block(0);
                self.indent += 4;
                self.children(id);
                self.indent -= 4;
                self.block(0);
            }
            "a" => {
                let href = attr(doc, id, "href").map(|h| h.trim().to_string());
                if let Some(n) = attr(doc, id, "name") {
                    self.anchor(n);
                }
                match href {
                    Some(h) if !h.is_empty() && !h.to_ascii_lowercase().starts_with("javascript:") => {
                        let idx = self.links.len();
                        self.links.push(Link {
                            href: h,
                            text: String::new(),
                            pos: None,
                        });
                        let outer = self.link;
                        if self.opts.number_links {
                            let dim = Style { dim: true, ..Style::default() };
                            self.atom(&format!("[{}]", idx + 1), dim, Target::Link(idx));
                        }
                        self.link = Some(idx);
                        let before = self.cur_w + self.out.len() * 1000;
                        self.with_style(id, |s| s.underline = true);
                        let after = self.cur_w + self.out.len() * 1000;
                        if after == before && !self.opts.number_links {
                            // Empty link (e.g. an image without alt).
                            let name = self.links[idx].href.rsplit('/').next().unwrap_or("link").to_string();
                            let st = Style { underline: true, ..self.style };
                            self.atom(&format!("[{}]", name), st, Target::Link(idx));
                        }
                        if self.links[idx].text.trim().is_empty() {
                            self.links[idx].text = attr(doc, id, "title").unwrap_or("").to_string();
                        }
                        self.link = outer;
                    }
                    _ => self.children(id),
                }
            }
            "img" => {
                let alt = attr(doc, id, "alt").map(html::collapse_ws);
                let text = match alt {
                    Some(a) if !a.is_empty() => Some(format!("[{}]", a)),
                    // Unlabelled images only matter inside links.
                    _ if self.link.is_some() => Some(String::from("[IMG]")),
                    _ => None,
                };
                if let Some(t) = text {
                    let (st, tg) = (self.style, self.target());
                    self.atom(&t, st, tg);
                    if let Some(l) = self.link {
                        if self.links[l].text.is_empty() {
                            self.links[l].text = t;
                        }
                    }
                }
            }
            "iframe" | "frame" => {
                if let Some(src) = attr(doc, id, "src") {
                    self.block(0);
                    let idx = self.links.len();
                    let title = attr(doc, id, "title").unwrap_or("frame").to_string();
                    self.links.push(Link {
                        href: src.to_string(),
                        text: title.clone(),
                        pos: None,
                    });
                    let st = Style { underline: true, ..Style::default() };
                    self.atom(&format!("[{}: {}]", tag, title), st, Target::Link(idx));
                    self.block(0);
                }
            }
            "b" | "strong" => self.with_style(id, |s| s.bold = true),
            "i" | "em" | "cite" | "var" | "dfn" => self.with_style(id, |s| s.italic = true),
            "u" | "ins" => self.with_style(id, |s| s.underline = true),
            "q" => {
                let st = self.style;
                let tg = self.target();
                self.atom("“", st, tg);
                self.children(id);
                self.push_span(String::from("”"), st, tg);
            }
            "sup" => {
                let (st, tg) = (self.style, self.target());
                self.push_span(String::from("^"), st, tg);
                self.children(id);
            }
            "table" => self.table(id),
            "form" => {
                self.block(0);
                let idx = self.forms.len();
                self.forms.push(Form {
                    action: attr(doc, id, "action").unwrap_or("").trim().to_string(),
                    method: attr(doc, id, "method").unwrap_or("get").trim().to_ascii_lowercase(),
                    enctype: attr(doc, id, "enctype")
                        .unwrap_or("application/x-www-form-urlencoded")
                        .trim()
                        .to_ascii_lowercase(),
                    id: attr(doc, id, "id").map(String::from),
                });
                let outer = self.form.replace(idx);
                self.children(id);
                self.form = outer;
                self.block(0);
            }
            "input" => self.input(id),
            "button" => {
                let ty = attr(doc, id, "type").unwrap_or("submit").to_ascii_lowercase();
                let kind = match ty.as_str() {
                    "reset" => FieldKind::Reset,
                    "button" => FieldKind::Button,
                    _ => FieldKind::Submit,
                };
                let label = html::collapse_ws(&doc.text_content(id));
                self.add_field(id, kind, &ty, label, 0);
            }
            "select" => self.select(id),
            "textarea" => {
                let value = doc.text_content(id);
                let value = value.strip_prefix('\n').unwrap_or(&value).to_string();
                let cols = attr(doc, id, "cols").and_then(|c| c.parse().ok()).unwrap_or(40usize);
                let mut f = self.new_field(id, FieldKind::Textarea, "textarea");
                f.value = value;
                f.size = cols.min(self.avail().saturating_sub(2)).max(8);
                self.place_field(f);
            }
            "label" => {
                if let Some(for_id) = attr(doc, id, "for") {
                    self.labels.push((for_id.to_string(), html::collapse_ws(&doc.text_content(id))));
                }
                self.children(id);
            }
            _ => self.children(id),
        }
    }

    fn new_field(&self, id: NodeId, kind: FieldKind, ty: &str) -> Field {
        let doc = self.doc;
        let a = |n: &str| attr(doc, id, n).map(String::from);
        Field {
            kind,
            input_type: ty.to_string(),
            name: a("name").unwrap_or_default(),
            value: a("value").unwrap_or_default(),
            checked: a("checked").is_some(),
            options: Vec::new(),
            selected: 0,
            form: match a("form") {
                // Resolved against form ids after layout.
                Some(f) => {
                    let found = self.forms.iter().position(|x| x.id.as_deref() == Some(f.as_str()));
                    found.or(self.form)
                }
                None => self.form,
            },
            size: 20,
            disabled: a("disabled").is_some(),
            readonly: a("readonly").is_some(),
            id: a("id"),
            label: a("placeholder").or_else(|| a("aria-label")).unwrap_or_default(),
            formaction: a("formaction"),
            formmethod: a("formmethod").map(|m| m.to_ascii_lowercase()),
            formenctype: a("formenctype").map(|m| m.to_ascii_lowercase()),
            pos: None,
        }
    }

    fn place_field(&mut self, f: Field) {
        let idx = self.fields.len();
        let text = field_text(&f);
        let hidden = f.kind == FieldKind::Hidden;
        self.fields.push(f);
        if !hidden {
            let st = Style::default();
            self.atom(&text, st, Target::Field(idx));
            self.space = true;
        }
    }

    fn add_field(&mut self, id: NodeId, kind: FieldKind, ty: &str, label: String, size: usize) {
        let mut f = self.new_field(id, kind, ty);
        if !label.is_empty() {
            f.label = label;
        }
        if size > 0 {
            f.size = size;
        }
        self.place_field(f);
    }

    fn input(&mut self, id: NodeId) {
        let doc = self.doc;
        let ty = attr(doc, id, "type").unwrap_or("text").trim().to_ascii_lowercase();
        let kind = match ty.as_str() {
            "password" => FieldKind::Password,
            "hidden" => FieldKind::Hidden,
            "checkbox" => FieldKind::Checkbox,
            "radio" => FieldKind::Radio,
            "submit" => FieldKind::Submit,
            "image" => FieldKind::Image,
            "reset" => FieldKind::Reset,
            "button" => FieldKind::Button,
            "file" => FieldKind::File,
            _ => FieldKind::Text,
        };
        let mut f = self.new_field(id, kind, &ty);
        match kind {
            FieldKind::Text | FieldKind::Password => {
                let size = attr(doc, id, "size").and_then(|s| s.trim().parse().ok()).unwrap_or(20usize);
                f.size = size.clamp(4, self.avail().saturating_sub(2).max(4));
            }
            FieldKind::Checkbox | FieldKind::Radio => {
                if f.value.is_empty() {
                    f.value = String::from("on");
                }
            }
            FieldKind::Image => {
                f.label = attr(doc, id, "alt").unwrap_or("Submit").to_string();
            }
            FieldKind::Submit | FieldKind::Reset | FieldKind::Button => f.label = String::new(),
            FieldKind::File => f.size = 20,
            _ => {}
        }
        self.place_field(f);
    }

    fn select(&mut self, id: NodeId) {
        let doc = self.doc;
        let mut f = self.new_field(id, FieldKind::Select, "select");
        let mut sel = None;
        for d in doc.descendants(id) {
            if doc.tag(d) == "option" {
                let label = attr(doc, d, "label")
                    .map(String::from)
                    .unwrap_or_else(|| html::collapse_ws(&doc.text_content(d)));
                let value = attr(doc, d, "value").map(String::from).unwrap_or_else(|| label.clone());
                if attr(doc, d, "selected").is_some() && sel.is_none() {
                    sel = Some(f.options.len());
                }
                f.options.push(SelectOption { value, label });
            }
        }
        f.selected = sel.unwrap_or(0);
        let w = f.options.iter().map(|o| text_width(&o.label)).max().unwrap_or(1);
        f.size = w.clamp(1, self.avail().saturating_sub(4).max(4));
        self.place_field(f);
    }

    // ---------------------------------------------------------------
    // Tables
    // ---------------------------------------------------------------

    /// Render a subtree into lines at `width`, sharing link/field state.
    fn sub_render(&mut self, id: NodeId, width: usize) -> Vec<Line> {
        let saved = (
            core::mem::take(&mut self.out),
            core::mem::take(&mut self.cur),
            self.cur_w,
            self.indent,
            self.first_indent,
            self.space,
            self.blank,
            self.started,
            self.width,
            core::mem::take(&mut self.lists),
        );
        self.cur_w = 0;
        self.indent = 0;
        self.first_indent = None;
        self.space = false;
        self.blank = 0;
        self.started = false;
        self.width = width.max(1);
        self.children(id);
        if !self.cur.is_empty() {
            self.flush_line();
        }
        let mut lines = core::mem::take(&mut self.out);
        while lines.last().is_some_and(|l| l.width() == 0 && l.spans.iter().all(|s| !matches!(s.target, Target::Anchor(_)))) {
            lines.pop();
        }
        (
            self.out,
            self.cur,
            self.cur_w,
            self.indent,
            self.first_indent,
            self.space,
            self.blank,
            self.started,
            self.width,
            self.lists,
        ) = saved;
        lines
    }

    /// Render a subtree only to measure it (no links/fields are kept).
    fn measure(&mut self, id: NodeId, width: usize) -> usize {
        let marks = (self.links.len(), self.fields.len(), self.forms.len(), self.anchors.len(), self.labels.len());
        let lines = self.sub_render(id, width);
        self.links.truncate(marks.0);
        self.fields.truncate(marks.1);
        self.forms.truncate(marks.2);
        self.anchors.truncate(marks.3);
        self.labels.truncate(marks.4);
        lines.iter().map(|l| l.width()).max().unwrap_or(0)
    }

    fn rows(&self, table: NodeId) -> Vec<Vec<(NodeId, usize)>> {
        let doc = self.doc;
        let mut rows = Vec::new();
        let mut stack = vec![table];
        let mut tr_list = Vec::new();
        while let Some(n) = stack.pop() {
            for &c in doc.nodes[n].children.iter().rev() {
                match doc.tag(c) {
                    "tr" => tr_list.push(c),
                    "thead" | "tbody" | "tfoot" => stack.push(c),
                    _ => {}
                }
            }
        }
        // Node ids follow document order.
        tr_list.sort_unstable();
        for tr in tr_list {
            let cells: Vec<(NodeId, usize)> = doc.nodes[tr]
                .children
                .iter()
                .filter(|&&c| matches!(doc.tag(c), "td" | "th"))
                .map(|&c| {
                    let span = attr(doc, c, "colspan").and_then(|s| s.trim().parse().ok()).unwrap_or(1usize);
                    (c, span.clamp(1, 20))
                })
                .collect();
            if !cells.is_empty() {
                rows.push(cells);
            }
        }
        rows
    }

    fn table(&mut self, id: NodeId) {
        let rows = self.rows(id);
        self.block(0);
        // Caption above.
        if let Some(cap) = self.doc.nodes[id].children.iter().copied().find(|&c| self.doc.tag(c) == "caption") {
            self.node(cap);
        }
        if rows.is_empty() {
            for c in self.doc.nodes[id].children.clone() {
                if self.doc.tag(c) != "caption" {
                    self.node(c);
                }
            }
            self.block(0);
            return;
        }
        const GAP: usize = 2;
        let ncols = rows.iter().map(|r| r.iter().map(|c| c.1).sum::<usize>()).max().unwrap_or(1);
        let avail = self.avail();
        // Natural width of each column (single-span cells).
        let mut nat = vec![0usize; ncols];
        let mut minw = vec![1usize; ncols];
        for r in &rows {
            let mut col = 0;
            for &(cell, span) in r {
                if span == 1 && col < ncols {
                    let w = self.measure(cell, avail);
                    nat[col] = nat[col].max(w);
                    let longest = self
                        .doc
                        .text_content(cell)
                        .split_whitespace()
                        .map(text_width)
                        .max()
                        .unwrap_or(1);
                    minw[col] = minw[col].max(longest.min(20));
                }
                col += span;
            }
        }
        let total_gap = GAP * (ncols - 1);
        let sum: usize = nat.iter().sum();
        let widths: Vec<usize> = if sum + total_gap <= avail {
            nat.clone()
        } else if ncols * 6 + total_gap <= avail {
            let room = avail - total_gap;
            let mut w: Vec<usize> = nat
                .iter()
                .zip(&minw)
                .map(|(&n, &m)| (n * room / sum.max(1)).max(m.min(room / ncols)).max(3))
                .collect();
            // Trim the widest columns until everything fits.
            while w.iter().sum::<usize>() > room {
                let (i, _) = w.iter().enumerate().max_by_key(|(_, v)| **v).unwrap();
                w[i] -= 1;
            }
            w
        } else {
            // Too narrow for columns: one cell after another.
            for r in &rows {
                for &(cell, _) in r {
                    self.block(0);
                    self.children(cell);
                }
                self.block(0);
            }
            return;
        };
        for r in rows {
            let mut cols: Vec<(Vec<Line>, usize)> = Vec::new();
            let mut col = 0;
            for &(cell, span) in &r {
                let end = (col + span).min(ncols);
                let w = widths[col..end].iter().sum::<usize>() + GAP * (end - col).saturating_sub(1);
                let saved = self.style;
                if self.doc.tag(cell) == "th" {
                    self.style.bold = true;
                }
                let lines = self.sub_render(cell, w.max(1));
                self.style = saved;
                cols.push((lines, w));
                col = end;
            }
            let height = cols.iter().map(|c| c.0.len()).max().unwrap_or(0);
            for k in 0..height {
                self.start_line_if_needed();
                for (ci, (lines, w)) in cols.iter().enumerate() {
                    let mut used = 0;
                    if let Some(l) = lines.get(k) {
                        for s in &l.spans {
                            used += text_width(&s.text);
                            self.push_span(s.text.clone(), s.style, s.target);
                        }
                    }
                    if ci + 1 < cols.len() {
                        let fill = w.saturating_sub(used) + GAP;
                        self.push_span(" ".repeat(fill), Style::default(), Target::None);
                    }
                }
                self.flush_line();
            }
        }
        self.block(0);
    }
}

/// Lay out a document.
pub fn render(doc: &Document, opts: &Options) -> Page {
    let mut c = Ctx {
        doc,
        opts: *opts,
        width: opts.width.max(20),
        out: Vec::new(),
        cur: Vec::new(),
        cur_w: 0,
        indent: 0,
        first_indent: None,
        space: false,
        blank: 0,
        started: false,
        style: Style::default(),
        link: None,
        pre: 0,
        lists: Vec::new(),
        form: None,
        links: Vec::new(),
        fields: Vec::new(),
        forms: Vec::new(),
        anchors: Vec::new(),
        labels: Vec::new(),
    };
    c.children(0);
    if !c.cur.is_empty() {
        c.flush_line();
    }
    // Trim trailing whitespace from lines and trailing empty lines.
    for l in &mut c.out {
        while l.spans.last().is_some_and(|s| s.text.trim().is_empty() && !matches!(s.target, Target::Anchor(_) | Target::Field(_))) {
            l.spans.pop();
        }
        if let Some(s) = l.spans.last_mut() {
            if s.target == Target::None {
                let t = s.text.trim_end().to_string();
                s.text = t;
            }
        }
    }
    while c.out.last().is_some_and(|l| l.spans.is_empty()) {
        c.out.pop();
    }
    let mut page = Page {
        title: doc.title.clone(),
        lines: c.out,
        links: c.links,
        fields: c.fields,
        forms: c.forms,
        anchors: Vec::new(),
    };
    for (id, label) in c.labels {
        if let Some(f) = page.fields.iter_mut().find(|f| f.id.as_deref() == Some(id.as_str())) {
            if f.label.is_empty() || matches!(f.kind, FieldKind::Text | FieldKind::Password | FieldKind::Checkbox | FieldKind::Radio | FieldKind::Select | FieldKind::Textarea) {
                f.label = label;
            }
        }
    }
    // Positions of links, fields and anchors.
    for (li, line) in page.lines.iter().enumerate() {
        let mut col = 0;
        for s in &line.spans {
            match s.target {
                Target::Link(i) if page.links[i].pos.is_none() => page.links[i].pos = Some((li, col)),
                Target::Field(i) if page.fields[i].pos.is_none() => page.fields[i].pos = Some((li, col)),
                Target::Anchor(i) => page.anchors.push((c.anchors[i].clone(), li)),
                _ => {}
            }
            col += text_width(&s.text);
        }
    }
    page
}

/// Plain-text rendering (`browse -dump`): the lines, then the list of
/// link targets.
pub fn dump(page: &Page) -> String {
    let mut s = String::new();
    for l in &page.lines {
        s.push_str(l.text().trim_end());
        s.push('\n');
    }
    if !page.links.is_empty() {
        s.push_str("\nReferences\n\n");
        for (i, l) in page.links.iter().enumerate() {
            s.push_str(&format!("{:>4}. {}\n", i + 1, l.href));
        }
    }
    s
}

#[cfg(test)]
mod tests;
