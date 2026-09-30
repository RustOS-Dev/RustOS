//! Inline formatting contexts (CSS 2 §9.4.2, §10.8; CSS Text 3):
//! white-space processing, break opportunities, line boxes around
//! floats, alignment, list markers and atomic inline-level boxes.

use crate::flow::{Cb, Engine, Floats, PendingAbs};
use crate::fragment::{FragKind, Fragment};
use crate::geom::Rect;
use crate::page::{Target, char_width};
use crate::tree::{BoxKind, LayoutBox, StyleRef};
use alloc::string::String;
use alloc::vec::Vec;
use css::style::{Float, OverflowWrap, Position, TextAlign, VerticalAlign, WhiteSpace, WordBreak};

#[derive(Clone)]
enum Tok<'a> {
    /// Text with no break opportunity inside (a word, or preserved
    /// non-breaking spaces).
    Word {
        text: String,
        src: &'a LayoutBox,
    },
    /// A breakable (collapsible or pre-wrap) space.
    Space {
        text: String,
        src: &'a LayoutBox,
        collapsible: bool,
    },
    Break,
    Open(&'a LayoutBox),
    Close(&'a LayoutBox),
    Atomic(&'a LayoutBox),
    Float(&'a LayoutBox),
    Abs(&'a LayoutBox),
    Anchor(&'a LayoutBox),
    /// Outside list marker (hangs left of the first line).
    Marker(&'a LayoutBox),
}

fn wraps(ws: WhiteSpace) -> bool {
    !matches!(ws, WhiteSpace::Nowrap | WhiteSpace::Pre)
}

fn collapses(ws: WhiteSpace) -> bool {
    matches!(
        ws,
        WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
    )
}

/// Flatten inline-level content and turn text into tokens.
fn tokenize<'a>(kids: &'a [LayoutBox], out: &mut Vec<Tok<'a>>, prev_space: &mut bool) {
    for c in kids {
        match &c.kind {
            BoxKind::Text { text, .. } => {
                if text.is_empty() {
                    if matches!(c.target, Target::Anchor(_)) {
                        out.push(Tok::Anchor(c));
                    }
                    continue;
                }
                text_tokens(text, c, out, prev_space);
            }
            BoxKind::Marker { text, outside } => {
                if *outside {
                    out.push(Tok::Marker(c));
                } else {
                    out.push(Tok::Word {
                        text: text.clone(),
                        src: c,
                    });
                    *prev_space = text.ends_with(' ');
                }
            }
            BoxKind::LineBreak => {
                out.push(Tok::Break);
                *prev_space = true;
            }
            BoxKind::Inline => {
                out.push(Tok::Open(c));
                tokenize(&c.children, out, prev_space);
                out.push(Tok::Close(c));
            }
            _ => {
                if c.style.float != Float::None {
                    out.push(Tok::Float(c));
                } else if matches!(c.style.position, Position::Absolute | Position::Fixed) {
                    out.push(Tok::Abs(c));
                } else {
                    out.push(Tok::Atomic(c));
                    *prev_space = false;
                }
            }
        }
    }
}

fn text_tokens<'a>(text: &str, src: &'a LayoutBox, out: &mut Vec<Tok<'a>>, prev_space: &mut bool) {
    let ws = src.style.white_space;
    let collapse = collapses(ws);
    let wrap = wraps(ws);
    let keep_newlines = !matches!(ws, WhiteSpace::Normal | WhiteSpace::Nowrap);
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut Vec<Tok<'a>>| {
        if !word.is_empty() {
            out.push(Tok::Word {
                text: core::mem::take(word),
                src,
            });
        }
    };
    let mut col = 0usize; // for tab stops in preserved text
    for ch in text.chars() {
        match ch {
            '\n' if keep_newlines => {
                flush(&mut word, out);
                out.push(Tok::Break);
                *prev_space = true;
                col = 0;
            }
            ' ' | '\t' | '\n' | '\r' | '\x0c' => {
                if collapse {
                    if !*prev_space {
                        if wrap {
                            flush(&mut word, out);
                            out.push(Tok::Space {
                                text: String::from(" "),
                                src,
                                collapsible: true,
                            });
                        } else {
                            // No break opportunity: the space joins the word.
                            word.push(' ');
                        }
                        *prev_space = true;
                    }
                } else {
                    let n = if ch == '\t' { 8 - col % 8 } else { 1 };
                    let sp: String = core::iter::repeat_n(' ', n).collect();
                    col += n;
                    if wrap {
                        flush(&mut word, out);
                        out.push(Tok::Space {
                            text: sp,
                            src,
                            collapsible: false,
                        });
                    } else {
                        word.push_str(&sp);
                    }
                    *prev_space = false;
                }
            }
            c => {
                *prev_space = false;
                col += 1;
                if wrap && char_width(c) == 2 {
                    // Ideographs: a break opportunity around each.
                    flush(&mut word, out);
                    word.push(c);
                    flush(&mut word, out);
                } else {
                    word.push(c);
                    if wrap && src.style.word_break == WordBreak::BreakAll {
                        flush(&mut word, out);
                    }
                }
            }
        }
    }
    flush(&mut word, out);
}

/// A run of tokens between break opportunities.
struct Unit {
    start: usize,
    end: usize,
    /// A breakable space (dropped at line ends).
    space: bool,
}

fn units(toks: &[Tok]) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < toks.len() {
        let breakable_here = match &toks[i] {
            Tok::Space { .. } | Tok::Break | Tok::Float(_) | Tok::Abs(_) | Tok::Marker(_) => true,
            Tok::Atomic(_) => true,
            Tok::Word { src, .. } => {
                // Between two words of wide characters or break-all.
                i > start && matches!(toks[i - 1], Tok::Word { .. }) && wraps(src.style.white_space)
            }
            _ => false,
        };
        if breakable_here {
            if i > start {
                out.push(Unit {
                    start,
                    end: i,
                    space: false,
                });
            }
            let single = !matches!(toks[i], Tok::Word { .. });
            if single {
                out.push(Unit {
                    start: i,
                    end: i + 1,
                    space: matches!(toks[i], Tok::Space { .. }),
                });
                start = i + 1;
            } else {
                start = i;
            }
        }
        i += 1;
    }
    if start < toks.len() {
        out.push(Unit {
            start,
            end: toks.len(),
            space: false,
        });
    }
    out
}

struct Placed<'a> {
    tok: usize,
    x: f32,
    w: f32,
    /// Atomic box fragment (already laid out).
    frag: Option<Fragment>,
    text: Option<String>,
    src: Option<&'a LayoutBox>,
}

struct LineBox<'a> {
    placed: Vec<Placed<'a>>,
    y: f32,
    left: f32,
    right: f32,
    height: f32,
    forced: bool,
    first: bool,
}

