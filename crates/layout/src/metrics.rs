//! Text measurement and device policies. Layout is in CSS px; the
//! character-cell device maps one column to 8 px and one row to 16 px.

use crate::page::{Field, field_text, text_width};
use crate::tree::{Replaced, ReplacedKind};
use alloc::format;
use css::ComputedStyle;

pub trait Metrics {
    /// Advance width of `text` (already white-space processed).
    fn text_width(&self, text: &str, style: &ComputedStyle) -> f32;
    /// Height of a line box contributed by an inline of this style.
    fn line_height(&self, style: &ComputedStyle) -> f32;
    /// Distance from the top of the line box contribution to the baseline.
    fn ascent(&self, style: &ComputedStyle) -> f32;
    /// Intrinsic size of a replaced element (None: no intrinsic size).
    fn replaced_size(
        &self,
        r: &Replaced,
        fields: &[Field],
        style: &ComputedStyle,
    ) -> (Option<f32>, Option<f32>);
    /// Used border width of side `side` (0 top, 1 right, 2 bottom, 3 left).
    fn border(&self, style: &ComputedStyle, side: usize, tag: &str) -> f32 {
        let _ = tag;
        style.border_px(side)
    }
    /// Width of a space (for justification and markers).
    fn space_width(&self, style: &ComputedStyle) -> f32 {
        self.text_width(" ", style)
    }
    /// Snap a length to the device grid (identity for pixel devices).
    fn snap_x(&self, v: f32) -> f32 {
        v
    }
    fn snap_y(&self, v: f32) -> f32 {
        v
    }
    fn cell_mode(&self) -> bool {
        false
    }
}

pub const CELL_W: f32 = 8.0;
pub const CELL_H: f32 = 16.0;

/// A terminal: every character is a cell of 8×16 px (wide characters
/// two); every line of text is one row whatever the font size.
pub struct CellMetrics;

impl Metrics for CellMetrics {
    fn text_width(&self, text: &str, _style: &ComputedStyle) -> f32 {
        text_width(text) as f32 * CELL_W
    }
    fn line_height(&self, _style: &ComputedStyle) -> f32 {
        CELL_H
    }
    fn ascent(&self, _style: &ComputedStyle) -> f32 {
        12.0
    }
    fn replaced_size(
        &self,
        r: &Replaced,
        fields: &[Field],
        _style: &ComputedStyle,
    ) -> (Option<f32>, Option<f32>) {
        let cols = match &r.kind {
            ReplacedKind::Image { alt, .. } => {
                if alt.is_empty() {
                    return (Some(0.0), Some(0.0));
                }
                text_width(&format!("[{}]", alt))
            }
            ReplacedKind::Field(i) => fields.get(*i).map_or(0, |f| text_width(&field_text(f))),
            ReplacedKind::Placeholder(s) => {
                if s.is_empty() {
                    return (Some(0.0), Some(0.0));
                }
                text_width(&format!("[{}]", s))
            }
        };
        (Some(cols as f32 * CELL_W), Some(CELL_H))
    }
    fn border(&self, style: &ComputedStyle, side: usize, tag: &str) -> f32 {
        // Borders take no cells, except a horizontal rule's one row.
        if tag == "hr" && side == 0 && style.border_px(0) + style.border_px(2) > 0.0 {
            return CELL_H;
        }
        0.0
    }
    fn snap_x(&self, v: f32) -> f32 {
        crate::roundf(v / CELL_W) * CELL_W
    }
    fn snap_y(&self, v: f32) -> f32 {
        // Vertical space rounds down unless close to a whole row: an 8 px
        // gap takes no row, 12 px and more one.
        css::math::floorf(v / CELL_H + 0.35) * CELL_H
    }
    fn cell_mode(&self) -> bool {
        true
    }
}
