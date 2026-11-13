//! Validating deserializers for config values, for
//! `#[serde(deserialize_with = "...")]`.

use serde::{Deserialize, Deserializer, de::Error as _};

/// RRGGBB or RRGGBBAA, optionally with a leading '#', as 0xRRGGBBAA.
pub fn parse_color(s: &str) -> Result<u32, String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    let invalid = || format!("expected RRGGBB or RRGGBBAA, got '{s}'");
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let v = u32::from_str_radix(hex, 16).map_err(|_| invalid())?;
    match hex.len() {
        6 => Ok(v << 8 | 0xff),
        8 => Ok(v),
        _ => Err(invalid()),
    }
}

/// A color string; see `parse_color`.
pub fn color<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    parse_color(&String::deserialize(d)?).map_err(D::Error::custom)
}

/// Pixels, 0 to 100000.
pub fn px<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let v = i64::deserialize(d)?;
    u32::try_from(v)
        .ok()
        .filter(|&v| v <= 100_000)
        .ok_or_else(|| D::Error::custom(format!("expected 0-100000, got {v}")))
}

fn check_signed_px<E: serde::de::Error>(v: i64) -> Result<i32, E> {
    i32::try_from(v)
        .ok()
        .filter(|v| v.abs() <= 100_000)
        .ok_or_else(|| E::custom(format!("expected -100000-100000, got {v}")))
}

/// Pixels, -100000 to 100000.
pub fn signed_px<'de, D: Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
    check_signed_px(i64::deserialize(d)?)
}

/// `[x, y]` in signed pixels.
pub fn offset<'de, D: Deserializer<'de>>(d: D) -> Result<[i32; 2], D::Error> {
    let [x, y] = <[i64; 2]>::deserialize(d)?;
    Ok([check_signed_px(x)?, check_signed_px(y)?])
}

/// An integer from `min` to `max`.
pub fn bounded<'de, D: Deserializer<'de>>(d: D, min: u32, max: u32) -> Result<u32, D::Error> {
    let v = i64::deserialize(d)?;
    u32::try_from(v)
        .ok()
        .filter(|v| (min..=max).contains(v))
        .ok_or_else(|| D::Error::custom(format!("expected {min}-{max}, got {v}")))
}

/// A font size, 1 to 1000.
pub fn font_size<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    bounded(d, 1, 1000)
}

/// Milliseconds, 0 to 10000.
pub fn duration<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    bounded(d, 0, 10_000)
}

/// 0 to 1.
pub fn unit<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(d)?;
    if (0.0..=1.0).contains(&v) {
        Ok(v)
    } else {
        Err(D::Error::custom(format!("expected 0-1, got {v}")))
    }
}

/// More than 0, at most 1.
pub fn fraction<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(d)?;
    if v > 0.0 && v <= 1.0 {
        Ok(v)
    } else {
        Err(D::Error::custom(format!(
            "expected more than 0, at most 1, got {v}"
        )))
    }
}

/// A zoom factor, 1 to 2.
pub fn zoom<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(d)?;
    if (1.0..=2.0).contains(&v) {
        Ok(v)
    } else {
        Err(D::Error::custom(format!("expected 1-2, got {v}")))
    }
}

/// A positive, finite number.
pub fn positive<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(d)?;
    if v > 0.0 && v.is_finite() {
        Ok(v)
    } else {
        Err(D::Error::custom(format!(
            "expected a positive number, got {v}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("000000b3"), Ok(0x000000b3));
        assert_eq!(parse_color("#4c8dff"), Ok(0x4c8dffff));
        assert!(parse_color("+4c8dff").is_err());
        assert!(parse_color("4c8df").is_err());
        assert!(parse_color("4c8dffff00").is_err());
    }
}
