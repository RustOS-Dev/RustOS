//! Value types and their parsers: lengths, percentages, `calc()`,
//! colors, numbers.

use crate::parser::Cv;
use crate::tokenizer::Token;
use alloc::boxed::Box;
use alloc::vec::Vec;

/// Font-relative and viewport units need these to compute to pixels.
#[derive(Debug, Clone, Copy)]
pub struct LengthContext {
    /// The element's computed font size (for `em`; the parent's when
    /// computing `font-size` itself).
    pub font_size: f32,
    pub root_font_size: f32,
    pub viewport_w: f32,
    pub viewport_h: f32,
    /// `line-height` in px for `lh`.
    pub line_height: f32,
}

impl Default for LengthContext {
    fn default() -> Self {
        LengthContext {
            font_size: 16.0,
            root_font_size: 16.0,
            viewport_w: 800.0,
            viewport_h: 600.0,
            line_height: 19.2,
        }
    }
}

/// A length that may depend on a percentage basis known only at layout.
#[derive(Debug, Clone, PartialEq)]
pub enum LengthPercentage {
    /// `px + pct% of basis`.
    Mix { px: f32, pct: f32 },
    /// `min()`/`max()`/`clamp()` involving percentages.
    Calc(Box<Calc>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Calc {
    Leaf { px: f32, pct: f32 },
    Min(Vec<Calc>),
    Max(Vec<Calc>),
    Clamp(Box<Calc>, Box<Calc>, Box<Calc>),
}

impl Calc {
    fn eval(&self, basis: f32) -> f32 {
        match self {
            Calc::Leaf { px, pct } => px + pct / 100.0 * basis,
            Calc::Min(v) => v
                .iter()
                .map(|c| c.eval(basis))
                .fold(f32::INFINITY, f32::min),
            Calc::Max(v) => v
                .iter()
                .map(|c| c.eval(basis))
                .fold(f32::NEG_INFINITY, f32::max),
            Calc::Clamp(a, b, c) => b.eval(basis).min(c.eval(basis)).max(a.eval(basis)),
        }
    }
}

impl LengthPercentage {
    pub const ZERO: LengthPercentage = LengthPercentage::Mix { px: 0.0, pct: 0.0 };

    pub fn px(px: f32) -> LengthPercentage {
        LengthPercentage::Mix { px, pct: 0.0 }
    }

    pub fn percent(pct: f32) -> LengthPercentage {
        LengthPercentage::Mix { px: 0.0, pct }
    }

    /// Resolve against a percentage basis.
    pub fn resolve(&self, basis: f32) -> f32 {
        match self {
            LengthPercentage::Mix { px, pct } => px + pct / 100.0 * basis,
            LengthPercentage::Calc(c) => c.eval(basis),
        }
    }

    /// Resolve when the basis may be unknown (`None` makes percentages 0).
    pub fn resolve_opt(&self, basis: Option<f32>) -> f32 {
        self.resolve(basis.unwrap_or(0.0))
    }

    pub fn has_percent(&self) -> bool {
        match self {
            LengthPercentage::Mix { pct, .. } => *pct != 0.0,
            LengthPercentage::Calc(_) => true,
        }
    }

    pub fn fixed(&self) -> Option<f32> {
        match self {
            LengthPercentage::Mix { px, pct } if *pct == 0.0 => Some(*px),
            _ => None,
        }
    }
}

/// `auto` or a length-percentage.
#[derive(Debug, Clone, PartialEq)]
pub enum LengthAuto {
    Auto,
    Lp(LengthPercentage),
}

impl LengthAuto {
    pub fn is_auto(&self) -> bool {
        matches!(self, LengthAuto::Auto)
    }
    pub fn lp(&self) -> Option<&LengthPercentage> {
        match self {
            LengthAuto::Lp(l) => Some(l),
            LengthAuto::Auto => None,
        }
    }
    pub fn resolve(&self, basis: f32) -> Option<f32> {
        self.lp().map(|l| l.resolve(basis))
    }
}

/// Convert a dimension to px (None for unknown or non-length units).
pub fn unit_to_px(value: f32, unit: &str, ctx: &LengthContext) -> Option<f32> {
    let u = unit.to_ascii_lowercase();
    Some(match u.as_str() {
        "px" => value,
        "em" => value * ctx.font_size,
        "rem" => value * ctx.root_font_size,
        "ex" => value * ctx.font_size * 0.5,
        "rex" => value * ctx.root_font_size * 0.5,
        "ch" => value * ctx.font_size * 0.5,
        "rch" => value * ctx.root_font_size * 0.5,
        "cap" => value * ctx.font_size * 0.7,
        "ic" => value * ctx.font_size,
        "lh" => value * ctx.line_height,
        "rlh" => value * ctx.root_font_size * 1.2,
        "vw" | "svw" | "lvw" | "dvw" => value * ctx.viewport_w / 100.0,
        "vh" | "svh" | "lvh" | "dvh" => value * ctx.viewport_h / 100.0,
        "vmin" | "svmin" | "lvmin" | "dvmin" => value * ctx.viewport_w.min(ctx.viewport_h) / 100.0,
        "vmax" | "svmax" | "lvmax" | "dvmax" => value * ctx.viewport_w.max(ctx.viewport_h) / 100.0,
        "vi" => value * ctx.viewport_w / 100.0,
        "vb" => value * ctx.viewport_h / 100.0,
        "pt" => value * 96.0 / 72.0,
        "pc" => value * 16.0,
        "in" => value * 96.0,
        "cm" => value * 96.0 / 2.54,
        "mm" => value * 96.0 / 25.4,
        "q" => value * 96.0 / 101.6,
        _ => return None,
    })
}

/// Skip whitespace in a value.
pub fn non_ws(cvs: &[Cv]) -> Vec<&Cv> {
    cvs.iter().filter(|c| !c.is_ws()).collect()
}

/// Parse one length-percentage component (dimension, percentage, 0,
/// `calc()`/`min()`/`max()`/`clamp()`).
pub fn length_percentage(cv: &Cv, ctx: &LengthContext) -> Option<LengthPercentage> {
    match cv {
        Cv::Token(Token::Dimension { value, unit }) => {
            Some(LengthPercentage::px(unit_to_px(*value, unit, ctx)?))
        }
        Cv::Token(Token::Percentage(p)) => Some(LengthPercentage::percent(*p)),
        Cv::Token(Token::Number { value, .. }) if *value == 0.0 => Some(LengthPercentage::ZERO),
        Cv::Function { name, args } => {
            let c = math_function(name, args, ctx)?;
            let n = match c {
                MathValue::Length(c) => c,
                MathValue::Number(v) if v == 0.0 => Calc::Leaf { px: 0.0, pct: 0.0 },
                MathValue::Number(_) => return None,
            };
            Some(match n {
                Calc::Leaf { px, pct } => LengthPercentage::Mix { px, pct },
                other => LengthPercentage::Calc(Box::new(other)),
            })
        }
        _ => None,
    }
}

/// A plain length (no percentages).
pub fn length(cv: &Cv, ctx: &LengthContext) -> Option<f32> {
    length_percentage(cv, ctx)?.fixed()
}

pub fn length_auto(cv: &Cv, ctx: &LengthContext) -> Option<LengthAuto> {
    if cv.ident().is_some_and(|s| s.eq_ignore_ascii_case("auto")) {
        return Some(LengthAuto::Auto);
    }
    Some(LengthAuto::Lp(length_percentage(cv, ctx)?))
}

pub fn number(cv: &Cv) -> Option<f32> {
    match cv {
        Cv::Token(Token::Number { value, .. }) => Some(*value),
        Cv::Function { name, args } => {
            match math_function(name, args, &LengthContext::default())? {
                MathValue::Number(v) => Some(v),
                _ => None,
            }
        }
        _ => None,
    }
}

pub fn integer(cv: &Cv) -> Option<i32> {
    match cv {
        Cv::Token(Token::Number { value, int: true }) => Some(*value as i32),
        _ => number(cv).map(|v| crate::math::roundf(v) as i32),
    }
}

// ---------------------------------------------------------------------------
// Math functions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum MathValue {
    Number(f32),
    Length(Calc),
}

fn math_function(name: &str, args: &[Cv], ctx: &LengthContext) -> Option<MathValue> {
    let name = name.to_ascii_lowercase();
    match name.as_str() {
        "calc" | "-webkit-calc" | "-moz-calc" => Expr {
            items: &non_ws_owned(args),
            i: 0,
            ctx,
        }
        .sum(),
        "min" | "max" => {
            let mut vals = Vec::new();
            for part in split_commas(args) {
                vals.push(
                    Expr {
                        items: &non_ws_owned(part),
                        i: 0,
                        ctx,
                    }
                    .sum()?,
                );
            }
            if vals.iter().all(|v| matches!(v, MathValue::Number(_))) {
                let nums = vals.iter().map(|v| {
                    if let MathValue::Number(n) = v {
                        *n
                    } else {
                        0.0
                    }
                });
                return Some(MathValue::Number(if name == "min" {
                    nums.fold(f32::INFINITY, f32::min)
                } else {
                    nums.fold(f32::NEG_INFINITY, f32::max)
                }));
            }
            let calcs: Vec<Calc> = vals.into_iter().map(to_calc).collect::<Option<_>>()?;
            // All fixed: fold now.
            if calcs
                .iter()
                .all(|c| matches!(c, Calc::Leaf { pct, .. } if *pct == 0.0))
            {
                let px = calcs.iter().map(|c| {
                    if let Calc::Leaf { px, .. } = c {
                        *px
                    } else {
                        0.0
                    }
                });
                let v = if name == "min" {
                    px.fold(f32::INFINITY, f32::min)
                } else {
                    px.fold(f32::NEG_INFINITY, f32::max)
                };
                return Some(MathValue::Length(Calc::Leaf { px: v, pct: 0.0 }));
            }
            Some(MathValue::Length(if name == "min" {
                Calc::Min(calcs)
            } else {
                Calc::Max(calcs)
            }))
        }
        "clamp" => {
            let parts = split_commas(args);
            if parts.len() != 3 {
                return None;
            }
            let mut v = Vec::new();
            for p in parts {
                v.push(
                    Expr {
                        items: &non_ws_owned(p),
                        i: 0,
                        ctx,
                    }
                    .sum()?,
                );
            }
            if v.iter().all(|x| matches!(x, MathValue::Number(_))) {
                let n: Vec<f32> = v
                    .iter()
                    .map(|x| {
                        if let MathValue::Number(n) = x {
                            *n
                        } else {
                            0.0
                        }
                    })
                    .collect();
                return Some(MathValue::Number(n[1].min(n[2]).max(n[0])));
            }
            let mut c: Vec<Calc> = v.into_iter().map(to_calc).collect::<Option<_>>()?;
            let (hi, mid, lo) = (c.pop()?, c.pop()?, c.pop()?);
            if let (
                Calc::Leaf { px: a, pct: 0.0 },
                Calc::Leaf { px: b, pct: 0.0 },
                Calc::Leaf { px: d, pct: 0.0 },
            ) = (&lo, &mid, &hi)
            {
                return Some(MathValue::Length(Calc::Leaf {
                    px: b.min(*d).max(*a),
                    pct: 0.0,
                }));
            }
            Some(MathValue::Length(Calc::Clamp(
                Box::new(lo),
                Box::new(mid),
                Box::new(hi),
            )))
        }
        _ => None,
    }
}

fn to_calc(v: MathValue) -> Option<Calc> {
    match v {
        MathValue::Length(c) => Some(c),
        MathValue::Number(n) if n == 0.0 => Some(Calc::Leaf { px: 0.0, pct: 0.0 }),
        MathValue::Number(_) => None,
    }
}

fn non_ws_owned(cvs: &[Cv]) -> Vec<Cv> {
    cvs.iter().filter(|c| !c.is_ws()).cloned().collect()
}

pub fn split_commas(cvs: &[Cv]) -> Vec<&[Cv]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in cvs.iter().enumerate() {
        if matches!(c, Cv::Token(Token::Comma)) {
            out.push(&cvs[start..i]);
            start = i + 1;
        }
    }
    out.push(&cvs[start..]);
    out
}

