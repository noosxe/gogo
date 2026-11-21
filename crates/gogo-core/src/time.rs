//! The time formats gogo writes and reads.
//!
//! Every time in a file, a header, a dir name or `--json` output goes through
//! here, so that its text never changes by accident: older gogo builds read
//! what newer ones write, and the other way round.
//!
//! The golden strings below are the exact text the chrono-based code wrote.

#[cfg(test)]
mod tests {
    use chrono::{DateTime, SecondsFormat, Utc};

    // ---- the code under test: what each call site does today ----

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn secs_z(t: DateTime<Utc>) -> String {
        t.to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    fn autosi_z(t: DateTime<Utc>) -> String {
        t.to_rfc3339_opts(SecondsFormat::AutoSi, true)
    }

    fn autosi_offset(t: DateTime<Utc>) -> String {
        t.to_rfc3339()
    }

    fn json(t: DateTime<Utc>) -> String {
        serde_json::to_string(&t).unwrap()
    }

    fn utc_strftime(t: DateTime<Utc>, f: &str) -> String {
        t.format(f).to_string()
    }

    fn toml_datetime(t: DateTime<Utc>) -> toml::value::Datetime {
        autosi_z(t).parse().unwrap()
    }

    fn parse(s: &str) -> Option<i128> {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .and_then(|t| t.timestamp_nanos_opt())
            .map(i128::from)
    }

    fn checksum(s: &str) -> Option<u64> {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
            .ok()
            .map(|t| t.and_utc().timestamp().max(0) as u64)
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
            assert_eq!(parse(input), Some(want), "{input}");
        }
        for bad in ["", "2026-11-14", "2026-11-14T10:15:00", "yesterday"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn checksum_timestamps_are_naive_utc() {
        assert_eq!(checksum("2026-03-13 08:45:39"), Some(1_773_391_539));
        assert_eq!(checksum("1969-12-31 23:59:59"), Some(0));
        assert_eq!(checksum("2026-03-13T08:45:39"), None);
    }
}
