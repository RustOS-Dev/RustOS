//! Flex layout (CSS Flexible Box Layout §9): flex base sizes, line
//! breaking, resolving flexible lengths, cross sizes, `justify-content`,
//! `align-items`/`align-self`/`align-content`, `order`, reversal and
//! gaps.

use crate::flow::{Cb, Engine, Floats, PendingAbs};
use crate::fragment::Fragment;
use crate::tree::{BoxKind, LayoutBox};
use alloc::vec::Vec;
use css::style::{Align, FlexBasis, FlexDirection, FlexWrap, Position, Size};

struct Item<'a> {
    b: &'a LayoutBox,
    base: f32,
    hypo: f32,
    min: f32,
    max: f32,
    /// Outer main-axis margins + frame (border+padding) for row: used to
    /// convert between border-box and margin-box sizes.
    margin_main: f32,
    target: f32,
    frozen: bool,
    cross: f32,
    margin_cross: f32,
    frag: Option<Fragment>,
    auto_margins_main: (bool, bool),
}

impl<'a> Engine<'a> {
    /// Lay out a flex container's items inside its content box at
    /// (x, y). Returns the fragments and the content height.
    pub fn layout_flex(
        &mut self,
        b: &'a LayoutBox,
        x: f32,
        y: f32,
        cb: Cb,
    ) -> (Vec<Fragment>, f32) {
        let s = &b.style;
        let row = matches!(
            s.flex_direction,
            FlexDirection::Row | FlexDirection::RowReverse
        );
        let reverse = matches!(
            s.flex_direction,
            FlexDirection::RowReverse | FlexDirection::ColumnReverse
        );
        let wrap = s.flex_wrap != FlexWrap::Nowrap;
        let main_gap = if row {
            s.column_gap.as_ref()
        } else {
            s.row_gap.as_ref()
        }
        .map_or(0.0, |g| g.resolve(cb.w));
        let cross_gap = if row {
            s.row_gap.as_ref()
        } else {
            s.column_gap.as_ref()
        }
        .map_or(0.0, |g| g.resolve(cb.w));
        let main_gap = if self.m.cell_mode() && !row {
            self.m.snap_y(main_gap)
        } else {
            main_gap
        };
        let avail_main = if row { Some(cb.w) } else { cb.h };
        let mut frags = Vec::new();
        // Items in `order`.
        let mut kids: Vec<&'a LayoutBox> = Vec::new();
        for c in &b.children {
            if c.is_blank_text() {
                continue;
            }
            if matches!(c.style.position, Position::Absolute | Position::Fixed)
                && !matches!(c.kind, BoxKind::Text { .. })
            {
                let p = PendingAbs {
                    b: c,
                    static_x: x,
                    static_y: y,
                };
                if let Some(top) = self.abs_stack.last_mut() {
                    top.push(p);
                } else {
                    self.fixed.push(p);
                }
                continue;
            }
            kids.push(c);
        }
        kids.sort_by_key(|c| c.style.order);
        let mut items: Vec<Item<'a>> = Vec::new();
        for c in kids {
            let bm = self.box_model(c, cb.w);
            let (margin_main, margin_cross) = if row {
                (bm.margin.horizontal(), bm.margin.vertical())
            } else {
                (bm.margin.vertical(), bm.margin.horizontal())
            };
            let auto_margins_main = if row {
                (bm.auto[3], bm.auto[1])
            } else {
                (bm.auto[0], bm.auto[2])
            };
            // Flex base size (border box).
            let spec_main = if row {
                self.spec_width(c, Some(cb.w), &bm)
            } else {
                self.spec_height(c, cb.h, &bm)
            };
            let base = match &c.style.flex_basis {
                FlexBasis::Lp(l) if !l.has_percent() || avail_main.is_some() => {
                    let v = l.resolve_opt(avail_main);
                    match c.style.box_sizing {
                        css::style::BoxSizing::ContentBox => {
                            v + if row { bm.frame_h() } else { bm.frame_v() }
                        }
                        css::style::BoxSizing::BorderBox => v,
                    }
                }
                FlexBasis::Auto if spec_main.is_some() => spec_main.unwrap(),
                _ => {
                    if row {
                        let (_, max) = self.intrinsic(c);
                        (max - bm.margin.horizontal()).max(0.0)
                    } else {
                        let w = self.item_cross_size_for_column(c, cb.w, &bm);
                        let mut fl = Floats::default();
                        let out = self.layout_block_sized(
                            c,
                            Cb { w: cb.w, h: None },
                            0.0,
                            0.0,
                            w,
                            &mut fl,
                        );
                        out.frag.rect.h
                    }
                }
            };
            // min-width:auto for items is the min-content size (capped by
            // a specified size).
            let (min, max) = if row {
                let auto_min = if matches!(c.style.min_width, Size::Auto)
                    && c.style.overflow_x == css::style::Overflow::Visible
                {
                    let m = (self.intrinsic(c).0 - bm.margin.horizontal()).max(0.0);
                    spec_main.map_or(m, |s| m.min(s))
                } else {
                    0.0
                };
                let mn = c.style.min_width.resolve(Some(cb.w)).map_or(auto_min, |v| {
                    v + if c.style.box_sizing == css::style::BoxSizing::ContentBox {
                        bm.frame_h()
                    } else {
                        0.0
                    }
                });
                let mx = c
                    .style
                    .max_width
                    .resolve(Some(cb.w))
                    .map_or(f32::INFINITY, |v| {
                        v + if c.style.box_sizing == css::style::BoxSizing::ContentBox {
                            bm.frame_h()
                        } else {
                            0.0
                        }
                    });
                (mn.max(bm.frame_h()), mx)
            } else {
                let mn = c.style.min_height.resolve(cb.h).unwrap_or(0.0);
                let mx = c.style.max_height.resolve(cb.h).unwrap_or(f32::INFINITY);
                (mn.max(bm.frame_v()), mx)
            };
            let hypo = base.clamp(min, max.max(min));
            items.push(Item {
                b: c,
                base,
                hypo,
                min,
                max: max.max(min),
                margin_main,
                target: hypo,
                frozen: false,
                cross: 0.0,
                margin_cross,
                frag: None,
                auto_margins_main,
            });
        }
        // Lines.
        let container_main = avail_main.unwrap_or(f32::INFINITY);
        let mut lines: Vec<core::ops::Range<usize>> = Vec::new();
        let mut start = 0;
        let mut used = 0.0;
        for i in 0..items.len() {
            let outer = items[i].hypo + items[i].margin_main;
            if wrap && i > start && used + main_gap + outer > container_main + 0.01 {
                lines.push(start..i);
                start = i;
                used = 0.0;
            }
            used += outer + if i > start { main_gap } else { 0.0 };
        }
        if start < items.len() || lines.is_empty() {
            lines.push(start..items.len());
        }
        // Resolve flexible lengths per line (§9.7).
        let mut main_sizes_known = avail_main.is_some();
        if !row && avail_main.is_none() {
            main_sizes_known = false;
        }
        for r in &lines {
            let line_items = &mut items[r.clone()];
            if !main_sizes_known {
                for it in line_items.iter_mut() {
                    it.target = it.hypo;
                }
                continue;
            }
            let gaps = main_gap * line_items.len().saturating_sub(1) as f32;
            let sum_hypo: f32 = line_items
                .iter()
                .map(|i| i.hypo + i.margin_main)
                .sum::<f32>()
                + gaps;
            let grow = sum_hypo < container_main;
            for it in line_items.iter_mut() {
                let f = if grow {
                    it.b.style.flex_grow
                } else {
                    it.b.style.flex_shrink
                };
                it.frozen = f == 0.0 || (grow && it.base > it.hypo) || (!grow && it.base < it.hypo);
                it.target = if it.frozen { it.hypo } else { it.base };
            }
            for _ in 0..line_items.len() + 1 {
                if line_items.iter().all(|i| i.frozen) {
                    break;
                }
                let used: f32 = line_items
                    .iter()
                    .map(|i| if i.frozen { i.target } else { i.base } + i.margin_main)
                    .sum::<f32>()
                    + gaps;
                let free = container_main - used;
                let mut total_violation = 0.0;
                if grow {
                    let sum_f: f32 = line_items
                        .iter()
                        .filter(|i| !i.frozen)
                        .map(|i| i.b.style.flex_grow)
                        .sum();
                    let factor = if sum_f < 1.0 { sum_f } else { 1.0 };
                    let dist = free * factor;
                    for it in line_items.iter_mut().filter(|i| !i.frozen) {
                        let t =
                            it.base + dist * it.b.style.flex_grow / sum_f.max(f32::MIN_POSITIVE);
                        let c = t.clamp(it.min, it.max);
                        total_violation += c - t;
                        it.target = c;
                    }
                } else {
                    let sum_scaled: f32 = line_items
                        .iter()
                        .filter(|i| !i.frozen)
                        .map(|i| i.b.style.flex_shrink * i.base)
                        .sum();
                    for it in line_items.iter_mut().filter(|i| !i.frozen) {
                        let ratio = if sum_scaled > 0.0 {
                            it.b.style.flex_shrink * it.base / sum_scaled
                        } else {
                            0.0
                        };
                        let t = it.base + free * ratio;
                        let c = t.clamp(it.min, it.max);
                        total_violation += c - t;
                        it.target = c;
                    }
                }
                // Freeze violators.
                for it in line_items.iter_mut().filter(|i| !i.frozen) {
                    let min_v = it.target <= it.min + 0.001 && total_violation > 0.0;
                    let max_v = it.target >= it.max - 0.001 && total_violation < 0.0;
                    if total_violation == 0.0 || min_v || max_v {
                        it.frozen = true;
                    }
                }
            }
            for it in line_items.iter_mut() {
                if self.m.cell_mode() && row {
                    it.target = self.m.snap_x(it.target);
                }
            }
        }
        // Lay out items at their main size to find cross sizes.
        for it in items.iter_mut() {
            let bm = self.box_model(it.b, cb.w);
            let mut fl = Floats::default();
            let out = if row {
                self.layout_block_sized(it.b, Cb { w: cb.w, h: None }, 0.0, 0.0, it.target, &mut fl)
            } else {
                let w = self.item_cross_size_for_column(it.b, cb.w, &bm);
                let mut o =
                    self.layout_block_sized(it.b, Cb { w: cb.w, h: None }, 0.0, 0.0, w, &mut fl);
                if main_sizes_known || it.b.style.flex_grow > 0.0 || o.frag.rect.h != it.target {
                    o.frag.rect.h = it.target.max(o.frag.rect.h.min(it.target.max(it.hypo)));
                    o.frag.rect.h = it.target;
                }
                o
            };
            it.cross = if row {
                out.frag.rect.h
            } else {
                out.frag.rect.w
            };
            it.frag = Some(out.frag);
        }
        // Line cross sizes.
        let mut line_cross: Vec<f32> = lines
            .iter()
            .map(|r| {
                items[r.clone()]
                    .iter()
                    .map(|i| i.cross + i.margin_cross)
                    .fold(0.0, f32::max)
            })
            .collect();
        let definite_cross = if row { cb.h } else { Some(cb.w) };
        if !wrap
            && let Some(c) = definite_cross
            && let Some(l) = line_cross.first_mut()
        {
            *l = c;
        }
        let total_cross: f32 =
            line_cross.iter().sum::<f32>() + cross_gap * line_cross.len().saturating_sub(1) as f32;
        // align-content (stretch by default for multi-line).
        let mut line_offsets = Vec::with_capacity(lines.len());
        let free_cross = definite_cross.map_or(0.0, |c| (c - total_cross).max(0.0));
        let (mut lo, mut between) = (0.0, cross_gap);
        if wrap && free_cross > 0.0 {
            let n = lines.len() as f32;
            match s.align_content {
                Align::Center => lo = free_cross / 2.0,
                Align::End | Align::FlexEnd => lo = free_cross,
                Align::SpaceBetween if n > 1.0 => between += free_cross / (n - 1.0),
                Align::SpaceAround => {
                    lo = free_cross / n / 2.0;
                    between += free_cross / n;
                }
                Align::SpaceEvenly => {
                    lo = free_cross / (n + 1.0);
                    between += free_cross / (n + 1.0);
                }
                Align::Normal | Align::Stretch => {
                    let add = free_cross / n;
                    for l in line_cross.iter_mut() {
                        *l += add;
                    }
                }
                _ => {}
            }
        }
        for l in &line_cross {
            line_offsets.push(lo);
            lo += l + between;
        }
        // Position items.
        for (li, r) in lines.iter().enumerate() {
            let lc = line_cross[li];
            let n = r.len();
            let gaps = main_gap * n.saturating_sub(1) as f32;
            let used: f32 = items[r.clone()]
                .iter()
                .map(|i| i.target + i.margin_main)
                .sum::<f32>()
                + gaps;
            let free = if main_sizes_known {
                (container_main - used).max(0.0)
            } else {
                0.0
            };
            let auto_count: usize = items[r.clone()]
                .iter()
                .map(|i| usize::from(i.auto_margins_main.0) + usize::from(i.auto_margins_main.1))
                .sum();
            let (mut pos, mut extra) = (0.0, 0.0);
            let auto_share = if auto_count > 0 {
                free / auto_count as f32
            } else {
                0.0
            };
            if auto_count == 0 {
                match s.justify_content {
                    Align::Center => pos = free / 2.0,
                    Align::End | Align::FlexEnd | Align::Right => pos = free,
                    Align::SpaceBetween if n > 1 => extra = free / (n - 1) as f32,
                    Align::SpaceAround if n > 0 => {
                        extra = free / n as f32;
                        pos = extra / 2.0;
                    }
                    Align::SpaceEvenly => {
                        extra = free / (n + 1) as f32;
                        pos = extra;
                    }
                    _ => {}
                }
                if reverse
                    && matches!(
                        s.justify_content,
                        Align::Normal | Align::FlexStart | Align::Start
                    )
                {
                    pos = free; // flex-start is the end in reverse directions
                }
            }
            let order: Vec<usize> = if reverse {
                r.clone().rev().collect()
            } else {
                r.clone().collect()
            };
            for &i in &order {
                let it = &mut items[i];
                let bm = self.box_model(it.b, cb.w);
                if it.auto_margins_main.0 {
                    pos += auto_share;
                }
                let lead = if row {
                    if bm.auto[3] { 0.0 } else { bm.margin.left }
                } else if bm.auto[0] {
                    0.0
                } else {
                    bm.margin.top
                };
                let main_pos = pos + lead;
                pos += it.target + it.margin_main + main_gap + extra;
                if it.auto_margins_main.1 {
                    pos += auto_share;
                }
                // Cross alignment.
                let align = if matches!(it.b.style.align_self, Align::Auto) {
                    s.align_items
                } else {
                    it.b.style.align_self
                };
                let cross_auto = if row {
                    matches!(it.b.style.height, Size::Auto)
                } else {
                    matches!(it.b.style.width, Size::Auto)
                };
                let cross_lead = if row {
                    if bm.auto[0] { 0.0 } else { bm.margin.top }
                } else if bm.auto[3] {
                    0.0
                } else {
                    bm.margin.left
                };
                let free_c = (lc - it.cross - it.margin_cross).max(0.0);
                let mut f = it.frag.take().unwrap();
                let cross_off = match align {
                    Align::Center => free_c / 2.0,
                    Align::End | Align::FlexEnd | Align::SelfEnd => free_c,
                    Align::Normal | Align::Stretch if cross_auto => {
                        // Stretch: relayout at the line's cross size.
                        let target = lc - it.margin_cross;
                        if row && (target - f.rect.h).abs() > 0.01 {
                            let mut fl = Floats::default();
                            let mut out = self.layout_block_sized(
                                it.b,
                                Cb {
                                    w: cb.w,
                                    h: Some(target),
                                },
                                0.0,
                                0.0,
                                it.target,
                                &mut fl,
                            );
                            out.frag.rect.h = target.max(out.frag.rect.h.min(target)).max(target);
                            f = out.frag;
                        } else if !row && (target - f.rect.w).abs() > 0.01 {
                            let mut fl = Floats::default();
                            let mut out = self.layout_block_sized(
                                it.b,
                                Cb { w: target, h: None },
                                0.0,
                                0.0,
                                target,
                                &mut fl,
                            );
                            out.frag.rect.h = it.target;
                            f = out.frag;
                        }
                        0.0
                    }
                    _ => 0.0,
                };
                let (fx, fy) = if row {
                    (x + main_pos, y + line_offsets[li] + cross_lead + cross_off)
                } else {
                    (x + line_offsets[li] + cross_lead + cross_off, y + main_pos)
                };
                let (fx, fy) = (self.m.snap_x(fx), self.m.snap_y(fy));
                f.translate(fx - f.rect.x, fy - f.rect.y);
                if it.b.style.position == Position::Relative {
                    let (dx, dy) = self.relative_offset(it.b, cb);
                    f.translate(dx, dy);
                }
                frags.push(f);
            }
        }
        let content_h = if row {
            total_cross
        } else {
            // Column: the longest line (items stacked).
            lines
                .iter()
                .map(|r| {
                    items[r.clone()]
                        .iter()
                        .map(|i| i.target + i.margin_main)
                        .sum::<f32>()
                        + main_gap * r.len().saturating_sub(1) as f32
                })
                .fold(0.0, f32::max)
        };
        (frags, content_h)
    }

    /// Cross size (width) of an item in a column flex container.
    fn item_cross_size_for_column(
        &self,
        c: &LayoutBox,
        cb_w: f32,
        bm: &crate::flow::BoxModel,
    ) -> f32 {
        match self.spec_width(c, Some(cb_w), bm) {
            Some(w) => self.clamp_w(c, w, cb_w, bm),
            None => {
                let align = c.style.align_self;
                if matches!(align, Align::Auto | Align::Normal | Align::Stretch) {
                    self.clamp_w(c, (cb_w - bm.margin.horizontal()).max(0.0), cb_w, bm)
                } else {
                    self.shrink_to_fit(c, cb_w, bm)
                }
            }
        }
    }
}
