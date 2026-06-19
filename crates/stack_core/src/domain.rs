/// Cross-provider app metadata. Mirrors the Swift `AppInfo` from `StackProtocols`,
/// which is dissolved into the core (see RUST_CORE_PLAN.md §4).
///
/// Serializes camelCase (`bundleId`, not `bundle_id`) so persisted blobs match the
/// iOS-facing contract — see [`crate::service::sync::SyncService`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub bundle_id: String,
    pub platform: Option<String>,
}

/// The developer's response attached to a [`CustomerReview`]. Dates are raw
/// ISO8601 strings; the core does no date parsing (the host owns that).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct ReviewResponse {
    pub id: String,
    pub body: Option<String>,
    pub state: Option<String>,
    pub last_modified_date: Option<String>,
}

/// A single end-user App Store review, optionally with the developer's response.
/// Dates are raw ISO8601 strings; the core does no date parsing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct CustomerReview {
    pub id: String,
    pub rating: i32,
    pub title: Option<String>,
    pub body: Option<String>,
    pub reviewer_nickname: Option<String>,
    pub created_date: Option<String>,
    pub territory: Option<String>,
    pub response: Option<ReviewResponse>,
}

/// One page of customer reviews plus an opaque token to fetch the next page.
/// `next_token` is `None` on the last page; otherwise pass it back verbatim as
/// the next call's `page_token` (it is the JSON:API `links.next` URL).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CustomerReviewsPage {
    pub reviews: Vec<CustomerReview>,
    pub next_token: Option<String>,
}

/// A review submission to App Store review (the act of submitting an app version
/// for review), with the resolved version and submitter where available. Dates
/// are raw ISO8601 strings; the core does no date parsing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct ReviewSubmission {
    pub id: String,
    pub app_id: String,
    pub platform: Option<String>,
    pub submitted_date: Option<String>,
    pub state: Option<String>,
    pub version_string: Option<String>,
    pub version_id: Option<String>,
    pub submitted_by_name: Option<String>,
    pub submitted_by_email: Option<String>,
}

/// An App Store version of an app. Dates are raw ISO8601 strings; the core does
/// no date parsing (the host owns that).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct AppStoreVersionInfo {
    pub id: String,
    pub app_id: String,
    pub platform: Option<String>,
    pub app_store_state: Option<String>,
    pub app_version_state: Option<String>,
    pub version_string: Option<String>,
    pub copyright: Option<String>,
    pub release_type: Option<String>,
    pub created_date: Option<String>,
}

/// An App Store version's phased (staged) release. App Store Connect exposes
/// exactly one per version via the singular `appStoreVersionPhasedRelease`
/// relationship. `state` carries the raw ASC `phasedReleaseState` value
/// (`INACTIVE` / `ACTIVE` / `PAUSED` / `COMPLETE`) — the record field is named
/// `state` even though the attribute is `phasedReleaseState`. `start_date` is a
/// raw ISO8601 string; the core does no date parsing (the host owns that). All
/// optional fields are `None` when the corresponding attribute is absent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct PhasedReleaseInfo {
    pub id: String,
    pub state: Option<String>,
    pub start_date: Option<String>,
    pub total_pause_duration: Option<i32>,
    pub current_day_number: Option<i32>,
}

