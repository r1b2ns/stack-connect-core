//! The Apple ID sign-in state machine.
//!
//! ```text
//!            start(email, password)
//!   Ready ─────────────────────────────┬──► SignedIn ──► into_session()
//!     ▲                                ├──► NeedsAcknowledgement (terminal)
//!     │ SignInExpired                  └──► AwaitingCode
//!     │                                        │  request_sms(phone)  (stays)
//!     └────────────────────────────────────────┤  submit_code: WrongCode (stays)
//!                                              └─ submit_code: ok ──► SignedIn
//! ```
//!
//! Wire protocol (fastlane master, `client.rb` / `two_step_or_factor_client.rb`):
//!
//! 1. the widget key ([`super::widget_key`]);
//! 2. `POST idmsa/appleauth/auth/signin/init` with `a` (SRP `A`), the account
//!    name and the offered protocols; Apple answers the salt, `b`, `c`, the
//!    iteration count and the protocol it picked;
//! 3. the SRP proof ([`super::srp`]) and the hashcash
//!    (`GET idmsa/appleauth/auth/signin?widgetKey=…`, [`super::hashcash`]);
//! 4. `POST idmsa/appleauth/auth/signin/complete?isRememberMeEnabled=false`
//!    with `m1`, `m2`, `c`: 200 signed in, 409 two-factor, 412 acknowledgement;
//! 5. two-factor: `GET idmsa/appleauth/auth` (methods and phones),
//!    `PUT verify/phone` (send an SMS), `POST verify/{trusteddevice|phone}/securitycode`,
//!    then `GET 2sv/trust`, all with the `scnt` and `X-Apple-Id-Session-Id`
//!    the 409 carried;
//! 6. `GET appstoreconnect/olympus/v1/session` for the teams.

use std::fmt;
use std::sync::Arc;
use std::time::SystemTime;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};

use super::error::AppleSignInError;
use super::hashcash::{self, HashcashChallenge, HASHCASH_HEADER};
use super::session::{AppleSession, AppleTeam, SessionJar};
use super::srp::{derive_password_key, Password, PasswordProtocol, SrpClient, SrpProof};
use super::transport::{
    self, common_status_error, describe_errors, service_errors, AppleReply, ServiceError,
};
use super::{widget_key, AppleEndpoints};
use crate::logging::{self, target};

/// `AUTH_TYPES` in `client.rb`: a 412 with one of these is an
/// acknowledgement / two-factor upgrade prompt.
const AUTH_TYPES: [&str; 4] = ["sa", "hsa", "non-sa", "hsa2"];

/// Apple's "Incorrect verification code" error.
const WRONG_CODE_ERROR: &str = "-21669";

/// At most this many trusted phones are read.
const MAX_PHONES: usize = 20;

/// Codes shorter or longer than this are not believed.
const CODE_LENGTH_RANGE: std::ops::RangeInclusive<u64> = 4..=12;

/// The code length spaceship assumes when Apple does not say (6, hsa2).
const DEFAULT_CODE_LENGTH: u8 = 6;

/// What the caller does next.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum AppleSignInStep {
    /// Apple wants a two-factor code.
    NeedsCode {
        /// The ways a code can be submitted with [`AppleSignIn::submit_code`].
        methods: Vec<AppleCodeMethod>,
        /// The trusted phone numbers (masked by Apple).
        phones: Vec<AppleTrustedPhone>,
        /// How many digits the code has.
        code_length: u8,
        /// Where a code was already sent, if anywhere.
        delivery: AppleCodeDelivery,
    },
    /// The Apple ID must accept Apple's terms or upgrade its two-factor
    /// setup in a browser first (HTTP 412); this sign-in cannot continue.
    NeedsAcknowledgement,
    /// Signed in; [`AppleSignIn::into_session`] hands over the session.
    Done {
        /// Every team the Apple ID belongs to.
        teams: Vec<AppleTeam>,
    },
}

/// How a two-factor code is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum AppleCodeMethod {
    /// A code shown on a trusted Apple device (pushed automatically).
    TrustedDevice,
    /// A code sent by text message (or call) to a trusted phone.
    Phone {
        /// [`AppleTrustedPhone::id`].
        phone_id: u64,
    },
}

