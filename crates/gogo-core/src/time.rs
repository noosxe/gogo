//! The time formats gogo writes and reads.
//!
//! Every time in a file, a header, a dir name or `--json` output goes through
//! here, so that its text never changes by accident: older gogo builds read
//! what newer ones write, and the other way round. The golden strings in the
//! tests are the exact text the earlier chrono-based code wrote.
//!
//! The formats:
//! - [`secs_z`]: RFC 3339, whole seconds, `Z` (`2026-11-14T10:15:00Z`).
//! - [`autosi_z`]: RFC 3339 with 0, 3, 6 or 9 fraction digits, the fewest that
//!   keep every nonzero digit, then `Z` (`…:00.123400Z`). jiff's own
//!   `Display` trims to the fewest digits (`.1234`), which is a different text.
//! - [`autosi_offset`]: the same digits with `+00:00` for `Z`.

use std::time::SystemTime;

use jiff::tz::{Offset, TimeZone};
use jiff::{Timestamp, ZonedDateTime};

/// `2026-11-14T10:15:00Z`.
pub fn secs_z(t: Timestamp) -> String {
    format!("{t:.0}")
}

/// `2026-11-14T10:15:00.123400Z`.
pub fn autosi_z(t: Timestamp) -> String {
    format!("{t:.p$}", p = autosi_digits(t))
}

/// `2026-11-14T10:15:00.123400+00:00`.
pub fn autosi_offset(t: Timestamp) -> String {
    format!(
        "{:.p$}",
        t.display_with_offset(Offset::UTC),
        p = autosi_digits(t)
    )
}

fn autosi_digits(t: Timestamp) -> usize {
    match t.subsec_nanosecond() {
        0 => 0,
        n if n % 1_000_000 == 0 => 3,
        n if n % 1_000 == 0 => 6,
        _ => 9,
    }
}

/// An RFC 3339 time with any offset (`Z`, `+01:00`, …).
pub fn parse(s: &str) -> Option<Timestamp> {
    s.parse().ok()
}

/// A `SystemTime`, clamped into jiff's range (years ±9999): only a corrupt
/// mtime is out there, and clamping keeps every path's answer the same.
pub fn from_system(t: SystemTime) -> Timestamp {
    Timestamp::try_from(t).unwrap_or(if t >= SystemTime::UNIX_EPOCH {
        Timestamp::MAX
    } else {
        Timestamp::MIN
    })
}

/// Back to a `SystemTime`. On Linux and macOS it holds jiff's whole range.
pub fn to_system(t: Timestamp) -> SystemTime {
    SystemTime::try_from(t).expect("a 64-bit SystemTime holds every jiff time")
}

/// Now, to the whole second (truncated).
pub fn now_secs() -> Timestamp {
    Timestamp::from_second(Timestamp::now().as_second()).expect("now is in range")
}

/// TOML's own datetime for `t`, to the nanosecond (TOML renders the digits).
pub fn toml_datetime(t: Timestamp) -> toml::value::Datetime {
    autosi_z(t).parse().expect("RFC 3339 is a TOML datetime")
}

/// A TOML datetime with an offset; `None` for a local one.
pub fn of_toml(d: &toml::value::Datetime) -> Option<Timestamp> {
    parse(&d.to_string())
}

/// `t` in the system time zone, as `f` (`%Y-%m-%d %H:%M`). With no zone
/// found (a sandbox, a bare CI runner), jiff's unknown zone acts as UTC.
pub fn local(t: Timestamp, f: &str) -> String {
    in_zone(t, &TimeZone::system(), f)
}

fn in_zone(t: Timestamp, tz: &TimeZone, f: &str) -> String {
    t.to_zoned_datetime(tz.clone()).strftime(f).to_string()
}

/// Now in the system time zone, as `f`.
pub fn local_now(f: &str) -> String {
    ZonedDateTime::now().strftime(f).to_string()
}

/// A GOG checksum file's `timestamp="2026-03-13 08:45:39"`: a naive time, read
/// as UTC, in unix seconds (0 before the epoch).
pub fn naive_utc_secs(s: &str) -> Option<u64> {
    let t = jiff::civil::DateTime::strptime("%Y-%m-%d %H:%M:%S", s).ok()?;
    let secs = t
        .to_zoned_datetime(TimeZone::UTC)
        .ok()?
        .timestamp()
        .as_second();
    Some(secs.max(0) as u64)
}

/// `#[serde(with = "crate::time::serde_autosi")]`: an [`autosi_z`] string.
pub mod serde_autosi {
    use jiff::Timestamp;
    use serde::{Deserialize as _, Deserializer, Serializer, de};

