//! The graphical browser (`browse -g`): pages laid out in pixels with real
//! fonts and images (crates/paint), drawn on /dev/fb0 with the console in
//! graphics mode. Keys come from the terminal (lynx-like bindings), the
//! pointer from /dev/input/mice. `-dump-png FILE` renders a page into a
//! PNG instead (for tests and screenshots).

use crate::load::{LoadError, Loader};
use crate::script::{self, Action, Sessions, Ui};
use crate::term::{Key, Term};
use crate::{BatchUi, Entry, document, dom_state, fetch_sheets, use_js};
use alloc::collections::BTreeMap;
use html::NodeId;
use jsproto::Json;
use layout::{FieldKind, Target};
use paint::image::{decode, encode_png};
use paint::painter::{Scene, paint};
use paint::{Canvas, Family, Fonts, Image, PixelPage, Rgba};
use rustos_rt::fs::{self, File};
use rustos_rt::io::{self, POLLIN, PollFd};
use rustos_rt::prelude::*;
use webclient::httpc::cookie::Context;
use webclient::httpc::{Request, Url};

const BAR: u32 = 28;
const STATUS: u32 = 22;
const FONT_DIR: &str = "/usr/share/fonts/dejavu";

fn dbg(what: &str) {
    if rustos_rt::env::var("BROWSE_DEBUG").is_some() {
        eprintln!("[{} ms] {}", rustos_rt::time::millis(), what);
    }
}

pub fn load_fonts() -> Fonts {
    let mut f = Fonts::new();
    for (fam, bold, name) in [
        (Family::Sans, false, "DejaVuSans.ttf"),
        (Family::Sans, true, "DejaVuSans-Bold.ttf"),
        (Family::Serif, false, "DejaVuSerif.ttf"),
        (Family::Serif, true, "DejaVuSerif-Bold.ttf"),
        (Family::Mono, false, "DejaVuSansMono.ttf"),
        (Family::Mono, true, "DejaVuSansMono-Bold.ttf"),
    ] {
        if let Ok(d) = fs::read(&format!("{}/{}", FONT_DIR, name)) {
            f.add(fam, bold, false, &d);
        }
    }
    f
}

// ---------------------------------------------------------------------
// The framebuffer
// ---------------------------------------------------------------------

pub struct Screen {
    fb: File,
    pub w: u32,
    pub h: u32,
    stride: usize,
    bpp: usize,
    bgr: bool,
    row: Vec<u8>,
}

impl Screen {
    pub fn open() -> Option<Screen> {
        let fb = File::open_with("/dev/fb0", 2, 0).ok()?;
        let mut v = [0u32; 40];
        fb.ioctl(0x4600, v.as_mut_ptr() as usize).ok()?;
        let mut f = [0u8; 80];
        fb.ioctl(0x4602, f.as_mut_ptr() as usize).ok()?;
        let (w, h, bits) = (v[0], v[1], v[6]);
        // red.offset is v[8]; line_length is at byte 48 of fix info.
        let red_off = v[8];
        let stride = u32::from_le_bytes([f[48], f[49], f[50], f[51]]) as usize;
        if w == 0 || h == 0 || bits < 24 {
            return None;
        }
        let bpp = bits as usize / 8;
        Some(Screen {
            fb,
            w,
            h,
            stride,
            bpp,
            bgr: red_off == 16,
            row: vec![0; w as usize * bpp],
        })
    }

    /// Copy `c` to the screen at (0, y0), in one write when rows are
    /// contiguous.
    pub fn blit(&mut self, c: &Canvas, y0: u32) {
        let w = c.width.min(self.w) as usize;
        let rows = c.height.min(self.h.saturating_sub(y0)) as usize;
        let line = w * self.bpp;
        let contiguous = line == self.stride;
        if contiguous {
            self.row.resize(line * rows, 0);
        }
        for y in 0..rows {
            let src = &c.pixels[y * c.width as usize..y * c.width as usize + w];
            let base = if contiguous { y * line } else { 0 };
            let dst = &mut self.row[base..base + line];
            if self.bpp == 4 {
                for (d, p) in dst.chunks_exact_mut(4).zip(src) {
                    let (a, b) = if self.bgr { (p.b, p.r) } else { (p.r, p.b) };
                    d[0] = a;
                    d[1] = p.g;
                    d[2] = b;
                    d[3] = 0;
                }
            } else {
                for (d, p) in dst.chunks_exact_mut(3).zip(src) {
                    let (a, b) = if self.bgr { (p.b, p.r) } else { (p.r, p.b) };
                    d[0] = a;
                    d[1] = p.g;
                    d[2] = b;
                }
            }
            if !contiguous {
                let off = (y0 as usize + y) as u64 * self.stride as u64;
                let _ = self.fb.write_at(off, &self.row[..line]);
            }
        }
        if contiguous {
            // Writes may be short (the kernel copies in bounded chunks).
            let mut off = 0;
            let total = line * rows;
            while off < total {
                match self.fb.write_at(
                    y0 as u64 * self.stride as u64 + off as u64,
                    &self.row[off..total],
                ) {
                    Ok(n) if n > 0 => off += n,
                    _ => break,
                }
            }
        }
    }
}

