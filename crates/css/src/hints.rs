//! Presentational hints: HTML attributes that map to CSS (HTML §15),
//! returned as declarations for the cascade.

use crate::parser::{Declaration, component_values};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

fn decl(name: &str, value: &str) -> Declaration {
    let mut v = component_values(value);
    while v.first().is_some_and(|c| c.is_ws()) {
        v.remove(0);
    }
    Declaration {
        name: name.into(),
        value: v,
        important: false,
    }
}

/// `width="50"` / `width="50%"` as a CSS length.
fn dimension(v: &str) -> Option<String> {
    let v = v.trim();
    let digits: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        return None;
    }
    Some(if v[digits.len()..].starts_with('%') {
        format!("{}%", digits)
    } else {
        format!("{}px", digits)
    })
}

/// Legacy color attribute value (`bgcolor="red"`, `"#fff"`, `"ff0000"`).
fn legacy_color(v: &str) -> String {
    let v = v.trim();
    if v.len() == 6 && v.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{}", v)
    } else {
        String::from(v)
    }
}

/// Hints for element `tag` with `attrs` (lower-case names). `attr_of`
/// looks up an attribute of the nearest ancestor `table` for cells.
pub fn presentational_hints(
    tag: &str,
    attrs: &[(String, String)],
    table_attr: &dyn Fn(&str) -> Option<String>,
) -> Vec<Declaration> {
    let get = |n: &str| attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
    let mut out = Vec::new();
    if let Some(c) = get("bgcolor") {
        out.push(decl("background-color", &legacy_color(c)));
    }
    if let Some(b) = get("background") {
        out.push(decl("background-image", &format!("url(\"{}\")", b)));
    }
    match tag {
        "body" => {
            if let Some(c) = get("text") {
                out.push(decl("color", &legacy_color(c)));
            }
            for (a, p) in [
                ("marginwidth", "margin-left"),
                ("marginwidth", "margin-right"),
                ("leftmargin", "margin-left"),
                ("rightmargin", "margin-right"),
                ("marginheight", "margin-top"),
                ("marginheight", "margin-bottom"),
                ("topmargin", "margin-top"),
                ("bottommargin", "margin-bottom"),
            ] {
                if let Some(d) = get(a).and_then(dimension) {
                    out.push(decl(p, &d));
                }
            }
        }
        "font" => {
            if let Some(c) = get("color") {
                out.push(decl("color", &legacy_color(c)));
            }
            if let Some(f) = get("face") {
                out.push(decl("font-family", f));
            }
            if let Some(s) = get("size") {
                let s = s.trim();
                let n: i32 = s.trim_start_matches(['+', '-']).parse().unwrap_or(3);
                let v = if s.starts_with('+') {
                    3 + n
                } else if s.starts_with('-') {
                    3 - n
                } else {
                    n
                }
                .clamp(1, 7);
                let size = [
                    "x-small",
                    "small",
                    "medium",
                    "large",
                    "x-large",
                    "xx-large",
                    "xxx-large",
                ][(v - 1) as usize];
                out.push(decl("font-size", size));
            }
        }
        "table" => {
            if let Some(w) = get("width").and_then(dimension) {
                out.push(decl("width", &w));
            }
            if let Some(h) = get("height").and_then(dimension) {
                out.push(decl("height", &h));
            }
            if let Some(b) = get("border") {
                let px: u32 = b.trim().parse().unwrap_or(1);
                if px > 0 {
                    out.push(decl("border", &format!("{}px outset gray", px)));
                }
            }
            if let Some(s) = get("cellspacing").and_then(dimension) {
                out.push(decl("border-spacing", &s));
            }
            match get("align").map(|a| a.to_ascii_lowercase()).as_deref() {
                Some("center") => {
                    out.push(decl("margin-left", "auto"));
                    out.push(decl("margin-right", "auto"));
                }
                Some("left") => out.push(decl("float", "left")),
                Some("right") => out.push(decl("float", "right")),
                _ => {}
            }
        }
        "td" | "th" => {
            if let Some(w) = get("width").and_then(dimension) {
                out.push(decl("width", &w));
            }
            if let Some(h) = get("height").and_then(dimension) {
                out.push(decl("height", &h));
            }
            if get("nowrap").is_some() {
                out.push(decl("white-space", "nowrap"));
            }
            if let Some(p) = table_attr("cellpadding").and_then(|v| dimension(&v)) {
                out.push(decl("padding", &p));
            }
            if table_attr("border").is_some_and(|b| b.trim().parse::<u32>().unwrap_or(1) > 0) {
                out.push(decl("border", "1px inset gray"));
            }
        }
        "img" | "video" | "canvas" | "iframe" | "embed" | "object" | "input" => {
            if tag != "input" || get("type").is_some_and(|t| t.eq_ignore_ascii_case("image")) {
                if let Some(w) = get("width").and_then(dimension) {
                    out.push(decl("width", &w));
                }
                if let Some(h) = get("height").and_then(dimension) {
                    out.push(decl("height", &h));
                }
            }
            if let Some(b) = get("border").and_then(dimension) {
                out.push(decl("border", &format!("{} solid", b)));
            }
            match get("align").map(|a| a.to_ascii_lowercase()).as_deref() {
                Some("left") => out.push(decl("float", "left")),
                Some("right") => out.push(decl("float", "right")),
                Some("middle" | "center") => out.push(decl("vertical-align", "middle")),
                Some("top") => out.push(decl("vertical-align", "top")),
                _ => {}
            }
            if let Some(h) = get("hspace").and_then(dimension) {
                out.push(decl("margin-left", &h));
                out.push(decl("margin-right", &h));
            }
            if let Some(v) = get("vspace").and_then(dimension) {
                out.push(decl("margin-top", &v));
                out.push(decl("margin-bottom", &v));
            }
        }
        "hr" => {
            if let Some(w) = get("width").and_then(dimension) {
                out.push(decl("width", &w));
            }
            if let Some(s) = get("size").and_then(dimension) {
                out.push(decl("height", &s));
            }
            if get("noshade").is_some() {
                out.push(decl("border-style", "solid"));
            }
            if let Some(c) = get("color") {
                out.push(decl("color", &legacy_color(c)));
                out.push(decl("background-color", &legacy_color(c)));
            }
        }
        "ol" | "ul" | "li" => {
            if tag != "li"
                && let Some(s) = get("start")
                && let Ok(n) = s.trim().parse::<i32>()
            {
                out.push(decl("counter-reset", &format!("list-item {}", n - 1)));
            }
            if tag == "li"
                && let Some(v) = get("value")
                && let Ok(n) = v.trim().parse::<i32>()
            {
                out.push(decl("counter-set", &format!("list-item {}", n)));
            }
        }
        "pre" | "textarea" => {
            if tag == "pre" && get("wrap").is_some() {
                out.push(decl("white-space", "pre-wrap"));
            }
        }
        _ => {}
    }
    // align on block elements.
    if matches!(
        tag,
        "div"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "td"
            | "th"
            | "tr"
            | "tbody"
            | "thead"
            | "tfoot"
            | "caption"
            | "center"
    ) {
        match get("align").map(|a| a.to_ascii_lowercase()).as_deref() {
            Some("center" | "middle") => out.push(decl("text-align", "center")),
            Some("left") => out.push(decl("text-align", "left")),
            Some("right") => out.push(decl("text-align", "right")),
            Some("justify") => out.push(decl("text-align", "justify")),
            _ => {}
        }
    }
    if matches!(tag, "td" | "th" | "tr" | "tbody" | "thead" | "tfoot") {
        match get("valign").map(|a| a.to_ascii_lowercase()).as_deref() {
            Some(v @ ("top" | "middle" | "bottom" | "baseline")) => {
                out.push(decl("vertical-align", v))
            }
            _ => {}
        }
    }
    out
}
