//! App icons read from the **public** Google Play store page.
//!
//! The Android Publisher API only exposes an app's images through
//! `edits.images.list`, i.e. inside an edit, and opening an edit invalidates any
//! other edit the same service account has open for the app (e.g. a CI upload
//! in progress; see `edits`). Hosts want an icon for every app in a list, so
//! icons must not cost an edit each. They come from the store page instead:
//! `GET https://play.google.com/store/apps/details?id={package}&hl=en`, whose
//! `og:image` meta tag is the app icon. A published app answers `200`; an
//! unpublished or unknown package answers `404`.
//!
//! The page is public, so [`StorePageClient`] holds no authenticator: it cannot
//! mint or send an OAuth token, and it never calls the Android Publisher API.

use std::sync::Arc;

use super::client::is_valid_package_name;
use crate::error::StackError;
use crate::http_debug::send_and_read_capped;
use crate::ports::DebugLogger;

/// Where the public store pages live.
const DEFAULT_STORE_BASE_URL: &str = "https://play.google.com";
/// Path segments from the store host to an app's details page.
const DETAILS_PATH: [&str; 3] = ["store", "apps", "details"];
/// `hl`: the page language. Fixed, so the markup (and the icon tag) does not
/// depend on where the request comes from.
const PAGE_LANGUAGE: &str = "en";
/// Bound on how much of a store page is read. A page is about 1.3 MB today and
/// its `og:image` tag sits past the first megabyte (the head inlines CSS and
/// scripts), so this leaves wide headroom while bounding memory.
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
/// Google's image-server size option for a 512 px square, the same size the
/// App Store Connect icon defaults to.
const ICON_SIZE_SUFFIX: &str = "=s512";
/// Google's image hosts (`play-lh.googleusercontent.com` serves Play icons).
/// Any other host in `og:image` is not trusted as the app icon.
const ICON_HOST_SUFFIX: &str = ".googleusercontent.com";
/// Upper bound on an entity name between `&` and `;` (e.g. `#x1F600`), so a
/// stray `&` does not make the decoder scan the rest of the value.
const MAX_ENTITY_LEN: usize = 10;

/// Reads app icons from the public Google Play store page. Shares the
/// provider's `reqwest::Client` (connection pool and timeouts) but no
/// credentials.
///
/// The base URL is injectable so tests can point it at a mock server.
pub(crate) struct StorePageClient {
    base_url: String,
    http: reqwest::Client,
    /// Optional HTTP tracing sink, `None` by default (see
    /// [`Self::with_debug_logger`]).
    debug_logger: Option<Arc<dyn DebugLogger>>,
}

impl StorePageClient {
    pub(crate) fn new(http: reqwest::Client) -> Self {
        Self::with_base_url(http, DEFAULT_STORE_BASE_URL.to_string())
    }

