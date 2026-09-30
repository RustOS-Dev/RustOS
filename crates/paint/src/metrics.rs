//! Layout metrics for a pixel device: text measured with the real fonts,
//! form controls and images sized as a graphical browser draws them.

use crate::fonts::Fonts;
use alloc::collections::BTreeMap;
use alloc::string::String;
use css::ComputedStyle;
use layout::tree::{Replaced, ReplacedKind};
use layout::{Field, FieldKind, Metrics};

pub struct PixelMetrics<'a> {
    pub fonts: &'a Fonts,
    /// Natural sizes of decoded images, by resolved URL.
    pub images: &'a BTreeMap<String, (u32, u32)>,
    /// Resolve an image `src` to the key used in `images`.
    pub resolve: &'a dyn Fn(&str) -> String,
}

/// Inner padding and border of drawn controls (px).
pub const CONTROL_PAD: f32 = 4.0;

impl Metrics for PixelMetrics<'_> {
    fn text_width(&self, text: &str, style: &ComputedStyle) -> f32 {
        let id = self.fonts.face_for(style);
        self.fonts.text_width(id, text, style.font_size, style.letter_spacing)
    }

    fn line_height(&self, style: &ComputedStyle) -> f32 {
        style.line_height_px()
    }

    fn ascent(&self, style: &ComputedStyle) -> f32 {
        let id = self.fonts.face_for(style);
        let (a, d) = self.fonts.vmetrics(id, style.font_size);
        let half_leading = (style.line_height_px() - (a + d)) / 2.0;
        half_leading + a
    }

    fn replaced_size(&self, r: &Replaced, fields: &[Field], style: &ComputedStyle) -> (Option<f32>, Option<f32>) {
        let lh = style.line_height_px();
        match &r.kind {
            ReplacedKind::Image { src, alt } => {
                let key = (self.resolve)(src);
                if let Some(&(w, h)) = self.images.get(&key) {
                    return (Some(w as f32), Some(h as f32));
                }
                if alt.is_empty() {
                    return (Some(0.0), Some(0.0));
                }
                // Broken image: its alt text in a box.
                (Some(self.text_width(alt, style) + 2.0 * CONTROL_PAD), Some(lh))
            }
            ReplacedKind::Field(i) => {
                let Some(f) = fields.get(*i) else { return (Some(0.0), Some(0.0)) };
                let em = style.font_size;
                let text_w = |s: &str| self.text_width(s, style);
                let w = match f.kind {
                    FieldKind::Hidden => return (Some(0.0), Some(0.0)),
                    FieldKind::Checkbox | FieldKind::Radio => return (Some(em * 0.9), Some(em * 0.9)),
                    FieldKind::Submit | FieldKind::Reset | FieldKind::Button | FieldKind::Image => {
                        let label = if !f.label.is_empty() {
                            f.label.clone()
                        } else if !f.value.is_empty() {
                            f.value.clone()
                        } else {
                            String::from(match f.kind {
                                FieldKind::Reset => "Reset",
                                FieldKind::Submit | FieldKind::Image => "Submit",
                                _ => "",
                            })
                        };
                        text_w(&label) + 4.0 * CONTROL_PAD
                    }
                    FieldKind::Select => {
                        let widest = f.options.iter().map(|o| text_w(&o.label)).fold(0.0, f32::max);
                        widest + em + 3.0 * CONTROL_PAD
                    }
                    FieldKind::Textarea => {
                        let cols = f.size.max(8) as f32;
                        return (Some(cols * em * 0.55 + 2.0 * CONTROL_PAD), Some(2.0 * lh + 2.0 * CONTROL_PAD));
                    }
                    _ => f.size.max(4) as f32 * em * 0.55 + 2.0 * CONTROL_PAD,
                };
                (Some(w), Some(lh + 2.0 * CONTROL_PAD))
            }
            ReplacedKind::Placeholder(_) => (Some(300.0), Some(150.0)),
        }
    }
}
