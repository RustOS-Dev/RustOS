//! Painting a laid-out fragment tree into a canvas: backgrounds (colors,
//! gradients, images), borders with rounded corners, box shadows,
//! text with decorations, list markers, images and form controls, in
//! CSS painting order (positioned boxes by z-index), with overflow
//! clipping and opacity.

use crate::canvas::{Canvas, Clip, Fx, Image, Rgba};
use crate::fonts::Fonts;
use crate::metrics::CONTROL_PAD;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use css::style::{BorderStyle, Image as CssImage, Position, Visibility};
use css::ComputedStyle;
use layout::fragment::{FragKind, Fragment};
use layout::geom::Rect;
use layout::tree::ReplacedKind;
use layout::{Field, FieldKind, Target};

/// What the painter needs besides the fragments.
pub struct Scene<'a> {
    pub fonts: &'a Fonts,
    pub fields: &'a [Field],
    /// Decoded images by resolved URL.
    pub images: &'a BTreeMap<String, Image>,
    pub resolve: &'a dyn Fn(&str) -> String,
    /// Scroll offset (page px at the canvas top-left).
    pub scroll: (f32, f32),
    /// The focused link or field (drawn with a focus ring).
    pub focus: Option<Target>,
    /// Hovered link (underlined in a stronger color).
    pub hover: Option<Target>,
}

fn color(c: css::Rgba) -> Rgba {
    Rgba::from(c)
}

pub fn paint(canvas: &mut Canvas, root: &Fragment, scene: &Scene) {
    let mut deferred: Vec<(i32, usize, &Fragment, Clip)> = Vec::new();
    let clip = canvas.clip();
    paint_frag(canvas, root, scene, clip, &mut deferred, true);
    let mut round = 0;
    while !deferred.is_empty() {
        deferred.sort_by_key(|d| (d.0, d.1));
        let (_, _, f, c) = deferred.remove(0);
        let mut more = Vec::new();
        paint_frag(canvas, f, scene, c, &mut more, true);
        round += 1;
        let base = 1_000_000 * round;
        for (k, m) in more.into_iter().enumerate() {
            deferred.push((m.0, base + k, m.2, m.3));
        }
    }
}

fn to_clip(r: &Rect, scroll: (f32, f32)) -> Clip {
    Clip { x0: (r.x - scroll.0).fl() as i32, y0: (r.y - scroll.1).fl() as i32, x1: (r.x + r.w - scroll.0).cl() as i32, y1: (r.y + r.h - scroll.1).cl() as i32 }
}

fn radii(s: &ComputedStyle, r: &Rect) -> [f32; 4] {
    let mut out = [0.0; 4];
    for (i, lp) in s.border_radius.iter().enumerate() {
        out[i] = lp.resolve(r.w.min(r.h)).max(0.0).min(r.w / 2.0).min(r.h / 2.0);
    }
    out
}