pub struct InlineOut {
    pub frags: Vec<Fragment>,
    pub height: f32,
    pub baseline: Option<f32>,
}

impl<'a> Engine<'a> {
    fn open_close_width(&self, b: &LayoutBox, open: bool, cb_w: f32) -> f32 {
        let bm = self.box_model(b, cb_w);
        if open {
            (if bm.auto[3] { 0.0 } else { bm.margin.left }) + bm.border.left + bm.padding.left
        } else {
            (if bm.auto[1] { 0.0 } else { bm.margin.right }) + bm.border.right + bm.padding.right
        }
    }

    fn tok_width(&self, t: &Tok, cb_w: f32) -> f32 {
        match t {
            Tok::Word { text, src } | Tok::Space { text, src, .. } => {
                let s = &src.style;
                let mut w = self.m.text_width(text, s);
                if !self.m.cell_mode() {
                    w += s.letter_spacing * text.chars().count() as f32;
                    if matches!(t, Tok::Space { .. }) {
                        w += s.word_spacing;
                    }
                }
                w
            }
            Tok::Open(b) => self.open_close_width(b, true, cb_w),
            Tok::Close(b) => self.open_close_width(b, false, cb_w),
            Tok::Atomic(b) => {
                let bm = self.box_model(b, cb_w);
                self.shrink_to_fit(b, cb_w, &bm) + bm.margin.horizontal()
            }
            _ => 0.0,
        }
    }