/// A build (TestFlight / App Store Connect) of an app. `version` is the build
/// number (the ASC `version` attribute, distinct from a version string). Dates
/// are raw ISO8601 strings; the core does no date parsing (the host owns that).
///
/// Beyond the build's own attributes this record also carries enrichment
/// resolved from JSON:API `included` related resources — the marketing version
/// and platform (from `preReleaseVersion`), the external/internal build states
/// and auto-notify flag (from `buildBetaDetail`), and the beta review state and
/// submission date (from `betaAppReviewSubmission`). These enrichment fields are
/// only populated when the corresponding relationship is requested via `include`
/// (and present); they are `None` otherwise. `icon_url` is computed from the
/// build's `iconAssetToken` template.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub id: String,
    pub app_id: String,
    pub version: Option<String>,
    pub uploaded_date: Option<String>,
    pub expired: Option<bool>,
    pub processing_state: Option<String>,
    pub min_os_version: Option<String>,
    pub expiration_date: Option<String>,
    /// From included `preReleaseVersion.attributes.version`.
    pub marketing_version: Option<String>,
    /// From included `preReleaseVersion.attributes.platform`.
    pub platform: Option<String>,
    /// From included `buildBetaDetail.attributes.externalBuildState`.
    pub external_build_state: Option<String>,
    /// From included `buildBetaDetail.attributes.internalBuildState`.
    pub internal_build_state: Option<String>,
    /// From included `buildBetaDetail.attributes.autoNotifyEnabled`.
    pub auto_notify_enabled: Option<bool>,
    /// From included `betaAppReviewSubmission.attributes.betaReviewState`.
    pub beta_review_state: Option<String>,
    /// From included `betaAppReviewSubmission.attributes.submittedDate` (raw ISO8601).
    pub submitted_date: Option<String>,
    /// Build attribute `computedMinMacOsVersion`.
    pub computed_min_mac_os_version: Option<String>,
    /// Build attribute `computedMinVisionOsVersion`.
    pub computed_min_vision_os_version: Option<String>,
    /// Build attribute `buildAudienceType`.
    pub build_audience_type: Option<String>,
    /// Build attribute `usesNonExemptEncryption`.
    pub uses_non_exempt_encryption: Option<bool>,
    /// Computed from the build's `iconAssetToken` template (`{w}`/`{h}`/`{f}`
    /// substituted). `None` when no template URL is present.
    pub icon_url: Option<String>,
}

/// One page of builds plus an opaque token to fetch the next page. `next_token`
/// is `None` on the last page; otherwise pass it back verbatim as the next call's
/// `page_token` (it is the JSON:API `links.next` URL). Mirrors
/// [`CustomerReviewsPage`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BuildsPage {
    pub builds: Vec<BuildInfo>,
    pub next_token: Option<String>,
}

/// The full detail of a single build: the enriched [`BuildInfo`] plus its
/// associated beta groups and per-locale "What to Test" localizations, all
/// resolved from the JSON:API `included` section of the build document.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BuildDetailInfo {
    pub build: BuildInfo,
    pub beta_groups: Vec<BetaGroupInfo>,
    pub localizations: Vec<BetaBuildLocalizationInfo>,
}

/// A TestFlight beta group (internal or external) of an app. Dates are raw
/// ISO8601 strings; the core does no date parsing (the host owns that).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BetaGroupInfo {
    pub id: String,
    pub app_id: String,
    pub name: Option<String>,
    pub created_date: Option<String>,
    pub is_internal_group: Option<bool>,
    pub has_access_to_all_builds: Option<bool>,
    pub public_link_enabled: Option<bool>,
    pub public_link: Option<String>,
    pub feedback_enabled: Option<bool>,
}

/// A TestFlight "What to Test" localization for a single build, keyed by
/// `locale`. `whats_new` carries the per-locale testing notes shown to testers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BetaBuildLocalizationInfo {
    pub id: String,
    pub locale: String,
    pub whats_new: Option<String>,
}

/// A TestFlight app-level localization, keyed by `locale`. `feedback_email` is
/// the per-locale address testers' feedback is sent to, and `description` is the
/// per-locale TestFlight test description shown to testers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BetaAppLocalizationInfo {
    pub id: String,
    pub locale: String,
    pub feedback_email: Option<String>,
    pub description: Option<String>,
}

/// An App Store Connect accessibility declaration for a single device family of
/// an app. `device_family` is the ASC device-family value (e.g. `IPHONE`,
/// `IPAD`, `MAC`); `state` is the publication state (`DRAFT`, `PUBLISHED`,
/// `REPLACED`) when present. The nine `supports_*` flags declare which
/// accessibility features the app supports for that device family.
///
/// Note: the App Store Connect wire attribute for
/// [`Self::supports_differentiate_without_color`] is
/// `supportsDifferentiateWithoutColorAlone` (with an `Alone` suffix); the field
/// is named without the suffix here for the host's benefit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AccessibilityDeclarationInfo {
    pub id: String,
    pub device_family: String,
    pub state: Option<String>,
    pub supports_audio_descriptions: bool,
    pub supports_captions: bool,
    pub supports_dark_interface: bool,
    pub supports_differentiate_without_color: bool,
    pub supports_larger_text: bool,
    pub supports_reduced_motion: bool,
    pub supports_sufficient_contrast: bool,
    pub supports_voice_control: bool,
    pub supports_voiceover: bool,
}

