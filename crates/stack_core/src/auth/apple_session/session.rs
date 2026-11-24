//! The Apple ID web session: its cookie jar, the opaque blob it persists to,
//! the olympus session (teams) and team switching.

use std::convert::Infallible;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use reqwest::header::HeaderValue;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use zeroize::Zeroize;

use super::error::AppleSignInError;
use super::portal;
use super::transport::{self, common_status_error, describe_errors, service_errors, AppleReply};
use super::AppleEndpoints;
use crate::logging::{self, target};

/// Identifies a blob this module wrote.
const BLOB_FORMAT: &str = "stack_core.apple_session";
/// The current blob layout. Bump it (and keep reading the old one) when the
/// layout changes. Version 2 added the Developer Portal teams
/// (`portal_teams`); a version 1 blob reads as having none.
pub(crate) const BLOB_VERSION: u32 = 2;
/// The oldest blob layout still read.
const OLDEST_BLOB_VERSION: u32 = 1;
/// Blobs larger than this are refused before parsing.
const MAX_BLOB_BYTES: usize = 1024 * 1024;

/// At most this many teams are read from an olympus session.
const MAX_TEAMS: usize = 500;

/// The olympus session resource (`fetch_olympus_session`, `team_id=`).
const OLYMPUS_SESSION_PATH: &str = "olympus/v1/session";

/// The request `Cookie` value: cookies are sent as Apple set them, like a
/// browser does, except that a `DES…` cookie (the two-factor trust cookie,
/// which Apple sets quoted) held unquoted is sent quoted. That is the
/// workaround in spaceship's `send_shared_login_request`, whose cookie jar
/// strips the quotes.
fn cookie_header<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    pairs
        .map(|(name, value)| {
            let quoted = value.len() >= 2 && value.starts_with('"') && value.ends_with('"');
            if name.starts_with("DES") && !quoted {
                format!("{name}=\"{value}\"")
            } else {
                format!("{name}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The kind of a JSON (de)serialization failure, without its text (which can
/// quote the offending value, i.e. a cookie).
fn json_failure(err: &serde_json::Error) -> &'static str {
    match err.classify() {
        serde_json::error::Category::Io => "I/O error",
        serde_json::error::Category::Syntax => "syntax error",
        serde_json::error::Category::Data => "unexpected data",
        serde_json::error::Category::Eof => "truncated",
    }
}

/// The session's cookies. Implements reqwest's cookie provider, so every
/// request of the session client gets them and every response's `Set-Cookie`
/// updates them, redirects included.
#[derive(Default)]
pub(crate) struct SessionJar {
    store: RwLock<cookie_store::CookieStore>,
}

impl fmt::Debug for SessionJar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionJar([redacted])")
    }
}

impl SessionJar {
    fn from_cookies(cookies: Vec<cookie_store::Cookie<'static>>) -> Self {
        // Expired cookies are dropped on load.
        let store = match cookie_store::CookieStore::from_cookies(
            cookies.into_iter().map(Ok::<_, Infallible>),
            false,
        ) {
            Ok(store) => store,
            Err(never) => match never {},
        };
        Self {
            store: RwLock::new(store),
        }
    }

    /// Every unexpired cookie, session cookies included (spaceship saves with
    /// `session: true`: `myacinfo` has no expiry and is the session).
    /// Sorted by domain, path and name, so equal jars serialize equally.
    fn snapshot(&self) -> Vec<cookie_store::Cookie<'static>> {
        let mut cookies: Vec<cookie_store::Cookie<'static>> = self
            .store
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter_unexpired()
            .cloned()
            .collect();
        cookies.sort_by(|a, b| {
            (String::from(&a.domain), String::from(&a.path), a.name()).cmp(&(
                String::from(&b.domain),
                String::from(&b.path),
                b.name(),
            ))
        });
        cookies
    }

    /// The names of the unexpired cookies (never their values).
    #[cfg(any(feature = "apple-signin-probe", test))]
    pub(crate) fn cookie_names(&self) -> Vec<String> {
        self.snapshot()
            .iter()
            .map(|c| c.name().to_string())
            .collect()
    }
}

impl reqwest::cookie::CookieStore for SessionJar {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let cookies = cookie_headers.filter_map(|value| {
            let text = value.to_str().ok()?;
            cookie_store::RawCookie::parse(text.to_string()).ok()
        });
        self.store
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .store_response_cookies(cookies, url);
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
        let header = cookie_header(store.get_request_values(url));
        if header.is_empty() {
            return None;
        }
        HeaderValue::from_str(&header).ok()
    }
}

/// The persisted form of an [`AppleSession`]: opaque, versioned bytes the host
/// keeps in its secure storage (the Keychain on iOS) and hands back to
/// [`AppleSession::restore`].
///
/// It holds the session cookies, so it is a credential: its `Debug` shows
/// only its size, and its bytes are wiped on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionBlob(Vec<u8>);