/// Console graphics mode (the text console stops drawing).
fn kd_mode(graphics: bool) {
    let _ = rustos_rt::sys::syscall(rustos_rt::sys::nr::IOCTL, &[0, 0x4B3A, graphics as usize]);
}

// ---------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------

/// The pixel view of a history entry.
pub struct View {
    pub page: Option<PixelPage>,
    pub images: BTreeMap<String, Image>,
    pub sizes: BTreeMap<String, (u32, u32)>,
    pub scroll: f32,
    pub focus: Option<usize>,
    pub hover: Option<Target>,
}

impl View {
    fn new() -> View {
        View {
            page: None,
            images: BTreeMap::new(),
            sizes: BTreeMap::new(),
            scroll: 0.0,
            focus: None,
            hover: None,
        }
    }
}

fn base_of(e: &Entry) -> Url {
    let u = e.loaded.url.clone();
    match &e.doc.base {
        Some(b) => u.join(b).unwrap_or(u),
        None => u,
    }
}

/// Fetch the page's images (`<img src>`, `<input type=image>`, CSS
/// background URLs in `style` attributes), at most 40 and 4 MiB each.
fn fetch_images(e: &Entry, v: &mut View, loader: &mut Loader) {
    let base = base_of(e);
    let mut n = 0;
    for id in e.doc.descendants(0) {
        let tag = e.doc.tag(id);
        let src = match tag {
            "img" => e.doc.attr(id, "src"),
            "input"
                if e.doc
                    .attr(id, "type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("image")) =>
            {
                e.doc.attr(id, "src")
            }
            _ => None,
        };
        let mut urls: Vec<String> = src.map(|s| String::from(s.trim())).into_iter().collect();
        if let Some(st) = e.doc.attr(id, "style") {
            if let Some(i) = st.find("url(") {
                let rest = &st[i + 4..];
                if let Some(j) = rest.find(')') {
                    urls.push(
                        rest[..j]
                            .trim()
                            .trim_matches(|c| c == '"' || c == '\'')
                            .to_string(),
                    );
                }
            }
        }
        for s in urls {
            let Ok(u) = base.join(&s) else { continue };
            let key = u.to_string();
            if v.images.contains_key(&key) || n >= 40 {
                continue;
            }
            n += 1;
            if let Some((data, _)) = loader.fetch_bytes(&u, &e.loaded.url, 4 << 20) {
                if let Some(d) = decode(&data) {
                    let img = d.frames.into_iter().next().unwrap().0;
                    v.sizes.insert(key.clone(), (img.width, img.height));
                    v.images.insert(key, img);
                }
            }
        }
    }
}

/// Fetch the page's `@font-face` fonts (TTF/OTF/WOFF) into `fonts`.
pub fn load_web_fonts(e: &mut Entry, loader: &mut Loader, fonts: &mut Fonts, w: u32, h: u32) {
    let cols = (w / 8) as usize;
    let key = crate::sheet_key(&e.doc, cols);
    if key != e.sheet_key {
        e.sheets = fetch_sheets(loader, &e.loaded, &e.doc, cols);
        e.sheet_key = key;
    }
    let set = layout::style_set(
        paint::device(w as f32, h as f32, false),
        &e.sheets,
        &mut |_| None,
    );
    let base = base_of(e);
    let mut n = 0;
    for face in &set.font_faces {
        for src in &face.sources {
            if n >= 8 {
                return;
            }
            let Ok(u) = base.join(src) else { continue };
            n += 1;
            if let Some((data, _)) = loader.fetch_bytes(&u, &e.loaded.url, 8 << 20) {
                if let Some(sfnt) = paint::image::font_file(&data) {
                    if fonts.add_named(&face.family, &sfnt) {
                        break;
                    }
                }
            }
        }
    }
}

/// Lay the entry out for a `w`×`h` viewport.
pub fn layout(e: &mut Entry, v: &mut View, loader: &mut Loader, fonts: &Fonts, w: u32, h: u32) {
    let cols = (w / 8) as usize;
    let key = crate::sheet_key(&e.doc, cols);
    if key != e.sheet_key {
        e.sheets = fetch_sheets(loader, &e.loaded, &e.doc, cols);
        e.sheet_key = key;
    }
    let base = base_of(e);
    let resolve = move |s: &str| {
        base.join(s.trim())
            .map(|u| u.to_string())
            .unwrap_or_default()
    };
    let state = dom_state(e);
    let page = paint::layout_page(
        &e.doc,
        &e.sheets,
        fonts,
        &v.sizes,
        &resolve,
        w as f32,
        h as f32,
        &state,
        e.scripting(),
    );
    // The script bridge uses the entry's fields (node ids, values).
    e.fields = page.fields.clone();
    e.dirty = false;
    v.page = Some(page);
}

/// Focusable targets in reading order with their first box.
fn focusables(p: &PixelPage) -> Vec<(Target, layout::geom::Rect)> {
    let mut out = Vec::new();
    for (i, l) in p.links.iter().enumerate() {
        let mut r = Vec::new();
        p.root.rects_of(l.node, &mut r);
        if let Some(r) = r.into_iter().find(|r| r.w > 0.0) {
            out.push((Target::Link(i), r));
        }
    }
    for (i, f) in p.fields.iter().enumerate() {
        if f.kind == FieldKind::Hidden {
            continue;
        }
        let mut r = Vec::new();
        p.root.rects_of(f.node, &mut r);
        if let Some(r) = r.into_iter().find(|r| r.w > 0.0) {
            out.push((Target::Field(i), r));
        }
    }
    out.sort_by(|a, b| (a.1.y as i64, a.1.x as i64).cmp(&(b.1.y as i64, b.1.x as i64)));
    out
}

/// Render the page into a canvas of `w`×`h` starting at the view's scroll.
pub fn render(v: &View, fonts: &Fonts, e: &Entry, w: u32, h: u32) -> Canvas {
    let Some(p) = &v.page else {
        return Canvas::new(w, h, Rgba::WHITE);
    };
    let mut c = Canvas::new(w, h, paint::page_background(&p.root));
    let base = base_of(e);
    let resolve = move |s: &str| {
        base.join(s.trim())
            .map(|u| u.to_string())
            .unwrap_or_default()
    };
    let focus = v.focus.and_then(|i| focusables(p).get(i).map(|x| x.0));
    let scene = Scene {
        fonts,
        fields: &e.fields,
        images: &v.images,
        resolve: &resolve,
        scroll: (0.0, v.scroll),
        focus,
        hover: v.hover,
    };
    paint(&mut c, &p.root, &scene);
    c
}

// ---------------------------------------------------------------------
// Batch: -dump-png
// ---------------------------------------------------------------------

pub fn dump_png(url: Url, insecure: bool, out: &str, w: u32, h: u32, full: bool) -> i32 {
    let mut loader = Loader::new(insecure);
    let mut sessions = Sessions::default();
    let mut req = Request::get(url);
    for _ in 0..6 {
        let l = match loader.fetch(req.clone(), Context::USER) {
            Ok(l) => l,
            Err(LoadError::Certificate(_, e)) | Err(LoadError::Other(e)) => {
                eprintln!("browse: {}", e);
                return 1;
            }
        };
        let doc = document(&l, false);
        let mut e = Entry::new(l, doc, (w / 8) as usize);
        let mut next = None;
        if use_js() && e.loaded.mime.contains("html") && crate::js::available() {
            next = crate::batch_scripts(&mut e, &mut loader, &mut sessions, 5000);
        }
        match next {
            Some(Action::Navigate { url, .. })
                if url.without_fragment() != e.loaded.url.without_fragment() =>
            {
                req = Request::get(url);
                continue;
            }
            Some(Action::Submit(r)) => {
                req = r;
                continue;
            }
            _ => {}
        }
        // (Fonts are loaded after jsd started: forking with them is slow.)
        let mut fonts = load_fonts();
        if fonts.is_empty() {
            eprintln!("browse: no fonts in {}", FONT_DIR);
            return 1;
        }
        load_web_fonts(&mut e, &mut loader, &mut fonts, w, h);
        let mut v = View::new();
        fetch_images(&e, &mut v, &mut loader);
        layout(&mut e, &mut v, &mut loader, &fonts, w, h);
        // Canvases drawn by scripts.
        canvases(
            &mut e,
            &mut v,
            &mut loader,
            &mut sessions,
            &mut BatchUi,
            &fonts,
            w,
            h,
        );
        let height = if full {
            v.page.as_ref().map_or(h as f32, |p| p.height).min(8000.0) as u32
        } else {
            h
        };
        let c = render(&v, &fonts, &e, w, height.max(1));
        let png = encode_png(&c.to_image());
        if let Err(err) = fs::write(out, &png) {
            eprintln!("browse: {}: {}", out, err);
            return 1;
        }
        println!(
            "{}: {}x{} ({})",
            out,
            w,
            height,
            v.page.as_ref().map_or(String::new(), |p| p.title.clone())
        );
        return 0;
    }
    1
}

/// Ask the page's scripts for the pixels of its `<canvas>` elements
/// (drawn with the 2D context in jsd) and treat them as images.
#[allow(clippy::too_many_arguments)]
fn canvases(
    e: &mut Entry,
    v: &mut View,
    loader: &mut Loader,
    sessions: &mut Sessions,
    ui: &mut dyn Ui,
    fonts: &Fonts,
    w: u32,
    h: u32,
) {
    if !e.scripting() {
        return;
    }
    let ids: Vec<NodeId> = e
        .doc
        .descendants(0)
        .into_iter()
        .filter(|&n| e.doc.tag(n) == "canvas")
        .collect();
    if ids.is_empty() {
        return;
    }
    let code = "JSON.stringify(Array.from(document.querySelectorAll('canvas')).map(c => [c.__id, c.width, c.height, c._pixels ? __jsd_b64(c._pixels) : '']))";
    let r = script::eval(e, loader, sessions, ui, code);
    let Ok(Json::Arr(list)) = Json::parse(&r) else {
        return;
    };
    let mut changed = false;
    for item in list {
        let Some(a) = item.as_arr() else { continue };
        let (Some(id), Some(w), Some(h), Some(b64)) = (
            a.first().and_then(|x| x.as_i64()),
            a.get(1).and_then(|x| x.as_i64()),
            a.get(2).and_then(|x| x.as_i64()),
            a.get(3).and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        if b64.is_empty() || w <= 0 || h <= 0 {
            continue;
        }
        let bytes = script::b64d(b64);
        let (w, h) = (w as u32, h as u32);
        if bytes.len() < (w * h * 4) as usize {
            continue;
        }
        let mut img = Image::new(w, h);
        for (i, px) in img.pixels.iter_mut().enumerate() {
            let p = &bytes[i * 4..i * 4 + 4];
            *px = Rgba::new(p[0], p[1], p[2], p[3]);
        }
        // Painted as the canvas element's image: a synthetic src.
        let key = format!("canvas:{}", id);
        e.doc.set_attr(id as NodeId, "src", Some(&key));
        v.sizes.insert(key.clone(), (w, h));
        v.images.insert(key, img);
        changed = true;
    }
    if changed {
        layout(e, v, loader, fonts, w, h);
    }
}

// ---------------------------------------------------------------------
// Interactive
// ---------------------------------------------------------------------

struct GfxUi<'a> {
    term: &'a mut Term,
    notice: &'a mut String,
}

impl Ui for GfxUi<'_> {
    fn alert(&mut self, text: &str) {
        *self.notice = format!("Alert: {}", text);
    }
    fn confirm(&mut self, text: &str) -> bool {
        *self.notice = format!("Confirm: {} (answered yes)", text);
        true
    }
    fn prompt(&mut self, text: &str, default: &str) -> Option<String> {
        let _ = &self.term;
        *self.notice = format!("Prompt: {} (default used)", text);
        Some(default.to_string())
    }
}

