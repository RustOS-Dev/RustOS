//! Layout output: positioned fragments (absolute CSS px coordinates).

use crate::geom::Rect;
use crate::page::Target;
use crate::tree::{ReplacedKind, StyleRef};
use alloc::string::String;
use alloc::vec::Vec;
use html::NodeId;

#[derive(Debug, Clone)]
pub enum FragKind {
    /// A box (block, inline box piece on one line, flex/grid/table box).
    Box,
    Text {
        text: String,
        deco: u8,
        baseline: f32,
    },
    Replaced(ReplacedKind),
    Marker(String),
}

#[derive(Debug, Clone)]
pub struct Fragment {
    /// Border box.
    pub rect: Rect,
    pub style: StyleRef,
    pub node: Option<NodeId>,
    pub target: Target,
    pub heading: bool,
    pub kind: FragKind,
    pub children: Vec<Fragment>,
    /// Clip rectangle for descendants (`overflow: hidden/clip`).
    pub clip: Option<Rect>,
}

impl Fragment {
    pub fn new(rect: Rect, style: StyleRef, node: Option<NodeId>, kind: FragKind) -> Fragment {
        Fragment {
            rect,
            style,
            node,
            target: Target::None,
            heading: false,
            kind,
            children: Vec::new(),
            clip: None,
        }
    }

    /// Move this fragment and its descendants.
    pub fn translate(&mut self, dx: f32, dy: f32) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        self.rect = self.rect.translate(dx, dy);
        if let Some(c) = &mut self.clip {
            *c = c.translate(dx, dy);
        }
        if let FragKind::Text { baseline, .. } = &mut self.kind {
            *baseline += dy;
        }
        for c in &mut self.children {
            c.translate(dx, dy);
        }
    }

    /// Bounding box of this fragment and its descendants.
    pub fn extent(&self) -> Rect {
        let mut r = self.rect;
        for c in &self.children {
            r = r.union(&c.extent());
        }
        r
    }

    /// Border boxes of the fragments of `node` (for hit testing and
    /// `getBoundingClientRect`).
    pub fn rects_of(&self, node: NodeId, out: &mut Vec<Rect>) {
        if self.node == Some(node) {
            out.push(self.rect);
        }
        for c in &self.children {
            c.rects_of(node, out);
        }
    }

    /// The deepest fragment with a node at (x, y).
    pub fn hit(&self, x: f32, y: f32) -> Option<NodeId> {
        for c in self.children.iter().rev() {
            if let Some(n) = c.hit(x, y) {
                return Some(n);
            }
        }
        if self.rect.contains(x, y) {
            self.node
        } else {
            None
        }
    }
}
