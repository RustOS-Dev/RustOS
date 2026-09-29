//! Framebuffer text console: a VT100/ANSI terminal emulator on the UEFI GOP
//! framebuffer.
//!
//! The screen is a grid of character cells (kept in a static array so the
//! console works before the heap exists). Output updates the grid and marks
//! rows dirty; dirty rows are rendered once per `write`. Supported control
//! sequences cover what shells and full-screen tools use: cursor motion,
//! erase, insert/delete, scroll regions, SGR colours (16, 256 and truecolor),
//! reverse video, cursor visibility, save/restore and cursor-position
//! reports.

use crate::sync::Mutex;
use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use core::fmt;

const FONT_8X16: &[u8] = include_bytes!("../../assets/font8x16.bin");
const FONT_WIDTH: usize = 8;
const FONT_HEIGHT: usize = 16;

const MAX_COLS: usize = 480;
const MAX_ROWS: usize = 135;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const WHITE: Color = Color::rgb(0xE5, 0xE5, 0xE5);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const YELLOW: Color = Color::rgb(0xE5, 0xE5, 0x10);
    pub const RED: Color = Color::rgb(0xCD, 0x31, 0x31);
    pub const GREEN: Color = Color::rgb(0x0D, 0xBC, 0x79);
    pub const BLUE: Color = Color::rgb(0x24, 0x72, 0xC8);
    pub const CYAN: Color = Color::rgb(0x11, 0xA8, 0xCD);
    pub const MAGENTA: Color = Color::rgb(0xBC, 0x3F, 0xBC);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b }
    }
}

/// The 16 standard terminal colours.
const PALETTE: [Color; 16] = [
    Color::rgb(0x00, 0x00, 0x00),
    Color::rgb(0xCD, 0x31, 0x31),
    Color::rgb(0x0D, 0xBC, 0x79),
    Color::rgb(0xE5, 0xE5, 0x10),
    Color::rgb(0x24, 0x72, 0xC8),
    Color::rgb(0xBC, 0x3F, 0xBC),
    Color::rgb(0x11, 0xA8, 0xCD),
    Color::rgb(0xE5, 0xE5, 0xE5),
    Color::rgb(0x66, 0x66, 0x66),
    Color::rgb(0xF1, 0x4C, 0x4C),
    Color::rgb(0x23, 0xD1, 0x8B),
    Color::rgb(0xF5, 0xF5, 0x43),
    Color::rgb(0x3B, 0x8E, 0xEA),
    Color::rgb(0xD6, 0x70, 0xD6),
    Color::rgb(0x29, 0xB8, 0xDB),
    Color::rgb(0xFF, 0xFF, 0xFF),
];