/// A registered device of the connected App Store Connect account (a development
/// device used for provisioning). `name` and `status` are non-optional with sane
/// fallbacks (`""` and `"ENABLED"`) applied at the wire-mapping boundary when the
/// attribute is absent; the remaining attributes are optional.
///
/// `platform` is the raw ASC `BundleIdPlatform` value (`IOS`, `MAC_OS`, or
/// `UNIVERSAL`); `status` is the raw ASC device status (`ENABLED` or `DISABLED`).
/// `device_class` mirrors the ASC `deviceClass` attribute and `added_date` the
/// `addedDate` attribute — a raw ISO8601 string passed through verbatim (the host
/// owns any date parsing). All raw ASC strings are forwarded without remapping.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub udid: Option<String>,
    pub platform: Option<String>,
    pub device_class: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub added_date: Option<String>,
}

/// A bundle ID (App ID) registered for the connected App Store Connect account.
///
/// `identifier`, `name`, and `platform` are non-optional with an empty-string
/// fallback applied at the wire-mapping boundary when the attribute is absent;
/// `seed_id` is optional (the ASC `seedId` attribute). `platform` is the raw ASC
/// `BundleIdPlatform` value (`IOS`, `MAC_OS`, or `UNIVERSAL`), forwarded without
/// remapping.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BundleIdInfo {
    pub id: String,
    #[serde(default)]
    pub identifier: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub platform: String,
    pub seed_id: Option<String>,
}

/// A capability enabled on a bundle ID (e.g. `PUSH_NOTIFICATIONS`,
/// `FONT_INSTALLATION`, `CARPLAY_CHARGING`).
///
/// `capability_type` is the raw ASC `capabilityType` string, forwarded verbatim:
/// App Store Connect keeps adding values, so the core never models it as a closed
/// enum. The wire-mapping boundary skips resources whose `capabilityType` is
/// missing or empty.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BundleIdCapabilityInfo {
    pub id: String,
    pub capability_type: String,
}

/// A signing certificate registered for the connected App Store Connect account
/// (development, distribution, push, Pass Type ID, or Apple Pay merchant
/// certificate).
///
/// `display_name`, `name`, and `certificate_type` are non-optional with an
/// empty-string fallback applied at the wire-mapping boundary when the attribute
/// is absent; `platform`, `serial_number`, and `expiration_date` are optional.
/// `certificate_type` is the raw ASC `CertificateType` value (e.g.
/// `IOS_DEVELOPMENT`, `IOS_DISTRIBUTION`, `MAC_APP_DISTRIBUTION`), forwarded
/// without remapping. `expiration_date` is the raw ISO8601 string passed through
/// verbatim — the core does no date parsing.
///
/// `is_activated` maps the ASC `activated` attribute (note the wire key is
/// `activated`, not `isActivated`). `certificate_content` is the base64-encoded
/// certificate payload: it is `None` for list results (the list omits it) and
/// `Some` after a create or single-resource fetch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct CertificateInfo {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub certificate_type: String,
    pub platform: Option<String>,
    pub serial_number: Option<String>,
    pub expiration_date: Option<String>,
    pub is_activated: bool,
    pub certificate_content: Option<String>,
}

/// A provisioning profile registered for the connected App Store Connect
/// account, pairing a bundle ID with the certificates (and, for development
/// profiles, the devices) it authorizes.
///
/// `name`, `profile_type`, and `profile_state` are non-optional with an
/// empty-string fallback applied at the wire-mapping boundary when the attribute
/// is absent; `platform`, `uuid`, `bundle_id`, `created_date`, and
/// `expiration_date` are optional. `profile_type` is the raw ASC `profileType`
/// value (e.g. `IOS_APP_DEVELOPMENT`, `IOS_APP_STORE`, `MAC_APP_STORE`),
/// forwarded without remapping; `profile_state` is the raw `profileState` value
/// (e.g. `ACTIVE`, `INVALID`).
///
/// `bundle_id` is the RESOLVED bundle identifier string (e.g. `com.acme.app`),
/// looked up from the JSON:API `included[]` bundleIds by the profile's
/// `bundleId` relationship id — not the relationship id itself. It is `None` when
/// the profile carries no `bundleId` relationship, when the referenced bundle ID
/// is absent from `included[]`, or on the create path (the host does not resolve
/// it there).
///
/// `created_date` and `expiration_date` are raw ISO8601 strings passed through
/// verbatim — the core does no date parsing. `profile_content` is the base64-
/// encoded `.mobileprovision` payload: it is `None` for list results (the list
/// omits it) and `Some` after a create or single-resource content fetch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ProvisioningProfileInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub profile_type: String,
    #[serde(default)]
    pub profile_state: String,
    pub platform: Option<String>,
    pub uuid: Option<String>,
    pub bundle_id: Option<String>,
    pub created_date: Option<String>,
    pub expiration_date: Option<String>,
    pub profile_content: Option<String>,
}

