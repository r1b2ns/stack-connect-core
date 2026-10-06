//! Google Play customer reviews (Android Publisher `reviews` resource), mapped
//! onto the store-agnostic [`CustomerReview`] records so hosts reuse their
//! review UI.
//!
//! Google's limits, which no client can work around (see
//! <https://developers.google.com/android-publisher/reply-to-reviews>):
//!
//! - only reviews users **created or modified within the last week** are
//!   returned (older ones are only in the Play Console CSV export);
//! - only reviews **with a comment** (star-only ratings are left out), and only
//!   for **production** versions of the app;
//! - no server-side sort or filter; quotas of 200 `GET`s per hour and 2000
//!   replies per day.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::json;

use super::client::{is_valid_package_name, GooglePlayClient};
use super::error::{
    ErrorHints, GoogleApi, Target, PERMISSION_REPLY_TO_REVIEWS, PERMISSION_VIEW_APP_INFO,
};
use super::non_blank;
use super::timestamp::Timestamp;
use crate::domain::{CustomerReview, CustomerReviewsPage, ReviewResponse};
use crate::error::StackError;

/// The largest `maxResults` Google documents for `reviews.list` (default 10).
const MAX_PAGE_SIZE: u32 = 100;
/// Hard cap on `reviews.list` pages for [`GooglePlayClient::fetch_customer_reviews`]
/// (10 000 reviews), so a server that never stops paginating cannot loop
/// forever. A week of commented reviews rarely needs more than a few pages.
const MAX_PAGES: usize = 100;
/// Separates the package name from Google's review id in the composite
/// [`CustomerReview::id`]. Package names cannot contain it, so the first one
/// always ends the package name.
const REVIEW_ID_SEPARATOR: char = '/';
/// The `sort` values that select Google's own order (most recent first); every
/// other App Store Connect sort value is rejected.
const NATIVE_SORTS: [&str; 2] = ["", "-createdDate"];

/// One `reviews.list` page. Proto3 JSON omits empty fields: an app without
/// recent reviews answers `{}`, and the last page has no `tokenPagination`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewsListResponse {
    #[serde(default)]
    reviews: Vec<PlayReview>,
    #[serde(default)]
    token_pagination: Option<TokenPagination>,
}

