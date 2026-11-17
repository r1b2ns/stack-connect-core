//! Redaction rules for everything that reaches the host log sink.
//!
//! [`scrub`] is the safety net every emitted line goes through. It removes:
//!
//! - PEM blocks (`-----BEGIN … -----END …-----`), raw or JSON-escaped;
//! - `Bearer <token>` credentials;
//! - JWTs (`eyJ…​.…​.…`) and Google OAuth access tokens (`ya29.…`);
//! - the values of secret-looking keys (`access_token`, `assertion`,
//!   `private_key`, `private_key_id`, `client_secret`, …) in JSON or form
//!   bodies;
//! - the local part of every e-mail address (`ana@example.com` →
//!   `a***@example.com`).
//!
//! Call sites must still never format a secret in the first place; this only
//! catches what a remote error message might echo back.

/// Replaces a removed PEM block.
const REDACTED_PEM: &str = "[redacted PEM]";
/// Replaces a removed JWT or access token.
const REDACTED_TOKEN: &str = "[redacted token]";
/// Replaces a removed secret value.
const REDACTED: &str = "[redacted]";

/// Keys whose values are secrets wherever they appear (`key: value`,
/// `"key": "value"`, `key=value`). Matched case-insensitively, on a word
/// boundary.
const SECRET_KEYS: [&str; 9] = [
    "access_token",
    "refresh_token",
    "id_token",
    "assertion",
    "private_key_id",
    "private_key",
    "client_secret",
    "password",
    "secret",
];

/// Applies every redaction rule (see the module docs) to `text`.
pub(crate) fn scrub(text: &str) -> String {
    let text = redact_pem_blocks(text);
    let text = redact_bearer(&text);
    let text = redact_secret_values(&text);
    let text = redact_tokens(&text);
    mask_emails(&text)
}

fn first_char(s: &str) -> String {
    s.chars().next().map(String::from).unwrap_or_default()
}