/// The TestFlight "Test Information" beta review detail for an app: the beta
/// review contact (name, email, phone), optional demo account credentials, and
/// reviewer notes. App Store Connect exposes exactly one per app (the singular
/// `betaAppReviewDetail` relationship). All attributes are optional.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BetaAppReviewDetailInfo {
    pub id: String,
    pub contact_first_name: Option<String>,
    pub contact_last_name: Option<String>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub demo_account_name: Option<String>,
    pub demo_account_password: Option<String>,
    pub is_demo_account_required: Option<bool>,
    pub notes: Option<String>,
}

/// The App Store version "App Review Information" detail: the app-review contact
/// (name, email, phone), optional demo account credentials, and reviewer notes
/// surfaced at submission time. App Store Connect exposes exactly one per version
/// (the singular `appStoreReviewDetail` relationship). All attributes are
/// optional. This is version-scoped, distinct from [`BetaAppReviewDetailInfo`]
/// (which is the TestFlight/app-scoped beta review detail).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AppReviewDetailInfo {
    pub id: String,
    pub contact_first_name: Option<String>,
    pub contact_last_name: Option<String>,
    pub contact_email: Option<String>,
    pub contact_phone: Option<String>,
    pub notes: Option<String>,
    pub demo_account_name: Option<String>,
    pub demo_account_password: Option<String>,
    pub is_demo_account_required: Option<bool>,
}

/// An App Store app-info localization, keyed by `locale`. Carries the per-locale
/// App Store listing metadata: the `name` and `subtitle` shown on the product
/// page, plus the three privacy links/text (`privacy_policy_url`,
/// `privacy_choices_url`, `privacy_policy_text`). All attributes are optional;
/// App Store Connect serializes them camelCase (`privacyPolicyUrl`,
/// `privacyChoicesUrl`, `privacyPolicyText`), which `rename_all = "camelCase"`
/// maps without any per-field rename.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AppInfoLocalizationInfo {
    pub id: String,
    pub locale: String,
    pub name: Option<String>,
    pub subtitle: Option<String>,
    pub privacy_policy_url: Option<String>,
    pub privacy_choices_url: Option<String>,
    pub privacy_policy_text: Option<String>,
}

/// The full App Info detail for an app: the app-info resource's own ids and
/// category/age-rating wiring, merged with the owning app's `sku`,
/// `primary_locale`, and `content_rights_declaration`. `localizations` reuses the
/// sibling [`AppInfoLocalizationInfo`] record, and `age_rating` carries the
/// resolved [`AgeRatingDeclarationInfo`] when present in the JSON:API `included`
/// section. The category ids are resolved from the app-info resource's
/// relationships (not its attributes). All optional fields are `None` when the
/// corresponding attribute / relationship is absent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AppInfoDetails {
    pub app_info_id: String,
    pub app_id: String,
    pub sku: Option<String>,
    pub primary_locale: Option<String>,
    pub content_rights_declaration: Option<String>,
    pub primary_category_id: Option<String>,
    pub primary_subcategory_one_id: Option<String>,
    pub secondary_category_id: Option<String>,
    pub secondary_subcategory_one_id: Option<String>,
    pub age_rating_declaration_id: Option<String>,
    pub app_store_age_rating: Option<String>,
    pub localizations: Vec<AppInfoLocalizationInfo>,
    pub age_rating: Option<AgeRatingDeclarationInfo>,
}

/// An App Store age-rating declaration. Every content attribute is a raw ASC
/// enum string (e.g. `NONE` / `INFREQUENT_OR_MILD` / `FREQUENT_OR_INTENSE`)
/// passed through verbatim; the four `is_*` flags are booleans. All attributes
/// are optional and are `None` when absent from the response.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AgeRatingDeclarationInfo {
    pub id: String,
    pub alcohol_tobacco_or_drug_use_or_references: Option<String>,
    pub contests: Option<String>,
    pub gambling_simulated: Option<String>,
    pub guns_or_other_weapons: Option<String>,
    pub medical_or_treatment_information: Option<String>,
    pub profanity_or_crude_humor: Option<String>,
    pub sexual_content_graphic_and_nudity: Option<String>,
    pub sexual_content_or_nudity: Option<String>,
    pub horror_or_fear_themes: Option<String>,
    pub mature_or_suggestive_themes: Option<String>,
    pub violence_cartoon_or_fantasy: Option<String>,
    pub violence_realistic: Option<String>,
    pub violence_realistic_prolonged_graphic_or_sadistic: Option<String>,
    pub is_advertising: Option<bool>,
    pub is_gambling: Option<bool>,
    pub is_unrestricted_web_access: Option<bool>,
    pub is_user_generated_content: Option<bool>,
    pub age_rating_override_v2: Option<String>,
}

