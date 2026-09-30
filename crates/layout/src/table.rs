//! Table layout (CSS 2 §17, automatic and fixed algorithms): the cell
//! grid with row and column spans, column widths from cell min/max
//! content widths, row heights, `border-spacing`, captions and
//! `vertical-align` in cells.

use crate::flow::{BoxModel, Cb, Engine, Floats};
use crate::fragment::{FragKind, Fragment};
use crate::geom::Rect;
use crate::tree::{BoxKind, LayoutBox};
use alloc::vec;
use alloc::vec::Vec;
use css::style::{BorderCollapse, CaptionSide, Size, TableLayout, VerticalAlign};

struct Cell<'a> {
    b: &'a LayoutBox,
    row: usize,
    col: usize,
    cspan: usize,
    rspan: usize,
}

struct Grid<'a> {
    rows: Vec<&'a LayoutBox>,
    cells: Vec<Cell<'a>>,
    ncols: usize,
    /// Widths from `<col>`/`<colgroup>` elements.
    col_spec: Vec<Option<f32>>,
}

fn build_grid(b: &LayoutBox) -> Grid<'_> {
    let mut g = Grid {
        rows: Vec::new(),
        cells: Vec::new(),
        ncols: 0,
        col_spec: Vec::new(),
    };
    // Occupied slots from row spans: (row, col).
    let mut taken: Vec<Vec<bool>> = Vec::new();
    for c in &b.children {
        match &c.kind {
            BoxKind::TableColumn { span } => {
                let w = match &c.style.width {
                    Size::Lp(l) if !l.has_percent() => Some(l.resolve(0.0)),
                    _ => None,
                };
                if c.children.is_empty() {
                    for _ in 0..*span {
                        g.col_spec.push(w);
                    }
                } else {
                    for col in &c.children {
                        if let BoxKind::TableColumn { span } = col.kind {
                            let cw = match &col.style.width {
                                Size::Lp(l) if !l.has_percent() => Some(l.resolve(0.0)),
                                _ => w,
                            };
                            for _ in 0..span {
                                g.col_spec.push(cw);
                            }
                        }
                    }
                }
            }
            BoxKind::TableRowGroup => {
                for row in &c.children {
                    if !matches!(row.kind, BoxKind::TableRow) {
                        continue;
                    }
                    let r = g.rows.len();
                    g.rows.push(row);
                    while taken.len() <= r {
                        taken.push(Vec::new());
                    }
                    let mut col = 0;
                    for cell in &row.children {
                        let BoxKind::TableCell { colspan, rowspan } = cell.kind else {
                            continue;
                        };
                        while taken[r].get(col).copied().unwrap_or(false) {
                            col += 1;
                        }
                        let cspan = colspan as usize;
                        // rowspan=0 spans to the end of the group: approximate as 1.
                        let rspan = (rowspan as usize).max(1);
                        for rr in r..r + rspan {
                            while taken.len() <= rr {
                                taken.push(Vec::new());
                            }
                            if taken[rr].len() < col + cspan {
                                taken[rr].resize(col + cspan, false);
                            }
                            for cc in col..col + cspan {
                                taken[rr][cc] = true;
                            }
                        }
                        g.cells.push(Cell {
                            b: cell,
                            row: r,
                            col,
                            cspan,
                            rspan,
                        });
                        col += cspan;
                        g.ncols = g.ncols.max(col);
                    }
                }
            }
            _ => {}
        }
    }
    g.ncols = g.ncols.max(g.col_spec.len());
    // Clip row spans that run past the last row.
    let nrows = g.rows.len();
    for c in g.cells.iter_mut() {
        c.rspan = c.rspan.min(nrows - c.row).max(1);
    }
    g
}

impl<'a> Engine<'a> {
    /// Horizontal and vertical spacing between cells, and at the edges.
    fn spacing(&self, b: &LayoutBox) -> (f32, f32, f32) {
        let s = &b.style;
        let (h, v) = if s.border_collapse == BorderCollapse::Collapse {
            (0.0, 0.0)
        } else {
            s.border_spacing
        };
        if self.m.cell_mode() {
            // Columns need at least one blank cell between them; no
            // spacing at the table edges or between rows.
            (h.max(8.0).min(16.0), 0.0, 0.0)
        } else {
            (h, v, h)
        }
    }

