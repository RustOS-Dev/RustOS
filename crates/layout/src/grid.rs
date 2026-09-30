//! Grid layout (CSS Grid Layout 1, simplified track sizing): explicit
//! tracks with `repeat()` (including `auto-fill`/`auto-fit`), named lines
//! and areas, auto-placement (row/column, dense), implicit tracks, fixed,
//! percentage, `fr`, `auto`, `min-content`/`max-content`, `minmax()` and
//! `fit-content()` tracks, gaps and alignment.

use crate::flow::{Cb, Engine, Floats, PendingAbs};
use crate::fragment::Fragment;
use crate::tree::{BoxKind, LayoutBox};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use css::style::{
    Align, GridLine, GridTemplate, Position, RepeatCount, Size, TrackItem, TrackSize,
};

struct Tracks {
    sizes: Vec<TrackSize>,
    /// Names of line i (0..=n).
    names: Vec<Vec<String>>,
}

fn fixed_size(t: &TrackSize, basis: Option<f32>) -> Option<f32> {
    match t {
        TrackSize::Lp(l) if !l.has_percent() || basis.is_some() => Some(l.resolve_opt(basis)),
        TrackSize::MinMax(a, _) => fixed_size(a, basis),
        _ => None,
    }
}

fn expand(items: &[TrackItem], avail: Option<f32>, gap: f32, out: &mut Tracks) {
    for it in items {
        match it {
            TrackItem::Names(n) => out.names.last_mut().unwrap().extend(n.iter().cloned()),
            TrackItem::Size(s) => {
                out.sizes.push(s.clone());
                out.names.push(Vec::new());
            }
            TrackItem::Repeat(count, inner) => {
                let n = match count {
                    RepeatCount::Count(n) => *n as usize,
                    RepeatCount::AutoFill | RepeatCount::AutoFit => {
                        // As many as fit (at least one).
                        let per: f32 = inner
                            .iter()
                            .filter_map(|t| {
                                if let TrackItem::Size(s) = t {
                                    Some(fixed_size(s, avail).unwrap_or(0.0))
                                } else {
                                    None
                                }
                            })
                            .sum();
                        let k = inner
                            .iter()
                            .filter(|t| matches!(t, TrackItem::Size(_)))
                            .count()
                            .max(1) as f32;
                        match avail {
                            Some(a) if per > 0.0 => {
                                (css::math::floorf((a + gap) / (per + gap * k)) as usize).max(1)
                            }
                            _ => 1,
                        }
                    }
                };
                for _ in 0..n.min(1000) {
                    expand(inner, avail, gap, out);
                }
            }
        }
    }
}

fn explicit(t: &GridTemplate, avail: Option<f32>, gap: f32) -> Tracks {
    let mut out = Tracks {
        sizes: Vec::new(),
        names: vec![Vec::new()],
    };
    if let GridTemplate::Tracks(items) = t {
        expand(items, avail, gap, &mut out);
    }
    out
}

struct Placed<'a> {
    b: &'a LayoutBox,
    row: usize,
    col: usize,
    rspan: usize,
    cspan: usize,
}

/// Resolve a start/end pair on one axis to (start index, span); None
/// start means auto-placed.
fn resolve_lines(
    start: &GridLine,
    end: &GridLine,
    names: &[Vec<String>],
    areas: &[(String, usize, usize)],
    n_explicit: usize,
) -> (Option<i64>, usize) {
    let find_named = |name: &str, nth: i32, is_end: bool| -> Option<i64> {
        // Area names imply `name-start` / `name-end` lines.
        if let Some((_, s, e)) = areas.iter().find(|(a, _, _)| a == name) {
            return Some(if is_end { *e as i64 } else { *s as i64 });
        }
        let suffix = if is_end { "-end" } else { "-start" };
        let matches: Vec<usize> = names
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                n.iter()
                    .any(|x| x == name || x.strip_suffix(suffix) == Some(name))
            })
            .map(|(i, _)| i)
            .collect();
        if matches.is_empty() {
            return None;
        }
        let k = if nth > 0 {
            (nth - 1) as usize
        } else {
            matches.len().saturating_sub((-nth) as usize)
        };
        matches.get(k.min(matches.len() - 1)).map(|&i| i as i64)
    };
    let line = |l: &GridLine, is_end: bool| -> Option<i64> {
        match l {
            GridLine::Line(n, None) => Some(if *n > 0 {
                *n as i64 - 1
            } else {
                n_explicit as i64 + 1 + *n as i64
            }),
            GridLine::Line(n, Some(name)) => find_named(name, *n, is_end),
            GridLine::Named(name) => find_named(name, 1, is_end),
            _ => None,
        }
    };
    let s = line(start, false);
    let e = line(end, true);
    let span_of = |l: &GridLine| {
        if let GridLine::Span(n, _) = l {
            Some(*n as usize)
        } else {
            None
        }
    };
    match (s, e) {
        (Some(s), Some(e)) => {
            let (a, b) = if e > s {
                (s, e)
            } else if e < s {
                (e, s)
            } else {
                (s, s + 1)
            };
            (Some(a), (b - a) as usize)
        }
        (Some(s), None) => (Some(s), span_of(end).unwrap_or(1)),
        (None, Some(e)) => {
            let span = span_of(start).unwrap_or(1) as i64;
            (Some(e - span), span as usize)
        }
        (None, None) => (None, span_of(start).or(span_of(end)).unwrap_or(1)),
    }
}

