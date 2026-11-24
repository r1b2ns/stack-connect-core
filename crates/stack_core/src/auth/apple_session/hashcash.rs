//! The `X-Apple-HC` proof of work `signin/complete` carries, exactly as
//! spaceship's `hashcash.rb` builds it.
//!
//! `GET idmsa/appleauth/auth/signin?widgetKey=…` answers with
//! `X-Apple-HC-Bits` (leading zero bits wanted) and `X-Apple-HC-Challenge`
//! (the resource). The header is
//! `1:{bits}:{YYYYMMDDhhmmss}:{challenge}::{counter}` for the first decimal
//! counter, from 0, whose SHA-1 starts with `bits` zero bits. Apple's format
//! is not standard hashcash: there is no random field and the counter is
//! decimal, not base 64 (see the comment in `hashcash.rb`).

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;
use sha1::{Digest, Sha1};

use super::error::AppleSignInError;

/// Difficulty above this is refused instead of computed: Apple asks for about
/// 10-11 bits, and an absurd value must not spin the CPU (each extra bit
/// doubles the work; spaceship has no bound and would loop forever above 160).
pub(crate) const MAX_BITS: u32 = 32;

/// Response header with the number of leading zero bits wanted.
pub(crate) const BITS_HEADER: &str = "x-apple-hc-bits";
/// Response header with the challenge (the hashcash resource).
pub(crate) const CHALLENGE_HEADER: &str = "x-apple-hc-challenge";
/// Request header carrying the solved stamp.
pub(crate) const HASHCASH_HEADER: &str = "X-Apple-HC";

/// One challenge Apple issued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HashcashChallenge {
    bits: u32,
    challenge: String,
}

impl HashcashChallenge {
    /// The challenge in `headers`, `None` when either header is missing or
    /// empty (spaceship then sends `signin/complete` without `X-Apple-HC`).
    ///
    /// # Errors
    /// [`AppleSignInError::Unexpected`] when the bits are not a number, are
    /// over [`MAX_BITS`], or the challenge is not plain visible ASCII (it is
    /// echoed into a request header).
    pub(crate) fn from_headers(headers: &HeaderMap) -> Result<Option<Self>, AppleSignInError> {
        let read = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
        };
        let (Some(bits), Some(challenge)) = (read(BITS_HEADER), read(CHALLENGE_HEADER)) else {
            return Ok(None);
        };
        let bits: u32 = bits.parse().map_err(|_| {
            AppleSignInError::unexpected(200, "the hashcash difficulty is not a number")
        })?;
        Self::new(bits, challenge).map(Some)
    }

    /// A challenge from its parts.
    ///
    /// # Errors
    /// As [`Self::from_headers`].
    pub(crate) fn new(bits: u32, challenge: &str) -> Result<Self, AppleSignInError> {
        if bits > MAX_BITS {
            return Err(AppleSignInError::unexpected(
                200,
                format!("the hashcash difficulty {bits} is out of range (at most {MAX_BITS})"),
            ));
        }
        if challenge.is_empty() || !challenge.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(AppleSignInError::unexpected(
                200,
                "the hashcash challenge is not plain ASCII",
            ));
        }
        Ok(Self {
            bits,
            challenge: challenge.to_string(),
        })
    }

    /// The `X-Apple-HC` value for this challenge at `date`
    /// (`YYYYMMDDhhmmss`, see [`date_stamp`]).
    pub(crate) fn solve(&self, date: &str) -> String {
        let prefix = format!("1:{}:{date}:{}::", self.bits, self.challenge);
        let mut counter: u64 = 0;
        loop {
            let stamp = format!("{prefix}{counter}");
            if leading_zero_bits(&Sha1::digest(stamp.as_bytes())) >= self.bits {
                return stamp;
            }
            // 2^64 attempts cannot happen below MAX_BITS (the expected count
            // is 2^bits); saturating keeps the arithmetic panic-free.
            counter = counter.saturating_add(1);
        }
    }
}

/// Leading zero bits of `digest`.
fn leading_zero_bits(digest: &[u8]) -> u32 {
    let mut bits = 0;
    for byte in digest {
        if *byte == 0 {
            bits += 8;
        } else {
            return bits + byte.leading_zeros();
        }
    }
    bits
}