    /// (min, max) width per column (cell border boxes).
    fn column_widths(&self, g: &Grid) -> (Vec<f32>, Vec<f32>) {
        let n = g.ncols;
        let mut min = vec![0.0f32; n];
        let mut max = vec![0.0f32; n];
        let (hs, _, _) = (0.0, 0.0, 0.0);
        let _ = hs;
        let cell_mm = |c: &Cell| -> (f32, f32) {
            let (mut a, mut b) = self.intrinsic(c.b);
            let bm = self.box_model(c.b, 0.0);
            if let Some(w) = self.spec_width(c.b, None, &bm) {
                b = w.max(a);
                a = a.max(if self.m.cell_mode() {
                    a
                } else {
                    w.min(a).max(0.0)
                });
            }
            (a, b.max(a))
        };
        for c in g.cells.iter().filter(|c| c.cspan == 1) {
            let (a, b) = cell_mm(c);
            min[c.col] = min[c.col].max(a);
            max[c.col] = max[c.col].max(b);
        }
        for (i, w) in g.col_spec.iter().enumerate() {
            if let Some(w) = w
                && i < n
            {
                max[i] = max[i].max(*w);
                min[i] = min[i].max(if self.m.cell_mode() { 0.0 } else { *w });
            }
        }
        for c in g.cells.iter().filter(|c| c.cspan > 1) {
            let (a, b) = cell_mm(c);
            let r = c.col..c.col + c.cspan;
            let have_min: f32 = min[r.clone()].iter().sum();
            let have_max: f32 = max[r.clone()].iter().sum();
            if a > have_min {
                let add = (a - have_min) / c.cspan as f32;
                for i in r.clone() {
                    min[i] += add;
                }
            }
            if b > have_max {
                let total: f32 = have_max.max(f32::MIN_POSITIVE);
                for i in r.clone() {
                    let share = if have_max > 0.0 {
                        max[i] / total
                    } else {
                        1.0 / c.cspan as f32
                    };
                    max[i] += (b - have_max) * share;
                }
            }
            for i in r {
                max[i] = max[i].max(min[i]);
            }
        }
        (min, max)
    }

    pub fn table_intrinsic(&self, b: &LayoutBox) -> (f32, f32) {
        let g = build_grid(b);
        let (min, max) = self.column_widths(&g);
        let (hs, _, edge) = self.spacing(b);
        let gaps = hs * g.ncols.saturating_sub(1) as f32 + 2.0 * edge;
        let mut mn = min.iter().sum::<f32>() + gaps;
        let mut mx = max.iter().sum::<f32>() + gaps;
        for c in &b.children {
            if matches!(c.kind, BoxKind::TableCaption) {
                let (a, bb) = self.intrinsic(c);
                mn = mn.max(a);
                mx = mx.max(bb);
            }
        }
        (mn, mx)
    }

    /// Used border-box width of an auto-width table.
    pub fn table_width(&self, b: &LayoutBox, avail: f32, bm: &BoxModel) -> f32 {
        let (mn, mx) = self.table_intrinsic(b);
        let frame = bm.frame_h();
        let w = if mx + frame <= avail {
            mx + frame
        } else {
            (mn + frame).max(avail)
        };
        self.clamp_w(b, w, avail, bm)
    }

