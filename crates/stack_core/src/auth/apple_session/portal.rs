//! The Apple Developer Portal side of a web session
//! (`developer.apple.com/services-account`), where provisioning (devices,
//! bundle ids and their capabilities, certificates, profiles) lives for an
//! Apple ID session.
//!
//! Everything here follows fastlane master (`spaceship/lib/spaceship/`):
//! - `portal/portal_client.rb` `teams`: `POST services-account/QH65B2/
//!   account/listTeams.action`, no body. Its teams are the *Developer Portal*
//!   teams, whose alphanumeric `teamId` (e.g. `ABCDE12345`) is not the App
//!   Store Connect provider id an account is connected with.
//! - `connect_api/provisioning/client.rb`: the App Store Connect API's
//!   provisioning paths (`v1/devices`, …) on `services-account`, where a web
//!   session reaches them as follows:
//!   - `GET` → `POST` with `X-HTTP-Method-Override: GET` and the JSON body
//!     `{"urlEncodedQueryParams": <query>, "teamId": <portal team>}`
//!     (`proxy_get`);
//!   - `DELETE` → `POST` with `X-HTTP-Method-Override: DELETE` and the same
//!     body (`proxy_delete`);
//!   - `POST` → `POST` with the JSON:API body plus `data.attributes.teamId`
//!     (`proxy_post`);
//!   - `PATCH` is **not** proxied: `Provisioning::Client` does not override
//!     `APIClient#patch`, which sends a real `PATCH` with
//!     `Content-Type: application/json` and the body unchanged.
//!
//!   The three proxied forms send `Content-Type: application/vnd.api+json`
//!   and `X-Requested-With: XMLHttpRequest`. The query is encoded with
//!   Faraday's `NestedParamsEncoder` (fastlane 2.232.2 bundles faraday 1.8.0):
//!   pairs sorted by key, every byte outside `[A-Za-z0-9 .~_-]` percent-encoded
//!   in upper case, a space as `+`; no params (`nil`) is JSON `null`, empty
//!   params (`{}`) the empty string.
//!
//! Matching a portal team to the App Store Connect team follows spaceship's
//! `ConnectAPI::Client.login(team_name:)`, which selects both teams by the
//! same name: see [`match_portal_team`].

use reqwest::Url;
use serde_json::{json, Value};

use super::error::AppleSignInError;
use super::session::AppleTeam;
use super::transport::{self, common_status_error, AppleReply};
use crate::error::StackError;

/// spaceship's `Spaceship::Client::PROTOCOL_VERSION`, the path segment of the
/// Developer Portal's `.action` endpoints.
pub const PORTAL_PROTOCOL_VERSION: &str = "QH65B2";

/// The Developer Portal API root, relative to the Apple Developer site.
pub(crate) const SERVICES_ACCOUNT: &str = "services-account";

/// At most this many portal teams are read from `listTeams.action`.
const MAX_PORTAL_TEAMS: usize = 500;
/// A portal team id longer than this is not believed (Apple's are 10
/// characters).
const MAX_PORTAL_TEAM_ID_LEN: usize = 32;

/// Error entries quoted by [`summarize_errors`].
const MAX_SUMMARY_ERRORS: usize = 3;
/// Characters of one quoted error.
const MAX_SUMMARY_CHARS: usize = 160;

/// `listTeams.action`, relative to the Apple Developer site.
pub(crate) fn list_teams_path() -> String {
    format!("{SERVICES_ACCOUNT}/{PORTAL_PROTOCOL_VERSION}/account/listTeams.action")
}

/// The headers Ruby's `Net::HTTP` puts on every spaceship request it is not
/// given (`Net::HTTPGenericRequest#initialize`): `Accept: */*` and its
/// `Accept-Encoding`, whose gzip / deflate answers the transports inflate.
/// spaceship sets its own `User-Agent`, as the session client does.
pub(crate) fn net_http_defaults() -> [(&'static str, &'static str); 2] {
    [
        ("Accept", "*/*"),
        ("Accept-Encoding", transport::NET_HTTP_ACCEPT_ENCODING),
    ]
}

