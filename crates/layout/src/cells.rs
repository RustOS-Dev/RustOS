//! Paint fragments onto a grid of character cells (8×16 px each) and
//! turn it into the browser's [`Page`] lines.

use crate::fragment::{FragKind, Fragment};
use crate::geom::Rect;
use crate::metrics::{CELL_H, CELL_W};
use crate::page::{Field, Line, Page, Span, Style, Target, char_width, field_text};
use crate::roundf;
use crate::tree::ReplacedKind;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use css::Rgba;
use css::style::{DECO_LINE_THROUGH, DECO_UNDERLINE, FontStyle, Position, Visibility};
use html::Document;

#[derive(Clone, PartialEq)]
struct Cell {
    ch: char,
    /// Second half of a wide character.
    cont: bool,
    style: Style,
    target: Target,
    /// Author background painted here.
    bg: Option<Rgba>,
}

impl Cell {
    fn blank() -> Cell {
        Cell {
            ch: ' ',
            cont: false,
            style: Style::default(),
            target: Target::None,
            bg: None,
        }
    }
}

pub struct Canvas<'a> {
    cols: usize,
    rows: Vec<Vec<Cell>>,
    doc: &'a Document,
    fields: &'a [Field],
    /// (anchor index, row)
    anchors: Vec<(usize, usize)>,
}

fn col_of(x: f32) -> i64 {
    roundf(x / CELL_W) as i64
}

fn row_of(y: f32) -> i64 {
    roundf(y / CELL_H) as i64
}

fn luminance(c: Rgba) -> f32 {
    (0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32) / 255.0
}

fn is_whiteish(c: Rgba) -> bool {
    c.a < 128 || (c.r > 235 && c.g > 235 && c.b > 235)
}

