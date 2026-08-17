//! Owned datetime type mirroring TOML's four lexical forms.
//!
//! Configuration datetimes follow the TOML baseline (ADR-0001): four forms —
//! offset date-time, local date-time, local date, local time — with
//! parse/display only. No timezone arithmetic, no calendar math: the type is
//! a validated lexical value, exactly what a config file can spell.
//!
//! Formats without native datetimes (YAML, JSON) deliver these values as
//! strings; the schema-driven coercion pass parses them against this type's
//! grammar via [`FromStr`], and every adapter serializes with the same
//! spellings via [`Display`](std::fmt::Display) — so all four forms
//! round-trip in every format.
//!
//! The grammar is TOML 1.0's (RFC 3339 spellings): `T`, `t`, or a single
//! space separate date and time; `Z`/`z` or `±hh:mm` express the offset;
//! fractional seconds are optional and truncated beyond nanosecond
//! precision.

use std::fmt;
use std::str::FromStr;

use serde::de;
use serde::ser::SerializeStruct;

/// Marker name used to smuggle [`Datetime`] values through serde.
///
/// Serde's data model has no datetime primitive, so — following the `toml`
/// crate's pattern — [`Datetime`] serializes as a struct with this magic
/// name whose single field (also this name) holds the display string. The
/// [`Value`](super::Value) serializer and deserializer recognize the marker
/// and rebuild the datetime; every other serde format sees an ordinary
/// (if oddly named) one-field struct.
pub(crate) const DATETIME_MARKER: &str = "$__clapfig_private_Datetime";

/// A TOML-baseline datetime: one of the four lexical forms.
///
/// | Form | `date` | `time` | `offset` | Example |
/// | --- | --- | --- | --- | --- |
/// | Offset date-time | yes | yes | yes | `1979-05-27T07:32:00Z` |
/// | Local date-time | yes | yes | — | `1979-05-27T07:32:00` |
/// | Local date | yes | — | — | `1979-05-27` |
/// | Local time | — | yes | — | `07:32:00` |
///
/// Invariant (upheld by [`FromStr`], expected of hand-built values): at
/// least one of `date`/`time` is present, and `offset` only appears
/// alongside both `date` and `time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Datetime {
    /// Calendar date component, absent for the local-time form.
    pub date: Option<Date>,
    /// Time-of-day component, absent for the local-date form.
    pub time: Option<Time>,
    /// UTC offset, present only for the offset date-time form.
    pub offset: Option<Offset>,
}

/// Calendar date component of a [`Datetime`] (`YYYY-MM-DD`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Date {
    /// Four-digit year (0–9999).
    pub year: u16,
    /// Month of the year (1–12).
    pub month: u8,
    /// Day of the month (1–31, validated against month and leap years).
    pub day: u8,
}

/// Time-of-day component of a [`Datetime`] (`HH:MM:SS[.fraction]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Time {
    /// Hour (0–23).
    pub hour: u8,
    /// Minute (0–59).
    pub minute: u8,
    /// Second (0–60; 60 permits RFC 3339 leap seconds).
    pub second: u8,
    /// Fractional second in nanoseconds (0–999,999,999). Input digits
    /// beyond nanosecond precision are truncated at parse.
    pub nanosecond: u32,
}

/// UTC offset of an offset date-time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Offset {
    /// The `Z` suffix: UTC.
    Z,
    /// A numeric `±hh:mm` offset, stored as signed minutes east of UTC.
    /// `-00:00` parses to zero minutes and displays as `+00:00`.
    Custom {
        /// Total offset in minutes (`-1439..=1439`).
        minutes: i16,
    },
}

/// Error returned when a string is not one of the four TOML datetime forms.
///
/// Carries a static reason describing which rule the input broke — enough
/// for a type-error message naming the offending value, which is how the
/// schema-driven coercion pass surfaces it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid datetime: {reason}")]
pub struct DatetimeParseError {
    reason: &'static str,
}

impl DatetimeParseError {
    fn new(reason: &'static str) -> Self {
        DatetimeParseError { reason }
    }
}

