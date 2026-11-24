//! How the App Store Connect client authorizes its requests: with an API key
//! (an ES256 team JWT on the public API) or with an Apple ID web session (the
//! session cookies on iris, App Store Connect's web API, and on the Developer
//! Portal's `services-account`).
//!
//! The two modes reach the same JSON:API resources under different hosts:
//!
//! | Family | API key | Web session |
//! |---|---|---|
//! | App Store Connect resources | `api.appstoreconnect.apple.com/v1` | `appstoreconnect.apple.com/iris/v1` |
//! | Provisioning (devices, bundle ids, capabilities, certificates, profiles) | `api.appstoreconnect.apple.com/v1` | `developer.apple.com/services-account/v1`, spaceship's proxy form |
//! | Renaming or deleting a bundle id | `api.appstoreconnect.apple.com/v1` | [`StackError::Unsupported`] |
//! | Analytics Reports | `api.appstoreconnect.apple.com/v1` | [`StackError::Unsupported`] |
//!
//! Provisioning with a session follows spaceship's
//! `connect_api/provisioning/client.rb` (see
//! [`crate::auth::apple_session::portal`]): reads and deletes are `POST`s with
//! `X-HTTP-Method-Override` and `{urlEncodedQueryParams, teamId}` as the body,
//! creates carry `data.attributes.teamId`, updates are real `PATCH`es. The
//! `teamId` is the *Developer Portal* team, not the App Store Connect provider
//! id the account is built with: it is resolved once per team from
//! `listTeams.action` (matched by name, see
//! [`crate::auth::apple_session::portal::match_portal_team`]) and kept in the
//! session blob. spaceship has no session form for renaming or deleting a
//! bundle id, so those stay API-key only. Analytics Reports do not exist on
//! iris (`404 PATH_ERROR` in the spike).
//!
//! A web session request to iris carries, on top of the cookies the session's
//! jar adds below request building:
//! - `x-csrf-itc: [asc-ui]`, as App Store Connect's web client (and
//!   spaceship's `ConnectAPI` clients) send to iris;
//! - the `csrf` / `csrf_ts` tokens of the latest iris response that carried
//!   them (spaceship's `store_csrf_tokens`, merged into every request).
//!
//! A `services-account` request carries the latest `csrf` / `csrf_ts` of that
//! host instead (each host's tokens are kept apart, as spaceship keeps them per
//! client) and no `x-csrf-itc` (spaceship's provisioning client sets none).
//!
//! Before its first iris request the session is switched to the account's
//! team (`POST olympus/v1/session`) unless the saved session already acts for
//! it. After every response, refreshed cookies are written back through the
//! host's [`SessionStore`], and an HTTP 401 becomes
//! [`StackError::SessionExpired`].

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use reqwest::header::{HeaderMap, CONTENT_TYPE, SET_COOKIE};
use reqwest::{RequestBuilder, StatusCode, Url};
use serde_json::Value;
use tokio::sync::OnceCell;

use crate::auth::apple_session::portal::{self, ProxiedMethod};
use crate::auth::apple_session::session::AppleSession;
use crate::auth::es256::AppStoreAuthenticator;
use crate::error::StackError;
use crate::logging::{self, target};
use crate::ports::SessionStore;

/// The public App Store Connect API (API-key mode, every resource family).
pub(crate) const API_KEY_BASE_URL: &str = "https://api.appstoreconnect.apple.com";

/// The header App Store Connect's web client sends to iris.
pub(crate) const CSRF_ITC_HEADER: &str = "x-csrf-itc";
/// Its fixed value: names the web client, not a secret.
pub(crate) const CSRF_ITC_VALUE: &str = "[asc-ui]";
/// The CSRF token headers a session response may carry, echoed back on the
/// following requests.
const CSRF_TOKEN_HEADERS: [&str; 2] = ["csrf", "csrf_ts"];
/// A CSRF token longer than this is not believed (and not echoed).
const MAX_CSRF_TOKEN_LEN: usize = 1024;