pub struct Gfx {
    screen: Screen,
    term: Term,
    loader: Loader,
    fonts: Fonts,
    hist: Vec<(Entry, View)>,
    cur: usize,
    sessions: Sessions,
    msg: String,
    mouse: Option<File>,
    pointer: (i32, i32),
    /// Edit prompt shown in the status bar: (label, text).
    editing: Option<(String, String)>,
    /// Something changed since the last draw.
    redraw: bool,
}

impl Gfx {
    fn page_h(&self) -> u32 {
        self.screen.h - BAR - STATUS
    }

    fn draw(&mut self) {
        dbg("draw");
        let (w, h) = (self.screen.w, self.screen.h);
        let ph = self.page_h();
        if let Some((e, v)) = self.hist.get_mut(self.cur) {
            if e.dirty || v.page.is_none() {
                layout(e, v, &mut self.loader, &self.fonts, w, ph);
            }
        }
        // Top bar.
        let mut bar = Canvas::new(w, BAR, Rgba::new(232, 234, 237, 255));
        let (title, url) = match self.hist.get(self.cur) {
            Some((e, v)) => (
                v.page.as_ref().map(|p| p.title.clone()).unwrap_or_default(),
                e.loaded.url.to_string(),
            ),
            None => (String::new(), String::new()),
        };
        bar.fill_round_rect(
            6.0,
            4.0,
            w as f32 - 12.0,
            BAR as f32 - 8.0,
            [10.0; 4],
            Rgba::WHITE,
        );
        let st = css::ComputedStyle::default();
        let id = self.fonts.face_for(&st);
        let js = if self.hist.get(self.cur).is_some_and(|(e, _)| e.scripting()) {
            "  [JS]"
        } else {
            ""
        };
        let label = if title.is_empty() {
            format!("{}{}", url, js)
        } else {
            format!("{}  —  {}{}", title, url, js)
        };
        self.fonts.draw(
            &mut bar,
            id,
            &label,
            14.0,
            16.0,
            19.0,
            Rgba::new(32, 33, 36, 255),
            0.0,
        );
        self.screen.blit(&bar, 0);
        // Page.
        let page = match self.hist.get(self.cur) {
            Some((e, v)) => render(v, &self.fonts, e, w, ph),
            None => Canvas::new(w, ph, Rgba::WHITE),
        };
        let mut page = page;
        // Pointer.
        let (px, py) = (self.pointer.0 as f32, self.pointer.1 as f32 - BAR as f32);
        if py >= 0.0 {
            page.fill_polygon(
                &[
                    (px, py),
                    (px, py + 16.0),
                    (px + 4.5, py + 12.0),
                    (px + 11.0, py + 12.0),
                ],
                Rgba::BLACK,
                true,
            );
            page.fill_polygon(
                &[
                    (px + 1.5, py + 3.5),
                    (px + 1.5, py + 12.5),
                    (px + 4.5, py + 10.0),
                    (px + 8.0, py + 10.0),
                ],
                Rgba::WHITE,
                true,
            );
        }
        self.screen.blit(&page, BAR);
        // Status bar.
        let mut sb = Canvas::new(w, STATUS, Rgba::new(245, 245, 245, 255));
        sb.fill_rect(0.0, 0.0, w as f32, 1.0, Rgba::new(210, 210, 210, 255));
        let status = match &self.editing {
            Some((l, t)) => format!("{}: {}_", l, t),
            None if !self.msg.is_empty() => self.msg.clone(),
            None => self.describe_focus(),
        };
        self.fonts.draw(
            &mut sb,
            id,
            &status,
            13.0,
            8.0,
            16.0,
            Rgba::new(60, 64, 67, 255),
            0.0,
        );
        self.screen.blit(&sb, h - STATUS);
        dbg("drawn");
    }