    /// (min-content, max-content) of inline content.
    pub fn inline_intrinsic(&self, kids: &[LayoutBox]) -> (f32, f32) {
        let mut toks = Vec::new();
        let mut ps = true;
        tokenize(kids, &mut toks, &mut ps);
        let us = units(&toks);
        let mut min = 0.0f32;
        let mut max = 0.0f32;
        let mut line = 0.0f32;
        for u in &us {
            if matches!(toks[u.start], Tok::Break) {
                max = max.max(line);
                line = 0.0;
                continue;
            }
            let w: f32 = toks[u.start..u.end]
                .iter()
                .map(|t| match t {
                    Tok::Atomic(b) => self.intrinsic(b).1,
                    t => self.tok_width(t, 0.0),
                })
                .sum();
            let wmin: f32 = toks[u.start..u.end]
                .iter()
                .map(|t| match t {
                    Tok::Atomic(b) => self.intrinsic(b).0,
                    t => self.tok_width(t, 0.0),
                })
                .sum();
            if !u.space {
                // A long word may break anywhere if allowed.
                let breakable = toks[u.start..u.end].iter().all(|t| match t {
                    Tok::Word { src, .. } => {
                        matches!(src.style.overflow_wrap, OverflowWrap::Anywhere)
                            || src.style.word_break == WordBreak::BreakAll
                    }
                    _ => true,
                });
                min = min.max(if breakable {
                    self.m.text_width(
                        "m",
                        &kids.first().map_or_else(
                            || alloc::rc::Rc::new(css::ComputedStyle::default()),
                            |k| k.style.clone(),
                        ),
                    )
                } else {
                    wmin
                });
            }
            line += w;
        }
        max = max.max(line);
        (min, max)
    }

