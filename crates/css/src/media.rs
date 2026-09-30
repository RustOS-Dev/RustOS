//! Media Queries Level 4 (parsing and evaluation) and `@supports`.

use crate::parser::Cv;
use crate::tokenizer::Token;
use crate::values::{LengthContext, unit_to_px};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    Screen,
    Print,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    None,
    Coarse,
    Fine,
}

/// The device media queries are evaluated against (CSS px).
#[derive(Debug, Clone, Copy)]
pub struct Device {
    pub media: MediaType,
    pub width: f32,
    pub height: f32,
    pub dark: bool,
    pub hover: bool,
    pub pointer: Pointer,
    /// Bits per color component (0 = monochrome).
    pub color_bits: u32,
    /// Device pixels per CSS px.
    pub dppx: f32,
    pub reduced_motion: bool,
    /// Scripting enabled (`scripting: enabled`).
    pub scripting: bool,
}

impl Default for Device {
    fn default() -> Self {
        Device {
            media: MediaType::Screen,
            width: 1280.0,
            height: 800.0,
            dark: false,
            hover: true,
            pointer: Pointer::Fine,
            color_bits: 8,
            dppx: 1.0,
            reduced_motion: false,
            scripting: false,
        }
    }
}

fn items(cvs: &[Cv]) -> Vec<&Cv> {
    cvs.iter().filter(|c| !c.is_ws()).collect()
}

fn split_commas(cvs: &[Cv]) -> Vec<&[Cv]> {
    crate::values::split_commas(cvs)
}

/// Evaluate a media query list (empty = all). Invalid queries are false.
pub fn matches(query: &[Cv], dev: &Device) -> bool {
    if query.iter().all(Cv::is_ws) {
        return true;
    }
    split_commas(query)
        .into_iter()
        .any(|q| eval_query(&items(q), dev).unwrap_or(false))
}

fn eval_query(q: &[&Cv], dev: &Device) -> Option<bool> {
    if q.is_empty() {
        return None;
    }
    let mut i = 0;
    let mut negate = false;
    if q[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("not"))
        && q.get(1).is_some_and(|c| c.ident().is_some())
    {
        negate = true;
        i = 1;
    } else if q[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("only")) {
        i = 1;
    }
    if let Some(t) = q.get(i).and_then(|c| c.ident()) {
        let t = t.to_ascii_lowercase();
        let type_ok = match t.as_str() {
            "all" => true,
            "screen" => dev.media == MediaType::Screen,
            "print" => dev.media == MediaType::Print,
            "tty" | "tv" | "projection" | "handheld" | "braille" | "embossed" | "aural"
            | "speech" => false,
            _ => return None,
        };
        i += 1;
        let mut ok = type_ok;
        if i < q.len() {
            if !q[i].ident().is_some_and(|s| s.eq_ignore_ascii_case("and")) {
                return None;
            }
            ok = ok && eval_condition(&q[i + 1..], dev)?;
        }
        return Some(ok != negate);
    }
    eval_condition(q, dev)
}

/// `<media-condition>`: `not X`, `X and Y ...`, `X or Y ...`.
fn eval_condition(c: &[&Cv], dev: &Device) -> Option<bool> {
    if c.is_empty() {
        return None;
    }
    if c[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("not")) {
        return Some(!eval_in_parens(c.get(1)?, dev)?);
    }
    let mut result = eval_in_parens(c[0], dev)?;
    let mut i = 1;
    let mut op: Option<bool> = None; // true = and, false = or
    while i < c.len() {
        let kw = c[i].ident()?.to_ascii_lowercase();
        let is_and = match kw.as_str() {
            "and" => true,
            "or" => false,
            _ => return None,
        };
        if op.is_some_and(|o| o != is_and) {
            return None; // mixing and/or without parentheses
        }
        op = Some(is_and);
        let v = eval_in_parens(c.get(i + 1)?, dev)?;
        result = if is_and { result && v } else { result || v };
        i += 2;
    }
    Some(result)
}

fn eval_in_parens(cv: &Cv, dev: &Device) -> Option<bool> {
    let Cv::Block {
        open: '(',
        items: inner,
    } = cv
    else {
        // General enclosed `func(...)`: false.
        return match cv {
            Cv::Function { .. } => Some(false),
            _ => None,
        };
    };
    let it = items(inner);
    if it.is_empty() {
        return None;
    }
    // Nested condition.
    if matches!(it[0], Cv::Block { open: '(', .. })
        || it[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("not"))
    {
        return eval_condition(&it, dev);
    }
    // `(feature)`, `(feature: value)` or range forms.
    if it.len() == 1 {
        let f = it[0].ident()?.to_ascii_lowercase();
        return Some(boolean_feature(&f, dev));
    }
    if matches!(it[1], Cv::Token(Token::Colon)) {
        let f = it[0].ident()?.to_ascii_lowercase();
        return Some(plain_feature(&f, &it[2..], dev).unwrap_or(false));
    }
    range_feature(&it, dev)
}

