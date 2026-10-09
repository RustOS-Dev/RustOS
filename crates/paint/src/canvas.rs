//! An RGBA canvas with alpha blending, clipping, rounded rectangles,
//! gradients and scaled image drawing.

use alloc::vec;
use alloc::vec::Vec;

/// A color with straight (not premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        Rgba { r, g, b, a }
    }
    pub const WHITE: Rgba = Rgba::new(255, 255, 255, 255);
    pub const BLACK: Rgba = Rgba::new(0, 0, 0, 255);

    pub fn with_alpha(self, a: f32) -> Rgba {
        Rgba {
            a: (self.a as f32 * a.clamp(0.0, 1.0) + 0.5) as u8,
            ..self
        }
    }
}

impl From<css::Rgba> for Rgba {
    fn from(c: css::Rgba) -> Rgba {
        Rgba::new(c.r, c.g, c.b, c.a)
    }
}

/// An integer clip rectangle (x0, y0, x1, y1), exclusive ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clip {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Clip {
    pub fn intersect(self, o: Clip) -> Clip {
        Clip {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
}

/// A decoded image (RGBA, row-major).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Rgba>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Image {
        Image {
            width,
            height,
            pixels: vec![Rgba::default(); (width * height) as usize],
        }
    }
    pub fn get(&self, x: u32, y: u32) -> Rgba {
        self.pixels[(y * self.width + x) as usize]
    }
}

pub struct Canvas {
    pub width: u32,
    pub height: u32,
    /// Opaque pixels (the page background is always painted first).
    pub pixels: Vec<Rgba>,
    clip: Clip,
    /// Global alpha for `opacity`.
    pub alpha: f32,
}

#[inline]
fn blend_ch(dst: u8, src: u8, a: u32) -> u8 {
    ((src as u32 * a + dst as u32 * (255 - a) + 127) / 255) as u8
}

impl Canvas {
    pub fn new(width: u32, height: u32, bg: Rgba) -> Canvas {
        Canvas {
            width,
            height,
            pixels: vec![bg; (width * height) as usize],
            clip: Clip {
                x0: 0,
                y0: 0,
                x1: width as i32,
                y1: height as i32,
            },
            alpha: 1.0,
        }
    }

    pub fn clip(&self) -> Clip {
        self.clip
    }

    /// Replace the clip (returns the old one to restore later).
    pub fn set_clip(&mut self, c: Clip) -> Clip {
        let full = Clip {
            x0: 0,
            y0: 0,
            x1: self.width as i32,
            y1: self.height as i32,
        };
        core::mem::replace(&mut self.clip, c.intersect(full))
    }

    pub fn get(&self, x: u32, y: u32) -> Rgba {
        self.pixels[(y * self.width + x) as usize]
    }