/// A resource family a web session cannot reach (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiKeyOnly {
    /// Renaming or deleting a bundle id: spaceship has no session form for
    /// either (`connect_api/provisioning/provisioning.rb`).
    BundleIdRenameOrDelete,
    /// Analytics report requests, reports, instances and segments.
    AnalyticsReports,
}

impl ApiKeyOnly {
    fn message(self) -> &'static str {
        match self {
            Self::BundleIdRenameOrDelete => {
                "renaming or deleting a bundle id is not available for an account signed in \
                 with an Apple ID (the Developer Portal's web session has no such call); \
                 connect the account with an App Store Connect API key to use it"
            }
            Self::AnalyticsReports => {
                "Analytics Reports are not available for an account signed in with an Apple \
                 ID (App Store Connect's web API has no analytics reports); connect the \
                 account with an App Store Connect API key to use them"
            }
        }
    }
}

/// How the client authorizes.
pub(crate) enum AppStoreAuth {
    /// An App Store Connect API key (`.p8`).
    ApiKey(AppStoreAuthenticator),
    /// An Apple ID web session (boxed: it is much larger than a key).
    WebSession(Box<SessionAuth>),
}

impl AppStoreAuth {
    /// What to attach to the next request: a fresh (cached) bearer token, or
    /// the session headers after making sure the session acts for the
    /// account's team.
    ///
    /// # Errors
    /// API key: as [`AppStoreAuthenticator::bearer_token`]. Session: as
    /// [`SessionAuth::credential`].
    pub(crate) async fn credential(&self) -> Result<RequestAuth, StackError> {
        match self {
            Self::ApiKey(auth) => auth.bearer_token().await.map(RequestAuth::Bearer),
            Self::WebSession(session) => session.credential().await,
        }
    }

    /// `Ok` unless this is a web session and `family` needs an API key.
    ///
    /// # Errors
    /// [`StackError::Unsupported`] for a web session.
    pub(crate) fn require_api_key(&self, family: ApiKeyOnly) -> Result<(), StackError> {
        match self {
            Self::ApiKey(_) => Ok(()),
            Self::WebSession(_) => Err(StackError::unsupported(family.message())),
        }
    }

    /// Whether this is a web session.
    pub(crate) fn is_web_session(&self) -> bool {
        matches!(self, Self::WebSession(_))
    }
}

/// The CSRF tokens of a session response, in the order they arrived.
#[derive(Clone, PartialEq, Eq, Default)]
pub(crate) struct CsrfTokens(Vec<(&'static str, String)>);

impl fmt::Debug for CsrfTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.0.iter().map(|(name, _)| *name).collect();
        write!(f, "CsrfTokens({names:?}, values redacted)")
    }
}

impl CsrfTokens {
    /// The tokens in `headers`, `None` when it carries none.
    fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let tokens: Vec<(&'static str, String)> = CSRF_TOKEN_HEADERS
            .iter()
            .filter_map(|name| {
                let value = headers.get(*name)?.to_str().ok()?.trim();
                (!value.is_empty() && value.len() <= MAX_CSRF_TOKEN_LEN)
                    .then(|| (*name, value.to_string()))
            })
            .collect();
        (!tokens.is_empty()).then_some(Self(tokens))
    }
}

/// What one request is authorized with.
pub(crate) enum RequestAuth {
    /// `Authorization: Bearer …`.
    Bearer(String),
    /// The web session headers (the cookies come from the session's jar).
    Session {
        /// The latest CSRF tokens, if App Store Connect sent any yet.
        csrf: Option<CsrfTokens>,
    },
}

impl fmt::Debug for RequestAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("Bearer([redacted])"),
            Self::Session { csrf } => f.debug_struct("Session").field("csrf", csrf).finish(),
        }
    }
}

/// Attaches a [`RequestAuth`] to a request.
pub(crate) trait Authorize {
    /// The request with `auth`'s headers.
    #[must_use]
    fn authorize(self, auth: &RequestAuth) -> Self;
}