/// Where Apple already sent a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum AppleCodeDelivery {
    /// To the trusted devices.
    TrustedDevices,
    /// By text message to this phone (automatically when the Apple ID has
    /// no trusted device and one phone, or after [`AppleSignIn::request_sms`]).
    Phone {
        /// [`AppleTrustedPhone::id`].
        phone_id: u64,
    },
    /// Nowhere yet: pick a phone with [`AppleSignIn::request_sms`].
    NotSent,
}

/// A trusted phone number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, uniffi::Record)]
pub struct AppleTrustedPhone {
    /// Apple's id for the number.
    pub id: u64,
    /// The number as Apple masks it, e.g. `+49 •••• •••••85`.
    pub masked_number: String,
    /// `sms` or `voice` (`pushMode`).
    pub mode: String,
}

/// The two headers every two-factor request carries.
#[derive(Clone)]
struct TwoFactorHeaders {
    session_id: String,
    scnt: String,
}

/// A sign-in waiting for a code.
#[derive(Clone)]
struct PendingCode {
    headers: TwoFactorHeaders,
    phones: Vec<AppleTrustedPhone>,
    code_length: u8,
    delivery: AppleCodeDelivery,
    trusted_devices: bool,
}

impl PendingCode {
    fn methods(&self) -> Vec<AppleCodeMethod> {
        let devices = self
            .trusted_devices
            .then_some(AppleCodeMethod::TrustedDevice);
        devices
            .into_iter()
            .chain(
                self.phones
                    .iter()
                    .map(|p| AppleCodeMethod::Phone { phone_id: p.id }),
            )
            .collect()
    }

    fn step(&self) -> AppleSignInStep {
        AppleSignInStep::NeedsCode {
            methods: self.methods(),
            phones: self.phones.clone(),
            code_length: self.code_length,
            delivery: self.delivery,
        }
    }

    fn phone(&self, phone_id: u64) -> Result<&AppleTrustedPhone, AppleSignInError> {
        self.phones
            .iter()
            .find(|p| p.id == phone_id)
            .ok_or_else(|| {
                AppleSignInError::invalid_input(format!(
                    "phone {phone_id} is not one of the {} trusted phone(s)",
                    self.phones.len()
                ))
            })
    }
}

enum State {
    Ready,
    AwaitingCode(PendingCode),
    NeedsAcknowledgement,
    SignedIn {
        teams: Vec<AppleTeam>,
        current_team_id: Option<u64>,
    },
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            Self::Ready => "not started",
            Self::AwaitingCode(_) => "waiting for a two-factor code",
            Self::NeedsAcknowledgement => "stopped: the Apple ID needs an acknowledgement",
            Self::SignedIn { .. } => "signed in",
        }
    }
}

/// What `signin/init` answered.
struct InitChallenge {
    iterations: u32,
    salt: Vec<u8>,
    server_public: Vec<u8>,
    c: String,
    protocol: PasswordProtocol,
}

/// What `signin/complete` means.
enum CompleteOutcome {
    SignedIn,
    TwoFactor(TwoFactorHeaders),
    Acknowledge,
}

/// One Apple ID sign-in (see the module docs for the steps).
pub struct AppleSignIn {
    endpoints: AppleEndpoints,
    jar: Arc<SessionJar>,
    http: reqwest::Client,
    /// A key a saved session signed in with: the fallback when the sign-out
    /// redirect yields none.
    known_widget_key: Option<String>,
    widget_key: Option<String>,
    clock: fn() -> SystemTime,
    state: State,
    /// Tests fix the SRP private ephemeral to compare with the gem's vectors.
    #[cfg(test)]
    fixed_srp_private: Option<Vec<u8>>,
}

impl fmt::Debug for AppleSignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppleSignIn")
            .field("endpoints", &self.endpoints)
            .field("state", &self.state.name())
            .finish_non_exhaustive()
    }
}

impl AppleSignIn {
    /// A fresh sign-in (empty cookie jar).
    ///
    /// # Errors
    /// [`AppleSignInError::Network`] when the HTTP client cannot be built.
    pub fn new(endpoints: AppleEndpoints) -> Result<Self, AppleSignInError> {
        let jar = Arc::new(SessionJar::default());
        let http = transport::session_client(Arc::clone(&jar))?;
        Ok(Self::from_parts(endpoints, jar, http, None))
    }