/// Recursive-descent evaluation of a `calc()` sum.
struct Expr<'a> {
    items: &'a [Cv],
    i: usize,
    ctx: &'a LengthContext,
}

impl Expr<'_> {
    fn sum(&mut self) -> Option<MathValue> {
        let mut acc = self.product()?;
        while self.i < self.items.len() {
            let sign = if self.items[self.i].is_delim('+') {
                1.0
            } else if self.items[self.i].is_delim('-') {
                -1.0
            } else {
                // `calc(1px -2px)`: a signed number directly after is a
                // syntax error in CSS; treat it as addition.
                match &self.items[self.i] {
                    Cv::Token(
                        Token::Dimension { value, .. }
                        | Token::Number { value, .. }
                        | Token::Percentage(value),
                    ) if *value < 0.0 => 1.0,
                    _ => return None,
                }
            };
            if self.items[self.i].is_delim('+') || self.items[self.i].is_delim('-') {
                self.i += 1;
            }
            let rhs = self.product()?;
            acc = add(acc, rhs, sign)?;
        }
        Some(acc)
    }

    fn product(&mut self) -> Option<MathValue> {
        let mut acc = self.atom()?;
        while self.i < self.items.len() {
            if self.items[self.i].is_delim('*') {
                self.i += 1;
                let rhs = self.atom()?;
                acc = mul(acc, rhs)?;
            } else if self.items[self.i].is_delim('/') {
                self.i += 1;
                let MathValue::Number(d) = self.atom()? else {
                    return None;
                };
                if d == 0.0 {
                    return None;
                }
                acc = mul(acc, MathValue::Number(1.0 / d))?;
            } else {
                break;
            }
        }
        Some(acc)
    }

    fn atom(&mut self) -> Option<MathValue> {
        let cv = self.items.get(self.i)?.clone();
        self.i += 1;
        match &cv {
            Cv::Token(Token::Number { value, .. }) => Some(MathValue::Number(*value)),
            Cv::Token(Token::Dimension { value, unit }) => Some(MathValue::Length(Calc::Leaf {
                px: unit_to_px(*value, unit, self.ctx)?,
                pct: 0.0,
            })),
            Cv::Token(Token::Percentage(p)) => {
                Some(MathValue::Length(Calc::Leaf { px: 0.0, pct: *p }))
            }
            Cv::Token(Token::Ident(s)) => match s.to_ascii_lowercase().as_str() {
                "pi" => Some(MathValue::Number(core::f32::consts::PI)),
                "e" => Some(MathValue::Number(core::f32::consts::E)),
                _ => None,
            },
            Cv::Block { open: '(', items } => Expr {
                items: &non_ws_owned(items),
                i: 0,
                ctx: self.ctx,
            }
            .sum(),
            Cv::Function { name, args } => math_function(name, args, self.ctx),
            _ => None,
        }
    }
}

