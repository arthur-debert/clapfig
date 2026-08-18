//! The value model's datetime: TOML's four lexical forms, carried by
//! `toml_datetime`.
//!
//! Configuration datetimes follow the TOML baseline (ADR-0001): four forms —
//! offset date-time, local date-time, local date, local time — with
//! parse/display only. No timezone arithmetic, no calendar math: the type is
//! a validated lexical value, exactly what a config file can spell.
//!
//! The types are `toml_datetime`'s, re-exported. `toml_datetime` is a
//! standalone type crate, not a format crate, so this does not violate
//! ADR-0001's no-format-types rule; its grammar is TOML 1.0's (RFC 3339
//! spellings — `T`, `t`, or a single space between date and time; `Z`/`z`
//! or `±hh:mm` offsets; optional fractional seconds truncated beyond
//! nanosecond precision), and its `Display` spellings match what every
//! adapter serializes. The tests below pin that contract at the string
//! level.
//!
//! Formats without native datetimes (YAML, JSON) deliver these values as
//! strings; the schema-driven coercion pass parses them against this type's
//! grammar via [`FromStr`](std::str::FromStr), and every adapter serializes
//! with the same spellings via [`Display`](std::fmt::Display) — so all four
//! forms round-trip in every format.
//!
//! Serde has no datetime primitive, so [`Datetime`] rides a marker struct:
//! it serializes as a struct named [`DATETIME_NAME`] whose single field
//! [`DATETIME_FIELD`] holds the display string. The [`Value`](super::Value)
//! serializer and deserializer recognize the marker and rebuild the
//! datetime; every other serde format sees an ordinary (if oddly named)
//! one-field struct.

pub use toml_datetime::{Date, Datetime, DatetimeParseError, Offset, Time};

/// Marker struct name `Datetime` serializes under (see the [module
/// docs](self)).
pub(crate) use toml_datetime::__unstable::NAME as DATETIME_NAME;

/// Marker field key inside the marker struct (see the [module docs](self)).
pub(crate) use toml_datetime::__unstable::FIELD as DATETIME_FIELD;

#[cfg(test)]
mod tests {
    use super::Datetime;

    fn parse(s: &str) -> Datetime {
        s.parse().unwrap()
    }

    #[test]
    fn all_four_forms_round_trip_through_display() {
        for form in [
            "1979-05-27T07:32:00Z",
            "1979-05-27T00:32:00-07:00",
            "1979-05-27T07:32:00",
            "1979-05-27",
            "07:32:00",
        ] {
            assert_eq!(parse(form).to_string(), form);
        }
    }

    #[test]
    fn lowercase_t_and_z_accepted_and_normalized() {
        assert_eq!(
            parse("1979-05-27t07:32:00z").to_string(),
            "1979-05-27T07:32:00Z"
        );
    }

    #[test]
    fn space_separator_accepted_and_normalized() {
        assert_eq!(
            parse("1979-05-27 07:32:00Z").to_string(),
            "1979-05-27T07:32:00Z"
        );
    }

    #[test]
    fn fractional_seconds_display_without_trailing_zeros() {
        assert_eq!(parse("00:32:00.5").to_string(), "00:32:00.5");
        assert_eq!(
            parse("1979-05-27T00:32:00.999999-07:00").to_string(),
            "1979-05-27T00:32:00.999999-07:00"
        );
    }

    #[test]
    fn fraction_truncates_beyond_nanoseconds() {
        let dt = parse("07:32:00.1234567899");
        assert_eq!(dt.time.unwrap().nanosecond, 123_456_789);
        assert_eq!(dt.to_string(), "07:32:00.123456789");
    }

    #[test]
    fn negative_zero_offset_normalizes() {
        assert_eq!(
            parse("1979-05-27T07:32:00-00:00").to_string(),
            "1979-05-27T07:32:00+00:00"
        );
    }

    #[test]
    fn leap_rules_enforced() {
        assert!("23:59:60".parse::<Datetime>().is_ok());
        assert!("2000-02-29".parse::<Datetime>().is_ok());
        assert!("2024-02-29".parse::<Datetime>().is_ok());
        assert!("1900-02-29".parse::<Datetime>().is_err());
        assert!("2023-02-29".parse::<Datetime>().is_err());
    }

    #[test]
    fn malformed_inputs_rejected() {
        for bad in [
            "",
            "not a date",
            "1979-13-01",          // month out of range
            "1979-00-01",          // month zero
            "1979-05-32",          // day out of range
            "1979-05-00",          // day zero
            "24:00:00",            // hour out of range
            "07:60:00",            // minute out of range
            "07:32:61",            // second out of range
            "07:32",               // seconds are required
            "1979-05-27X07:32:00", // bad separator
            "1979-05-27T07:32:00X",
            "1979-05-27T07:32:00+25:00", // offset hour out of range
            "1979-05-27T07:32:00+07:60", // offset minute out of range
            "1979-05-27T07:32:00+0700",  // missing offset colon
            "07:32:00.",                 // empty fraction
            "07:32:00abc",               // trailing garbage
            "79-05-27",                  // two-digit year
            "1979-5-27",                 // one-digit month
        ] {
            assert!(
                bad.parse::<Datetime>().is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }
}