    fn describe_focus(&self) -> String {
        let Some((e, v)) = self.hist.get(self.cur) else {
            return String::new();
        };
        let Some(p) = &v.page else {
            return String::new();
        };
        match v.focus.and_then(|i| focusables(p).get(i).map(|x| x.0)) {
            Some(Target::Link(i)) => {
                let l = &p.links[i];
                if l.href.is_empty() {
                    format!("[*] {} (click: Enter)", l.text)
                } else {
                    base_of(e)
                        .join(&l.href)
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| l.href.clone())
                }
            }
            Some(Target::Field(i)) => format!(
                "{:?} field {} — Enter to use",
                p.fields[i].kind, p.fields[i].name
            ),
            _ => String::from(
                "Tab/arrows: move  Enter: follow  Space: page down  g: go  Left: back  q: quit",
            ),
        }
    }

    fn open(&mut self, req: Request, push: bool) {
        self.msg = format!("Loading {} ...", req.url);
        self.draw();
        let l = match self.loader.fetch(req, Context::USER) {
            Ok(l) => l,
            Err(LoadError::Certificate(_, e)) | Err(LoadError::Other(e)) => {
                self.msg = format!("Error: {}", e);
                return;
            }
        };
        self.leave();
        let referrer = self
            .hist
            .get(self.cur)
            .map(|(e, _)| e.loaded.url.to_string())
            .unwrap_or_default();
        let fragment = l.url.fragment.clone();
        let doc = document(&l, false);
        let mut e = Entry::new(l, doc, (self.screen.w / 8) as usize);
        e.referrer = referrer.clone();
        let mut v = View::new();
        let (cols, rows) = ((self.screen.w / 8) as usize, (self.page_h() / 16) as usize);
        if use_js()
            && e.loaded.mime.contains("html")
            && crate::js::available()
            && script::start(&mut e, &referrer, cols, rows)
        {
            let mut notice = String::new();
            let mut ui = GfxUi {
                term: &mut self.term,
                notice: &mut notice,
            };
            dbg("scripts started");
            script::pump(
                &mut e,
                &mut self.loader,
                &mut self.sessions,
                &mut ui,
                &mut |m| m.str("t") == Some("loaded"),
                false,
                10_000,
            );
            dbg("loaded");
            script::pump(
                &mut e,
                &mut self.loader,
                &mut self.sessions,
                &mut ui,
                &mut |_| false,
                true,
                300,
            );
            if !notice.is_empty() {
                self.msg = notice;
            }
        }
        dbg("scripts idle");
        let ph0 = self.page_h();
        load_web_fonts(
            &mut e,
            &mut self.loader,
            &mut self.fonts,
            self.screen.w,
            ph0,
        );
        fetch_images(&e, &mut v, &mut self.loader);
        dbg("images");
        let ph = self.page_h();
        layout(
            &mut e,
            &mut v,
            &mut self.loader,
            &self.fonts,
            self.screen.w,
            ph,
        );
        dbg("layout");
        let mut ui = BatchUi;
        canvases(
            &mut e,
            &mut v,
            &mut self.loader,
            &mut self.sessions,
            &mut ui,
            &self.fonts,
            self.screen.w,
            ph,
        );
        dbg("canvases");
        if let (Some(f), Some(p)) = (fragment, &v.page) {
            if let Some((_, y)) = p.anchors.iter().find(|(n, _)| *n == f) {
                v.scroll = *y;
            }
        }
        if push {
            self.hist.truncate(self.cur + 1);
            self.hist.push((e, v));
            self.cur = self.hist.len() - 1;
        } else if self.cur < self.hist.len() {
            self.hist[self.cur] = (e, v);
        } else {
            self.hist.push((e, v));
            self.cur = self.hist.len() - 1;
        }
        if self.msg.starts_with("Loading") {
            self.msg.clear();
        }
        self.after_script();
    }

    fn leave(&mut self) {
        if let Some((e, _)) = self.hist.get_mut(self.cur) {
            if e.script.is_some() {
                let mut ui = BatchUi;
                script::unload(e, &mut self.loader, &mut self.sessions, &mut ui);
                e.script = None;
                e.sockets.clear();
            }
        }
    }

    fn after_script(&mut self) {
        let Some((e, v)) = self.hist.get_mut(self.cur) else {
            return;
        };
        let Some(s) = e.script.as_mut() else { return };
        if let Some(n) = s.notice.take() {
            self.msg = n;
        }
        if let Some(y) = s.scroll.take() {
            v.scroll = y.max(0.0);
        }
        let action = if s.actions.is_empty() {
            None
        } else {
            Some(s.actions.remove(0))
        };
        s.actions.clear();
        match action {
            Some(Action::Navigate { url, replace }) => self.open(Request::get(url), !replace),
            Some(Action::Submit(req)) => self.open(req, true),
            Some(Action::Go(d)) => {
                let to = self.cur as i64 + d as i64;
                if to >= 0 && (to as usize) < self.hist.len() {
                    self.cur = to as usize;
                }
            }
            _ => {}
        }
    }

    fn scroll_by(&mut self, dy: f32) {
        let ph = self.page_h() as f32;
        if let Some((_, v)) = self.hist.get_mut(self.cur) {
            let max = v.page.as_ref().map_or(0.0, |p| (p.height - ph).max(0.0));
            v.scroll = (v.scroll + dy).clamp(0.0, max);
        }
    }

    fn move_focus(&mut self, down: bool) {
        let ph = self.page_h() as f32;
        let Some((_, v)) = self.hist.get_mut(self.cur) else {
            return;
        };
        let Some(p) = &v.page else { return };
        let f = focusables(p);
        if f.is_empty() {
            return;
        }
        let next = match (v.focus, down) {
            (None, true) => f.iter().position(|x| x.1.y >= v.scroll).unwrap_or(0),
            (None, false) => f.len() - 1,
            (Some(i), true) => (i + 1).min(f.len() - 1),
            (Some(i), false) => i.saturating_sub(1),
        };
        v.focus = Some(next);
        let r = f[next].1;
        if r.y < v.scroll || r.y + r.h > v.scroll + ph {
            v.scroll = (r.y - ph / 3.0).max(0.0);
        }
    }

    /// Activate the focused target (or the one under the pointer).
    fn activate(&mut self, t: Target) {
        let Some((e, v)) = self.hist.get_mut(self.cur) else {
            return;
        };
        let Some(p) = &v.page else { return };
        match t {
            Target::Link(i) => {
                let l = p.links[i].clone();
                if e.scripting() {
                    let mut ui = BatchUi;
                    script::event(
                        e,
                        &mut self.loader,
                        &mut self.sessions,
                        &mut ui,
                        "click",
                        l.node,
                    );
                    self.after_script();
                    return;
                }
                if let Ok(u) = base_of(e).join(&l.href) {
                    self.open(Request::get(u), true);
                }
            }
            Target::Field(i) => {
                let f = p.fields[i].clone();
                if f.disabled {
                    self.msg = String::from("This field is disabled");
                    return;
                }
                let js = e.scripting();
                if js {
                    let mut ui = BatchUi;
                    if script::event(
                        e,
                        &mut self.loader,
                        &mut self.sessions,
                        &mut ui,
                        "click",
                        f.node,
                    ) == Some(true)
                    {
                        return;
                    }
                }
                match f.kind {
                    FieldKind::Text
                    | FieldKind::Password
                    | FieldKind::Textarea
                    | FieldKind::File => {
                        let label = if f.label.is_empty() {
                            f.name.clone()
                        } else {
                            f.label.clone()
                        };
                        if let Some(val) = self.edit(&label, &f.value) {
                            let (e, _) = &mut self.hist[self.cur];
                            e.overrides.entry(f.node).or_default().value = Some(val.clone());
                            e.dirty = true;
                            if js {
                                script::input(
                                    e,
                                    f.node,
                                    Json::obj([("value", Json::from(val.as_str()))]),
                                );
                                let mut ui = BatchUi;
                                script::pump(
                                    e,
                                    &mut self.loader,
                                    &mut self.sessions,
                                    &mut ui,
                                    &mut |_| false,
                                    true,
                                    500,
                                );
                            }
                        }
                    }
                    FieldKind::Checkbox | FieldKind::Radio if !js => {
                        let o = e.overrides.entry(f.node).or_default();
                        o.checked = Some(if f.kind == FieldKind::Radio {
                            true
                        } else {
                            !f.checked
                        });
                        if f.kind == FieldKind::Radio {
                            for x in p.fields.iter().filter(|x| {
                                x.kind == FieldKind::Radio
                                    && x.name == f.name
                                    && x.node != f.node
                                    && x.form == f.form
                            }) {
                                e.overrides.entry(x.node).or_default().checked = Some(false);
                            }
                        }
                        e.dirty = true;
                    }
                    FieldKind::Select => {
                        let n = (f.selected + 1) % f.options.len().max(1);
                        e.overrides.entry(f.node).or_default().selected = Some(n);
                        e.dirty = true;
                        if js {
                            script::input(e, f.node, Json::obj([("index", Json::from(n))]));
                        }
                        self.msg = format!(
                            "{}: {}",
                            f.name,
                            f.options.get(n).map(|o| o.label.as_str()).unwrap_or("")
                        );
                    }
                    FieldKind::Submit | FieldKind::Image if !js => {
                        if let Some(fi) = f.form {
                            let fields: Vec<layout::Field> = p.fields.clone();
                            let base = base_of(e);
                            match crate::form::submission(&p.forms, &fields, fi, Some(i), &base) {
                                Ok(req) => self.open(req, true),
                                Err(err) => self.msg = format!("Cannot submit: {}", err),
                            }
                        }
                    }
                    _ => {}
                }
                self.after_script();
            }
            _ => {}
        }
    }

    /// Edit text in the status bar (Enter accepts, Esc cancels).
    fn edit(&mut self, label: &str, initial: &str) -> Option<String> {
        self.editing = Some((label.to_string(), initial.to_string()));
        let r = loop {
            self.draw();
            match self.term.key(-1) {
                Some(Key::Enter) => break self.editing.take().map(|e| e.1),
                Some(Key::Esc) | None => break None,
                Some(Key::Backspace) => {
                    if let Some((_, t)) = &mut self.editing {
                        t.pop();
                    }
                }
                Some(Key::Ctrl('u')) => {
                    if let Some((_, t)) = &mut self.editing {
                        t.clear();
                    }
                }
                Some(Key::Char(c)) => {
                    if let Some((_, t)) = &mut self.editing {
                        t.push(c);
                    }
                }
                _ => {}
            }
        };
        self.editing = None;
        r
    }

    fn click_at(&mut self, x: i32, y: i32) {
        let Some((_, v)) = self.hist.get(self.cur) else {
            return;
        };
        let Some(p) = &v.page else { return };
        let (px, py) = (x as f32, y as f32 - BAR as f32 + v.scroll);
        let Some(node) = p.root.hit(px, py) else {
            return;
        };
        // The link or control containing the node.
        let mut n = Some(node);
        let doc = &self.hist[self.cur].0.doc;
        while let Some(id) = n {
            if let Some(&l) = p.link_of.get(&id) {
                let t = Target::Link(l);
                if let Some(i) = focusables(p).iter().position(|f| f.0 == t) {
                    self.hist[self.cur].1.focus = Some(i);
                }
                return self.activate(t);
            }
            if let Some(&f) = p.field_of.get(&id) {
                let t = Target::Field(f);
                if let Some(i) = focusables(p).iter().position(|x| x.0 == t) {
                    self.hist[self.cur].1.focus = Some(i);
                }
                return self.activate(t);
            }
            n = doc.nodes.get(id).and_then(|x| x.parent);
        }
    }

    fn read_mouse(&mut self) -> Option<bool> {
        let m = self.mouse.as_ref()?;
        let mut b = [0u8; 64];
        let n = m.read(&mut b).ok()?;
        let mut clicked = false;
        for p in b[..n].chunks(3) {
            if p.len() < 3 {
                break;
            }
            let dx = p[1] as i8 as i32;
            let dy = p[2] as i8 as i32;
            self.pointer.0 = (self.pointer.0 + dx).clamp(0, self.screen.w as i32 - 1);
            self.pointer.1 = (self.pointer.1 - dy).clamp(0, self.screen.h as i32 - 1);
            if p[0] & 1 != 0 {
                clicked = true;
            }
        }
        Some(clicked)
    }

    fn run(&mut self) {
        let mut was_down = false;
        loop {
            let dirty = self.hist.get(self.cur).is_some_and(|(e, _)| e.dirty);
            if self.redraw || dirty {
                self.draw();
                self.redraw = false;
            }
            // Wait for keys, the mouse, or the page's scripts.
            let mut fds = vec![PollFd {
                fd: 0,
                events: POLLIN,
                revents: 0,
            }];
            if let Some(m) = &self.mouse {
                fds.push(PollFd {
                    fd: m.fd(),
                    events: POLLIN,
                    revents: 0,
                });
            }
            let js_fd = self
                .hist
                .get(self.cur)
                .and_then(|(e, _)| e.script.as_ref().filter(|s| !s.js.dead).map(|s| s.js.fd()));
            if let Some(fd) = js_fd {
                fds.push(PollFd {
                    fd,
                    events: POLLIN,
                    revents: 0,
                });
            }
            if !self.term.has_pending() {
                let _ = io::poll(&mut fds, 100);
            }
            let js_ready = fds
                .last()
                .is_some_and(|f| js_fd == Some(f.fd) && f.revents != 0);
            if let Some((e, _)) = self.hist.get_mut(self.cur) {
                crate::sockets::service(e);
                let mut ui = BatchUi;
                script::drain(e, &mut self.loader, &mut self.sessions, &mut ui);
            }
            if js_ready {
                self.redraw = true;
            }
            self.after_script();
            if self.mouse.is_some() && fds.get(1).is_some_and(|f| f.revents != 0) {
                self.redraw = true;
                if let Some(down) = self.read_mouse() {
                    if down && !was_down {
                        let (x, y) = self.pointer;
                        self.click_at(x, y);
                    }
                    was_down = down;
                }
            }
            if !(self.term.has_pending() || fds[0].revents != 0) {
                continue;
            }
            let Some(k) = self.term.key(-1) else { break };
            self.msg.clear();
            self.redraw = true;
            let ph = self.page_h() as f32;
            match k {
                Key::Char('q') | Key::Ctrl('c') => break,
                Key::Down | Key::Tab => self.move_focus(true),
                Key::Up | Key::BackTab => self.move_focus(false),
                Key::Char(' ') | Key::PageDown => self.scroll_by(ph * 0.9),
                Key::Char('b') | Key::PageUp => self.scroll_by(-ph * 0.9),
                Key::Home => self.scroll_by(-1e9),
                Key::End => self.scroll_by(1e9),
                Key::Char('j') => self.scroll_by(48.0),
                Key::Char('k') => self.scroll_by(-48.0),
                Key::Right | Key::Enter => {
                    let t = self.hist.get(self.cur).and_then(|(_, v)| {
                        v.page
                            .as_ref()
                            .and_then(|p| v.focus.and_then(|i| focusables(p).get(i).map(|x| x.0)))
                    });
                    if let Some(t) = t {
                        self.activate(t);
                    }
                }
                Key::Left | Key::Backspace => {
                    if self.cur > 0 {
                        self.leave();
                        self.cur -= 1;
                        let (e, v) = &mut self.hist[self.cur];
                        e.doc = document(&e.loaded, false);
                        e.dirty = true;
                        v.page = None;
                    }
                }
                Key::Char('g') => {
                    if let Some(u) = self.edit("URL", "") {
                        match Url::from_user_input(&u) {
                            Ok(url) => self.open(Request::get(url), true),
                            Err(e) => self.msg = format!("Bad URL: {}", e),
                        }
                    }
                }
                Key::Char('r') => {
                    if let Some(u) = self.hist.get(self.cur).map(|(e, _)| e.loaded.url.clone()) {
                        self.open(Request::get(u), false);
                    }
                }
                _ => {}
            }
        }
        self.leave();
        self.loader.save_cookies();
    }
}

pub fn run(url: Url, insecure: bool) -> i32 {
    let Some(screen) = Screen::open() else {
        eprintln!("browse: no framebuffer (/dev/fb0)");
        return 1;
    };
    let fonts = load_fonts();
    if fonts.is_empty() {
        eprintln!("browse: no fonts in {}", FONT_DIR);
        return 1;
    }
    let term = Term::enter();
    kd_mode(true);
    let (w, h) = (screen.w as i32, screen.h as i32);
    let mut g = Gfx {
        screen,
        term,
        loader: Loader::new(insecure),
        fonts,
        hist: Vec::new(),
        cur: 0,
        sessions: Sessions::default(),
        msg: String::new(),
        mouse: File::open("/dev/input/mice").ok(),
        pointer: (w / 2, h / 2),
        editing: None,
        redraw: true,
    };
    g.open(Request::get(url), true);
    g.run();
    kd_mode(false);
    g.term.leave();
    0
}