/// The `Content-Type` of spaceship's proxied provisioning requests.
pub(crate) const PROXY_CONTENT_TYPE: &str = "application/vnd.api+json";
/// The `X-Requested-With` of spaceship's proxied provisioning requests.
pub(crate) const PROXY_REQUESTED_WITH: &str = "XMLHttpRequest";
/// The header that carries a proxied request's real method.
pub(crate) const METHOD_OVERRIDE_HEADER: &str = "X-HTTP-Method-Override";

/// Whether `id` looks like a Developer Portal team id: 1 to
/// [`MAX_PORTAL_TEAM_ID_LEN`] ASCII letters and digits.
pub(crate) fn is_valid_portal_team_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_PORTAL_TEAM_ID_LEN
        && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// A Developer Portal team (one entry of `listTeams.action`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PortalTeam {
    /// `teamId`: what `services-account` calls take.
    pub(crate) team_id: String,
    /// `name`.
    pub(crate) name: String,
    /// `type` (e.g. `Company/Organization`, `Individual`, `In-House`).
    pub(crate) kind: String,
}

/// The teams in a `listTeams.action` answer, sorted by name then id as
/// spaceship's `teams` sorts them. Entries without a valid `teamId` are
/// skipped; at most [`MAX_PORTAL_TEAMS`] are read.
///
/// # Errors
/// [`AppleSignInError::SessionRejected`] on HTTP 401 (the session is no
/// longer accepted); [`AppleSignInError::Refused`] on 403 or when the portal
/// reports an error (`resultCode` other than 0); the common status errors;
/// [`AppleSignInError::Unexpected`] for another status or a body without a
/// `teams` array.
pub(crate) fn parse_list_teams(reply: &AppleReply) -> Result<Vec<PortalTeam>, AppleSignInError> {
    let status = reply.code();
    if status == 401 {
        return Err(AppleSignInError::SessionRejected { status });
    }
    if let Some(err) = common_status_error(reply.status) {
        return Err(err);
    }
    let reason = || {
        let summary = summarize_errors(&reply.body);
        if summary.is_empty() {
            "the Apple Developer Portal did not list the teams".to_string()
        } else {
            summary
        }
    };
    if status == 403 {
        return Err(AppleSignInError::Refused {
            status,
            message: reason(),
        });
    }
    if !reply.status.is_success() {
        return Err(AppleSignInError::unexpected(status, reason()));
    }
    let body = reply.json().ok_or_else(|| {
        AppleSignInError::unexpected(status, "the Developer Portal team list is not JSON")
    })?;
    let result_code = text(&body, "resultCode");
    if !result_code.is_empty() && result_code != "0" {
        return Err(AppleSignInError::Refused {
            status,
            message: reason(),
        });
    }
    let entries = body.get("teams").and_then(Value::as_array).ok_or_else(|| {
        AppleSignInError::unexpected(status, "the Developer Portal answer has no team list")
    })?;
    let mut teams: Vec<PortalTeam> = entries
        .iter()
        .take(MAX_PORTAL_TEAMS)
        .map(|t| PortalTeam {
            team_id: text(t, "teamId").trim().to_string(),
            name: text(t, "name"),
            kind: text(t, "type"),
        })
        .filter(|t| is_valid_portal_team_id(&t.team_id))
        .collect();
    teams.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.team_id.cmp(&b.team_id)));
    Ok(teams)
}

