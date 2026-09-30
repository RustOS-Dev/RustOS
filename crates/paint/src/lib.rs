//! Pixel rendering for the graphical browser (`browse -g`): the same
//! style and layout engines as the text browser (`css`, `layout`), with
//! text measured by real fonts (`fontdue`), painted into an RGBA canvas
//! with images (PNG, JPEG, GIF, BMP), gradients, rounded borders and
//! shadows; screenshots are written as PNG.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod canvas;
pub mod fonts;
pub mod image;
pub mod metrics;
pub mod painter;

#[cfg(test)]
mod tests;

pub use canvas::{Canvas, Clip, Image, Rgba};
pub use fonts::{Family, Fonts};
pub use metrics::PixelMetrics;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use css::Device;
use html::Document;
use layout::fragment::Fragment;
use layout::{DomState, Field, Form, Link};

/// A page laid out in pixels.
pub struct PixelPage {
    pub root: Fragment,
    pub links: Vec<Link>,
    pub fields: Vec<Field>,
    pub forms: Vec<Form>,
    pub link_of: BTreeMap<html::NodeId, usize>,
    pub field_of: BTreeMap<html::NodeId, usize>,
    /// Anchor names (fragment identifiers) and their y positions.
    pub anchors: Vec<(String, f32)>,
    /// Total document height (px).
    pub height: f32,
    pub title: String,
}

/// The device for a window `w`×`h` px.
pub fn device(w: f32, h: f32, scripting: bool) -> Device {
    Device { width: w, height: h, hover: true, pointer: css::Pointer::Fine, color_bits: 8, scripting, ..Device::default() }
}

/// Lay out `doc` for a viewport of `w`×`h` px.
#[allow(clippy::too_many_arguments)]
pub fn layout_page(
    doc: &Document,
    author: &[String],
    fonts: &Fonts,
    image_sizes: &BTreeMap<String, (u32, u32)>,
    resolve: &dyn Fn(&str) -> String,
    w: f32,
    h: f32,
    state: &DomState,
    scripting: bool,
) -> PixelPage {
    let styles = layout::style_set(device(w, h, scripting), author, &mut |_| None);
    let m = PixelMetrics { fonts, images: image_sizes, resolve };
    let (root, controls, anchor_names) = layout::layout(doc, &styles, &m, (w, h), state, false);
    let mut anchors = Vec::new();
    collect_anchors(&root, &anchor_names, &mut anchors);
    let height = root.extent().h.max(h);
    PixelPage {
        root,
        links: controls.links,
        fields: controls.fields,
        forms: controls.forms,
        link_of: controls.link_of,
        field_of: controls.field_of,
        anchors,
        height,
        title: doc.find("title").map(|t| html::collapse_ws(&doc.text_content(t))).unwrap_or_else(|| doc.title.clone()),
    }
}

fn collect_anchors(f: &Fragment, names: &[String], out: &mut Vec<(String, f32)>) {
    if let layout::Target::Anchor(i) = f.target {
        if let Some(n) = names.get(i) {
            out.push((n.clone(), f.rect.y));
        }
    }
    for c in &f.children {
        collect_anchors(c, names, out);
    }
}

/// The page background: the root's or body's background color (CSS
/// background propagation), else white.
pub fn page_background(root: &Fragment) -> Rgba {
    fn find(f: &Fragment, depth: u32) -> Option<Rgba> {
        let c = f.style.background_color_rgba();
        if c.a > 0 {
            return Some(Rgba::new(c.r, c.g, c.b, 255));
        }
        if depth < 3 {
            for ch in &f.children {
                if matches!(ch.kind, layout::FragKind::Box) && ch.node.is_some_and(|_| true) {
                    if let Some(c) = find(ch, depth + 1) {
                        return Some(c);
                    }
                }
            }
        }
        None
    }
    find(root, 0).unwrap_or(Rgba::WHITE)
}