fn add(a: MathValue, b: MathValue, sign: f32) -> Option<MathValue> {
    match (a, b) {
        (MathValue::Number(x), MathValue::Number(y)) => Some(MathValue::Number(x + sign * y)),
        (
            MathValue::Length(Calc::Leaf { px: a, pct: p }),
            MathValue::Length(Calc::Leaf { px: b, pct: q }),
        ) => Some(MathValue::Length(Calc::Leaf {
            px: a + sign * b,
            pct: p + sign * q,
        })),
        // `0` added to a length.
        (MathValue::Length(l), MathValue::Number(n))
        | (MathValue::Number(n), MathValue::Length(l))
            if n == 0.0 =>
        {
            Some(MathValue::Length(l))
        }
        // Sums of min()/max() with other terms: approximate by resolving
        // the non-leaf at a basis of 0 is wrong; reject instead.
        _ => None,
    }
}

fn mul(a: MathValue, b: MathValue) -> Option<MathValue> {
    match (a, b) {
        (MathValue::Number(x), MathValue::Number(y)) => Some(MathValue::Number(x * y)),
        (MathValue::Length(l), MathValue::Number(n))
        | (MathValue::Number(n), MathValue::Length(l)) => Some(MathValue::Length(scale(l, n))),
        _ => None,
    }
}