fn feature_value(name: &str, dev: &Device) -> Option<f32> {
    Some(match name {
        "width" | "device-width" => dev.width,
        "height" | "device-height" => dev.height,
        "aspect-ratio" | "device-aspect-ratio" => dev.width / dev.height.max(1.0),
        "resolution" => dev.dppx,
        "color" => dev.color_bits as f32,
        "monochrome" => {
            if dev.color_bits == 0 {
                1.0
            } else {
                0.0
            }
        }
        "color-index" => 0.0,
        _ => return None,
    })
}

fn value_of(v: &[&Cv], name: &str) -> Option<f32> {
    let ctx = LengthContext {
        font_size: 16.0,
        root_font_size: 16.0,
        ..Default::default()
    };
    match v {
        [
            Cv::Token(Token::Number { value: a, .. }),
            d,
            Cv::Token(Token::Number { value: b, .. }),
        ] if d.is_delim('/') => Some(a / b.max(f32::MIN_POSITIVE)),
        [Cv::Token(Token::Dimension { value, unit })] => {
            let u = unit.to_ascii_lowercase();
            match u.as_str() {
                "dppx" | "x" => Some(*value),
                "dpi" => Some(value / 96.0),
                "dpcm" => Some(value * 2.54 / 96.0),
                _ => unit_to_px(*value, unit, &ctx),
            }
        }
        [Cv::Token(Token::Number { value, .. })] => {
            if name.ends_with("aspect-ratio") {
                Some(*value)
            } else {
                Some(*value)
            }
        }
        _ => None,
    }
}

fn plain_feature(f: &str, v: &[&Cv], dev: &Device) -> Option<bool> {
    let (prefix, name) = if let Some(n) = f.strip_prefix("min-") {
        (1, n)
    } else if let Some(n) = f.strip_prefix("max-") {
        (2, n)
    } else {
        (0, f)
    };
    let kw_owned = v
        .first()
        .and_then(|c| c.ident())
        .map(|s| s.to_ascii_lowercase());
    let kw = kw_owned.as_deref();
    match name {
        "prefers-color-scheme" => return Some(kw? == if dev.dark { "dark" } else { "light" }),
        "prefers-reduced-motion" => return Some((kw? == "reduce") == dev.reduced_motion),
        "prefers-contrast" | "forced-colors" | "inverted-colors" => {
            return Some(kw? == "no-preference" || kw == Some("none"));
        }
        "prefers-reduced-transparency" | "prefers-reduced-data" => {
            return Some(kw? == "no-preference");
        }
        "hover" | "any-hover" => return Some((kw? == "hover") == dev.hover),
        "pointer" | "any-pointer" => {
            return Some(
                kw? == match dev.pointer {
                    Pointer::None => "none",
                    Pointer::Coarse => "coarse",
                    Pointer::Fine => "fine",
                },
            );
        }
        "orientation" => {
            return Some(
                kw? == if dev.height >= dev.width {
                    "portrait"
                } else {
                    "landscape"
                },
            );
        }
        "scan" => return Some(kw? == "progressive"),
        "grid" => return Some(value_of(v, name)? == 0.0),
        "update" => {
            return Some(
                kw? == if dev.media == MediaType::Print {
                    "none"
                } else {
                    "fast"
                },
            );
        }
        "scripting" => return Some(kw? == if dev.scripting { "enabled" } else { "none" }),
        "display-mode" => return Some(kw? == "browser"),
        "dynamic-range" | "video-dynamic-range" => return Some(kw? == "standard"),
        "color-gamut" => return Some(kw? == "srgb"),
        "overflow-block" => {
            return Some(
                kw? == if dev.media == MediaType::Print {
                    "paged"
                } else {
                    "scroll"
                },
            );
        }
        "overflow-inline" => return Some(kw? == "scroll"),
        _ => {}
    }
    let actual = feature_value(name, dev)?;
    let want = value_of(v, name)?;
    Some(match prefix {
        1 => actual >= want,
        2 => actual <= want,
        _ => (actual - want).abs() < 0.01,
    })
}

fn boolean_feature(f: &str, dev: &Device) -> bool {
    match f {
        "hover" | "any-hover" => dev.hover,
        "pointer" | "any-pointer" => dev.pointer != Pointer::None,
        "color" => dev.color_bits > 0,
        "monochrome" => dev.color_bits == 0,
        "grid" => false,
        "prefers-reduced-motion" => dev.reduced_motion,
        _ => feature_value(f, dev).is_some_and(|v| v != 0.0),
    }
}