    /// Signs in again over a saved session: its cookies (a `DES…` trust
    /// cookie lets Apple skip the two-factor step) and its widget key as the
    /// fallback key.
    pub fn from_session(session: AppleSession) -> Self {
        let (endpoints, jar, http, widget_key) = session.into_parts();
        Self::from_parts(endpoints, jar, http, widget_key)
    }

    fn from_parts(
        endpoints: AppleEndpoints,
        jar: Arc<SessionJar>,
        http: reqwest::Client,
        known_widget_key: Option<String>,
    ) -> Self {
        Self {
            endpoints,
            jar,
            http,
            known_widget_key,
            widget_key: None,
            clock: SystemTime::now,
            state: State::Ready,
            #[cfg(test)]
            fixed_srp_private: None,
        }
    }

    /// Starts the sign-in: widget key, SRP exchange, and either the end of
    /// the sign-in or the two-factor step.
    ///
    /// The password is consumed and wiped once the SRP key is derived; it
    /// never goes on the wire.
    ///
    /// # Errors
    /// [`AppleSignInError::WrongStep`] unless the sign-in is not started;
    /// [`AppleSignInError::InvalidInput`] for an empty email or password;
    /// [`AppleSignInError::WrongCredentials`]; [`AppleSignInError::SecurityKeyRequired`]
    /// and [`AppleSignInError::TwoStepUnsupported`] for unsupported second
    /// factors; [`AppleSignInError::Refused`] (e.g. a locked Apple ID);
    /// [`AppleSignInError::NotEnabledForAppStoreConnect`]; the transport and
    /// status errors. On any error the sign-in can be started again.
    pub async fn start(
        &mut self,
        email: &str,
        password: Password,
    ) -> Result<AppleSignInStep, AppleSignInError> {
        if !matches!(self.state, State::Ready) {
            return Err(self.wrong_step("start"));
        }
        let email = email.trim();
        if email.is_empty() {
            return Err(AppleSignInError::invalid_input("the Apple ID is empty"));
        }
        if password.is_empty() {
            return Err(AppleSignInError::invalid_input("the password is empty"));
        }

        let key = widget_key::resolve(
            &self.http,
            &self.endpoints,
            self.known_widget_key.as_deref(),
        )
        .await?;
        self.widget_key = Some(key.clone());

        let srp = self.srp_client()?;
        let challenge = self.signin_init(&key, email, srp.public_key()).await?;
        let proof = {
            let derived = derive_password_key(
                &password,
                &challenge.salt,
                challenge.iterations,
                challenge.protocol,
            )?;
            drop(password);
            srp.process_challenge(email, &derived, &challenge.salt, &challenge.server_public)?
        };
        let hashcash = self.fetch_hashcash(&key).await?;
        let reply = self
            .signin_complete(&key, email, &challenge.c, &proof, hashcash.as_deref())
            .await?;

        match classify_complete(&reply)? {
            CompleteOutcome::SignedIn => {
                logging::info(target::AUTH, || {
                    "Apple ID sign-in: signed in without a code".to_string()
                });
                self.finish_sign_in().await
            }
            CompleteOutcome::Acknowledge => {
                logging::warn(target::AUTH, || {
                    "Apple ID sign-in: Apple asks for an acknowledgement (HTTP 412)".to_string()
                });
                self.state = State::NeedsAcknowledgement;
                Ok(AppleSignInStep::NeedsAcknowledgement)
            }
            CompleteOutcome::TwoFactor(headers) => {
                let pending = self.fetch_auth_options(&key, headers).await?;
                logging::info(target::AUTH, || {
                    format!(
                        "Apple ID sign-in: two-factor code required ({} trusted phone(s), delivery {:?})",
                        pending.phones.len(),
                        pending.delivery
                    )
                });
                let step = pending.step();
                self.state = State::AwaitingCode(pending);
                Ok(step)
            }
        }
    }