impl ReviewsListResponse {
    /// The next page's token; `None` when absent or blank (last page).
    fn next_page_token(&self) -> Option<String> {
        non_blank(self.token_pagination.as_ref()?.next_page_token.clone())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenPagination {
    #[serde(default)]
    next_page_token: Option<String>,
}

/// A `Review` resource: its user comment(s) and any developer reply live in
/// `comments`, each entry carrying one or the other.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayReview {
    #[serde(default)]
    review_id: Option<String>,
    #[serde(default)]
    author_name: Option<String>,
    #[serde(default)]
    comments: Vec<PlayComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayComment {
    #[serde(default)]
    user_comment: Option<UserComment>,
    #[serde(default)]
    developer_comment: Option<DeveloperComment>,
}

/// The fields of Play's `UserComment` this slice maps (app version, device and
/// language are a follow-up: `CustomerReview` has no place for them yet).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserComment {
    /// `title\tbody` when the review has a title, else just the body.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    last_modified: Option<Timestamp>,
    #[serde(default)]
    star_rating: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeveloperComment {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    last_modified: Option<Timestamp>,
}

/// `reviews.reply` answer.
#[derive(Deserialize)]
struct ReplyResponse {
    #[serde(default)]
    result: Option<ReplyResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplyResult {
    #[serde(default)]
    reply_text: Option<String>,
    #[serde(default)]
    last_edited: Option<Timestamp>,
}

/// The two halves of a Play [`CustomerReview::id`]: `{package_name}/{review_id}`.
///
/// Google's reply endpoint needs the package name, but the store-agnostic
/// `reply_to_review(review_id, body)` only receives the review id, so the id
/// handed to hosts carries both. Hosts must treat it as opaque.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReviewKey<'a> {
    package_name: &'a str,
    review_id: &'a str,
}

impl<'a> ReviewKey<'a> {
    /// The composite id for `review_id` of `package_name`.
    fn compose(package_name: &str, review_id: &str) -> String {
        format!("{package_name}{REVIEW_ID_SEPARATOR}{review_id}")
    }

    /// Splits a composite id at its first separator.
    ///
    /// # Errors
    /// [`StackError::Decode`] when `id` is not a composite this module made:
    /// no separator, an invalid package name, or a blank, `.` or `..` review
    /// id. Nothing is sent to Google.
    fn parse(id: &'a str) -> Result<Self, StackError> {
        let invalid = || {
            StackError::decode(format!(
                "invalid Google Play review id {id:?}: pass the id returned by \
                 fetch_customer_reviews unchanged (<packageName>/<reviewId>)"
            ))
        };
        let (package_name, review_id) = id.split_once(REVIEW_ID_SEPARATOR).ok_or_else(invalid)?;
        if !is_valid_package_name(package_name) || matches!(review_id.trim(), "" | "." | "..") {
            return Err(invalid());
        }
        Ok(Self {
            package_name,
            review_id,
        })
    }
}

/// Splits Play's `text` into `(title, body)`: a review with a title is sent as
/// `title\tbody` (the first tab separates them), one without is just the body.
/// Blank halves are `None`; the text is otherwise passed through untouched.
fn split_title_body(text: &str) -> (Option<String>, Option<String>) {
    let owned = |s: &str| non_blank(Some(s.to_string()));
    match text.split_once('\t') {
        Some((title, body)) => (owned(title), owned(body)),
        None => (None, owned(text)),
    }
}

/// The comment with the latest `lastModified`; on a tie, or when none has a
/// usable timestamp, the last one in Google's order.
fn latest<T>(comments: Vec<T>, last_modified: impl Fn(&T) -> Option<&Timestamp>) -> Option<T> {
    comments
        .into_iter()
        .max_by_key(|comment| last_modified(comment).and_then(Timestamp::instant))
}

impl PlayReview {
    /// Maps to a [`CustomerReview`] of `package_name`, using the latest user
    /// comment and the latest developer reply. `None` without a non-blank
    /// `reviewId`: the review could neither be keyed nor replied to.
    ///
    /// - `rating`: the user comment's `starRating` (`0` when absent);
    /// - `title` / `body`: see [`split_title_body`];
    /// - `created_date`: the user comment's `lastModified`, Play's only date;
    /// - `territory`: always `None` (Play has none);
    /// - `response`: the developer reply, keyed by the review's own id.
    fn into_customer_review(self, package_name: &str) -> Option<CustomerReview> {
        let review_id = non_blank(self.review_id)?;
        let id = ReviewKey::compose(package_name, &review_id);

        let mut user_comments = Vec::new();
        let mut developer_comments = Vec::new();
        for comment in self.comments {
            user_comments.extend(comment.user_comment);
            developer_comments.extend(comment.developer_comment);
        }
        let user = latest(user_comments, |c| c.last_modified.as_ref());
        let developer = latest(developer_comments, |c| c.last_modified.as_ref());

        let (title, body) = user
            .as_ref()
            .and_then(|u| u.text.as_deref())
            .map(split_title_body)
            .unwrap_or((None, None));
        let response = developer.map(|reply| ReviewResponse {
            id: id.clone(),
            body: non_blank(reply.text),
            state: None,
            last_modified_date: reply.last_modified.as_ref().and_then(Timestamp::to_iso8601),
        });

        Some(CustomerReview {
            id,
            rating: user.as_ref().and_then(|u| u.star_rating).unwrap_or(0),
            title,
            body,
            reviewer_nickname: non_blank(self.author_name),
            created_date: user
                .as_ref()
                .and_then(|u| u.last_modified.as_ref())
                .and_then(Timestamp::to_iso8601),
            territory: None,
            response,
        })
    }
}

/// Keeps the reviews whose rating is in the requested set. Values are trimmed
/// and compared with the rating as a decimal string (`"1"` … `"5"`); blank
/// values are ignored, and an empty set keeps everything.
struct RatingFilter<'a> {
    wanted: Vec<&'a str>,
}

impl<'a> RatingFilter<'a> {
    fn new(values: &'a [String]) -> Self {
        Self {
            wanted: values
                .iter()
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .collect(),
        }
    }

    fn accepts(&self, review: &CustomerReview) -> bool {
        self.wanted.is_empty() || self.wanted.contains(&review.rating.to_string().as_str())
    }
}

/// Hints for reading an app's reviews (a 404 means the package is unknown).
fn list_hints(package_name: &str) -> ErrorHints<'_> {
    ErrorHints {
        api: GoogleApi::Publisher,
        permission: PERMISSION_VIEW_APP_INFO,
        target: Target::App(package_name),
    }
}

/// Hints for replying (a 404 may be the review, so Google's message is kept).
fn reply_hints(package_name: &str) -> ErrorHints<'_> {
    ErrorHints {
        api: GoogleApi::Publisher,
        permission: PERMISSION_REPLY_TO_REVIEWS,
        target: Target::AppResource(package_name),
    }
}