/// An App Store app category, with the ids of its subcategories. Deliberately
/// NON-recursive (UniFFI-friendly): `subcategory_ids` carries only the ids of the
/// nested subcategories, leaving the host to materialize the tree from a flat
/// list of [`AppCategoryInfo`] values if it needs the nesting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AppCategoryInfo {
    pub id: String,
    pub subcategory_ids: Vec<String>,
}

/// An App Store version localization, keyed by `locale`. Carries the per-locale
/// version listing metadata shown on the product page: `description`, `keywords`,
/// `promotional_text`, the `support_url`/`marketing_url` links, and the
/// `whats_new` release notes. App Store Connect serializes the URL/notes
/// attributes camelCase (`promotionalText`, `supportUrl`, `marketingUrl`,
/// `whatsNew`), which `rename_all = "camelCase"` maps without any per-field
/// rename. All attributes are optional and are `None` when absent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AppStoreLocalizationInfo {
    pub id: String,
    pub locale: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<String>,
    pub promotional_text: Option<String>,
    pub support_url: Option<String>,
    pub marketing_url: Option<String>,
    pub whats_new: Option<String>,
}

/// A set of App Store screenshots for a single device display type within a
/// version localization. `display_type` carries the raw ASC
/// `screenshotDisplayType` value (e.g. `APP_IPHONE_67`) passed through verbatim,
/// and `screenshots` lists the set's screenshots in the relationship order
/// reported by App Store Connect.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotSetInfo {
    pub id: String,
    pub display_type: Option<String>,
    pub screenshots: Vec<ScreenshotInfo>,
}

/// A single App Store screenshot. `image_url` is computed from the screenshot's
/// `imageAsset` template (`{w}`/`{h}`/`{f}` substituted, defaulting to 512/512/png),
/// exactly as the build icon URL is; it is `None` when no template URL is present.
/// `width`/`height` come from the `imageAsset` dimensions, and `file_name` /
/// `file_size` from the screenshot's own attributes. All optional fields are
/// `None` when the corresponding attribute is absent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotInfo {
    pub id: String,
    pub image_url: Option<String>,
    pub file_name: Option<String>,
    pub file_size: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// A TestFlight beta tester. `invite_type` and `state` are the raw ASC values
/// passed through verbatim; the core does no remapping.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct BetaTesterInfo {
    pub id: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub invite_type: Option<String>,
    pub state: Option<String>,
}

/// A team member of the connected App Store Connect account: the lightweight
/// projection of a `users` resource carrying only `first_name`/`last_name`, the
/// `username` (App Store Connect stores the member's login email here), and the
/// raw ASC `roles` strings (e.g. `"ADMIN"`, `"DEVELOPER"`, `"APP_MANAGER"`),
/// passed through verbatim without remapping. `first_name`/`last_name`/
/// `username` are optional; `roles` is empty when absent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct TeamMemberInfo {
    pub id: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub roles: Vec<String>,
}

/// A user of the connected App Store Connect account, unifying two ASC
/// resources: active members (`users`) and outstanding invitations
/// (`userInvitations`). `is_pending` discriminates the source — `false` for an
/// active member, `true` for a pending invitation. For active members `email`
/// is taken from the `username` attribute (App Store Connect stores the login
/// email there) and `expiration_date` is always `None`; for pending invitations
/// `email` is the invitation's own `email` and `expiration_date` is the raw
/// ISO8601 expiry (host owns parsing). `roles` carries the raw ASC role strings
/// verbatim; `all_apps_visible` and `provisioning_allowed` default to `false`
/// when the attribute is absent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct UserInfo {
    pub id: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
    pub roles: Vec<String>,
    pub all_apps_visible: bool,
    pub provisioning_allowed: bool,
    pub is_pending: bool,
    pub expiration_date: Option<String>,
}