    /// Asks Apple to text (or call, per the phone's mode) a code to
    /// `phone_id` (`PUT verify/phone`).
    ///
    /// # Errors
    /// [`AppleSignInError::WrongStep`] unless a code is pending;
    /// [`AppleSignInError::InvalidInput`] for an unknown phone (nothing
    /// sent); [`AppleSignInError::TwoFactorUnavailable`] when Apple cannot
    /// send it; [`AppleSignInError::SignInExpired`] (the sign-in is reset).
    pub async fn request_sms(
        &mut self,
        phone_id: u64,
    ) -> Result<AppleSignInStep, AppleSignInError> {
        let State::AwaitingCode(pending) = &self.state else {
            return Err(self.wrong_step("request a text message"));
        };
        let phone = pending.phone(phone_id)?.clone();
        let headers = pending.headers.clone();
        let key = self.current_widget_key()?;

        let reply = transport::send(
            self.two_factor(
                self.http
                    .put(self.endpoints.idmsa("appleauth/auth/verify/phone")),
                &key,
                &headers,
            )
            .header("Content-Type", "application/json")
            .body(json!({ "phoneNumber": { "id": phone.id }, "mode": phone.mode }).to_string()),
        )
        .await?;
        let body = reply.json().unwrap_or(Value::Null);
        let errors = service_errors(&body);
        if reply.code() == 401 {
            self.state = State::Ready;
            return Err(AppleSignInError::SignInExpired);
        }
        if let Some(err) = common_status_error(reply.status) {
            return Err(err);
        }
        if !errors.is_empty() {
            return Err(AppleSignInError::TwoFactorUnavailable {
                message: describe_errors(&errors, ""),
            });
        }
        if !reply.status.is_success() {
            return Err(AppleSignInError::unexpected(
                reply.code(),
                "Apple did not send the text message",
            ));
        }
        logging::info(target::AUTH, || {
            format!(
                "Apple ID sign-in: code requested for phone {phone_id} ({})",
                phone.mode
            )
        });
        let State::AwaitingCode(pending) = &mut self.state else {
            return Err(self.wrong_step("request a text message"));
        };
        pending.delivery = AppleCodeDelivery::Phone { phone_id };
        Ok(pending.step())
    }

    /// Submits the two-factor code, then trusts this client
    /// (`GET 2sv/trust`) and reads the teams.
    ///
    /// # Errors
    /// [`AppleSignInError::WrongStep`] unless a code is pending;
    /// [`AppleSignInError::InvalidInput`] for a code that is not
    /// `code_length` digits or an unknown phone (nothing sent);
    /// [`AppleSignInError::WrongCode`] (the sign-in still waits for a code);
    /// [`AppleSignInError::SignInExpired`] (the sign-in is reset);
    /// [`AppleSignInError::TwoFactorUnavailable`] / [`AppleSignInError::Refused`];
    /// the errors of the olympus session.
    pub async fn submit_code(
        &mut self,
        code: &str,
        method: AppleCodeMethod,
    ) -> Result<AppleSignInStep, AppleSignInError> {
        let State::AwaitingCode(pending) = &self.state else {
            return Err(self.wrong_step("submit a code"));
        };
        let code = code.trim();
        if code.len() != usize::from(pending.code_length)
            || !code.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(AppleSignInError::invalid_input(format!(
                "the code must be {} digits",
                pending.code_length
            )));
        }
        let (path, body) = match method {
            AppleCodeMethod::TrustedDevice => {
                if !pending.trusted_devices {
                    return Err(AppleSignInError::invalid_input(
                        "this Apple ID has no trusted device to show a code",
                    ));
                }
                (
                    "appleauth/auth/verify/trusteddevice/securitycode",
                    json!({ "securityCode": { "code": code } }),
                )
            }
            AppleCodeMethod::Phone { phone_id } => {
                let phone = pending.phone(phone_id)?;
                (
                    "appleauth/auth/verify/phone/securitycode",
                    json!({
                        "securityCode": { "code": code },
                        "phoneNumber": { "id": phone.id },
                        "mode": phone.mode,
                    }),
                )
            }
        };
        let headers = pending.headers.clone();
        let key = self.current_widget_key()?;

        let reply = transport::send(
            self.two_factor(self.http.post(self.endpoints.idmsa(path)), &key, &headers)
                .header("Content-Type", "application/json")
                .body(body.to_string()),
        )
        .await?;
        match classify_code_reply(&reply) {
            Ok(CodeOutcome::Accepted) => {}
            Ok(CodeOutcome::Acknowledge) => {
                self.state = State::NeedsAcknowledgement;
                return Ok(AppleSignInStep::NeedsAcknowledgement);
            }
            Err(AppleSignInError::SignInExpired) => {
                self.state = State::Ready;
                return Err(AppleSignInError::SignInExpired);
            }
            Err(err) => {
                logging::warn(target::AUTH, || {
                    format!("Apple ID sign-in: code refused: {err}")
                });
                return Err(err);
            }
        }
        logging::info(target::AUTH, || {
            "Apple ID sign-in: code accepted".to_string()
        });

