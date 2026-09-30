//! Image decoding: PNG and JPEG (zune), GIF (in-tree LZW, all frames with
//! their delays), BMP; and a PNG encoder for screenshots.

use crate::canvas::{Image, Rgba};
use alloc::vec;
use alloc::vec::Vec;

/// A decoded image with animation frames (GIF); `frames[0]` is the still.
pub struct Decoded {
    pub frames: Vec<(Image, u32)>,
}

impl Decoded {
    pub fn still(&self) -> &Image {
        &self.frames[0].0
    }
}

/// Largest accepted image (pixels), to bound memory.
const MAX_PIXELS: usize = 16 << 20;

pub fn decode(data: &[u8]) -> Option<Decoded> {
    if data.starts_with(b"\x89PNG") {
        return png(data).map(|i| Decoded { frames: vec![(i, 0)] });
    }
    if data.starts_with(&[0xFF, 0xD8]) {
        return jpeg(data).map(|i| Decoded { frames: vec![(i, 0)] });
    }
    if data.starts_with(b"GIF8") {
        return gif(data);
    }
    if data.starts_with(b"BM") {
        return bmp(data).map(|i| Decoded { frames: vec![(i, 0)] });
    }
    None
}

fn from_rgba(w: usize, h: usize, px: &[u8], channels: usize) -> Option<Image> {
    if w * h > MAX_PIXELS || px.len() < w * h * channels {
        return None;
    }
    let mut img = Image::new(w as u32, h as u32);
    for i in 0..w * h {
        let p = &px[i * channels..];
        img.pixels[i] = match channels {
            1 => Rgba::new(p[0], p[0], p[0], 255),
            2 => Rgba::new(p[0], p[0], p[0], p[1]),
            3 => Rgba::new(p[0], p[1], p[2], 255),
            _ => Rgba::new(p[0], p[1], p[2], p[3]),
        };
    }
    Some(img)
}

fn png(data: &[u8]) -> Option<Image> {
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().png_set_strip_to_8bit(true).set_max_width(8192).set_max_height(8192);
    let mut d = zune_png::PngDecoder::new_with_options(zune_core::bytestream::ZCursor::new(data), opts);
    let px = d.decode_raw().ok()?;
    let (w, h) = d.dimensions()?;
    let ch = d.colorspace()?.num_components();
    from_rgba(w, h, &px, ch)
}

fn jpeg(data: &[u8]) -> Option<Image> {
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB).set_max_width(8192).set_max_height(8192);
    let mut d = zune_jpeg::JpegDecoder::new_with_options(zune_core::bytestream::ZCursor::new(data), opts);
    let px = d.decode().ok()?;
    let (w, h) = d.dimensions()?;
    from_rgba(w, h, &px, 3)
}

fn bmp(d: &[u8]) -> Option<Image> {
    let u32at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let off = u32at(10)? as usize;
    let w = u32at(18)? as i32;
    let h = u32at(22)? as i32;
    let bpp = u16::from_le_bytes([*d.get(28)?, *d.get(29)?]);
    let comp = u32at(30)?;
    if w <= 0 || h == 0 || !(bpp == 24 || bpp == 32) || (comp != 0 && comp != 3) {
        return None;
    }
    let (w, flip) = (w as usize, h > 0);
    let h = h.unsigned_abs() as usize;
    if w * h > MAX_PIXELS {
        return None;
    }
    let bytes = bpp as usize / 8;
    let stride = (w * bytes).div_ceil(4) * 4;
    let mut img = Image::new(w as u32, h as u32);
    for y in 0..h {
        let row = if flip { h - 1 - y } else { y };
        for x in 0..w {
            let o = off + row * stride + x * bytes;
            let p = d.get(o..o + bytes)?;
            img.pixels[y * w + x] = Rgba::new(p[2], p[1], p[0], if bytes == 4 && comp == 3 { p[3] } else { 255 });
        }
    }
    Some(img)
}

// ---- GIF ----