impl GooglePlayClient {
    /// One `GET …/applications/{package_name}/reviews?maxResults=…[&token=…]`.
    async fn list_reviews(
        &self,
        package_name: &str,
        max_results: u32,
        token: Option<&str>,
    ) -> Result<ReviewsListResponse, StackError> {
        let url = self.publisher_url(package_name, &["reviews"])?;
        let body = self
            .execute(
                || {
                    let request = self
                        .http()
                        .get(url.clone())
                        .query(&[("maxResults", max_results)]);
                    match token {
                        Some(token) => request.query(&[("token", token)]),
                        None => request,
                    }
                },
                &list_hints(package_name),
            )
            .await?;
        serde_json::from_str(&body)
            .map_err(|e| StackError::decode(format!("reviews response: {e}")))
    }

    /// Lists the app's recent reviews (see the module docs for Google's
    /// one-week window), following `tokenPagination.nextPageToken` with the
    /// largest page size. Reviews without an id are skipped.
    ///
    /// # Errors
    /// [`StackError::Http`] `404` for an unknown or invalid package name,
    /// [`StackError::Auth`] when Google denies access or the API is disabled,
    /// [`StackError::Decode`] on malformed JSON, a repeated page token or more
    /// than [`MAX_PAGES`] pages, [`StackError::Http`] for other non-2xx
    /// responses, or [`StackError::Network`] on transport failure.
    pub(crate) async fn fetch_customer_reviews(
        &self,
        package_name: &str,
    ) -> Result<Vec<CustomerReview>, StackError> {
        let mut reviews = Vec::new();
        let mut seen_tokens = HashSet::new();
        let mut token: Option<String> = None;

        for _ in 0..MAX_PAGES {
            let page = self
                .list_reviews(package_name, MAX_PAGE_SIZE, token.as_deref())
                .await?;
            let next = page.next_page_token();
            reviews.extend(
                page.reviews
                    .into_iter()
                    .filter_map(|review| review.into_customer_review(package_name)),
            );

            match next {
                None => return Ok(reviews),
                // A token seen before would loop forever.
                Some(next) if !seen_tokens.insert(next.clone()) => {
                    return Err(StackError::decode("reviews repeated nextPageToken"));
                }
                Some(next) => token = Some(next),
            }
        }

        Err(StackError::decode(format!(
            "reviews exceeded {MAX_PAGES} pages"
        )))
    }

    /// Fetches one page of the app's recent reviews.
    ///
    /// Google Play has no server-side sort or filter, so:
    /// - `sort` must be `""` or `-createdDate`, which both mean Google's own
    ///   order (most recent first); anything else is
    ///   [`StackError::Unsupported`], checked before any request;
    /// - `filter_rating` is applied to the page **after** it is fetched (see
    ///   [`RatingFilter`]), so a filtered page can hold fewer than `limit`
    ///   reviews, even none, while `next_token` is still `Some`: keep paging;
    /// - `limit` becomes `maxResults`, clamped to `1..=100`.
    ///
    /// `page_token` is `None` (or blank) for the first page, else a previous
    /// `next_token` verbatim.
    ///
    /// # Errors
    /// [`StackError::Unsupported`] for another `sort`; otherwise as
    /// [`Self::fetch_customer_reviews`] (without the paging errors).
    pub(crate) async fn fetch_customer_reviews_page(
        &self,
        package_name: &str,
        sort: &str,
        filter_rating: &[String],
        limit: u32,
        page_token: Option<&str>,
    ) -> Result<CustomerReviewsPage, StackError> {
        if !NATIVE_SORTS.contains(&sort) {
            return Err(StackError::unsupported(format!(
                "Google Play lists reviews in its own order (most recent first) and \
                 cannot sort them by {sort:?}"
            )));
        }

        let token = page_token.filter(|t| !t.trim().is_empty());
        let page = self
            .list_reviews(package_name, limit.clamp(1, MAX_PAGE_SIZE), token)
            .await?;
        let next_token = page.next_page_token();
        let ratings = RatingFilter::new(filter_rating);
        let reviews = page
            .reviews
            .into_iter()
            .filter_map(|review| review.into_customer_review(package_name))
            .filter(|review| ratings.accepts(review))
            .collect();

        Ok(CustomerReviewsPage {
            reviews,
            next_token,
        })
    }