/// Comparison operator at `it[i]` (`<`, `<=`, `>`, `>=`, `=`): (op, width).
fn op_at(it: &[&Cv], i: usize) -> Option<(char, bool, usize)> {
    let c = it.get(i)?;
    let op = ['<', '>', '='].into_iter().find(|&o| c.is_delim(o))?;
    let eq = op != '=' && it.get(i + 1).is_some_and(|n| n.is_delim('='));
    Some((op, eq, if eq { 2 } else { 1 }))
}

fn cmp(a: f32, op: char, eq: bool, b: f32) -> bool {
    match (op, eq) {
        ('<', false) => a < b,
        ('<', true) => a <= b,
        ('>', false) => a > b,
        ('>', true) => a >= b,
        _ => (a - b).abs() < 0.01,
    }
}

/// `(width >= 600px)`, `(400px < width <= 700px)`.
fn range_feature(it: &[&Cv], dev: &Device) -> Option<bool> {
    // Find the feature name (an identifier that is not part of a value).
    let name_pos = it.iter().position(|c| c.ident().is_some())?;
    let name = it[name_pos].ident()?.to_ascii_lowercase();
    let actual = feature_value(&name, dev)?;
    if name_pos == 0 {
        let (op, eq, w) = op_at(it, 1)?;
        let v = value_of(&it[1 + w..], &name)?;
        return Some(cmp(actual, op, eq, v));
    }
    // value op name [op value]
    let before = &it[..name_pos];
    let n = before.len();
    let (op1, eq1, lhs_items) = if n >= 2
        && before[n - 1].is_delim('=')
        && (before[n - 2].is_delim('<') || before[n - 2].is_delim('>'))
    {
        (
            if before[n - 2].is_delim('<') {
                '<'
            } else {
                '>'
            },
            true,
            &before[..n - 2],
        )
    } else {
        let (op, _, _) = op_at(before, n.checked_sub(1)?)?;
        (op, false, &before[..n - 1])
    };
    let lhs = value_of(lhs_items, &name)?;
    let flip = |o: char| match o {
        '<' => '>',
        '>' => '<',
        x => x,
    };
    let mut ok = cmp(actual, flip(op1), eq1, lhs);
    if name_pos + 1 < it.len() {
        let (op2, eq2, w) = op_at(it, name_pos + 1)?;
        let rhs = value_of(&it[name_pos + 1 + w..], &name)?;
        ok = ok && cmp(actual, op2, eq2, rhs);
    }
    Some(ok)
}

/// Evaluate an `@supports` condition. `supported(name, value)` says
/// whether a declaration would be accepted.
pub fn supports(cond: &[Cv], supported: &dyn Fn(&str, &[Cv]) -> bool) -> bool {
    let it = items(cond);
    supports_items(&it, supported).unwrap_or(false)
}

fn supports_items(c: &[&Cv], supported: &dyn Fn(&str, &[Cv]) -> bool) -> Option<bool> {
    if c.is_empty() {
        return None;
    }
    if c[0].ident().is_some_and(|s| s.eq_ignore_ascii_case("not")) {
        return Some(!supports_one(c.get(1)?, supported)?);
    }
    let mut result = supports_one(c[0], supported)?;
    let mut i = 1;
    while i < c.len() {
        let kw = c[i].ident()?.to_ascii_lowercase();
        let v = supports_one(c.get(i + 1)?, supported)?;
        result = match kw.as_str() {
            "and" => result && v,
            "or" => result || v,
            _ => return None,
        };
        i += 2;
    }
    Some(result)
}

fn supports_one(cv: &Cv, supported: &dyn Fn(&str, &[Cv]) -> bool) -> Option<bool> {
    match cv {
        Cv::Block {
            open: '(',
            items: inner,
        } => {
            let it = items(inner);
            if it.len() >= 2 && it[0].ident().is_some() && matches!(it[1], Cv::Token(Token::Colon))
            {
                // A declaration.
                let name = it[0].ident()?.to_ascii_lowercase();
                let colon = inner
                    .iter()
                    .position(|c| matches!(c, Cv::Token(Token::Colon)))?;
                let mut value: Vec<Cv> = inner[colon + 1..].to_vec();
                while value.first().is_some_and(Cv::is_ws) {
                    value.remove(0);
                }
                while value.last().is_some_and(Cv::is_ws) {
                    value.pop();
                }
                return Some(supported(&name, &value));
            }
            supports_items(&it, supported)
        }
        Cv::Function { name, args } => {
            let n = name.to_ascii_lowercase();
            Some(match n.as_str() {
                "selector" => crate::selector::SelectorList::parse(args).is_some(),
                _ => false,
            })
        }
        _ => None,
    }
}