fn xterm256(n: u8) -> Color {
    match n {
        0..=15 => PALETTE[n as usize],
        16..=231 => {
            let n = n - 16;
            let lvl = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color::rgb(lvl(n / 36), lvl((n / 6) % 6), lvl(n % 6))
        }
        _ => {
            let v = 8 + (n - 232) * 10;
            Color::rgb(v, v, v)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: u8,
    fg: Color,
    bg: Color,
}

const BLANK: Cell = Cell {
    ch: b' ',
    fg: PALETTE[7],
    bg: PALETTE[0],
};

static mut GRID: [Cell; MAX_COLS * MAX_ROWS] = [BLANK; MAX_COLS * MAX_ROWS];
const ZERO_CELL: Cell = Cell {
    ch: 0,
    fg: Color::rgb(0, 0, 0),
    bg: Color::rgb(0, 0, 0),
};

/// The normal screen, saved while the alternate screen is active (all
/// zero so it lives in .bss).
static mut SAVED_GRID: [Cell; MAX_COLS * MAX_ROWS] = [ZERO_CELL; MAX_COLS * MAX_ROWS];

enum Parse {
    Ground,
    Escape,
    Csi,
    Charset,
    Osc,
}

pub struct FbConsole {
    fb: &'static mut [u8],
    info: FrameBufferInfo,
    cols: usize,
    rows: usize,
    cx: usize,
    cy: usize,
    fg: Color,
    bg: Color,
    default_fg: Color,
    default_bg: Color,
    reverse: bool,
    bold: bool,
    cursor_visible: bool,
    saved: (usize, usize),
    /// Alternate screen active; the normal screen and cursor are saved.
    alt: Option<(usize, usize)>,
    scroll_top: usize,
    scroll_bottom: usize,
    dirty: [bool; MAX_ROWS],
    state: Parse,
    params: [u32; 16],
    nparams: usize,
    private: bool,
    /// Column-80 pending wrap (VT100 semantics).
    wrap_pending: bool,
    utf8_skip: u8,
    /// Replies to device status queries, drained by the TTY.
    pub replies: heapless_reply::Reply,
    /// Last cursor position drawn (to erase it on move).
    drawn_cursor: Option<(usize, usize)>,
}

pub mod heapless_reply {
    /// Tiny fixed buffer for terminal replies (e.g. cursor position reports).
    pub struct Reply {
        pub buf: [u8; 32],
        pub len: usize,
    }
    impl Reply {
        pub const fn new() -> Reply {
            Reply {
                buf: [0; 32],
                len: 0,
            }
        }
        pub fn push(&mut self, s: &[u8]) {
            for &b in s {
                if self.len < self.buf.len() {
                    self.buf[self.len] = b;
                    self.len += 1;
                }
            }
        }
    }
    impl Default for Reply {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub static CONSOLE: Mutex<Option<FbConsole>> = Mutex::new(None);

impl FbConsole {
    #[allow(clippy::deref_addrof)]
    fn grid(&mut self) -> &mut [Cell] {
        // SAFETY: only accessed while holding CONSOLE.
        unsafe { &mut *(&raw mut GRID) }
    }

    fn cell(&mut self, x: usize, y: usize) -> &mut Cell {
        let cols = self.cols;
        &mut self.grid()[y * cols + x]
    }

    pub fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    fn blank(&self) -> Cell {
        Cell {
            ch: b' ',
            fg: self.fg,
            bg: self.bg,
        }
    }

    fn put_pixel_row(&mut self, x: usize, y: usize, bits: u8, fg: Color, bg: Color) {
        let bpp = self.info.bytes_per_pixel;
        let fgb = encode(self.info.pixel_format, fg);
        let bgb = encode(self.info.pixel_format, bg);
        let base = (y * self.info.stride + x) * bpp;
        for col in 0..FONT_WIDTH {
            let px = if (bits >> (7 - col)) & 1 == 1 {
                &fgb
            } else {
                &bgb
            };
            let off = base + col * bpp;
            if off + bpp <= self.fb.len() {
                self.fb[off..off + bpp.min(4)].copy_from_slice(&px[..bpp.min(4)]);
            }
        }
    }

    fn render_cell(&mut self, x: usize, y: usize, invert: bool) {
        let c = *self.cell(x, y);
        let (fg, bg) = if invert { (c.bg, c.fg) } else { (c.fg, c.bg) };
        let glyph = if (32..=126).contains(&c.ch) {
            Some((c.ch - 32) as usize * FONT_HEIGHT)
        } else {
            None
        };
        for row in 0..FONT_HEIGHT {
            let bits = glyph
                .and_then(|g| FONT_8X16.get(g + row).copied())
                .unwrap_or(0);
            self.put_pixel_row(x * FONT_WIDTH, y * FONT_HEIGHT + row, bits, fg, bg);
        }
    }

    fn render(&mut self) {
        if let Some((x, y)) = self.drawn_cursor.take()
            && x < self.cols
            && y < self.rows
        {
            self.dirty[y] = true;
        }
        for y in 0..self.rows {
            if self.dirty[y] {
                self.dirty[y] = false;
                for x in 0..self.cols {
                    self.render_cell(x, y, false);
                }
                // Redraws run with interrupts off; keep other CPUs going.
                crate::arch::x86_64::smp::poll();
            }
        }
        if self.cursor_visible && crate::drivers::console::view_is_live() {
            let (x, y) = (self.cx.min(self.cols - 1), self.cy);
            self.render_cell(x, y, true);
            self.drawn_cursor = Some((x, y));
        }
    }

    /// Enter (`set`) or leave the alternate screen: the normal screen and
    /// cursor are saved on entry and restored on exit.
    #[allow(clippy::deref_addrof)]
    fn alternate_screen(&mut self, set: bool, rows: usize, cols: usize) {
        let n = rows * cols;
        // SAFETY: only accessed while holding CONSOLE.
        let saved = unsafe { &mut *(&raw mut SAVED_GRID) };
        if set {
            if self.alt.is_some() {
                return;
            }
            saved[..n].copy_from_slice(&self.grid()[..n]);
            self.alt = Some((self.cx, self.cy));
            for y in 0..rows {
                self.erase(y, 0, cols);
            }
        } else if let Some((x, y)) = self.alt.take() {
            self.grid()[..n].copy_from_slice(&saved[..n]);
            (self.cx, self.cy) = (x.min(cols - 1), y.min(rows - 1));
            self.wrap_pending = false;
            self.mark_all();
        }
    }

    fn mark_all(&mut self) {
        for d in self.dirty.iter_mut().take(self.rows) {
            *d = true;
        }
    }

    /// Scroll lines [top, bottom] up by `n`.
    fn scroll_up(&mut self, top: usize, bottom: usize, n: usize) {
        let cols = self.cols;
        let blank = self.blank();
        if top == 0 && bottom == self.rows - 1 {
            for y in 0..n.min(self.rows) {
                let mut line = [0u8; MAX_COLS];
                for (x, slot) in line.iter_mut().enumerate().take(cols) {
                    *slot = self.cell(x, y).ch;
                }
                crate::drivers::console::push_history(&line[..cols]);
            }
        }
        let g = self.grid();
        for y in top..=bottom {
            for x in 0..cols {
                g[y * cols + x] = if y + n <= bottom {
                    g[(y + n) * cols + x]
                } else {
                    blank
                };
            }
        }
        for d in self.dirty.iter_mut().take(bottom + 1).skip(top) {
            *d = true;
        }
    }

    fn scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
        let cols = self.cols;
        let blank = self.blank();
        let g = self.grid();
        for y in (top..=bottom).rev() {
            for x in 0..cols {
                g[y * cols + x] = if y >= top + n {
                    g[(y - n) * cols + x]
                } else {
                    blank
                };
            }
        }
        for d in self.dirty.iter_mut().take(bottom + 1).skip(top) {
            *d = true;
        }
    }

    fn newline(&mut self) {
        if self.cy == self.scroll_bottom {
            self.scroll_up(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    fn put_char(&mut self, ch: u8) {
        if self.wrap_pending {
            self.wrap_pending = false;
            self.cx = 0;
            self.newline();
        }
        let (fg, bg) = if self.reverse {
            (self.bg, self.fg)
        } else {
            (self.fg, self.bg)
        };
        let fg = if self.bold && fg == self.default_fg {
            PALETTE[15]
        } else {
            fg
        };
        let (x, y) = (self.cx, self.cy);
        *self.cell(x, y) = Cell { ch, fg, bg };
        self.dirty[y] = true;
        if self.cx + 1 >= self.cols {
            self.wrap_pending = true;
        } else {
            self.cx += 1;
        }
    }

    fn erase(&mut self, y: usize, x0: usize, x1: usize) {
        let blank = self.blank();
        for x in x0..x1.min(self.cols) {
            *self.cell(x, y) = blank;
        }
        self.dirty[y] = true;
    }

    fn param(&self, i: usize, default: u32) -> u32 {
        if i < self.nparams && self.params[i] != 0 {
            self.params[i]
        } else {
            default
        }
    }

    fn sgr(&mut self) {
        if self.nparams == 0 {
            self.nparams = 1;
            self.params[0] = 0;
        }
        let mut i = 0;
        while i < self.nparams {
            let p = self.params[i];
            match p {
                0 => {
                    self.fg = self.default_fg;
                    self.bg = self.default_bg;
                    self.reverse = false;
                    self.bold = false;
                }
                1 => self.bold = true,
                22 => self.bold = false,
                7 => self.reverse = true,
                27 => self.reverse = false,
                30..=37 => self.fg = PALETTE[(p - 30) as usize + if self.bold { 8 } else { 0 }],
                39 => self.fg = self.default_fg,
                40..=47 => self.bg = PALETTE[(p - 40) as usize],
                49 => self.bg = self.default_bg,
                90..=97 => self.fg = PALETTE[(p - 90) as usize + 8],
                100..=107 => self.bg = PALETTE[(p - 100) as usize + 8],
                38 | 48 => {
                    let c = if self.params.get(i + 1) == Some(&5) && i + 2 < self.nparams {
                        let c = xterm256(self.params[i + 2] as u8);
                        i += 2;
                        Some(c)
                    } else if self.params.get(i + 1) == Some(&2) && i + 4 < self.nparams {
                        let c = Color::rgb(
                            self.params[i + 2] as u8,
                            self.params[i + 3] as u8,
                            self.params[i + 4] as u8,
                        );
                        i += 4;
                        Some(c)
                    } else {
                        None
                    };
                    if let Some(c) = c {
                        if p == 38 {
                            self.fg = c;
                        } else {
                            self.bg = c;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn csi(&mut self, fin: u8) {
        let rows = self.rows;
        let cols = self.cols;
        self.wrap_pending = false;
        match fin {
            b'A' => self.cy = self.cy.saturating_sub(self.param(0, 1) as usize),
            b'B' => self.cy = (self.cy + self.param(0, 1) as usize).min(rows - 1),
            b'C' => self.cx = (self.cx + self.param(0, 1) as usize).min(cols - 1),
            b'D' => self.cx = self.cx.saturating_sub(self.param(0, 1) as usize),
            b'E' => {
                self.cy = (self.cy + self.param(0, 1) as usize).min(rows - 1);
                self.cx = 0;
            }
            b'F' => {
                self.cy = self.cy.saturating_sub(self.param(0, 1) as usize);
                self.cx = 0;
            }
            b'G' | b'`' => self.cx = (self.param(0, 1) as usize - 1).min(cols - 1),
            b'd' => self.cy = (self.param(0, 1) as usize - 1).min(rows - 1),
            b'H' | b'f' => {
                self.cy = (self.param(0, 1) as usize - 1).min(rows - 1);
                self.cx = (self.param(1, 1) as usize - 1).min(cols - 1);
            }
            b'J' => {
                let mode = if self.nparams > 0 { self.params[0] } else { 0 };
                let (cx, cy) = (self.cx, self.cy);
                match mode {
                    0 => {
                        self.erase(cy, cx, cols);
                        for y in cy + 1..rows {
                            self.erase(y, 0, cols);
                        }
                    }
                    1 => {
                        for y in 0..cy {
                            self.erase(y, 0, cols);
                        }
                        self.erase(cy, 0, cx + 1);
                    }
                    _ => {
                        for y in 0..rows {
                            self.erase(y, 0, cols);
                        }
                    }
                }
            }
            b'K' => {
                let mode = if self.nparams > 0 { self.params[0] } else { 0 };
                let (cx, cy) = (self.cx, self.cy);
                match mode {
                    0 => self.erase(cy, cx, cols),
                    1 => self.erase(cy, 0, cx + 1),
                    _ => self.erase(cy, 0, cols),
                }
            }
            b'L' if self.cy >= self.scroll_top && self.cy <= self.scroll_bottom => {
                let n = self.param(0, 1) as usize;
                self.scroll_down(self.cy, self.scroll_bottom, n);
            }
            b'M' if self.cy >= self.scroll_top && self.cy <= self.scroll_bottom => {
                let n = self.param(0, 1) as usize;
                self.scroll_up(self.cy, self.scroll_bottom, n);
            }
            b'P' => {
                let n = self.param(0, 1) as usize;
                let (cx, cy) = (self.cx, self.cy);
                let blank = self.blank();
                for x in cx..cols {
                    let v = if x + n < cols {
                        *self.cell(x + n, cy)
                    } else {
                        blank
                    };
                    *self.cell(x, cy) = v;
                }
                self.dirty[cy] = true;
            }
            b'@' => {
                let n = self.param(0, 1) as usize;
                let (cx, cy) = (self.cx, self.cy);
                let blank = self.blank();
                for x in (cx..cols).rev() {
                    let v = if x >= cx + n {
                        *self.cell(x - n, cy)
                    } else {
                        blank
                    };
                    *self.cell(x, cy) = v;
                }
                self.dirty[cy] = true;
            }
            b'X' => {
                let n = self.param(0, 1) as usize;
                let (cx, cy) = (self.cx, self.cy);
                self.erase(cy, cx, cx + n);
            }
            b'S' => {
                let n = self.param(0, 1) as usize;
                self.scroll_up(self.scroll_top, self.scroll_bottom, n);
            }
            b'T' => {
                let n = self.param(0, 1) as usize;
                self.scroll_down(self.scroll_top, self.scroll_bottom, n);
            }
            b'm' => self.sgr(),
            b'r' => {
                let top = self.param(0, 1) as usize - 1;
                let bottom = self.param(1, rows as u32) as usize - 1;
                if top < bottom && bottom < rows {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.cx = 0;
                    self.cy = 0;
                }
            }
            b's' => self.saved = (self.cx, self.cy),
            b'u' => (self.cx, self.cy) = self.saved,
            b'n' => {
                if self.param(0, 0) == 6 {
                    let mut buf = [0u8; 24];
                    let s = fmt_cpr(&mut buf, self.cy + 1, self.cx + 1);
                    self.replies.push(s);
                } else if self.param(0, 0) == 5 {
                    self.replies.push(b"\x1b[0n");
                }
            }
            b'h' | b'l' if self.private => {
                let set = fin == b'h';
                for i in 0..self.nparams {
                    match self.params[i] {
                        25 => self.cursor_visible = set,
                        1049 | 47 | 1047 => self.alternate_screen(set, rows, cols),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn feed(&mut self, b: u8) {
        match self.state {
            Parse::Ground => {
                if self.utf8_skip > 0 {
                    self.utf8_skip -= 1;
                    return;
                }
                match b {
                    0x1B => self.state = Parse::Escape,
                    b'\n' | 0x0B | 0x0C => {
                        self.wrap_pending = false;
                        self.newline();
                    }
                    b'\r' => {
                        self.wrap_pending = false;
                        self.cx = 0;
                    }
                    0x08 => {
                        self.wrap_pending = false;
                        self.cx = self.cx.saturating_sub(1);
                    }
                    b'\t' => {
                        let next = (self.cx / 8 + 1) * 8;
                        self.cx = next.min(self.cols - 1);
                    }
                    0x07 => {}
                    0x20..=0x7E => self.put_char(b),
                    0xC0..=0xDF => {
                        self.utf8_skip = 1;
                        self.put_char(b'?');
                    }
                    0xE0..=0xEF => {
                        self.utf8_skip = 2;
                        self.put_char(b'?');
                    }
                    0xF0..=0xF7 => {
                        self.utf8_skip = 3;
                        self.put_char(b'?');
                    }
                    _ => {}
                }
            }
            Parse::Escape => {
                self.state = Parse::Ground;
                match b {
                    b'[' => {
                        self.state = Parse::Csi;
                        self.nparams = 0;
                        self.params = [0; 16];
                        self.private = false;
                    }
                    b']' => self.state = Parse::Osc,
                    b'(' | b')' => self.state = Parse::Charset,
                    b'7' => self.saved = (self.cx, self.cy),
                    b'8' => (self.cx, self.cy) = self.saved,
                    b'D' => self.newline(),
                    b'E' => {
                        self.cx = 0;
                        self.newline();
                    }
                    b'M' => {
                        if self.cy == self.scroll_top {
                            self.scroll_down(self.scroll_top, self.scroll_bottom, 1);
                        } else {
                            self.cy = self.cy.saturating_sub(1);
                        }
                    }
                    b'c' => self.reset(),
                    _ => {}
                }
            }
            Parse::Charset => self.state = Parse::Ground,
            Parse::Osc => {
                if b == 0x07 || b == 0x1B {
                    self.state = Parse::Ground;
                }
            }
            Parse::Csi => match b {
                b'0'..=b'9' => {
                    if self.nparams == 0 {
                        self.nparams = 1;
                    }
                    let i = self.nparams - 1;
                    if i < 16 {
                        self.params[i] = self.params[i].saturating_mul(10) + (b - b'0') as u32;
                    }
                }
                b';' | b':' => {
                    if self.nparams == 0 {
                        self.nparams = 1;
                    }
                    if self.nparams < 16 {
                        self.nparams += 1;
                    }
                }
                b'?' | b'>' | b'=' => self.private = true,
                0x40..=0x7E => {
                    self.state = Parse::Ground;
                    self.csi(b);
                }
                _ => {}
            },
        }
    }

    fn reset(&mut self) {
        self.fg = self.default_fg;
        self.bg = self.default_bg;
        self.reverse = false;
        self.bold = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.cursor_visible = true;
        self.clear();
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.feed(b);
        }
        if crate::drivers::console::view_is_live() {
            self.render();
        }
    }

    pub fn clear(&mut self) {
        let blank = self.blank();
        for c in self.grid().iter_mut() {
            *c = blank;
        }
        self.cx = 0;
        self.cy = 0;
        self.mark_all();
        self.render();
    }

    pub fn set_colors(&mut self, fg: Color, bg: Color) {
        self.fg = fg;
        self.bg = bg;
    }

    /// Show `lines` of history (oldest first) followed by the live grid,
    /// scrolled back by `offset` lines. `offset == 0` redraws the live view.
    pub fn show_history(&mut self, history: &[&[u8]], offset: usize) {
        if offset == 0 {
            self.mark_all();
            self.render();
            return;
        }
        let rows = self.rows;
        let cols = self.cols;
        let total = history.len() + rows;
        let start = total.saturating_sub(rows + offset);
        for y in 0..rows {
            let idx = start + y;
            for x in 0..cols {
                let ch = if idx < history.len() {
                    history[idx].get(x).copied().unwrap_or(b' ')
                } else {
                    let gy = idx - history.len();
                    self.cell(x, gy).ch
                };
                let fg = PALETTE[7];
                let bg = PALETTE[0];
                let glyph = if (32..=126).contains(&ch) {
                    Some((ch - 32) as usize * FONT_HEIGHT)
                } else {
                    None
                };
                for row in 0..FONT_HEIGHT {
                    let bits = glyph
                        .and_then(|g| FONT_8X16.get(g + row).copied())
                        .unwrap_or(0);
                    self.put_pixel_row(x * FONT_WIDTH, y * FONT_HEIGHT + row, bits, fg, bg);
                }
            }
        }
    }

    /// Take pending terminal replies (e.g. cursor position reports).
    pub fn take_replies(&mut self, out: &mut [u8]) -> usize {
        let n = self.replies.len.min(out.len());
        out[..n].copy_from_slice(&self.replies.buf[..n]);
        self.replies.len = 0;
        n
    }

    /// Raw framebuffer access for /dev/fb0.
    pub fn framebuffer(&mut self) -> (&mut [u8], FrameBufferInfo) {
        (self.fb, self.info)
    }
}

fn fmt_cpr(buf: &mut [u8; 24], row: usize, col: usize) -> &[u8] {
    use core::fmt::Write;
    struct W<'a>(&'a mut [u8; 24], usize);
    impl fmt::Write for W<'_> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            for &b in s.as_bytes() {
                if self.1 < 24 {
                    self.0[self.1] = b;
                    self.1 += 1;
                }
            }
            Ok(())
        }
    }
    let mut w = W(buf, 0);
    let _ = write!(w, "\x1b[{};{}R", row, col);
    let n = w.1;
    &buf[..n]
}

fn encode(fmt: PixelFormat, c: Color) -> [u8; 4] {
    match fmt {
        PixelFormat::Bgr => [c.b, c.g, c.r, 0],
        PixelFormat::U8 => {
            let y = ((c.r as u16 * 3 + c.g as u16 * 6 + c.b as u16) / 10) as u8;
            [y, y, y, 0]
        }
        _ => [c.r, c.g, c.b, 0],
    }
}

impl fmt::Write for FbConsole {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
}

/// Take over the bootloader framebuffer as the console.
///
/// # Safety
/// Must be called once with the bootloader's framebuffer.
pub unsafe fn init(framebuffer: FrameBuffer) {
    let info = framebuffer.info();
    let fb = framebuffer.into_buffer();
    let cols = (info.width / FONT_WIDTH).clamp(1, MAX_COLS);
    let rows = (info.height / FONT_HEIGHT).clamp(1, MAX_ROWS);
    let mut con = FbConsole {
        fb,
        info,
        cols,
        rows,
        cx: 0,
        cy: 0,
        fg: PALETTE[7],
        bg: PALETTE[0],
        default_fg: PALETTE[7],
        default_bg: PALETTE[0],
        reverse: false,
        bold: false,
        cursor_visible: true,
        saved: (0, 0),
        alt: None,
        scroll_top: 0,
        scroll_bottom: rows - 1,
        dirty: [false; MAX_ROWS],
        state: Parse::Ground,
        params: [0; 16],
        nparams: 0,
        private: false,
        wrap_pending: false,
        utf8_skip: 0,
        replies: heapless_reply::Reply::new(),
        drawn_cursor: None,
    };
    con.clear();
    *CONSOLE.lock() = Some(con);
}

/// (columns, rows) of the console, if a framebuffer exists.
pub fn console_size() -> Option<(usize, usize)> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        CONSOLE.lock().as_ref().map(|c| c.size())
    })
}

/// Framebuffer geometry: (width, height, stride, bytes per pixel, BGR?).
pub fn geometry() -> Option<(usize, usize, usize, usize, bool)> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        CONSOLE.lock().as_ref().map(|c| {
            (
                c.info.width,
                c.info.height,
                c.info.stride,
                c.info.bytes_per_pixel,
                matches!(c.info.pixel_format, PixelFormat::Bgr),
            )
        })
    })
}

/// Physical address of the framebuffer (for mmap of /dev/fb0).
pub fn framebuffer_phys() -> Option<(u64, usize)> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        CONSOLE.lock().as_mut().and_then(|c| {
            let virt = c.fb.as_ptr() as u64;
            let len = c.fb.len();
            crate::mm::virt_to_phys(virt).map(|p| (p, len))
        })
    })
}