/// The Developer Portal team of the App Store Connect team `asc_team` (one of
/// the Apple ID's `asc_team_count` App Store Connect teams), among `portal`.
///
/// spaceship selects the portal team and the App Store Connect team by the
/// same `team_name` (`ConnectAPI::Client.login`), so the rule is:
/// 1. exactly one portal team carries the App Store Connect team's name
///    (trimmed, exact) → that team;
/// 2. otherwise, when both sides have a single team → that team (one
///    membership, the names merely differ in form);
/// 3. otherwise there is no unambiguous answer, and guessing would show or
///    change *another team's* devices, certificates and profiles: an error.
///
/// # Errors
/// [`StackError::Unsupported`] when the Apple ID has no Developer Portal team
/// (no Apple Developer Program membership), or when the portal team cannot be
/// told apart (several carry the name, or none does and there are several).
pub(crate) fn match_portal_team(
    asc_team: &AppleTeam,
    asc_team_count: usize,
    portal: &[PortalTeam],
) -> Result<String, StackError> {
    if portal.is_empty() {
        return Err(StackError::unsupported(
            "provisioning is not available for this account: the Apple ID is not a member \
             of any Apple Developer Program team",
        ));
    }
    let wanted = asc_team.name.trim();
    let named: Vec<&PortalTeam> = portal
        .iter()
        .filter(|t| !wanted.is_empty() && t.name.trim() == wanted)
        .collect();
    match (named.as_slice(), portal) {
        ([only], _) => Ok(only.team_id.clone()),
        ([], [only]) if asc_team_count == 1 => Ok(only.team_id.clone()),
        ([], _) => Err(StackError::unsupported(format!(
            "provisioning is not available for this account: none of the Apple ID's {} \
             Developer Portal team(s) is named like App Store Connect team {}, so the \
             Developer Portal team cannot be chosen safely; connect the account with an App \
             Store Connect API key to use provisioning",
            portal.len(),
            asc_team.provider_id
        ))),
        (several, _) => Err(StackError::unsupported(format!(
            "provisioning is not available for this account: {} Developer Portal teams share \
             the name of App Store Connect team {}, so the Developer Portal team cannot be \
             chosen safely; connect the account with an App Store Connect API key to use \
             provisioning",
            several.len(),
            asc_team.provider_id
        ))),
    }
}

/// How a proxied provisioning request carries its real method (see the
/// module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProxiedMethod {
    /// `proxy_get`.
    Get,
    /// `proxy_delete`.
    Delete,
}

impl ProxiedMethod {
    /// The `X-HTTP-Method-Override` value.
    pub(crate) fn override_value(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Delete => "DELETE",
        }
    }
}

/// The body of a proxied `GET` / `DELETE`:
/// `{"urlEncodedQueryParams": <query>, "teamId": <team_id>}`, the query being
/// [`faraday_encode_query`] of `raw_query` (`None` → `null`).
pub(crate) fn proxied_body(raw_query: Option<&str>, team_id: &str) -> Value {
    json!({
        "urlEncodedQueryParams": raw_query.map(faraday_encode_query),
        "teamId": team_id,
    })
}

/// `body` with `data.attributes.teamId` set (`proxy_post`), creating
/// `data.attributes` when the body has none.
pub(crate) fn with_team_id(body: &Value, team_id: &str) -> Value {
    let mut body = body.clone();
    if !body.get("data").is_some_and(Value::is_object) {
        body["data"] = json!({});
    }
    if !body["data"].get("attributes").is_some_and(Value::is_object) {
        body["data"]["attributes"] = json!({});
    }
    body["data"]["attributes"]["teamId"] = json!(team_id);
    body
}

