//! The layout engine: block flow (CSS 2 §9–10) with margin collapsing,
//! floats and clearance, positioning, replaced elements and intrinsic
//! sizes. Inline, flex, grid and table layout live in their own modules
//! as further `impl Engine` blocks.

use crate::fragment::{FragKind, Fragment};
use crate::geom::{Edges, Rect};
use crate::metrics::Metrics;
use crate::page::Field;
use crate::tree::{BoxKind, LayoutBox, Replaced, ReplacedKind};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use css::style::{BoxSizing, Clear, Display, Float, Overflow, Position, Size};
use css::values::LengthAuto;
use html::Document;

/// Containing block: its width and (if definite) height.
#[derive(Debug, Clone, Copy)]
pub struct Cb {
    pub w: f32,
    pub h: Option<f32>,
}

/// Collapsing margins: the largest positive and the most negative.
#[derive(Debug, Clone, Copy, Default)]
pub struct Collapse {
    pos: f32,
    neg: f32,
}

impl Collapse {
    pub fn of(m: f32) -> Collapse {
        let mut c = Collapse::default();
        c.add(m);
        c
    }
    pub fn add(&mut self, m: f32) {
        if m >= 0.0 {
            self.pos = self.pos.max(m);
        } else {
            self.neg = self.neg.min(m);
        }
    }
    pub fn merge(&mut self, o: Collapse) {
        self.pos = self.pos.max(o.pos);
        self.neg = self.neg.min(o.neg);
    }
    pub fn resolve(&self) -> f32 {
        self.pos + self.neg
    }
}

/// Floats of one block formatting context (margin boxes).
#[derive(Debug, Default, Clone)]
pub struct Floats {
    items: Vec<(Rect, bool)>,
}

impl Floats {
    /// Free horizontal band [left, right) within [x0, x1) for a line from
    /// `y` of height `h`.
    pub fn band(&self, y: f32, h: f32, x0: f32, x1: f32) -> (f32, f32) {
        let (mut l, mut r) = (x0, x1);
        for (f, left) in &self.items {
            if f.y < y + h.max(0.01) && f.bottom() > y {
                if *left {
                    l = l.max(f.right());
                } else {
                    r = r.min(f.x);
                }
            }
        }
        (l, r.max(l))
    }

    /// The first y at or below `y` with at least `w` free width.
    pub fn fit(&self, y: f32, h: f32, w: f32, x0: f32, x1: f32) -> f32 {
        let mut y = y;
        for _ in 0..(self.items.len() + 1) {
            let (l, r) = self.band(y, h, x0, x1);
            if r - l >= w - 0.01 {
                return y;
            }
            // Move below the float that ends first among those in the band.
            let next = self
                .items
                .iter()
                .filter(|(f, _)| f.y < y + h.max(0.01) && f.bottom() > y)
                .map(|(f, _)| f.bottom())
                .fold(f32::INFINITY, f32::min);
            if !next.is_finite() {
                return y;
            }
            y = next;
        }
        y
    }

    pub fn place(&mut self, w: f32, h: f32, left: bool, y: f32, x0: f32, x1: f32) -> (f32, f32) {
        let y = self.fit(y, h, w.min(x1 - x0), x0, x1);
        let (l, r) = self.band(y, h, x0, x1);
        let x = if left { l } else { r - w };
        self.items.push((Rect::new(x, y, w, h), left));
        (x, y)
    }

    /// Clearance: y below the floats `clear` refers to.
    pub fn clear_y(&self, clear: Clear, y: f32) -> f32 {
        let mut out = y;
        for (f, left) in &self.items {
            let applies = match clear {
                Clear::Left | Clear::InlineStart => *left,
                Clear::Right | Clear::InlineEnd => !*left,
                Clear::Both => true,
                Clear::None => false,
            };
            if applies {
                out = out.max(f.bottom());
            }
        }
        out
    }

    pub fn bottom(&self) -> f32 {
        self.items
            .iter()
            .map(|(f, _)| f.bottom())
            .fold(0.0, f32::max)
    }
}

pub struct PendingAbs<'a> {
    pub b: &'a LayoutBox,
    pub static_x: f32,
    pub static_y: f32,
}

pub struct Engine<'a> {
    pub m: &'a dyn Metrics,
    pub doc: &'a Document,
    pub fields: &'a [Field],
    pub viewport: (f32, f32),
    pub(crate) abs_stack: Vec<Vec<PendingAbs<'a>>>,
    pub(crate) fixed: Vec<PendingAbs<'a>>,
    icache: core::cell::RefCell<BTreeMap<usize, (f32, f32)>>,
}

