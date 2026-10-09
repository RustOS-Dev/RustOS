use crate::canvas::{Canvas, Rgba};
use crate::fonts::{Family, Fonts};
use crate::image::{decode, encode_png};
use crate::painter::{Scene, paint};
use crate::*;
use alloc::collections::BTreeMap;
use alloc::string::String;

fn fonts() -> Option<Fonts> {
    let mut f = Fonts::new();
    let dir = "/usr/share/fonts/truetype/dejavu/";
    let load = |n: &str| std::fs::read(format!("{dir}{n}")).ok();
    f.add(Family::Sans, false, false, &load("DejaVuSans.ttf")?);
    if let Some(d) = load("DejaVuSans-Bold.ttf") {
        f.add(Family::Sans, true, false, &d);
    }
    if let Some(d) = load("DejaVuSansMono.ttf") {
        f.add(Family::Mono, false, false, &d);
    }
    Some(f)
}

#[test]
fn canvas_fill_and_blend() {
    let mut c = Canvas::new(10, 10, Rgba::WHITE);
    c.fill_rect(2.0, 2.0, 4.0, 4.0, Rgba::new(255, 0, 0, 255));
    assert_eq!(c.get(3, 3), Rgba::new(255, 0, 0, 255));
    assert_eq!(c.get(7, 7), Rgba::WHITE);
    // Half-covered edge pixel.
    c.fill_rect(0.0, 8.5, 10.0, 1.0, Rgba::BLACK);
    let p = c.get(0, 8);
    assert!(p.r > 100 && p.r < 150, "{p:?}");
    // 50% alpha.
    c.fill_rect(0.0, 0.0, 1.0, 1.0, Rgba::new(0, 0, 255, 128));
    assert_eq!(c.get(0, 0), Rgba::new(127, 127, 255, 255));
    // Clipping.
    let old = c.set_clip(Clip {
        x0: 0,
        y0: 0,
        x1: 2,
        y1: 2,
    });
    c.fill_rect(0.0, 0.0, 10.0, 10.0, Rgba::BLACK);
    c.set_clip(old);
    assert_eq!(c.get(1, 1), Rgba::BLACK);
    assert_eq!(c.get(3, 3), Rgba::new(255, 0, 0, 255));
}

#[test]
fn rounded_rect_and_polygon() {
    let mut c = Canvas::new(20, 20, Rgba::WHITE);
    c.fill_round_rect(0.0, 0.0, 20.0, 20.0, [10.0; 4], Rgba::BLACK);
    assert_eq!(c.get(0, 0), Rgba::WHITE); // corner cut
    assert_eq!(c.get(10, 10), Rgba::BLACK);
    let mut c = Canvas::new(20, 20, Rgba::WHITE);
    c.fill_polygon(&[(0.0, 0.0), (20.0, 0.0), (0.0, 20.0)], Rgba::BLACK, true);
    assert_eq!(c.get(2, 2), Rgba::BLACK);
    assert_eq!(c.get(18, 18), Rgba::WHITE);
}

#[test]
fn png_roundtrip() {
    let mut img = Image::new(3, 2);
    img.pixels[0] = Rgba::new(255, 0, 0, 255);
    img.pixels[4] = Rgba::new(0, 255, 0, 255);
    for p in img.pixels.iter_mut() {
        p.a = 255;
    }
    let png = encode_png(&img);
    let back = decode(&png).expect("decodes");
    assert_eq!(back.still(), &img);
}

#[test]
fn gif_decodes() {
    // A 2x2 GIF: red, green / blue, white (LZW min code size 2).
    let gif: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 2, 0, 2, 0, 0x81, 0,
        0, // header, 4-color global table
        255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, //
        0x2C, 0, 0, 0, 0, 2, 0, 2, 0, 0, // image descriptor
        2, 3, 0x44, 0x34, 0x05, 0, // LZW min 2; data: clear 0 1 2 3 end
        0x3B,
    ];
    let d = decode(gif).expect("gif");
    let i = d.still();
    assert_eq!((i.width, i.height), (2, 2));
    assert_eq!(i.get(0, 0), Rgba::new(255, 0, 0, 255));
    assert_eq!(i.get(1, 0), Rgba::new(0, 255, 0, 255));
    assert_eq!(i.get(0, 1), Rgba::new(0, 0, 255, 255));
    assert_eq!(i.get(1, 1), Rgba::new(255, 255, 255, 255));
}