impl Authorize for RequestBuilder {
    fn authorize(self, auth: &RequestAuth) -> Self {
        match auth {
            RequestAuth::Bearer(token) => self.bearer_auth(token),
            RequestAuth::Session { csrf } => {
                let builder = self.header(CSRF_ITC_HEADER, CSRF_ITC_VALUE);
                csrf.iter()
                    .flat_map(|tokens| tokens.0.iter())
                    .fold(builder, |b, (name, value)| b.header(*name, value))
            }
        }
    }
}

/// Which of the session's hosts a response came from: each keeps its own
/// CSRF tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionHost {
    /// App Store Connect (iris, olympus).
    AppStoreConnect,
    /// The Developer Portal (`developer.apple.com/services-account`).
    DeveloperPortal,
}

/// A provisioning request in spaceship's web session form (see the module
/// docs and [`crate::auth::apple_session::portal`]).
#[derive(Debug, Clone, Copy)]
pub(crate) enum PortalCall<'a> {
    /// A read: `proxy_get`.
    Get,
    /// A delete: `proxy_delete`.
    Delete,
    /// A create (`proxy_post`) with this JSON:API body.
    Create(&'a Value),
    /// An update with this JSON:API body: a real `PATCH` (`APIClient#patch`).
    Update(&'a Value),
}

/// The Apple ID web session of one App Store Connect account (one team).
pub(crate) struct SessionAuth {
    account_id: String,
    team_id: u64,
    session: tokio::sync::Mutex<AppleSession>,
    http: reqwest::Client,
    iris_base_url: String,
    /// `{developer}/services-account`.
    services_base_url: String,
    team_ready: OnceCell<()>,
    /// The Developer Portal team, resolved once (see [`Self::portal_team_id`]).
    portal_team: OnceCell<String>,
    csrf: Mutex<Option<CsrfTokens>>,
    portal_csrf: Mutex<Option<CsrfTokens>>,
    store: Arc<dyn SessionStore>,
}

impl fmt::Debug for SessionAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionAuth")
            .field("team_id", &self.team_id)
            .finish_non_exhaustive()
    }
}

impl SessionAuth {
    /// The session `account_id` acts with for `team_id` (an App Store Connect
    /// provider id); refreshed cookies are saved to `store` under
    /// `account_id`.
    pub(crate) fn new(
        account_id: String,
        team_id: u64,
        session: AppleSession,
        store: Arc<dyn SessionStore>,
    ) -> Self {
        let http = session.http().clone();
        let iris_base_url = session.iris_base_url();
        let services_base_url = session.services_account_base_url();
        Self {
            account_id,
            team_id,
            session: tokio::sync::Mutex::new(session),
            http,
            iris_base_url,
            services_base_url,
            team_ready: OnceCell::new(),
            portal_team: OnceCell::new(),
            csrf: Mutex::new(None),
            portal_csrf: Mutex::new(None),
            store,
        }
    }

    /// The same session with `services-account` at `base_url` (tests: a mock
    /// server, so the provisioning paths are `/v1/...` like the API key's).
    #[cfg(test)]
    pub(crate) fn with_services_base_url(mut self, base_url: &str) -> Self {
        self.services_base_url = base_url.trim_end_matches('/').to_string();
        self
    }

    /// `{developer}/services-account`: where the provisioning requests go.
    pub(crate) fn services_base_url(&self) -> &str {
        &self.services_base_url
    }