        self.trust(&key, &headers).await?;
        self.finish_sign_in().await
    }

    /// The teams, once signed in.
    pub fn teams(&self) -> Option<&[AppleTeam]> {
        match &self.state {
            State::SignedIn { teams, .. } => Some(teams),
            _ => None,
        }
    }

    /// Hands over the signed-in session (acting for the team olympus reports
    /// as current).
    ///
    /// # Errors
    /// [`AppleSignInError::WrongStep`] unless signed in.
    pub fn into_session(self) -> Result<AppleSession, AppleSignInError> {
        let State::SignedIn {
            current_team_id, ..
        } = self.state
        else {
            return Err(self.wrong_step("hand over the session"));
        };
        Ok(AppleSession::from_parts(
            self.endpoints,
            self.jar,
            self.http,
            self.widget_key,
            current_team_id,
        ))
    }

    fn wrong_step(&self, action: &str) -> AppleSignInError {
        AppleSignInError::wrong_step(format!(
            "cannot {action}: the sign-in is {}",
            self.state.name()
        ))
    }

    fn current_widget_key(&self) -> Result<String, AppleSignInError> {
        self.widget_key
            .clone()
            .ok_or(AppleSignInError::WidgetKeyUnavailable)
    }

    fn srp_client(&self) -> Result<SrpClient, AppleSignInError> {
        #[cfg(test)]
        if let Some(a) = &self.fixed_srp_private {
            return Ok(SrpClient::from_private(a));
        }
        SrpClient::new()
    }

    /// The headers of `signin/init` and `signin/complete` (`do_sirp`).
    fn sign_in_request(
        &self,
        builder: reqwest::RequestBuilder,
        key: &str,
    ) -> reqwest::RequestBuilder {
        builder
            .header("Content-Type", "application/json")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("X-Apple-Widget-Key", key)
            .header("Accept", "application/json, text/javascript")
    }

    /// The headers of every two-factor request (`update_request_headers`).
    fn two_factor(
        &self,
        builder: reqwest::RequestBuilder,
        key: &str,
        headers: &TwoFactorHeaders,
    ) -> reqwest::RequestBuilder {
        builder
            .header("X-Apple-Id-Session-Id", &headers.session_id)
            .header("X-Apple-Widget-Key", key)
            .header("Accept", "application/json")
            .header("scnt", &headers.scnt)
    }

    async fn signin_init(
        &self,
        key: &str,
        email: &str,
        public: &[u8],
    ) -> Result<InitChallenge, AppleSignInError> {
        let body = json!({
            "a": STANDARD.encode(public),
            "accountName": email,
            "protocols": PasswordProtocol::OFFERED,
        });
        let reply = transport::send(
            self.sign_in_request(
                self.http
                    .post(self.endpoints.idmsa("appleauth/auth/signin/init")),
                key,
            )
            .body(body.to_string()),
        )
        .await?;
        parse_init(&reply)
    }

    /// `fetch_hashcash`: `None` when Apple sends no challenge.
    async fn fetch_hashcash(&self, key: &str) -> Result<Option<String>, AppleSignInError> {
        let url = format!(
            "{}?widgetKey={}",
            self.endpoints.idmsa("appleauth/auth/signin"),
            percent_encoding::utf8_percent_encode(key, percent_encoding::NON_ALPHANUMERIC)
        );
        let reply = transport::send(self.http.get(url)).await?;
        match HashcashChallenge::from_headers(&reply.headers)? {
            Some(challenge) => Ok(Some(challenge.solve(&hashcash::date_stamp((self.clock)())))),
            None => {
                logging::warn(target::AUTH, || {
                    format!(
                        "Apple ID sign-in: no hashcash challenge (HTTP {}); completing without X-Apple-HC",
                        reply.code()
                    )
                });
                Ok(None)
            }
        }
    }

    async fn signin_complete(
        &self,
        key: &str,
        email: &str,
        c: &str,
        proof: &SrpProof,
        hashcash: Option<&str>,
    ) -> Result<AppleReply, AppleSignInError> {
        let body = json!({
            "accountName": email,
            "c": c,
            "m1": STANDARD.encode(&proof.m1),
            "m2": STANDARD.encode(&proof.m2),
            "rememberMe": false,
        });
        let mut builder = self
            .sign_in_request(
                self.http.post(
                    self.endpoints
                        .idmsa("appleauth/auth/signin/complete?isRememberMeEnabled=false"),
                ),
                key,
            )
            .body(body.to_string());
        if let Some(hashcash) = hashcash {
            builder = builder.header(HASHCASH_HEADER, hashcash);
        }
        transport::send(builder).await
    }

    /// `GET appleauth/auth`: which second factor, and the phones.
    async fn fetch_auth_options(
        &self,
        key: &str,
        headers: TwoFactorHeaders,
    ) -> Result<PendingCode, AppleSignInError> {
        let reply = transport::send(self.two_factor(
            self.http.get(self.endpoints.idmsa("appleauth/auth")),
            key,
            &headers,
        ))
        .await?;
        parse_auth_options(&reply, headers)
    }

    /// `store_session`: `GET 2sv/trust` refreshes `myacinfo` and sets the
    /// `DES…` trust cookie. Its status does not matter (spaceship: "we don't
    /// really care"); a transport failure does.
    async fn trust(&self, key: &str, headers: &TwoFactorHeaders) -> Result<(), AppleSignInError> {
        let reply = transport::send(
            self.two_factor(
                self.http
                    .get(self.endpoints.idmsa("appleauth/auth/2sv/trust")),
                key,
                headers,
            ),
        )
        .await?;
        if !reply.status.is_success() {
            logging::warn(target::AUTH, || {
                format!(
                    "Apple ID sign-in: 2sv/trust answered HTTP {}; continuing",
                    reply.code()
                )
            });
        }
        Ok(())
    }

    /// `fetch_olympus_session_after_signin`.
    async fn finish_sign_in(&mut self) -> Result<AppleSignInStep, AppleSignInError> {
        let session = AppleSession::from_parts(
            self.endpoints.clone(),
            Arc::clone(&self.jar),
            self.http.clone(),
            self.widget_key.clone(),
            None,
        );
        let olympus = match session.olympus_session().await {
            Ok(olympus) => olympus,
            Err(err) => {
                if matches!(err, AppleSignInError::SessionRejected { .. })
                    && self.widget_key.is_some()
                    && self.widget_key == self.known_widget_key
                {
                    logging::warn(target::AUTH, || {
                        "Apple ID sign-in: App Store Connect refused the session; the fallback widget key may be stale"
                            .to_string()
                    });
                }
                self.state = State::Ready;
                return Err(err);
            }
        };
        let teams = olympus.teams.clone();
        self.state = State::SignedIn {
            teams: olympus.teams,
            current_team_id: olympus.current_team_id,
        };
        Ok(AppleSignInStep::Done { teams })
    }

    /// Tests fix the SRP private ephemeral.
    #[cfg(test)]
    pub(crate) fn with_srp_private(mut self, a: Vec<u8>) -> Self {
        self.fixed_srp_private = Some(a);
        self
    }

    /// Tests fix the clock (the hashcash date).
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: fn() -> SystemTime) -> Self {
        self.clock = clock;
        self
    }
}

