//! CSS layout for the RustOS browser.
//!
//! [`render`] styles an [`html::Document`] with the `css` crate (the HTML
//! user-agent sheet plus author sheets), builds the box tree, lays it out
//! (block and inline flow with floats, flexbox, grid, tables, positioned
//! boxes) and paints the fragments onto character cells, producing a
//! [`Page`] of styled lines with links, form fields and anchors. The
//! layout itself works in CSS px through the [`Metrics`] trait, so a
//! pixel renderer can reuse it.

#![no_std]

extern crate alloc;

pub mod cells;
pub mod dom;
pub mod flex;
pub mod flow;
pub mod forms;
pub mod fragment;
pub mod geom;
pub mod grid;
pub mod inline;
pub mod metrics;
pub mod page;
pub mod table;
pub mod tree;

#[cfg(test)]
mod tests;

pub use dom::DomState;
pub use fragment::{FragKind, Fragment};
pub use metrics::{CellMetrics, Metrics};
pub use page::*;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use css::{Device, Origin, StyleSet};
use html::Document;

pub(crate) fn roundf(x: f32) -> f32 {
    css::math::roundf(x)
}

/// Render options for the character-cell renderer.
pub struct Options {
    /// Terminal columns.
    pub width: usize,
    /// Terminal rows (for `vh` units and media queries).
    pub height: usize,
    /// Prefix links with `[n]` as lynx does.
    pub number_links: bool,
    /// Apply author style sheets (`-nocss` turns this off).
    pub author_css: bool,
    /// Scripting on (for `<noscript>` and `@media (scripting)`).
    pub scripting: bool,
    pub dark: bool,
    pub state: DomState,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            width: 80,
            height: 24,
            number_links: true,
            author_css: true,
            scripting: false,
            dark: false,
            state: DomState::default(),
        }
    }
}

/// A style sheet source found in a document, in document order.
pub enum SheetSource {
    /// `<style>` contents.
    Inline(String),
    /// `<link rel=stylesheet href>` (the href as written).
    Link(String),
}