impl SessionBlob {
    /// Wraps bytes previously produced by [`Self::as_bytes`].
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// The bytes to store.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SessionBlob {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SessionBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionBlob({} bytes)", self.0.len())
    }
}

/// The blob layout (JSON), versions 1 and 2: version 1 has no
/// `portal_teams`, which then reads as empty.
#[derive(Serialize, Deserialize)]
struct BlobLayout {
    format: String,
    version: u32,
    /// The last widget key that worked: a fallback when the sign-out redirect
    /// does not yield one.
    widget_key: Option<String>,
    /// The App Store Connect team (`providerId`) last selected.
    team_id: Option<u64>,
    /// Version 2: the Developer Portal team resolved for each App Store
    /// Connect team, so `listTeams.action` is called once per team.
    #[serde(default)]
    portal_teams: Vec<PortalTeamBinding>,
    cookies: Vec<cookie_store::Cookie<'static>>,
}

/// The Developer Portal team (`listTeams.action` `teamId`) resolved for an
/// App Store Connect team (`providerId`). One blob serves every team of the
/// Apple ID, so it keeps one binding per App Store Connect team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PortalTeamBinding {
    pub(crate) provider_id: u64,
    pub(crate) portal_team_id: String,
}

/// At most this many portal team bindings are kept (one per App Store Connect
/// team).
const MAX_PORTAL_BINDINGS: usize = MAX_TEAMS;

/// `bindings` with invalid ids dropped, one per provider (the first wins) and
/// at most [`MAX_PORTAL_BINDINGS`]: a corrupt or hand-edited blob must not
/// put an arbitrary string into a request body.
fn sanitize_bindings(bindings: Vec<PortalTeamBinding>) -> Vec<PortalTeamBinding> {
    let mut kept: Vec<PortalTeamBinding> = Vec::new();
    for binding in bindings {
        if kept.len() >= MAX_PORTAL_BINDINGS {
            break;
        }
        if portal::is_valid_portal_team_id(&binding.portal_team_id)
            && !kept.iter().any(|b| b.provider_id == binding.provider_id)
        {
            kept.push(binding);
        }
    }
    kept
}

/// Just enough of a blob to dispatch on its version.
#[derive(Deserialize)]
struct BlobHeader {
    format: String,
    version: u32,
}

/// An App Store Connect team (an olympus "provider").
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AppleTeam {
    /// `providerId`: what team switching and the session use.
    pub provider_id: u64,
    /// The team's display name.
    pub name: String,
    /// `publicProviderId`, when Apple sends one.
    pub public_provider_id: Option<String>,
    /// `contentTypes` (e.g. `SOFTWARE`).
    pub content_types: Vec<String>,
}

/// What `GET olympus/v1/session` says about the signed-in user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OlympusSession {
    /// The team the session currently acts for (`provider.providerId`).
    pub current_team_id: Option<u64>,
    /// Every team the Apple ID belongs to (`availableProviders`), sorted by
    /// name then id, as spaceship's `teams` sorts them.
    pub teams: Vec<AppleTeam>,
}

/// A raw App Store Connect response read with the session.
#[cfg(any(feature = "apple-signin-probe", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionResponse {
    /// The HTTP status.
    pub status: u16,
    /// The body (JSON for iris).
    pub body: String,
}

/// A signed-in Apple ID web session.
pub struct AppleSession {
    endpoints: AppleEndpoints,
    jar: Arc<SessionJar>,
    http: reqwest::Client,
    widget_key: Option<String>,
    team_id: Option<u64>,
    portal_teams: Vec<PortalTeamBinding>,
}

impl fmt::Debug for AppleSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppleSession")
            .field("endpoints", &self.endpoints)
            .field("team_id", &self.team_id)
            .finish_non_exhaustive()
    }
}

impl AppleSession {
    /// A session over an existing jar and client (the sign-in flow hands its
    /// own over when it finishes).
    pub(crate) fn from_parts(
        endpoints: AppleEndpoints,
        jar: Arc<SessionJar>,
        http: reqwest::Client,
        widget_key: Option<String>,
        team_id: Option<u64>,
    ) -> Self {
        Self {
            endpoints,
            jar,
            http,
            widget_key,
            team_id,
            portal_teams: Vec::new(),
        }
    }

    /// The pieces a new sign-in reuses to sign in again with this session's
    /// cookies (a `DES…` trust cookie lets Apple skip the two-factor step).
    pub(crate) fn into_parts(
        self,
    ) -> (
        AppleEndpoints,
        Arc<SessionJar>,
        reqwest::Client,
        Option<String>,
    ) {
        (self.endpoints, self.jar, self.http, self.widget_key)
    }