impl<'a> Canvas<'a> {
    pub fn new(cols: usize, doc: &'a Document, fields: &'a [Field]) -> Canvas<'a> {
        Canvas {
            cols,
            rows: Vec::new(),
            doc,
            fields,
            anchors: Vec::new(),
        }
    }

    fn cell(&mut self, row: i64, col: i64) -> Option<&mut Cell> {
        if row < 0 || col < 0 || col as usize >= self.cols {
            return None;
        }
        let row = row as usize;
        while self.rows.len() <= row {
            self.rows.push(vec![Cell::blank(); self.cols]);
        }
        self.rows[row].get_mut(col as usize)
    }

    fn fill_bg(&mut self, r: Rect, bg: Rgba, clip: Option<Rect>) {
        let r = match clip {
            Some(c) => r.intersect(&c),
            None => r,
        };
        let (c0, c1) = (col_of(r.x), col_of(r.right()));
        let (r0, r1) = (row_of(r.y), row_of(r.bottom()));
        for row in r0..r1 {
            for col in c0..c1 {
                if let Some(c) = self.cell(row, col) {
                    c.bg = Some(bg);
                }
            }
        }
    }

    fn put_text(
        &mut self,
        x: f32,
        y: f32,
        text: &str,
        style: Style,
        target: Target,
        clip: Option<Rect>,
    ) {
        let row = row_of(y);
        let mut col = col_of(x);
        let (cmin, cmax, rmin, rmax) = match clip {
            Some(c) => (
                col_of(c.x),
                col_of(c.right()),
                row_of(c.y),
                row_of(c.bottom()),
            ),
            None => (i64::MIN, i64::MAX, i64::MIN, i64::MAX),
        };
        if row < rmin || row >= rmax {
            return;
        }
        for ch in text.chars() {
            let w = char_width(ch) as i64;
            if w == 0 {
                continue;
            }
            if col >= cmin && col + w <= cmax.min(self.cols as i64) {
                if let Some(c) = self.cell(row, col) {
                    c.ch = ch;
                    c.cont = false;
                    c.style = style;
                    c.target = target;
                }
                if w == 2
                    && let Some(c) = self.cell(row, col + 1)
                {
                    c.ch = ' ';
                    c.cont = true;
                    c.style = style;
                    c.target = target;
                }
            }
            col += w;
        }
    }

    fn text_style(f: &Fragment, deco: u8) -> Style {
        let s = &f.style;
        Style {
            bold: s.font_weight >= 600,
            underline: deco & DECO_UNDERLINE != 0,
            italic: s.font_style != FontStyle::Normal,
            heading: f.heading,
            dim: false,
            strike: deco & DECO_LINE_THROUGH != 0,
            fg: Some((s.color.r, s.color.g, s.color.b)),
            bg: None,
        }
    }

    /// Paint a fragment tree (in-flow content first, then positioned
    /// boxes by `z-index`).
    pub fn paint(&mut self, root: &Fragment) {
        let mut deferred: Vec<(i32, usize, &Fragment, Option<Rect>)> = Vec::new();
        self.paint_frag(root, None, &mut deferred, true);
        let mut round = 0;
        while !deferred.is_empty() {
            deferred.sort_by_key(|d| (d.0, d.1));
            let (_, _, f, clip) = deferred.remove(0);
            let mut more = Vec::new();
            // `top`: paint this positioned box now (its own positioned
            // descendants are deferred again).
            self.paint_frag(f, clip, &mut more, true);
            round += 1;
            let base = 1_000_000 * round;
            for (k, m) in more.into_iter().enumerate() {
                deferred.push((m.0, base + k, m.2, m.3));
            }
        }
    }

    fn paint_frag<'f>(
        &mut self,
        f: &'f Fragment,
        clip: Option<Rect>,
        deferred: &mut Vec<(i32, usize, &'f Fragment, Option<Rect>)>,
        top: bool,
    ) {
        let s = &f.style;
        if s.opacity == 0.0 {
            return;
        }
        if !top
            && matches!(f.kind, FragKind::Box)
            && (matches!(s.position, Position::Absolute | Position::Fixed)
                || s.z_index.is_some() && s.position != Position::Static)
        {
            let n = deferred.len();
            deferred.push((s.z_index.unwrap_or(0), n, f, clip));
            return;
        }
        let visible = s.visibility == Visibility::Visible;
        match &f.kind {
            FragKind::Box => {
                if visible {
                    let bg = s.background_color_rgba();
                    if bg.a > 0 {
                        self.fill_bg(f.rect, bg, clip);
                    }
                    if f.node.is_some_and(|n| self.doc.tag(n) == "hr") && f.rect.h >= CELL_H - 0.5 {
                        let n = ((f.rect.w / CELL_W) as usize).max(1);
                        let line: String = core::iter::repeat_n('─', n).collect();
                        self.put_text(
                            f.rect.x,
                            f.rect.y,
                            &line,
                            Style {
                                dim: true,
                                ..Style::default()
                            },
                            Target::None,
                            clip,
                        );
                    }
                }
            }
            FragKind::Text { text, deco, .. } => {
                if visible {
                    if matches!(f.target, Target::Anchor(i) if text.is_empty() && { self.anchors.push((i, row_of(f.rect.y).max(0) as usize)); true })
                    {
                        return;
                    }
                    let mut st = Self::text_style(f, *deco);
                    if matches!(f.target, Target::Link(_))
                        && text.starts_with('[')
                        && text.ends_with(']')
                        && text[1..text.len() - 1].chars().all(|c| c.is_ascii_digit())
                    {
                        st.dim = true;
                        st.underline = false;
                    }
                    self.put_text(f.rect.x, f.rect.y, text, st, f.target, clip);
                }
            }
            FragKind::Marker(text) => {
                if visible {
                    let st = Self::text_style(f, 0);
                    self.put_text(f.rect.x, f.rect.y, text, st, Target::None, clip);
                }
            }
            FragKind::Replaced(r) => {
                if visible {
                    let st = Self::text_style(f, s.text_decoration_line);
                    let (text, target) = match r {
                        ReplacedKind::Image { alt, .. } => (
                            if alt.is_empty() {
                                String::new()
                            } else {
                                format!("[{}]", alt)
                            },
                            f.target,
                        ),
                        ReplacedKind::Field(i) => (
                            self.fields.get(*i).map(field_text).unwrap_or_default(),
                            Target::Field(*i),
                        ),
                        ReplacedKind::Placeholder(p) => (
                            if p.is_empty() {
                                String::new()
                            } else {
                                format!("[{}]", p)
                            },
                            f.target,
                        ),
                    };
                    let st = if matches!(r, ReplacedKind::Field(_)) {
                        Style { fg: None, ..st }
                    } else {
                        st
                    };
                    self.put_text(f.rect.x, f.rect.y, &text, st, target, clip);
                }
            }
        }
        let child_clip = match (clip, f.clip) {
            (Some(a), Some(b)) => Some(a.intersect(&b)),
            (a, b) => b.or(a),
        };
        for c in &f.children {
            self.paint_frag(c, child_clip, deferred, false);
        }
    }

    /// The painted page as lines of spans, with link/field positions and
    /// anchors.
    pub fn into_page(self, page: &mut Page, anchor_names: &[String]) {
        let mut lines = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let mut spans: Vec<Span> = Vec::new();
            // Trailing blank cells without background are dropped.
            let end = row
                .iter()
                .rposition(|c| {
                    c.ch != ' '
                        || c.bg.is_some_and(|b| !is_whiteish(b))
                        || !matches!(c.target, Target::None)
                })
                .map_or(0, |i| i + 1);
            for c in &row[..end] {
                if c.cont {
                    continue;
                }
                let mut st = c.style;
                // Terminal colors: an author background is shown with the
                // exact foreground; on the terminal's own background only
                // light or strongly colored text keeps its color.
                match c.bg.filter(|b| !is_whiteish(*b)) {
                    Some(b) => {
                        st.bg = Some((b.r, b.g, b.b));
                        if st.fg.is_none() {
                            st.fg = None;
                        }
                    }
                    None => {
                        st.bg = None;
                        st.fg = st.fg.filter(|&(r, g, b)| {
                            let col = Rgba::rgb(r, g, b);
                            let max = r.max(g).max(b) as i32;
                            let min = r.min(g).min(b) as i32;
                            luminance(col) > 0.55 || (max - min > 90 && luminance(col) > 0.25)
                        });
                    }
                }
                if matches!(c.target, Target::Link(_) | Target::Field(_)) {
                    // The browser draws links and fields itself.
                    st.fg = None;
                    if c.bg.is_none_or(is_whiteish) {
                        st.bg = None;
                    }
                }
                match spans.last_mut() {
                    Some(last) if last.style == st && last.target == c.target => {
                        last.text.push(c.ch)
                    }
                    _ => spans.push(Span {
                        text: String::from(c.ch),
                        style: st,
                        target: c.target,
                    }),
                }
            }
            lines.push(Line { spans });
        }
        while lines.last().is_some_and(|l: &Line| l.spans.is_empty()) {
            lines.pop();
        }
        // Leading blank rows (top margins) waste terminal lines.
        let skip = lines.iter().position(|l| !l.spans.is_empty()).unwrap_or(0);
        lines.drain(..skip);
        let anchors: Vec<(usize, usize)> = self
            .anchors
            .iter()
            .map(|&(i, r)| (i, r.saturating_sub(skip)))
            .collect();
        // Positions.
        for (li, line) in lines.iter().enumerate() {
            let mut col = 0;
            for s in &line.spans {
                match s.target {
                    Target::Link(i) if i < page.links.len() && page.links[i].pos.is_none() => {
                        page.links[i].pos = Some((li, col))
                    }
                    Target::Field(i) if i < page.fields.len() && page.fields[i].pos.is_none() => {
                        page.fields[i].pos = Some((li, col))
                    }
                    _ => {}
                }
                col += crate::page::text_width(&s.text);
            }
        }
        for (i, row) in anchors {
            if let Some(n) = anchor_names.get(i) {
                page.anchors
                    .push((n.clone(), row.min(lines.len().saturating_sub(1))));
            }
        }
        page.lines = lines;
    }
}