/// `signin/init`'s answer.
fn parse_init(reply: &AppleReply) -> Result<InitChallenge, AppleSignInError> {
    let status = reply.code();
    let body = reply.json();
    let errors = body.as_ref().map(service_errors).unwrap_or_default();
    if status == 401 {
        return Err(AppleSignInError::WrongCredentials);
    }
    if status == 403 {
        return Err(refused(status, &errors, "Apple refused the sign-in"));
    }
    if let Some(err) = common_status_error(reply.status) {
        return Err(err);
    }
    let Some(body) = body else {
        return Err(AppleSignInError::unexpected(
            status,
            "signin/init did not answer JSON",
        ));
    };
    if !errors.is_empty() {
        return Err(refused(status, &errors, ""));
    }
    if !reply.status.is_success() {
        return Err(AppleSignInError::unexpected(status, "signin/init failed"));
    }

    let missing = |field: &str| {
        AppleSignInError::unexpected(status, format!("signin/init has no valid {field:?}"))
    };
    let iterations = body
        .get("iteration")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| missing("iteration"))?;
    let decode = |field: &str| {
        body.get(field)
            .and_then(Value::as_str)
            .and_then(|text| STANDARD.decode(text).ok())
            .ok_or_else(|| missing(field))
    };
    let salt = decode("salt")?;
    let server_public = decode("b")?;
    let c = body
        .get("c")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| missing("c"))?
        .to_string();
    let protocol = body
        .get("protocol")
        .and_then(Value::as_str)
        .ok_or_else(|| missing("protocol"))?;
    Ok(InitChallenge {
        iterations,
        salt,
        server_public,
        c,
        protocol: PasswordProtocol::from_apple(protocol)?,
    })
}

