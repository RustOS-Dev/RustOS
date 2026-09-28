//! A tolerant HTML parser for the `browse` text browser.
//!
//! The tokenizer follows the HTML5 tokenizer loosely (tags, attributes in
//! any quoting style, comments, doctypes, raw-text elements, character
//! references). The tree builder handles what real pages rely on without
//! the full HTML5 insertion-mode machinery: void elements, implied end
//! tags (`p`, `li`, `dt`/`dd`, `option`, table rows and cells), and
//! mis-nested or stray end tags. The result is a simple DOM plus the
//! document metadata a text browser needs (`<title>`, `<base>`,
//! `<meta http-equiv=refresh>`).

#![no_std]

extern crate alloc;

pub mod entities;

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Start {
        name: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    End(String),
    Text(String),
    Comment(String),
    Doctype(String),
}

/// Decode character references in `s` (`&amp;`, `&#233;`, `&#xE9;`, and
/// the legacy forms without `;` for common names).
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'&' {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        let rest = &s[i + 1..];
        if let Some(num) = rest.strip_prefix('#') {
            let (hex, digits) = match num.strip_prefix(['x', 'X']) {
                Some(h) => (true, h),
                None => (false, num),
            };
            let len = digits
                .bytes()
                .take_while(|c| if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() })
                .count();
            if len > 0 {
                let v = u32::from_str_radix(&digits[..len.min(8)], if hex { 16 } else { 10 }).unwrap_or(0xFFFD);
                out.push(entities::numeric(v));
                let mut consumed = 1 + usize::from(hex) + len;
                if digits.as_bytes().get(len) == Some(&b';') {
                    consumed += 1;
                }
                i += 1 + consumed;
                continue;
            }
        } else {
            let len = rest.bytes().take_while(|c| c.is_ascii_alphanumeric()).count();
            if len > 0 {
                let name = &rest[..len];
                let semi = rest.as_bytes().get(len) == Some(&b';');
                if let Some(v) = entities::named(name) {
                    // Without ';' only the legacy Latin-1 names are honoured.
                    if semi || matches!(name, "amp" | "lt" | "gt" | "quot" | "nbsp" | "copy" | "reg") {
                        out.push_str(v);
                        i += 1 + len + usize::from(semi);
                        continue;
                    }
                }
            }
        }
        out.push('&');
        i += 1;
    }
    out
}

/// Elements whose content is raw text up to the matching end tag.
fn is_raw_text(name: &str) -> bool {
    matches!(name, "script" | "style" | "textarea" | "title" | "xmp" | "iframe" | "noembed" | "plaintext")
}