/// `time` as `YYYYMMDDhhmmss` in UTC (spaceship uses `Time.now.strftime`, the
/// local time; UTC is used here because it needs no time zone database).
pub(crate) fn date_stamp(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian (Howard
/// Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    // `d` is in 1..=31 and `m` in 1..=12 by construction.
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;
    use std::time::Duration;

    /// Vectors from spaceship itself: the first two are `hashcash_spec.rb`'s,
    /// the others were produced offline by loading fastlane master's
    /// `hashcash.rb` with `Time#strftime` stubbed to the date shown (Ruby
    /// script `hashcash_vectors.rb`, no network).
    const VECTORS: [(u32, &str, &str, &str); 5] = [
        (
            11,
            "4d74fb15eb23f465f1f6fcbf534e5877",
            "20230223170600",
            "1:11:20230223170600:4d74fb15eb23f465f1f6fcbf534e5877::6373",
        ),
        (
            12,
            "f8b58554b2f22960fc0dc99aea342276",
            "20230223213732",
            "1:12:20230223213732:f8b58554b2f22960fc0dc99aea342276::2381",
        ),
        (
            10,
            "c0ffee00c0ffee00c0ffee00c0ffee00",
            "20261119120000",
            "1:10:20261119120000:c0ffee00c0ffee00c0ffee00c0ffee00::309",
        ),
        (
            16,
            "0123456789abcdef0123456789abcdef",
            "20261119120000",
            "1:16:20261119120000:0123456789abcdef0123456789abcdef::9060",
        ),
        (
            0,
            "0123456789abcdef0123456789abcdef",
            "20261119120000",
            "1:0:20261119120000:0123456789abcdef0123456789abcdef::0",
        ),
    ];

    #[test]
    fn solve_matches_spaceship_hashcash() {
        for (bits, challenge, date, expected) in VECTORS {
            let hc = HashcashChallenge::new(bits, challenge).unwrap();
            assert_eq!(hc.solve(date), expected, "{bits} bits");
        }
    }

    #[test]
    fn the_solution_is_the_first_counter_with_enough_zero_bits() {
        let (bits, challenge, date, expected) = VECTORS[0];
        let counter: u64 = expected.rsplit(':').next().unwrap().parse().unwrap();
        let prefix = format!("1:{bits}:{date}:{challenge}::");
        assert!(leading_zero_bits(&Sha1::digest(expected.as_bytes())) >= bits);
        for earlier in 0..counter {
            let stamp = format!("{prefix}{earlier}");
            assert!(leading_zero_bits(&Sha1::digest(stamp.as_bytes())) < bits);
        }
    }

    #[test]
    fn leading_zero_bits_counts_across_bytes() {
        assert_eq!(leading_zero_bits(&[0x80]), 0);
        assert_eq!(leading_zero_bits(&[0x01]), 7);
        assert_eq!(leading_zero_bits(&[0x00, 0x20]), 10);
        assert_eq!(leading_zero_bits(&[0x00, 0x00]), 16);
        assert_eq!(leading_zero_bits(&[]), 0);
    }

    fn headers(bits: Option<&'static str>, challenge: Option<&'static str>) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Some(bits) = bits {
            map.insert("X-Apple-HC-Bits", HeaderValue::from_static(bits));
        }
        if let Some(challenge) = challenge {
            map.insert("X-Apple-HC-Challenge", HeaderValue::from_static(challenge));
        }
        map
    }

    #[test]
    fn from_headers_reads_both_headers_case_insensitively() {
        let hc = HashcashChallenge::from_headers(&headers(Some("11"), Some("abc")))
            .unwrap()
            .unwrap();
        assert_eq!(hc, HashcashChallenge::new(11, "abc").unwrap());
    }

    #[test]
    fn a_missing_header_means_no_hashcash() {
        for map in [
            headers(None, Some("abc")),
            headers(Some("11"), None),
            headers(Some(" "), Some("abc")),
            HeaderMap::new(),
        ] {
            assert_eq!(HashcashChallenge::from_headers(&map).unwrap(), None);
        }
    }

    #[test]
    fn hostile_challenges_are_refused() {
        for map in [
            headers(Some("eleven"), Some("abc")),
            headers(Some("-1"), Some("abc")),
            headers(Some("33"), Some("abc")),
            headers(Some("4294967296"), Some("abc")),
            headers(Some("11"), Some("a b")),
        ] {
            assert!(matches!(
                HashcashChallenge::from_headers(&map),
                Err(AppleSignInError::Unexpected { .. })
            ));
        }
        // The bound itself is accepted.
        assert!(HashcashChallenge::new(MAX_BITS, "abc").is_ok());
    }

    #[test]
    fn date_stamp_is_utc_yyyymmddhhmmss() {
        assert_eq!(date_stamp(UNIX_EPOCH), "19700101000000");
        // 2023-02-23T17:06:00Z (the spec's date).
        assert_eq!(
            date_stamp(UNIX_EPOCH + Duration::from_secs(1_677_171_960)),
            "20230223170600"
        );
        // A leap day and the last second of a year.
        assert_eq!(
            date_stamp(UNIX_EPOCH + Duration::from_secs(1_709_208_000)),
            "20240229120000"
        );
        assert_eq!(
            date_stamp(UNIX_EPOCH + Duration::from_secs(1_798_761_599)),
            "20261231235959"
        );
        // Before the epoch clamps to it.
        assert_eq!(
            date_stamp(UNIX_EPOCH - Duration::from_secs(10)),
            "19700101000000"
        );
    }
}