/// Style sheets of `doc` in document order (`<style>` and
/// `<link rel="stylesheet">` whose `media` applies).
pub fn sheet_sources(doc: &Document, device: &Device) -> Vec<SheetSource> {
    let mut out = Vec::new();
    for id in doc.descendants(0) {
        let media_ok = |id: usize| match doc.attr(id, "media") {
            Some(m) => css::media::matches(&css::parser::component_values(m), device),
            None => true,
        };
        match doc.tag(id) {
            "style" if media_ok(id) => {
                let t = doc
                    .attr(id, "type")
                    .unwrap_or("text/css")
                    .to_ascii_lowercase();
                if t.is_empty() || t == "text/css" {
                    out.push(SheetSource::Inline(doc.text_content(id)));
                }
            }
            "link" if media_ok(id) => {
                let rel = doc.attr(id, "rel").unwrap_or("").to_ascii_lowercase();
                if rel.split_ascii_whitespace().any(|r| r == "stylesheet")
                    && !rel.split_ascii_whitespace().any(|r| r == "alternate")
                    && doc.attr(id, "disabled").is_none()
                {
                    if let Some(h) = doc.attr(id, "href") {
                        out.push(SheetSource::Link(String::from(h.trim())));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The device a terminal of `opts.width` × `opts.height` cells presents to
/// media queries.
pub fn cell_device(opts: &Options) -> Device {
    Device {
        width: opts.width as f32 * metrics::CELL_W,
        height: opts.height as f32 * metrics::CELL_H,
        dark: opts.dark,
        hover: false,
        pointer: css::Pointer::None,
        color_bits: 8,
        scripting: opts.scripting,
        ..Device::default()
    }
}

/// Build the style set: the UA sheet and the author sheets given as CSS
/// text in cascade order (already fetched; `@import`s resolved by
/// `import`, which gets the URL as written and returns its CSS).
pub fn style_set(
    device: Device,
    author: &[String],
    import: &mut dyn FnMut(&str) -> Option<String>,
) -> StyleSet {
    let mut set = StyleSet::with_user_agent(device);
    if device.pointer == css::Pointer::None {
        // A terminal: no side margin on the page (a whole column each).
        set.add("body { margin-left: 0; margin-right: 0 }", Origin::User);
    }
    for css in author {
        add_with_imports(&mut set, css, import, 0);
    }
    set
}

fn add_with_imports(
    set: &mut StyleSet,
    css: &str,
    import: &mut dyn FnMut(&str) -> Option<String>,
    depth: u32,
) {
    // Imports come first in a sheet: add them, then the sheet itself.
    let mut probe = StyleSet::new(set.device);
    let imports = probe.add(css, Origin::Author);
    if depth < 4 {
        for i in imports.iter().filter(|i| i.applies) {
            if let Some(t) = import(&i.url) {
                add_with_imports(set, &t, import, depth + 1);
            }
        }
    }
    set.add(css, Origin::Author);
}

/// Lay out `doc` with `styles` and return the fragment tree, the
/// controls and the anchors.
pub fn layout(
    doc: &Document,
    styles: &StyleSet,
    m: &dyn Metrics,
    viewport: (f32, f32),
    state: &DomState,
    number_links: bool,
) -> (Fragment, forms::Controls, Vec<String>) {
    let nav = dom::Nav::new(doc, state);
    let mut controls = forms::collect(doc, &state.clickable);
    // Control state from the user and scripts.
    for f in &mut controls.fields {
        if let Some((_, v)) = state.values.iter().find(|(n, _)| *n == f.node) {
            f.value = v.clone();
        }
        if let Some((_, c)) = state.checked.iter().find(|(n, _)| *n == f.node) {
            f.checked = *c;
        }
        if let Some((_, s)) = state.selected.iter().find(|(n, _)| *n == f.node) {
            if *s < f.options.len() {
                f.selected = *s;
            }
        }
    }
    let (root, anchors) = tree::build(
        &nav,
        styles,
        &controls,
        &tree::BuildOptions {
            number_links,
            ascii_markers: m.cell_mode(),
        },
    );
    let frag = {
        let mut e = flow::Engine::new(m, doc, &controls.fields, viewport);
        e.layout_root(&root)
    };
    (frag, controls, anchors)
}

/// Render `doc` for a terminal (see the crate docs). `author` holds the
/// author style sheets in document order (see [`sheet_sources`]).
pub fn render_with(
    doc: &Document,
    opts: &Options,
    author: &[String],
    import: &mut dyn FnMut(&str) -> Option<String>,
) -> Page {
    let device = cell_device(opts);
    let sheets: Vec<String> = if opts.author_css {
        author.to_vec()
    } else {
        Vec::new()
    };
    let styles = style_set(device, &sheets, import);
    let viewport = (device.width, device.height);
    let (frag, controls, anchors) = layout(
        doc,
        &styles,
        &CellMetrics,
        viewport,
        &opts.state,
        opts.number_links,
    );
    let mut page = Page {
        // From the DOM (scripts may have changed it).
        title: doc
            .find("title")
            .map(|t| html::collapse_ws(&doc.text_content(t)))
            .unwrap_or_else(|| doc.title.clone()),
        lines: Vec::new(),
        links: controls.links,
        fields: controls.fields,
        forms: controls.forms,
        anchors: Vec::new(),
        boxes: BTreeMap::new(),
    };
    collect_boxes(&frag, &mut page.boxes);
    let fields = page.fields.clone();
    let mut canvas = cells::Canvas::new(opts.width, doc, &fields);
    canvas.paint(&frag);
    canvas.into_page(&mut page, &anchors);
    page
}

fn collect_boxes(f: &fragment::Fragment, out: &mut BTreeMap<html::NodeId, Vec<[f32; 4]>>) {
    if let Some(n) = f.node {
        out.entry(n)
            .or_default()
            .push([f.rect.x, f.rect.y, f.rect.w, f.rect.h]);
    }
    for c in &f.children {
        collect_boxes(c, out);
    }
}

/// The layout boxes of element `id` (its own and its descendants'), for
/// `getBoundingClientRect`: the element's own boxes if it has any,
/// otherwise its content's.
pub fn rects_of(page: &Page, doc: &Document, id: html::NodeId) -> Vec<[f32; 4]> {
    if let Some(b) = page.boxes.get(&id) {
        return b.clone();
    }
    let mut out = Vec::new();
    for d in doc.descendants(id) {
        if let Some(b) = page.boxes.get(&d) {
            out.extend_from_slice(b);
        }
    }
    out
}

/// The innermost element whose box contains (x, y) (CSS px).
pub fn hit_test(page: &Page, doc: &Document, x: f32, y: f32) -> Option<html::NodeId> {
    let mut best: Option<(html::NodeId, f32)> = None;
    for (&n, rects) in &page.boxes {
        if !matches!(
            doc.nodes.get(n).map(|x| &x.kind),
            Some(html::NodeKind::Element { .. })
        ) {
            continue;
        }
        for r in rects {
            if x >= r[0] && y >= r[1] && x < r[0] + r[2] && y < r[1] + r[3] {
                let area = r[2] * r[3];
                if best.is_none_or(|b| area <= b.1) {
                    best = Some((n, area));
                }
            }
        }
    }
    best.map(|b| b.0)
}

/// Computed values of element `id` for `getComputedStyle` (the
/// properties scripts commonly read), with `author` sheets applied.
pub fn computed_style(
    doc: &Document,
    opts: &Options,
    author: &[String],
    id: html::NodeId,
) -> Vec<(String, String)> {
    let device = cell_device(opts);
    let sheets: Vec<String> = if opts.author_css {
        author.to_vec()
    } else {
        Vec::new()
    };
    let styles = style_set(device, &sheets, &mut |_| None);
    let nav = dom::Nav::new(doc, &opts.state);
    // The chain from the root element down to `id`.
    let mut chain = Vec::new();
    let mut p = Some(id);
    while let Some(x) = p {
        if x == 0 {
            break;
        }
        chain.push(x);
        p = doc.nodes.get(x).and_then(|n| n.parent);
    }
    chain.reverse();
    let mut st: Option<css::ComputedStyle> = None;
    let mut hidden = false;
    for &n in &chain {
        let Some((tag, attrs)) = doc.element(n) else {
            return Vec::new();
        };
        let table_attr = |_: &str| None;
        let hints = css::hints::presentational_hints(tag, attrs, &table_attr);
        let s = styles.compute(&nav.el(n), st.as_ref(), doc.attr(n, "style"), &hints, None);
        hidden |= s.display == css::style::Display::None;
        st = Some(s);
    }
    let Some(s) = st else { return Vec::new() };
    let kebab = |d: &str| {
        let mut o = String::new();
        for (i, c) in d.chars().enumerate() {
            if c.is_ascii_uppercase() {
                if i > 0 {
                    o.push('-');
                }
                o.push(c.to_ascii_lowercase());
            } else {
                o.push(c);
            }
        }
        o
    };
    let rgb = |c: css::values::Rgba| {
        if c.a == 255 {
            format!("rgb({}, {}, {})", c.r, c.g, c.b)
        } else {
            format!(
                "rgba({}, {}, {}, {})",
                c.r,
                c.g,
                c.b,
                css::math::roundf(c.a as f32 / 255.0 * 100.0) / 100.0
            )
        }
    };
    let px = |v: f32| format!("{}px", css::math::roundf(v * 100.0) / 100.0);
    let _ = hidden;
    let mut out = Vec::new();
    let mut put = |k: &str, v: String| out.push((String::from(k), v));
    put("display", kebab(&format!("{:?}", s.display)));
    put("position", String::from(s.position.as_str()));
    put("float", String::from(s.float.as_str()));
    put("visibility", String::from(s.visibility.as_str()));
    put("opacity", format!("{}", s.opacity));
    put("color", rgb(s.color));
    put("background-color", rgb(s.background_color_rgba()));
    put("font-size", px(s.font_size));
    put("font-weight", format!("{}", s.font_weight));
    put("font-style", String::from(s.font_style.as_str()));
    put("text-align", String::from(s.text_align.as_str()));
    put("white-space", String::from(s.white_space.as_str()));
    put("line-height", px(s.line_height_px()));
    out
}

/// Render with the document's own `<style>` sheets only (no network).
pub fn render(doc: &Document, opts: &Options) -> Page {
    let device = cell_device(opts);
    let author: Vec<String> = sheet_sources(doc, &device)
        .into_iter()
        .filter_map(|s| match s {
            SheetSource::Inline(t) => Some(t),
            SheetSource::Link(_) => None,
        })
        .collect();
    render_with(doc, opts, &author, &mut |_| None)
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

/// URLs a style sheet `@import`s (whose media query applies to a terminal).
pub fn imports_of(css: &str) -> Vec<String> {
    let mut probe = StyleSet::new(cell_device(&Options::default()));
    probe
        .add(css, Origin::Author)
        .into_iter()
        .filter(|i| i.applies)
        .map(|i| i.url)
        .collect()
}