    /// Sets the developer reply of a review (`POST …/reviews/{id}:reply`),
    /// replacing any previous reply. `review_id` is the opaque
    /// [`CustomerReview::id`] this module returns.
    ///
    /// `body` is sent as is. Google rejects replies longer than about 350
    /// characters and strips HTML; the length is deliberately not checked here
    /// (Google's limit is approximate), so a rejection comes back as Google's
    /// own [`StackError::Http`] `400`.
    ///
    /// The returned [`ReviewResponse`] carries the text Google applied, the time
    /// it took effect, and `review_id` as its id.
    ///
    /// # Errors
    /// [`StackError::Decode`] for an id this module did not produce (nothing is
    /// sent) or a malformed answer, [`StackError::Auth`] when Google denies the
    /// reply (the service account needs "Reply to reviews"),
    /// [`StackError::Http`] for other non-2xx responses (`404`: unknown
    /// review), or [`StackError::Network`] on transport failure.
    pub(crate) async fn reply_to_review(
        &self,
        review_id: &str,
        body: &str,
    ) -> Result<ReviewResponse, StackError> {
        let key = ReviewKey::parse(review_id)?;
        let reply_segment = format!("{}:reply", key.review_id);
        let url = self.publisher_url(key.package_name, &["reviews", &reply_segment])?;
        let request_body = json!({ "replyText": body });

        let response = self
            .execute(
                || self.http().post(url.clone()).json(&request_body),
                &reply_hints(key.package_name),
            )
            .await?;
        let reply: ReplyResponse = serde_json::from_str(&response)
            .map_err(|e| StackError::decode(format!("review reply response: {e}")))?;
        let result = reply
            .result
            .ok_or_else(|| StackError::decode("review reply response: missing result"))?;

        Ok(ReviewResponse {
            id: review_id.to_string(),
            body: non_blank(result.reply_text),
            state: None,
            last_modified_date: result.last_edited.as_ref().and_then(Timestamp::to_iso8601),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::googleplay::test_support::{client, mount_token, CapturingLogger};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use wiremock::matchers::{body_json, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    const PACKAGE: &str = "com.example.app";
    const REVIEWS_PATH: &str = "/androidpublisher/v3/applications/com.example.app/reviews";

    fn list() -> wiremock::MockBuilder {
        Mock::given(method("GET")).and(path(REVIEWS_PATH))
    }

    fn reply_mock(review_id: &str) -> wiremock::MockBuilder {
        Mock::given(method("POST")).and(path(format!("{REVIEWS_PATH}/{review_id}:reply")))
    }

    /// A Play review with one user comment (`rating`, `text`, `seconds`).
    fn review(id: &str, rating: i32, text: &str, seconds: i64) -> serde_json::Value {
        serde_json::json!({
            "reviewId": id,
            "authorName": "Ana",
            "comments": [{
                "userComment": {
                    "text": text,
                    "lastModified": { "seconds": seconds.to_string(), "nanos": 500 },
                    "starRating": rating,
                    "reviewerLanguage": "en",
                    "device": "klte",
                    "appVersionCode": 42,
                    "appVersionName": "1.4.2"
                }
            }]
        })
    }

    fn plain(id: &str, rating: i32) -> CustomerReview {
        CustomerReview {
            id: format!("{PACKAGE}/{id}"),
            rating,
            title: None,
            body: Some("ok".into()),
            reviewer_nickname: Some("Ana".into()),
            created_date: Some("2023-11-14T22:13:20Z".into()),
            territory: None,
            response: None,
        }
    }

    /// Answers every page with a fresh token, so paging never ends on its own.
    struct EndlessPages {
        served: AtomicUsize,
    }

    impl Respond for EndlessPages {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            let n = self.served.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reviews": [review(&format!("r{n}"), 5, "ok", 1_700_000_000)],
                "tokenPagination": { "nextPageToken": format!("T{n}") }
            }))
        }
    }

    // -----------------------------------------------------------------------
    // Composite review ids
    // -----------------------------------------------------------------------

    #[test]
    fn review_key_round_trips() {
        let id = ReviewKey::compose(PACKAGE, "gp:AOqpTOH");
        assert_eq!(id, "com.example.app/gp:AOqpTOH");
        assert_eq!(
            ReviewKey::parse(&id).unwrap(),
            ReviewKey {
                package_name: PACKAGE,
                review_id: "gp:AOqpTOH"
            }
        );
    }

    #[test]
    fn review_key_splits_at_the_first_separator() {
        // Package names cannot hold a `/`, so the rest belongs to Google's id.
        assert_eq!(
            ReviewKey::parse("com.example.app/a/b").unwrap(),
            ReviewKey {
                package_name: PACKAGE,
                review_id: "a/b"
            }
        );
    }