impl<'a> Engine<'a> {
    pub fn grid_intrinsic(&self, b: &LayoutBox) -> (f32, f32) {
        let s = &b.style;
        let cols = explicit(&s.grid_template_columns, None, 0.0);
        let gap = s.column_gap.as_ref().map_or(0.0, |g| g.resolve(0.0));
        let items: Vec<(f32, f32)> = b
            .children
            .iter()
            .filter(|c| !c.is_blank_text())
            .map(|c| self.intrinsic(c))
            .collect();
        if cols.sizes.is_empty() {
            return items
                .iter()
                .fold((0.0, 0.0), |a, i| (a.0.max(i.0), a.1.max(i.1)));
        }
        let n = cols.sizes.len();
        let mut min = gap * (n - 1) as f32;
        let mut max = min;
        let widest = items
            .iter()
            .fold((0.0f32, 0.0f32), |a, i| (a.0.max(i.0), a.1.max(i.1)));
        for t in &cols.sizes {
            match fixed_size(t, None) {
                Some(v) => {
                    min += v;
                    max += v;
                }
                None => {
                    min += widest.0;
                    max += widest.1;
                }
            }
        }
        (min, max)
    }

    pub fn layout_grid(
        &mut self,
        b: &'a LayoutBox,
        x: f32,
        y: f32,
        cb: Cb,
    ) -> (Vec<Fragment>, f32) {
        let s = &b.style;
        let col_gap = s.column_gap.as_ref().map_or(0.0, |g| g.resolve(cb.w));
        let col_gap = if self.m.cell_mode() && col_gap > 0.0 {
            self.m.snap_x(col_gap).max(8.0)
        } else {
            col_gap
        };
        let row_gap = s
            .row_gap
            .as_ref()
            .map_or(0.0, |g| g.resolve(cb.h.unwrap_or(0.0)));
        let row_gap = if self.m.cell_mode() {
            self.m.snap_y(row_gap)
        } else {
            row_gap
        };
        let mut cols = explicit(&s.grid_template_columns, Some(cb.w), col_gap);
        let mut rows = explicit(&s.grid_template_rows, cb.h, row_gap);
        // Areas.
        let mut areas: Vec<(String, (usize, usize), (usize, usize))> = Vec::new();
        for (r, line) in s.grid_template_areas.iter().enumerate() {
            for (c, name) in line.iter().enumerate() {
                if name == "." {
                    continue;
                }
                match areas.iter_mut().find(|(n, _, _)| n == name) {
                    Some((_, rr, cc)) => {
                        rr.1 = rr.1.max(r + 1);
                        cc.1 = cc.1.max(c + 1);
                    }
                    None => areas.push((name.clone(), (r, r + 1), (c, c + 1))),
                }
            }
        }
        let n_area_rows = s.grid_template_areas.len();
        let n_area_cols = s.grid_template_areas.first().map_or(0, Vec::len);
        while rows.sizes.len() < n_area_rows {
            rows.sizes
                .push(s.grid_auto_rows.first().cloned().unwrap_or(TrackSize::Auto));
            rows.names.push(Vec::new());
        }
        while cols.sizes.len() < n_area_cols {
            cols.sizes.push(
                s.grid_auto_columns
                    .first()
                    .cloned()
                    .unwrap_or(TrackSize::Auto),
            );
            cols.names.push(Vec::new());
        }
        let row_areas: Vec<(String, usize, usize)> = areas
            .iter()
            .map(|(n, r, _)| (n.clone(), r.0, r.1))
            .collect();
        let col_areas: Vec<(String, usize, usize)> = areas
            .iter()
            .map(|(n, _, c)| (n.clone(), c.0, c.1))
            .collect();
        // Items.
        let mut kids: Vec<&'a LayoutBox> = Vec::new();
        let mut frags = Vec::new();
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
        let n_cols_explicit = cols.sizes.len().max(1);
        let n_rows_explicit = rows.sizes.len();
        let mut placed: Vec<Placed<'a>> = Vec::new();
        let mut auto_items: Vec<(&'a LayoutBox, Option<i64>, usize, Option<i64>, usize)> =
            Vec::new();
        for c in &kids {
            let st = &c.style;
            let (r, rs) = resolve_lines(
                &st.grid_row_start,
                &st.grid_row_end,
                &rows.names,
                &row_areas,
                n_rows_explicit,
            );
            let (cc, cs) = resolve_lines(
                &st.grid_column_start,
                &st.grid_column_end,
                &cols.names,
                &col_areas,
                n_cols_explicit,
            );
            auto_items.push((c, r, rs, cc, cs));
        }
        // Occupancy grid (grows).
        let mut ncols = n_cols_explicit;
        for (_, _, _, c, cs) in &auto_items {
            if let Some(c) = c {
                ncols = ncols.max((*c).max(0) as usize + cs);
            } else {
                ncols = ncols.max(*cs);
            }
        }
        let mut occ: Vec<Vec<bool>> = Vec::new();
        let ensure = |occ: &mut Vec<Vec<bool>>, rows: usize, ncols: usize| {
            while occ.len() < rows {
                occ.push(vec![false; ncols]);
            }
        };
        let fits = |occ: &Vec<Vec<bool>>,
                    r: usize,
                    c: usize,
                    rs: usize,
                    cs: usize,
                    ncols: usize|
         -> bool {
            if c + cs > ncols {
                return false;
            }
            for rr in r..r + rs {
                if let Some(row) = occ.get(rr) {
                    for cc in c..c + cs {
                        if row[cc] {
                            return false;
                        }
                    }
                }
            }
            true
        };
        let mark =
            |occ: &mut Vec<Vec<bool>>, r: usize, c: usize, rs: usize, cs: usize, ncols: usize| {
                ensure(occ, r + rs, ncols);
                for row in occ.iter_mut().take(r + rs).skip(r) {
                    for cell in row.iter_mut().take(c + cs).skip(c) {
                        *cell = true;
                    }
                }
            };
        let column_flow = s.grid_auto_flow.column;
        // 1. Items with both positions definite; 2. with a definite row
        // (or column when flowing by column); 3. fully automatic.
        for pass in 0..3 {
            let mut cursor = (0usize, 0usize);
            for (c, r, rs, cc, cs) in auto_items.iter() {
                let definite = match pass {
                    0 => r.is_some() && cc.is_some(),
                    1 => {
                        (r.is_some() && cc.is_none() && !column_flow)
                            || (cc.is_some() && r.is_none() && column_flow)
                    }
                    _ => {
                        (r.is_none() && cc.is_none())
                            || (r.is_some() && cc.is_none() && column_flow)
                            || (cc.is_some() && r.is_none() && !column_flow)
                    }
                };
                if !definite {
                    continue;
                }
                let (rs, cs) = (*rs, (*cs).min(ncols.max(1)));
                let (pr, pc) = match (r, cc) {
                    (Some(r), Some(c)) => ((*r).max(0) as usize, (*c).max(0) as usize),
                    (Some(r), None) => {
                        let r = (*r).max(0) as usize;
                        let mut c = 0;
                        while !fits(&occ, r, c, rs, cs, ncols) && c + cs <= ncols {
                            c += 1;
                        }
                        (r, c.min(ncols.saturating_sub(cs)))
                    }
                    (None, Some(c)) => {
                        let c = (*c).max(0) as usize;
                        let mut r = 0;
                        while !fits(&occ, r, c, rs, cs, ncols.max(c + cs)) {
                            r += 1;
                        }
                        (r, c)
                    }
                    (None, None) => {
                        let (mut r, mut c) = if s.grid_auto_flow.dense {
                            (0, 0)
                        } else {
                            cursor
                        };
                        if column_flow {
                            let nrows = n_rows_explicit.max(1);
                            loop {
                                if r + rs <= nrows.max(rs)
                                    && fits(&occ, r, c, rs, cs, ncols.max(c + cs))
                                {
                                    break;
                                }
                                r += 1;
                                if r + rs > nrows.max(rs) {
                                    r = 0;
                                    c += 1;
                                }
                            }
                            ncols = ncols.max(c + cs);
                        } else {
                            loop {
                                if fits(&occ, r, c, rs, cs, ncols) {
                                    break;
                                }
                                c += 1;
                                if c + cs > ncols {
                                    c = 0;
                                    r += 1;
                                }
                            }
                        }
                        cursor = if column_flow { (r, c) } else { (r, c + cs) };
                        (r, c)
                    }
                };
                ncols = ncols.max(pc + cs);
                for row in occ.iter_mut() {
                    row.resize(ncols, false);
                }
                mark(&mut occ, pr, pc, rs, cs, ncols);
                placed.push(Placed {
                    b: c,
                    row: pr,
                    col: pc,
                    rspan: rs,
                    cspan: cs,
                });
            }
        }
        let nrows = occ
            .len()
            .max(n_rows_explicit)
            .max(placed.iter().map(|p| p.row + p.rspan).max().unwrap_or(0));
        let auto_col = |i: usize| {
            s.grid_auto_columns
                .get(i % s.grid_auto_columns.len().max(1))
                .cloned()
                .unwrap_or(TrackSize::Auto)
        };
        let auto_row = |i: usize| {
            s.grid_auto_rows
                .get(i % s.grid_auto_rows.len().max(1))
                .cloned()
                .unwrap_or(TrackSize::Auto)
        };
        while cols.sizes.len() < ncols {
            let i = cols.sizes.len() - n_cols_explicit.min(cols.sizes.len());
            cols.sizes.push(auto_col(i));
        }
        while rows.sizes.len() < nrows {
            let i = rows.sizes.len() - n_rows_explicit.min(rows.sizes.len());
            rows.sizes.push(auto_row(i));
        }
        // Column sizing.
        let col_w = self.size_tracks(&cols.sizes, Some(cb.w), col_gap, &placed, true, &[]);
        // Row sizing: lay out items at their column widths first.
        let mut item_frags: Vec<Fragment> = Vec::new();
        let mut item_heights: Vec<f32> = Vec::new();
        for p in &placed {
            let w: f32 =
                col_w[p.col..p.col + p.cspan].iter().sum::<f32>() + col_gap * (p.cspan - 1) as f32;
            let bm = self.box_model(p.b, w);
            let justify = if matches!(p.b.style.justify_self, Align::Auto) {
                s.justify_items
            } else {
                p.b.style.justify_self
            };
            let iw = match self.spec_width(p.b, Some(w), &bm) {
                Some(v) => self.clamp_w(p.b, v, w, &bm),
                None if matches!(justify, Align::Normal | Align::Stretch | Align::Auto) => {
                    self.clamp_w(p.b, (w - bm.margin.horizontal()).max(0.0), w, &bm)
                }
                None => self.shrink_to_fit(p.b, w, &bm),
            };
            let mut fl = Floats::default();
            let out = self.layout_block_sized(p.b, Cb { w, h: None }, 0.0, 0.0, iw, &mut fl);
            item_heights.push(out.frag.rect.h + bm.margin.vertical());
            item_frags.push(out.frag);
        }
        let row_h = self.size_tracks(&rows.sizes, cb.h, row_gap, &placed, false, &item_heights);
        let col_x: Vec<f32> = offsets(&col_w, col_gap, x, s.justify_content, cb.w);
        let total_h: f32 =
            row_h.iter().sum::<f32>() + row_gap * row_h.len().saturating_sub(1) as f32;
        let row_y: Vec<f32> = offsets(&row_h, row_gap, y, s.align_content, cb.h.unwrap_or(total_h));
        for (i, p) in placed.iter().enumerate() {
            let mut f = item_frags[i].clone();
            let area_w: f32 =
                col_w[p.col..p.col + p.cspan].iter().sum::<f32>() + col_gap * (p.cspan - 1) as f32;
            let area_h: f32 =
                row_h[p.row..p.row + p.rspan].iter().sum::<f32>() + row_gap * (p.rspan - 1) as f32;
            let bm = self.box_model(p.b, area_w);
            let justify = if matches!(p.b.style.justify_self, Align::Auto) {
                s.justify_items
            } else {
                p.b.style.justify_self
            };
            let align = if matches!(p.b.style.align_self, Align::Auto) {
                s.align_items
            } else {
                p.b.style.align_self
            };
            let fw = f.rect.w + bm.margin.horizontal();
            let dx = match justify {
                Align::Center => (area_w - fw) / 2.0,
                Align::End | Align::FlexEnd | Align::Right | Align::SelfEnd => area_w - fw,
                _ => 0.0,
            };
            // Stretch the height when auto.
            if matches!(align, Align::Normal | Align::Stretch | Align::Auto)
                && matches!(p.b.style.height, Size::Auto)
            {
                let target = area_h - bm.margin.vertical();
                if target > f.rect.h + 0.01 {
                    f.rect.h = target;
                }
            }
            let fh = f.rect.h + bm.margin.vertical();
            let dy = match align {
                Align::Center => (area_h - fh) / 2.0,
                Align::End | Align::FlexEnd | Align::SelfEnd => area_h - fh,
                _ => 0.0,
            };
            let nx = self
                .m
                .snap_x(col_x[p.col] + dx.max(0.0) + if bm.auto[3] { 0.0 } else { bm.margin.left });
            let ny = self
                .m
                .snap_y(row_y[p.row] + dy.max(0.0) + if bm.auto[0] { 0.0 } else { bm.margin.top });
            f.translate(nx - f.rect.x, ny - f.rect.y);
            if p.b.style.position == Position::Relative {
                let (rx, ry) = self.relative_offset(p.b, cb);
                f.translate(rx, ry);
            }
            frags.push(f);
        }
        let content_h = row_y
            .last()
            .map_or(0.0, |ly| ly + row_h.last().copied().unwrap_or(0.0))
            - y;
        (frags, content_h.max(0.0))
    }

    /// Track sizes along one axis. Columns use the items' intrinsic
    /// widths; rows the heights from `item_sizes`.
    fn size_tracks(
        &self,
        sizes: &[TrackSize],
        avail: Option<f32>,
        gap: f32,
        placed: &[Placed],
        cols: bool,
        item_sizes: &[f32],
    ) -> Vec<f32> {
        let n = sizes.len();
        let mut base = vec![0.0f32; n];
        let mut limit = vec![0.0f32; n];
        let contribution = |i: usize, p: &Placed| -> (f32, f32) {
            if cols {
                self.intrinsic(p.b)
            } else {
                (item_sizes[i], item_sizes[i])
            }
        };
        for (t, size) in sizes.iter().enumerate() {
            let (mn, mx) = match size {
                TrackSize::MinMax(a, b) => (a.as_ref().clone(), b.as_ref().clone()),
                s => (s.clone(), s.clone()),
            };
            let fixed = |s: &TrackSize| fixed_size(s, avail);
            // Single-span item contributions.
            let (mut cmin, mut cmax) = (0.0f32, 0.0f32);
            for (i, p) in placed.iter().enumerate() {
                let (start, span) = if cols {
                    (p.col, p.cspan)
                } else {
                    (p.row, p.rspan)
                };
                if start == t && span == 1 {
                    let (a, b) = contribution(i, p);
                    cmin = cmin.max(a);
                    cmax = cmax.max(b);
                }
            }
            base[t] = match &mn {
                TrackSize::Fr(_) | TrackSize::Auto => cmin.min(if matches!(mx, TrackSize::Fr(_)) {
                    cmin
                } else {
                    cmin
                }),
                TrackSize::MinContent => cmin,
                TrackSize::MaxContent => cmax,
                TrackSize::FitContent(_) => cmin,
                s => fixed(s).unwrap_or(0.0),
            };
            if matches!(mn, TrackSize::Fr(_)) && matches!(size, TrackSize::Fr(_)) {
                base[t] = if cols { 0.0 } else { cmin };
                // `1fr` is minmax(auto, 1fr): at least the min-content.
                base[t] = cmin;
            }
            limit[t] = match &mx {
                TrackSize::Fr(_) => base[t],
                TrackSize::Auto | TrackSize::MaxContent => cmax.max(base[t]),
                TrackSize::MinContent => cmin.max(base[t]),
                TrackSize::FitContent(l) => cmax.min(l.resolve_opt(avail)).max(base[t]),
                s => fixed(s).unwrap_or(cmax).max(base[t]),
            };
        }
        // Spanning items: distribute extra over the spanned tracks.
        for (i, p) in placed.iter().enumerate() {
            let (start, span) = if cols {
                (p.col, p.cspan)
            } else {
                (p.row, p.rspan)
            };
            if span <= 1 {
                continue;
            }
            let (_, need) = contribution(i, p);
            let have: f32 = base[start..start + span].iter().sum::<f32>() + gap * (span - 1) as f32;
            if need > have {
                let add = (need - have) / span as f32;
                for t in start..start + span {
                    if !matches!(sizes[t], TrackSize::Fr(_)) {
                        base[t] += add;
                        limit[t] = limit[t].max(base[t]);
                    }
                }
            }
        }
        let gaps = gap * n.saturating_sub(1) as f32;
        let mut out = base.clone();
        let Some(avail) = avail else {
            // Indefinite: grow to limits; fr tracks to their max-content.
            for t in 0..n {
                out[t] = limit[t].max(base[t]);
                if matches!(sizes[t], TrackSize::Fr(_)) {
                    out[t] = out[t].max(base[t]);
                }
            }
            return out;
        };
        // Grow non-fr tracks toward their limits.
        let mut free = avail - gaps - out.iter().sum::<f32>();
        if free > 0.0 {
            let growable: Vec<usize> = (0..n).filter(|&t| !matches!(sizes[t], TrackSize::Fr(_)) && !matches!(sizes[t], TrackSize::MinMax(_, ref b) if matches!(**b, TrackSize::Fr(_))) && limit[t] > out[t]).collect();
            let want: f32 = growable.iter().map(|&t| limit[t] - out[t]).sum();
            if want > 0.0 {
                let k = (free / want).min(1.0);
                for &t in &growable {
                    out[t] += (limit[t] - out[t]) * k;
                }
            }
            free = avail - gaps - out.iter().sum::<f32>();
        }
        // Flexible tracks share the rest.
        let frs: Vec<(usize, f32)> = (0..n)
            .filter_map(|t| match &sizes[t] {
                TrackSize::Fr(f) => Some((t, *f)),
                TrackSize::MinMax(_, b) => match **b {
                    TrackSize::Fr(f) => Some((t, f)),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        if !frs.is_empty() {
            let total_fr: f32 = frs.iter().map(|x| x.1).sum::<f32>().max(1.0);
            let space = free + frs.iter().map(|(t, _)| out[*t]).sum::<f32>();
            // Tracks whose base exceeds their share keep it.
            let mut fixed_sum = 0.0;
            let mut fr_left = total_fr;
            let mut flexible: Vec<(usize, f32)> = frs.clone();
            loop {
                let unit = ((space - fixed_sum) / fr_left).max(0.0);
                let too_big: Vec<(usize, f32)> = flexible
                    .iter()
                    .copied()
                    .filter(|(t, f)| out[*t] > unit * f)
                    .collect();
                if too_big.is_empty() {
                    for (t, f) in &flexible {
                        out[*t] = unit * f;
                    }
                    break;
                }
                for (t, f) in too_big {
                    fixed_sum += out[t];
                    fr_left -= f;
                    flexible.retain(|(x, _)| *x != t);
                }
                if flexible.is_empty() || fr_left <= 0.0 {
                    break;
                }
            }
        }
        if self.m.cell_mode() {
            for v in out.iter_mut() {
                *v = if cols {
                    self.m.snap_x(*v)
                } else {
                    self.m.snap_y(*v)
                };
            }
        }
        out
    }
}

/// Start offsets of tracks with content distribution.
fn offsets(sizes: &[f32], gap: f32, origin: f32, align: Align, avail: f32) -> Vec<f32> {
    let n = sizes.len();
    let used: f32 = sizes.iter().sum::<f32>() + gap * n.saturating_sub(1) as f32;
    let free = (avail - used).max(0.0);
    let (mut pos, mut extra) = (origin, 0.0);
    match align {
        Align::Center => pos += free / 2.0,
        Align::End | Align::FlexEnd => pos += free,
        Align::SpaceBetween if n > 1 => extra = free / (n - 1) as f32,
        Align::SpaceAround if n > 0 => {
            extra = free / n as f32;
            pos += extra / 2.0;
        }
        Align::SpaceEvenly => {
            extra = free / (n + 1) as f32;
            pos += extra;
        }
        _ => {}
    }
    let mut out = Vec::with_capacity(n);
    for s in sizes {
        out.push(pos);
        pos += s + gap + extra;
    }
    out
}