#[test]
fn text_measures_and_draws() {
    let Some(f) = fonts() else {
        eprintln!("no DejaVu fonts; skipping");
        return;
    };
    let st = css::ComputedStyle::default();
    let id = f.face_for(&st);
    let w1 = f.text_width(id, "i", 16.0, 0.0);
    let w2 = f.text_width(id, "WWW", 16.0, 0.0);
    assert!(w1 > 2.0 && w2 > w1 * 3.0, "{w1} {w2}");
    let mut c = Canvas::new(60, 24, Rgba::WHITE);
    f.draw(&mut c, id, "Hi", 16.0, 2.0, 18.0, Rgba::BLACK, 0.0);
    let dark = c.pixels.iter().filter(|p| p.r < 100).count();
    assert!(dark > 20, "{dark}");
}

#[test]
fn page_paints() {
    let Some(f) = fonts() else {
        return;
    };
    let doc = html::parse(
        "<body style='margin:0;background:#eee'><div id=box style='width:100px;height:50px;background:rgb(200,0,0);border-radius:10px'></div>\
         <p style='color:blue;font-size:20px'>Hello <a href=/x>link</a></p><ul><li>one</ul><input value=typed><button>Go</button></body>",
    );
    let sizes = BTreeMap::new();
    let resolve = |s: &str| String::from(s);
    let page = layout_page(
        &doc,
        &[],
        &f,
        &sizes,
        &resolve,
        320.0,
        240.0,
        &layout::DomState::default(),
        false,
    );
    assert_eq!(page.links.len(), 1);
    assert_eq!(page.fields.len(), 2);
    let bg = page_background(&page.root);
    assert_eq!(bg, Rgba::new(0xee, 0xee, 0xee, 255));
    let mut c = Canvas::new(320, 240, bg);
    let images = BTreeMap::new();
    let scene = Scene {
        fonts: &f,
        fields: &page.fields,
        images: &images,
        resolve: &resolve,
        scroll: (0.0, 0.0),
        focus: None,
        hover: None,
    };
    paint(&mut c, &page.root, &scene);
    assert_eq!(c.get(50, 25), Rgba::new(200, 0, 0, 255));
    assert_eq!(c.get(0, 0), Rgba::new(0xee, 0xee, 0xee, 255)); // rounded corner
    assert_eq!(c.get(150, 25), Rgba::new(0xee, 0xee, 0xee, 255));
    // Blue text somewhere below the box.
    let blue = (50..120)
        .flat_map(|y| (0..320).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = c.get(x, y);
            p.b > 150 && p.r < 100
        });
    assert!(blue.count() > 30);
    if std::env::var("PAINT_PNG").is_ok() {
        std::fs::write("/tmp/paint-test.png", encode_png(&c.to_image())).unwrap();
    }
}

#[test]
fn woff1_unpacks() {
    let Ok(ttf) = std::fs::read("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf") else {
        return;
    };
    // Wrap the TrueType tables in a WOFF 1.0 container (stored tables).
    let n = u16::from_be_bytes([ttf[4], ttf[5]]) as usize;
    let mut dir = Vec::new();
    let mut body = Vec::new();
    let data_start = 44 + 20 * n;
    for i in 0..n {
        let e = 12 + 16 * i;
        let tag = &ttf[e..e + 4];
        let csum = &ttf[e + 4..e + 8];
        let off = u32::from_be_bytes([ttf[e + 8], ttf[e + 9], ttf[e + 10], ttf[e + 11]]) as usize;
        let len = u32::from_be_bytes([ttf[e + 12], ttf[e + 13], ttf[e + 14], ttf[e + 15]]) as usize;
        while body.len() % 4 != 0 {
            body.push(0);
        }
        dir.extend_from_slice(tag);
        dir.extend_from_slice(&((data_start + body.len()) as u32).to_be_bytes());
        dir.extend_from_slice(&(len as u32).to_be_bytes());
        dir.extend_from_slice(&(len as u32).to_be_bytes());
        dir.extend_from_slice(csum);
        body.extend_from_slice(&ttf[off..off + len]);
    }
    let mut woff = b"wOFF".to_vec();
    woff.extend_from_slice(&ttf[0..4]);
    woff.extend_from_slice(&((data_start + body.len()) as u32).to_be_bytes());
    woff.extend_from_slice(&(n as u16).to_be_bytes());
    woff.extend_from_slice(&[0, 0]);
    woff.extend_from_slice(&(ttf.len() as u32).to_be_bytes());
    woff.extend_from_slice(&[0u8; 24]);
    woff.extend_from_slice(&dir);
    woff.extend_from_slice(&body);
    let sfnt = crate::image::font_file(&woff).expect("unpacks");
    let mut f = Fonts::new();
    assert!(f.add_named("webmono", &sfnt));
    let mut st = css::ComputedStyle::default();
    st.font_family = vec![String::from("WebMono")];
    let id = f.face_for(&st);
    assert!(id >= 100);
    assert!(f.text_width(id, "mmm", 20.0, 0.0) > 30.0);
}