    pub fn serialize<S: Serializer>(t: &Timestamp, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::autosi_z(*t))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Timestamp, D::Error> {
        let s = String::deserialize(d)?;
        super::parse(&s).ok_or_else(|| de::Error::custom(format!("not an RFC 3339 time: {s}")))
    }

    /// The same, for an `Option` (`null` for `None`).
    pub mod option {
        use jiff::Timestamp;
        use serde::{Deserialize as _, Deserializer, Serializer, de};

        pub fn serialize<S: Serializer>(t: &Option<Timestamp>, s: S) -> Result<S::Ok, S::Error> {
            match t {
                Some(t) => s.serialize_some(&super::super::autosi_z(*t)),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Timestamp>, D::Error> {
            Option::<String>::deserialize(d)?
                .map(|s| {
                    super::super::parse(&s)
                        .ok_or_else(|| de::Error::custom(format!("not an RFC 3339 time: {s}")))
                })
                .transpose()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    fn json(t: Timestamp) -> String {
        #[derive(serde::Serialize)]
        struct W(#[serde(with = "super::serde_autosi")] Timestamp);
        serde_json::to_string(&W(t)).unwrap()
    }

    fn utc_strftime(t: Timestamp, f: &str) -> String {
        t.strftime(f).to_string()
    }

    fn parse_ns(s: &str) -> Option<i128> {
        parse(s).map(Timestamp::as_nanosecond)
    }

    fn checksum(s: &str) -> Option<u64> {
        naive_utc_secs(s)
    }

    // ---- golden strings ----

    /// 2026-11-14T10:15:00Z.
    const SECS: i128 = 1_794_651_300;

    #[test]
    fn secs_z_drops_the_fraction() {
        assert_eq!(secs_z(ts("2026-11-14T10:15:00Z")), "2026-11-14T10:15:00Z");
        assert_eq!(
            secs_z(ts("2026-11-14T10:15:00.999999999Z")),
            "2026-11-14T10:15:00Z"
        );
    }

    #[test]
    fn autosi_z_prints_0_3_6_or_9_digits() {
        for (input, want) in [
            ("2026-11-14T10:15:00Z", "2026-11-14T10:15:00Z"),
            ("2026-11-14T10:15:00.123Z", "2026-11-14T10:15:00.123Z"),
            ("2026-11-14T10:15:00.1234Z", "2026-11-14T10:15:00.123400Z"),
            ("2026-11-14T10:15:00.5Z", "2026-11-14T10:15:00.500Z"),
            ("2026-11-14T10:15:00.000001Z", "2026-11-14T10:15:00.000001Z"),
            (
                "2026-11-14T10:15:00.12345678Z",
                "2026-11-14T10:15:00.123456780Z",
            ),
            (
                "2026-11-14T10:15:00.123456789Z",
                "2026-11-14T10:15:00.123456789Z",
            ),
        ] {
            assert_eq!(autosi_z(ts(input)), want, "{input}");
        }
    }

    #[test]
    fn autosi_offset_says_plus_zero() {
        for (input, want) in [
            ("2026-11-14T10:15:00Z", "2026-11-14T10:15:00+00:00"),
            (
                "2026-11-14T10:15:00.1234Z",
                "2026-11-14T10:15:00.123400+00:00",
            ),
            (
                "2026-11-14T10:15:00.123456789Z",
                "2026-11-14T10:15:00.123456789+00:00",
            ),
        ] {
            assert_eq!(autosi_offset(ts(input)), want, "{input}");
        }
    }

    #[test]
    fn json_is_a_quoted_autosi_z() {
        assert_eq!(
            json(ts("2026-11-14T10:15:00.1234Z")),
            "\"2026-11-14T10:15:00.123400Z\""
        );
        assert_eq!(json(ts("2026-11-14T10:15:00Z")), "\"2026-11-14T10:15:00Z\"");
    }

    #[test]
    fn stamps_and_dates() {
        let t = ts("2026-11-04T09:05:07.25Z");
        assert_eq!(utc_strftime(t, "%Y%m%dT%H%M%SZ"), "20261104T090507Z");
        assert_eq!(utc_strftime(t, "%Y%m%dT%H%M%S"), "20261104T090507");
        assert_eq!(utc_strftime(t, "%Y-%m-%d"), "2026-11-04");
        assert_eq!(utc_strftime(t, "%Y-%m-%d %H:%M"), "2026-11-04 09:05");
        assert_eq!(utc_strftime(t, "%Y-%m-%d %H:%M:%S"), "2026-11-04 09:05:07");
    }

    #[test]
    fn toml_renders_the_record_time_itself() {
        let d = toml_datetime(ts("2026-11-14T10:15:00.12345678Z"));
        assert_eq!(d.to_string(), "2026-11-14T10:15:00.12345678Z");
        let d = toml_datetime(ts("2026-11-14T10:15:00Z"));
        assert_eq!(d.to_string(), "2026-11-14T10:15:00Z");
    }

    #[test]
    fn old_text_still_parses() {
        let ns = SECS * 1_000_000_000;
        for (input, want) in [
            ("2026-11-14T10:15:00Z", ns),
            ("2026-11-14T10:15:00+00:00", ns),
            ("2026-11-14T11:15:00+01:00", ns),
            ("2026-11-14T05:15:00-05:00", ns),
            ("2026-11-14t10:15:00z", ns),
            ("2026-11-14 10:15:00Z", ns),
            ("2026-11-14T10:15:00.12345678Z", ns + 123_456_780),
            ("2026-11-14T10:15:00.123456789+00:00", ns + 123_456_789),
        ] {
            assert_eq!(parse_ns(input), Some(want), "{input}");
        }
        for bad in ["", "2026-11-14", "2026-11-14T10:15:00", "yesterday"] {
            assert_eq!(parse_ns(bad), None, "{bad}");
        }
    }

    #[test]
    fn checksum_timestamps_are_naive_utc() {
        assert_eq!(checksum("2026-03-13 08:45:39"), Some(1_773_391_539));
        assert_eq!(checksum("1969-12-31 23:59:59"), Some(0));
        assert_eq!(checksum("2026-03-13T08:45:39"), None);
    }
    // ---- what only the jiff side needs ----

    #[test]
    fn a_leap_second_clamps_to_59() {
        assert_eq!(
            parse("2026-12-31T23:59:60Z"),
            Some(ts("2026-12-31T23:59:59Z"))
        );
    }

    #[test]
    fn serde_round_trips_the_old_bytes() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct W {
            #[serde(with = "super::serde_autosi")]
            at: Timestamp,
            #[serde(with = "super::serde_autosi::option")]
            maybe: Option<Timestamp>,
        }
        let old = r#"{"at":"2026-11-14T10:15:00.123400Z","maybe":null}"#;
        let w: W = serde_json::from_str(old).unwrap();
        assert_eq!(w.at, ts("2026-11-14T10:15:00.1234Z"));
        assert_eq!(serde_json::to_string(&w).unwrap(), old);
        let w = W {
            maybe: Some(ts("2026-11-14T10:15:00Z")),
            ..w
        };
        assert_eq!(
            serde_json::to_string(&w).unwrap(),
            r#"{"at":"2026-11-14T10:15:00.123400Z","maybe":"2026-11-14T10:15:00Z"}"#
        );
        assert!(serde_json::from_str::<W>(r#"{"at":"nope","maybe":null}"#).is_err());
    }

    #[test]
    fn system_times_round_trip_and_clamp() {
        use std::time::Duration;
        let t = SystemTime::UNIX_EPOCH + Duration::new(1_794_651_300, 123_456_789);
        assert_eq!(from_system(t), ts("2026-11-14T10:15:00.123456789Z"));
        assert_eq!(to_system(from_system(t)), t);
        let far = SystemTime::UNIX_EPOCH + Duration::from_secs(400_000_000_000);
        assert_eq!(from_system(far), Timestamp::MAX);
        assert_eq!(
            toml_datetime(from_system(far)).to_string(),
            "9999-12-30T22:00:00.999999999Z"
        );
        let before = SystemTime::UNIX_EPOCH - Duration::from_secs(400_000_000_000);
        assert_eq!(from_system(before), Timestamp::MIN);
    }

    #[test]
    fn toml_round_trips() {
        let t = ts("2026-11-14T10:15:00.12345678Z");
        assert_eq!(of_toml(&toml_datetime(t)), Some(t));
        let local: toml::value::Datetime = "2026-11-14T10:15:00".parse().unwrap();
        assert_eq!(of_toml(&local), None);
    }

    #[test]
    fn local_formats_in_the_given_zone() {
        let t = ts("2026-01-15T10:15:00Z");
        let est = TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(in_zone(t, &est, "%Y-%m-%d %H:%M"), "2026-01-15 05:15");
        assert_eq!(
            in_zone(t, &TimeZone::UTC, "%Y-%m-%d %H:%M"),
            "2026-01-15 10:15"
        );
        assert_eq!(
            in_zone(t, &TimeZone::unknown(), "%Y-%m-%d %H:%M"),
            "2026-01-15 10:15"
        );
    }

    #[test]
    fn now_secs_has_no_fraction() {
        assert_eq!(now_secs().subsec_nanosecond(), 0);
    }
}