fn lzw(data: &[u8], min: u8, npix: usize) -> Vec<u8> {
    let clear = 1u16 << min;
    let end = clear + 1;
    let mut out = Vec::with_capacity(npix);
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut first = vec![0u8; 4096];
    for i in 0..clear as usize {
        suffix[i] = i as u8;
        first[i] = i as u8;
    }
    let mut size = min as u32 + 1;
    let mut next = end + 1;
    let mut prev: Option<u16> = None;
    let (mut bits, mut nbits) = (0u32, 0u32);
    let mut stack = Vec::with_capacity(4096);
    for &b in data {
        bits |= (b as u32) << nbits;
        nbits += 8;
        while nbits >= size {
            let code = (bits & ((1 << size) - 1)) as u16;
            bits >>= size;
            nbits -= size;
            if code == clear {
                size = min as u32 + 1;
                next = end + 1;
                prev = None;
                continue;
            }
            if code == end {
                return out;
            }
            let Some(p) = prev else {
                if code < clear {
                    out.push(code as u8);
                    prev = Some(code);
                }
                continue;
            };
            let known = code < next;
            let c = if known { code } else { p };
            // Unwind the string for c.
            stack.clear();
            let mut k = c;
            while k >= clear && stack.len() < 4096 {
                stack.push(suffix[k as usize]);
                k = prefix[k as usize];
            }
            stack.push(k as u8);
            let head = *stack.last().unwrap();
            out.extend(stack.iter().rev());
            if !known {
                out.push(head);
            }
            if next < 4096 {
                prefix[next as usize] = p;
                suffix[next as usize] = if known { head } else { first[p as usize] };
                first[next as usize] = first[p as usize];
                next += 1;
                if next == (1 << size) && size < 12 {
                    size += 1;
                }
            }
            prev = Some(code);
            if out.len() >= npix {
                return out;
            }
        }
    }
    out
}

fn gif(d: &[u8]) -> Option<Decoded> {
    let w = u16::from_le_bytes([*d.get(6)?, *d.get(7)?]) as usize;
    let h = u16::from_le_bytes([*d.get(8)?, *d.get(9)?]) as usize;
    if w == 0 || h == 0 || w * h > MAX_PIXELS {
        return None;
    }
    let flags = *d.get(10)?;
    let bg_index = *d.get(11)?;
    let mut p = 13;
    let mut global: Vec<Rgba> = Vec::new();
    if flags & 0x80 != 0 {
        let n = 2usize << (flags & 7);
        for i in 0..n {
            let c = d.get(p + 3 * i..p + 3 * i + 3)?;
            global.push(Rgba::new(c[0], c[1], c[2], 255));
        }
        p += 3 * n;
    }
    let _ = bg_index;
    let mut canvas = Image::new(w as u32, h as u32);
    let mut frames = Vec::new();
    let (mut delay, mut transparent, mut dispose) = (0u32, None::<u8>, 0u8);
    while p < d.len() {
        match d[p] {
            0x21 => {
                let label = *d.get(p + 1)?;
                p += 2;
                if label == 0xF9 && d.get(p) == Some(&4) {
                    let pf = *d.get(p + 1)?;
                    delay = u16::from_le_bytes([*d.get(p + 2)?, *d.get(p + 3)?]) as u32 * 10;
                    transparent = if pf & 1 != 0 { Some(*d.get(p + 4)?) } else { None };
                    dispose = (pf >> 2) & 7;
                }
                // Skip sub-blocks.
                while let Some(&n) = d.get(p) {
                    p += 1 + n as usize;
                    if n == 0 {
                        break;
                    }
                }
            }
            0x2C => {
                let fx = u16::from_le_bytes([*d.get(p + 1)?, *d.get(p + 2)?]) as usize;
                let fy = u16::from_le_bytes([*d.get(p + 3)?, *d.get(p + 4)?]) as usize;
                let fw = u16::from_le_bytes([*d.get(p + 5)?, *d.get(p + 6)?]) as usize;
                let fh = u16::from_le_bytes([*d.get(p + 7)?, *d.get(p + 8)?]) as usize;
                let lf = *d.get(p + 9)?;
                p += 10;
                let mut local = Vec::new();
                if lf & 0x80 != 0 {
                    let n = 2usize << (lf & 7);
                    for i in 0..n {
                        let c = d.get(p + 3 * i..p + 3 * i + 3)?;
                        local.push(Rgba::new(c[0], c[1], c[2], 255));
                    }
                    p += 3 * n;
                }
                let pal = if local.is_empty() { &global } else { &local };
                let min = *d.get(p)?;
                p += 1;
                if min == 0 || min > 11 {
                    return None;
                }
                let mut data = Vec::new();
                while let Some(&n) = d.get(p) {
                    p += 1;
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(d.get(p..p + n as usize)?);
                    p += n as usize;
                }
                let idx = lzw(&data, min, fw * fh);
                let before = canvas.clone();
                // Interlaced rows come in 4 passes.
                let rows: Vec<usize> = if lf & 0x40 != 0 {
                    (0..fh).step_by(8).chain((4..fh).step_by(8)).chain((2..fh).step_by(4)).chain((1..fh).step_by(2)).collect()
                } else {
                    (0..fh).collect()
                };
                for (k, &row) in rows.iter().enumerate() {
                    for col in 0..fw {
                        let Some(&ci) = idx.get(k * fw + col) else { break };
                        if Some(ci) == transparent {
                            continue;
                        }
                        let (x, y) = (fx + col, fy + row);
                        if x < w && y < h {
                            if let Some(&c) = pal.get(ci as usize) {
                                canvas.pixels[y * w + x] = c;
                            }
                        }
                    }
                }
                frames.push((canvas.clone(), delay.max(20)));
                match dispose {
                    2 => {
                        for row in fy..(fy + fh).min(h) {
                            for col in fx..(fx + fw).min(w) {
                                canvas.pixels[row * w + col] = Rgba::default();
                            }
                        }
                    }
                    3 => canvas = before,
                    _ => {}
                }
                transparent = None;
                delay = 0;
                if frames.len() >= 256 {
                    break;
                }
            }
            0x3B => break,
            _ => break,
        }
    }
    if frames.is_empty() {
        return None;
    }
    Some(Decoded { frames })
}