    #[test]
    fn review_key_rejects_foreign_ids() {
        for id in [
            "",
            "gp:AOqpTOH",
            "/gp:AOqpTOH",
            "com.example.app/",
            "com.example.app/  ",
            "com.example.app/.",
            "com.example.app/..",
            "com/gp:AOqpTOH",
            "com.exa mple/gp:AOqpTOH",
            // An App Store Connect review id.
            "00000029-1f2b-4d0e-a8a6-5b1a8f0e4c9a",
        ] {
            match ReviewKey::parse(id).unwrap_err() {
                StackError::Decode { message } => {
                    assert!(message.contains("invalid Google Play review id"), "{id:?}");
                    assert!(message.contains("<packageName>/<reviewId>"), "{id:?}");
                }
                other => panic!("expected Decode for {id:?}, got {other:?}"),
            }
        }
    }

    // -----------------------------------------------------------------------
    // Mapping
    // -----------------------------------------------------------------------

    #[test]
    fn splits_title_and_body_at_the_first_tab() {
        for (text, title, body) in [
            (
                "Great app\tLove the widgets",
                Some("Great app"),
                Some("Love the widgets"),
            ),
            ("No title here", None, Some("No title here")),
            ("\tBody only", None, Some("Body only")),
            ("Title only\t", Some("Title only"), None),
            ("a\tb\tc", Some("a"), Some("b\tc")),
            ("  \t  ", None, None),
            ("", None, None),
        ] {
            assert_eq!(
                split_title_body(text),
                (title.map(str::to_string), body.map(str::to_string)),
                "{text:?}"
            );
        }
    }

    fn parse_review(json: serde_json::Value) -> Option<CustomerReview> {
        serde_json::from_value::<PlayReview>(json)
            .unwrap()
            .into_customer_review(PACKAGE)
    }

    #[test]
    fn maps_user_comment_and_developer_reply() {
        let mapped = parse_review(serde_json::json!({
            "reviewId": "gp:1",
            "authorName": "Ana",
            "comments": [
                { "userComment": {
                    "text": "Great\tWorks offline",
                    "lastModified": { "seconds": "1452717656", "nanos": 123 },
                    "starRating": 4
                } },
                { "developerComment": {
                    "text": "Thanks!",
                    "lastModified": { "seconds": "1700000000" }
                } }
            ]
        }))
        .unwrap();

        assert_eq!(
            mapped,
            CustomerReview {
                id: "com.example.app/gp:1".into(),
                rating: 4,
                title: Some("Great".into()),
                body: Some("Works offline".into()),
                reviewer_nickname: Some("Ana".into()),
                created_date: Some("2016-01-13T20:40:56Z".into()),
                territory: None,
                response: Some(ReviewResponse {
                    id: "com.example.app/gp:1".into(),
                    body: Some("Thanks!".into()),
                    state: None,
                    last_modified_date: Some("2023-11-14T22:13:20Z".into()),
                }),
            }
        );
    }

    #[test]
    fn latest_comments_win_regardless_of_order() {
        let mapped = parse_review(serde_json::json!({
            "reviewId": "gp:1",
            "comments": [
                { "userComment": { "text": "new", "starRating": 2,
                    "lastModified": { "seconds": "200" } } },
                { "developerComment": { "text": "new reply",
                    "lastModified": { "seconds": "300", "nanos": 2 } } },
                { "userComment": { "text": "old", "starRating": 5,
                    "lastModified": { "seconds": "100" } } },
                { "developerComment": { "text": "old reply",
                    "lastModified": { "seconds": "300", "nanos": 1 } } }
            ]
        }))
        .unwrap();

        assert_eq!(mapped.body.as_deref(), Some("new"));
        assert_eq!(mapped.rating, 2);
        assert_eq!(
            mapped.response.and_then(|r| r.body).as_deref(),
            Some("new reply")
        );
    }

    #[test]
    fn without_timestamps_the_last_comment_wins() {
        let mapped = parse_review(serde_json::json!({
            "reviewId": "gp:1",
            "comments": [
                { "userComment": { "text": "first", "starRating": 1 } },
                { "userComment": { "text": "second", "starRating": 3 } }
            ]
        }))
        .unwrap();
        assert_eq!(mapped.body.as_deref(), Some("second"));
        assert_eq!(mapped.rating, 3);
        assert_eq!(mapped.created_date, None);
    }

