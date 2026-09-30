//! Small `no_std` float helpers (no libm dependency).

pub fn floorf(x: f32) -> f32 {
    let t = x as i64 as f32;
    if t > x { t - 1.0 } else { t }
}

pub fn roundf(x: f32) -> f32 {
    floorf(x + 0.5)
}

pub fn expf(x: f32) -> f32 {
    // e^x = 2^k * e^r with |r| <= ln2/2.
    if x > 88.0 {
        return f32::INFINITY;
    }
    if x < -88.0 {
        return 0.0;
    }
    let ln2 = core::f32::consts::LN_2;
    let k = floorf(x / ln2 + 0.5);
    let r = x - k * ln2;
    let mut term = 1.0f32;
    let mut sum = 1.0f32;
    for i in 1..12 {
        term *= r / i as f32;
        sum += term;
    }
    sum * f32::from_bits(((k as i32 + 127) as u32) << 23)
}

pub fn lnf(x: f32) -> f32 {
    if x <= 0.0 {
        return f32::NEG_INFINITY;
    }
    // x = m * 2^e with m in [1, 2); ln m by atanh series.
    let bits = x.to_bits();
    let e = ((bits >> 23) & 0xFF) as i32 - 127;
    let m = f32::from_bits((bits & 0x007F_FFFF) | 0x3F80_0000);
    let y = (m - 1.0) / (m + 1.0);
    let y2 = y * y;
    let mut term = y;
    let mut sum = 0.0;
    let mut k = 1.0;
    for _ in 0..10 {
        sum += term / k;
        term *= y2;
        k += 2.0;
    }
    2.0 * sum + e as f32 * core::f32::consts::LN_2
}

pub fn powf(x: f32, y: f32) -> f32 {
    if x == 0.0 {
        return 0.0;
    }
    expf(y * lnf(x))
}

pub fn sinf(x: f32) -> f32 {
    let tau = core::f32::consts::TAU;
    let mut r = x - floorf(x / tau) * tau;
    if r > core::f32::consts::PI {
        r -= tau;
    }
    let r2 = r * r;
    // Taylor series to x^11.
    r * (1.0
        - r2 / 6.0 * (1.0 - r2 / 20.0 * (1.0 - r2 / 42.0 * (1.0 - r2 / 72.0 * (1.0 - r2 / 110.0)))))
}

pub fn cosf(x: f32) -> f32 {
    sinf(x + core::f32::consts::FRAC_PI_2)
}

pub fn sqrtf(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut g = f32::from_bits((x.to_bits() >> 1) + 0x1FC0_0000);
    for _ in 0..4 {
        g = 0.5 * (g + x / g);
    }
    g
}
