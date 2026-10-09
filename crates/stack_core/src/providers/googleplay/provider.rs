use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::client::GooglePlayClient;
use super::store_page::StorePageClient;
use super::PLAY_SCOPES;
use crate::auth::oauth_jwt::{ServiceAccountAuthenticator, ServiceAccountKey};
use crate::domain::{
    AppDetailsInfo, AppInfo, CustomerReview, CustomerReviewsPage, ReviewResponse, ReviewSubmission,
    StoreListingInfo, TrackInfo,
};
use crate::error::StackError;
use crate::ports::DebugLogger;
use crate::service::capabilities::app_details::{AppDetails, AppDetailsImpl};
use crate::service::capabilities::app_icons::{AppIcons, AppIconsImpl};
use crate::service::capabilities::reviews::{Reviews, ReviewsImpl};
use crate::service::capabilities::store_listings::{StoreListings, StoreListingsImpl};
use crate::service::capabilities::tracks::{Tracks, TracksImpl};
use crate::service::kind::ServiceKind;
use crate::service::provider::{Capability, ProviderImpl};

/// Bound on establishing a connection to Google.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on a whole request. The authenticator holds its token lock across the
/// token exchange, so an unbounded request would stall every caller.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Google Play implementation of the internal [`ProviderImpl`] contract:
/// [`Capability::Apps`] (Reporting API), plus [`Capability::Reviews`],
/// [`Capability::AppDetails`], [`Capability::StoreListings`] and
/// [`Capability::Tracks`] (Android Publisher API), and [`Capability::AppIcons`]
/// (the public store page, no credentials). Every other capability accessor
/// keeps the trait's `None` default.
///
/// The [`GooglePlayClient`] is held behind an `Arc` so the capability
/// sub-objects share the same client — and therefore the same authenticator,
/// token cache and edit lock — as `validate`/`fetch_apps`. The
/// [`StorePageClient`] shares only the HTTP client: it holds no credentials.
pub(crate) struct GooglePlayProvider {
    client: Arc<GooglePlayClient>,
    store_page: Arc<StorePageClient>,
}

impl GooglePlayProvider {
    /// Builds the provider from the three service-account fields.
    ///
    /// Unlike App Store Connect, the key is parsed eagerly — including one test
    /// signature (see [`ServiceAccountKey::parse`]) — so a malformed key fails
    /// at connect. One `reqwest::Client` with connect and request timeouts is
    /// shared by the authenticator, the API client and the store-page client.
    /// When `debug_logger` is `Some`, the token exchange, every API call and
    /// every store-page read are traced through it (see
    /// [`crate::ports::DebugLogger`]).
    ///
    /// # Errors
    /// [`StackError::InvalidCredentials`] if a field is blank or the key is not
    /// a usable RSA private key; [`StackError::Network`] if the HTTP client
    /// cannot be built.
    pub(crate) fn try_new(
        client_email: &str,
        private_key_id: &str,
        private_key: &str,
        debug_logger: Option<Arc<dyn DebugLogger>>,
    ) -> Result<Self, StackError> {
        let key = ServiceAccountKey::parse(client_email, private_key_id, private_key)?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| StackError::network(format!("HTTP client setup failed: {e}")))?;

        // `reqwest::Client` is an `Arc` internally: the clones share one pool.
        let auth = ServiceAccountAuthenticator::new(key, PLAY_SCOPES, http.clone());
        let store_page = StorePageClient::new(http.clone()).with_debug_logger(debug_logger.clone());
        Ok(Self {
            client: Arc::new(GooglePlayClient::new(auth, http).with_debug_logger(debug_logger)),
            store_page: Arc::new(store_page),
        })
    }
}