/// Split HTML into tokens.
pub fn tokenize(src: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0;
    let mut text_start = 0;
    let flush_text = |out: &mut Vec<Token>, from: usize, to: usize| {
        if to > from {
            out.push(Token::Text(decode_entities(&src[from..to])));
        }
    };
    while i < n {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        // Comments, doctypes, CDATA.
        if src[i..].starts_with("<!--") {
            flush_text(&mut out, text_start, i);
            let end = src[i + 4..].find("-->").map_or(n, |e| i + 4 + e);
            out.push(Token::Comment(src[i + 4..end].to_string()));
            i = (end + 3).min(n);
            text_start = i;
            continue;
        }
        if b.get(i + 1) == Some(&b'!') || b.get(i + 1) == Some(&b'?') {
            flush_text(&mut out, text_start, i);
            let end = src[i..].find('>').map_or(n, |e| i + e);
            let body = &src[i + 2..end];
            if body.len() >= 7 && body[..7].eq_ignore_ascii_case("doctype") {
                out.push(Token::Doctype(body[7..].trim().to_string()));
            } else if let Some(cdata) = body.strip_prefix("[CDATA[") {
                out.push(Token::Text(cdata.trim_end_matches("]]").to_string()));
            } else {
                out.push(Token::Comment(body.to_string()));
            }
            i = (end + 1).min(n);
            text_start = i;
            continue;
        }
        let end_tag = b.get(i + 1) == Some(&b'/');
        let name_start = i + 1 + usize::from(end_tag);
        if !b.get(name_start).is_some_and(|c| c.is_ascii_alphabetic()) {
            // "<" followed by something else is text ("a < b", "</ >").
            i += 1;
            continue;
        }
        flush_text(&mut out, text_start, i);
        let mut j = name_start;
        while j < n && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
            j += 1;
        }
        let name = src[name_start..j].to_ascii_lowercase();
        // Attributes.
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_closing = false;
        loop {
            while j < n && (b[j].is_ascii_whitespace() || (b[j] == b'/' && b.get(j + 1) != Some(&b'>'))) {
                j += 1;
            }
            if j >= n {
                break;
            }
            if b[j] == b'>' {
                j += 1;
                break;
            }
            if b[j] == b'/' {
                self_closing = true;
                j += 2;
                break;
            }
            let a0 = j;
            while j < n && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'=' && !(b[j] == b'/' && b.get(j + 1) == Some(&b'>')) {
                j += 1;
            }
            let aname = src[a0..j].to_ascii_lowercase();
            while j < n && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let mut value = String::new();
            if j < n && b[j] == b'=' {
                j += 1;
                while j < n && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < n && (b[j] == b'"' || b[j] == b'\'') {
                    let q = b[j];
                    let v0 = j + 1;
                    let v1 = src[v0..].find(q as char).map_or(n, |e| v0 + e);
                    value = decode_entities(&src[v0..v1]);
                    j = (v1 + 1).min(n);
                } else {
                    let v0 = j;
                    while j < n && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                        j += 1;
                    }
                    value = decode_entities(&src[v0..j]);
                }
            }
            if !aname.is_empty() && !attrs.iter().any(|(k, _)| *k == aname) {
                attrs.push((aname, value));
            }
        }
        i = j;
        text_start = i;
        if end_tag {
            out.push(Token::End(name));
            continue;
        }
        let raw = is_raw_text(&name) && !self_closing;
        out.push(Token::Start {
            name: name.clone(),
            attrs,
            self_closing,
        });
        if raw {
            // Everything up to </name> is text (decoded only for RCDATA).
            let close = alloc::format!("</{}", name);
            let lower = src[i..].to_ascii_lowercase();
            let end = lower.find(&close).map_or(n, |e| i + e);
            if end > i {
                let t = &src[i..end];
                let t = if matches!(name.as_str(), "textarea" | "title") { decode_entities(t) } else { t.to_string() };
                out.push(Token::Text(t));
            }
            let after = src[end..].find('>').map_or(n, |e| end + e + 1);
            if end < n {
                out.push(Token::End(name));
            }
            i = after;
            text_start = i;
        }
    }
    flush_text(&mut out, text_start, n);
    out
}

// ---------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------

pub type NodeId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    Element { tag: String, attrs: Vec<(String, String)> },
    Text(String),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
}

#[derive(Debug, Clone, Default)]
pub struct Document {
    pub nodes: Vec<Node>,
    pub title: String,
    /// `<base href>`.
    pub base: Option<String>,
    /// `<meta http-equiv=refresh content="N; url=...">`: delay and URL
    /// (empty URL = reload).
    pub refresh: Option<(u32, String)>,
    /// Charset declared by `<meta>`.
    pub charset: Option<String>,
}

pub fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta" | "param"
            | "source" | "track" | "wbr" | "keygen" | "frame"
    )
}

/// Elements that close an open `<p>`.
fn closes_p(tag: &str) -> bool {
    matches!(
        tag,
        "address" | "article" | "aside" | "blockquote" | "center" | "details" | "dialog" | "dir"
            | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "form" | "h1" | "h2"
            | "h3" | "h4" | "h5" | "h6" | "header" | "hgroup" | "hr" | "main" | "menu" | "nav"
            | "ol" | "p" | "pre" | "section" | "summary" | "table" | "ul" | "li" | "dd" | "dt"
    )
}

impl Document {
    pub fn element(&self, id: NodeId) -> Option<(&str, &[(String, String)])> {
        match &self.nodes[id].kind {
            NodeKind::Element { tag, attrs } => Some((tag.as_str(), attrs.as_slice())),
            _ => None,
        }
    }