/// A URL query (`a=1&b[c]=x,y`, raw or already percent-encoded) re-encoded the
/// way Faraday's `NestedParamsEncoder.encode` encodes the params hash
/// spaceship builds: pairs decoded, sorted by key (then value), each key and
/// value escaped with Faraday's `Utils.escape`.
pub(crate) fn faraday_encode_query(raw_query: &str) -> String {
    let Ok(url) = Url::parse(&format!("http://q.invalid/?{raw_query}")) else {
        return String::new();
    };
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", faraday_escape(k), faraday_escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Faraday's `Utils.escape`: every byte outside `[A-Za-z0-9 .~_-]` as `%XX`
/// (upper-case hex), then a space as `+`.
fn faraday_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b' ' => out.push('+'),
            b if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'~' | b'_' | b'-') => {
                out.push(char::from(b));
            }
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Apple's error codes and titles in `body`, for messages and the probe's
/// verdict table: JSON:API `errors[]` (`code`, `title` / `detail`), the
/// Developer Portal's `resultCode` / `resultString` / `userString`, or the
/// idmsa/olympus `serviceErrors`. Empty when there are none. Scrubbed and
/// bounded.
pub fn summarize_errors(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(errors) = value.get("errors").and_then(Value::as_array) {
        parts.extend(errors.iter().take(MAX_SUMMARY_ERRORS).map(|e| {
            let code = text(e, "code");
            let title = [text(e, "title"), text(e, "detail")]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_default();
            format!("{code}: {title}")
        }));
    }
    let result_code = text(&value, "resultCode");
    if !result_code.is_empty() && result_code != "0" {
        let result_string = text(&value, "resultString");
        let detail = if result_string.is_empty() {
            String::new()
        } else {
            format!(" ({result_string})")
        };
        parts.push(format!(
            "resultCode {result_code}{detail}: {}",
            text(&value, "userString")
        ));
    }
    let service = transport::service_errors(&value);
    if !service.is_empty() {
        parts.push(transport::describe_errors(&service, ""));
    }
    parts
        .into_iter()
        .map(|p| {
            let cut: String = p.chars().take(MAX_SUMMARY_CHARS).collect();
            crate::logging::redact::scrub(cut.trim())
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A string or number field as text, empty otherwise.
pub(crate) fn text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap;
    use reqwest::StatusCode;

    fn reply(status: u16, body: Value) -> AppleReply {
        AppleReply {
            status: StatusCode::from_u16(status).unwrap(),
            headers: HeaderMap::new(),
            body: body.to_string(),
        }
    }

    fn asc(provider_id: u64, name: &str) -> AppleTeam {
        AppleTeam {
            provider_id,
            name: name.into(),
            public_provider_id: None,
            content_types: vec![],
        }
    }

    fn portal(team_id: &str, name: &str) -> PortalTeam {
        PortalTeam {
            team_id: team_id.into(),
            name: name.into(),
            kind: "Company/Organization".into(),
        }
    }

    #[test]
    fn list_teams_is_parsed_sorted_and_filtered() {
        let teams = parse_list_teams(&reply(
            200,
            json!({
                "resultCode": 0,
                "teams": [
                    { "teamId": "ZZZ9999999", "name": "Beta", "type": "Individual" },
                    { "teamId": "AAA1111111", "name": "Acme", "type": "Company/Organization" },
                    { "teamId": "bad id!", "name": "Junk" },
                    { "name": "No id" },
                    { "teamId": "X".repeat(MAX_PORTAL_TEAM_ID_LEN + 1), "name": "Too long" }
                ]
            }),
        ))
        .unwrap();
        assert_eq!(
            teams,
            vec![
                PortalTeam {
                    team_id: "AAA1111111".into(),
                    name: "Acme".into(),
                    kind: "Company/Organization".into()
                },
                PortalTeam {
                    team_id: "ZZZ9999999".into(),
                    name: "Beta".into(),
                    kind: "Individual".into()
                },
            ]
        );
    }

    #[test]
    fn list_teams_failures_are_mapped() {
        assert_eq!(
            parse_list_teams(&reply(401, json!({}))),
            Err(AppleSignInError::SessionRejected { status: 401 })
        );
        assert!(matches!(
            parse_list_teams(&reply(403, json!({"resultCode": 1100, "userString": "No access"}))),
            Err(AppleSignInError::Refused { status: 403, message }) if message.contains("No access")
        ));
        assert!(matches!(
            parse_list_teams(&reply(200, json!({"resultCode": 1200, "resultString": "x.NotAllowed"}))),
            Err(AppleSignInError::Refused { status: 200, message }) if message.contains("NotAllowed")
        ));
        assert_eq!(
            parse_list_teams(&reply(503, json!({}))),
            Err(AppleSignInError::AppleUnavailable { status: 503 })
        );
        assert!(matches!(
            parse_list_teams(&reply(200, json!({"resultCode": 0}))),
            Err(AppleSignInError::Unexpected { status: 200, .. })
        ));
        let mut not_json = reply(200, json!({}));
        not_json.body = "<html>".into();
        assert!(matches!(
            parse_list_teams(&not_json),
            Err(AppleSignInError::Unexpected { .. })
        ));
    }

    #[test]
    fn a_single_team_on_each_side_matches_whatever_the_names() {
        let id = match_portal_team(&asc(1, "Acme Inc."), 1, &[portal("AAA1111111", "ACME")]);
        assert_eq!(id.unwrap(), "AAA1111111");
    }

    #[test]
    fn several_portal_teams_match_by_name() {
        let teams = [
            portal("AAA1111111", "Acme"),
            portal("BBB2222222", "Beta Corp"),
            portal("CCC3333333", "Gamma"),
        ];
        assert_eq!(
            match_portal_team(&asc(2, " Beta Corp "), 3, &teams).unwrap(),
            "BBB2222222"
        );
        // A name match wins even when the Apple ID has one App Store Connect team.
        assert_eq!(
            match_portal_team(&asc(3, "Gamma"), 1, &teams).unwrap(),
            "CCC3333333"
        );
    }

    #[test]
    fn ambiguous_or_unmatched_teams_are_refused_not_guessed() {
        let twins = [portal("AAA1111111", "Acme"), portal("BBB2222222", "Acme")];
        assert!(matches!(
            match_portal_team(&asc(1, "Acme"), 2, &twins),
            Err(StackError::Unsupported { message }) if message.contains("2 Developer Portal teams share")
        ));
        let others = [portal("AAA1111111", "Acme"), portal("BBB2222222", "Beta")];
        assert!(matches!(
            match_portal_team(&asc(7, "Gamma"), 1, &others),
            Err(StackError::Unsupported { message }) if message.contains("none of the Apple ID's 2")
        ));
        // One portal team, but several App Store Connect teams and no name match:
        // the portal team may belong to another of them.
        assert!(matches!(
            match_portal_team(&asc(7, "Gamma"), 2, &[portal("AAA1111111", "Acme")]),
            Err(StackError::Unsupported { .. })
        ));
        // An unnamed App Store Connect team never matches by name.
        assert!(match_portal_team(&asc(7, ""), 2, &[portal("AAA1111111", "")]).is_err());
        assert!(matches!(
            match_portal_team(&asc(1, "Acme"), 1, &[]),
            Err(StackError::Unsupported { message }) if message.contains("Apple Developer Program")
        ));
    }

    #[test]
    fn portal_team_ids_are_validated() {
        assert!(is_valid_portal_team_id("ABCDE12345"));
        for bad in ["", "ABC DE", "ABC\"DE", "AB/CD", &"A".repeat(33)] {
            assert!(!is_valid_portal_team_id(bad), "{bad}");
        }
    }

    #[test]
    fn queries_are_encoded_like_faradays_nested_params_encoder() {
        assert_eq!(
            faraday_encode_query("sort=name&limit=200"),
            "limit=200&sort=name"
        );
        assert_eq!(
            faraday_encode_query("fields[certificates]=certificateContent,displayName"),
            "fields%5Bcertificates%5D=certificateContent%2CdisplayName"
        );
        assert_eq!(
            faraday_encode_query("sort=name&limit=200&include=bundleId"),
            "include=bundleId&limit=200&sort=name"
        );
        // Already-encoded input (a pagination link) is decoded first.
        assert_eq!(
            faraday_encode_query("cursor=QU%3D%3D&limit=200"),
            "cursor=QU%3D%3D&limit=200"
        );
        assert_eq!(faraday_encode_query("q=a b~c"), "q=a+b~c");
        assert_eq!(faraday_encode_query(""), "");
    }

    #[test]
    fn proxied_bodies_have_spaceships_shape() {
        assert_eq!(
            proxied_body(Some("limit=1"), "T1"),
            json!({"urlEncodedQueryParams": "limit=1", "teamId": "T1"})
        );
        assert_eq!(
            proxied_body(Some(""), "T1"),
            json!({"urlEncodedQueryParams": "", "teamId": "T1"})
        );
        assert_eq!(
            proxied_body(None, "T1"),
            json!({"urlEncodedQueryParams": null, "teamId": "T1"})
        );
        assert_eq!(
            with_team_id(
                &json!({"data": {"type": "devices", "attributes": {"name": "n"}}}),
                "T1"
            ),
            json!({"data": {"type": "devices", "attributes": {"name": "n", "teamId": "T1"}}})
        );
        assert_eq!(
            with_team_id(&json!({"data": {"type": "x"}}), "T1"),
            json!({"data": {"type": "x", "attributes": {"teamId": "T1"}}})
        );
        assert_eq!(ProxiedMethod::Get.override_value(), "GET");
        assert_eq!(ProxiedMethod::Delete.override_value(), "DELETE");
        assert_eq!(
            list_teams_path(),
            "services-account/QH65B2/account/listTeams.action"
        );
    }
}