    /// Blend `c` into pixel (x, y) with extra coverage `cov` (0..=255).
    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: Rgba, cov: u32) {
        if x < self.clip.x0 || y < self.clip.y0 || x >= self.clip.x1 || y >= self.clip.y1 {
            return;
        }
        let a = if self.alpha >= 1.0 {
            c.a as u32 * cov / 255
        } else {
            ((c.a as u32 * cov / 255) as f32 * self.alpha) as u32
        };
        if a == 0 {
            return;
        }
        let p = &mut self.pixels[(y as u32 * self.width + x as u32) as usize];
        if a >= 255 {
            *p = Rgba::new(c.r, c.g, c.b, 255);
        } else {
            *p = Rgba::new(
                blend_ch(p.r, c.r, a),
                blend_ch(p.g, c.g, a),
                blend_ch(p.b, c.b, a),
                255,
            );
        }
    }

    /// Fill a rectangle given in fractional pixels (edges anti-aliased).
    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        if w <= 0.0 || h <= 0.0 || c.a == 0 {
            return;
        }
        let (x0, y0, x1, y1) = (x, y, x + w, y + h);
        // Opaque interior: plain stores; only the edges are blended.
        if c.a == 255 && self.alpha >= 1.0 {
            let (ax0, ay0) = (
                (x0.cl() as i32).max(self.clip.x0),
                (y0.cl() as i32).max(self.clip.y0),
            );
            let (ax1, ay1) = (
                (x1.fl() as i32).min(self.clip.x1),
                (y1.fl() as i32).min(self.clip.y1),
            );
            if ax1 > ax0 && ay1 > ay0 {
                let px = Rgba::new(c.r, c.g, c.b, 255);
                for py in ay0..ay1 {
                    let row = (py as u32 * self.width) as usize;
                    self.pixels[row + ax0 as usize..row + ax1 as usize].fill(px);
                }
                // Edge strips (fractional coverage).
                if x0 < ax0 as f32 {
                    self.fill_rect_blend(x0, y0, ax0 as f32 - x0, h, c);
                }
                if x1 > ax1 as f32 {
                    self.fill_rect_blend(ax1 as f32, y0, x1 - ax1 as f32, h, c);
                }
                if y0 < ay0 as f32 {
                    self.fill_rect_blend(ax0 as f32, y0, (ax1 - ax0) as f32, ay0 as f32 - y0, c);
                }
                if y1 > ay1 as f32 {
                    self.fill_rect_blend(
                        ax0 as f32,
                        ay1 as f32,
                        (ax1 - ax0) as f32,
                        y1 - ay1 as f32,
                        c,
                    );
                }
                return;
            }
        }
        self.fill_rect_blend(x, y, w, h, c);
    }

    fn fill_rect_blend(&mut self, x: f32, y: f32, w: f32, h: f32, c: Rgba) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let (x0, y0, x1, y1) = (x, y, x + w, y + h);
        let ix0 = (x0.fl() as i32).max(self.clip.x0);
        let iy0 = (y0.fl() as i32).max(self.clip.y0);
        let ix1 = (x1.cl() as i32).min(self.clip.x1);
        let iy1 = (y1.cl() as i32).min(self.clip.y1);
        for py in iy0..iy1 {
            let cy = cover(py as f32, y0, y1);
            for px in ix0..ix1 {
                let cx = cover(px as f32, x0, x1);
                let cov = (cx * cy * 255.0 + 0.5) as u32;
                self.blend(px, py, c, cov);
            }
        }
    }

    /// Fill a rounded rectangle (radii per corner: tl, tr, br, bl).
    pub fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: [f32; 4], c: Rgba) {
        if r.iter().all(|&v| v <= 0.0) {
            return self.fill_rect(x, y, w, h, c);
        }
        let ix0 = (x.fl() as i32).max(self.clip.x0);
        let iy0 = (y.fl() as i32).max(self.clip.y0);
        let ix1 = ((x + w).cl() as i32).min(self.clip.x1);
        let iy1 = ((y + h).cl() as i32).min(self.clip.y1);
        for py in iy0..iy1 {
            for px in ix0..ix1 {
                let cov = round_rect_coverage(px as f32 + 0.5, py as f32 + 0.5, x, y, w, h, r);
                if cov > 0.0 {
                    self.blend(px, py, c, (cov * 255.0 + 0.5) as u32);
                }
            }
        }
    }

    /// A gradient fill: `color_at(t)` for t along the gradient line.
    pub fn fill_gradient(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        angle_deg: f32,
        color_at: &dyn Fn(f32) -> Rgba,
    ) {
        let a = angle_deg.to_radians();
        let (dx, dy) = (libm_sin(a), -libm_cos(a));
        // Gradient length: the box's extent along the direction.
        let len = (w * dx.abs() + h * dy.abs()).max(1.0);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        let ix0 = (x.fl() as i32).max(self.clip.x0);
        let iy0 = (y.fl() as i32).max(self.clip.y0);
        let ix1 = ((x + w).cl() as i32).min(self.clip.x1);
        let iy1 = ((y + h).cl() as i32).min(self.clip.y1);
        for py in iy0..iy1 {
            for px in ix0..ix1 {
                let t = ((px as f32 + 0.5 - cx) * dx + (py as f32 + 0.5 - cy) * dy) / len + 0.5;
                let c = color_at(t.clamp(0.0, 1.0));
                self.blend(px, py, c, 255);
            }
        }
    }

    /// Draw `img` scaled into (x, y, w, h) with bilinear filtering.
    pub fn draw_image(&mut self, img: &Image, x: f32, y: f32, w: f32, h: f32) {
        if img.width == 0 || img.height == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let ix0 = (x.fl() as i32).max(self.clip.x0);
        let iy0 = (y.fl() as i32).max(self.clip.y0);
        let ix1 = ((x + w).cl() as i32).min(self.clip.x1);
        let iy1 = ((y + h).cl() as i32).min(self.clip.y1);
        let sx = img.width as f32 / w;
        let sy = img.height as f32 / h;
        for py in iy0..iy1 {
            let fy = ((py as f32 + 0.5 - y) * sy - 0.5).clamp(0.0, (img.height - 1) as f32);
            let y0 = fy as u32;
            let y1 = (y0 + 1).min(img.height - 1);
            let ty = fy - y0 as f32;
            for px in ix0..ix1 {
                let fx = ((px as f32 + 0.5 - x) * sx - 0.5).clamp(0.0, (img.width - 1) as f32);
                let x0 = fx as u32;
                let x1 = (x0 + 1).min(img.width - 1);
                let tx = fx - x0 as f32;
                let c = bilerp(
                    img.get(x0, y0),
                    img.get(x1, y0),
                    img.get(x0, y1),
                    img.get(x1, y1),
                    tx,
                    ty,
                );
                self.blend(px, py, c, 255);
            }
        }
    }

    /// Draw an 8-bit coverage mask (a glyph) in color `c` at integer (x, y).
    pub fn draw_mask(&mut self, x: i32, y: i32, w: usize, h: usize, mask: &[u8], c: Rgba) {
        for j in 0..h {
            let py = y + j as i32;
            if py < self.clip.y0 || py >= self.clip.y1 {
                continue;
            }
            for i in 0..w {
                let m = mask[j * w + i];
                if m != 0 {
                    self.blend(x + i as i32, py, c, m as u32);
                }
            }
        }
    }

    /// A 1-pixel-thick line (for underlines and canvas strokes), axis-aligned
    /// or not (Wu-style anti-aliasing for the general case).
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgba) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len = libm_sqrt(dx * dx + dy * dy);
        if len < 0.01 {
            return;
        }
        if dy.abs() < 0.001 {
            return self.fill_rect(x0.min(x1), y0 - width / 2.0, dx.abs(), width, c);
        }
        if dx.abs() < 0.001 {
            return self.fill_rect(x0 - width / 2.0, y0.min(y1), width, dy.abs(), c);
        }
        // Distance-based coverage in the line's bounding box.
        let hw = width / 2.0 + 0.5;
        let bx0 = (x0.min(x1) - hw).fl() as i32;
        let bx1 = (x0.max(x1) + hw).cl() as i32;
        let by0 = (y0.min(y1) - hw).fl() as i32;
        let by1 = (y0.max(y1) + hw).cl() as i32;
        let (ux, uy) = (dx / len, dy / len);
        for py in by0..by1 {
            for px in bx0..bx1 {
                let (qx, qy) = (px as f32 + 0.5 - x0, py as f32 + 0.5 - y0);
                let t = (qx * ux + qy * uy).clamp(0.0, len);
                let (ex, ey) = (qx - t * ux, qy - t * uy);
                let d = libm_sqrt(ex * ex + ey * ey);
                let cov = (width / 2.0 + 0.5 - d).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(px, py, c, (cov * 255.0) as u32);
                }
            }
        }
    }

    /// Fill a polygon (even-odd or nonzero) with 4× vertical supersampling.
    pub fn fill_polygon(&mut self, pts: &[(f32, f32)], c: Rgba, nonzero: bool) {
        if pts.len() < 3 {
            return;
        }
        let miny = pts
            .iter()
            .map(|p| p.1)
            .fold(f32::MAX, f32::min)
            .fl()
            .max(self.clip.y0 as f32) as i32;
        let maxy = pts
            .iter()
            .map(|p| p.1)
            .fold(f32::MIN, f32::max)
            .cl()
            .min(self.clip.y1 as f32) as i32;
        let w = (self.clip.x1 - self.clip.x0).max(0) as usize;
        let mut acc = vec![0f32; w];
        const S: usize = 4;
        for py in miny..maxy {
            acc.iter_mut().for_each(|v| *v = 0.0);
            for s in 0..S {
                let sy = py as f32 + (s as f32 + 0.5) / S as f32;
                let mut xs: Vec<(f32, i32)> = Vec::new();
                for i in 0..pts.len() {
                    let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                    if (a.1 <= sy && b.1 > sy) || (b.1 <= sy && a.1 > sy) {
                        let x = a.0 + (sy - a.1) * (b.0 - a.0) / (b.1 - a.1);
                        xs.push((x, if b.1 > a.1 { 1 } else { -1 }));
                    }
                }
                xs.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap_or(core::cmp::Ordering::Equal));
                let mut wind = 0;
                for k in 0..xs.len() {
                    let inside_before = if nonzero { wind != 0 } else { wind % 2 != 0 };
                    wind += if nonzero { xs[k].1 } else { 1 };
                    let inside = if nonzero { wind != 0 } else { wind % 2 != 0 };
                    if !inside_before && inside {
                        // span start at xs[k].0; find its end
                        let mut w2 = wind;
                        let mut end = xs[k].0;
                        for q in xs.iter().skip(k + 1) {
                            w2 += if nonzero { q.1 } else { 1 };
                            let ins = if nonzero { w2 != 0 } else { w2 % 2 != 0 };
                            end = q.0;
                            if !ins {
                                break;
                            }
                        }
                        let (xa, xb) = (
                            xs[k].0.max(self.clip.x0 as f32),
                            end.min(self.clip.x1 as f32),
                        );
                        let mut x = xa;
                        while x < xb {
                            let px = x.fl();
                            let seg = (px + 1.0).min(xb) - x;
                            let i = (px as i32 - self.clip.x0) as usize;
                            if i < acc.len() {
                                acc[i] += seg / S as f32;
                            }
                            x = px + 1.0;
                        }
                    }
                }
            }
            for (i, v) in acc.iter().enumerate() {
                if *v > 0.0 {
                    self.blend(self.clip.x0 + i as i32, py, c, (v.min(1.0) * 255.0) as u32);
                }
            }
        }
    }

    /// The canvas as an image (copy).
    pub fn to_image(&self) -> Image {
        Image {
            width: self.width,
            height: self.height,
            pixels: self.pixels.clone(),
        }
    }
}