    pub fn tag(&self, id: NodeId) -> &str {
        self.element(id).map_or("", |e| e.0)
    }

    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id)?
            .1
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Concatenated text of a subtree.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut s = String::new();
        self.collect_text(id, &mut s);
        s
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        match &self.nodes[id].kind {
            NodeKind::Text(t) => out.push_str(t),
            _ => {
                for &c in &self.nodes[id].children {
                    self.collect_text(c, out);
                }
            }
        }
    }

    /// All descendants in document order.
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if n != id {
                out.push(n);
            }
            for &c in self.nodes[n].children.iter().rev() {
                stack.push(c);
            }
        }
        out
    }

    /// First element with the given tag.
    pub fn find(&self, tag: &str) -> Option<NodeId> {
        self.descendants(0).into_iter().find(|&n| self.tag(n) == tag)
    }
}

struct Builder {
    doc: Document,
    stack: Vec<NodeId>,
}

impl Builder {
    fn add(&mut self, kind: NodeKind) -> NodeId {
        let parent = *self.stack.last().unwrap();
        let id = self.doc.nodes.len();
        self.doc.nodes.push(Node {
            kind,
            parent: Some(parent),
            children: Vec::new(),
        });
        self.doc.nodes[parent].children.push(id);
        id
    }

    fn open_tags(&self) -> impl Iterator<Item = &str> + '_ {
        self.stack.iter().rev().map(|&n| self.doc.tag(n))
    }

    /// Pop up to and including the innermost `tag`, if it is open below
    /// any of the `boundary` tags.
    fn close(&mut self, tag: &str, boundary: &[&str]) -> bool {
        let mut depth = None;
        for (k, t) in self.open_tags().enumerate() {
            if t == tag {
                depth = Some(k);
                break;
            }
            if boundary.contains(&t) {
                return false;
            }
        }
        match depth {
            Some(k) => {
                let keep = self.stack.len() - 1 - k;
                self.stack.truncate(keep.max(1));
                true
            }
            None => false,
        }
    }

    fn start(&mut self, name: String, attrs: Vec<(String, String)>, self_closing: bool) {
        const SCOPE: &[&str] = &["table", "td", "th", "caption", "html", "body", "button", "object", "template"];
        match name.as_str() {
            "html" | "head" | "body" => {
                // Keep a flat structure; merge attributes are irrelevant here.
                return;
            }
            t if closes_p(t) => {
                self.close("p", SCOPE);
                match t {
                    "li" => {
                        self.close("li", &["ul", "ol", "menu", "dir", "table", "td", "th"]);
                    }
                    "dt" | "dd" => {
                        if !self.close("dt", &["dl", "table"]) {
                            self.close("dd", &["dl", "table"]);
                        }
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        // Headings do not nest.
                        let open = self
                            .open_tags()
                            .next()
                            .filter(|t| t.len() == 2 && t.starts_with('h') && t.as_bytes()[1].is_ascii_digit())
                            .map(String::from);
                        if let Some(h) = open {
                            self.close(&h, SCOPE);
                        }
                    }
                    _ => {}
                }
            }
            "option" => {
                self.close("option", &["select", "datalist"]);
            }
            "optgroup" => {
                self.close("option", &["select"]);
                self.close("optgroup", &["select"]);
            }
            "tr" => {
                self.close("td", &["table"]);
                self.close("th", &["table"]);
                self.close("tr", &["table"]);
            }
            "td" | "th" => {
                self.close("td", &["table", "tr"]);
                self.close("th", &["table", "tr"]);
            }
            "thead" | "tbody" | "tfoot" => {
                self.close("td", &["table"]);
                self.close("th", &["table"]);
                self.close("tr", &["table"]);
                for s in ["thead", "tbody", "tfoot"] {
                    self.close(s, &["table"]);
                }
            }
            "a" => {
                // Links do not nest.
                self.close("a", SCOPE);
            }
            _ => {}
        }
        match name.as_str() {
            "base" => {
                if self.doc.base.is_none() {
                    self.doc.base = attrs.iter().find(|(k, _)| k == "href").map(|(_, v)| v.trim().to_string());
                }
            }
            "meta" => {
                let get = |k: &str| attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
                if let Some(cs) = get("charset") {
                    self.doc.charset = Some(cs.trim().to_ascii_lowercase());
                }
                let equiv = get("http-equiv").unwrap_or("").to_ascii_lowercase();
                let content = get("content").unwrap_or("");
                if equiv == "refresh" {
                    self.doc.refresh = parse_refresh(content);
                } else if equiv == "content-type" {
                    if let Some(i) = content.to_ascii_lowercase().find("charset=") {
                        let cs = content[i + 8..].trim_matches(|c: char| c == '"' || c == '\'' || c == ';' || c.is_whitespace());
                        self.doc.charset = Some(cs.to_ascii_lowercase());
                    }
                }
            }
            _ => {}
        }
        let void = is_void(&name);
        let id = self.add(NodeKind::Element { tag: name, attrs });
        if !void && !self_closing {
            self.stack.push(id);
        }
    }

    fn end(&mut self, name: &str) {
        match name {
            "html" | "head" | "body" => {}
            "p" => {
                if !self.close("p", &["table", "td", "th", "button"]) {
                    // A stray </p> creates an empty paragraph (HTML5).
                    self.add(NodeKind::Element { tag: String::from("p"), attrs: Vec::new() });
                }
            }
            "br" => {
                self.add(NodeKind::Element { tag: String::from("br"), attrs: Vec::new() });
            }
            t => {
                let boundary: &[&str] = match t {
                    "td" | "th" | "tr" | "tbody" | "thead" | "tfoot" | "caption" => &["table"],
                    "table" => &[],
                    "li" => &["ul", "ol"],
                    _ => &["table", "td", "th"],
                };
                self.close(t, boundary);
            }
        }
    }
}