    pub(crate) fn with_base_url(http: reqwest::Client, base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
            debug_logger: None,
        }
    }

    /// Attaches an optional HTTP tracing sink. Builder-style; `None` is a
    /// no-op (no logging). The page body itself is never logged, only its
    /// size.
    pub(crate) fn with_debug_logger(mut self, logger: Option<Arc<dyn DebugLogger>>) -> Self {
        self.debug_logger = logger;
        self
    }

    /// Writes `message()` to the debug logger, if one is attached. Lazy, so
    /// nothing is formatted without a logger.
    fn trace(&self, message: impl FnOnce() -> String) {
        if let Some(logger) = &self.debug_logger {
            logger.log(message());
        }
    }

    /// `{base}/store/apps/details?id={package_name}&hl=en`, with the query
    /// form-encoded (a `&`, `#` or space in `package_name` cannot change it).
    ///
    /// # Errors
    /// [`StackError::Network`] if the base URL is not a valid base URL.
    fn details_url(&self, package_name: &str) -> Result<reqwest::Url, StackError> {
        let mut url = reqwest::Url::parse(&self.base_url)
            .map_err(|e| StackError::network(format!("invalid Google Play store URL: {e}")))?;
        url.path_segments_mut()
            .map_err(|()| StackError::network("invalid Google Play store URL: not a base URL"))?
            .pop_if_empty()
            .extend(DETAILS_PATH);
        url.query_pairs_mut()
            .clear()
            .append_pair("id", package_name)
            .append_pair("hl", PAGE_LANGUAGE);
        Ok(url)
    }

    /// The icon URL of `package_name` from its public store page, sized to
    /// 512 px (see [`normalize_icon_url`]), or `None` when the app is not
    /// visible in the store or the page has no usable icon.
    ///
    /// - A name that breaks Android's package rules cannot be in the store:
    ///   `None` without a request.
    /// - `404` (unpublished, unknown, or not visible in the store): `None`.
    /// - `2xx` without a usable `og:image` (e.g. a consent page served
    ///   instead): `None`, and a debug line says why.
    ///
    /// # Errors
    /// [`StackError::Http`] on any other non-2xx answer, or
    /// [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_icon_url(
        &self,
        package_name: &str,
    ) -> Result<Option<String>, StackError> {
        if !is_valid_package_name(package_name) {
            self.trace(|| {
                format!(
                    "[RustCore] Google Play icon: {package_name:?} is not a valid package \
                     name, so it has no store page (nothing was sent)"
                )
            });
            return Ok(None);
        }

        let url = self.details_url(package_name)?;
        let (status, body) = send_and_read_capped(
            self.http.get(url),
            self.debug_logger.as_deref(),
            MAX_PAGE_BYTES,
        )
        .await?;

        if status == reqwest::StatusCode::NOT_FOUND {
            self.trace(|| {
                format!(
                    "[RustCore] Google Play icon: no store page for {package_name} \
                     (unpublished or not visible in the store)"
                )
            });
            return Ok(None);
        }
        if !status.is_success() {
            return Err(StackError::Http {
                status: status.as_u16(),
                message: format!(
                    "Google Play store page for {package_name}: {}",
                    status.canonical_reason().unwrap_or("unexpected status")
                ),
            });
        }

        let icon = icon_url_from_page(&body);
        if icon.is_none() {
            self.trace(|| {
                format!(
                    "[RustCore] Google Play icon: the store page for {package_name} has no \
                     usable og:image (e.g. a consent page was served instead)"
                )
            });
        }
        Ok(icon)
    }
}

/// The app icon a store page advertises: the first `og:image` meta tag's
/// `content`, entity-decoded, accepted and resized by [`normalize_icon_url`].
fn icon_url_from_page(html: &str) -> Option<String> {
    let content = og_image_content(html)?;
    normalize_icon_url(&decode_html_entities(&content))
}

/// The raw `content` of the first `<meta>` tag whose `property` (or `name`) is
/// `og:image` and that has a `content`. Tag and attribute names match
/// case-insensitively; attributes may come in any order, with double, single
/// or no quotes.
fn og_image_content(html: &str) -> Option<String> {
    meta_tags(html)
        .map(parse_attributes)
        .filter(|attributes| {
            attributes.iter().any(|(name, value)| {
                (name == "property" || name == "name")
                    && value.trim().eq_ignore_ascii_case("og:image")
            })
        })
        .find_map(|attributes| {
            attributes
                .into_iter()
                .find_map(|(name, value)| (name == "content").then_some(value))
        })
}