    pub fn layout_inline(
        &mut self,
        b: &'a LayoutBox,
        x: f32,
        y: f32,
        cb: Cb,
        floats: &mut Floats,
    ) -> InlineOut {
        let style = b.style.clone();
        let mut toks: Vec<Tok<'a>> = Vec::new();
        let mut ps = true;
        tokenize(&b.children, &mut toks, &mut ps);
        let us = units(&toks);
        let strut = self.m.line_height(&style);
        let x0 = x;
        let x1 = x + cb.w;
        let indent = style.text_indent.resolve(cb.w);
        let mut lines: Vec<LineBox<'a>> = Vec::new();
        let mut frags: Vec<Fragment> = Vec::new();
        let mut cursor_y = y;
        let new_line = |floats: &Floats, y: f32, first: bool| -> LineBox<'a> {
            let (l, r) = floats.band(y, strut, x0, x1);
            LineBox {
                placed: Vec::new(),
                y,
                left: l + if first { indent } else { 0.0 },
                right: r,
                height: 0.0,
                forced: false,
                first,
            }
        };
        let mut line = new_line(floats, cursor_y, true);
        let mut pen = line.left;
        let mut has_content = false;
        let mut marker: Option<(&'a LayoutBox, f32)> = None;
        let mut ui = 0;
        while ui < us.len() {
            let u = &us[ui];
            let first_tok = &toks[u.start];
            match first_tok {
                Tok::Break => {
                    line.forced = true;
                    finish_line(&mut line, strut, &mut lines);
                    cursor_y = lines.last().map_or(cursor_y, |l| l.y + l.height);
                    line = new_line(floats, cursor_y, false);
                    pen = line.left;
                    has_content = false;
                    ui += 1;
                    continue;
                }
                Tok::Marker(m) => {
                    if let BoxKind::Marker { text, .. } = &m.kind {
                        marker = Some((m, self.m.text_width(text, &m.style)));
                    }
                    ui += 1;
                    continue;
                }
                Tok::Float(fb) => {
                    let fy = if has_content {
                        cursor_y + strut.max(line.height)
                    } else {
                        cursor_y
                    };
                    let f = self.layout_float(fb, x0, fy, cb, floats);
                    frags.push(f);
                    if !has_content {
                        let (l, r) = floats.band(cursor_y, strut, x0, x1);
                        line.left = l + if line.first { indent } else { 0.0 };
                        line.right = r;
                        pen = line.left;
                    }
                    ui += 1;
                    continue;
                }
                Tok::Abs(ab) => {
                    let p = PendingAbs {
                        b: ab,
                        static_x: pen,
                        static_y: cursor_y,
                    };
                    if ab.style.position == Position::Fixed {
                        self.fixed.push(p);
                    } else if let Some(top) = self.abs_stack.last_mut() {
                        top.push(p);
                    } else {
                        self.fixed.push(p);
                    }
                    ui += 1;
                    continue;
                }
                _ => {}
            }
            if u.space {
                if let Tok::Space {
                    collapsible: true, ..
                } = first_tok
                    && !has_content
                {
                    ui += 1;
                    continue; // spaces at the start of a line are removed
                }
                let w = self.tok_width(first_tok, cb.w);
                line.placed.push(Placed {
                    tok: u.start,
                    x: pen,
                    w,
                    frag: None,
                    text: None,
                    src: None,
                });
                pen += w;
                ui += 1;
                continue;
            }
            // Measure the unit.
            let mut widths: Vec<f32> = Vec::with_capacity(u.end - u.start);
            let mut atomic_frags: Vec<Option<Fragment>> = Vec::with_capacity(u.end - u.start);
            for t in &toks[u.start..u.end] {
                if let Tok::Atomic(ab) = t {
                    let (f, w) = self.layout_atomic(ab, cb);
                    widths.push(w);
                    atomic_frags.push(Some(f));
                } else {
                    widths.push(self.tok_width(t, cb.w));
                    atomic_frags.push(None);
                }
            }
            let uw: f32 = widths.iter().sum();
            if has_content && pen + uw > line.right + 0.01 {
                // Wrap before this unit.
                trim_trailing_spaces(&mut line, &toks);
                finish_line(&mut line, strut, &mut lines);
                cursor_y = lines.last().map_or(cursor_y, |l| l.y + l.height);
                // Find room for the unit below floats if needed.
                let mut ny = cursor_y;
                let (l, r) = floats.band(ny, strut, x0, x1);
                if r - l < uw && uw <= x1 - x0 {
                    ny = floats.fit(ny, strut, uw, x0, x1);
                }
                line = new_line(floats, ny, false);
                cursor_y = ny;
                pen = line.left;
                has_content = false;
            } else if !has_content
                && pen + uw > line.right + 0.01
                && line.right - line.left < uw
                && uw <= x1 - x0
                && !floats_empty(floats)
            {
                // Move below floats to fit.
                let ny = floats.fit(cursor_y, strut, uw, x0, x1);
                if ny > cursor_y {
                    line = new_line(floats, ny, line.first);
                    cursor_y = ny;
                    pen = line.left;
                }
            }
            // An overlong word (possibly inside inline boxes) is split if
            // allowed (always in a terminal).
            let words: Vec<usize> = (u.start..u.end)
                .filter(|&t| matches!(toks[t], Tok::Word { .. }))
                .collect();
            let only_boxes = (u.start..u.end)
                .all(|t| matches!(toks[t], Tok::Word { .. } | Tok::Open(_) | Tok::Close(_)));
            if !has_content
                && uw > line.right - line.left
                && words.len() == 1
                && only_boxes
                && let Tok::Word { text, src } = &toks[words[0]]
            {
                let wi = words[0];
                let may_split = self.m.cell_mode()
                    || matches!(
                        src.style.overflow_wrap,
                        OverflowWrap::Anywhere | OverflowWrap::BreakWord
                    )
                    || src.style.word_break == WordBreak::BreakWord
                    || src.style.word_break == WordBreak::BreakAll;
                if may_split && wraps(src.style.white_space) {
                    for (k, t) in (u.start..wi).enumerate() {
                        line.placed.push(Placed {
                            tok: t,
                            x: pen,
                            w: widths[k],
                            frag: None,
                            text: None,
                            src: None,
                        });
                        pen += widths[k];
                    }
                    let mut rest: &str = text;
                    let mut first_piece = true;
                    while !rest.is_empty() {
                        if !first_piece {
                            finish_line(&mut line, strut, &mut lines);
                            cursor_y = lines.last().map_or(cursor_y, |l| l.y + l.height);
                            line = new_line(floats, cursor_y, false);
                            pen = line.left;
                        }
                        first_piece = false;
                        let room = (line.right - pen).max(0.0);
                        let mut take = 0;
                        let mut w = 0.0;
                        for (i, ch) in rest.char_indices() {
                            let cw = self.m.text_width(&rest[i..i + ch.len_utf8()], &src.style);
                            if w + cw > room && take > 0 {
                                break;
                            }
                            w += cw;
                            take = i + ch.len_utf8();
                        }
                        line.placed.push(Placed {
                            tok: wi,
                            x: pen,
                            w,
                            frag: None,
                            text: Some(String::from(&rest[..take])),
                            src: Some(src),
                        });
                        pen += w;
                        rest = &rest[take..];
                    }
                    for (k, t) in (wi + 1..u.end).enumerate() {
                        let w = widths[wi + 1 - u.start + k];
                        line.placed.push(Placed {
                            tok: t,
                            x: pen,
                            w,
                            frag: None,
                            text: None,
                            src: None,
                        });
                        pen += w;
                    }
                    has_content = true;
                    ui += 1;
                    continue;
                }
            }
            for (k, t) in (u.start..u.end).enumerate() {
                let w = widths[k];
                let frag = atomic_frags[k].take();
                let _ = &toks[t];
                line.placed.push(Placed {
                    tok: t,
                    x: pen,
                    w,
                    frag,
                    text: None,
                    src: None,
                });
                pen += w;
            }
            has_content = true;
            ui += 1;
        }
        trim_trailing_spaces(&mut line, &toks);
        if !line.placed.is_empty() || line.forced || lines.is_empty() && marker.is_some() {
            line.forced = true;
            finish_line(&mut line, strut, &mut lines);
        }
        // Heights, alignment and fragments.
        let mut last_baseline = None;
        let n_lines = lines.len();
        for (li, ln) in lines.iter_mut().enumerate() {
            let used = ln.placed.last().map_or(0.0, |p| p.x + p.w) - ln.left;
            let free = (ln.right - ln.left - used).max(0.0);
            let last = li + 1 == n_lines || ln.forced;
            let offset = match style.text_align {
                TextAlign::Center | TextAlign::WebkitCenter => free / 2.0,
                TextAlign::Right | TextAlign::End => free,
                _ => 0.0,
            };
            let offset = self.m.snap_x(offset);
            // Justification: spread the free space over the spaces.
            let mut extra_per_space = 0.0;
            if style.text_align == TextAlign::Justify && !last && !self.m.cell_mode() {
                let spaces = ln
                    .placed
                    .iter()
                    .filter(|p| matches!(toks[p.tok], Tok::Space { .. }))
                    .count();
                if spaces > 0 {
                    extra_per_space = free / spaces as f32;
                }
            }
            let mut shift = offset;
            let baseline = ln.y + self.m.ascent(&style) + (ln.height - strut).max(0.0);
            last_baseline = Some(baseline);
            // Text runs: merge adjacent tokens of the same box.
            let mut run: Option<(Fragment, &'a LayoutBox)> = None;
            let mut opens: Vec<(&'a LayoutBox, f32)> = Vec::new();
            let flush_run = |run: &mut Option<(Fragment, &'a LayoutBox)>,
                             frags: &mut Vec<Fragment>| {
                if let Some((f, _)) = run.take() {
                    frags.push(f);
                }
            };
            for p in ln.placed.iter_mut() {
                let px = p.x + shift;
                match &toks[p.tok] {
                    Tok::Word { text, src } | Tok::Space { text, src, .. } => {
                        let src: &'a LayoutBox = p.src.unwrap_or(src);
                        let text = p.text.clone().unwrap_or_else(|| text.clone());
                        let is_space = matches!(toks[p.tok], Tok::Space { .. });
                        let w = p.w + if is_space { extra_per_space } else { 0.0 };
                        if is_space {
                            shift += extra_per_space;
                        }
                        let lh = self.m.line_height(&src.style);
                        let mut ty = ln.y + (ln.height - lh).max(0.0);
                        if !self.m.cell_mode() {
                            match src.style.vertical_align {
                                VerticalAlign::Sub => ty += src.style.font_size * 0.2,
                                VerticalAlign::Super => ty -= src.style.font_size * 0.3,
                                _ => {}
                            }
                        }
                        let same = run.as_ref().is_some_and(|(f, s)| {
                            core::ptr::eq(*s, src) && (f.rect.right() - px).abs() < 0.01
                        });
                        if same {
                            let (f, _) = run.as_mut().unwrap();
                            f.rect.w += w;
                            if let FragKind::Text { text: t, .. } = &mut f.kind {
                                t.push_str(&text);
                            }
                        } else {
                            flush_run(&mut run, &mut frags);
                            let deco = if let BoxKind::Text { deco, .. } = src.kind {
                                deco
                            } else {
                                0
                            };
                            let mut f = Fragment::new(
                                Rect::new(px, ty, w, lh),
                                src.style.clone(),
                                None,
                                FragKind::Text {
                                    text,
                                    deco,
                                    baseline: ty + self.m.ascent(&src.style),
                                },
                            );
                            f.target = src.target;
                            f.heading = src.heading;
                            run = Some((f, src));
                        }
                    }
                    Tok::Open(ib) => {
                        flush_run(&mut run, &mut frags);
                        opens.push((ib, px));
                    }
                    Tok::Close(ib) => {
                        flush_run(&mut run, &mut frags);
                        let start = opens
                            .iter()
                            .rposition(|(o, _)| core::ptr::eq(*o, *ib))
                            .map(|i| opens.remove(i).1)
                            .unwrap_or(ln.left + offset);
                        frags.push(inline_box_frag(
                            ib,
                            start,
                            px + p.w,
                            ln.y,
                            ln.height,
                            strut,
                            self,
                        ));
                    }
                    Tok::Atomic(_) => {
                        flush_run(&mut run, &mut frags);
                        if let Some(mut f) = p.frag.take() {
                            let fh = f.rect.h;
                            let bm = self.box_model(f_box(&toks[p.tok]), cb.w);
                            let mh = fh + bm.margin.vertical();
                            let top = match f_box(&toks[p.tok]).style.vertical_align {
                                VerticalAlign::Top | VerticalAlign::TextTop => ln.y,
                                VerticalAlign::Middle => ln.y + (ln.height - mh) / 2.0,
                                _ => ln.y + (ln.height - mh).max(0.0),
                            };
                            let tx =
                                px + (if bm.auto[3] { 0.0 } else { bm.margin.left }) - f.rect.x;
                            let ty = self.m.snap_y(top + bm.margin.top) - f.rect.y;
                            f.translate(tx, ty);
                            frags.push(f);
                        }
                    }
                    Tok::Anchor(ab) => {
                        flush_run(&mut run, &mut frags);
                        let mut f = Fragment::new(
                            Rect::new(px, ln.y, 0.0, 0.0),
                            ab.style.clone(),
                            None,
                            FragKind::Text {
                                text: String::new(),
                                deco: 0,
                                baseline: ln.y,
                            },
                        );
                        f.target = ab.target;
                        frags.push(f);
                    }
                    _ => {}
                }
            }
            flush_run(&mut run, &mut frags);
            // Inline boxes still open at the line end.
            for (ib, start) in opens {
                let end = ln.placed.last().map_or(start, |p| p.x + p.w + shift);
                frags.push(inline_box_frag(
                    ib, start, end, ln.y, ln.height, strut, self,
                ));
            }
            if ln.first
                && let Some((mb, mw)) = marker.take()
                && let BoxKind::Marker { text, .. } = &mb.kind
            {
                let mx = self.m.snap_x(x0 - mw);
                let f = Fragment::new(
                    Rect::new(mx, ln.y, mw, strut),
                    mb.style.clone(),
                    None,
                    FragKind::Marker(text.clone()),
                );
                frags.push(f);
            }
        }
        let height = lines.last().map_or(0.0, |l| l.y + l.height) - y;
        InlineOut {
            frags,
            height: height.max(0.0),
            baseline: last_baseline,
        }
    }

    /// Lay out an atomic inline (inline-block, replaced, inline-flex...):
    /// its fragment at the origin and its margin-box width.
    fn layout_atomic(&mut self, ab: &'a LayoutBox, cb: Cb) -> (Fragment, f32) {
        let bm = self.box_model(ab, cb.w);
        let w = self.shrink_to_fit(ab, cb.w, &bm);
        let mut fl = Floats::default();
        let out = self.layout_block_sized(ab, cb, 0.0, 0.0, w, &mut fl);
        let mut f = out.frag;
        if ab.style.position == Position::Relative {
            let (dx, dy) = self.relative_offset(ab, cb);
            f.translate(dx, dy);
        }
        (f, w + bm.margin.horizontal())
    }
}

fn floats_empty(f: &Floats) -> bool {
    f.bottom() == 0.0
}

fn f_box<'a>(t: &Tok<'a>) -> &'a LayoutBox {
    match t {
        Tok::Atomic(b) => b,
        _ => unreachable!(),
    }
}

fn inline_box_frag(
    ib: &LayoutBox,
    x0: f32,
    x1: f32,
    y: f32,
    h: f32,
    strut: f32,
    e: &Engine,
) -> Fragment {
    let lh = e.m.line_height(&ib.style).min(h.max(strut));
    let top = y + (h - lh).max(0.0);
    let mut f = Fragment::new(
        Rect::new(x0, top, (x1 - x0).max(0.0), lh),
        ib.style.clone(),
        ib.node,
        FragKind::Box,
    );
    f.target = ib.target;
    f.heading = ib.heading;
    f
}

fn trim_trailing_spaces(line: &mut LineBox, toks: &[Tok]) {
    while let Some(p) = line.placed.last() {
        if matches!(
            toks[p.tok],
            Tok::Space {
                collapsible: true,
                ..
            }
        ) {
            line.placed.pop();
        } else {
            break;
        }
    }
}

fn finish_line<'a>(line: &mut LineBox<'a>, strut: f32, lines: &mut Vec<LineBox<'a>>) {
    let mut h = strut;
    for p in &line.placed {
        if let Some(f) = &p.frag {
            h = h.max(f.rect.h);
        }
    }
    line.height = h;
    let done = core::mem::replace(
        line,
        LineBox {
            placed: Vec::new(),
            y: 0.0,
            left: 0.0,
            right: 0.0,
            height: 0.0,
            forced: false,
            first: false,
        },
    );
    lines.push(done);
}

#[allow(dead_code)]
fn style_of(b: &LayoutBox) -> &StyleRef {
    &b.style
}