    /// Restores a session from a blob [`Self::to_blob`] wrote. Nothing is sent:
    /// call [`Self::olympus_session`] to check the cookies are still good.
    ///
    /// # Errors
    /// [`AppleSignInError::InvalidSessionBlob`] for bytes that are not a blob
    /// of a known version; [`AppleSignInError::Network`] when the HTTP client
    /// cannot be built.
    pub fn restore(
        blob: &SessionBlob,
        endpoints: AppleEndpoints,
    ) -> Result<Self, AppleSignInError> {
        let bytes = blob.as_bytes();
        if bytes.len() > MAX_BLOB_BYTES {
            return Err(AppleSignInError::invalid_blob(format!(
                "it is larger than {MAX_BLOB_BYTES} bytes"
            )));
        }
        let header: BlobHeader = serde_json::from_slice(bytes)
            .map_err(|_| AppleSignInError::invalid_blob("it is not a session blob"))?;
        if header.format != BLOB_FORMAT {
            return Err(AppleSignInError::invalid_blob("it is not a session blob"));
        }
        if !(OLDEST_BLOB_VERSION..=BLOB_VERSION).contains(&header.version) {
            return Err(AppleSignInError::invalid_blob(format!(
                "format version {} is not supported (expected {OLDEST_BLOB_VERSION} to \
                 {BLOB_VERSION})",
                header.version
            )));
        }
        let blob: BlobLayout = serde_json::from_slice(bytes).map_err(|e| {
            AppleSignInError::invalid_blob(format!(
                "malformed version {} blob ({})",
                header.version,
                json_failure(&e)
            ))
        })?;
        // A version 1 blob has no portal teams; ignore any that one claims.
        let portal_teams = if header.version >= 2 {
            sanitize_bindings(blob.portal_teams)
        } else {
            Vec::new()
        };

        let jar = Arc::new(SessionJar::from_cookies(blob.cookies));
        let http = transport::session_client(Arc::clone(&jar))?;
        Ok(Self {
            endpoints,
            jar,
            http,
            widget_key: blob.widget_key,
            team_id: blob.team_id,
            portal_teams,
        })
    }

    /// Serializes the cookies, the widget key, the selected team and the
    /// resolved Developer Portal teams (always the current layout,
    /// [`BLOB_VERSION`]).
    ///
    /// # Errors
    /// [`AppleSignInError::InvalidSessionBlob`] if serialization fails (it
    /// does not for cookies this jar parsed).
    pub fn to_blob(&self) -> Result<SessionBlob, AppleSignInError> {
        let blob = BlobLayout {
            format: BLOB_FORMAT.to_string(),
            version: BLOB_VERSION,
            widget_key: self.widget_key.clone(),
            team_id: self.team_id,
            portal_teams: self.portal_teams.clone(),
            cookies: self.jar.snapshot(),
        };
        serde_json::to_vec(&blob).map(SessionBlob).map_err(|e| {
            AppleSignInError::invalid_blob(format!("cannot serialize ({})", json_failure(&e)))
        })
    }

    /// The team selected with [`Self::select_team`] (or restored), if any.
    pub fn team_id(&self) -> Option<u64> {
        self.team_id
    }

    /// The Developer Portal team resolved for the App Store Connect team
    /// `provider_id`, if any.
    pub(crate) fn portal_team_id(&self, provider_id: u64) -> Option<&str> {
        self.portal_teams
            .iter()
            .find(|b| b.provider_id == provider_id)
            .map(|b| b.portal_team_id.as_str())
    }

    /// [`Self::portal_team_id`], for the dev probe (a portal team id is not a
    /// secret).
    #[cfg(feature = "apple-signin-probe")]
    pub fn saved_portal_team(&self, provider_id: u64) -> Option<String> {
        self.portal_team_id(provider_id).map(str::to_string)
    }

    /// Remembers `portal_team_id` as the Developer Portal team of the App
    /// Store Connect team `provider_id` (saved with the blob). An invalid id
    /// is ignored, and so is a new team once [`MAX_PORTAL_BINDINGS`] are
    /// kept.
    pub(crate) fn remember_portal_team(&mut self, provider_id: u64, portal_team_id: String) {
        if !portal::is_valid_portal_team_id(&portal_team_id) {
            return;
        }
        if let Some(binding) = self
            .portal_teams
            .iter_mut()
            .find(|b| b.provider_id == provider_id)
        {
            binding.portal_team_id = portal_team_id;
        } else if self.portal_teams.len() < MAX_PORTAL_BINDINGS {
            self.portal_teams.push(PortalTeamBinding {
                provider_id,
                portal_team_id,
            });
        }
    }

    /// `{developer}/services-account`: the Developer Portal API root.
    pub(crate) fn services_account_base_url(&self) -> String {
        self.endpoints.developer(portal::SERVICES_ACCOUNT)
    }