#[async_trait]
impl ProviderImpl for GooglePlayProvider {
    fn kind(&self) -> ServiceKind {
        ServiceKind::GooglePlay
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Apps,
            Capability::Reviews,
            Capability::AppDetails,
            Capability::StoreListings,
            Capability::Tracks,
            Capability::AppIcons,
        ]
    }

    async fn validate(&self) -> Result<(), StackError> {
        self.client.validate().await
    }

    async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        self.client.fetch_apps().await
    }

    fn reviews(&self) -> Option<Arc<Reviews>> {
        Some(Reviews::new(Box::new(GooglePlayReviews {
            client: Arc::clone(&self.client),
        })))
    }

    fn app_details(&self) -> Option<Arc<AppDetails>> {
        Some(AppDetails::new(Box::new(GooglePlayAppDetails {
            client: Arc::clone(&self.client),
        })))
    }

    fn store_listings(&self) -> Option<Arc<StoreListings>> {
        Some(StoreListings::new(Box::new(GooglePlayStoreListings {
            client: Arc::clone(&self.client),
        })))
    }

    fn tracks(&self) -> Option<Arc<Tracks>> {
        Some(Tracks::new(Box::new(GooglePlayTracks {
            client: Arc::clone(&self.client),
        })))
    }

    fn app_icons(&self) -> Option<Arc<AppIcons>> {
        Some(AppIcons::new(Box::new(GooglePlayAppIcons {
            store_page: Arc::clone(&self.store_page),
        })))
    }
}

/// Why [`ReviewsImpl::delete_review_response`] is unsupported on Google Play.
const NO_REPLY_DELETE: &str = "Google Play has no API to delete a review reply; \
     reply again to replace it";
/// Why the review-submission methods of [`ReviewsImpl`] are unsupported.
const NO_REVIEW_SUBMISSIONS: &str = "review submissions are an App Store Connect concept \
     with no Google Play equivalent";

/// Google Play implementation of the [`ReviewsImpl`] capability contract (see
/// `super::reviews`). `app_id` is the package name; review ids are opaque
/// composites. Holds the shared [`GooglePlayClient`] so it reuses the
/// provider's token cache.
struct GooglePlayReviews {
    client: Arc<GooglePlayClient>,
}

#[async_trait]
impl ReviewsImpl for GooglePlayReviews {
    async fn fetch_customer_reviews(
        &self,
        app_id: String,
    ) -> Result<Vec<CustomerReview>, StackError> {
        self.client.fetch_customer_reviews(&app_id).await
    }

    async fn fetch_customer_reviews_page(
        &self,
        app_id: String,
        sort: String,
        filter_rating: Vec<String>,
        limit: u32,
        page_token: Option<String>,
    ) -> Result<CustomerReviewsPage, StackError> {
        self.client
            .fetch_customer_reviews_page(
                &app_id,
                &sort,
                &filter_rating,
                limit,
                page_token.as_deref(),
            )
            .await
    }

    async fn fetch_review_submissions(
        &self,
        _app_id: String,
    ) -> Result<Vec<ReviewSubmission>, StackError> {
        Err(StackError::unsupported(NO_REVIEW_SUBMISSIONS))
    }

    async fn reply_to_review(
        &self,
        review_id: String,
        body: String,
    ) -> Result<ReviewResponse, StackError> {
        self.client.reply_to_review(&review_id, &body).await
    }

    async fn delete_review_response(&self, _response_id: String) -> Result<(), StackError> {
        Err(StackError::unsupported(NO_REPLY_DELETE))
    }

    async fn submit_review_submission(&self, _submission_id: String) -> Result<(), StackError> {
        Err(StackError::unsupported(NO_REVIEW_SUBMISSIONS))
    }

    async fn discard_review_submission(&self, _submission_id: String) -> Result<(), StackError> {
        Err(StackError::unsupported(NO_REVIEW_SUBMISSIONS))
    }
}

/// Google Play implementation of the [`AppDetailsImpl`] capability contract
/// (`edits.details` in a throwaway edit). `app_id` is the package name.
struct GooglePlayAppDetails {
    client: Arc<GooglePlayClient>,
}

#[async_trait]
impl AppDetailsImpl for GooglePlayAppDetails {
    async fn fetch_app_details(&self, app_id: String) -> Result<AppDetailsInfo, StackError> {
        self.client.fetch_app_details(&app_id).await
    }
}

/// Google Play implementation of the [`StoreListingsImpl`] capability contract
/// (`edits.listings` in a throwaway edit). `app_id` is the package name.
struct GooglePlayStoreListings {
    client: Arc<GooglePlayClient>,
}