/// The attribute text of every `<meta …>` tag in `html`, in order: what lies
/// between `<meta` and the `>` that closes the tag (a `>` inside a quoted value
/// does not). A tag cut off by the end of the input is dropped.
fn meta_tags(html: &str) -> impl Iterator<Item = &str> {
    let bytes = html.as_bytes();
    let mut position = 0;
    std::iter::from_fn(move || loop {
        let open = position + html.get(position..)?.find('<')?;
        position = open + 1;
        let name_end = open + 5;
        let is_meta = bytes
            .get(open + 1..name_end)
            .is_some_and(|name| name.eq_ignore_ascii_case(b"meta"));
        let name_ends_here = bytes
            .get(name_end)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'/' || *b == b'>');
        if !(is_meta && name_ends_here) {
            continue;
        }

        let mut quote: Option<u8> = None;
        let mut close = None;
        for (offset, &byte) in bytes.get(name_end..)?.iter().enumerate() {
            match quote {
                Some(q) if byte == q => quote = None,
                Some(_) => {}
                None if byte == b'"' || byte == b'\'' => quote = Some(byte),
                None if byte == b'>' => {
                    close = Some(name_end + offset);
                    break;
                }
                None => {}
            }
        }
        let close = close?;
        position = close + 1;
        // Both ends sit on ASCII bytes, hence on char boundaries.
        return html.get(name_end..close);
    })
}

/// Parses an HTML tag's attribute text into `(lowercased name, raw value)`
/// pairs, in order. A value may be double-quoted, single-quoted or unquoted;
/// an attribute without a value gets `""`. Entities are left undecoded.
fn parse_attributes(text: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let is_space = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_whitespace);
    // Every index used to slice below sits on an ASCII byte or at an end, so on
    // a char boundary; `get` keeps it panic-free regardless.
    let slice = |from: usize, to: usize| text.get(from..to).unwrap_or_default();

    let mut attributes = Vec::new();
    let mut i = 0;
    while i < len {
        while is_space(i) || bytes.get(i) == Some(&b'/') {
            i += 1;
        }
        if i >= len {
            break;
        }
        let start = i;

        while i < len && !is_space(i) && !matches!(bytes[i], b'=' | b'/' | b'>') {
            i += 1;
        }
        let name = slice(start, i).to_ascii_lowercase();
        while is_space(i) {
            i += 1;
        }

        let mut value = String::new();
        if bytes.get(i) == Some(&b'=') {
            i += 1;
            while is_space(i) {
                i += 1;
            }
            match bytes.get(i) {
                Some(&quote @ (b'"' | b'\'')) => {
                    let value_start = i + 1;
                    let value_end = bytes
                        .get(value_start..)
                        .and_then(|rest| rest.iter().position(|&b| b == quote))
                        .map_or(len, |offset| value_start + offset);
                    value = slice(value_start, value_end).to_string();
                    i = value_end + 1;
                }
                _ => {
                    let value_start = i;
                    while i < len && !is_space(i) && bytes[i] != b'>' {
                        i += 1;
                    }
                    value = slice(value_start, i).to_string();
                }
            }
        }

        if !name.is_empty() {
            attributes.push((name, value));
        }
        if i == start {
            // A lone delimiter (e.g. `>`): skip it so the loop always advances.
            i += 1;
        }
    }
    attributes
}

/// Decodes the HTML character references an attribute value may carry: the
/// named `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;` and numeric `&#NN;` /
/// `&#xHH;`. Anything else (an unknown name, a lone `&`, a missing `;`, a NUL
/// or invalid code point) is kept verbatim.
fn decode_html_entities(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(amp) = rest.find('&') {
        decoded.push_str(rest.get(..amp).unwrap_or_default());
        let after = rest.get(amp + 1..).unwrap_or_default();
        let entity = after
            .find(';')
            .filter(|&semi| semi <= MAX_ENTITY_LEN)
            .and_then(|semi| Some((decode_entity(after.get(..semi)?)?, semi)));
        match entity {
            Some((ch, semi)) => {
                decoded.push(ch);
                rest = after.get(semi + 1..).unwrap_or_default();
            }
            None => {
                decoded.push('&');
                rest = after;
            }
        }
    }
    decoded.push_str(rest);
    decoded
}