fn paint_frag<'f>(canvas: &mut Canvas, f: &'f Fragment, scene: &Scene, clip: Clip, deferred: &mut Vec<(i32, usize, &'f Fragment, Clip)>, top: bool) {
    let s = &f.style;
    if s.opacity <= 0.0 {
        return;
    }
    if !top && matches!(f.kind, FragKind::Box) && (matches!(s.position, Position::Absolute | Position::Fixed) || s.z_index.is_some() && s.position != Position::Static) {
        let n = deferred.len();
        deferred.push((s.z_index.unwrap_or(0), n, f, clip));
        return;
    }
    let saved_clip = canvas.set_clip(clip);
    let saved_alpha = canvas.alpha;
    canvas.alpha *= s.opacity;
    let (sx, sy) = scene.scroll;
    let r = f.rect;
    let (x, y) = (r.x - sx, r.y - sy);
    let visible = s.visibility == Visibility::Visible;
    match &f.kind {
        FragKind::Box if visible => paint_box(canvas, f, scene, x, y),
        FragKind::Text { text, deco, baseline } if visible => {
            let id = scene.fonts.face_for(s);
            let mut c = color(s.color);
            let is_link = matches!(f.target, Target::Link(_));
            if is_link && scene.hover == Some(f.target) {
                c = Rgba::new(c.r / 2, c.g / 2, c.b / 2 + 60, c.a);
            }
            let by = baseline - sy;
            let w = scene.fonts.draw(canvas, id, text, s.font_size, x, by, c, s.letter_spacing);
            let dc = color(s.text_decoration_color.resolve(s.color));
            let thick = (s.font_size / 14.0).max(1.0);
            if deco & css::style::DECO_UNDERLINE != 0 {
                canvas.fill_rect(x, by + thick.max(1.5), w, thick, dc); // underline
            }
            if deco & css::style::DECO_OVERLINE != 0 {
                canvas.fill_rect(x, by - s.font_size * 0.8, w, thick, dc); // overline
            }
            if deco & css::style::DECO_LINE_THROUGH != 0 {
                canvas.fill_rect(x, by - s.font_size * 0.3, w, thick, dc); // line-through
            }
            if scene.focus == Some(f.target) && is_link {
                focus_ring(canvas, x - 1.0, y, w + 2.0, r.h);
            }
        }
        FragKind::Marker(text) if visible => {
            let id = scene.fonts.face_for(s);
            let (a, _) = scene.fonts.vmetrics(id, s.font_size);
            let half = (s.line_height_px() - s.font_size * 1.2) / 2.0;
            let base = y + half.max(0.0) + a;
            // Bullets are drawn as shapes (not every font has them).
            let c = color(s.color);
            let d = s.font_size * 0.35;
            match text.trim() {
                "•" | "*" => fill_circle(canvas, x + r.w - d * 1.8, base - s.font_size * 0.35, d / 2.0, c),
                "◦" | "o" => {
                    fill_circle(canvas, x + r.w - d * 1.8, base - s.font_size * 0.35, d / 2.0, c);
                    fill_circle(canvas, x + r.w - d * 1.8, base - s.font_size * 0.35, d / 2.0 - 1.0, Rgba::WHITE);
                }
                "▪" | "#" | "+" => canvas.fill_rect(x + r.w - d * 2.3, base - s.font_size * 0.35 - d / 2.0, d, d, c),
                _ => {
                    let w = scene.fonts.text_width(id, text, s.font_size, 0.0);
                    scene.fonts.draw(canvas, id, text, s.font_size, x + r.w - w, base, c, 0.0);
                }
            }
        }
        FragKind::Replaced(rk) if visible => paint_replaced(canvas, f, rk, scene, x, y),
        _ => {}
    }
    let child_clip = match f.clip {
        Some(c) => clip.intersect(to_clip(&c, scene.scroll)),
        None => clip,
    };
    for c in &f.children {
        paint_frag(canvas, c, scene, child_clip, deferred, false);
    }
    canvas.alpha = saved_alpha;
    canvas.set_clip(saved_clip);
}