    pub fn layout_table(
        &mut self,
        b: &'a LayoutBox,
        x: f32,
        y: f32,
        content_w: f32,
        cb_h: Option<f32>,
    ) -> (Vec<Fragment>, f32) {
        let g = build_grid(b);
        let (hs, vs, edge) = self.spacing(b);
        let n = g.ncols;
        let avail_cols = (content_w - hs * n.saturating_sub(1) as f32 - 2.0 * edge).max(0.0);
        let (min, max) = self.column_widths(&g);
        let mut widths = vec![0.0f32; n];
        let fixed =
            b.style.table_layout == TableLayout::Fixed && !matches!(b.style.width, Size::Auto);
        if fixed && n > 0 {
            // Widths from the first row (and <col>), the rest equally.
            let mut known = vec![None; n];
            for (i, w) in g.col_spec.iter().enumerate() {
                if i < n {
                    known[i] = *w;
                }
            }
            for c in g.cells.iter().filter(|c| c.row == 0) {
                let bm = self.box_model(c.b, content_w);
                if let Some(w) = self.spec_width(c.b, Some(content_w), &bm) {
                    for k in known.iter_mut().skip(c.col).take(c.cspan) {
                        if k.is_none() {
                            *k = Some(w / c.cspan as f32);
                        }
                    }
                }
            }
            let used: f32 = known.iter().flatten().sum();
            let unknown = known.iter().filter(|k| k.is_none()).count();
            let each = if unknown > 0 {
                ((avail_cols - used) / unknown as f32).max(0.0)
            } else {
                0.0
            };
            for i in 0..n {
                widths[i] = known[i].unwrap_or(each);
            }
        } else {
            let smin: f32 = min.iter().sum();
            let smax: f32 = max.iter().sum();
            if avail_cols >= smax {
                // Extra space (a wide table): grow in proportion to max.
                let extra = avail_cols - smax;
                for i in 0..n {
                    let share = if smax > 0.0 {
                        max[i] / smax
                    } else {
                        1.0 / n as f32
                    };
                    widths[i] = max[i] + extra * share;
                }
            } else if avail_cols >= smin && smax > smin {
                let k = (avail_cols - smin) / (smax - smin);
                for i in 0..n {
                    widths[i] = min[i] + (max[i] - min[i]) * k;
                }
            } else {
                widths[..n].copy_from_slice(&min[..n]);
            }
        }
        if self.m.cell_mode() {
            // Whole cells; give the rounding error to the last column.
            let mut acc = 0.0;
            for w in widths.iter_mut() {
                let s = self.m.snap_x(acc + *w) - self.m.snap_x(acc);
                acc += *w;
                *w = s;
            }
        }
        let col_x: Vec<f32> = {
            let mut v = Vec::with_capacity(n);
            let mut cx = x + edge;
            for w in &widths {
                v.push(cx);
                cx += w + hs;
            }
            v
        };
        let mut frags = Vec::new();
        let mut cy = y;
        // Captions (top).
        let caption_frags =
            |me: &mut Engine<'a>, side: CaptionSide, cy: &mut f32, frags: &mut Vec<Fragment>| {
                for c in b.children.iter().filter(|c| {
                    matches!(c.kind, BoxKind::TableCaption) && c.style.caption_side == side
                }) {
                    let mut fl = Floats::default();
                    let out = me.layout_block(
                        c,
                        Cb {
                            w: content_w,
                            h: None,
                        },
                        x,
                        *cy,
                        &mut fl,
                        false,
                    );
                    *cy = out.frag.rect.bottom();
                    frags.push(out.frag);
                }
            };
        caption_frags(self, CaptionSide::Top, &mut cy, &mut frags);
        cy += vs;
        // Lay out cells at their widths.
        let mut cell_frags: Vec<Fragment> = Vec::with_capacity(g.cells.len());
        let nrows = g.rows.len();
        let mut row_h = vec![0.0f32; nrows];
        for (ri, row) in g.rows.iter().enumerate() {
            let bm = self.box_model(row, content_w);
            if let Some(h) = self.spec_height(row, cb_h, &bm) {
                row_h[ri] = h;
            }
        }
        for c in &g.cells {
            let w: f32 =
                widths[c.col..c.col + c.cspan].iter().sum::<f32>() + hs * (c.cspan - 1) as f32;
            let mut fl = Floats::default();
            let out = self.layout_block_sized(c.b, Cb { w, h: None }, 0.0, 0.0, w, &mut fl);
            if c.rspan == 1 {
                row_h[c.row] = row_h[c.row].max(out.frag.rect.h);
            }
            cell_frags.push(out.frag);
        }
        for (i, c) in g.cells.iter().enumerate().filter(|(_, c)| c.rspan > 1) {
            let have: f32 =
                row_h[c.row..c.row + c.rspan].iter().sum::<f32>() + vs * (c.rspan - 1) as f32;
            let need = cell_frags[i].rect.h;
            if need > have {
                row_h[c.row + c.rspan - 1] += need - have;
            }
        }
        let mut row_y = Vec::with_capacity(nrows);
        for h in &row_h {
            row_y.push(cy);
            cy += h + vs;
        }
        if nrows == 0 {
            cy -= vs;
        }
        // Row fragments (backgrounds) with their cells.
        let mut row_frags: Vec<Fragment> = g
            .rows
            .iter()
            .enumerate()
            .map(|(ri, row)| {
                let mut f = Fragment::new(
                    Rect::new(x + edge, row_y[ri], content_w - 2.0 * edge, row_h[ri]),
                    row.style.clone(),
                    row.node,
                    FragKind::Box,
                );
                f.target = row.target;
                f
            })
            .collect();
        for (i, c) in g.cells.iter().enumerate() {
            let mut f = cell_frags[i].clone();
            let h: f32 =
                row_h[c.row..c.row + c.rspan].iter().sum::<f32>() + vs * (c.rspan - 1) as f32;
            let content_h = f.rect.h;
            f.rect.h = h;
            let off = match c.b.style.vertical_align {
                VerticalAlign::Middle => (h - content_h) / 2.0,
                VerticalAlign::Bottom => h - content_h,
                _ => 0.0,
            };
            let off = self.m.snap_y(off.max(0.0));
            if off > 0.0 {
                for ch in f.children.iter_mut() {
                    ch.translate(0.0, off);
                }
            }
            let (tx, ty) = (col_x[c.col], row_y[c.row]);
            f.translate(tx - f.rect.x, ty - f.rect.y);
            row_frags[c.row].children.push(f);
        }
        frags.extend(row_frags);
        cy -= vs;
        cy += vs;
        caption_frags(self, CaptionSide::Bottom, &mut cy, &mut frags);
        (frags, (cy - y).max(0.0))
    }
}