fn scale(c: Calc, n: f32) -> Calc {
    match c {
        Calc::Leaf { px, pct } => Calc::Leaf {
            px: px * n,
            pct: pct * n,
        },
        Calc::Min(v) if n >= 0.0 => Calc::Min(v.into_iter().map(|c| scale(c, n)).collect()),
        Calc::Max(v) if n >= 0.0 => Calc::Max(v.into_iter().map(|c| scale(c, n)).collect()),
        Calc::Min(v) => Calc::Max(v.into_iter().map(|c| scale(c, n)).collect()),
        Calc::Max(v) => Calc::Min(v.into_iter().map(|c| scale(c, n)).collect()),
        Calc::Clamp(a, b, c) => Calc::Clamp(
            Box::new(scale(*a, n)),
            Box::new(scale(*b, n)),
            Box::new(scale(*c, n)),
        ),
    }
}

// ---------------------------------------------------------------------------
// Colors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    pub const BLACK: Rgba = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    pub const WHITE: Rgba = Rgba {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba { r, g, b, a: 255 }
    }

    pub fn is_transparent(&self) -> bool {
        self.a == 0
    }

    /// Composite over an opaque background.
    pub fn over(self, bg: Rgba) -> Rgba {
        let a = self.a as u32;
        let mix = |f: u8, b: u8| ((f as u32 * a + b as u32 * (255 - a)) / 255) as u8;
        Rgba {
            r: mix(self.r, bg.r),
            g: mix(self.g, bg.g),
            b: mix(self.b, bg.b),
            a: 255,
        }
    }
}