/// `send_shared_login_request`'s handling of `signin/complete` (with
/// `handle_error`'s status checks first, as spaceship's `send_request` runs
/// them before the caller sees the response).
fn classify_complete(reply: &AppleReply) -> Result<CompleteOutcome, AppleSignInError> {
    let status = reply.code();
    let body = reply.json();
    let errors = body.as_ref().map(service_errors).unwrap_or_default();
    match status {
        401 => return Err(AppleSignInError::WrongCredentials),
        403 => return Err(refused(status, &errors, "Apple refused the sign-in")),
        _ => {}
    }
    if let Some(err) = common_status_error(reply.status) {
        return Err(err);
    }
    match status {
        200 => return Ok(CompleteOutcome::SignedIn),
        409 => {
            let (Some(session_id), Some(scnt)) =
                (reply.header("x-apple-id-session-id"), reply.header("scnt"))
            else {
                return Err(AppleSignInError::unexpected(
                    409,
                    "two-factor is required but Apple sent no session headers",
                ));
            };
            return Ok(CompleteOutcome::TwoFactor(TwoFactorHeaders {
                session_id: session_id.to_string(),
                scnt: scnt.to_string(),
            }));
        }
        _ => {}
    }
    if reply.body.contains("invalid=\"true\"") {
        return Err(AppleSignInError::WrongCredentials);
    }
    let auth_type = body
        .as_ref()
        .and_then(|b| b.get("authType"))
        .and_then(Value::as_str);
    if status == 412 && auth_type.is_some_and(|t| AUTH_TYPES.contains(&t)) {
        return Ok(CompleteOutcome::Acknowledge);
    }
    if reply.sets_cookie_named("itctx") {
        return Err(AppleSignInError::NotEnabledForAppStoreConnect);
    }
    Err(AppleSignInError::unexpected(
        status,
        describe_errors(&errors, "signin/complete did not sign in"),
    ))
}

/// `handle_two_step_or_factor` + `handle_two_factor`'s reading of
/// `GET appleauth/auth`.
fn parse_auth_options(
    reply: &AppleReply,
    headers: TwoFactorHeaders,
) -> Result<PendingCode, AppleSignInError> {
    let status = reply.code();
    if status == 401 {
        return Err(AppleSignInError::SignInExpired);
    }
    if let Some(err) = common_status_error(reply.status) {
        return Err(err);
    }
    let body = reply.json().unwrap_or(Value::Null);
    let errors = service_errors(&body);
    if status == 403 {
        return Err(refused(
            status,
            &errors,
            "Apple refused the two-factor step",
        ));
    }
    if !reply.status.is_success() {
        return Err(AppleSignInError::unexpected(
            status,
            "appleauth/auth failed",
        ));
    }

    // A security key (FIDO) account: Apple sends a WebAuthn challenge
    // (`fsaChallenge`) and the key names. Not in spaceship, which does not
    // support security keys either; detected so the user gets a clear answer.
    let has_keys = body
        .get("keyNames")
        .and_then(Value::as_array)
        .is_some_and(|names| !names.is_empty());
    if body.get("fsaChallenge").is_some_and(Value::is_object) || has_keys {
        return Err(AppleSignInError::SecurityKeyRequired);
    }
    if body.get("trustedDevices").is_some_and(Value::is_array) {
        return Err(AppleSignInError::TwoStepUnsupported);
    }
    let phones_value = body.get("trustedPhoneNumbers").and_then(Value::as_array);
    let Some(phones_value) = phones_value.filter(|p| p.first().is_some_and(Value::is_object))
    else {
        let mut message = describe_errors(&errors, "");
        if message.is_empty() {
            return Err(AppleSignInError::unexpected(
                status,
                "Apple asked for two-factor authentication without a way to verify",
            ));
        }
        if body.get("noTrustedDevices").and_then(Value::as_bool) == Some(true) {
            message.push_str(
                ". This Apple ID has no trusted devices: signing in to it on an Apple device adds one",
            );
        }
        return Err(AppleSignInError::TwoFactorUnavailable { message });
    };
    if security_code_locked(&body) {
        return Err(AppleSignInError::TwoFactorUnavailable {
            message: "too many codes were requested or entered; try again later".to_string(),
        });
    }

    let phones: Vec<AppleTrustedPhone> = phones_value
        .iter()
        .take(MAX_PHONES)
        .filter_map(parse_phone)
        .collect();
    if phones.is_empty() {
        return Err(AppleSignInError::unexpected(
            status,
            "Apple listed trusted phones without ids",
        ));
    }
    let code_length = match body.get("securityCode").and_then(|c| c.get("length")) {
        None => DEFAULT_CODE_LENGTH,
        Some(length) => length
            .as_u64()
            .filter(|n| CODE_LENGTH_RANGE.contains(n))
            .and_then(|n| u8::try_from(n).ok())
            .ok_or_else(|| {
                AppleSignInError::unexpected(status, "the code length is out of range")
            })?,
    };

    // `sms_fallback` / `sms_automatically_sent`.
    let no_trusted_devices = body.get("noTrustedDevices").and_then(Value::as_bool) == Some(true);
    let delivery = match (no_trusted_devices, phones.as_slice()) {
        (true, [only]) => AppleCodeDelivery::Phone { phone_id: only.id },
        (true, _) => AppleCodeDelivery::NotSent,
        (false, _) => AppleCodeDelivery::TrustedDevices,
    };
    Ok(PendingCode {
        headers,
        phones,
        code_length,
        delivery,
        trusted_devices: !no_trusted_devices,
    })
}

