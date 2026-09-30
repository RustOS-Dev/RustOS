//! Rectangles and edge sizes (CSS px, f32).

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.w <= 0.0 && self.h <= 0.0 {
            return *o;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect {
            x,
            y,
            w: self.right().max(o.right()) - x,
            h: self.bottom().max(o.bottom()) - y,
        }
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect {
            x,
            y,
            w: (r - x).max(0.0),
            h: (b - y).max(0.0),
        }
    }
}

/// Sizes of the four sides: top, right, bottom, left.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
    pub fn from_array(a: [f32; 4]) -> Edges {
        Edges {
            top: a[0],
            right: a[1],
            bottom: a[2],
            left: a[3],
        }
    }
    pub fn add(&self, o: &Edges) -> Edges {
        Edges {
            top: self.top + o.top,
            right: self.right + o.right,
            bottom: self.bottom + o.bottom,
            left: self.left + o.left,
        }
    }
}