/// A specified color: a value or `currentColor` (resolved after `color`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Rgba(Rgba),
    CurrentColor,
}

impl Color {
    pub fn resolve(self, current: Rgba) -> Rgba {
        match self {
            Color::Rgba(c) => c,
            Color::CurrentColor => current,
        }
    }
}

fn clamp_u8(v: f32) -> u8 {
    crate::math::roundf(v).clamp(0.0, 255.0) as u8
}

fn channel(cv: &Cv) -> Option<f32> {
    match cv {
        Cv::Token(Token::Number { value, .. }) => Some(*value),
        Cv::Token(Token::Percentage(p)) => Some(p * 2.55),
        c if c.ident().is_some_and(|s| s.eq_ignore_ascii_case("none")) => Some(0.0),
        _ => None,
    }
}

fn alpha(cv: &Cv) -> Option<f32> {
    match cv {
        Cv::Token(Token::Number { value, .. }) => Some(value.clamp(0.0, 1.0)),
        Cv::Token(Token::Percentage(p)) => Some((p / 100.0).clamp(0.0, 1.0)),
        c if c.ident().is_some_and(|s| s.eq_ignore_ascii_case("none")) => Some(0.0),
        _ => None,
    }
}

fn hue(cv: &Cv) -> Option<f32> {
    match cv {
        Cv::Token(Token::Number { value, .. }) => Some(*value),
        Cv::Token(Token::Dimension { value, unit }) => {
            Some(match unit.to_ascii_lowercase().as_str() {
                "deg" => *value,
                "rad" => value.to_degrees(),
                "grad" => value * 0.9,
                "turn" => value * 360.0,
                _ => return None,
            })
        }
        c if c.ident().is_some_and(|s| s.eq_ignore_ascii_case("none")) => Some(0.0),
        _ => None,
    }
}