    #[test]
    fn review_without_comments_or_author_keeps_its_id() {
        let mapped =
            parse_review(serde_json::json!({ "reviewId": "gp:1", "authorName": " " })).unwrap();
        assert_eq!(
            mapped,
            CustomerReview {
                id: "com.example.app/gp:1".into(),
                rating: 0,
                title: None,
                body: None,
                reviewer_nickname: None,
                created_date: None,
                territory: None,
                response: None,
            }
        );
    }

    #[test]
    fn review_without_id_is_skipped() {
        assert_eq!(
            parse_review(serde_json::json!({ "authorName": "Ana" })),
            None
        );
        assert_eq!(parse_review(serde_json::json!({ "reviewId": "  " })), None);
    }

    #[test]
    fn rating_filter_trims_ignores_blanks_and_matches_decimal_text() {
        let review = |rating| CustomerReview {
            rating,
            ..plain("gp:1", rating)
        };
        let values = vec![" 4".to_string(), "5 ".to_string(), "".to_string()];
        let filter = RatingFilter::new(&values);
        assert!(filter.accepts(&review(4)));
        assert!(filter.accepts(&review(5)));
        assert!(!filter.accepts(&review(3)));

        let none: Vec<String> = vec![];
        assert!(RatingFilter::new(&none).accepts(&review(1)));
        let blanks = vec!["  ".to_string()];
        assert!(RatingFilter::new(&blanks).accepts(&review(1)));
        let garbage = vec!["five".to_string()];
        assert!(!RatingFilter::new(&garbage).accepts(&review(5)));
    }