#[async_trait]
impl StoreListingsImpl for GooglePlayStoreListings {
    async fn fetch_store_listings(
        &self,
        app_id: String,
    ) -> Result<Vec<StoreListingInfo>, StackError> {
        self.client.fetch_store_listings(&app_id).await
    }
}

/// Google Play implementation of the [`TracksImpl`] capability contract
/// (`edits.tracks` in a throwaway edit). `app_id` is the package name.
struct GooglePlayTracks {
    client: Arc<GooglePlayClient>,
}

#[async_trait]
impl TracksImpl for GooglePlayTracks {
    async fn fetch_tracks(&self, app_id: String) -> Result<Vec<TrackInfo>, StackError> {
        self.client.fetch_tracks(&app_id).await
    }
}

/// Google Play implementation of the [`AppIconsImpl`] capability contract: the
/// `og:image` of the app's public store page (see `super::store_page`), never
/// an edit and never a token. `app_id` is the package name.
struct GooglePlayAppIcons {
    store_page: Arc<StorePageClient>,
}

#[async_trait]
impl AppIconsImpl for GooglePlayAppIcons {
    async fn fetch_icon_url(&self, app_id: String) -> Result<Option<String>, StackError> {
        self.store_page.fetch_icon_url(&app_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::googleplay::test_support::{
        client, mount_token, store_page_client, CapturingLogger,
    };
    use crate::service::provider::Provider;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PKCS8_PEM: &str = include_str!("../../../tests/fixtures/test_rsa_private_pkcs8.pem");
    const EMAIL: &str = "play-bot@my-project.iam.gserviceaccount.com";
    const KID: &str = "0123456789abcdef0123456789abcdef01234567";

    fn provider() -> GooglePlayProvider {
        // `unwrap` (not `unwrap_err`) is fine: the error type is `Debug`.
        GooglePlayProvider::try_new(EMAIL, KID, PKCS8_PEM, None).unwrap()
    }

    const PLAY_CAPABILITIES: [Capability; 6] = [
        Capability::Apps,
        Capability::Reviews,
        Capability::AppDetails,
        Capability::StoreListings,
        Capability::Tracks,
        Capability::AppIcons,
    ];

    #[test]
    fn reports_kind_and_capabilities() {
        let p = provider();
        assert_eq!(p.kind(), ServiceKind::GooglePlay);
        assert_eq!(p.capabilities(), PLAY_CAPABILITIES.to_vec());
    }

    #[test]
    fn exposes_exactly_the_play_capability_handles() {
        let p = provider();
        assert!(p.reviews().is_some());
        assert!(p.app_details().is_some());
        assert!(p.store_listings().is_some());
        assert!(p.tracks().is_some());
        assert!(p.app_icons().is_some());
        // App Store Connect-only capabilities keep the trait's `None` default.
        assert!(p.app_store_versions().is_none());
        assert!(p.builds().is_none());
        assert!(p.beta_groups().is_none());
        assert!(p.beta_build_localizations().is_none());
        assert!(p.beta_app_localizations().is_none());
        assert!(p.beta_app_review_detail().is_none());
        assert!(p.app_metadata().is_none());
        assert!(p.accessibility_declarations().is_none());
        assert!(p.users().is_none());
        assert!(p.devices().is_none());
        assert!(p.bundle_ids().is_none());
        assert!(p.certificates().is_none());
        assert!(p.profiles().is_none());
        assert!(p.analytics().is_none());
    }

    #[test]
    fn provider_wrapper_exposes_the_same_handles() {
        // The same through the exported `Provider` the hosts hold.
        let p = Provider::new(Box::new(provider()));
        assert_eq!(p.kind(), ServiceKind::GooglePlay);
        assert_eq!(p.capabilities(), PLAY_CAPABILITIES.to_vec());
        assert!(p.reviews().is_some());
        assert!(p.app_details().is_some());
        assert!(p.store_listings().is_some());
        assert!(p.tracks().is_some());
        assert!(p.app_icons().is_some());
        assert!(p.app_store_versions().is_none());
        assert!(p.builds().is_none());
        assert!(p.beta_groups().is_none());
        assert!(p.beta_build_localizations().is_none());
        assert!(p.beta_app_localizations().is_none());
        assert!(p.beta_app_review_detail().is_none());
        assert!(p.app_metadata().is_none());
        assert!(p.accessibility_declarations().is_none());
        assert!(p.users().is_none());
        assert!(p.devices().is_none());
        assert!(p.bundle_ids().is_none());
        assert!(p.certificates().is_none());
        assert!(p.profiles().is_none());
        assert!(p.analytics().is_none());
    }

    #[test]
    fn every_advertised_capability_has_a_handle() {
        // Apps is served by `fetch_apps`, not a handle.
        let p = provider();
        for capability in p.capabilities() {
            let has_handle = match capability {
                Capability::Apps => true,
                Capability::Reviews => p.reviews().is_some(),
                Capability::AppDetails => p.app_details().is_some(),
                Capability::StoreListings => p.store_listings().is_some(),
                Capability::Tracks => p.tracks().is_some(),
                Capability::AppIcons => p.app_icons().is_some(),
                other => panic!("Google Play should not advertise {other:?}"),
            };
            assert!(has_handle, "{capability:?} has no handle");
        }
    }

    fn play_reviews(client: GooglePlayClient) -> Arc<Reviews> {
        Reviews::new(Box::new(GooglePlayReviews {
            client: Arc::new(client),
        }))
    }

    #[tokio::test]
    async fn unsupported_review_methods_explain_why() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 0).await;
        let reviews = play_reviews(client(&server));

        let results = [
            reviews
                .fetch_review_submissions("com.example.app".into())
                .await
                .map(|_| ()),
            reviews.submit_review_submission("sub-1".into()).await,
            reviews.discard_review_submission("sub-1".into()).await,
        ];
        for result in results {
            match result {
                Err(StackError::Unsupported { message }) => {
                    assert_eq!(message, NO_REVIEW_SUBMISSIONS)
                }
                other => panic!("expected Unsupported, got {other:?}"),
            }
        }
        match reviews
            .delete_review_response("com.example.app/gp:1".into())
            .await
        {
            Err(StackError::Unsupported { message }) => {
                assert!(message.contains("no API to delete a review reply"))
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
        // None of them touched Google.
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reviews_handle_delegates_page_arguments_in_order() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("GET"))
            .and(path(
                "/androidpublisher/v3/applications/com.example.app/reviews",
            ))
            .and(query_param("maxResults", "7"))
            .and(query_param("token", "T2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reviews": [
                    { "reviewId": "gp:5", "comments": [{ "userComment": { "starRating": 5 } }] },
                    { "reviewId": "gp:3", "comments": [{ "userComment": { "starRating": 3 } }] }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let page = play_reviews(client(&server))
            .fetch_customer_reviews_page(
                "com.example.app".into(),
                "-createdDate".into(),
                vec!["3".into()],
                7,
                Some("T2".into()),
            )
            .await
            .unwrap();
        let ids: Vec<&str> = page.reviews.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["com.example.app/gp:3"]);
    }

    #[tokio::test]
    async fn reviews_handle_delegates_reply() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        Mock::given(method("POST"))
            .and(path(
                "/androidpublisher/v3/applications/com.example.app/reviews/gp:1:reply",
            ))
            .and(body_json(serde_json::json!({ "replyText": "Thanks" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": { "replyText": "Thanks" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = play_reviews(client(&server))
            .reply_to_review("com.example.app/gp:1".into(), "Thanks".into())
            .await
            .unwrap();
        assert_eq!(response.id, "com.example.app/gp:1");
        assert_eq!(response.body.as_deref(), Some("Thanks"));
    }

    #[tokio::test]
    async fn edit_handles_delegate_to_their_resource() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        let edits = "/androidpublisher/v3/applications/com.example.app/edits";
        Mock::given(method("POST"))
            .and(path(edits))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": "E" })),
            )
            .expect(3)
            .mount(&server)
            .await;
        for (resource, body) in [
            ("details", serde_json::json!({ "defaultLanguage": "en-US" })),
            (
                "listings",
                serde_json::json!({ "listings": [{ "language": "en-US" }] }),
            ),
            (
                "tracks",
                serde_json::json!({ "tracks": [{ "track": "beta" }] }),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("{edits}/E/{resource}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("DELETE"))
            .and(path(format!("{edits}/E")))
            .respond_with(ResponseTemplate::new(204))
            .expect(3)
            .mount(&server)
            .await;

        let client = Arc::new(client(&server));
        let details = AppDetails::new(Box::new(GooglePlayAppDetails {
            client: Arc::clone(&client),
        }))
        .fetch_app_details("com.example.app".into())
        .await
        .unwrap();
        assert_eq!(details.app_id, "com.example.app");
        assert_eq!(details.default_language.as_deref(), Some("en-US"));

        let listings = StoreListings::new(Box::new(GooglePlayStoreListings {
            client: Arc::clone(&client),
        }))
        .fetch_store_listings("com.example.app".into())
        .await
        .unwrap();
        assert_eq!(listings[0].language, "en-US");

        let tracks = Tracks::new(Box::new(GooglePlayTracks {
            client: Arc::clone(&client),
        }))
        .fetch_tracks("com.example.app".into())
        .await
        .unwrap();
        assert_eq!(tracks[0].track, "beta");
    }

    #[tokio::test]
    async fn app_icons_handle_reads_the_store_page_without_a_token() {
        let server = MockServer::start().await;
        // Neither a token exchange nor an Android Publisher call (e.g. an edit)
        // may happen: unmatched paths would 404 and `expect(0)` fails on drop.
        mount_token(&server, "tok-1", 0).await;
        Mock::given(method("GET"))
            .and(path("/store/apps/details"))
            .and(query_param("id", "com.example.app"))
            .and(query_param("hl", "en"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<meta content="https://play-lh.googleusercontent.com/Icon=s0-br30" property="og:image">"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        let provider = GooglePlayProvider {
            client: Arc::new(client(&server)),
            store_page: Arc::new(store_page_client(&server)),
        };
        let icons = provider.app_icons().expect("Google Play exposes AppIcons");
        let icon = icons
            .fetch_icon_url("com.example.app".into())
            .await
            .unwrap();
        assert_eq!(
            icon.as_deref(),
            Some("https://play-lh.googleusercontent.com/Icon=s512")
        );

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "only the store page may be requested");
        assert_eq!(requests[0].url.path(), "/store/apps/details");
        assert!(requests[0].headers.get("authorization").is_none());
    }

    #[tokio::test]
    async fn app_icons_unpublished_app_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/store/apps/details"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let icons = AppIcons::new(Box::new(GooglePlayAppIcons {
            store_page: Arc::new(store_page_client(&server)),
        }));
        assert_eq!(
            icons
                .fetch_icon_url("com.example.unpublished".into())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn try_new_wires_the_debug_logger_into_the_store_page_client() {
        // Offline: an invalid package name is answered locally, and the trace
        // proves the logger reached the store-page client.
        let logger = Arc::new(CapturingLogger::default());
        let p = GooglePlayProvider::try_new(EMAIL, KID, PKCS8_PEM, Some(logger.clone())).unwrap();
        let icon = p
            .app_icons()
            .unwrap()
            .fetch_icon_url("not a package".into())
            .await
            .unwrap();
        assert_eq!(icon, None);
        let messages = logger.messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("is not a valid package name"));
    }

    #[test]
    fn try_new_rejects_malformed_key() {
        // `GooglePlayProvider` is not `Debug`, so no `unwrap_err`.
        let result = GooglePlayProvider::try_new(EMAIL, KID, "not a key", None);
        assert!(matches!(result, Err(StackError::InvalidCredentials { .. })));
    }

    #[test]
    fn try_new_rejects_invalid_identity_fields() {
        assert!(matches!(
            GooglePlayProvider::try_new("not-an-email", KID, PKCS8_PEM, None),
            Err(StackError::InvalidCredentials { .. })
        ));
        assert!(matches!(
            GooglePlayProvider::try_new(EMAIL, "  ", PKCS8_PEM, None),
            Err(StackError::InvalidCredentials { .. })
        ));
    }

    #[test]
    fn http_timeouts_are_bounded() {
        // Literals, so a changed bound fails here.
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(10));
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(30));
    }
}