fn paint_box(canvas: &mut Canvas, f: &Fragment, scene: &Scene, x: f32, y: f32) {
    let s = &f.style;
    let (w, h) = (f.rect.w, f.rect.h);
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let rad = radii(s, &f.rect);
    // Outer shadows (blur approximated by a few expanding layers).
    for sh in s.box_shadow.iter().filter(|s| !s.inset) {
        let c = color(sh.color.resolve(s.color));
        let layers = (sh.blur / 2.0).cl().clamp(1.0, 6.0) as i32;
        for k in 0..layers {
            let grow = sh.spread + sh.blur * (k as f32 + 1.0) / (layers as f32 + 1.0);
            let a = 1.0 / (layers as f32 + 1.0);
            let rr = rad.map(|v| v + grow);
            canvas.fill_round_rect(x + sh.x - grow, y + sh.y - grow, w + 2.0 * grow, h + 2.0 * grow, rr, c.with_alpha(a));
        }
        if layers <= 1 && sh.blur == 0.0 {
            canvas.fill_round_rect(x + sh.x - sh.spread, y + sh.y - sh.spread, w + 2.0 * sh.spread, h + 2.0 * sh.spread, rad, c);
        }
    }
    // Background color, then layers (last listed is painted first).
    let bg = color(s.background_color_rgba());
    if bg.a > 0 {
        canvas.fill_round_rect(x, y, w, h, rad, bg);
    }
    for layer in s.background.iter().rev() {
        match &layer.image {
            Some(CssImage::Linear { angle, stops, .. }) => {
                let cols: Vec<(Rgba, f32)> = stops
                    .iter()
                    .enumerate()
                    .map(|(i, (c, p))| {
                        let t = p.as_ref().map(|lp| lp.resolve(1.0)).unwrap_or(if stops.len() > 1 { i as f32 / (stops.len() - 1) as f32 } else { 0.0 });
                        (color(c.resolve(s.color)), t)
                    })
                    .collect();
                canvas.fill_gradient(x, y, w, h, *angle, &|t| gradient_at(&cols, t));
            }
            Some(CssImage::Radial { stops, .. }) => {
                // Approximated by a vertical gradient from the center color.
                let cols: Vec<(Rgba, f32)> = stops.iter().enumerate().map(|(i, (c, _))| (color(c.resolve(s.color)), i as f32 / (stops.len().max(2) - 1) as f32)).collect();
                canvas.fill_gradient(x, y, w, h, 180.0, &|t| gradient_at(&cols, (t - 0.5).abs() * 2.0));
            }
            Some(CssImage::Url(u)) => {
                let key = (scene.resolve)(u);
                if let Some(img) = scene.images.get(&key) {
                    let (iw, ih) = match &layer.size {
                        css::style::BgSize::Cover => {
                            let k = (w / img.width as f32).max(h / img.height as f32);
                            (img.width as f32 * k, img.height as f32 * k)
                        }
                        css::style::BgSize::Contain => {
                            let k = (w / img.width as f32).min(h / img.height as f32);
                            (img.width as f32 * k, img.height as f32 * k)
                        }
                        _ => (img.width as f32, img.height as f32),
                    };
                    let px = x + layer.position.0.resolve(w - iw);
                    let py = y + layer.position.1.resolve(h - ih);
                    let old = canvas.set_clip(canvas.clip().intersect(Clip { x0: x as i32, y0: y as i32, x1: (x + w) as i32, y1: (y + h) as i32 }));
                    let nx = if layer.repeat_x && iw > 0.0 { ((w / iw).cl() as i32 + 1).min(64) } else { 1 };
                    let ny = if layer.repeat_y && ih > 0.0 { ((h / ih).cl() as i32 + 1).min(64) } else { 1 };
                    let (ox, oy) = if layer.repeat_x || layer.repeat_y { ((px - x) % iw.max(1.0), (py - y) % ih.max(1.0)) } else { (px - x, py - y) };
                    for j in 0..ny {
                        for i in 0..nx {
                            let tx = if layer.repeat_x { x + ox - iw + i as f32 * iw } else { px };
                            let ty = if layer.repeat_y { y + oy - ih + j as f32 * ih } else { py };
                            canvas.draw_image(img, tx, ty, iw, ih);
                        }
                    }
                    canvas.set_clip(old);
                }
            }
            None => {}
        }
    }
    // Borders.
    let bw = s.border_width;
    let styles = s.border_style;
    if bw.iter().any(|&b| b > 0.0) {
        let cols: [Rgba; 4] = core::array::from_fn(|i| color(s.border_color[i].resolve(s.color)));
        let uniform = bw.iter().all(|&b| b == bw[0]) && cols.iter().all(|&c| c == cols[0]) && styles.iter().all(|&st| st == styles[0]);
        if uniform && rad.iter().any(|&r| r > 0.0) && styles[0] != BorderStyle::None {
            // Ring: outer rounded rect minus inner (drawn by coverage).
            ring(canvas, x, y, w, h, rad, bw[0], cols[0]);
        } else {
            for side in 0..4 {
                if bw[side] <= 0.0 || matches!(styles[side], BorderStyle::None | BorderStyle::Hidden) {
                    continue;
                }
                let c = match styles[side] {
                    BorderStyle::Inset if side < 2 => darken(cols[side]),
                    BorderStyle::Outset if side >= 2 => darken(cols[side]),
                    BorderStyle::Groove if side < 2 => darken(cols[side]),
                    BorderStyle::Ridge if side >= 2 => darken(cols[side]),
                    _ => cols[side],
                };
                let (rx, ry, rw, rh) = match side {
                    0 => (x, y, w, bw[0]),
                    1 => (x + w - bw[1], y, bw[1], h),
                    2 => (x, y + h - bw[2], w, bw[2]),
                    _ => (x, y, bw[3], h),
                };
                match styles[side] {
                    BorderStyle::Dotted | BorderStyle::Dashed => {
                        let dash = if styles[side] == BorderStyle::Dotted { bw[side] } else { bw[side] * 3.0 };
                        let horizontal = side % 2 == 0;
                        let len = if horizontal { rw } else { rh };
                        let mut t = 0.0;
                        while t < len {
                            let l = dash.min(len - t);
                            if horizontal {
                                canvas.fill_rect(rx + t, ry, l, rh, c);
                            } else {
                                canvas.fill_rect(rx, ry + t, rw, l, c);
                            }
                            t += dash * 2.0;
                        }
                    }
                    BorderStyle::Double if bw[side] >= 3.0 => {
                        let third = bw[side] / 3.0;
                        if side % 2 == 0 {
                            canvas.fill_rect(rx, ry, rw, third, c);
                            canvas.fill_rect(rx, ry + rh - third, rw, third, c);
                        } else {
                            canvas.fill_rect(rx, ry, third, rh, c);
                            canvas.fill_rect(rx + rw - third, ry, third, rh, c);
                        }
                    }
                    _ => canvas.fill_rect(rx, ry, rw, rh, c),
                }
            }
        }
    }
    // Inset shadows.
    for sh in s.box_shadow.iter().filter(|s| s.inset) {
        let c = color(sh.color.resolve(s.color)).with_alpha(0.5);
        canvas.fill_rect(x, y, w, (sh.y + sh.blur / 2.0).max(0.0), c);
        canvas.fill_rect(x, y, (sh.x + sh.blur / 2.0).max(0.0), h, c);
    }
}