    /// The Developer Portal team of the account's App Store Connect team:
    /// the one the saved session resolved before, else resolved now from
    /// `listTeams.action` (see [`portal::match_portal_team`]) and saved with
    /// the session. Resolved at most once per client; a failure is retried
    /// on the next call.
    ///
    /// # Errors
    /// [`StackError::SessionExpired`] when App Store Connect or the Developer
    /// Portal refuses the session; [`StackError::Auth`] when the team is not
    /// one of the Apple ID's teams or the portal refuses to list them;
    /// [`StackError::Unsupported`] when no portal team, or no unambiguous
    /// one, belongs to the account's team; [`StackError::Network`] /
    /// [`StackError::Http`] for transport and server failures.
    pub(crate) async fn portal_team_id(&self) -> Result<&str, StackError> {
        self.portal_team
            .get_or_try_init(|| self.resolve_portal_team())
            .await
            .map(String::as_str)
    }

    async fn resolve_portal_team(&self) -> Result<String, StackError> {
        let mut session = self.session.lock().await;
        if let Some(saved) = session.portal_team_id(self.team_id) {
            return Ok(saved.to_string());
        }
        let olympus = session.olympus_session().await?;
        let asc_team = olympus
            .teams
            .iter()
            .find(|t| t.provider_id == self.team_id)
            .ok_or_else(|| {
                StackError::auth(format!(
                    "team {} is not one of this Apple ID's {} team(s)",
                    self.team_id,
                    olympus.teams.len()
                ))
            })?;
        let csrf = self.csrf_of(SessionHost::DeveloperPortal);
        let reply = session
            .send_list_teams(csrf.as_ref().map_or(&[], |t| t.0.as_slice()))
            .await?;
        self.remember_csrf(SessionHost::DeveloperPortal, &reply.headers);
        let portal_teams = portal::parse_list_teams(&reply)?;
        let portal_team = portal::match_portal_team(asc_team, olympus.teams.len(), &portal_teams)?;
        session.remember_portal_team(self.team_id, portal_team.clone());
        drop(session);
        logging::info(target::AUTH, || {
            format!(
                "Apple ID session: resolved the Developer Portal team of team {} among {} portal team(s)",
                self.team_id,
                portal_teams.len()
            )
        });
        // The binding (and any refreshed cookie) goes into the saved session.
        self.persist().await;
        Ok(portal_team)
    }