/// Resolved margins, borders and padding of a box.
#[derive(Debug, Clone, Copy, Default)]
pub struct BoxModel {
    pub margin: Edges,
    pub border: Edges,
    pub padding: Edges,
    pub auto: [bool; 4],
}

impl BoxModel {
    pub fn frame_h(&self) -> f32 {
        self.border.horizontal() + self.padding.horizontal()
    }
    pub fn frame_v(&self) -> f32 {
        self.border.vertical() + self.padding.vertical()
    }
}

pub struct BlockOut {
    pub frag: Fragment,
    /// Bottom margin escaping to the parent (collapsible).
    pub mb: Collapse,
    /// The box has no height and collapses its margins through.
    pub empty: bool,
    /// Baseline of the last line (for inline-block alignment).
    pub baseline: Option<f32>,
}

impl<'a> Engine<'a> {
    pub fn new(
        m: &'a dyn Metrics,
        doc: &'a Document,
        fields: &'a [Field],
        viewport: (f32, f32),
    ) -> Engine<'a> {
        Engine {
            m,
            doc,
            fields,
            viewport,
            abs_stack: Vec::new(),
            fixed: Vec::new(),
            icache: core::cell::RefCell::new(BTreeMap::new()),
        }
    }

    pub fn tag(&self, b: &LayoutBox) -> &'a str {
        b.node.map_or("", |n| self.doc.tag(n))
    }

    pub fn box_model(&self, b: &LayoutBox, cb_w: f32) -> BoxModel {
        let s = &b.style;
        let tag = self.tag(b);
        let mut bm = BoxModel::default();
        let mut margin = [0.0f32; 4];
        for (i, m) in s.margin.iter().enumerate() {
            match m {
                LengthAuto::Auto => bm.auto[i] = true,
                LengthAuto::Lp(l) => margin[i] = l.resolve(cb_w),
            }
        }
        bm.margin = Edges::from_array(margin);
        let mut pad = [0.0f32; 4];
        for (i, p) in s.padding.iter().enumerate() {
            pad[i] = p.resolve(cb_w).max(0.0);
        }
        bm.padding = Edges::from_array(pad);
        bm.border = Edges::from_array([0, 1, 2, 3].map(|i| self.m.border(s, i, tag)));
        if self.m.cell_mode() {
            // Padding smaller than a cell does not take one.
            bm.padding = Edges {
                top: self.m.snap_y(bm.padding.top).max(0.0),
                bottom: self.m.snap_y(bm.padding.bottom).max(0.0),
                left: if bm.padding.left < 4.0 {
                    0.0
                } else {
                    bm.padding.left
                },
                right: if bm.padding.right < 4.0 {
                    0.0
                } else {
                    bm.padding.right
                },
            };
        }
        bm
    }

    /// Border-box width from a specified content/border-box width.
    fn outer_from_spec(&self, spec: f32, style: &css::ComputedStyle, bm: &BoxModel) -> f32 {
        match style.box_sizing {
            BoxSizing::ContentBox => spec + bm.frame_h(),
            BoxSizing::BorderBox => spec.max(bm.frame_h()),
        }
    }

    fn outer_h_from_spec(&self, spec: f32, style: &css::ComputedStyle, bm: &BoxModel) -> f32 {
        match style.box_sizing {
            BoxSizing::ContentBox => spec + bm.frame_v(),
            BoxSizing::BorderBox => spec.max(bm.frame_v()),
        }
    }

    /// Clamp a border-box width by min/max-width.
    pub fn clamp_w(&self, b: &LayoutBox, w: f32, cb_w: f32, bm: &BoxModel) -> f32 {
        let s = &b.style;
        let mut w = w;
        if let Some(mx) = s.max_width.resolve(Some(cb_w)) {
            w = w.min(self.outer_from_spec(mx, s, bm));
        }
        if let Some(mn) = s.min_width.resolve(Some(cb_w)) {
            w = w.max(self.outer_from_spec(mn, s, bm));
        } else if matches!(s.min_width, Size::MinContent) {
            w = w.max(self.intrinsic(b).0 - bm.margin.horizontal());
        }
        w.max(bm.frame_h())
    }

    pub fn clamp_h(&self, b: &LayoutBox, h: f32, cb_h: Option<f32>, bm: &BoxModel) -> f32 {
        let s = &b.style;
        let mut h = h;
        if let Some(mx) = s.max_height.resolve(cb_h) {
            h = h.min(self.outer_h_from_spec(mx, s, bm));
        }
        if let Some(mn) = s.min_height.resolve(cb_h) {
            h = h.max(self.outer_h_from_spec(mn, s, bm));
        }
        h.max(bm.frame_v())
    }

    /// Specified border-box width, if definite.
    pub fn spec_width(&self, b: &LayoutBox, cb_w: Option<f32>, bm: &BoxModel) -> Option<f32> {
        match &b.style.width {
            Size::Lp(l) if !l.has_percent() || cb_w.is_some() => {
                Some(self.outer_from_spec(l.resolve_opt(cb_w), &b.style, bm))
            }
            Size::MinContent => Some(self.intrinsic(b).0 - bm.margin.horizontal()),
            Size::MaxContent => Some(self.intrinsic(b).1 - bm.margin.horizontal()),
            _ => None,
        }
    }

    /// Specified border-box height, if definite.
    pub fn spec_height(&self, b: &LayoutBox, cb_h: Option<f32>, bm: &BoxModel) -> Option<f32> {
        if self.m.cell_mode() && matches!(b.style.overflow_y, Overflow::Auto | Overflow::Scroll) {
            return None; // scrollable areas show all their content in a terminal
        }
        match &b.style.height {
            Size::Lp(l) if !l.has_percent() || cb_h.is_some() => {
                Some(self.outer_h_from_spec(l.resolve_opt(cb_h), &b.style, bm))
            }
            _ => None,
        }
    }

    /// Whether `b` establishes a new block formatting context.
    pub fn is_bfc_root(&self, b: &LayoutBox) -> bool {
        let s = &b.style;
        s.float != Float::None
            || matches!(s.position, Position::Absolute | Position::Fixed)
            || !matches!(s.overflow_x, Overflow::Visible | Overflow::Clip)
            || matches!(
                s.display,
                Display::InlineBlock
                    | Display::FlowRoot
                    | Display::TableCell
                    | Display::TableCaption
                    | Display::Flex
                    | Display::InlineFlex
                    | Display::Grid
                    | Display::InlineGrid
                    | Display::Table
                    | Display::InlineTable
            )
            || !matches!(b.kind, BoxKind::Block)
            || b.node.is_none() && b.style.display != Display::Block
    }

    fn collapses_top(&self, b: &LayoutBox, bm: &BoxModel) -> bool {
        matches!(b.kind, BoxKind::Block)
            && bm.border.top == 0.0
            && bm.padding.top == 0.0
            && !self.is_bfc_root(b)
    }

    /// First in-flow child if it is block-level (its top margin may
    /// collapse with the parent's).
    fn first_block_child<'b>(&self, b: &'b LayoutBox) -> Option<&'b LayoutBox> {
        let c = b.children.iter().find(|c| {
            !(c.is_blank_text()
                || (c.style.is_out_of_flow() && !matches!(c.kind, BoxKind::Text { .. })))
        })?;
        if c.is_inline_level() || c.style.clear != Clear::None {
            None
        } else {
            Some(c)
        }
    }

    /// The top margin of `b` merged with those of its first descendants
    /// that collapse with it.
    pub fn top_chain(&self, b: &LayoutBox, cb_w: f32) -> Collapse {
        let bm = self.box_model(b, cb_w);
        let mut c = Collapse::of(if bm.auto[0] { 0.0 } else { bm.margin.top });
        if self.collapses_top(b, &bm)
            && let Some(first) = self.first_block_child(b)
        {
            c.merge(self.top_chain(first, cb_w));
        }
        c
    }

    // -----------------------------------------------------------------
    // Block-level boxes
    // -----------------------------------------------------------------

    /// Lay out a block-level box in normal flow. `x` is the left edge of
    /// the containing block's content box, `y` the box's border-box top;
    /// `chain_applied`: the caller placed the box after its collapsed top
    /// margin chain (block flow), so a collapsing first child's margin is
    /// already counted.
    pub fn layout_block(
        &mut self,
        b: &'a LayoutBox,
        cb: Cb,
        x: f32,
        y: f32,
        floats: &mut Floats,
        chain_applied: bool,
    ) -> BlockOut {
        self.layout_block_w(b, cb, x, y, floats, chain_applied, None)
    }

    /// `forced_w`: used border-box width decided by the caller (floats,
    /// inline-blocks, flex and grid items); `x` is then the margin-box left.
    pub fn layout_block_w(
        &mut self,
        b: &'a LayoutBox,
        cb: Cb,
        x: f32,
        y: f32,
        floats: &mut Floats,
        chain_applied: bool,
        forced_w: Option<f32>,
    ) -> BlockOut {
        let bm = self.box_model(b, cb.w);
        let s = &b.style;
        // Width (CSS 2 §10.3.3).
        let bfc = self.is_bfc_root(b);
        let (mut avail_x, mut avail_w) = (x, cb.w);
        if bfc && !floats.items.is_empty() {
            // A BFC root does not overlap floats: fit beside them.
            let (l, r) = floats.band(y, 1.0, x, x + cb.w);
            avail_x = l;
            avail_w = r - l;
        }
        let mut ml = if bm.auto[3] { 0.0 } else { bm.margin.left };
        let mut mr = if bm.auto[1] { 0.0 } else { bm.margin.right };
        let replaced = matches!(b.kind, BoxKind::Replaced(_));
        let w = match forced_w {
            Some(w) => {
                avail_x = x;
                w
            }
            None => match self.spec_width(b, Some(cb.w), &bm) {
                Some(w) => {
                    let w = self.clamp_w(b, w, cb.w, &bm);
                    let free = avail_w - w - ml - mr;
                    match (bm.auto[3], bm.auto[1]) {
                        (true, true) => {
                            ml = (free / 2.0).max(0.0);
                            mr = free - ml;
                        }
                        (true, false) => ml = free,
                        (false, true) => mr = free,
                        _ => {}
                    }
                    w
                }
                None if replaced => {
                    let (w, _) = self.replaced_size(b, cb);
                    let w = self.clamp_w(b, w + bm.frame_h(), cb.w, &bm);
                    let free = avail_w - w - ml - mr;
                    if bm.auto[3] && bm.auto[1] {
                        ml = (free / 2.0).max(0.0);
                    } else if bm.auto[3] {
                        ml = free;
                    }
                    w
                }
                None if matches!(b.kind, BoxKind::Table) => {
                    let w = self.table_width(b, avail_w - ml - mr, &bm);
                    let free = avail_w - w - ml - mr;
                    if bm.auto[3] && bm.auto[1] {
                        ml = (free / 2.0).max(0.0);
                    } else if bm.auto[3] {
                        ml = free;
                    }
                    w
                }
                None => self.clamp_w(b, (avail_w - ml - mr).max(0.0), cb.w, &bm),
            },
        };
        let _ = mr;
        let bx = self.m.snap_x(avail_x + ml);
        let content_x = bx + bm.border.left + bm.padding.left;
        let content_w = (w - bm.frame_h()).max(0.0);
        let content_y = y + bm.border.top + bm.padding.top;
        let spec_h = self.spec_height(b, cb.h, &bm);
        let inner_cb = Cb {
            w: content_w,
            h: spec_h.map(|h| (h - bm.frame_v()).max(0.0)),
        };
        let positioned = s.position != Position::Static;
        if positioned {
            self.abs_stack.push(Vec::new());
        }
        let mut frag = Fragment::new(
            Rect::new(bx, y, w, 0.0),
            b.style.clone(),
            b.node,
            FragKind::Box,
        );
        frag.target = b.target;
        frag.heading = b.heading;
        let mut own_floats = Floats::default();
        let fl: &mut Floats = if bfc { &mut own_floats } else { floats };
        let (content_h, mb, baseline, empty_content) = match &b.kind {
            BoxKind::Replaced(r) => {
                let (_, h) = self.replaced_size(b, cb);
                frag.kind = FragKind::Replaced(r.kind.clone());
                (h, Collapse::default(), Some(content_y + h), false)
            }
            BoxKind::Flex => {
                let (kids, h) = self.layout_flex(b, content_x, content_y, inner_cb);
                frag.children = kids;
                (h, Collapse::default(), None, false)
            }
            BoxKind::Grid => {
                let (kids, h) = self.layout_grid(b, content_x, content_y, inner_cb);
                frag.children = kids;
                (h, Collapse::default(), None, false)
            }
            BoxKind::Table => {
                let (kids, h) = self.layout_table(b, content_x, content_y, content_w, inner_cb.h);
                frag.children = kids;
                (h, Collapse::default(), None, false)
            }
            _ => {
                let collapses_bottom = bm.border.bottom == 0.0
                    && bm.padding.bottom == 0.0
                    && spec_h.is_none()
                    && !bfc
                    && s.min_height.resolve(cb.h).is_none_or(|m| m <= 0.0);
                let r = self.layout_children(
                    b,
                    content_x,
                    content_y,
                    inner_cb,
                    fl,
                    chain_applied && self.collapses_top(b, &bm),
                );
                frag.children = r.frags;
                let mut h = r.height;
                let mut escaped = Collapse::default();
                if collapses_bottom {
                    escaped = r.pending;
                } else {
                    h += r.pending.resolve().max(if self.m.cell_mode() {
                        0.0
                    } else {
                        f32::NEG_INFINITY
                    });
                }
                if bfc {
                    // A BFC root contains its floats.
                    h = h.max(fl.bottom() - content_y);
                }
                (h.max(0.0), escaped, r.baseline, r.empty)
            }
        };
        let mut h = match spec_h {
            Some(h) => h,
            None => content_h + bm.frame_v(),
        };
        h = self.clamp_h(b, h, cb.h, &bm);
        frag.rect.h = h;
        if matches!(s.overflow_x, Overflow::Hidden | Overflow::Clip)
            || matches!(s.overflow_y, Overflow::Hidden | Overflow::Clip)
        {
            frag.clip = Some(Rect::new(
                bx + bm.border.left,
                y + bm.border.top,
                w - bm.border.horizontal(),
                h - bm.border.vertical(),
            ));
        }
        if positioned {
            self.finish_positioned(b, &mut frag, &bm);
        }
        let mut mb_out = Collapse::of(if bm.auto[2] { 0.0 } else { bm.margin.bottom });
        mb_out.merge(mb);
        let empty = h == 0.0 && empty_content && bm.frame_v() == 0.0;
        if s.position == Position::Relative || s.position == Position::Sticky {
            let (dx, dy) = self.relative_offset(b, cb);
            frag.translate(dx, dy);
        }
        BlockOut {
            frag,
            mb: mb_out,
            empty,
            baseline,
        }
    }

    pub fn relative_offset(&self, b: &LayoutBox, cb: Cb) -> (f32, f32) {
        let s = &b.style;
        if s.position == Position::Sticky {
            return (0.0, 0.0);
        }
        let dx = match (s.left.resolve(cb.w), s.right.resolve(cb.w)) {
            (Some(l), _) => l,
            (None, Some(r)) => -r,
            _ => 0.0,
        };
        let dy = match (
            s.top.resolve(cb.h.unwrap_or(0.0)),
            s.bottom.resolve(cb.h.unwrap_or(0.0)),
        ) {
            (Some(t), _) => t,
            (None, Some(b)) => -b,
            _ => 0.0,
        };
        (self.m.snap_x(dx), self.m.snap_y(dy))
    }

    /// Lay out the in-flow children of a block container: either block
    /// boxes stacked vertically or an inline formatting context.
    pub fn layout_children(
        &mut self,
        b: &'a LayoutBox,
        x: f32,
        y: f32,
        cb: Cb,
        floats: &mut Floats,
        first_absorbed: bool,
    ) -> ChildrenOut {
        let inline = b.children.iter().all(|c| {
            c.is_inline_level()
                || c.style.is_out_of_flow() && !matches!(c.kind, BoxKind::Text { .. })
                || c.is_blank_text()
        });
        if inline
            && b.children
                .iter()
                .any(|c| c.is_inline_level() && !c.is_blank_text())
        {
            let r = self.layout_inline(b, x, y, cb, floats);
            return ChildrenOut {
                frags: r.frags,
                height: r.height,
                pending: Collapse::default(),
                baseline: r.baseline,
                empty: r.height == 0.0,
            };
        }
        let mut frags = Vec::new();
        let mut cursor = y;
        let mut pending = Collapse::default();
        let mut first = true;
        let mut baseline = None;
        let mut any_content = false;
        for c in &b.children {
            if c.is_blank_text()
                || matches!(c.kind, BoxKind::Text { .. } | BoxKind::LineBreak) && c.is_blank_text()
            {
                continue;
            }
            match c.kind {
                BoxKind::Text { ref text, .. } if text.is_empty() => {
                    // Zero-width anchor.
                    let mut f = Fragment::new(
                        Rect::new(x, cursor, 0.0, 0.0),
                        c.style.clone(),
                        None,
                        FragKind::Text {
                            text: alloc::string::String::new(),
                            deco: 0,
                            baseline: cursor,
                        },
                    );
                    f.target = c.target;
                    frags.push(f);
                    continue;
                }
                _ => {}
            }
            let s = &c.style;
            if matches!(s.position, Position::Absolute | Position::Fixed) {
                let p = PendingAbs {
                    b: c,
                    static_x: x,
                    static_y: cursor + pending.resolve(),
                };
                if s.position == Position::Fixed {
                    self.fixed.push(p);
                } else if let Some(top) = self.abs_stack.last_mut() {
                    top.push(p);
                } else {
                    self.fixed.push(p);
                }
                continue;
            }
            if s.float != Float::None {
                let f = self.layout_float(c, x, cursor + pending.resolve(), cb, floats);
                frags.push(f);
                continue;
            }
            let chain = self.top_chain(c, cb.w);
            let mut top = if first && first_absorbed {
                Collapse::default()
            } else {
                chain
            };
            top.merge(pending);
            let mut cy = cursor + top.resolve();
            if s.clear != Clear::None {
                let cleared = floats.clear_y(s.clear, cy);
                if cleared > cy {
                    cy = cleared;
                }
            }
            let cy = self.m.snap_y(cy);
            let out = self.layout_block(c, cb, x, cy, floats, true);
            if out.empty {
                // Margins collapse through an empty box.
                pending = top;
                pending.merge(out.mb);
                frags.push(out.frag);
                continue;
            }
            any_content = true;
            first = false;
            cursor = out.frag.rect.y + out.frag.rect.h;
            if matches!(c.style.position, Position::Relative) {
                // Relative offsets do not move the flow.
                let (_, dy) = self.relative_offset(c, cb);
                cursor -= dy;
            }
            pending = out.mb;
            if out.baseline.is_some() {
                baseline = out.baseline;
            }
            frags.push(out.frag);
        }
        ChildrenOut {
            frags,
            height: cursor - y,
            pending,
            baseline,
            empty: !any_content,
        }
    }

    /// Place a float at `y` in the float context.
    pub fn layout_float(
        &mut self,
        c: &'a LayoutBox,
        x: f32,
        y: f32,
        cb: Cb,
        floats: &mut Floats,
    ) -> Fragment {
        let bm = self.box_model(c, cb.w);
        let w = self.shrink_to_fit(c, cb.w, &bm);
        // Lay out at the origin, then move into place.
        let mut tmp = Floats::default();
        let sub_cb = Cb {
            w: w + bm.margin.horizontal(),
            h: cb.h,
        };
        let mut out = self.layout_block_sized(c, sub_cb, 0.0, 0.0, w, &mut tmp);
        let mw = w + bm.margin.horizontal();
        let mh = out.frag.rect.h + bm.margin.vertical();
        let left = !matches!(c.style.float, Float::Right | Float::InlineEnd);
        let y = floats.clear_y(c.style.clear, y);
        let (fx, fy) = floats.place(mw, mh, left, y, x, x + cb.w);
        out.frag.translate(
            fx + bm.margin.left - out.frag.rect.x,
            fy + bm.margin.top - out.frag.rect.y,
        );
        out.frag
    }

    /// Lay out a block-level box with a given border-box width (floats,
    /// inline-blocks, absolutely positioned boxes, flex and grid items).
    /// `x`, `y`: margin-box top-left.
    pub fn layout_block_sized(
        &mut self,
        c: &'a LayoutBox,
        cb: Cb,
        x: f32,
        y: f32,
        w: f32,
        floats: &mut Floats,
    ) -> BlockOut {
        let bm = self.box_model(c, cb.w);
        let mt = if bm.auto[0] { 0.0 } else { bm.margin.top };
        // (layout_block_w adds the left margin itself)
        self.layout_block_w(c, cb, x, y + mt, floats, false, Some(w))
    }

    /// Shrink-to-fit border-box width (CSS 2 §10.3.5).
    pub fn shrink_to_fit(&self, b: &LayoutBox, avail: f32, bm: &BoxModel) -> f32 {
        if let Some(w) = self.spec_width(b, Some(avail), bm) {
            return self.clamp_w(b, w, avail, bm);
        }
        if let BoxKind::Replaced(_) = b.kind {
            let (w, _) = self.replaced_size(b, Cb { w: avail, h: None });
            return self.clamp_w(b, w + bm.frame_h(), avail, bm);
        }
        let (min, max) = self.intrinsic(b);
        let m = bm.margin.horizontal();
        let w = (max - m).min((avail - m).max(min - m)).max(0.0);
        self.clamp_w(b, w, avail, bm)
    }

    /// Content size of a replaced element (CSS 2 §10.3.2, §10.6.2).
    pub fn replaced_size(&self, b: &LayoutBox, cb: Cb) -> (f32, f32) {
        let BoxKind::Replaced(r) = &b.kind else {
            return (0.0, 0.0);
        };
        let (iw, ih) = self.m.replaced_size(r, self.fields, &b.style);
        let bm = self.box_model(b, cb.w);
        let sw = self
            .spec_width(b, Some(cb.w), &bm)
            .map(|w| w - bm.frame_h());
        let sh = self.spec_height(b, cb.h, &bm).map(|h| h - bm.frame_v());
        if self.m.cell_mode() {
            // Text renderings keep their natural size.
            return (iw.unwrap_or(0.0), ih.unwrap_or(0.0));
        }
        let ratio = match (iw, ih) {
            (Some(w), Some(h)) if h > 0.0 => Some(w / h),
            _ => b.style.aspect_ratio,
        };
        match (sw, sh) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, ratio.map_or(ih.unwrap_or(150.0), |r| w / r)),
            (None, Some(h)) => (ratio.map_or(iw.unwrap_or(300.0), |r| h * r), h),
            (None, None) => (
                iw.unwrap_or(300.0),
                ih.unwrap_or_else(|| ratio.map_or(150.0, |r| iw.unwrap_or(300.0) / r)),
            ),
        }
    }

    // -----------------------------------------------------------------
    // Positioning
    // -----------------------------------------------------------------

    /// Lay out the absolutely positioned boxes whose containing block is
    /// `b` (popping the list pushed for it) and add them to `frag`.
    fn finish_positioned(&mut self, b: &'a LayoutBox, frag: &mut Fragment, bm: &BoxModel) {
        let pending = self.abs_stack.pop().unwrap_or_default();
        let pad = Rect::new(
            frag.rect.x + bm.border.left,
            frag.rect.y + bm.border.top,
            frag.rect.w - bm.border.horizontal(),
            frag.rect.h - bm.border.vertical(),
        );
        let _ = b;
        for p in pending {
            let f = self.layout_abs(&p, pad);
            frag.children.push(f);
        }
    }

    /// Absolutely positioned box in containing block `cbr` (padding box).
    pub fn layout_abs(&mut self, p: &PendingAbs<'a>, cbr: Rect) -> Fragment {
        let c = p.b;
        let s = &c.style;
        let bm = self.box_model(c, cbr.w);
        let left = s.left.resolve(cbr.w);
        let right = s.right.resolve(cbr.w);
        let top = s.top.resolve(cbr.h);
        let bottom = s.bottom.resolve(cbr.h);
        let w = match (self.spec_width(c, Some(cbr.w), &bm), left, right) {
            (Some(w), _, _) => self.clamp_w(c, w, cbr.w, &bm),
            (None, Some(l), Some(r)) if !matches!(c.kind, BoxKind::Replaced(_)) => self.clamp_w(
                c,
                (cbr.w - l - r - bm.margin.horizontal()).max(0.0),
                cbr.w,
                &bm,
            ),
            _ => self.shrink_to_fit(
                c,
                (cbr.w - left.unwrap_or(0.0) - right.unwrap_or(0.0)).max(0.0),
                &bm,
            ),
        };
        let mut floats = Floats::default();
        let cb = Cb {
            w: cbr.w,
            h: Some(cbr.h),
        };
        let mut out = self.layout_block_sized(c, cb, 0.0, 0.0, w, &mut floats);
        let h = out.frag.rect.h;
        let x = match (left, right) {
            (Some(l), _) => cbr.x + l + bm.margin.left,
            (None, Some(r)) => cbr.right() - r - bm.margin.right - w,
            (None, None) => p.static_x + bm.margin.left,
        };
        let spec_h = self.spec_height(c, Some(cbr.h), &bm);
        let (y, h) = match (top, bottom) {
            (Some(t), Some(bt)) if spec_h.is_none() && !matches!(c.kind, BoxKind::Replaced(_)) => {
                let hh = (cbr.h - t - bt - bm.margin.vertical()).max(h);
                (cbr.y + t + bm.margin.top, hh)
            }
            (Some(t), _) => (cbr.y + t + bm.margin.top, h),
            (None, Some(bt)) => (cbr.bottom() - bt - bm.margin.bottom - h, h),
            (None, None) => (p.static_y + bm.margin.top, h),
        };
        out.frag.rect.h = h;
        let (x, y) = (self.m.snap_x(x), self.m.snap_y(y));
        out.frag.translate(x - out.frag.rect.x, y - out.frag.rect.y);
        out.frag
    }

    // -----------------------------------------------------------------
    // Intrinsic sizes
    // -----------------------------------------------------------------

    /// (min-content, max-content) outer width (margin box) of `b`.
    pub fn intrinsic(&self, b: &LayoutBox) -> (f32, f32) {
        let key = b as *const LayoutBox as usize;
        if let Some(v) = self.icache_get(key) {
            return v;
        }
        let v = self.intrinsic_uncached(b);
        self.icache_put(key, v);
        v
    }

    fn icache_get(&self, k: usize) -> Option<(f32, f32)> {
        self.icache.borrow().get(&k).copied()
    }

    fn icache_put(&self, k: usize, v: (f32, f32)) {
        self.icache.borrow_mut().insert(k, v);
    }

    fn intrinsic_uncached(&self, b: &LayoutBox) -> (f32, f32) {
        let bm = self.box_model(b, 0.0);
        let frame = bm.frame_h() + bm.margin.horizontal();
        if let Some(w) = match &b.style.width {
            Size::Lp(l) if !l.has_percent() => {
                Some(self.outer_from_spec(l.resolve(0.0), &b.style, &bm))
            }
            _ => None,
        } {
            let w = w + bm.margin.horizontal();
            return (w, w);
        }
        let (min, max) = match &b.kind {
            BoxKind::Text { .. }
            | BoxKind::Inline
            | BoxKind::LineBreak
            | BoxKind::Marker { .. } => self.inline_intrinsic(core::slice::from_ref(b)),
            BoxKind::Replaced(_) => {
                let (w, _) = self.replaced_size(b, Cb { w: 0.0, h: None });
                (w, w)
            }
            BoxKind::Table => self.table_intrinsic(b),
            BoxKind::Flex => {
                let row = matches!(
                    b.style.flex_direction,
                    css::style::FlexDirection::Row | css::style::FlexDirection::RowReverse
                );
                let items: Vec<(f32, f32)> = b
                    .children
                    .iter()
                    .filter(|c| !c.style.is_out_of_flow() || matches!(c.kind, BoxKind::Text { .. }))
                    .map(|c| self.intrinsic(c))
                    .collect();
                let gap = b.style.column_gap.as_ref().map_or(0.0, |g| g.resolve(0.0));
                if row {
                    let gaps = gap * items.len().saturating_sub(1) as f32;
                    let max = items.iter().map(|i| i.1).sum::<f32>() + gaps;
                    let min = if b.style.flex_wrap == css::style::FlexWrap::Nowrap {
                        items.iter().map(|i| i.0).sum::<f32>() + gaps
                    } else {
                        items.iter().map(|i| i.0).fold(0.0, f32::max)
                    };
                    (min, max)
                } else {
                    items
                        .iter()
                        .fold((0.0f32, 0.0f32), |a, i| (a.0.max(i.0), a.1.max(i.1)))
                }
            }
            BoxKind::Grid => self.grid_intrinsic(b),
            _ => {
                // Block container.
                let inline = b.children.iter().all(|c| {
                    c.is_inline_level()
                        || c.is_blank_text()
                        || c.style.is_out_of_flow() && !matches!(c.kind, BoxKind::Text { .. })
                });
                if inline {
                    let (mut mn, mut mx) = self.inline_intrinsic(&b.children);
                    // Floats among the inline content.
                    for c in b.children.iter().filter(|c| {
                        c.style.float != Float::None && !matches!(c.kind, BoxKind::Text { .. })
                    }) {
                        let (a, bb) = self.intrinsic(c);
                        mn = mn.max(a);
                        mx += bb;
                    }
                    (mn, mx)
                } else {
                    let mut mn = 0.0f32;
                    let mut mx = 0.0f32;
                    let mut float_run = 0.0f32;
                    for c in &b.children {
                        if c.is_blank_text()
                            || matches!(c.style.position, Position::Absolute | Position::Fixed)
                                && !matches!(c.kind, BoxKind::Text { .. })
                        {
                            continue;
                        }
                        let (a, bb) = self.intrinsic(c);
                        mn = mn.max(a);
                        if c.style.float != Float::None && !matches!(c.kind, BoxKind::Text { .. }) {
                            float_run += bb;
                            mx = mx.max(float_run);
                        } else {
                            float_run = 0.0;
                            mx = mx.max(bb);
                        }
                    }
                    (mn, mx)
                }
            }
        };
        let mut min = min + frame;
        let mut max = max + frame;
        if let Some(mw) = b.style.min_width.resolve(None) {
            let v = self.outer_from_spec(mw, &b.style, &bm) + bm.margin.horizontal();
            min = min.max(v);
            max = max.max(v);
        }
        if let Some(mw) = b.style.max_width.resolve(None) {
            let v = self.outer_from_spec(mw, &b.style, &bm) + bm.margin.horizontal();
            min = min.min(v);
            max = max.min(v);
        }
        (min, max.max(min))
    }

    /// Lay out the whole tree in the viewport. Returns the root fragment.
    pub fn layout_root(&mut self, root: &'a LayoutBox) -> Fragment {
        let (vw, vh) = self.viewport;
        let mut floats = Floats::default();
        self.abs_stack.push(Vec::new());
        let cb = Cb { w: vw, h: Some(vh) };
        let mut out = self.layout_block(root, cb, 0.0, 0.0, &mut floats, false);
        let mut pending = self.abs_stack.pop().unwrap_or_default();
        pending.append(&mut self.fixed);
        let icb = Rect::new(0.0, 0.0, vw, vh);
        for p in pending {
            let f = self.layout_abs(&p, icb);
            out.frag.children.push(f);
        }
        out.frag.rect.h = out.frag.rect.h.max(floats.bottom());
        out.frag
    }
}

pub struct ChildrenOut {
    pub frags: Vec<Fragment>,
    pub height: f32,
    pub pending: Collapse,
    pub baseline: Option<f32>,
    pub empty: bool,
}

impl Replaced {
    pub fn is_image(&self) -> bool {
        matches!(self.kind, ReplacedKind::Image { .. })
    }
}
