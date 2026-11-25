//! The fixed formatters (design 26 §2): `fmt.bytes`, `fmt.rate`, `fmt.date`, `fmt.ago`,
//! `fmt.hours`, `fmt.eta`, `fmt.pct`; and the colour helper behind `@token alpha=x`.

use gogo_contract::{Value, value::fmt_num};

/// Bytes with decimal units: `18.6 GB`.
pub(super) fn bytes(n: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n;
    let mut u = 0;
    while v.abs() >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{} B", fmt_num(v.round()))
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// `48.2 MB/s`.
pub(super) fn rate(n: f64) -> String {
    format!("{}/s", bytes(n))
}

/// Seconds as whole hours: `41 h`.
pub(super) fn hours(secs: f64) -> String {
    format!("{} h", fmt_num((secs / 3600.0).round()))
}

/// Seconds left as minutes: `12 min left`.
pub(super) fn eta(secs: f64) -> String {
    format!("{} min left", fmt_num((secs / 60.0).round()))
}

/// A fraction as a percentage: `62%`.
pub(super) fn pct(f: f64) -> String {
    format!("{}%", fmt_num((f * 100.0).round()))
}

/// Civil date (UTC) from unix seconds, Howard Hinnant's algorithm. Times far outside any
/// real date are clamped, so the arithmetic cannot overflow.
pub(super) fn date(t: f64) -> String {
    const LIMIT: f64 = 1.0e12;
    let days = (t.clamp(-LIMIT, LIMIT) / 86400.0).floor() as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// How long ago, from an elapsed number of seconds: `2 days ago`.
pub(super) fn ago(secs: f64) -> String {
    let m = (secs / 60.0).round() as i64;
    match m {
        i64::MIN..=0 => "just now".into(),
        1..=59 => format!("{m} min ago"),
        60..=1439 => format!("{} h ago", m / 60),
        1440..=2879 => "yesterday".into(),
        _ => format!("{} days ago", m / 1440),
    }
}

/// `#rrggbb` (or `#rrggbbaa`, whose alpha is replaced) plus an alpha in 0..=1 gives
/// `#rrggbbaa`. `None` if `v` is not such a colour.
pub(super) fn with_alpha(v: &Value, a: f64) -> Option<Value> {
    let hex = v.as_str()?.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rgb = hex.get(..6)?;
    let a = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Value::Str(format!("#{rgb}{a:02x}")))
}