fn darken(c: Rgba) -> Rgba {
    Rgba::new(c.r / 2, c.g / 2, c.b / 2, c.a)
}

fn gradient_at(stops: &[(Rgba, f32)], t: f32) -> Rgba {
    if stops.is_empty() {
        return Rgba::default();
    }
    if t <= stops[0].1 {
        return stops[0].0;
    }
    for w in stops.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.1 {
            let k = if b.1 > a.1 { (t - a.1) / (b.1 - a.1) } else { 1.0 };
            let l = |p: u8, q: u8| (p as f32 + (q as f32 - p as f32) * k + 0.5) as u8;
            return Rgba::new(l(a.0.r, b.0.r), l(a.0.g, b.0.g), l(a.0.b, b.0.b), l(a.0.a, b.0.a));
        }
    }
    stops[stops.len() - 1].0
}

fn ring(canvas: &mut Canvas, x: f32, y: f32, w: f32, h: f32, rad: [f32; 4], bw: f32, c: Rgba) {
    // Paint the outer shape into a mask, subtract the inner one.
    let (ix0, iy0) = (x.fl() as i32, y.fl() as i32);
    let (iw, ih) = ((w.cl() as i32 + 2).max(0) as usize, (h.cl() as i32 + 2).max(0) as usize);
    if iw * ih > 4_000_000 {
        return;
    }
    let mut outer = Canvas::new(iw as u32, ih as u32, Rgba::new(0, 0, 0, 255));
    outer.fill_round_rect(x - ix0 as f32, y - iy0 as f32, w, h, rad, Rgba::WHITE);
    let inner_r = rad.map(|r| (r - bw).max(0.0));
    let mut inner = Canvas::new(iw as u32, ih as u32, Rgba::new(0, 0, 0, 255));
    inner.fill_round_rect(x - ix0 as f32 + bw, y - iy0 as f32 + bw, w - 2.0 * bw, h - 2.0 * bw, inner_r, Rgba::WHITE);
    for j in 0..ih {
        for i in 0..iw {
            let a = outer.pixels[j * iw + i].r as i32 - inner.pixels[j * iw + i].r as i32;
            if a > 0 {
                canvas.blend(ix0 + i as i32, iy0 + j as i32, c, a as u32);
            }
        }
    }
}