/// Removes every `-----BEGIN …` block through its `-----END …-----` line (or
/// to the end of `text` when it never ends).
fn redact_pem_blocks(text: &str) -> String {
    const BEGIN: &str = "-----BEGIN";
    const END: &str = "-----END";
    const DASHES: &str = "-----";

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        out.push_str(&rest[..start]);
        out.push_str(REDACTED_PEM);
        let after_begin = &rest[start + BEGIN.len()..];
        rest = match after_begin.find(END) {
            Some(end) => {
                let after_end = &after_begin[end + END.len()..];
                match after_end.find(DASHES) {
                    Some(close) => &after_end[close + DASHES.len()..],
                    None => "",
                }
            }
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Characters a token (bearer, JWT, access token) is made of.
fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
}

/// `Bearer <token>` → `Bearer [redacted]` (scheme matched case-insensitively).
fn redact_bearer(text: &str) -> String {
    const SCHEME: &str = "bearer ";
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut search = 0;
    while let Some(found) = lower[search..].find(SCHEME) {
        let token_start = search + found + SCHEME.len();
        let mut token_end = token_start;
        while token_end < bytes.len() && is_token_byte(bytes[token_end]) {
            token_end += 1;
        }
        if token_end > token_start {
            out.push_str(&text[copied..token_start]);
            out.push_str(REDACTED);
            copied = token_end;
        }
        search = token_end.max(token_start);
    }
    out.push_str(&text[copied..]);
    out
}

/// The value after each [`SECRET_KEYS`] key → `[redacted]`.
fn redact_secret_values(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    // (start, end) byte ranges of secret values, possibly overlapping.
    let mut ranges: Vec<(usize, usize)> = Vec::new();

    for key in SECRET_KEYS {
        let mut search = 0;
        while let Some(found) = lower[search..].find(key) {
            let key_start = search + found;
            let key_end = key_start + key.len();
            search = key_end;

            let boundary_before = key_start == 0
                || !(bytes[key_start - 1].is_ascii_alphanumeric() || bytes[key_start - 1] == b'_');
            let boundary_after = key_end == bytes.len()
                || !(bytes[key_end].is_ascii_alphanumeric() || bytes[key_end] == b'_');
            if !(boundary_before && boundary_after) {
                continue;
            }
            if let Some(range) = value_after_key(bytes, key_end) {
                ranges.push(range);
            }
        }
    }

    if ranges.is_empty() {
        return text.to_string();
    }
    ranges.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (start, end) in ranges {
        if start < copied {
            continue;
        }
        out.push_str(&text[copied..start]);
        out.push_str(REDACTED);
        copied = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// The byte range of the value following a key that ends at `i`: optional
/// closing quote, whitespace, `:` or `=`, whitespace, then a quoted string
/// (escapes honored) or a bare run up to a delimiter. `None` without a
/// separator or with an empty value.
fn value_after_key(bytes: &[u8], mut i: usize) -> Option<(usize, usize)> {
    let skip_ws = |mut i: usize| {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    };
    if i < bytes.len() && matches!(bytes[i], b'"' | b'\'') {
        i += 1;
    }
    i = skip_ws(i);
    if i >= bytes.len() || !matches!(bytes[i], b':' | b'=') {
        return None;
    }
    i = skip_ws(i + 1);

    let start = i;
    if i < bytes.len() && matches!(bytes[i], b'"' | b'\'') {
        let quote = bytes[i];
        let value_start = i + 1;
        let mut j = value_start;
        while j < bytes.len() && bytes[j] != quote {
            j += if bytes[j] == b'\\' { 2 } else { 1 };
        }
        let end = j.min(bytes.len());
        return (end > value_start).then_some((value_start, end));
    }
    let mut j = start;
    while j < bytes.len()
        && !bytes[j].is_ascii_whitespace()
        && !matches!(bytes[j], b'&' | b',' | b'}' | b')' | b']' | b';' | b'"')
    {
        j += 1;
    }
    (j > start).then_some((start, j))
}

/// Runs of token characters that are a JWT (`eyJ…` with two dots) or a Google
/// access token (`ya29.…`) → `[redacted token]`.
fn redact_tokens(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if !is_token_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_token_byte(bytes[i]) {
            i += 1;
        }
        let run = &text[start..i];
        if is_secret_token(run) {
            out.push_str(&text[copied..start]);
            out.push_str(REDACTED_TOKEN);
            copied = i;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// A token-character run that embeds a JWT or a Google access token. Path
/// separators split the run first, so `/v1/eyJ…` is still recognized.
fn is_secret_token(run: &str) -> bool {
    run.split('/').any(|part| {
        let jwt = part.starts_with("eyJ") && part.matches('.').count() >= 2;
        let google_access_token = part.starts_with("ya29.") && part.len() > "ya29.".len();
        jwt || google_access_token
    })
}

/// Non-ASCII bytes count as part of an address (internationalized names); a
/// run of them always ends on a character boundary, at an ASCII byte.
fn is_email_local_byte(b: u8) -> bool {
    !b.is_ascii() || b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain_byte(b: u8) -> bool {
    !b.is_ascii() || b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')
}

/// Masks the local part of every `local@domain.tld` in `text`, keeping its
/// first character. Idempotent: a masked address (`a***@…`) is not matched
/// again.
fn mask_emails(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (at, _) in text.match_indices('@') {
        if at < copied {
            continue;
        }
        let mut local_start = at;
        while local_start > copied && is_email_local_byte(bytes[local_start - 1]) {
            local_start -= 1;
        }
        let mut domain_end = at + 1;
        while domain_end < bytes.len() && is_email_domain_byte(bytes[domain_end]) {
            domain_end += 1;
        }
        let domain = text[at + 1..domain_end].trim_end_matches('.');
        let is_email = local_start < at
            && domain.contains('.')
            && !domain.starts_with('.')
            && !domain.starts_with('-');
        if is_email {
            out.push_str(&text[copied..local_start]);
            out.push_str(&first_char(&text[local_start..at]));
            out.push_str("***");
            copied = at;
        }
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const JWT: &str = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJ4In0.c2lnbmF0dXJl";

    #[test]
    fn scrub_masks_every_email_and_is_idempotent() {
        let text = "invite ana.b+ci@agency.example and play-bot@proj.iam.gserviceaccount.com";
        let once = scrub(text);
        assert_eq!(
            once,
            "invite a***@agency.example and p***@proj.iam.gserviceaccount.com"
        );
        assert_eq!(scrub(&once), once);
    }

    #[test]
    fn scrub_leaves_non_emails_alone() {
        for text in [
            "developers/5374/users",
            "@scope without local part",
            "user@localhost",
            "edits/123:commit",
        ] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn scrub_removes_bearer_tokens_case_insensitively() {
        assert_eq!(
            scrub("authorization: Bearer ya29.a0AfH6SMB-xyz"),
            "authorization: Bearer [redacted]"
        );
        assert_eq!(scrub("BEARER abc.def/ghi=="), "BEARER [redacted]");
        // A scheme without a token stays as is.
        assert_eq!(scrub("bearer "), "bearer ");
    }

    #[test]
    fn scrub_removes_jwts_and_google_access_tokens() {
        assert_eq!(
            scrub(&format!("assertion {JWT} sent")),
            "assertion [redacted token] sent"
        );
        assert_eq!(scrub(&format!("/v1/{JWT}")), "[redacted token]");
        assert_eq!(
            scrub("token ya29.c.b0Aaek and more"),
            "token [redacted token] and more"
        );
        // A word that merely starts like one is kept.
        assert_eq!(scrub("eyJust one dot."), "eyJust one dot.");
        assert_eq!(scrub("ya29."), "ya29.");
    }

    #[test]
    fn scrub_removes_secret_values_in_json_and_form_bodies() {
        assert_eq!(
            scrub(r#"{"access_token": "ya29-xyz", "expires_in": 3599}"#),
            r#"{"access_token": "[redacted]", "expires_in": 3599}"#
        );
        assert_eq!(
            scrub("grant_type=jwt-bearer&assertion=abc.def&x=1"),
            "grant_type=jwt-bearer&assertion=[redacted]&x=1"
        );
        assert_eq!(
            scrub(r#"{"private_key_id":"0123abcd","client_secret":"s3cr\"et"}"#),
            r#"{"private_key_id":"[redacted]","client_secret":"[redacted]"}"#
        );
        // Not a key on its own: part of a longer word.
        assert_eq!(scrub("secretary: Ana"), "secretary: Ana");
        // A key without a separator is just a word.
        assert_eq!(scrub("the assertion failed"), "the assertion failed");
    }

    #[test]
    fn scrub_removes_pem_blocks_raw_and_json_escaped() {
        let raw = "key -----BEGIN PRIVATE KEY-----\nMIIEvQ\n-----END PRIVATE KEY----- tail";
        assert_eq!(scrub(raw), "key [redacted PEM] tail");
        let escaped =
            r#"{"k":"-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\n"}"#;
        assert_eq!(scrub(escaped), r#"{"k":"[redacted PEM]\n"}"#);
        // An unterminated block is cut to the end.
        assert_eq!(
            scrub("a -----BEGIN PRIVATE KEY-----\nMIIE"),
            "a [redacted PEM]"
        );
    }

    #[test]
    fn scrub_keeps_multibyte_text_intact() {
        assert_eq!(scrub("Ünïcødé — ok"), "Ünïcødé — ok");
        assert_eq!(scrub("café ana@exämple.com"), "café a***@exämple.com");
        assert_eq!(scrub("— élodie@x.fr —"), "— é***@x.fr —");
    }
}
