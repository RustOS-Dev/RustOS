//! Box tree construction: computes element styles, generates boxes by
//! `display` (with `::before`/`::after` content, list markers, counters
//! and quotes), and fixes up the tree with anonymous boxes.

use crate::dom::Nav;
use crate::forms::Controls;
use crate::page::{Field, FieldKind, Target};
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};
use css::style::{
    Content, ContentItem, Display, ListStylePosition, ListStyleType, TextTransform, WhiteSpace,
};
use css::{ComputedStyle, PseudoElement, StyleSet};
use html::{NodeId, NodeKind};

pub type StyleRef = Rc<ComputedStyle>;

#[derive(Debug, Clone, PartialEq)]
pub enum ReplacedKind {
    Image {
        src: String,
        alt: String,
    },
    Field(usize),
    /// Embedded content shown as a labelled placeholder (`iframe`,
    /// `video`, `canvas`, ...).
    Placeholder(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Replaced {
    pub kind: ReplacedKind,
    /// Size from attributes (the style may override it).
    pub attr_width: Option<f32>,
    pub attr_height: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BoxKind {
    /// Block container (block formatting or, with only inline-level
    /// children, an inline formatting context).
    Block,
    /// Non-atomic inline box.
    Inline,
    /// Text with the decoration lines propagated from ancestors.
    Text {
        text: String,
        deco: u8,
    },
    Replaced(Replaced),
    Flex,
    Grid,
    Table,
    TableRowGroup,
    TableRow,
    TableCell {
        colspan: u32,
        rowspan: u32,
    },
    TableCaption,
    TableColumn {
        span: u32,
    },
    LineBreak,
    /// List marker; `outside` markers hang left of the first line.
    Marker {
        text: String,
        outside: bool,
    },
}

#[derive(Debug, Clone)]
pub struct LayoutBox {
    pub style: StyleRef,
    pub node: Option<NodeId>,
    pub kind: BoxKind,
    pub children: Vec<LayoutBox>,
    pub target: Target,
    /// Inside a heading (`h1`..`h6`).
    pub heading: bool,
}

impl LayoutBox {
    pub fn is_inline_level(&self) -> bool {
        match &self.kind {
            BoxKind::Inline | BoxKind::Text { .. } | BoxKind::LineBreak => true,
            BoxKind::Marker { .. } => true,
            BoxKind::Replaced(_) => {
                self.style.display.is_inline_level() || self.style.display == Display::Inline
            }
            BoxKind::Block | BoxKind::Flex | BoxKind::Grid | BoxKind::Table => {
                self.style.display.is_inline_level() && !self.style.is_out_of_flow()
            }
            _ => false,
        }
    }

    /// Only collapsible whitespace text.
    pub fn is_blank_text(&self) -> bool {
        match &self.kind {
            BoxKind::Text { text, .. } => {
                !matches!(
                    self.style.white_space,
                    WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces
                ) && text
                    .chars()
                    .all(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c'))
            }
            _ => false,
        }
    }
}

pub struct BuildOptions {
    /// Prefix links with `[n]` (lynx style).
    pub number_links: bool,
    /// ASCII list bullets (`*`, `o`, `+`) for terminals.
    pub ascii_markers: bool,
}

struct Counter {
    name: String,
    value: i32,
}

struct Ctx<'a, 'b> {
    nav: &'b Nav<'a>,
    styles: &'b StyleSet,
    controls: &'b Controls,
    opts: &'b BuildOptions,
    counters: Vec<Counter>,
    quote_depth: usize,
    link: Option<usize>,
    heading: bool,
    deco: u8,
    /// Fragment identifiers (ids and `a[name]`), indexed by
    /// `Target::Anchor`.
    pub anchors: Vec<String>,
    depth: usize,
}

/// Build the box tree of a document. Returns the root box (the initial
/// containing block's child) and the anchor names.
pub fn build(
    nav: &Nav,
    styles: &StyleSet,
    controls: &Controls,
    opts: &BuildOptions,
) -> (LayoutBox, Vec<String>) {
    let viewport = Rc::new(ComputedStyle {
        display: Display::Block,
        ..ComputedStyle::default()
    });
    let mut ctx = Ctx {
        nav,
        styles,
        controls,
        opts,
        counters: Vec::new(),
        quote_depth: 0,
        link: None,
        heading: false,
        deco: 0,
        anchors: Vec::new(),
        depth: 0,
    };
    let mut children = Vec::new();
    for &c in &nav.doc.nodes[0].children {
        children.extend(ctx.node(c, &viewport));
    }
    let mut root = LayoutBox {
        style: viewport,
        node: None,
        kind: BoxKind::Block,
        children,
        target: Target::None,
        heading: false,
    };
    fixup(&mut root);
    (root, ctx.anchors)
}

fn transform_text(s: &str, t: TextTransform, first_in_word: &mut bool) -> String {
    match t {
        TextTransform::None | TextTransform::FullWidth => s.to_string(),
        TextTransform::Uppercase => s.to_uppercase(),
        TextTransform::Lowercase => s.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(s.len());
            for c in s.chars() {
                if c.is_alphanumeric() {
                    if *first_in_word {
                        out.extend(c.to_uppercase());
                    } else {
                        out.push(c);
                    }
                    *first_in_word = false;
                } else {
                    out.push(c);
                    *first_in_word = c.is_whitespace() || c == '-';
                }
            }
            out
        }
    }
}

pub fn format_counter(v: i32, t: &ListStyleType) -> String {
    fn alpha(mut n: i32, upper: bool) -> String {
        if n <= 0 {
            return n.to_string();
        }
        let mut s = Vec::new();
        while n > 0 {
            n -= 1;
            s.push((if upper { b'A' } else { b'a' }) + (n % 26) as u8);
            n /= 26;
        }
        s.reverse();
        String::from_utf8(s).unwrap_or_default()
    }
    fn roman(n: i32, upper: bool) -> String {
        if !(1..4000).contains(&n) {
            return n.to_string();
        }
        let table = [
            (1000, "m"),
            (900, "cm"),
            (500, "d"),
            (400, "cd"),
            (100, "c"),
            (90, "xc"),
            (50, "l"),
            (40, "xl"),
            (10, "x"),
            (9, "ix"),
            (5, "v"),
            (4, "iv"),
            (1, "i"),
        ];
        let mut n = n;
        let mut s = String::new();
        for (v, r) in table {
            while n >= v {
                s.push_str(r);
                n -= v;
            }
        }
        if upper { s.to_uppercase() } else { s }
    }
    match t {
        ListStyleType::None => String::new(),
        ListStyleType::Disc => String::from("•"),
        ListStyleType::Circle => String::from("◦"),
        ListStyleType::Square => String::from("▪"),
        ListStyleType::DisclosureOpen => String::from("▾"),
        ListStyleType::DisclosureClosed => String::from("▸"),
        ListStyleType::Decimal => v.to_string(),
        ListStyleType::DecimalLeadingZero => format!("{:02}", v),
        ListStyleType::LowerAlpha => alpha(v, false),
        ListStyleType::UpperAlpha => alpha(v, true),
        ListStyleType::LowerRoman => roman(v, false),
        ListStyleType::UpperRoman => roman(v, true),
        ListStyleType::LowerGreek => {
            if (1..=24).contains(&v) {
                char::from_u32(0x3B1 + (v as u32 - 1) + u32::from(v >= 18))
                    .map(String::from)
                    .unwrap_or_default()
            } else {
                v.to_string()
            }
        }
        ListStyleType::String(s) => s.clone(),
    }
}

/// Marker text for a list item (`1. `, `• `).
fn marker_text(t: &ListStyleType, value: i32, ascii: bool) -> String {
    if ascii {
        match t {
            ListStyleType::Disc => return String::from("* "),
            ListStyleType::Circle => return String::from("o "),
            ListStyleType::Square => return String::from("+ "),
            ListStyleType::DisclosureOpen => return String::from("v "),
            ListStyleType::DisclosureClosed => return String::from("> "),
            _ => {}
        }
    }
    match t {
        ListStyleType::None => String::new(),
        ListStyleType::String(s) => s.clone(),
        ListStyleType::Disc
        | ListStyleType::Circle
        | ListStyleType::Square
        | ListStyleType::DisclosureOpen
        | ListStyleType::DisclosureClosed => {
            format!("{} ", format_counter(value, t))
        }
        _ => format!("{}. ", format_counter(value, t)),
    }
}

fn html_len(v: &str) -> Option<f32> {
    let digits: String = v
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.parse().ok()
}

impl Ctx<'_, '_> {
    fn text_box(&self, text: String, style: &StyleRef) -> LayoutBox {
        LayoutBox {
            style: style.clone(),
            node: None,
            kind: BoxKind::Text {
                text,
                deco: self.deco | style.text_decoration_line,
            },
            children: Vec::new(),
            target: self.link.map_or(Target::None, Target::Link),
            heading: self.heading,
        }
    }

    fn counter_value(&self, name: &str) -> i32 {
        self.counters
            .iter()
            .rev()
            .find(|c| c.name == name)
            .map_or(0, |c| c.value)
    }

    fn apply_counters(&mut self, st: &ComputedStyle) {
        for (name, v) in &st.counter_reset {
            self.counters.push(Counter {
                name: name.clone(),
                value: *v,
            });
        }
        for (name, v) in &st.counter_set {
            match self.counters.iter_mut().rev().find(|c| &c.name == name) {
                Some(c) => c.value = *v,
                None => self.counters.push(Counter {
                    name: name.clone(),
                    value: *v,
                }),
            }
        }
        let mut incs = st.counter_increment.clone();
        if st.display == Display::ListItem && !incs.iter().any(|(n, _)| n == "list-item") {
            incs.push((String::from("list-item"), 1));
        }
        for (name, v) in incs {
            match self.counters.iter_mut().rev().find(|c| c.name == name) {
                Some(c) => c.value += v,
                None => self.counters.push(Counter { name, value: v }),
            }
        }
    }

    fn content_text(
        &mut self,
        items: &[ContentItem],
        node: NodeId,
        style: &ComputedStyle,
    ) -> String {
        let mut s = String::new();
        let quotes = style.quotes.clone().unwrap_or_else(|| {
            vec![
                (String::from("“"), String::from("”")),
                (String::from("‘"), String::from("’")),
            ]
        });
        for it in items {
            match it {
                ContentItem::String(t) => s.push_str(t),
                ContentItem::Attr(a) => s.push_str(self.nav.doc.attr(node, a).unwrap_or("")),
                ContentItem::Counter(n, t) => s.push_str(&format_counter(self.counter_value(n), t)),
                ContentItem::Counters(n, sep, t) => {
                    let vals: Vec<String> = self
                        .counters
                        .iter()
                        .filter(|c| &c.name == n)
                        .map(|c| format_counter(c.value, t))
                        .collect();
                    s.push_str(&if vals.is_empty() {
                        String::from("0")
                    } else {
                        vals.join(sep)
                    });
                }
                ContentItem::OpenQuote => {
                    if !quotes.is_empty() {
                        s.push_str(&quotes[self.quote_depth.min(quotes.len() - 1)].0);
                    }
                    self.quote_depth += 1;
                }
                ContentItem::CloseQuote => {
                    self.quote_depth = self.quote_depth.saturating_sub(1);
                    if !quotes.is_empty() {
                        s.push_str(&quotes[self.quote_depth.min(quotes.len() - 1)].1);
                    }
                }
                ContentItem::NoOpenQuote => self.quote_depth += 1,
                ContentItem::NoCloseQuote => self.quote_depth = self.quote_depth.saturating_sub(1),
                ContentItem::Url(_) => {}
            }
        }
        s
    }

    fn pseudo(&mut self, id: NodeId, style: &StyleRef, which: PseudoElement) -> Option<LayoutBox> {
        let el = self.nav.el(id);
        if !self.styles.has_pseudo(&el, which) {
            return None;
        }
        let ps = self
            .styles
            .compute(&el, Some(style), None, &[], Some(which));
        let Content::Items(items) = &ps.content else {
            return None;
        };
        if ps.display == Display::None {
            return None;
        }
        let items = items.clone();
        self.apply_counters(&ps);
        let text = self.content_text(&items, id, &ps);
        let ps = Rc::new(ps);
        let kind = display_kind(&ps, None)?;
        let mut first = true;
        let text = transform_text(&text, ps.text_transform, &mut first);
        Some(LayoutBox {
            style: ps.clone(),
            node: None,
            kind,
            children: if text.is_empty() {
                Vec::new()
            } else {
                vec![self.text_box(text, &ps)]
            },
            target: self.link.map_or(Target::None, Target::Link),
            heading: self.heading,
        })
    }

    /// Boxes for DOM node `id` (none for `display: none`, several for
    /// `display: contents`).
    fn node(&mut self, id: NodeId, parent: &StyleRef) -> Vec<LayoutBox> {
        let doc = self.nav.doc;
        match &doc.nodes[id].kind {
            NodeKind::Text(t) => {
                if t.is_empty() {
                    return Vec::new();
                }
                let mut first = true;
                let text = transform_text(t, parent.text_transform, &mut first);
                vec![self.text_box(text, parent)]
            }
            NodeKind::Document => Vec::new(),
            NodeKind::Element { tag, attrs } => {
                let tag = tag.as_str();
                if matches!(
                    tag,
                    "script" | "style" | "template" | "head" | "title" | "meta" | "link" | "base"
                ) {
                    return Vec::new();
                }
                let el = self.nav.el(id);
                let table_attr = |name: &str| -> Option<String> {
                    let mut p = doc.nodes[id].parent;
                    while let Some(x) = p {
                        if doc.tag(x) == "table" {
                            return doc.attr(x, name).map(String::from);
                        }
                        p = doc.nodes[x].parent;
                    }
                    None
                };
                let hints = css::hints::presentational_hints(tag, attrs, &table_attr);
                let st =
                    self.styles
                        .compute(&el, Some(parent), doc.attr(id, "style"), &hints, None);
                if st.display == Display::None {
                    return Vec::new();
                }
                let st = Rc::new(st);
                let saved = (self.link, self.heading, self.deco, self.counters.len());
                self.apply_counters(&st);
                if let Some(&l) = self.controls.link_of.get(&id) {
                    self.link = Some(l);
                }
                if matches!(tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
                    self.heading = true;
                }
                let mut out_children = Vec::new();
                // Fragment anchors.
                let anchor_name = doc.attr(id, "id").or_else(|| {
                    if tag == "a" {
                        doc.attr(id, "name")
                    } else {
                        None
                    }
                });
                if let Some(a) = anchor_name {
                    self.anchors.push(a.to_string());
                    out_children.push(LayoutBox {
                        style: st.clone(),
                        node: None,
                        kind: BoxKind::Text {
                            text: String::new(),
                            deco: 0,
                        },
                        children: Vec::new(),
                        target: Target::Anchor(self.anchors.len() - 1),
                        heading: self.heading,
                    });
                }
                // Replaced elements and form controls.
                let replaced = self.replaced(id, tag, &st);
                if let Some(r) = replaced {
                    let b = LayoutBox {
                        style: st.clone(),
                        node: Some(id),
                        kind: BoxKind::Replaced(r),
                        children: Vec::new(),
                        target: match self.controls.field_of.get(&id) {
                            Some(&f) => Target::Field(f),
                            None => self.link.map_or(Target::None, Target::Link),
                        },
                        heading: self.heading,
                    };
                    self.restore(saved);
                    if out_children.is_empty() {
                        return vec![b];
                    }
                    // Keep the anchor next to the control.
                    out_children.push(b);
                    return out_children;
                }
                if tag == "br" {
                    self.restore(saved);
                    out_children.push(LayoutBox {
                        style: st.clone(),
                        node: Some(id),
                        kind: BoxKind::LineBreak,
                        children: Vec::new(),
                        target: Target::None,
                        heading: false,
                    });
                    return out_children;
                }
                self.deco |= st.text_decoration_line;
                // lynx-style link numbers.
                if self.opts.number_links
                    && let Some(&l) = self.controls.link_of.get(&id)
                {
                    let mut ns = ComputedStyle::inherit_from(&st);
                    ns.display = Display::Inline;
                    let ns = Rc::new(ns);
                    let mut b = self.text_box(format!("[{}]", l + 1), &ns);
                    b.kind = BoxKind::Text {
                        text: format!("[{}]", l + 1),
                        deco: 0,
                    };
                    b.target = Target::Link(l);
                    out_children.push(b);
                }
                if st.display == Display::ListItem {
                    let v = self.counter_value("list-item");
                    let text = if self.styles.has_pseudo(&el, PseudoElement::Marker) {
                        let ms = self.styles.compute(
                            &el,
                            Some(&st),
                            None,
                            &[],
                            Some(PseudoElement::Marker),
                        );
                        match &ms.content {
                            Content::Items(items) => {
                                let items = items.clone();
                                self.content_text(&items, id, &ms)
                            }
                            _ => marker_text(&st.list_style_type, v, self.opts.ascii_markers),
                        }
                    } else {
                        marker_text(&st.list_style_type, v, self.opts.ascii_markers)
                    };
                    if !text.is_empty() {
                        let mut ms = ComputedStyle::inherit_from(&st);
                        ms.display = Display::Inline;
                        out_children.push(LayoutBox {
                            style: Rc::new(ms),
                            node: None,
                            kind: BoxKind::Marker {
                                text,
                                outside: st.list_style_position == ListStylePosition::Outside,
                            },
                            children: Vec::new(),
                            target: Target::None,
                            heading: self.heading,
                        });
                    }
                }
                if let Some(b) = self.pseudo(id, &st, PseudoElement::Before) {
                    out_children.push(b);
                }
                self.depth += 1;
                let mark = self.counters.len();
                for &c in &doc.nodes[id].children {
                    out_children.extend(self.node(c, &st));
                }
                // Counters created by the children go out of scope.
                self.counters.truncate(mark);
                self.depth -= 1;
                if let Some(b) = self.pseudo(id, &st, PseudoElement::After) {
                    out_children.push(b);
                }
                if tag == "details" && doc.attr(id, "open").is_none() {
                    // Closed details: only the summary is shown.
                    out_children.retain(|b| {
                        b.node.is_some_and(|n| doc.tag(n) == "summary")
                            || matches!(b.kind, BoxKind::Marker { .. })
                    });
                }
                // Counters reset on this element stay visible to its
                // following siblings; only drop what the element's own
                // increments created? (Kept: scoped by the parent.)
                self.link = saved.0;
                self.heading = saved.1;
                self.deco = saved.2;
                if st.display == Display::Contents {
                    return out_children;
                }
                let Some(kind) = display_kind(&st, Some((doc, id))) else {
                    return out_children;
                };
                vec![LayoutBox {
                    style: st,
                    node: Some(id),
                    kind,
                    children: out_children,
                    target: self.link.map_or(Target::None, Target::Link),
                    heading: self.heading,
                }]
            }
        }
    }

    fn restore(&mut self, saved: (Option<usize>, bool, u8, usize)) {
        self.link = saved.0;
        self.heading = saved.1;
        self.deco = saved.2;
    }

    fn replaced(&self, id: NodeId, tag: &str, st: &ComputedStyle) -> Option<Replaced> {
        let doc = self.nav.doc;
        let w = doc.attr(id, "width").and_then(html_len);
        let h = doc.attr(id, "height").and_then(html_len);
        let kind = match tag {
            "img" => ReplacedKind::Image {
                src: doc.attr(id, "src").unwrap_or("").to_string(),
                alt: doc
                    .attr(id, "alt")
                    .map(String::from)
                    .unwrap_or_else(|| String::from("IMG")),
            },
            "input" | "select" | "textarea" | "button" => {
                let f = *self.controls.field_of.get(&id)?;
                if self.controls.fields[f].kind == FieldKind::Hidden {
                    return None;
                }
                ReplacedKind::Field(f)
            }
            "iframe" | "frame" => {
                ReplacedKind::Placeholder(format!("IFRAME: {}", doc.attr(id, "src").unwrap_or("")))
            }
            "video" => ReplacedKind::Placeholder(String::from("VIDEO")),
            "audio" => ReplacedKind::Placeholder(String::from("AUDIO")),
            // A canvas whose pixels the browser has (a script drew it) is
            // shown as an image; otherwise it is an empty box.
            "canvas" => match doc.attr(id, "src").filter(|s| s.starts_with("canvas:")) {
                Some(src) => ReplacedKind::Image { src: String::from(src), alt: String::new() },
                None => ReplacedKind::Placeholder(String::new()),
            },
            "svg" => {
                let label = doc
                    .attr(id, "aria-label")
                    .map(String::from)
                    .unwrap_or_default();
                ReplacedKind::Placeholder(label)
            }
            "object" | "embed" => ReplacedKind::Placeholder(String::from("OBJECT")),
            _ => return None,
        };
        let _ = st;
        Some(Replaced {
            kind,
            attr_width: w,
            attr_height: h,
        })
    }
}

fn display_kind(st: &ComputedStyle, el: Option<(&html::Document, NodeId)>) -> Option<BoxKind> {
    let span = |name: &str| -> u32 {
        el.and_then(|(d, id)| d.attr(id, name))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(1)
            .max(1)
    };
    Some(match st.display {
        Display::None | Display::Contents => return None,
        Display::Block | Display::FlowRoot | Display::ListItem | Display::InlineBlock => {
            BoxKind::Block
        }
        Display::Inline => BoxKind::Inline,
        Display::Flex | Display::InlineFlex => BoxKind::Flex,
        Display::Grid | Display::InlineGrid => BoxKind::Grid,
        Display::Table | Display::InlineTable => BoxKind::Table,
        Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup => {
            BoxKind::TableRowGroup
        }
        Display::TableRow => BoxKind::TableRow,
        Display::TableCell => BoxKind::TableCell {
            colspan: span("colspan").min(1000),
            rowspan: el
                .and_then(|(d, id)| d.attr(id, "rowspan"))
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(1)
                .min(65534),
        },
        Display::TableCaption => BoxKind::TableCaption,
        Display::TableColumn | Display::TableColumnGroup => {
            BoxKind::TableColumn { span: span("span") }
        }
    })
}

fn anon(parent: &StyleRef, display: Display) -> StyleRef {
    let mut s = ComputedStyle::inherit_from(parent);
    s.display = display;
    Rc::new(s)
}

fn anon_box(
    parent: &LayoutBox,
    display: Display,
    kind: BoxKind,
    children: Vec<LayoutBox>,
) -> LayoutBox {
    LayoutBox {
        style: anon(&parent.style, display),
        node: None,
        kind,
        children,
        target: parent.target,
        heading: parent.heading,
    }
}

/// Anonymous box generation (CSS 2 §9.2.1.1, §17.2.1) and cleanup.
pub fn fixup(b: &mut LayoutBox) {
    for c in b.children.iter_mut() {
        fixup(c);
    }
    match b.kind {
        BoxKind::Block | BoxKind::TableCell { .. } | BoxKind::TableCaption => {
            let has_block = b
                .children
                .iter()
                .any(|c| !c.is_inline_level() && !is_out_of_flow_box(c));
            if has_block {
                // An outside list marker goes to the first line of the
                // first block child.
                let mpos = b
                    .children
                    .iter()
                    .position(|c| matches!(c.kind, BoxKind::Marker { outside: true, .. }));
                let marker = match mpos {
                    Some(i) if b.children[..i].iter().all(|c| c.is_blank_text() || matches!(c.kind, BoxKind::Text { ref text, .. } if text.is_empty())) => {
                        let next_block = b.children[i + 1..].iter().find(|c| !c.is_blank_text()).is_some_and(|c| !c.is_inline_level() && !is_out_of_flow_box(c));
                        if next_block { Some(b.children.remove(i)) } else { None }
                    }
                    _ => None,
                };
                wrap_inline_runs(b);
                if let Some(m) = marker {
                    insert_marker(b, m);
                }
            }
        }
        BoxKind::Inline => {
            // Block-level boxes inside an inline: approximate by making
            // the inline a block container.
            if b.children
                .iter()
                .any(|c| !c.is_inline_level() && !is_out_of_flow_box(c))
            {
                b.kind = BoxKind::Block;
                let mut st = (*b.style).clone();
                st.display = Display::Block;
                b.style = Rc::new(st);
                wrap_inline_runs(b);
            }
        }
        BoxKind::Flex | BoxKind::Grid => {
            // Each in-flow child is an item; runs of text are wrapped in
            // anonymous blocks; whitespace-only text is dropped.
            let kids = core::mem::take(&mut b.children);
            let mut run: Vec<LayoutBox> = Vec::new();
            for c in kids {
                let inline_like = matches!(
                    c.kind,
                    BoxKind::Text { .. } | BoxKind::Inline | BoxKind::LineBreak
                ) || matches!(c.kind, BoxKind::Marker { .. });
                if inline_like {
                    run.push(c);
                } else {
                    flush_run(b, &mut run);
                    b.children.push(c);
                }
            }
            flush_run(b, &mut run);
        }
        BoxKind::Table => fix_table(b),
        BoxKind::TableRowGroup => {
            // Only rows.
            let kids = core::mem::take(&mut b.children);
            let mut cells: Vec<LayoutBox> = Vec::new();
            for c in kids {
                match c.kind {
                    BoxKind::TableRow => {
                        if !cells.is_empty() {
                            let row = anon_box(
                                b,
                                Display::TableRow,
                                BoxKind::TableRow,
                                core::mem::take(&mut cells),
                            );
                            b.children.push(row);
                        }
                        b.children.push(c);
                    }
                    _ if c.is_blank_text() => {}
                    _ => cells.push(wrap_cell(b, c)),
                }
            }
            if !cells.is_empty() {
                let row = anon_box(b, Display::TableRow, BoxKind::TableRow, cells);
                b.children.push(row);
            }
        }
        BoxKind::TableRow => {
            let kids = core::mem::take(&mut b.children);
            let mut out = Vec::new();
            let mut run: Vec<LayoutBox> = Vec::new();
            for c in kids {
                if matches!(c.kind, BoxKind::TableCell { .. }) {
                    if !run.is_empty() {
                        let cell = anon_box(
                            b,
                            Display::TableCell,
                            BoxKind::TableCell {
                                colspan: 1,
                                rowspan: 1,
                            },
                            core::mem::take(&mut run),
                        );
                        out.push(cell);
                    }
                    out.push(c);
                } else if !c.is_blank_text() {
                    run.push(c);
                }
            }
            if !run.is_empty() {
                out.push(anon_box(
                    b,
                    Display::TableCell,
                    BoxKind::TableCell {
                        colspan: 1,
                        rowspan: 1,
                    },
                    run,
                ));
            }
            b.children = out;
        }
        _ => {}
    }
}

/// Put a list marker at the start of the first line box inside `b`.
fn insert_marker(b: &mut LayoutBox, m: LayoutBox) {
    let first = b
        .children
        .iter()
        .position(|c| !c.is_blank_text() && !is_out_of_flow_box(c));
    match first {
        Some(i)
            if !b.children[i].is_inline_level() && matches!(b.children[i].kind, BoxKind::Block) =>
        {
            insert_marker(&mut b.children[i], m)
        }
        _ => b.children.insert(0, m),
    }
}

fn is_out_of_flow_box(b: &LayoutBox) -> bool {
    !matches!(b.kind, BoxKind::Text { .. } | BoxKind::LineBreak) && b.style.is_out_of_flow()
}

fn flush_run(parent: &mut LayoutBox, run: &mut Vec<LayoutBox>) {
    if run.is_empty() {
        return;
    }
    if run.iter().all(|c| c.is_blank_text()) {
        run.clear();
        return;
    }
    let kids = core::mem::take(run);
    let a = anon_box(parent, Display::Block, BoxKind::Block, kids);
    parent.children.push(a);
}

/// Wrap runs of inline-level children of a block container that also
/// has block-level children in anonymous blocks.
fn wrap_inline_runs(b: &mut LayoutBox) {
    let kids = core::mem::take(&mut b.children);
    let mut run: Vec<LayoutBox> = Vec::new();
    for c in kids {
        if c.is_inline_level()
            || is_out_of_flow_box(c_ref(&c)) && run.iter().any(|x| !x.is_blank_text())
        {
            run.push(c);
        } else {
            flush_run(b, &mut run);
            b.children.push(c);
        }
    }
    flush_run(b, &mut run);
}

fn c_ref(c: &LayoutBox) -> &LayoutBox {
    c
}

fn wrap_cell(parent: &LayoutBox, c: LayoutBox) -> LayoutBox {
    if matches!(c.kind, BoxKind::TableCell { .. }) {
        return c;
    }
    anon_box(
        parent,
        Display::TableCell,
        BoxKind::TableCell {
            colspan: 1,
            rowspan: 1,
        },
        vec![c],
    )
}

fn fix_table(b: &mut LayoutBox) {
    // Children: captions, column groups, row groups; stray rows and cells
    // get wrapped. Header groups first, footer groups last.
    let kids = core::mem::take(&mut b.children);
    let mut captions = Vec::new();
    let mut cols = Vec::new();
    let mut head = Vec::new();
    let mut body = Vec::new();
    let mut foot = Vec::new();
    let mut loose: Vec<LayoutBox> = Vec::new();
    let flush_loose = |loose: &mut Vec<LayoutBox>, body: &mut Vec<LayoutBox>, b: &LayoutBox| {
        if loose.is_empty() {
            return;
        }
        let mut g = anon_box(
            b,
            Display::TableRowGroup,
            BoxKind::TableRowGroup,
            core::mem::take(loose),
        );
        fixup(&mut g);
        body.push(g);
    };
    for c in kids {
        match c.kind {
            BoxKind::TableCaption => captions.push(c),
            BoxKind::TableColumn { .. } => cols.push(c),
            BoxKind::TableRowGroup => {
                flush_loose(&mut loose, &mut body, b);
                match c.style.display {
                    Display::TableHeaderGroup if head.is_empty() => head.push(c),
                    Display::TableFooterGroup if foot.is_empty() => foot.push(c),
                    _ => body.push(c),
                }
            }
            _ if c.is_blank_text() => {}
            _ => loose.push(c),
        }
    }
    flush_loose(&mut loose, &mut body, b);
    b.children = captions;
    b.children.extend(cols);
    b.children.extend(head);
    b.children.extend(body);
    b.children.extend(foot);
}

/// A field's text as the cell renderer shows it (used for sizing).
pub fn field_label(f: &Field) -> String {
    crate::page::field_text(f)
}