// ---- PNG encoding ----

fn crc32(data: &[u8], mut crc: u32) -> u32 {
    crc = !crc;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let c = crc32(&out[start..], 0);
    out.extend_from_slice(&c.to_be_bytes());
}

/// Encode as an RGB PNG (zlib "stored" blocks after an Up filter: simple
/// and good enough for test screenshots).
pub fn encode_png(img: &Image) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let p = img.pixels[y * w + x];
            raw.extend_from_slice(&[p.r, p.g, p.b]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    if raw.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        let n = c.len() as u16;
        z.extend_from_slice(&n.to_le_bytes());
        z.extend_from_slice(&(!n).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

// ---- web fonts ----

/// A font file usable by fontdue: TrueType/OpenType as is, WOFF 1.0
/// unpacked to an sfnt. (WOFF2 needs Brotli and is not supported.)
pub fn font_file(data: &[u8]) -> Option<Vec<u8>> {
    match data.get(..4)? {
        [0, 1, 0, 0] | b"OTTO" | b"true" | b"ttcf" => Some(data.to_vec()),
        b"wOFF" => woff1(data),
        _ => None,
    }
}

fn woff1(d: &[u8]) -> Option<Vec<u8>> {
    let be32 = |o: usize| d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let be16 = |o: usize| d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let flavor = be32(4)?;
    let n = be16(12)? as usize;
    let total = be32(16)? as usize;
    if n == 0 || n > 512 || total > 32 << 20 {
        return None;
    }
    let mut out = vec![0u8; 12 + 16 * n];
    out[0..4].copy_from_slice(&flavor.to_be_bytes());
    out[4..6].copy_from_slice(&(n as u16).to_be_bytes());
    let mut sr = 1u16;
    let mut es = 0u16;
    while (sr as usize) * 2 <= n {
        sr *= 2;
        es += 1;
    }
    out[6..8].copy_from_slice(&(sr * 16).to_be_bytes());
    out[8..10].copy_from_slice(&es.to_be_bytes());
    out[10..12].copy_from_slice(&((n as u16) * 16 - sr * 16).to_be_bytes());
    for i in 0..n {
        let e = 44 + 20 * i;
        let tag = d.get(e..e + 4)?;
        let off = be32(e + 4)? as usize;
        let clen = be32(e + 8)? as usize;
        let olen = be32(e + 12)? as usize;
        let csum = be32(e + 16)?;
        let src = d.get(off..off + clen)?;
        let table = if clen < olen {
            let mut z = zune_inflate::DeflateDecoder::new_with_options(src, zune_inflate::DeflateOptions::default().set_size_hint(olen));
            z.decode_zlib().ok()?
        } else {
            src.to_vec()
        };
        if table.len() != olen {
            return None;
        }
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let at = out.len();
        let de = 12 + 16 * i;
        out[de..de + 4].copy_from_slice(tag);
        out[de + 4..de + 8].copy_from_slice(&csum.to_be_bytes());
        out[de + 8..de + 12].copy_from_slice(&(at as u32).to_be_bytes());
        out[de + 12..de + 16].copy_from_slice(&(olen as u32).to_be_bytes());
        out.extend_from_slice(&table);
    }
    Some(out)
}