/// Number of days in `month` of `year`, accounting for leap years.
fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap =
                (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

/// Parse exactly `N` ASCII digits into a number.
fn digits<const N: usize>(b: &[u8], reason: &'static str) -> Result<u32, DatetimeParseError> {
    if b.len() != N {
        return Err(DatetimeParseError::new(reason));
    }
    let mut out: u32 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            return Err(DatetimeParseError::new(reason));
        }
        out = out * 10 + u32::from(c - b'0');
    }
    Ok(out)
}

/// Parse a full `YYYY-MM-DD` date from exactly ten bytes.
fn parse_date(b: &[u8]) -> Result<Date, DatetimeParseError> {
    debug_assert_eq!(b.len(), 10);
    if b[4] != b'-' || b[7] != b'-' {
        return Err(DatetimeParseError::new("expected YYYY-MM-DD date"));
    }
    let year = digits::<4>(&b[0..4], "expected four-digit year")? as u16;
    let month = digits::<2>(&b[5..7], "expected two-digit month")? as u8;
    let day = digits::<2>(&b[8..10], "expected two-digit day")? as u8;
    if !(1..=12).contains(&month) {
        return Err(DatetimeParseError::new("month out of range (1-12)"));
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err(DatetimeParseError::new("day out of range for month"));
    }
    Ok(Date { year, month, day })
}

/// Parse an `HH:MM:SS[.fraction]` time from the front of `b`.
///
/// Returns the time and the number of bytes consumed (the fraction has no
/// fixed length).
fn parse_time(b: &[u8]) -> Result<(Time, usize), DatetimeParseError> {
    if b.len() < 8 || b[2] != b':' || b[5] != b':' {
        return Err(DatetimeParseError::new("expected HH:MM:SS time"));
    }
    let hour = digits::<2>(&b[0..2], "expected two-digit hour")? as u8;
    let minute = digits::<2>(&b[3..5], "expected two-digit minute")? as u8;
    let second = digits::<2>(&b[6..8], "expected two-digit second")? as u8;
    if hour > 23 {
        return Err(DatetimeParseError::new("hour out of range (0-23)"));
    }
    if minute > 59 {
        return Err(DatetimeParseError::new("minute out of range (0-59)"));
    }
    if second > 60 {
        return Err(DatetimeParseError::new("second out of range (0-60)"));
    }
    let mut consumed = 8;
    let mut nanosecond: u32 = 0;
    if b.len() > 8 && b[8] == b'.' {
        let frac = &b[9..];
        let digit_count = frac.iter().take_while(|c| c.is_ascii_digit()).count();
        if digit_count == 0 {
            return Err(DatetimeParseError::new(
                "expected at least one fractional-second digit",
            ));
        }
        for &c in frac.iter().take(digit_count.min(9)) {
            nanosecond = nanosecond * 10 + u32::from(c - b'0');
        }
        // Scale e.g. ".5" to 500_000_000 ns; digits beyond nine are truncated.
        for _ in digit_count..9 {
            nanosecond *= 10;
        }
        consumed = 9 + digit_count;
    }
    Ok((
        Time {
            hour,
            minute,
            second,
            nanosecond,
        },
        consumed,
    ))
}

/// Parse a `Z`/`z` or `±hh:mm` offset occupying all of `b`.
fn parse_offset(b: &[u8]) -> Result<Offset, DatetimeParseError> {
    match b {
        [b'Z'] | [b'z'] => Ok(Offset::Z),
        [sign @ (b'+' | b'-'), rest @ ..] => {
            if rest.len() != 5 || rest[2] != b':' {
                return Err(DatetimeParseError::new("expected ±hh:mm offset"));
            }
            let hours = digits::<2>(&rest[0..2], "expected two-digit offset hour")? as i16;
            let minutes = digits::<2>(&rest[3..5], "expected two-digit offset minute")? as i16;
            if hours > 23 {
                return Err(DatetimeParseError::new("offset hour out of range (0-23)"));
            }
            if minutes > 59 {
                return Err(DatetimeParseError::new("offset minute out of range (0-59)"));
            }
            let total = hours * 60 + minutes;
            let total = if *sign == b'-' { -total } else { total };
            Ok(Offset::Custom { minutes: total })
        }
        _ => Err(DatetimeParseError::new("expected Z or ±hh:mm offset")),
    }
}