fn cover(p: f32, a: f32, b: f32) -> f32 {
    ((p + 1.0).min(b) - p.max(a)).clamp(0.0, 1.0)
}

fn bilerp(a: Rgba, b: Rgba, c: Rgba, d: Rgba, tx: f32, ty: f32) -> Rgba {
    let l = |p: u8, q: u8, t: f32| p as f32 + (q as f32 - p as f32) * t;
    let ch = |f: fn(Rgba) -> u8| {
        let top = l(f(a), f(b), tx);
        let bot = l(f(c), f(d), tx);
        (top + (bot - top) * ty + 0.5) as u8
    };
    Rgba::new(ch(|p| p.r), ch(|p| p.g), ch(|p| p.b), ch(|p| p.a))
}

/// Coverage of pixel center (px, py) by a rounded rectangle (0..1).
fn round_rect_coverage(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: [f32; 4]) -> f32 {
    if px < x - 0.5 || py < y - 0.5 || px > x + w + 0.5 || py > y + h + 0.5 {
        return 0.0;
    }
    // Which corner region?
    let corners = [
        (x + r[0], y + r[0], r[0]),
        (x + w - r[1], y + r[1], r[1]),
        (x + w - r[2], y + h - r[2], r[2]),
        (x + r[3], y + h - r[3], r[3]),
    ];
    let (cx, cy, rad) = if px < corners[0].0 && py < corners[0].1 {
        corners[0]
    } else if px > corners[1].0 && py < corners[1].1 {
        corners[1]
    } else if px > corners[2].0 && py > corners[2].1 {
        corners[2]
    } else if px < corners[3].0 && py > corners[3].1 {
        corners[3]
    } else {
        let cx = cover(px - 0.5, x, x + w);
        let cy = cover(py - 0.5, y, y + h);
        return cx * cy;
    };
    let d = libm_sqrt((px - cx) * (px - cx) + (py - cy) * (py - cy));
    (rad - d + 0.5).clamp(0.0, 1.0)
}

// Float helpers (no_std).
pub(crate) trait Fx {
    fn fl(self) -> f32;
    fn cl(self) -> f32;
}

impl Fx for f32 {
    fn fl(self) -> f32 {
        css::math::floorf(self)
    }
    fn cl(self) -> f32 {
        -css::math::floorf(-self)
    }
}

pub(crate) fn libm_sqrt(x: f32) -> f32 {
    css::math::sqrtf(x)
}

pub(crate) fn libm_sin(x: f32) -> f32 {
    css::math::sinf(x)
}

pub(crate) fn libm_cos(x: f32) -> f32 {
    css::math::cosf(x)
}
