//! `multipart/form-data` bodies (RFC 7578) for forms with file inputs or
//! `enctype="multipart/form-data"`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub enum Part<'a> {
    Text(&'a str),
    File {
        filename: &'a str,
        content_type: &'a str,
        data: &'a [u8],
    },
}

/// Escape a name or file name for a `Content-Disposition` parameter as
/// HTML's form submission algorithm does.
fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("%22"),
            '\r' => o.push_str("%0D"),
            '\n' => o.push_str("%0A"),
            c => o.push(c),
        }
    }
    o
}

/// Encode fields; returns the `Content-Type` header value and the body.
/// `seed` makes the boundary unique (e.g. a clock or random value).
pub fn encode(fields: &[(&str, Part)], seed: u64) -> (String, Vec<u8>) {
    let boundary = format!("----RustOSFormBoundary{:016x}", seed);
    let mut body = Vec::new();
    for (name, part) in fields {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        match part {
            Part::Text(v) => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                        escape(name)
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(v.as_bytes());
            }
            Part::File {
                filename,
                content_type,
                data,
            } => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                        escape(name),
                        escape(filename),
                        if content_type.is_empty() { "application/octet-stream" } else { content_type }
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(data);
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    (format!("multipart/form-data; boundary={}", boundary), body)
}