/// The character one entity name (the text between `&` and `;`) stands for.
fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let numeric = name.strip_prefix('#')?;
            let (digits, radix) = match numeric.strip_prefix(['x', 'X']) {
                Some(hex) => (hex, 16),
                None => (numeric, 10),
            };
            // `from_str_radix` alone would accept a leading `+`.
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return None;
            }
            let code = u32::from_str_radix(digits, radix).ok()?;
            char::from_u32(code).filter(|&c| c != '\0')
        }
    }
}

/// Accepts `raw` as an app icon URL only if it is an `https://` URL on a
/// `*.googleusercontent.com` host (no credentials, no explicit port), then
/// sizes it to 512 px: the image-server options after the last `=` of the last
/// path segment (e.g. `=s0-br30`) are replaced with `=s512`, or `=s512` is
/// appended when there are none. Query and fragment are dropped.
///
/// `None` for anything else, including an empty image id.
fn normalize_icon_url(raw: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(raw.trim()).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let host = url.domain()?;
    if host.len() <= ICON_HOST_SUFFIX.len() || !host.ends_with(ICON_HOST_SUFFIX) {
        return None;
    }

    let (directory, last_segment) = url.path().rsplit_once('/')?;
    let image_id = last_segment
        .rsplit_once('=')
        .map_or(last_segment, |(id, _)| id);
    if image_id.is_empty() {
        return None;
    }
    let path = format!("{directory}/{image_id}{ICON_SIZE_SUFFIX}");
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::googleplay::test_support::{store_page_client, CapturingLogger};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The real icon of the YouTube app, as its store page served it.
    const YOUTUBE_ICON: &str = "https://play-lh.googleusercontent.com/QNmuZQc9I6Zbe3mWnSr0hycnENqGFCI5p3yE29Hkxtf22T0IWS6zTrpxULLyyjWpB7ONAXDsDQXnXcVWokl3eg=s0-br30";
    /// The same icon at 512 px.
    const YOUTUBE_ICON_512: &str = "https://play-lh.googleusercontent.com/QNmuZQc9I6Zbe3mWnSr0hycnENqGFCI5p3yE29Hkxtf22T0IWS6zTrpxULLyyjWpB7ONAXDsDQXnXcVWokl3eg=s512";

    /// The Open Graph tags of a live store page (com.google.android.youtube,
    /// hl=en), verbatim: note `og:type` with `content` first and the `&amp;`
    /// in `og:url`.
    fn real_page_head() -> String {
        format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta content="website" property="og:type"><meta property="og:url" content="https://play.google.com/store/apps/details?id=com.google.android.youtube&amp;hl=en"><meta property="og:title" content="YouTube - Apps on Google Play"><meta name="description" property="og:description" content="Enjoy your favorite videos and channels with the official YouTube app."><meta property="og:image" content="{YOUTUBE_ICON}"></head><body></body></html>"#
        )
    }

    fn details() -> wiremock::MockBuilder {
        Mock::given(method("GET")).and(path("/store/apps/details"))
    }

    // -----------------------------------------------------------------------
    // Page parsing
    // -----------------------------------------------------------------------

    #[test]
    fn reads_the_icon_from_real_play_markup() {
        assert_eq!(
            icon_url_from_page(&real_page_head()).as_deref(),
            Some(YOUTUBE_ICON_512)
        );
    }

    #[test]
    fn og_image_tolerates_attribute_order_quotes_and_case() {
        let url = "https://play-lh.googleusercontent.com/Abc=s0";
        for tag in [
            format!(r#"<meta property="og:image" content="{url}">"#),
            format!(r#"<meta content="{url}" property="og:image">"#),
            format!(r#"<meta property='og:image' content='{url}'>"#),
            format!(r#"<meta content='{url}' property="og:image" />"#),
            format!(r#"<META PROPERTY="OG:IMAGE" CONTENT="{url}"/>"#),
            format!(r#"<meta property=og:image content={url}>"#),
            format!(r#"<meta name="og:image" content="{url}">"#),
            format!("<meta\n\tproperty = \"og:image\"\n\tcontent = \"{url}\"\n>"),
            format!(r#"<meta itemprop="x>y" property="og:image" data-z content="{url}">"#),
        ] {
            assert_eq!(
                og_image_content(&tag).as_deref(),
                Some(url),
                "failed to read {tag}"
            );
        }
    }

    #[test]
    fn og_image_ignores_look_alike_properties_and_tags() {
        let html = r#"
            <metadata property="og:image" content="https://play-lh.googleusercontent.com/NotMeta">
            <meta property="og:image:alt" content="https://play-lh.googleusercontent.com/Alt">
            <meta property="og:image:width" content="512">
            <meta name="twitter:image" content="https://play-lh.googleusercontent.com/Twitter">
            <meta property="og:image">
            <meta property="og:image" content="https://play-lh.googleusercontent.com/Real=s0">
            <meta property="og:image" content="https://play-lh.googleusercontent.com/Second=s0">
        "#;
        assert_eq!(
            og_image_content(html).as_deref(),
            Some("https://play-lh.googleusercontent.com/Real=s0")
        );
    }

    #[test]
    fn og_image_absent_or_cut_off_is_none() {
        for html in [
            "",
            "<html><head><title>Before you continue to Google</title></head></html>",
            r#"<meta property="og:title" content="App">"#,
            // Truncated mid-tag (e.g. at the read cap): no closing `>`.
            r#"<meta property="og:image" content="https://play-lh.googleusercontent.com/A"#,
            "<meta",
            "<",
        ] {
            assert_eq!(og_image_content(html), None, "{html:?}");
        }
    }

    #[test]
    fn page_with_non_ascii_text_around_the_tag_is_read() {
        let html = format!(
            r#"<p>Écoutez — 视频 🎬</p><meta property="og:image" content="{YOUTUBE_ICON}"><p>ü</p>"#
        );
        assert_eq!(icon_url_from_page(&html).as_deref(), Some(YOUTUBE_ICON_512));
    }

    #[test]
    fn parse_attributes_handles_bare_and_unterminated_values() {
        assert_eq!(
            parse_attributes(r#" async data-x=1 content="open"#),
            vec![
                ("async".to_string(), String::new()),
                ("data-x".to_string(), "1".to_string()),
                ("content".to_string(), "open".to_string()),
            ]
        );
        assert!(parse_attributes(" / = > ").is_empty());
    }

    // -----------------------------------------------------------------------
    // Entities
    // -----------------------------------------------------------------------

    #[test]
    fn decodes_named_and_numeric_entities() {
        for (input, expected) in [
            ("a&amp;b", "a&b"),
            ("&lt;&gt;&quot;&apos;", "<>\"'"),
            ("&#39;&#x41;&#X42;&#67;", "'ABC"),
            ("https:&#x2F;&#x2F;host", "https://host"),
            ("&amp;amp;", "&amp;"),
        ] {
            assert_eq!(decode_html_entities(input), expected, "{input:?}");
        }
    }

    #[test]
    fn keeps_what_is_not_a_known_entity() {
        for input in [
            "plain",
            "a & b",
            "&",
            "&;",
            "&nbsp;",
            "&amp",
            "&#;",
            "&#x;",
            "&#xZZ;",
            "&#+5;",
            "&#0;",
            "&#xD800;",
            "&#99999999999;",
            "&verylongentityname;",
        ] {
            assert_eq!(decode_html_entities(input), input, "{input:?}");
        }
        // A kept `&` does not swallow a real entity right after it.
        assert_eq!(decode_html_entities("&&amp;"), "&&");
    }

    // -----------------------------------------------------------------------
    // Icon URL acceptance and sizing
    // -----------------------------------------------------------------------

    #[test]
    fn sizes_the_icon_to_512() {
        for (raw, expected) in [
            (YOUTUBE_ICON, YOUTUBE_ICON_512),
            (
                "https://play-lh.googleusercontent.com/AkThx-mA=w240-h480-rw",
                "https://play-lh.googleusercontent.com/AkThx-mA=s512",
            ),
            // No size options: appended.
            (
                "https://play-lh.googleusercontent.com/AkThx-mA",
                "https://play-lh.googleusercontent.com/AkThx-mA=s512",
            ),
            // Empty options.
            (
                "https://play-lh.googleusercontent.com/AkThx-mA=",
                "https://play-lh.googleusercontent.com/AkThx-mA=s512",
            ),
            // Only the last `=` of the last segment starts the options.
            (
                "https://play-lh.googleusercontent.com/a=b/Id=c=s0",
                "https://play-lh.googleusercontent.com/a=b/Id=c=s512",
            ),
            // Query and fragment are dropped; surrounding blanks trimmed.
            (
                "  https://play-lh.googleusercontent.com/Id=s0?x=1#frag ",
                "https://play-lh.googleusercontent.com/Id=s512",
            ),
            // Any googleusercontent.com host; an explicit default port is fine.
            (
                "https://lh3.googleusercontent.com:443/Id=s96",
                "https://lh3.googleusercontent.com/Id=s512",
            ),
        ] {
            assert_eq!(normalize_icon_url(raw).as_deref(), Some(expected), "{raw}");
        }
    }

    #[test]
    fn rejects_urls_that_are_not_google_image_urls() {
        for raw in [
            "http://play-lh.googleusercontent.com/Id=s0",
            "https://example.com/Id=s0",
            "https://play-lh.googleusercontent.com.evil.example/Id=s0",
            "https://evilgoogleusercontent.com/Id=s0",
            "https://googleusercontent.com/Id=s0",
            "https://user:pass@play-lh.googleusercontent.com/Id=s0",
            "https://user@play-lh.googleusercontent.com/Id=s0",
            "https://play-lh.googleusercontent.com:8443/Id=s0",
            "https://142.250.1.1/Id=s0",
            "//play-lh.googleusercontent.com/Id=s0",
            "/Id=s0",
            "",
            "not a url",
            // Empty image id.
            "https://play-lh.googleusercontent.com/",
            "https://play-lh.googleusercontent.com",
            "https://play-lh.googleusercontent.com/=s0",
            "https://play-lh.googleusercontent.com/dir/",
        ] {
            assert_eq!(normalize_icon_url(raw), None, "{raw:?} should be rejected");
        }
    }

    #[test]
    fn entity_encoded_icon_url_is_decoded_before_validation() {
        let html = r#"<meta property="og:image" content="https:&#x2F;&#x2F;play-lh.googleusercontent.com/Ak&amp;Thx=s0-br30">"#;
        assert_eq!(
            icon_url_from_page(html).as_deref(),
            Some("https://play-lh.googleusercontent.com/Ak&Thx=s512")
        );
    }

    #[test]
    fn foreign_og_image_is_not_an_icon() {
        let html = r#"<meta property="og:image" content="https://cdn.example.com/logo.png">"#;
        assert_eq!(icon_url_from_page(html), None);
    }

    #[test]
    fn limits_are_fixed() {
        // Literals, so a changed bound fails here.
        assert_eq!(MAX_PAGE_BYTES, 8 * 1024 * 1024);
        assert_eq!(ICON_SIZE_SUFFIX, "=s512");
        assert_eq!(MAX_ENTITY_LEN, 10);
    }

    // -----------------------------------------------------------------------
    // URL building
    // -----------------------------------------------------------------------

    #[test]
    fn new_targets_the_public_play_store() {
        let client = StorePageClient::new(reqwest::Client::new());
        assert_eq!(client.base_url, "https://play.google.com");
        assert_eq!(
            client.details_url("com.example.app").unwrap().as_str(),
            "https://play.google.com/store/apps/details?id=com.example.app&hl=en"
        );
    }

    #[test]
    fn details_url_form_encodes_the_query() {
        let client =
            StorePageClient::with_base_url(reqwest::Client::new(), "http://store.test/".into());
        assert_eq!(client.base_url, "http://store.test");
        assert_eq!(
            client.details_url("a&b c#d=e").unwrap().as_str(),
            "http://store.test/store/apps/details?id=a%26b+c%23d%3De&hl=en"
        );
    }

    #[test]
    fn details_url_keeps_a_base_path_and_drops_a_base_query() {
        let client = StorePageClient::with_base_url(
            reqwest::Client::new(),
            "http://proxy.test/play?stale=1".into(),
        );
        assert_eq!(
            client.details_url("com.example.app").unwrap().as_str(),
            "http://proxy.test/play/store/apps/details?id=com.example.app&hl=en"
        );
    }

    #[test]
    fn details_url_with_invalid_base_is_network() {
        let client = StorePageClient::with_base_url(reqwest::Client::new(), "not a url".into());
        assert!(matches!(
            client.details_url("com.example.app"),
            Err(StackError::Network { .. })
        ));
        let client = StorePageClient::with_base_url(reqwest::Client::new(), "mailto:x@y.z".into());
        assert!(matches!(
            client.details_url("com.example.app"),
            Err(StackError::Network { .. })
        ));
    }

    // -----------------------------------------------------------------------
    // fetch_icon_url
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_icon_url_reads_og_image_and_sizes_it() {
        let server = MockServer::start().await;
        details()
            .and(query_param("id", "com.google.android.youtube"))
            .and(query_param("hl", "en"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .set_body_string(real_page_head()),
            )
            .expect(1)
            .mount(&server)
            .await;

        let icon = store_page_client(&server)
            .fetch_icon_url("com.google.android.youtube")
            .await
            .unwrap();
        assert_eq!(icon.as_deref(), Some(YOUTUBE_ICON_512));
    }

    #[tokio::test]
    async fn fetch_icon_url_finds_og_image_past_the_first_megabyte() {
        // Live pages put the tag ~1 MB in, behind inlined CSS and scripts.
        let server = MockServer::start().await;
        let page = format!(
            "<html><head><style>{}</style><meta property=\"og:image\" content=\"{YOUTUBE_ICON}\"></head></html>",
            "x".repeat(1_100_000)
        );
        details()
            .respond_with(ResponseTemplate::new(200).set_body_string(page))
            .expect(1)
            .mount(&server)
            .await;

        let icon = store_page_client(&server)
            .fetch_icon_url("com.google.android.youtube")
            .await
            .unwrap();
        assert_eq!(icon.as_deref(), Some(YOUTUBE_ICON_512));
    }

    #[tokio::test]
    async fn fetch_icon_url_404_is_none() {
        // What Google answers for an unpublished or unknown package.
        let server = MockServer::start().await;
        details()
            .and(query_param("id", "com.example.unpublished"))
            .respond_with(ResponseTemplate::new(404).set_body_string("<html>Not Found</html>"))
            .expect(1)
            .mount(&server)
            .await;

        let logger = Arc::new(CapturingLogger::default());
        let icon = store_page_client(&server)
            .with_debug_logger(Some(logger.clone()))
            .fetch_icon_url("com.example.unpublished")
            .await
            .unwrap();
        assert_eq!(icon, None);
        assert!(logger
            .messages()
            .iter()
            .any(|m| m.contains("no store page for com.example.unpublished")));
    }

    #[tokio::test]
    async fn fetch_icon_url_page_without_og_image_is_none_and_traced() {
        let server = MockServer::start().await;
        let consent = "<html><head><title>Before you continue to Google</title></head>\
                       <body>consent form</body></html>";
        details()
            .respond_with(ResponseTemplate::new(200).set_body_string(consent))
            .expect(1)
            .mount(&server)
            .await;

        let logger = Arc::new(CapturingLogger::default());
        let icon = store_page_client(&server)
            .with_debug_logger(Some(logger.clone()))
            .fetch_icon_url("com.example.app")
            .await
            .unwrap();
        assert_eq!(icon, None);

        let messages = logger.messages();
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(messages[0].starts_with("[RustCore] → request"));
        assert!(messages[0].contains(&format!(
            "curl -X GET '{}/store/apps/details?id=com.example.app&hl=en'",
            server.uri()
        )));
        // The page body is never logged, only its size.
        assert_eq!(
            messages[1],
            format!(
                "[RustCore] ← 200 OK\n[{} bytes read; body not logged]",
                consent.len()
            )
        );
        assert!(messages.iter().all(|m| !m.contains("consent form")));
        assert!(messages[2].contains("the store page for com.example.app has no usable og:image"));
    }

    #[tokio::test]
    async fn fetch_icon_url_foreign_og_image_is_none() {
        let server = MockServer::start().await;
        details()
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<meta property="og:image" content="https://tracker.example/pixel.png">"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let icon = store_page_client(&server)
            .fetch_icon_url("com.example.app")
            .await
            .unwrap();
        assert_eq!(icon, None);
    }

    #[tokio::test]
    async fn fetch_icon_url_server_errors_are_http() {
        for (status, reason) in [(500, "Internal Server Error"), (429, "Too Many Requests")] {
            let server = MockServer::start().await;
            details()
                .respond_with(
                    ResponseTemplate::new(status).set_body_string("<html>huge error page</html>"),
                )
                .expect(1)
                .mount(&server)
                .await;

            match store_page_client(&server)
                .fetch_icon_url("com.example.app")
                .await
            {
                Err(StackError::Http {
                    status: got,
                    message,
                }) => {
                    assert_eq!(got, status);
                    assert_eq!(
                        message,
                        format!("Google Play store page for com.example.app: {reason}")
                    );
                }
                other => panic!("expected Http {status}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn fetch_icon_url_sends_no_credentials() {
        let server = MockServer::start().await;
        details()
            .respond_with(ResponseTemplate::new(200).set_body_string(real_page_head()))
            .expect(1)
            .mount(&server)
            .await;

        store_page_client(&server)
            .fetch_icon_url("com.google.android.youtube")
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.path(), "/store/apps/details");
        assert!(requests[0].headers.get("authorization").is_none());
        assert!(requests[0].headers.get("cookie").is_none());
    }

    #[tokio::test]
    async fn fetch_icon_url_invalid_package_is_none_without_a_request() {
        let server = MockServer::start().await;
        let logger = Arc::new(CapturingLogger::default());
        let client = store_page_client(&server).with_debug_logger(Some(logger.clone()));

        for package in ["", "not a package", "com", "com.example/../x", "a&b.c"] {
            assert_eq!(client.fetch_icon_url(package).await.unwrap(), None);
        }
        assert!(server.received_requests().await.unwrap().is_empty());
        let messages = logger.messages();
        assert_eq!(messages.len(), 5);
        assert!(messages
            .iter()
            .all(|m| m.contains("is not a valid package name") && m.contains("nothing was sent")));
    }

    #[tokio::test]
    async fn fetch_icon_url_transport_failure_is_network() {
        // Nothing listens on port 1, so the connection is refused.
        let client =
            StorePageClient::with_base_url(reqwest::Client::new(), "http://127.0.0.1:1".into());
        assert!(matches!(
            client.fetch_icon_url("com.example.app").await,
            Err(StackError::Network { .. })
        ));
    }

    #[test]
    fn trace_is_lazy_and_needs_a_logger() {
        let client = StorePageClient::new(reqwest::Client::new());
        client.trace(|| panic!("formatted without a logger"));

        let logger = Arc::new(CapturingLogger::default());
        let client = client.with_debug_logger(Some(logger.clone()));
        client.trace(|| "hello".to_string());
        assert_eq!(logger.messages(), vec!["hello".to_string()]);
    }
}