impl FromStr for Datetime {
    type Err = DatetimeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let b = s.as_bytes();
        if b.is_empty() {
            return Err(DatetimeParseError::new("empty string"));
        }
        // A date form has `-` at byte 4; a bare time has `:` at byte 2.
        let looks_like_date = b.len() >= 10 && b[4] == b'-';
        if looks_like_date {
            let date = parse_date(&b[..10])?;
            if b.len() == 10 {
                return Ok(Datetime {
                    date: Some(date),
                    time: None,
                    offset: None,
                });
            }
            if !matches!(b[10], b'T' | b't' | b' ') {
                return Err(DatetimeParseError::new(
                    "expected 'T' or space between date and time",
                ));
            }
            let rest = &b[11..];
            let (time, consumed) = parse_time(rest)?;
            let offset_bytes = &rest[consumed..];
            let offset = if offset_bytes.is_empty() {
                None
            } else {
                Some(parse_offset(offset_bytes)?)
            };
            Ok(Datetime {
                date: Some(date),
                time: Some(time),
                offset,
            })
        } else {
            let (time, consumed) = parse_time(b)?;
            if consumed != b.len() {
                return Err(DatetimeParseError::new("trailing characters after time"));
            }
            Ok(Datetime {
                date: None,
                time: Some(time),
                offset: None,
            })
        }
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.hour, self.minute, self.second)?;
        if self.nanosecond > 0 {
            let frac = format!("{:09}", self.nanosecond);
            write!(f, ".{}", frac.trim_end_matches('0'))?;
        }
        Ok(())
    }
}

impl fmt::Display for Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Offset::Z => f.write_str("Z"),
            Offset::Custom { minutes } => {
                let sign = if *minutes < 0 { '-' } else { '+' };
                let abs = minutes.unsigned_abs();
                write!(f, "{}{:02}:{:02}", sign, abs / 60, abs % 60)
            }
        }
    }
}

impl fmt::Display for Datetime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(date) = &self.date {
            write!(f, "{date}")?;
        }
        if self.date.is_some() && self.time.is_some() {
            f.write_str("T")?;
        }
        if let Some(time) = &self.time {
            write!(f, "{time}")?;
        }
        if let Some(offset) = &self.offset {
            write!(f, "{offset}")?;
        }
        Ok(())
    }
}

impl serde::Serialize for Datetime {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // The marker-struct dance: see [`DATETIME_MARKER`].
        let mut s = serializer.serialize_struct(DATETIME_MARKER, 1)?;
        s.serialize_field(DATETIME_MARKER, &self.to_string())?;
        s.end()
    }
}

impl<'de> de::Deserialize<'de> for Datetime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct DatetimeVisitor;

        impl<'de> de::Visitor<'de> for DatetimeVisitor {
            type Value = Datetime;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a TOML-form datetime")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Datetime, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let key: Option<DatetimeKey> = map.next_key()?;
                if key.is_none() {
                    return Err(de::Error::custom("datetime key not found"));
                }
                let repr: String = map.next_value()?;
                repr.parse().map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_struct(DATETIME_MARKER, &[DATETIME_MARKER], DatetimeVisitor)
    }
}

/// Deserialization guard for the datetime marker key: succeeds only on the
/// exact [`DATETIME_MARKER`] string, so an ordinary map never masquerades
/// as a datetime.
struct DatetimeKey;

