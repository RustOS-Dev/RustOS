//! Fonts: a set of faces (sans, serif, monospace; regular, bold, italic,
//! bold italic) chosen from the CSS `font-family`/`font-weight`/
//! `font-style`, rasterized with fontdue into a glyph cache, plus the
//! layout metrics for pixel devices.

use crate::canvas::{Canvas, Fx, Rgba};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use css::ComputedStyle;
use fontdue::{Font, FontSettings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

/// A face slot: family × bold × italic.
fn slot(f: Family, bold: bool, italic: bool) -> usize {
    (f as usize) * 4 + (bold as usize) * 2 + italic as usize
}

struct Glyph {
    xmin: i32,
    ymin: i32,
    width: usize,
    height: usize,
    advance: f32,
    mask: Vec<u8>,
}

/// A face whose file is parsed on first use (fontdue loads every glyph
/// outline, so only faces a page uses are parsed).
struct Lazy {
    data: Vec<u8>,
    font: core::cell::OnceCell<Option<Font>>,
}

impl Lazy {
    fn get(&self) -> Option<&Font> {
        self.font.get_or_init(|| Font::from_bytes(self.data.as_slice(), FontSettings::default()).ok()).as_ref()
    }
}

pub struct Fonts {
    faces: Vec<Option<Lazy>>,
    /// Extra faces for characters the chosen face lacks (symbols, CJK).
    fallback: Vec<Font>,
    /// Faces from `@font-face` by family name.
    named: BTreeMap<String, usize>,
    web: Vec<Font>,
    cache: RefCell<BTreeMap<(usize, u32, u32), Glyph>>,
    adv: RefCell<BTreeMap<(usize, u32, u32), f32>>,
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

impl Fonts {
    pub fn new() -> Fonts {
        Fonts {
            faces: (0..12).map(|_| None).collect(),
            fallback: Vec::new(),
            named: BTreeMap::new(),
            web: Vec::new(),
            cache: RefCell::new(BTreeMap::new()),
            adv: RefCell::new(BTreeMap::new()),
        }
    }

    /// Add a TrueType/OpenType face (parsed when first used).
    pub fn add(&mut self, family: Family, bold: bool, italic: bool, data: &[u8]) -> bool {
        if data.len() < 12 {
            return false;
        }
        self.faces[slot(family, bold, italic)] = Some(Lazy { data: data.to_vec(), font: core::cell::OnceCell::new() });
        true
    }

    pub fn add_fallback(&mut self, data: &[u8]) -> bool {
        match Font::from_bytes(data, FontSettings::default()) {
            Ok(f) => {
                self.fallback.push(f);
                true
            }
            Err(_) => false,
        }
    }

    /// An `@font-face` font for `family` (lower-cased).
    pub fn add_named(&mut self, family: &str, data: &[u8]) -> bool {
        match Font::from_bytes(data, FontSettings::default()) {
            Ok(f) => {
                self.web.push(f);
                self.named.insert(family.to_ascii_lowercase(), 100 + self.web.len() - 1);
                true
            }
            Err(_) => false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.faces.iter().all(|f| f.is_none())
    }

    fn font(&self, id: usize) -> Option<&Font> {
        if id >= 200 {
            self.fallback.get(id - 200)
        } else if id >= 100 {
            self.web.get(id - 100)
        } else {
            self.faces.get(id)?.as_ref()?.get()
        }
    }

    /// The face for a style (with fallbacks to regular and to sans).
    pub fn face_for(&self, st: &ComputedStyle) -> usize {
        for fam in &st.font_family {
            let f = fam.to_ascii_lowercase();
            if let Some(&id) = self.named.get(&f) {
                return id;
            }
            let generic = match f.as_str() {
                "monospace" | "courier" | "courier new" | "consolas" | "menlo" | "monaco" | "dejavu sans mono" | "ui-monospace" => Some(Family::Mono),
                "serif" | "times" | "times new roman" | "georgia" | "dejavu serif" | "cambria" => Some(Family::Serif),
                "sans-serif" | "arial" | "helvetica" | "verdana" | "system-ui" | "-apple-system" | "segoe ui" | "roboto" | "dejavu sans" | "ui-sans-serif" | "tahoma" => Some(Family::Sans),
                _ => None,
            };
            if let Some(g) = generic {
                return self.pick(g, st);
            }
        }
        self.pick(Family::Sans, st)
    }

    fn pick(&self, fam: Family, st: &ComputedStyle) -> usize {
        let bold = st.font_weight >= 600;
        let italic = st.font_style != css::style::FontStyle::Normal;
        for (f, b, i) in [(fam, bold, italic), (fam, bold, false), (fam, false, italic), (fam, false, false), (Family::Sans, bold, italic), (Family::Sans, false, false)] {
            let s = slot(f, b, i);
            if self.faces[s].is_some() {
                return s;
            }
        }
        0
    }

    /// The face that has `c`: `id` itself, else a fallback.
    fn face_with(&self, id: usize, c: char) -> usize {
        if let Some(f) = self.font(id) {
            if f.lookup_glyph_index(c) != 0 || c == ' ' {
                return id;
            }
        }
        for (k, f) in self.fallback.iter().enumerate() {
            if f.lookup_glyph_index(c) != 0 {
                return 200 + k;
            }
        }
        if id >= 100 {
            // A web font without the glyph: the regular sans face.
            return 0;
        }
        id
    }

    fn size_key(px: f32) -> u32 {
        (px * 4.0 + 0.5) as u32
    }

    /// Advance of `c` in face `id` at `px`.
    pub fn advance(&self, id: usize, c: char, px: f32) -> f32 {
        let id = self.face_with(id, c);
        let key = (id, c as u32, Self::size_key(px));
        if let Some(&a) = self.adv.borrow().get(&key) {
            return a;
        }
        let a = match self.font(id) {
            Some(f) => f.metrics(c, px).advance_width,
            None => px * 0.5,
        };
        self.adv.borrow_mut().insert(key, a);
        a
    }

    pub fn text_width(&self, id: usize, text: &str, px: f32, letter_spacing: f32) -> f32 {
        let mut w = 0.0;
        let mut prev: Option<char> = None;
        for c in text.chars() {
            w += self.advance(id, c, px) + letter_spacing;
            if let (Some(p), Some(f)) = (prev, self.font(id)) {
                w += f.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            prev = Some(c);
        }
        w
    }

    /// Ascent and descent (positive, px) of face `id`.
    pub fn vmetrics(&self, id: usize, px: f32) -> (f32, f32) {
        match self.font(id).and_then(|f| f.horizontal_line_metrics(px)) {
            Some(m) => (m.ascent, -m.descent),
            None => (px * 0.8, px * 0.2),
        }
    }

    /// Draw `text` with its baseline at (x, y); returns the advance.
    pub fn draw(&self, canvas: &mut Canvas, id: usize, text: &str, px: f32, x: f32, y: f32, color: Rgba, letter_spacing: f32) -> f32 {
        let mut pen = x;
        let mut prev: Option<char> = None;
        for c in text.chars() {
            let fid = self.face_with(id, c);
            if let (Some(p), Some(f)) = (prev, self.font(id)) {
                pen += f.horizontal_kern(p, c, px).unwrap_or(0.0);
            }
            prev = Some(c);
            let key = (fid, c as u32, Self::size_key(px));
            if !self.cache.borrow().contains_key(&key) {
                let g = match self.font(fid) {
                    Some(f) => {
                        let (m, mask) = f.rasterize(c, px);
                        Glyph { xmin: m.xmin, ymin: m.ymin, width: m.width, height: m.height, advance: m.advance_width, mask }
                    }
                    None => Glyph { xmin: 0, ymin: 0, width: 0, height: 0, advance: px * 0.5, mask: Vec::new() },
                };
                let mut cache = self.cache.borrow_mut();
                if cache.len() > 4096 {
                    cache.clear();
                }
                cache.insert(key, g);
            }
            let cache = self.cache.borrow();
            let g = &cache[&key];
            if g.width > 0 && !c.is_whitespace() {
                let gx = (pen + g.xmin as f32).fl() as i32;
                let gy = (y - g.ymin as f32 - g.height as f32).fl() as i32;
                // Round the pen to whole pixels for crisp glyphs.
                let gx = gx + if pen - pen.fl() >= 0.5 { 1 } else { 0 };
                canvas.draw_mask(gx, gy + if y - y.fl() >= 0.5 { 1 } else { 0 }, g.width, g.height, &g.mask, color);
            }
            pen += g.advance + letter_spacing;
        }
        pen - x
    }
}