    /// The request for a provisioning call `call` on `url` (a `.../v1/...`
    /// URL: a path the client built or a pagination link), in spaceship's
    /// session form, with the Developer Portal team and that host's CSRF
    /// tokens. The request always goes to [`Self::services_base_url`]: only
    /// the `/v1/...` path and the query of `url` are kept, so a pagination
    /// link naming another host never receives the session's cookies.
    ///
    /// # Errors
    /// [`StackError::Decode`] for a URL without a `/v1/` path; the errors of
    /// [`Self::portal_team_id`].
    pub(crate) async fn portal_request(
        &self,
        call: PortalCall<'_>,
        url: &str,
    ) -> Result<RequestBuilder, StackError> {
        let (target, query) = self.portal_target(url)?;
        let team_id = self.portal_team_id().await?;
        let builder = match call {
            PortalCall::Get => self.proxied(
                &target,
                ProxiedMethod::Get,
                Some(query.unwrap_or_default()),
                team_id,
            ),
            PortalCall::Delete => self.proxied(&target, ProxiedMethod::Delete, query, team_id),
            PortalCall::Create(body) => self
                .http
                .post(&target)
                .header(CONTENT_TYPE, portal::PROXY_CONTENT_TYPE)
                .header("X-Requested-With", portal::PROXY_REQUESTED_WITH)
                .body(portal::with_team_id(body, team_id).to_string()),
            PortalCall::Update(body) => self
                .http
                .patch(&target)
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string()),
        };
        let builder = portal::net_http_defaults()
            .into_iter()
            .fold(builder, |b, (name, value)| b.header(name, value));
        let csrf = self.csrf_of(SessionHost::DeveloperPortal);
        Ok(csrf
            .iter()
            .flat_map(|tokens| tokens.0.iter())
            .fold(builder, |b, (name, value)| b.header(*name, value)))
    }

    /// `POST` with `X-HTTP-Method-Override` and the proxied body.
    fn proxied(
        &self,
        target: &str,
        method: ProxiedMethod,
        query: Option<String>,
        team_id: &str,
    ) -> RequestBuilder {
        self.http
            .post(target)
            .header(CONTENT_TYPE, portal::PROXY_CONTENT_TYPE)
            .header(portal::METHOD_OVERRIDE_HEADER, method.override_value())
            .header("X-Requested-With", portal::PROXY_REQUESTED_WITH)
            .body(portal::proxied_body(query.as_deref(), team_id).to_string())
    }

    /// `{services_base_url}/v1/...` and the raw query of `url`.
    fn portal_target(&self, url: &str) -> Result<(String, Option<String>), StackError> {
        let parsed = Url::parse(url)
            .map_err(|_| StackError::decode("a provisioning URL is not a valid URL"))?;
        let path = parsed.path();
        let suffix = path
            .find("/v1/")
            .and_then(|at| path.get(at..))
            .ok_or_else(|| {
                StackError::decode(format!(
                    "a provisioning URL has no /v1/ path ({} characters)",
                    path.len()
                ))
            })?;
        Ok((
            format!("{}{suffix}", self.services_base_url),
            parsed.query().map(str::to_string),
        ))
    }

    /// The session's HTTP client (its cookie jar).
    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// `{app_store_connect}/iris`.
    pub(crate) fn iris_base_url(&self) -> &str {
        &self.iris_base_url
    }

    /// The session headers for the next request, after the one-time team
    /// check.
    ///
    /// # Errors
    /// [`StackError::SessionExpired`] when App Store Connect refuses the
    /// session while switching the team; [`StackError::Auth`] when the team is
    /// not one of the Apple ID's teams or the switch is refused;
    /// [`StackError::Network`] / [`StackError::Http`] for transport and
    /// server failures. A failed check is retried on the next request.
    pub(crate) async fn credential(&self) -> Result<RequestAuth, StackError> {
        self.team_ready
            .get_or_try_init(|| self.ensure_team())
            .await?;
        let csrf = self.csrf_of(SessionHost::AppStoreConnect);
        Ok(RequestAuth::Session { csrf })
    }

    fn csrf_slot(&self, host: SessionHost) -> &Mutex<Option<CsrfTokens>> {
        match host {
            SessionHost::AppStoreConnect => &self.csrf,
            SessionHost::DeveloperPortal => &self.portal_csrf,
        }
    }

    /// The latest CSRF tokens `host` sent.
    fn csrf_of(&self, host: SessionHost) -> Option<CsrfTokens> {
        self.csrf_slot(host)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Keeps the CSRF tokens of a `host` response, if it carries any.
    fn remember_csrf(&self, host: SessionHost, headers: &HeaderMap) {
        if let Some(tokens) = CsrfTokens::from_headers(headers) {
            *self
                .csrf_slot(host)
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(tokens);
        }
    }

    async fn ensure_team(&self) -> Result<(), StackError> {
        let mut session = self.session.lock().await;
        if session.team_id() == Some(self.team_id) {
            return Ok(());
        }
        session
            .select_team(self.team_id)
            .await
            .map_err(StackError::from)?;
        drop(session);
        // The switch sets a new team cookie; keep the saved session in step.
        self.persist().await;
        Ok(())
    }

    /// Reads what a response tells the session: new CSRF tokens, refreshed
    /// cookies (saved through the host's store) and an expired session.
    ///
    /// # Errors
    /// [`StackError::SessionExpired`] on HTTP 401. A 403 stays the caller's
    /// to map: iris answers 403 for a role that lacks a permission, which is
    /// not an expired session.
    pub(crate) async fn observe(
        &self,
        host: SessionHost,
        status: StatusCode,
        headers: &HeaderMap,
    ) -> Result<(), StackError> {
        self.remember_csrf(host, headers);
        if headers.contains_key(SET_COOKIE) {
            self.persist().await;
        }
        if status == StatusCode::UNAUTHORIZED {
            let service = match host {
                SessionHost::AppStoreConnect => "App Store Connect",
                SessionHost::DeveloperPortal => "the Apple Developer Portal",
            };
            logging::warn(target::AUTH, || {
                format!(
                    "Apple ID session: {service} answered HTTP 401; the user must sign in again"
                )
            });
            return Err(StackError::session_expired(format!(
                "{service} no longer accepts this Apple ID session (HTTP 401); \
                 sign in with the Apple ID again"
            )));
        }
        Ok(())
    }

    /// Writes the session (its cookies as the jar holds them now) back to the
    /// host's store. A blob that cannot be serialized is logged and skipped:
    /// the previous one stays usable.
    async fn persist(&self) {
        let blob = self.session.lock().await.to_blob();
        match blob {
            Ok(blob) => {
                self.store
                    .save(self.account_id.clone(), blob.as_bytes().to_vec());
                logging::debug(target::AUTH, || {
                    "Apple ID session: saved refreshed cookies".to_string()
                });
            }
            Err(err) => logging::warn(target::AUTH, || {
                format!("Apple ID session: could not save refreshed cookies: {err}")
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn csrf_tokens_are_read_bounded_and_never_debug_printed() {
        let mut headers = HeaderMap::new();
        headers.insert("csrf", HeaderValue::from_static(" tok-1 "));
        headers.insert("csrf_ts", HeaderValue::from_static("1763550000"));
        headers.insert("other", HeaderValue::from_static("x"));
        let tokens = CsrfTokens::from_headers(&headers).unwrap();
        assert_eq!(
            tokens.0,
            vec![
                ("csrf", "tok-1".to_string()),
                ("csrf_ts", "1763550000".into())
            ]
        );
        let debug = format!("{tokens:?}");
        assert!(!debug.contains("tok-1") && debug.contains("csrf_ts"));

        assert!(CsrfTokens::from_headers(&HeaderMap::new()).is_none());
        let mut huge = HeaderMap::new();
        huge.insert(
            "csrf",
            HeaderValue::from_str(&"x".repeat(MAX_CSRF_TOKEN_LEN + 1)).unwrap(),
        );
        assert!(CsrfTokens::from_headers(&huge).is_none());
    }

    #[test]
    fn request_auth_debug_hides_the_bearer_token() {
        assert_eq!(
            format!("{:?}", RequestAuth::Bearer("secret-jwt".into())),
            "Bearer([redacted])"
        );
    }

    #[test]
    fn authorize_adds_the_mode_headers() {
        let client = reqwest::Client::new();
        let bearer = client
            .get("http://localhost/v1/apps")
            .authorize(&RequestAuth::Bearer("jwt".into()))
            .build()
            .unwrap();
        assert_eq!(bearer.headers()["authorization"], "Bearer jwt");
        assert!(bearer.headers().get(CSRF_ITC_HEADER).is_none());

        let session = client
            .get("http://localhost/iris/v1/apps")
            .authorize(&RequestAuth::Session {
                csrf: Some(CsrfTokens(vec![
                    ("csrf", "c".into()),
                    ("csrf_ts", "t".into()),
                ])),
            })
            .build()
            .unwrap();
        assert!(session.headers().get("authorization").is_none());
        assert_eq!(session.headers()[CSRF_ITC_HEADER], "[asc-ui]");
        assert_eq!(session.headers()["csrf"], "c");
        assert_eq!(session.headers()["csrf_ts"], "t");

        let first = client
            .get("http://localhost/iris/v1/apps")
            .authorize(&RequestAuth::Session { csrf: None })
            .build()
            .unwrap();
        assert_eq!(first.headers().len(), 1);
    }

    #[test]
    fn unsupported_families_name_the_api_key_path() {
        for family in [
            ApiKeyOnly::BundleIdRenameOrDelete,
            ApiKeyOnly::AnalyticsReports,
        ] {
            assert!(family.message().contains("API key"));
        }
    }
}