impl<'de> de::Deserialize<'de> for DatetimeKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct KeyVisitor;

        impl de::Visitor<'_> for KeyVisitor {
            type Value = ();

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a valid datetime field")
            }

            fn visit_str<E>(self, s: &str) -> Result<(), E>
            where
                E: de::Error,
            {
                if s == DATETIME_MARKER {
                    Ok(())
                } else {
                    Err(de::Error::custom("expected the datetime marker field"))
                }
            }
        }

        deserializer.deserialize_identifier(KeyVisitor)?;
        Ok(DatetimeKey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Datetime {
        s.parse().unwrap()
    }

    #[test]
    fn offset_datetime_z() {
        let dt = parse("1979-05-27T07:32:00Z");
        assert_eq!(
            dt.date,
            Some(Date {
                year: 1979,
                month: 5,
                day: 27
            })
        );
        assert_eq!(
            dt.time,
            Some(Time {
                hour: 7,
                minute: 32,
                second: 0,
                nanosecond: 0
            })
        );
        assert_eq!(dt.offset, Some(Offset::Z));
        assert_eq!(dt.to_string(), "1979-05-27T07:32:00Z");
    }

    #[test]
    fn offset_datetime_numeric_offset() {
        let dt = parse("1979-05-27T00:32:00-07:00");
        assert_eq!(dt.offset, Some(Offset::Custom { minutes: -420 }));
        assert_eq!(dt.to_string(), "1979-05-27T00:32:00-07:00");
    }

    #[test]
    fn offset_datetime_fractional_seconds() {
        let dt = parse("1979-05-27T00:32:00.999999-07:00");
        assert_eq!(dt.time.unwrap().nanosecond, 999_999_000);
        assert_eq!(dt.to_string(), "1979-05-27T00:32:00.999999-07:00");
    }

    #[test]
    fn local_datetime() {
        let dt = parse("1979-05-27T07:32:00");
        assert!(dt.date.is_some());
        assert!(dt.time.is_some());
        assert_eq!(dt.offset, None);
        assert_eq!(dt.to_string(), "1979-05-27T07:32:00");
    }

    #[test]
    fn local_date() {
        let dt = parse("1979-05-27");
        assert!(dt.date.is_some());
        assert_eq!(dt.time, None);
        assert_eq!(dt.offset, None);
        assert_eq!(dt.to_string(), "1979-05-27");
    }

    #[test]
    fn local_time() {
        let dt = parse("07:32:00");
        assert_eq!(dt.date, None);
        assert!(dt.time.is_some());
        assert_eq!(dt.to_string(), "07:32:00");
    }

    #[test]
    fn local_time_fractional() {
        let dt = parse("00:32:00.5");
        assert_eq!(dt.time.unwrap().nanosecond, 500_000_000);
        assert_eq!(dt.to_string(), "00:32:00.5");
    }

    #[test]
    fn lowercase_t_and_z_accepted() {
        assert_eq!(
            parse("1979-05-27t07:32:00z").to_string(),
            "1979-05-27T07:32:00Z"
        );
    }

    #[test]
    fn space_separator_accepted() {
        assert_eq!(
            parse("1979-05-27 07:32:00Z").to_string(),
            "1979-05-27T07:32:00Z"
        );
    }

    #[test]
    fn fraction_truncates_beyond_nanoseconds() {
        let dt = parse("07:32:00.1234567899");
        assert_eq!(dt.time.unwrap().nanosecond, 123_456_789);
    }

    #[test]
    fn leap_day_valid_only_in_leap_years() {
        assert!("2000-02-29".parse::<Datetime>().is_ok());
        assert!("2024-02-29".parse::<Datetime>().is_ok());
        assert!("1900-02-29".parse::<Datetime>().is_err());
        assert!("2023-02-29".parse::<Datetime>().is_err());
    }

    #[test]
    fn negative_zero_offset_normalizes() {
        let dt = parse("1979-05-27T07:32:00-00:00");
        assert_eq!(dt.offset, Some(Offset::Custom { minutes: 0 }));
        assert_eq!(dt.to_string(), "1979-05-27T07:32:00+00:00");
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

    #[test]
    fn leap_second_accepted() {
        assert!("23:59:60".parse::<Datetime>().is_ok());
    }
}