/// Split color function arguments: `r g b / a` or `r, g, b, a`.
fn color_args(args: &[Cv]) -> Option<(Vec<Cv>, Option<Cv>)> {
    let items: Vec<Cv> = args
        .iter()
        .filter(|c| !c.is_ws() && !matches!(c, Cv::Token(Token::Comma)))
        .cloned()
        .collect();
    if let Some(slash) = items.iter().position(|c| c.is_delim('/')) {
        let a = items.get(slash + 1).cloned();
        return Some((items[..slash].to_vec(), a));
    }
    if items.len() == 4 {
        return Some((items[..3].to_vec(), Some(items[3].clone())));
    }
    Some((items, None))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = ((h % 360.0) + 360.0) % 360.0 / 360.0;
    let f = |n: f32| {
        let k = (n + h * 12.0) % 12.0;
        let a = s * l.min(1.0 - l);
        l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    (f(0.0), f(8.0), f(4.0))
}

fn srgb_gamma(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0031308 {
        12.92 * x
    } else {
        1.055 * libm_pow(x, 1.0 / 2.4) - 0.055
    }
}

fn libm_pow(x: f32, y: f32) -> f32 {
    crate::math::powf(x, y)
}

fn oklab_to_rgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let bb = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;
    (srgb_gamma(r), srgb_gamma(g), srgb_gamma(bb))
}

fn lab_to_rgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    // CIE Lab (D50) -> XYZ -> linear sRGB (D65, Bradford-adapted).
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let e = 216.0 / 24389.0;
    let k = 24389.0 / 27.0;
    let finv = |t: f32| {
        if t * t * t > e {
            t * t * t
        } else {
            (116.0 * t - 16.0) / k
        }
    };
    let (x, y, z) = (
        finv(fx) * 0.964_22,
        if l > k * e { fy * fy * fy } else { l / k },
        finv(fz) * 0.825_21,
    );
    let r = 3.134_136 * x - 1.617_386 * y - 0.490_662 * z;
    let g = -0.978_795 * x + 1.916_254 * y + 0.033_443 * z;
    let bb = 0.071_955 * x - 0.228_977 * y + 1.405_386 * z;
    (srgb_gamma(r), srgb_gamma(g), srgb_gamma(bb))
}

