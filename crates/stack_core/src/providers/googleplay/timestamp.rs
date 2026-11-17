//! Google's JSON number encodings: the Android Publisher `Timestamp` message
//! (`{"seconds": "…", "nanos": …}`) turned into the ISO8601 strings the core
//! hands to hosts, and int64 values that proto3 JSON sends as strings.
//!
//! The date math is self-contained (no date crate), like the rest of the core,
//! which treats dates as opaque strings.

use serde::Deserialize;

/// Seconds in a day.
const SECONDS_PER_DAY: i64 = 86_400;
/// The years [`unix_seconds_to_iso8601`] renders: four digits, no year zero
/// (common ISO8601 parsers, Swift's included, stop there).
const YEARS: std::ops::RangeInclusive<i64> = 1..=9999;

/// An int64 that Google sends as a JSON string (proto3 JSON) or, in some
/// responses, as a plain number. Both are accepted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub(super) enum Int64 {
    Text(String),
    Number(i64),
}

impl Int64 {
    /// The numeric value; `None` when the text is not an integer.
    pub(super) fn value(&self) -> Option<i64> {
        match self {
            Self::Text(text) => text.trim().parse().ok(),
            Self::Number(number) => Some(*number),
        }
    }

    /// The value as decimal text: Google's own text verbatim, or the number
    /// formatted.
    pub(super) fn into_text(self) -> String {
        match self {
            Self::Text(text) => text,
            Self::Number(number) => number.to_string(),
        }
    }
}

/// An Android Publisher `Timestamp`: seconds since the Unix epoch (UTC) plus a
/// nanosecond fraction.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub(super) struct Timestamp {
    #[serde(default)]
    seconds: Option<Int64>,
    #[serde(default)]
    nanos: Option<i64>,
}

impl Timestamp {
    /// `(seconds, nanos)` for ordering; `None` when `seconds` is absent or not
    /// an integer.
    pub(super) fn instant(&self) -> Option<(i64, i64)> {
        let seconds = self.seconds.as_ref()?.value()?;
        Some((seconds, self.nanos.unwrap_or(0)))
    }

    /// The instant as `YYYY-MM-DDTHH:MM:SSZ`. The sub-second part is dropped so
    /// that a default `ISO8601DateFormatter` (no fractional seconds) parses it.
    /// `None` when `seconds` is absent, not an integer, or out of range (see
    /// [`unix_seconds_to_iso8601`]).
    pub(super) fn to_iso8601(&self) -> Option<String> {
        unix_seconds_to_iso8601(self.instant()?.0)
    }
}

/// Formats Unix `seconds` (UTC) as `YYYY-MM-DDTHH:MM:SSZ`, or `None` when the
/// date falls outside the years 0001–9999.
///
/// Days-to-civil-date conversion after Howard Hinnant's `civil_from_days`. No
/// step can overflow for any `i64` input: `days` is at most about `1.1e14`.
pub(super) fn unix_seconds_to_iso8601(seconds: i64) -> Option<String> {
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let second_of_day = seconds.rem_euclid(SECONDS_PER_DAY);
    let (hour, minute, second) = (
        second_of_day / 3600,
        (second_of_day % 3600) / 60,
        second_of_day % 60,
    );

    // Shift the epoch to 0000-03-01 so leap days fall at the end of a year.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    YEARS
        .contains(&year)
        .then(|| format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        // Expected values computed independently (Python `datetime`).
        for (seconds, expected) in [
            (0, "1970-01-01T00:00:00Z"),
            (1_452_717_656, "2016-01-13T20:40:56Z"),
            (1_700_000_000, "2023-11-14T22:13:20Z"),
            (946_684_799, "1999-12-31T23:59:59Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (951_868_800, "2000-03-01T00:00:00Z"),
            (1_709_208_000, "2024-02-29T12:00:00Z"),
            (-1, "1969-12-31T23:59:59Z"),
            (-62_135_596_800, "0001-01-01T00:00:00Z"),
            (253_402_300_799, "9999-12-31T23:59:59Z"),
        ] {
            assert_eq!(
                unix_seconds_to_iso8601(seconds).as_deref(),
                Some(expected),
                "seconds = {seconds}"
            );
        }
    }

    #[test]
    fn out_of_range_years_are_none() {
        for seconds in [-62_135_596_801, 253_402_300_800, i64::MIN, i64::MAX] {
            assert_eq!(
                unix_seconds_to_iso8601(seconds),
                None,
                "seconds = {seconds}"
            );
        }
    }

    #[test]
    fn timestamp_accepts_string_or_number_seconds_and_drops_nanos() {
        let text: Timestamp =
            serde_json::from_str(r#"{"seconds":"1452717656","nanos":123000000}"#).unwrap();
        assert_eq!(text.to_iso8601().as_deref(), Some("2016-01-13T20:40:56Z"));
        assert_eq!(text.instant(), Some((1_452_717_656, 123_000_000)));

        let number: Timestamp = serde_json::from_str(r#"{"seconds":1452717656}"#).unwrap();
        assert_eq!(number.to_iso8601().as_deref(), Some("2016-01-13T20:40:56Z"));
        assert_eq!(number.instant(), Some((1_452_717_656, 0)));
    }

    #[test]
    fn timestamp_without_usable_seconds_has_no_date() {
        for json in [r#"{}"#, r#"{"nanos":5}"#, r#"{"seconds":"soon"}"#] {
            let timestamp: Timestamp = serde_json::from_str(json).unwrap();
            assert_eq!(timestamp.instant(), None, "{json}");
            assert_eq!(timestamp.to_iso8601(), None, "{json}");
        }
    }

    #[test]
    fn int64_text_and_number() {
        let text: Int64 = serde_json::from_str(r#""9007199254740993""#).unwrap();
        assert_eq!(text.value(), Some(9_007_199_254_740_993));
        assert_eq!(text.into_text(), "9007199254740993");

        let number: Int64 = serde_json::from_str("42").unwrap();
        assert_eq!(number.value(), Some(42));
        assert_eq!(number.into_text(), "42");

        let garbage: Int64 = serde_json::from_str(r#""4x2""#).unwrap();
        assert_eq!(garbage.value(), None);
        // Passed through verbatim: the core does not reinterpret Google's text.
        assert_eq!(garbage.into_text(), "4x2");
    }
}
