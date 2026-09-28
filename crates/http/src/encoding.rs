//! Content codings (`gzip`, `deflate`) and text charsets.

use crate::{Error, Result};
use alloc::string::String;
use alloc::vec::Vec;
use miniz_oxide::inflate;

/// Codings we can decode, for the `Accept-Encoding` request header.
pub const ACCEPT_ENCODING: &str = "gzip, deflate";

fn inflate_raw(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    inflate::decompress_to_vec_with_limit(data, limit).map_err(|e| match e.status {
        inflate::TINFLStatus::HasMoreOutput => Error::TooLarge,
        _ => Error::Protocol("corrupt compressed body"),
    })
}

/// Decode a gzip member (RFC 1952).
pub fn gunzip(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    let bad = Error::Protocol("bad gzip header");
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return Err(bad);
    }
    let flg = data[3];
    let mut i = 10;
    if flg & 4 != 0 {
        let xlen = u16::from_le_bytes([
            *data.get(i).ok_or(bad.clone())?,
            *data.get(i + 1).ok_or(bad.clone())?,
        ]) as usize;
        i += 2 + xlen;
    }
    for bit in [8u8, 16] {
        if flg & bit != 0 {
            while *data.get(i).ok_or(bad.clone())? != 0 {
                i += 1;
            }
            i += 1;
        }
    }
    if flg & 2 != 0 {
        i += 2;
    }
    if i >= data.len() {
        return Err(bad);
    }
    inflate_raw(&data[i..], limit)
}

/// Decode a body according to its `Content-Encoding` header value.
pub fn decode_content(coding: &str, body: Vec<u8>, limit: usize) -> Result<Vec<u8>> {
    let mut body = body;
    // Codings are listed in the order applied; undo them in reverse.
    for c in coding.split(',').rev() {
        body = match c.trim().to_ascii_lowercase().as_str() {
            "" | "identity" => body,
            "gzip" | "x-gzip" => gunzip(&body, limit)?,
            // "deflate" is zlib-wrapped per the RFC; some servers send raw.
            "deflate" => match inflate::decompress_to_vec_zlib_with_limit(&body, limit) {
                Ok(v) => v,
                Err(e) if e.status == inflate::TINFLStatus::HasMoreOutput => {
                    return Err(Error::TooLarge)
                }
                Err(_) => inflate_raw(&body, limit)?,
            },
            _ => return Err(Error::Protocol("unsupported content encoding")),
        };
    }
    Ok(body)
}

/// windows-1252 code points for bytes 0x80..0x9F (0 = undefined, kept
/// as the C1 control like browsers do).
const CP1252: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
    0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

fn single_byte(data: &[u8], map: impl Fn(u8) -> char) -> String {
    data.iter().map(|&b| map(b)).collect()
}

fn cp1252(b: u8) -> char {
    if (0x80..0xA0).contains(&b) {
        let cp = CP1252[(b - 0x80) as usize];
        char::from_u32(if cp == 0 { b as u32 } else { cp as u32 }).unwrap_or('\u{FFFD}')
    } else {
        b as char
    }
}

fn latin9(b: u8) -> char {
    match b {
        0xA4 => '€',
        0xA6 => 'Š',
        0xA8 => 'š',
        0xB4 => 'Ž',
        0xB8 => 'ž',
        0xBC => 'Œ',
        0xBD => 'œ',
        0xBE => 'Ÿ',
        _ => b as char,
    }
}

fn utf16(data: &[u8], le: bool) -> String {
    let units = data.chunks_exact(2).map(|c| {
        if le {
            u16::from_le_bytes([c[0], c[1]])
        } else {
            u16::from_be_bytes([c[0], c[1]])
        }
    });
    char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect()
}

/// Decode text in `charset` (label as found in headers or `<meta>`),
/// honouring a byte-order mark. Unknown labels are treated as UTF-8.
pub fn decode_text(data: &[u8], charset: Option<&str>) -> String {
    if let Some(rest) = data.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = data.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    let label = charset.unwrap_or("utf-8").trim().to_ascii_lowercase();
    match label.as_str() {
        // Browsers map all of these to windows-1252 (WHATWG Encoding).
        "iso-8859-1" | "latin1" | "l1" | "iso8859-1" | "iso_8859-1" | "us-ascii" | "ascii"
        | "windows-1252" | "cp1252" | "x-cp1252" | "cp819" => single_byte(data, cp1252),
        "iso-8859-15" | "latin9" | "l9" | "iso8859-15" => single_byte(data, latin9),
        "utf-16le" | "utf-16" => utf16(data, true),
        "utf-16be" => utf16(data, false),
        _ => String::from_utf8_lossy(data).into_owned(),
    }
}