/// Parse a color value.
pub fn color(cv: &Cv) -> Option<Color> {
    match cv {
        Cv::Token(Token::Ident(name)) => {
            let n = name.to_ascii_lowercase();
            if n == "currentcolor" {
                return Some(Color::CurrentColor);
            }
            if n == "transparent" {
                return Some(Color::Rgba(Rgba::TRANSPARENT));
            }
            crate::colors::named(&n).map(Color::Rgba)
        }
        Cv::Token(Token::Hash { value, .. }) => hex(value).map(Color::Rgba),
        Cv::Function { name, args } => {
            let n = name.to_ascii_lowercase();
            let (c, a) = color_args(args)?;
            let a = match &a {
                Some(x) => alpha(x)?,
                None => 1.0,
            };
            let a8 = clamp_u8(a * 255.0);
            let rgb = match n.as_str() {
                "rgb" | "rgba" => {
                    if c.len() != 3 {
                        return None;
                    }
                    Rgba {
                        r: clamp_u8(channel(&c[0])?),
                        g: clamp_u8(channel(&c[1])?),
                        b: clamp_u8(channel(&c[2])?),
                        a: a8,
                    }
                }
                "hsl" | "hsla" => {
                    if c.len() != 3 {
                        return None;
                    }
                    let pct = |cv: &Cv| match cv {
                        Cv::Token(Token::Percentage(p)) => Some(p / 100.0),
                        Cv::Token(Token::Number { value, .. }) => Some(value / 100.0),
                        _ => None,
                    };
                    let (r, g, b) = hsl_to_rgb(
                        hue(&c[0])?,
                        pct(&c[1])?.clamp(0.0, 1.0),
                        pct(&c[2])?.clamp(0.0, 1.0),
                    );
                    Rgba {
                        r: clamp_u8(r * 255.0),
                        g: clamp_u8(g * 255.0),
                        b: clamp_u8(b * 255.0),
                        a: a8,
                    }
                }
                "hwb" => {
                    if c.len() != 3 {
                        return None;
                    }
                    let pct = |cv: &Cv| match cv {
                        Cv::Token(Token::Percentage(p)) => Some(p / 100.0),
                        Cv::Token(Token::Number { value, .. }) => Some(value / 100.0),
                        _ => None,
                    };
                    let (mut w, mut bl) = (pct(&c[1])?, pct(&c[2])?);
                    if w + bl > 1.0 {
                        let s = w + bl;
                        w /= s;
                        bl /= s;
                    }
                    let (r, g, b) = hsl_to_rgb(hue(&c[0])?, 1.0, 0.5);
                    let f = |x: f32| clamp_u8((x * (1.0 - w - bl) + w) * 255.0);
                    Rgba {
                        r: f(r),
                        g: f(g),
                        b: f(b),
                        a: a8,
                    }
                }
                "lab" | "lch" | "oklab" | "oklch" => {
                    if c.len() != 3 {
                        return None;
                    }
                    let ok = n.starts_with("ok");
                    let lmax = if ok { 1.0 } else { 100.0 };
                    let l = match &c[0] {
                        Cv::Token(Token::Percentage(p)) => p / 100.0 * lmax,
                        Cv::Token(Token::Number { value, .. }) => *value,
                        _ => return None,
                    };
                    let comp = |cv: &Cv, full: f32| match cv {
                        Cv::Token(Token::Percentage(p)) => Some(p / 100.0 * full),
                        Cv::Token(Token::Number { value, .. }) => Some(*value),
                        c if c.ident().is_some_and(|s| s.eq_ignore_ascii_case("none")) => Some(0.0),
                        _ => None,
                    };
                    let (a_, b_) = if n.ends_with("ch") {
                        let chroma = comp(&c[1], if ok { 0.4 } else { 150.0 })?;
                        let h = hue(&c[2])?.to_radians();
                        (chroma * crate::math::cosf(h), chroma * crate::math::sinf(h))
                    } else {
                        let full = if ok { 0.4 } else { 125.0 };
                        (comp(&c[1], full)?, comp(&c[2], full)?)
                    };
                    let (r, g, b) = if ok {
                        oklab_to_rgb(l, a_, b_)
                    } else {
                        lab_to_rgb(l, a_, b_)
                    };
                    Rgba {
                        r: clamp_u8(r * 255.0),
                        g: clamp_u8(g * 255.0),
                        b: clamp_u8(b * 255.0),
                        a: a8,
                    }
                }
                _ => return None,
            };
            Some(Color::Rgba(rgb))
        }
        _ => None,
    }
}

fn hex(s: &str) -> Option<Rgba> {
    let d: Vec<u8> = s
        .chars()
        .map(|c| c.to_digit(16).map(|v| v as u8))
        .collect::<Option<_>>()?;
    let dup = |x: u8| x * 17;
    Some(match d.len() {
        3 => Rgba::rgb(dup(d[0]), dup(d[1]), dup(d[2])),
        4 => Rgba {
            r: dup(d[0]),
            g: dup(d[1]),
            b: dup(d[2]),
            a: dup(d[3]),
        },
        6 => Rgba::rgb(d[0] * 16 + d[1], d[2] * 16 + d[3], d[4] * 16 + d[5]),
        8 => Rgba {
            r: d[0] * 16 + d[1],
            g: d[2] * 16 + d[3],
            b: d[4] * 16 + d[5],
            a: d[6] * 16 + d[7],
        },
        _ => return None,
    })
}