    /// spaceship's `PortalClient#teams`: `POST services-account/QH65B2/
    /// account/listTeams.action` with no params and no body (so an empty body
    /// and `Content-Length: 0`, as `Net::HTTP` sends it; hyper would send no
    /// length and the portal answers 411), the session cookies, the given
    /// CSRF headers and `Net::HTTP`'s default headers. The reply is returned
    /// whatever its status; parse it with [`portal::parse_list_teams`].
    ///
    /// # Errors
    /// [`AppleSignInError::Network`] on transport failure,
    /// [`AppleSignInError::Unexpected`] for an oversized or undecodable body.
    pub(crate) async fn send_list_teams(
        &self,
        csrf: &[(&'static str, String)],
    ) -> Result<AppleReply, AppleSignInError> {
        let mut builder = self
            .http
            .post(self.endpoints.developer(&portal::list_teams_path()));
        for (name, value) in portal::net_http_defaults() {
            builder = builder.header(name, value);
        }
        for (name, value) in csrf {
            builder = builder.header(*name, value);
        }
        transport::send(
            builder
                .header(reqwest::header::CONTENT_LENGTH, "0")
                .body(Vec::<u8>::new()),
        )
        .await
    }

    /// The session's HTTP client: its cookie jar, no redirect following,
    /// the sign-in `User-Agent`. Requests built on it carry the session's
    /// cookies, so it must only be used for App Store Connect.
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The hosts the session talks to.
    #[cfg(any(feature = "apple-signin-probe", test))]
    pub(crate) fn endpoints(&self) -> &AppleEndpoints {
        &self.endpoints
    }

    /// iris, App Store Connect's web API (`{app_store_connect}/iris`): the
    /// same JSON:API resources as the public API, under `/iris/v1/...`.
    pub(crate) fn iris_base_url(&self) -> String {
        self.endpoints.app_store_connect("iris")
    }

    /// The widget key the session signed in with, if known.
    #[cfg(test)]
    pub(crate) fn widget_key(&self) -> Option<&str> {
        self.widget_key.as_deref()
    }

    /// The cookie jar, for tests that plant cookies.
    #[cfg(test)]
    pub(crate) fn jar(&self) -> &Arc<SessionJar> {
        &self.jar
    }

    /// The names of the cookies the session holds (never their values), for
    /// diagnostics.
    #[cfg(any(feature = "apple-signin-probe", test))]
    pub fn cookie_names(&self) -> Vec<String> {
        self.jar.cookie_names()
    }

    /// `GET olympus/v1/session`: the current team and every team.
    ///
    /// # Errors
    /// [`AppleSignInError::SessionRejected`] on 401/403 (signed out, expired,
    /// or a stale widget key — Apple accepts a stale key for the sign-in and
    /// only refuses the session); [`AppleSignInError::NotEnabledForAppStoreConnect`]
    /// when the session has no team; the common status errors.
    pub async fn olympus_session(&self) -> Result<OlympusSession, AppleSignInError> {
        let reply = transport::send(
            self.http
                .get(self.endpoints.app_store_connect(OLYMPUS_SESSION_PATH))
                .header("Accept", "application/json"),
        )
        .await?;
        let session = parse_olympus_session(&reply)?;
        logging::info(target::AUTH, || {
            format!(
                "Apple ID session: {} team(s), current team {}",
                session.teams.len(),
                session
                    .current_team_id
                    .map_or_else(|| "none".to_string(), |id| id.to_string())
            )
        });
        Ok(session)
    }

    /// Every team the Apple ID belongs to.
    ///
    /// # Errors
    /// As [`Self::olympus_session`].
    #[cfg(any(feature = "apple-signin-probe", test))]
    pub async fn teams(&self) -> Result<Vec<AppleTeam>, AppleSignInError> {
        Ok(self.olympus_session().await?.teams)
    }

    /// Switches the session to `team_id` (`POST olympus/v1/session`), after
    /// checking it is one of the Apple ID's teams (otherwise App Store Connect
    /// answers a confusing "invalid content provider id", per spaceship's
    /// `team_id=`).
    ///
    /// # Errors
    /// [`AppleSignInError::InvalidInput`] for a team the Apple ID is not in
    /// (nothing switched); [`AppleSignInError::Refused`] when App Store Connect
    /// reports an error; the errors of [`Self::olympus_session`].
    pub async fn select_team(&mut self, team_id: u64) -> Result<(), AppleSignInError> {
        let session = self.olympus_session().await?;
        if !session.teams.iter().any(|t| t.provider_id == team_id) {
            return Err(AppleSignInError::invalid_input(format!(
                "team {team_id} is not one of this Apple ID's {} team(s)",
                session.teams.len()
            )));
        }
        let reply = transport::send(
            self.http
                .post(self.endpoints.app_store_connect(OLYMPUS_SESSION_PATH))
                .header("Content-Type", "application/json")
                .header("X-Requested-With", "olympus-ui")
                .header("Accept", "application/json")
                .body(json!({ "provider": { "providerId": team_id } }).to_string()),
        )
        .await?;
        check_team_switch(&reply)?;
        self.team_id = Some(team_id);
        logging::info(target::AUTH, || {
            format!("Apple ID session: switched to team {team_id}")
        });
        Ok(())
    }

    /// `GET {app_store_connect}/{relative}` with the session, as App Store
    /// Connect's web client calls iris (`x-csrf-itc: [asc-ui]`, see
    /// spaceship's `ConnectAPI` clients).
    ///
    /// # Errors
    /// [`AppleSignInError::InvalidInput`] for an absolute or scheme-relative
    /// path (the session's cookies only go to App Store Connect);
    /// [`AppleSignInError::Network`] on transport failure. A non-2xx status
    /// is returned, not an error.
    #[cfg(any(feature = "apple-signin-probe", test))]
    pub async fn app_store_connect_get(
        &self,
        relative: &str,
    ) -> Result<SessionResponse, AppleSignInError> {
        if relative.contains("://") || relative.starts_with('/') {
            return Err(AppleSignInError::invalid_input(
                "the path must be relative to App Store Connect",
            ));
        }
        let reply = transport::send(
            self.http
                .get(self.endpoints.app_store_connect(relative))
                .header("Accept", "application/json")
                .header("x-csrf-itc", "[asc-ui]"),
        )
        .await?;
        Ok(SessionResponse {
            status: reply.code(),
            body: reply.body,
        })
    }
}

/// Maps the olympus session response.
fn parse_olympus_session(reply: &AppleReply) -> Result<OlympusSession, AppleSignInError> {
    let status = reply.code();
    if status == 401 || status == 403 {
        return Err(AppleSignInError::SessionRejected { status });
    }
    if let Some(err) = common_status_error(reply.status) {
        return Err(err);
    }
    if !reply.status.is_success() {
        return Err(AppleSignInError::unexpected(
            status,
            "the olympus session request failed",
        ));
    }
    let body = reply
        .json()
        .ok_or_else(|| AppleSignInError::unexpected(status, "the olympus session is not JSON"))?;

    let current_team_id = body
        .get("provider")
        .and_then(|p| provider_id(p.get("providerId")));
    let mut teams: Vec<AppleTeam> = body
        .get("availableProviders")
        .and_then(Value::as_array)
        .map(|providers| {
            providers
                .iter()
                .take(MAX_TEAMS)
                .filter_map(parse_team)
                .collect()
        })
        .unwrap_or_default();
    teams.sort_by(|a, b| a.name.cmp(&b.name).then(a.provider_id.cmp(&b.provider_id)));

    if current_team_id.is_none() && teams.is_empty() {
        return Err(AppleSignInError::NotEnabledForAppStoreConnect);
    }
    Ok(OlympusSession {
        current_team_id,
        teams,
    })
}

fn parse_team(value: &Value) -> Option<AppleTeam> {
    Some(AppleTeam {
        provider_id: provider_id(value.get("providerId"))?,
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        public_provider_id: value
            .get("publicProviderId")
            .and_then(Value::as_str)
            .map(str::to_string),
        content_types: value
            .get("contentTypes")
            .and_then(Value::as_array)
            .map(|types| {
                types
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// `providerId` as a number, whether Apple sends it as one or as a string.
fn provider_id(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `handle_itc_response` for the team switch: a 2xx without error entries.
fn check_team_switch(reply: &AppleReply) -> Result<(), AppleSignInError> {
    let status = reply.code();
    if status == 401 || status == 403 {
        return Err(AppleSignInError::SessionRejected { status });
    }
    if let Some(err) = common_status_error(reply.status) {
        return Err(err);
    }
    let body = reply.json().unwrap_or(Value::Null);
    let errors = service_errors(&body);
    let section_errors = body
        .get("sectionErrorKeys")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty());
    if !reply.status.is_success() || !errors.is_empty() || section_errors {
        return Err(AppleSignInError::Refused {
            status,
            message: describe_errors(&errors, "App Store Connect did not switch the team"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::apple_session::test_support::{olympus_session_body, session_on};
    use reqwest::cookie::CookieStore as _;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn set(jar: &SessionJar, url: &Url, cookies: &[&'static str]) {
        let values: Vec<HeaderValue> = cookies
            .iter()
            .map(|c| HeaderValue::from_static(c))
            .collect();
        jar.set_cookies(&mut values.iter(), url);
    }

    #[test]
    fn jar_sends_cookies_back_and_shares_apple_dot_com_cookies() {
        let jar = SessionJar::default();
        let idmsa = url("https://idmsa.apple.com/appleauth/auth/signin/complete");
        set(
            &jar,
            &idmsa,
            &[
                "myacinfo=session-value; Domain=apple.com; Path=/; Secure; HttpOnly",
                "aasp=host-only; Path=/; Secure",
            ],
        );
        let asc = url("https://appstoreconnect.apple.com/olympus/v1/session");
        assert_eq!(
            jar.cookies(&asc).unwrap().to_str().unwrap(),
            "myacinfo=session-value"
        );
        let header = jar.cookies(&idmsa).unwrap();
        let header = header.to_str().unwrap();
        assert!(header.contains("myacinfo=session-value") && header.contains("aasp=host-only"));
        // Other sites never see them.
        assert!(jar.cookies(&url("https://example.com/")).is_none());
    }

    #[test]
    fn des_trust_cookie_is_sent_quoted_exactly_once() {
        assert_eq!(
            cookie_header([("DES5c14", "HSARM==SRVT"), ("myacinfo", "v")].into_iter()),
            "DES5c14=\"HSARM==SRVT\"; myacinfo=v"
        );
        assert_eq!(
            cookie_header([("DES5c14", "\"HSARM==SRVT\"")].into_iter()),
            "DES5c14=\"HSARM==SRVT\""
        );
        assert_eq!(cookie_header([("DESx", "\"")].into_iter()), "DESx=\"\"\"");
        assert_eq!(cookie_header([("other", "a")].into_iter()), "other=a");

        // Through the jar: a quoted Set-Cookie value is kept as Apple sent it.
        let jar = SessionJar::default();
        let idmsa = url("https://idmsa.apple.com/appleauth/auth/2sv/trust");
        set(&jar, &idmsa, &["DES5c148586=\"HSARMtrust==SRVT\"; Domain=idmsa.apple.com; Path=/; Max-Age=2592000; Secure"]);
        assert_eq!(
            jar.cookies(&idmsa).unwrap().to_str().unwrap(),
            "DES5c148586=\"HSARMtrust==SRVT\""
        );
    }

    #[tokio::test]
    async fn blob_round_trips_cookies_widget_key_and_team() {
        let server = MockServer::start().await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        let mut session = session_on(&endpoints, Some("widget-123".to_string()));
        let base = url(&server.uri());
        set(
            &session.jar,
            &base,
            &[
                "myacinfo=session-cookie; Path=/; HttpOnly",
                "DES5c148586=\"trust\"; Path=/; Max-Age=2592000",
                "gone=x; Path=/; Max-Age=0",
            ],
        );
        session.team_id = Some(118_291_181);

        let blob = session.to_blob().unwrap();
        assert!(
            format!("{blob:?}").starts_with("SessionBlob(")
                && !format!("{blob:?}").contains("session-cookie")
        );
        let restored = AppleSession::restore(
            &SessionBlob::from_bytes(blob.as_bytes().to_vec()),
            endpoints.clone(),
        )
        .unwrap();
        assert_eq!(restored.team_id(), Some(118_291_181));
        assert_eq!(restored.widget_key(), Some("widget-123"));
        let mut names = restored.cookie_names();
        names.sort();
        assert_eq!(
            names,
            ["DES5c148586", "myacinfo"],
            "expired cookies are not kept"
        );
        assert_eq!(
            restored.to_blob().unwrap().as_bytes(),
            blob.as_bytes(),
            "a restored session serializes to the same blob"
        );

        // The restored cookies really go out on the next request.
        Mock::given(method("GET"))
            .and(path("/olympus/v1/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(olympus_session_body()))
            .expect(1)
            .mount(&server)
            .await;
        let olympus = restored.olympus_session().await.unwrap();
        assert_eq!(olympus.current_team_id, Some(118_291_181));
        let requests = server.received_requests().await.unwrap();
        let sent = requests[0].headers.get("cookie").unwrap().to_str().unwrap();
        let mut sent: Vec<&str> = sent.split("; ").collect();
        sent.sort_unstable();
        assert_eq!(sent, ["DES5c148586=\"trust\"", "myacinfo=session-cookie"]);
    }

    #[test]
    fn restore_refuses_foreign_and_future_blobs() {
        let endpoints = AppleEndpoints::default();
        let cases: [(&[u8], &str); 6] = [
            (b"not json", "not a session blob"),
            (br#"{"format":"other","version":1}"#, "not a session blob"),
            (
                br#"{"format":"stack_core.apple_session","version":3,"cookies":[]}"#,
                "version 3 is not supported",
            ),
            (
                br#"{"format":"stack_core.apple_session","version":1}"#,
                "malformed version 1 blob",
            ),
            (
                br#"{"format":"stack_core.apple_session","version":1,"cookies":[{"x":1}]}"#,
                "malformed version 1 blob",
            ),
            (
                br#"{"format":"stack_core.apple_session","version":2,"cookies":[],"portal_teams":[{"provider_id":"x"}]}"#,
                "malformed version 2 blob",
            ),
        ];
        for (bytes, expected) in cases {
            let err =
                AppleSession::restore(&SessionBlob::from_bytes(bytes.to_vec()), endpoints.clone())
                    .unwrap_err();
            assert!(
                matches!(&err, AppleSignInError::InvalidSessionBlob { message } if message.contains(expected)),
                "{expected}: {err}"
            );
        }
        let huge = vec![b' '; MAX_BLOB_BYTES + 1];
        assert!(matches!(
            AppleSession::restore(&SessionBlob::from_bytes(huge), endpoints),
            Err(AppleSignInError::InvalidSessionBlob { .. })
        ));
    }

    /// A blob as Phase 2 wrote it (version 1, no portal teams).
    fn v1_blob(extra: &str) -> Vec<u8> {
        format!(
            r#"{{"format":"stack_core.apple_session","version":1,"widget_key":"wk","team_id":118291181{extra},"cookies":[]}}"#
        )
        .into_bytes()
    }

    #[test]
    fn a_version_1_blob_is_read_and_rewritten_as_version_2() {
        let endpoints = AppleEndpoints::default();
        let mut session =
            AppleSession::restore(&SessionBlob::from_bytes(v1_blob("")), endpoints.clone())
                .unwrap();
        assert_eq!(session.team_id(), Some(118_291_181));
        assert_eq!(session.widget_key(), Some("wk"));
        assert_eq!(session.portal_team_id(118_291_181), None);

        // A version 1 blob never carries portal teams, even if it claims some.
        let claimed =
            v1_blob(r#","portal_teams":[{"provider_id":118291181,"portal_team_id":"ABCDE12345"}]"#);
        let restored =
            AppleSession::restore(&SessionBlob::from_bytes(claimed), endpoints.clone()).unwrap();
        assert_eq!(restored.portal_team_id(118_291_181), None);

        session.remember_portal_team(118_291_181, "ABCDE12345".into());
        let rewritten = session.to_blob().unwrap();
        let json: Value = serde_json::from_slice(rewritten.as_bytes()).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(
            json["portal_teams"],
            json!([{"provider_id": 118_291_181, "portal_team_id": "ABCDE12345"}])
        );
        let again = AppleSession::restore(&rewritten, endpoints).unwrap();
        assert_eq!(again.portal_team_id(118_291_181), Some("ABCDE12345"));
        assert_eq!(again.team_id(), Some(118_291_181));
        assert_eq!(again.widget_key(), Some("wk"));
    }

    #[test]
    fn portal_team_bindings_are_validated_on_load_and_on_remember() {
        let blob = br#"{"format":"stack_core.apple_session","version":2,"widget_key":null,"team_id":null,"cookies":[],"portal_teams":[
            {"provider_id":1,"portal_team_id":"GOOD111111"},
            {"provider_id":1,"portal_team_id":"DUPE222222"},
            {"provider_id":2,"portal_team_id":"bad\"id"},
            {"provider_id":3,"portal_team_id":""}
        ]}"#;
        let mut session = AppleSession::restore(
            &SessionBlob::from_bytes(blob.to_vec()),
            AppleEndpoints::default(),
        )
        .unwrap();
        assert_eq!(session.portal_team_id(1), Some("GOOD111111"));
        assert_eq!(session.portal_team_id(2), None);
        assert_eq!(session.portal_team_id(3), None);

        session.remember_portal_team(4, "not valid!".into());
        assert_eq!(session.portal_team_id(4), None);
        session.remember_portal_team(1, "NEW3333333".into());
        assert_eq!(session.portal_team_id(1), Some("NEW3333333"));
        for provider in 10..(10 + MAX_PORTAL_BINDINGS as u64) {
            session.remember_portal_team(provider, "TEAM444444".into());
        }
        assert_eq!(session.portal_teams.len(), MAX_PORTAL_BINDINGS);
    }

    #[tokio::test]
    async fn list_teams_is_an_empty_post_with_net_http_headers_and_csrf() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/services-account/QH65B2/account/listTeams.action"))
            .and(header("content-length", "0"))
            .and(header("accept", "*/*"))
            .and(header("csrf", "c1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"resultCode": 0, "teams": []})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri())
            .with_developer_base_url(&server.uri());
        let session = session_on(&endpoints, None);
        assert_eq!(
            session.services_account_base_url(),
            format!("{}/services-account", server.uri())
        );
        let reply = session
            .send_list_teams(&[("csrf", "c1".to_string())])
            .await
            .unwrap();
        assert_eq!(reply.code(), 200);
        let sent = &server.received_requests().await.unwrap()[0];
        assert!(sent.body.is_empty());
        assert!(sent.headers.get("content-type").is_none());
    }

    #[test]
    fn an_empty_session_round_trips() {
        let endpoints = AppleEndpoints::default();
        let session = session_on(&endpoints, None);
        let blob = session.to_blob().unwrap();
        let restored = AppleSession::restore(&blob, endpoints).unwrap();
        assert_eq!(restored.team_id(), None);
        assert_eq!(restored.widget_key(), None);
        assert!(restored.cookie_names().is_empty());
    }

    #[tokio::test]
    async fn olympus_session_parses_teams_sorted_like_spaceship() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/olympus/v1/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user": {"emailAddress": "dev@example.com"},
                "provider": {"providerId": 2, "name": "Beta"},
                "availableProviders": [
                    {"providerId": 2, "name": "Beta", "contentTypes": ["SOFTWARE"], "publicProviderId": "uuid-2"},
                    {"providerId": "1", "name": "Alpha"},
                    {"providerId": 3, "name": "Alpha"},
                    {"name": "no id"}
                ]
            })))
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        let session = session_on(&endpoints, None);
        let olympus = session.olympus_session().await.unwrap();
        assert_eq!(olympus.current_team_id, Some(2));
        assert_eq!(session.teams().await.unwrap(), olympus.teams);
        let ids: Vec<u64> = olympus.teams.iter().map(|t| t.provider_id).collect();
        assert_eq!(ids, [1, 3, 2]);
        assert_eq!(
            olympus.teams[2].public_provider_id.as_deref(),
            Some("uuid-2")
        );
        assert_eq!(olympus.teams[2].content_types, ["SOFTWARE"]);
    }

    #[tokio::test]
    async fn olympus_session_maps_failures() {
        let cases = [
            (
                ResponseTemplate::new(401),
                AppleSignInError::SessionRejected { status: 401 },
            ),
            (
                ResponseTemplate::new(403),
                AppleSignInError::SessionRejected { status: 403 },
            ),
            (
                ResponseTemplate::new(503),
                AppleSignInError::AppleUnavailable { status: 503 },
            ),
            (ResponseTemplate::new(429), AppleSignInError::RateLimited),
            (
                ResponseTemplate::new(200).set_body_json(json!({"user": {}})),
                AppleSignInError::NotEnabledForAppStoreConnect,
            ),
        ];
        for (response, expected) in cases {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/olympus/v1/session"))
                .respond_with(response)
                .mount(&server)
                .await;
            let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
            let err = session_on(&endpoints, None)
                .olympus_session()
                .await
                .unwrap_err();
            assert_eq!(err, expected);
        }
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/olympus/v1/session"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        assert!(matches!(
            session_on(&endpoints, None).olympus_session().await,
            Err(AppleSignInError::Unexpected { status: 200, .. })
        ));
    }

    #[tokio::test]
    async fn select_team_posts_the_provider_id_and_remembers_it() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/olympus/v1/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(olympus_session_body()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/olympus/v1/session"))
            .and(header("x-requested-with", "olympus-ui"))
            .and(header("content-type", "application/json"))
            .and(body_json(json!({"provider": {"providerId": 222}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        let mut session = session_on(&endpoints, None);
        session.select_team(222).await.unwrap();
        assert_eq!(session.team_id(), Some(222));
        let restored = AppleSession::restore(&session.to_blob().unwrap(), endpoints).unwrap();
        assert_eq!(restored.team_id(), Some(222));
    }

    #[tokio::test]
    async fn select_team_refuses_an_unknown_team_without_posting() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/olympus/v1/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(olympus_session_body()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        let mut session = session_on(&endpoints, None);
        let err = session.select_team(999).await.unwrap_err();
        assert!(
            matches!(err, AppleSignInError::InvalidInput { message } if message.contains("999"))
        );
        assert_eq!(session.team_id(), None);
    }

    #[tokio::test]
    async fn select_team_maps_a_refusal() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/olympus/v1/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(olympus_session_body()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/olympus/v1/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "serviceErrors": [{"code": "x", "message": "invalid content provider id"}]
            })))
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        let mut session = session_on(&endpoints, None);
        let err = session.select_team(222).await.unwrap_err();
        assert!(
            matches!(&err, AppleSignInError::Refused { status: 200, message } if message.contains("invalid content provider id")),
            "{err}"
        );
        assert_eq!(session.team_id(), None);
    }

    #[tokio::test]
    async fn app_store_connect_get_sends_the_iris_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/iris/v1/apps"))
            .and(header("x-csrf-itc", "[asc-ui]"))
            .and(header("accept", "application/json"))
            .and(header("user-agent", transport::USER_AGENT))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [{"id": "1"}]})))
            .expect(1)
            .mount(&server)
            .await;
        let endpoints = AppleEndpoints::with_base_urls(&server.uri(), &server.uri());
        let session = session_on(&endpoints, None);
        let response = session
            .app_store_connect_get("iris/v1/apps?limit=1")
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert!(response.body.contains("\"data\""));

        for bad in [
            "https://evil.example/x",
            "//evil.example/x",
            "/iris/v1/apps",
        ] {
            assert!(matches!(
                session.app_store_connect_get(bad).await,
                Err(AppleSignInError::InvalidInput { .. })
            ));
        }
    }
}