fn parse_phone(value: &Value) -> Option<AppleTrustedPhone> {
    let id = match value.get("id")? {
        Value::Number(n) => n.as_u64()?,
        Value::String(s) => s.trim().parse().ok()?,
        _ => return None,
    };
    let masked_number = value
        .get("numberWithDialCode")
        .or_else(|| value.get("obfuscatedNumber"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mode = value
        .get("pushMode")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty() && m.bytes().all(|b| b.is_ascii_alphanumeric()))
        .unwrap_or("sms")
        .to_string();
    Some(AppleTrustedPhone {
        id,
        masked_number,
        mode,
    })
}

fn security_code_locked(body: &Value) -> bool {
    let flag = |v: Option<&Value>| v.and_then(Value::as_bool) == Some(true);
    let code = body.get("securityCode");
    flag(body.get("securityCodeLocked"))
        || flag(code.and_then(|c| c.get("securityCodeLocked")))
        || flag(code.and_then(|c| c.get("tooManyCodesLock")))
}

/// What a code submission means.
enum CodeOutcome {
    Accepted,
    Acknowledge,
}

/// `handle_itc_response` on the code answer, plus the status checks.
fn classify_code_reply(reply: &AppleReply) -> Result<CodeOutcome, AppleSignInError> {
    let status = reply.code();
    let body = reply.json().unwrap_or(Value::Null);
    let errors = service_errors(&body);
    if is_wrong_code(&errors) {
        return Err(AppleSignInError::WrongCode);
    }
    if status == 401 {
        return Err(AppleSignInError::SignInExpired);
    }
    if let Some(err) = common_status_error(reply.status) {
        return Err(err);
    }
    if security_code_locked(&body) {
        return Err(AppleSignInError::TwoFactorUnavailable {
            message: "too many incorrect codes were entered; try again later".to_string(),
        });
    }
    if status == 412 {
        return Ok(CodeOutcome::Acknowledge);
    }
    if !errors.is_empty() || !reply.status.is_success() {
        return Err(refused(status, &errors, "Apple did not accept the code"));
    }
    Ok(CodeOutcome::Accepted)
}

/// Apple's "Incorrect verification code" (-21669). spaceship matches the
/// message text ("verification code"); the code is matched first here, the
/// message as a fallback.
fn is_wrong_code(errors: &[ServiceError]) -> bool {
    errors.iter().any(|e| {
        e.code == WRONG_CODE_ERROR
            || e.message
                .to_ascii_lowercase()
                .contains("incorrect verification code")
    })
}

fn refused(status: u16, errors: &[ServiceError], fallback: &str) -> AppleSignInError {
    AppleSignInError::Refused {
        status,
        message: describe_errors(errors, fallback),
    }
}

#[cfg(test)]
mod tests;