fn fill_circle(canvas: &mut Canvas, cx: f32, cy: f32, r: f32, c: Rgba) {
    canvas.fill_round_rect(cx - r, cy - r, 2.0 * r, 2.0 * r, [r; 4], c);
}

fn focus_ring(canvas: &mut Canvas, x: f32, y: f32, w: f32, h: f32) {
    let c = Rgba::new(40, 110, 230, 255);
    canvas.fill_rect(x - 2.0, y - 2.0, w + 4.0, 2.0, c);
    canvas.fill_rect(x - 2.0, y + h, w + 4.0, 2.0, c);
    canvas.fill_rect(x - 2.0, y, 2.0, h, c);
    canvas.fill_rect(x + w, y, 2.0, h, c);
}

fn paint_replaced(canvas: &mut Canvas, f: &Fragment, rk: &ReplacedKind, scene: &Scene, x: f32, y: f32) {
    let s = &f.style;
    let r = f.rect;
    // Content box.
    let (cx, cy) = (x + s.border_width[3] + s.padding[3].resolve(r.w), y + s.border_width[0] + s.padding[0].resolve(r.w));
    let cw = (r.w - s.border_width[1] - s.border_width[3] - s.padding[1].resolve(r.w) - s.padding[3].resolve(r.w)).max(0.0);
    let ch = (r.h - s.border_width[0] - s.border_width[2] - s.padding[0].resolve(r.w) - s.padding[2].resolve(r.w)).max(0.0);
    paint_box(canvas, f, scene, x, y);
    let fonts = scene.fonts;
    let id = fonts.face_for(s);
    let text_c = color(s.color);
    match rk {
        ReplacedKind::Image { src, alt } => {
            let key = (scene.resolve)(src);
            if let Some(img) = scene.images.get(&key) {
                canvas.draw_image(img, cx, cy, cw, ch);
            } else if !alt.is_empty() {
                canvas.fill_rect(cx, cy, cw, 1.0, Rgba::new(180, 180, 180, 255));
                canvas.fill_rect(cx, cy + ch - 1.0, cw, 1.0, Rgba::new(180, 180, 180, 255));
                canvas.fill_rect(cx, cy, 1.0, ch, Rgba::new(180, 180, 180, 255));
                canvas.fill_rect(cx + cw - 1.0, cy, 1.0, ch, Rgba::new(180, 180, 180, 255));
                let (a, _) = fonts.vmetrics(id, s.font_size);
                fonts.draw(canvas, id, alt, s.font_size, cx + CONTROL_PAD, cy + (ch - s.font_size * 1.2) / 2.0 + a, text_c, 0.0);
            }
            if scene.focus.is_some() && scene.focus == Some(f.target) {
                focus_ring(canvas, cx, cy, cw, ch);
            }
        }
        ReplacedKind::Placeholder(label) => {
            canvas.fill_rect(cx, cy, cw, ch, Rgba::new(235, 235, 235, 255));
            let (a, _) = fonts.vmetrics(id, s.font_size);
            fonts.draw(canvas, id, label, s.font_size, cx + CONTROL_PAD, cy + CONTROL_PAD + a, Rgba::new(90, 90, 90, 255), 0.0);
        }
        ReplacedKind::Field(i) => {
            let Some(fd) = scene.fields.get(*i) else { return };
            paint_control(canvas, fd, s, fonts, id, cx, cy, cw, ch);
            if scene.focus == Some(Target::Field(*i)) {
                focus_ring(canvas, cx, cy, cw, ch);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_control(canvas: &mut Canvas, fd: &Field, s: &ComputedStyle, fonts: &Fonts, id: usize, x: f32, y: f32, w: f32, h: f32) {
    let text_c = if fd.disabled { Rgba::new(140, 140, 140, 255) } else { color(s.color) };
    let border = Rgba::new(118, 118, 118, 255);
    let (a, _) = fonts.vmetrics(id, s.font_size);
    let base = y + (h - s.font_size * 1.2) / 2.0 + a;
    let author_bg = s.background_color_rgba().a > 0;
    let outline = |canvas: &mut Canvas| {
        canvas.fill_rect(x, y, w, 1.0, border);
        canvas.fill_rect(x, y + h - 1.0, w, 1.0, border);
        canvas.fill_rect(x, y, 1.0, h, border);
        canvas.fill_rect(x + w - 1.0, y, 1.0, h, border);
    };
    match fd.kind {
        FieldKind::Hidden => {}
        FieldKind::Checkbox | FieldKind::Radio => {
            let d = w.min(h);
            if fd.kind == FieldKind::Radio {
                fill_circle(canvas, x + d / 2.0, y + d / 2.0, d / 2.0, border);
                fill_circle(canvas, x + d / 2.0, y + d / 2.0, d / 2.0 - 1.0, Rgba::WHITE);
                if fd.checked {
                    fill_circle(canvas, x + d / 2.0, y + d / 2.0, d / 4.0, Rgba::new(0, 95, 204, 255));
                }
            } else {
                canvas.fill_round_rect(x, y, d, d, [2.0; 4], if fd.checked { Rgba::new(0, 95, 204, 255) } else { border });
                if !fd.checked {
                    canvas.fill_rect(x + 1.0, y + 1.0, d - 2.0, d - 2.0, Rgba::WHITE);
                } else {
                    canvas.line(x + d * 0.22, y + d * 0.52, x + d * 0.42, y + d * 0.72, 2.0, Rgba::WHITE);
                    canvas.line(x + d * 0.42, y + d * 0.72, x + d * 0.78, y + d * 0.28, 2.0, Rgba::WHITE);
                }
            }
        }
        FieldKind::Submit | FieldKind::Reset | FieldKind::Button | FieldKind::Image => {
            if !author_bg {
                canvas.fill_round_rect(x, y, w, h, [3.0; 4], border);
                canvas.fill_round_rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0, [2.0; 4], Rgba::new(239, 239, 239, 255));
            }
            let label = if !fd.label.is_empty() {
                fd.label.as_str()
            } else if !fd.value.is_empty() {
                fd.value.as_str()
            } else if fd.kind == FieldKind::Reset {
                "Reset"
            } else {
                "Submit"
            };
            let tw = fonts.text_width(id, label, s.font_size, 0.0);
            fonts.draw(canvas, id, label, s.font_size, x + (w - tw) / 2.0, base, text_c, 0.0);
        }
        FieldKind::Select => {
            if !author_bg {
                canvas.fill_rect(x, y, w, h, Rgba::WHITE);
            }
            outline(canvas);
            let label = fd.options.get(fd.selected).map(|o| o.label.as_str()).unwrap_or("");
            fonts.draw(canvas, id, label, s.font_size, x + CONTROL_PAD, base, text_c, 0.0);
            // A down arrow.
            let ax = x + w - s.font_size * 0.9;
            let ay = y + h / 2.0 - 2.0;
            canvas.fill_polygon(&[(ax, ay), (ax + 8.0, ay), (ax + 4.0, ay + 5.0)], text_c, true);
        }
        _ => {
            if !author_bg {
                canvas.fill_rect(x, y, w, h, Rgba::WHITE);
            }
            outline(canvas);
            let old = canvas.set_clip(canvas.clip().intersect(Clip { x0: x as i32 + 1, y0: y as i32 + 1, x1: (x + w) as i32 - 1, y1: (y + h) as i32 - 1 }));
            if fd.value.is_empty() {
                if !fd.label.is_empty() && fd.kind != FieldKind::File {
                    fonts.draw(canvas, id, &fd.label, s.font_size, x + CONTROL_PAD, base, Rgba::new(150, 150, 150, 255), 0.0);
                }
            } else {
                let shown: String = if fd.kind == FieldKind::Password { fd.value.chars().map(|_| '•').collect() } else { fd.value.clone() };
                if fd.kind == FieldKind::Textarea {
                    let lh = s.line_height_px();
                    for (k, line) in shown.split('\n').enumerate() {
                        fonts.draw(canvas, id, line, s.font_size, x + CONTROL_PAD, y + CONTROL_PAD + a + k as f32 * lh, text_c, 0.0);
                    }
                } else {
                    fonts.draw(canvas, id, &shown, s.font_size, x + CONTROL_PAD, base, text_c, 0.0);
                }
            }
            canvas.set_clip(old);
        }
    }
}