    // -----------------------------------------------------------------------
    // fetch_customer_reviews
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn fetch_customer_reviews_maps_and_paginates() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .and(query_param("maxResults", "100"))
            .and(query_param_is_missing("token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reviews": [review("gp:1", 5, "ok", 1_700_000_000)],
                "pageInfo": { "totalResults": 2, "resultPerPage": 1, "startIndex": 0 },
                "tokenPagination": { "nextPageToken": "PAGE+2/=" }
            })))
            .expect(1)
            .mount(&server)
            .await;
        list()
            .and(query_param("maxResults", "100"))
            .and(query_param("token", "PAGE+2/="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reviews": [review("gp:2", 1, "ok", 1_700_000_000)],
                "tokenPagination": {}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let reviews = client(&server)
            .fetch_customer_reviews(PACKAGE)
            .await
            .unwrap();
        assert_eq!(reviews, vec![plain("gp:1", 5), plain("gp:2", 1)]);
    }

    #[tokio::test]
    async fn fetch_customer_reviews_empty_object_is_empty() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        let reviews = client(&server)
            .fetch_customer_reviews(PACKAGE)
            .await
            .unwrap();
        assert!(reviews.is_empty());
    }

    #[tokio::test]
    async fn fetch_customer_reviews_repeated_token_is_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reviews": [], "tokenPagination": { "nextPageToken": "T1" }
            })))
            .expect(2)
            .mount(&server)
            .await;

        match client(&server)
            .fetch_customer_reviews(PACKAGE)
            .await
            .unwrap_err()
        {
            StackError::Decode { message } => assert!(message.contains("repeated nextPageToken")),
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_customer_reviews_stops_after_max_pages() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        // Literal, not `MAX_PAGES`, so a changed cap fails here.
        list()
            .respond_with(EndlessPages {
                served: AtomicUsize::new(0),
            })
            .expect(100)
            .mount(&server)
            .await;

        match client(&server)
            .fetch_customer_reviews(PACKAGE)
            .await
            .unwrap_err()
        {
            StackError::Decode { message } => assert!(message.contains("exceeded 100 pages")),
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_customer_reviews_malformed_json_is_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"reviews": 1}"#))
            .mount(&server)
            .await;

        let err = client(&server)
            .fetch_customer_reviews(PACKAGE)
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Decode { .. }));
    }

    #[tokio::test]
    async fn unknown_package_is_http_404_naming_it() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": { "code": 404, "message": "Package not found: com.example.app.",
                           "status": "NOT_FOUND" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server)
            .fetch_customer_reviews(PACKAGE)
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 404);
                assert!(message
                    .starts_with("Google Play found no app with package name com.example.app"));
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn publisher_api_disabled_is_auth_naming_it() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": { "code": 403, "status": "PERMISSION_DENIED",
                    "message": "Google Play Android Developer API has not been used in project 1 before or it is disabled.",
                    "details": [{ "@type": "type.googleapis.com/google.rpc.ErrorInfo",
                        "reason": "SERVICE_DISABLED",
                        "metadata": { "service": "androidpublisher.googleapis.com",
                            "serviceTitle": "Google Play Android Developer API",
                            "activationUrl": "https://console.developers.google.com/apis/api/androidpublisher.googleapis.com/overview?project=1" } }] }
            })))
            .mount(&server)
            .await;

        match client(&server)
            .fetch_customer_reviews(PACKAGE)
            .await
            .unwrap_err()
        {
            StackError::Auth { message } => {
                assert!(message.starts_with("Google Play Android Developer API is not enabled"));
                assert!(message.contains("androidpublisher.googleapis.com/overview?project=1"));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn permission_denied_401_is_auth_after_one_retry() {
        let server = MockServer::start().await;
        // The 401 drops the cached token, so the retry exchanges a new one.
        mount_token(&server, "tok-1", 2).await;
        list()
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": { "code": 401,
                    "message": "The current user has insufficient permissions to perform the requested operation.",
                    "errors": [{ "domain": "androidpublisher", "reason": "permissionDenied" }] }
            })))
            .expect(2)
            .mount(&server)
            .await;

        match client(&server)
            .fetch_customer_reviews(PACKAGE)
            .await
            .unwrap_err()
        {
            StackError::Auth { message } => {
                assert!(message.contains("cannot access the app com.example.app"));
                assert!(message.contains("\"View app information (read-only)\""));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn invalid_package_sends_nothing() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 0).await;

        match client(&server)
            .fetch_customer_reviews("../com.example.app")
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 404);
                assert!(message.contains("is not a valid Android package name"));
            }
            other => panic!("expected Http, got {other:?}"),
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // fetch_customer_reviews_page
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn page_first_page_returns_next_token() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .and(query_param("maxResults", "20"))
            .and(query_param_is_missing("token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reviews": [review("gp:1", 5, "ok", 1_700_000_000)],
                "tokenPagination": { "nextPageToken": "NEXT" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let page = client(&server)
            .fetch_customer_reviews_page(PACKAGE, "-createdDate", &[], 20, None)
            .await
            .unwrap();
        assert_eq!(
            page,
            CustomerReviewsPage {
                reviews: vec![plain("gp:1", 5)],
                next_token: Some("NEXT".into()),
            }
        );
    }

    #[tokio::test]
    async fn page_follows_token_and_ends_without_one() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .and(query_param("token", "a+b/c="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reviews": [review("gp:9", 3, "ok", 1_700_000_000)]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let page = client(&server)
            .fetch_customer_reviews_page(PACKAGE, "", &[], 10, Some("a+b/c="))
            .await
            .unwrap();
        assert_eq!(page.reviews, vec![plain("gp:9", 3)]);
        assert_eq!(page.next_token, None);

        // `query_param` decodes; the wire form must be percent-encoded.
        let requests = server.received_requests().await.unwrap();
        let listed = requests
            .iter()
            .find(|r| r.url.path() == REVIEWS_PATH)
            .expect("a reviews request");
        assert!(listed.url.query().unwrap().contains("token=a%2Bb%2Fc%3D"));
    }

    #[tokio::test]
    async fn page_blank_token_is_the_first_page() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .and(query_param_is_missing("token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        let page = client(&server)
            .fetch_customer_reviews_page(PACKAGE, "", &[], 10, Some("  "))
            .await
            .unwrap();
        assert!(page.reviews.is_empty());
        assert_eq!(page.next_token, None);
    }

    #[tokio::test]
    async fn page_clamps_limit_to_google_bounds() {
        for (limit, sent) in [
            (0, "1"),
            (1, "1"),
            (100, "100"),
            (101, "100"),
            (u32::MAX, "100"),
        ] {
            let server = MockServer::start().await;
            mount_token(&server, "tok-1", 1).await;
            list()
                .and(query_param("maxResults", sent))
                .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
                .expect(1)
                .mount(&server)
                .await;

            client(&server)
                .fetch_customer_reviews_page(PACKAGE, "", &[], limit, None)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn page_filters_ratings_after_fetching_and_keeps_the_token() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reviews": [
                    review("gp:1", 1, "ok", 1_700_000_000),
                    review("gp:4", 4, "ok", 1_700_000_000),
                    review("gp:5", 5, "ok", 1_700_000_000)
                ],
                "tokenPagination": { "nextPageToken": "NEXT" }
            })))
            .expect(2)
            .mount(&server)
            .await;
        let client = client(&server);

        let page = client
            .fetch_customer_reviews_page(PACKAGE, "", &["4".into(), "5".into()], 3, None)
            .await
            .unwrap();
        assert_eq!(page.reviews, vec![plain("gp:4", 4), plain("gp:5", 5)]);
        assert_eq!(page.next_token.as_deref(), Some("NEXT"));

        // Nothing matches on this page, but there are more pages.
        let empty = client
            .fetch_customer_reviews_page(PACKAGE, "", &["2".into()], 3, None)
            .await
            .unwrap();
        assert!(empty.reviews.is_empty());
        assert_eq!(empty.next_token.as_deref(), Some("NEXT"));
    }

    #[tokio::test]
    async fn page_rejects_other_sorts_before_any_request() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 0).await;
        let client = client(&server);

        for sort in ["createdDate", "-rating", "rating", "-createddate"] {
            match client
                .fetch_customer_reviews_page(PACKAGE, sort, &[], 10, None)
                .await
                .unwrap_err()
            {
                StackError::Unsupported { message } => {
                    assert!(message.contains("own order (most recent first)"));
                    assert!(message.contains(&format!("{sort:?}")));
                }
                other => panic!("expected Unsupported for {sort}, got {other:?}"),
            }
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // reply_to_review
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn reply_posts_text_and_maps_the_result() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        reply_mock("gp:AOqp")
            .and(body_json(
                serde_json::json!({ "replyText": "Thanks for the feedback!" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "result": {
                    "replyText": "Thanks for the feedback!",
                    "lastEdited": { "seconds": "1700000000", "nanos": 999 }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let response = client(&server)
            .reply_to_review("com.example.app/gp:AOqp", "Thanks for the feedback!")
            .await
            .unwrap();
        assert_eq!(
            response,
            ReviewResponse {
                id: "com.example.app/gp:AOqp".into(),
                body: Some("Thanks for the feedback!".into()),
                state: None,
                last_modified_date: Some("2023-11-14T22:13:20Z".into()),
            }
        );
    }

    #[tokio::test]
    async fn reply_sends_long_text_unchanged_and_surfaces_googles_rejection() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        let long = "x".repeat(351);
        reply_mock("gp:1")
            .and(body_json(serde_json::json!({ "replyText": long })))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": { "code": 400, "message": "Reply text is too long.",
                           "status": "INVALID_ARGUMENT" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        match client(&server)
            .reply_to_review("com.example.app/gp:1", &long)
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 400);
                assert_eq!(message, "Reply text is too long. [INVALID_ARGUMENT]");
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reply_with_a_foreign_id_sends_nothing() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 0).await;

        let err = client(&server)
            .reply_to_review("00000029-1f2b-4d0e", "hi")
            .await
            .unwrap_err();
        assert!(matches!(err, StackError::Decode { .. }));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn reply_forbidden_names_the_reply_permission() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        reply_mock("gp:1")
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": { "code": 403, "message": "The caller does not have permission",
                           "status": "PERMISSION_DENIED" }
            })))
            .mount(&server)
            .await;

        match client(&server)
            .reply_to_review("com.example.app/gp:1", "hi")
            .await
            .unwrap_err()
        {
            StackError::Auth { message } => {
                assert!(message.contains("cannot access the app com.example.app"));
                assert!(message.contains("\"Reply to reviews\""));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reply_404_keeps_googles_message() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        reply_mock("gp:gone")
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": { "code": 404, "message": "Review not found.", "status": "NOT_FOUND" }
            })))
            .mount(&server)
            .await;

        match client(&server)
            .reply_to_review("com.example.app/gp:gone", "hi")
            .await
            .unwrap_err()
        {
            StackError::Http { status, message } => {
                assert_eq!(status, 404);
                assert_eq!(message, "Review not found. [NOT_FOUND]");
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reply_without_result_is_decode() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        reply_mock("gp:1")
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        match client(&server)
            .reply_to_review("com.example.app/gp:1", "hi")
            .await
            .unwrap_err()
        {
            StackError::Decode { message } => assert!(message.contains("missing result")),
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn debug_logger_traces_reviews_calls() {
        let server = MockServer::start().await;
        mount_token(&server, "tok-1", 1).await;
        list()
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let logger = Arc::new(CapturingLogger::default());
        let client = client(&server).with_debug_logger(Some(logger.clone()));
        client.fetch_customer_reviews(PACKAGE).await.unwrap();

        // Token exchange (2 lines), then the reviews request and response.
        let messages = logger.messages();
        assert_eq!(messages.len(), 4);
        assert!(messages[2].contains(&format!(
            "curl -X GET '{}{REVIEWS_PATH}?maxResults=100'",
            server.uri()
        )));
        assert!(messages[2].contains("authorization: Bearer tok-1"));
        assert!(messages[3].starts_with("[RustCore] ← 200"));
    }
}