/// Parse `N; url=...` (also `N;URL='...'` and bare `N`).
pub fn parse_refresh(content: &str) -> Option<(u32, String)> {
    let c = content.trim();
    let digits = c.bytes().take_while(|b| b.is_ascii_digit()).count();
    let secs = c[..digits].parse().unwrap_or(0);
    let rest = c[digits..].trim_start_matches(|ch: char| ch == '.' || ch.is_ascii_digit()).trim_start_matches([';', ',', ' ']);
    let url = if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case("url") {
        rest[3..].trim_start().trim_start_matches('=').trim()
    } else {
        rest.trim()
    };
    if digits == 0 && url.is_empty() {
        return None;
    }
    Some((secs, url.trim_matches(|ch| ch == '\'' || ch == '"').to_string()))
}

/// Parse a document.
pub fn parse(src: &str) -> Document {
    let mut b = Builder {
        doc: Document {
            nodes: vec![Node {
                kind: NodeKind::Document,
                parent: None,
                children: Vec::new(),
            }],
            ..Document::default()
        },
        stack: vec![0],
    };
    let mut in_title = false;
    for t in tokenize(src) {
        match t {
            Token::Start {
                name,
                attrs,
                self_closing,
            } => {
                in_title = name == "title" && !self_closing;
                if in_title {
                    continue;
                }
                b.start(name, attrs, self_closing);
            }
            Token::End(name) => {
                if name == "title" {
                    in_title = false;
                    continue;
                }
                b.end(&name);
            }
            Token::Text(t) => {
                if in_title {
                    if b.doc.title.is_empty() {
                        b.doc.title = collapse_ws(&t);
                    }
                    continue;
                }
                // Merge adjacent text nodes.
                let parent = *b.stack.last().unwrap();
                if let Some(&last) = b.doc.nodes[parent].children.last() {
                    if let NodeKind::Text(prev) = &mut b.doc.nodes[last].kind {
                        prev.push_str(&t);
                        continue;
                    }
                }
                b.add(NodeKind::Text(t));
            }
            Token::Comment(_) | Token::Doctype(_) => {}
        }
    }
    b.doc
}

/// Collapse runs of whitespace to single spaces and trim.
pub fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Find a charset declaration in the first bytes of a document (the HTML
/// "prescan"), before it is decoded.
pub fn sniff_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let text: String = head.iter().map(|&b| (b as char).to_ascii_lowercase()).collect();
    let mut from = 0;
    while let Some(i) = text[from..].find("<meta") {
        let start = from + i;
        let end = text[start..].find('>').map_or(text.len(), |e| start + e);
        let tag = &text[start..end];
        if let Some(j) = tag.find("charset") {
            let v = tag[j + 7..].trim_start().strip_prefix('=').unwrap_or("").trim_start();
            let v = v.trim_start_matches(['"', '\'']);
            let cs: String = v
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
                .collect();
            if !cs.is_empty() {
                return Some(cs);
            }
        }
        from = end;
    }
    None
}

#[cfg(test)]
mod tests;
