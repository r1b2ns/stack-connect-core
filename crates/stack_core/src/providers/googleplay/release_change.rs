//! The pure half of a Google Play track write: local validation of a
//! [`ReleaseChange`] and its application to a track's `releases[]`.
//!
//! `edits.tracks.update` replaces the whole track, so the releases are handled
//! as the raw JSON objects Google returned: a field the core does not model
//! survives the round trip instead of being silently dropped. No I/O happens
//! here; `track_writes` reads the track, calls [`apply`] and writes the result.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use super::error::local_rejection;
use crate::domain::{
    CountryTargetingInfo, LocalizedTextInfo, NewRelease, NewReleaseStatus, ReleaseChange,
    ReleaseMetadataUpdate,
};
use crate::error::StackError;
use crate::logging::target;

/// One `TrackRelease`, exactly as Google sent it (plus the core's edits).
pub(super) type Release = Map<String, Value>;

const STATUS_DRAFT: &str = "draft";
const STATUS_IN_PROGRESS: &str = "inProgress";
const STATUS_HALTED: &str = "halted";
const STATUS_COMPLETED: &str = "completed";

const KEY_STATUS: &str = "status";
const KEY_USER_FRACTION: &str = "userFraction";
const KEY_VERSION_CODES: &str = "versionCodes";
const KEY_NAME: &str = "name";
const KEY_RELEASE_NOTES: &str = "releaseNotes";
const KEY_PRIORITY: &str = "inAppUpdatePriority";
const KEY_COUNTRY_TARGETING: &str = "countryTargeting";

/// The documented range of `inAppUpdatePriority`.
const PRIORITY_RANGE: std::ops::RangeInclusive<i32> = 0..=5;

/// A locally rejected change, worded like the other local checks: reported as
/// [`StackError::Http`] `400` (what Google answers for an invalid argument), so
/// hosts handle it with Google's own validation errors (see
/// [`local_rejection`]).
fn invalid(message: impl std::fmt::Display) -> StackError {
    local_rejection(400, target::PLAY_TRACKS, message)
}

/// The set of version codes that identifies a release.
type VersionKey = BTreeSet<i64>;

/// Parses version codes a host sent: each must be a positive integer, none may
/// repeat, and there must be at least one.
fn input_key(codes: &[String]) -> Result<VersionKey, StackError> {
    if codes.is_empty() {
        return Err(invalid("a release needs at least one version code"));
    }
    let mut key = VersionKey::new();
    for code in codes {
        let value = code
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|v| *v > 0)
            .ok_or_else(|| invalid(format!("{code:?} is not a valid version code")))?;
        if !key.insert(value) {
            return Err(invalid(format!("version code {code} is listed twice")));
        }
    }
    Ok(key)
}

/// The key of a release Google returned; `None` when it has no version codes
/// or one that is not an integer (such a release can never be a target).
fn release_key(release: &Release) -> Option<VersionKey> {
    let codes = release.get(KEY_VERSION_CODES)?.as_array()?;
    let key = codes
        .iter()
        .map(|code| match code {
            Value::String(text) => text.trim().parse::<i64>().ok(),
            Value::Number(number) => number.as_i64(),
            _ => None,
        })
        .collect::<Option<VersionKey>>()?;
    (!key.is_empty()).then_some(key)
}

fn status_of(release: &Release) -> Option<&str> {
    release.get(KEY_STATUS).and_then(Value::as_str)
}

fn check_fraction(fraction: f64) -> Result<(), StackError> {
    // `!(0 < f < 1)` also rejects NaN.
    if fraction > 0.0 && fraction < 1.0 {
        Ok(())
    } else {
        Err(invalid(format!(
            "a staged rollout fraction must be greater than 0 and less than 1, got {fraction}"
        )))
    }
}

fn check_priority(priority: Option<i32>) -> Result<(), StackError> {
    match priority {
        Some(p) if !PRIORITY_RANGE.contains(&p) => Err(invalid(format!(
            "the in-app update priority must be between 0 and 5, got {p}"
        ))),
        _ => Ok(()),
    }
}

fn check_status(status: &NewReleaseStatus) -> Result<(), StackError> {
    match status {
        NewReleaseStatus::InProgress { user_fraction } => check_fraction(*user_fraction),
        NewReleaseStatus::Draft | NewReleaseStatus::Completed => Ok(()),
    }
}

/// Rejects, before any request, every value Google documents as invalid. Which
/// status transitions are allowed is left to `edits.validate`.
///
/// # Errors
/// [`StackError::Http`] `400` (local) naming the bad value.
pub(super) fn validate(change: &ReleaseChange, track: &str) -> Result<(), StackError> {
    match change {
        ReleaseChange::SetRollout {
            version_codes,
            user_fraction,
        } => {
            input_key(version_codes)?;
            check_fraction(*user_fraction)
        }
        ReleaseChange::Halt { version_codes }
        | ReleaseChange::Resume { version_codes }
        | ReleaseChange::Complete { version_codes } => input_key(version_codes).map(|_| ()),
        ReleaseChange::UpdateMetadata {
            version_codes,
            update,
        } => {
            input_key(version_codes)?;
            check_priority(update.in_app_update_priority)
        }
        ReleaseChange::CreateRelease { release } => {
            input_key(&release.version_codes)?;
            check_status(&release.status)?;
            check_priority(release.in_app_update_priority)
        }
        ReleaseChange::Promote {
            from_track,
            version_codes,
            status,
            in_app_update_priority,
            ..
        } => {
            if from_track.trim().is_empty() {
                return Err(invalid("promoting needs the track to promote from"));
            }
            if from_track == track {
                return Err(invalid(format!(
                    "cannot promote a release from track {track:?} to itself"
                )));
            }
            input_key(version_codes)?;
            check_status(status)?;
            check_priority(*in_app_update_priority)
        }
    }
}

fn not_found(key: &VersionKey, track: &str) -> StackError {
    let codes: Vec<String> = key.iter().map(i64::to_string).collect();
    StackError::release_not_found(format!(
        "No release with version codes [{}] is on track {track:?} any more: it may have \
         been replaced, completed or removed since the track was read. Nothing was \
         changed; reload the track.",
        codes.join(", ")
    ))
}

/// The index of the release identified by `version_codes` on `releases`.
fn find(releases: &[Release], version_codes: &[String], track: &str) -> Result<usize, StackError> {
    let key = input_key(version_codes)?;
    releases
        .iter()
        .position(|release| release_key(release).as_ref() == Some(&key))
        .ok_or_else(|| not_found(&key, track))
}

fn notes_value(notes: &[LocalizedTextInfo]) -> Value {
    Value::Array(
        notes
            .iter()
            .map(|note| json!({ "language": note.language, "text": note.text }))
            .collect(),
    )
}

fn targeting_value(targeting: &CountryTargetingInfo) -> Value {
    json!({
        "countries": targeting.countries,
        "includeRestOfWorld": targeting.include_rest_of_world,
    })
}

/// Sets `key` to `value`, or removes it when `value` is `None`.
fn set_or_remove(release: &mut Release, key: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            release.insert(key.to_string(), value);
        }
        None => {
            release.remove(key);
        }
    }
}

/// A non-blank name, or `None` (then Google derives one).
fn name_value(name: &str) -> Option<Value> {
    (!name.trim().is_empty()).then(|| Value::String(name.to_string()))
}

fn set_status(release: &mut Release, status: &NewReleaseStatus) {
    let (name, fraction) = match status {
        NewReleaseStatus::Draft => (STATUS_DRAFT, None),
        NewReleaseStatus::InProgress { user_fraction } => {
            (STATUS_IN_PROGRESS, Some(json!(user_fraction)))
        }
        NewReleaseStatus::Completed => (STATUS_COMPLETED, None),
    };
    release.insert(KEY_STATUS.to_string(), json!(name));
    set_or_remove(release, KEY_USER_FRACTION, fraction);
}

/// Makes `releases[index]` the completed release: status `completed`, no
/// fraction, and every other completed release dropped (superseded).
fn complete(releases: &mut Vec<Release>, index: usize) {
    set_status(&mut releases[index], &NewReleaseStatus::Completed);
    let mut position = 0;
    releases.retain(|release| {
        let keep = position == index || status_of(release) != Some(STATUS_COMPLETED);
        position += 1;
        keep
    });
}

fn apply_metadata(release: &mut Release, update: &ReleaseMetadataUpdate) {
    if let Some(name) = &update.name {
        set_or_remove(release, KEY_NAME, name_value(name));
    }
    if let Some(notes) = &update.release_notes {
        set_or_remove(
            release,
            KEY_RELEASE_NOTES,
            (!notes.is_empty()).then(|| notes_value(notes)),
        );
    }
    if let Some(priority) = update.in_app_update_priority {
        release.insert(KEY_PRIORITY.to_string(), json!(priority));
    }
    if let Some(targeting) = &update.country_targeting {
        release.insert(
            KEY_COUNTRY_TARGETING.to_string(),
            targeting_value(targeting),
        );
    }
}

/// Removes what a new release with `status` replaces (see [`NewRelease`]),
/// then puts it first.
fn add_release(releases: &mut Vec<Release>, release: Release, status: &NewReleaseStatus) {
    match status {
        NewReleaseStatus::Draft => releases.retain(|r| status_of(r) != Some(STATUS_DRAFT)),
        NewReleaseStatus::InProgress { .. } => releases.retain(|r| {
            !matches!(
                status_of(r),
                Some(STATUS_DRAFT | STATUS_IN_PROGRESS | STATUS_HALTED)
            )
        }),
        NewReleaseStatus::Completed => releases.clear(),
    }
    releases.insert(0, release);
}

fn new_release(release: &NewRelease) -> Result<Release, StackError> {
    let key = input_key(&release.version_codes)?;
    let mut object = Release::new();
    object.insert(
        KEY_VERSION_CODES.to_string(),
        // Google's int64 JSON encoding: decimal strings.
        Value::Array(key.iter().map(|code| json!(code.to_string())).collect()),
    );
    if let Some(name) = release.name.as_deref().and_then(name_value) {
        object.insert(KEY_NAME.to_string(), name);
    }
    if !release.release_notes.is_empty() {
        object.insert(
            KEY_RELEASE_NOTES.to_string(),
            notes_value(&release.release_notes),
        );
    }
    if let Some(priority) = release.in_app_update_priority {
        object.insert(KEY_PRIORITY.to_string(), json!(priority));
    }
    if let Some(targeting) = &release.country_targeting {
        object.insert(
            KEY_COUNTRY_TARGETING.to_string(),
            targeting_value(targeting),
        );
    }
    set_status(&mut object, &release.status);
    Ok(object)
}

/// The release [`ReleaseChange::Promote`] adds: the source's version codes
/// (verbatim), name, notes and priority, each unless overridden.
fn promoted_release(
    source: &Release,
    status: &NewReleaseStatus,
    name: Option<&str>,
    release_notes: Option<&[LocalizedTextInfo]>,
    in_app_update_priority: Option<i32>,
    country_targeting: Option<&CountryTargetingInfo>,
) -> Release {
    let mut object = Release::new();
    for key in [KEY_VERSION_CODES, KEY_NAME, KEY_RELEASE_NOTES, KEY_PRIORITY] {
        if let Some(value) = source.get(key) {
            object.insert(key.to_string(), value.clone());
        }
    }
    if let Some(name) = name {
        set_or_remove(&mut object, KEY_NAME, name_value(name));
    }
    if let Some(notes) = release_notes {
        set_or_remove(
            &mut object,
            KEY_RELEASE_NOTES,
            (!notes.is_empty()).then(|| notes_value(notes)),
        );
    }
    if let Some(priority) = in_app_update_priority {
        object.insert(KEY_PRIORITY.to_string(), json!(priority));
    }
    if let Some(targeting) = country_targeting {
        object.insert(
            KEY_COUNTRY_TARGETING.to_string(),
            targeting_value(targeting),
        );
    }
    set_status(&mut object, status);
    object
}

/// Applies `change` to `releases` (the target `track`'s releases, as read in
/// the edit). `source` is the promoted-from track's name and releases, which
/// [`ReleaseChange::Promote`] needs and every other change ignores.
///
/// # Errors
/// [`StackError::ReleaseNotFound`] when the targeted (or promoted) release is
/// not on its track; [`StackError::Http`] `400` (local) for an invalid value
/// (see [`validate`]); [`StackError::Decode`] when a `Promote` comes without
/// its source track (a caller bug).
pub(super) fn apply(
    change: &ReleaseChange,
    track: &str,
    mut releases: Vec<Release>,
    source: Option<(&str, &[Release])>,
) -> Result<Vec<Release>, StackError> {
    match change {
        ReleaseChange::SetRollout {
            version_codes,
            user_fraction,
        } => {
            check_fraction(*user_fraction)?;
            let index = find(&releases, version_codes, track)?;
            set_status(
                &mut releases[index],
                &NewReleaseStatus::InProgress {
                    user_fraction: *user_fraction,
                },
            );
        }
        ReleaseChange::Halt { version_codes } => {
            let index = find(&releases, version_codes, track)?;
            releases[index].insert(KEY_STATUS.to_string(), json!(STATUS_HALTED));
        }
        ReleaseChange::Resume { version_codes } => {
            let index = find(&releases, version_codes, track)?;
            let has_fraction = releases[index]
                .get(KEY_USER_FRACTION)
                .is_some_and(Value::is_number);
            if has_fraction {
                releases[index].insert(KEY_STATUS.to_string(), json!(STATUS_IN_PROGRESS));
            } else {
                complete(&mut releases, index);
            }
        }
        ReleaseChange::Complete { version_codes } => {
            let index = find(&releases, version_codes, track)?;
            complete(&mut releases, index);
        }
        ReleaseChange::UpdateMetadata {
            version_codes,
            update,
        } => {
            check_priority(update.in_app_update_priority)?;
            let index = find(&releases, version_codes, track)?;
            apply_metadata(&mut releases[index], update);
        }
        ReleaseChange::CreateRelease { release } => {
            check_status(&release.status)?;
            check_priority(release.in_app_update_priority)?;
            let object = new_release(release)?;
            add_release(&mut releases, object, &release.status);
        }
        ReleaseChange::Promote {
            from_track,
            version_codes,
            status,
            name,
            release_notes,
            in_app_update_priority,
            country_targeting,
        } => {
            check_status(status)?;
            check_priority(*in_app_update_priority)?;
            let (source_track, source_releases) = source
                .filter(|(name, _)| name == from_track)
                .ok_or_else(|| {
                    StackError::decode(format!("promote: track {from_track:?} was not read"))
                })?;
            let index = find(source_releases, version_codes, source_track)?;
            let object = promoted_release(
                &source_releases[index],
                status,
                name.as_deref(),
                release_notes.as_deref(),
                *in_app_update_priority,
                country_targeting.as_ref(),
            );
            add_release(&mut releases, object, status);
        }
    }
    Ok(releases)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn release(value: Value) -> Release {
        match value {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    fn releases(values: Value) -> Vec<Release> {
        match values {
            Value::Array(items) => items.into_iter().map(release).collect(),
            other => panic!("not an array: {other}"),
        }
    }

    fn as_json(releases: Vec<Release>) -> Value {
        Value::Array(releases.into_iter().map(Value::Object).collect())
    }

    /// A production track mid-rollout: 42 at 10 %, 40 completed, a draft 43.
    fn production() -> Vec<Release> {
        releases(json!([
            { "versionCodes": ["43"], "status": "draft", "name": "4.3" },
            { "versionCodes": ["42", "41"], "status": "inProgress", "userFraction": 0.1,
              "name": "4.2", "futureField": { "kept": true } },
            { "versionCodes": ["40"], "status": "completed", "name": "4.0" }
        ]))
    }

    fn not_found_message(err: StackError) -> String {
        match err {
            StackError::ReleaseNotFound { message } => message,
            other => panic!("expected ReleaseNotFound, got {other:?}"),
        }
    }

    fn local_400(err: StackError) -> String {
        match err {
            StackError::Http {
                status: 400,
                message,
            } => {
                assert!(message.starts_with("local validation failed: "));
                assert!(message.ends_with("; request not sent to Google"));
                message
            }
            other => panic!("expected a local Http 400, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Targeting (R7)
    // -----------------------------------------------------------------------

    #[test]
    fn targets_by_sorted_version_codes_ignoring_order_and_zeros() {
        let change = ReleaseChange::Halt {
            version_codes: codes(&["041", " 42"]),
        };
        let out = apply(&change, "production", production(), None).unwrap();
        assert_eq!(out[1]["status"], json!("halted"));
    }

    #[test]
    fn a_subset_or_superset_of_codes_is_not_the_release() {
        for target in [codes(&["42"]), codes(&["42", "41", "40"])] {
            let change = ReleaseChange::Halt {
                version_codes: target,
            };
            let message =
                not_found_message(apply(&change, "production", production(), None).unwrap_err());
            assert!(message.contains("on track \"production\" any more"));
        }
    }

    #[test]
    fn missing_release_names_codes_and_track() {
        let change = ReleaseChange::Complete {
            version_codes: codes(&["99", "7"]),
        };
        let message = not_found_message(apply(&change, "beta", production(), None).unwrap_err());
        assert!(message.starts_with("No release with version codes [7, 99] is on track \"beta\""));
    }

    #[test]
    fn numeric_version_codes_from_google_also_match() {
        let track = releases(json!([{ "versionCodes": [7], "status": "inProgress",
                                      "userFraction": 0.5 }]));
        let change = ReleaseChange::Halt {
            version_codes: codes(&["7"]),
        };
        assert_eq!(
            apply(&change, "beta", track, None).unwrap()[0]["status"],
            json!("halted")
        );
    }

    // -----------------------------------------------------------------------
    // Rollout mutations
    // -----------------------------------------------------------------------

    #[test]
    fn set_rollout_changes_only_the_target_and_keeps_unknown_fields() {
        let change = ReleaseChange::SetRollout {
            version_codes: codes(&["41", "42"]),
            user_fraction: 0.5,
        };
        let out = apply(&change, "production", production(), None).unwrap();
        assert_eq!(
            as_json(out),
            json!([
                { "versionCodes": ["43"], "status": "draft", "name": "4.3" },
                { "versionCodes": ["42", "41"], "status": "inProgress", "userFraction": 0.5,
                  "name": "4.2", "futureField": { "kept": true } },
                { "versionCodes": ["40"], "status": "completed", "name": "4.0" }
            ])
        );
    }

    #[test]
    fn set_rollout_starts_a_draft() {
        let change = ReleaseChange::SetRollout {
            version_codes: codes(&["43"]),
            user_fraction: 0.05,
        };
        let out = apply(&change, "production", production(), None).unwrap();
        assert_eq!(
            Value::Object(out[0].clone()),
            json!({ "versionCodes": ["43"], "status": "inProgress", "userFraction": 0.05,
                    "name": "4.3" })
        );
    }

    #[test]
    fn halt_keeps_the_fraction() {
        let change = ReleaseChange::Halt {
            version_codes: codes(&["42", "41"]),
        };
        let out = apply(&change, "production", production(), None).unwrap();
        assert_eq!(out[1]["status"], json!("halted"));
        assert_eq!(out[1]["userFraction"], json!(0.1));
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn resume_with_a_fraction_goes_back_in_progress() {
        let track = releases(json!([
            { "versionCodes": ["42"], "status": "halted", "userFraction": 0.2 },
            { "versionCodes": ["40"], "status": "completed" }
        ]));
        let change = ReleaseChange::Resume {
            version_codes: codes(&["42"]),
        };
        assert_eq!(
            as_json(apply(&change, "production", track, None).unwrap()),
            json!([
                { "versionCodes": ["42"], "status": "inProgress", "userFraction": 0.2 },
                { "versionCodes": ["40"], "status": "completed" }
            ])
        );
    }

    #[test]
    fn resume_without_a_fraction_completes_and_supersedes() {
        let track = releases(json!([
            { "versionCodes": ["42"], "status": "halted" },
            { "versionCodes": ["40"], "status": "completed" }
        ]));
        let change = ReleaseChange::Resume {
            version_codes: codes(&["42"]),
        };
        assert_eq!(
            as_json(apply(&change, "beta", track, None).unwrap()),
            json!([{ "versionCodes": ["42"], "status": "completed" }])
        );
    }

    #[test]
    fn complete_drops_the_fraction_and_the_superseded_completed_release() {
        let change = ReleaseChange::Complete {
            version_codes: codes(&["42", "41"]),
        };
        let out = apply(&change, "production", production(), None).unwrap();
        assert_eq!(
            as_json(out),
            json!([
                { "versionCodes": ["43"], "status": "draft", "name": "4.3" },
                { "versionCodes": ["42", "41"], "status": "completed", "name": "4.2",
                  "futureField": { "kept": true } }
            ])
        );
    }

    #[test]
    fn complete_of_the_only_release_keeps_it() {
        let track = releases(json!([{ "versionCodes": ["1"], "status": "draft" }]));
        let change = ReleaseChange::Complete {
            version_codes: codes(&["1"]),
        };
        assert_eq!(
            as_json(apply(&change, "internal", track, None).unwrap()),
            json!([{ "versionCodes": ["1"], "status": "completed" }])
        );
    }

    // -----------------------------------------------------------------------
    // Metadata
    // -----------------------------------------------------------------------

    #[test]
    fn update_metadata_sets_only_the_given_fields() {
        let change = ReleaseChange::UpdateMetadata {
            version_codes: codes(&["40"]),
            update: ReleaseMetadataUpdate {
                name: Some("4.0 final".into()),
                release_notes: Some(vec![LocalizedTextInfo {
                    language: "en-US".into(),
                    text: "Fixes".into(),
                }]),
                in_app_update_priority: Some(5),
                country_targeting: Some(CountryTargetingInfo {
                    countries: vec!["BR".into()],
                    include_rest_of_world: true,
                }),
            },
        };
        let out = apply(&change, "production", production(), None).unwrap();
        assert_eq!(
            Value::Object(out[2].clone()),
            json!({
                "versionCodes": ["40"], "status": "completed", "name": "4.0 final",
                "releaseNotes": [{ "language": "en-US", "text": "Fixes" }],
                "inAppUpdatePriority": 5,
                "countryTargeting": { "countries": ["BR"], "includeRestOfWorld": true }
            })
        );
        // Nothing else moved.
        assert_eq!(out[0], production()[0]);
        assert_eq!(out[1], production()[1]);
    }

    #[test]
    fn update_metadata_blank_name_and_empty_notes_remove_them() {
        let track = releases(
            json!([{ "versionCodes": ["3"], "status": "draft", "name": "x",
            "releaseNotes": [{ "language": "en-US", "text": "old" }], "inAppUpdatePriority": 2 }]),
        );
        let change = ReleaseChange::UpdateMetadata {
            version_codes: codes(&["3"]),
            update: ReleaseMetadataUpdate {
                name: Some("  ".into()),
                release_notes: Some(vec![]),
                ..ReleaseMetadataUpdate::default()
            },
        };
        assert_eq!(
            as_json(apply(&change, "beta", track, None).unwrap()),
            json!([{ "versionCodes": ["3"], "status": "draft", "inAppUpdatePriority": 2 }])
        );
    }

    #[test]
    fn update_metadata_with_nothing_set_changes_nothing() {
        let change = ReleaseChange::UpdateMetadata {
            version_codes: codes(&["43"]),
            update: ReleaseMetadataUpdate::default(),
        };
        assert_eq!(
            apply(&change, "production", production(), None).unwrap(),
            production()
        );
    }

    // -----------------------------------------------------------------------
    // New releases
    // -----------------------------------------------------------------------

    fn new(status: NewReleaseStatus) -> ReleaseChange {
        ReleaseChange::CreateRelease {
            release: NewRelease {
                version_codes: codes(&["45", "044"]),
                name: Some("4.5".into()),
                release_notes: vec![LocalizedTextInfo {
                    language: "pt-BR".into(),
                    text: "Novidades".into(),
                }],
                status,
                in_app_update_priority: Some(1),
                country_targeting: None,
            },
        }
    }

    #[test]
    fn create_draft_replaces_only_the_draft() {
        let out = apply(
            &new(NewReleaseStatus::Draft),
            "production",
            production(),
            None,
        )
        .unwrap();
        assert_eq!(
            Value::Object(out[0].clone()),
            json!({
                "versionCodes": ["44", "45"], "name": "4.5",
                "releaseNotes": [{ "language": "pt-BR", "text": "Novidades" }],
                "inAppUpdatePriority": 1, "status": "draft"
            })
        );
        assert_eq!(&out[1..], &production()[1..]);
    }

    #[test]
    fn create_in_progress_replaces_draft_and_rollout_but_keeps_completed() {
        let out = apply(
            &new(NewReleaseStatus::InProgress { user_fraction: 0.2 }),
            "production",
            production(),
            None,
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["status"], json!("inProgress"));
        assert_eq!(out[0]["userFraction"], json!(0.2));
        assert_eq!(out[1], production()[2]);
    }

    #[test]
    fn create_completed_replaces_everything() {
        let out = apply(
            &new(NewReleaseStatus::Completed),
            "production",
            production(),
            None,
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["status"], json!("completed"));
        assert!(out[0].get("userFraction").is_none());
    }

    #[test]
    fn create_on_an_empty_track_and_blank_name_is_left_to_google() {
        let change = ReleaseChange::CreateRelease {
            release: NewRelease {
                version_codes: codes(&["1"]),
                name: Some(" ".into()),
                release_notes: vec![],
                status: NewReleaseStatus::Draft,
                in_app_update_priority: None,
                country_targeting: Some(CountryTargetingInfo {
                    countries: vec!["PT".into()],
                    include_rest_of_world: false,
                }),
            },
        };
        assert_eq!(
            as_json(apply(&change, "internal", vec![], None).unwrap()),
            json!([{ "versionCodes": ["1"], "status": "draft",
                     "countryTargeting": { "countries": ["PT"], "includeRestOfWorld": false } }])
        );
    }

    // -----------------------------------------------------------------------
    // Promote
    // -----------------------------------------------------------------------

    fn beta() -> Vec<Release> {
        releases(json!([{
            "versionCodes": ["42", "41"], "status": "completed", "name": "4.2",
            "releaseNotes": [{ "language": "en-US", "text": "New" }],
            "inAppUpdatePriority": 3,
            "countryTargeting": { "countries": ["BR"] },
            "futureField": 1
        }]))
    }

    fn promote(status: NewReleaseStatus) -> ReleaseChange {
        promote_from("beta", &["41", "42"], status)
    }

    fn promote_from(from: &str, version_codes: &[&str], status: NewReleaseStatus) -> ReleaseChange {
        ReleaseChange::Promote {
            from_track: from.into(),
            version_codes: codes(version_codes),
            status,
            name: None,
            release_notes: None,
            in_app_update_priority: None,
            country_targeting: None,
        }
    }

    #[test]
    fn promote_copies_codes_name_notes_and_priority_only() {
        let beta = beta();
        let target = releases(json!([{ "versionCodes": ["40"], "status": "completed" }]));
        let out = apply(
            &promote(NewReleaseStatus::InProgress { user_fraction: 0.1 }),
            "production",
            target,
            Some(("beta", &beta)),
        )
        .unwrap();
        assert_eq!(
            as_json(out),
            json!([
                { "versionCodes": ["42", "41"], "name": "4.2",
                  "releaseNotes": [{ "language": "en-US", "text": "New" }],
                  "inAppUpdatePriority": 3, "status": "inProgress", "userFraction": 0.1 },
                { "versionCodes": ["40"], "status": "completed" }
            ])
        );
    }

    #[test]
    fn promote_overrides_win() {
        let beta = beta();
        let change = ReleaseChange::Promote {
            from_track: "beta".into(),
            version_codes: codes(&["41", "42"]),
            status: NewReleaseStatus::Completed,
            name: Some("Prod 4.2".into()),
            release_notes: Some(vec![]),
            in_app_update_priority: Some(0),
            country_targeting: Some(CountryTargetingInfo {
                countries: vec!["US".into()],
                include_rest_of_world: true,
            }),
        };
        let out = apply(&change, "production", vec![], Some(("beta", &beta))).unwrap();
        assert_eq!(
            as_json(out),
            json!([{ "versionCodes": ["42", "41"], "name": "Prod 4.2",
                     "inAppUpdatePriority": 0, "status": "completed",
                     "countryTargeting": { "countries": ["US"], "includeRestOfWorld": true } }])
        );
    }

    #[test]
    fn promote_of_a_missing_release_names_the_source_track() {
        let change = promote_from("beta", &["9"], NewReleaseStatus::Draft);
        let beta = beta();
        let message = not_found_message(
            apply(&change, "production", vec![], Some(("beta", &beta))).unwrap_err(),
        );
        assert!(message.contains("on track \"beta\""));
    }

    #[test]
    fn promote_without_its_source_track_is_decode() {
        let err = apply(
            &promote(NewReleaseStatus::Draft),
            "production",
            vec![],
            Some(("alpha", &[])),
        )
        .unwrap_err();
        assert!(matches!(err, StackError::Decode { .. }));
        let err = apply(
            &promote(NewReleaseStatus::Draft),
            "production",
            vec![],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, StackError::Decode { .. }));
    }

    // -----------------------------------------------------------------------
    // Local validation
    // -----------------------------------------------------------------------

    #[test]
    fn rollout_fraction_must_be_strictly_between_zero_and_one() {
        for fraction in [
            0.0,
            1.0,
            -0.1,
            1.5,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            let change = ReleaseChange::SetRollout {
                version_codes: codes(&["1"]),
                user_fraction: fraction,
            };
            let message = local_400(validate(&change, "production").unwrap_err());
            assert!(
                message.contains("greater than 0 and less than 1"),
                "{fraction}"
            );
            // `apply` enforces it as well.
            local_400(apply(&change, "production", production(), None).unwrap_err());

            let created = new(NewReleaseStatus::InProgress {
                user_fraction: fraction,
            });
            local_400(validate(&created, "production").unwrap_err());
            local_400(
                validate(
                    &promote(NewReleaseStatus::InProgress {
                        user_fraction: fraction,
                    }),
                    "production",
                )
                .unwrap_err(),
            );
        }
        for fraction in [f64::MIN_POSITIVE, 0.000_1, 0.5, 0.999_9] {
            let change = ReleaseChange::SetRollout {
                version_codes: codes(&["1"]),
                user_fraction: fraction,
            };
            validate(&change, "production").unwrap();
        }
    }

    #[test]
    fn version_codes_must_be_distinct_positive_integers() {
        for bad in [
            codes(&[]),
            codes(&["0"]),
            codes(&["-3"]),
            codes(&["4x"]),
            codes(&[""]),
            codes(&["9223372036854775808"]),
            codes(&["5", "05"]),
        ] {
            let change = ReleaseChange::Halt {
                version_codes: bad.clone(),
            };
            local_400(validate(&change, "beta").unwrap_err());
        }
        validate(
            &ReleaseChange::Halt {
                version_codes: codes(&["9223372036854775807", "1"]),
            },
            "beta",
        )
        .unwrap();
    }

    #[test]
    fn priority_must_be_zero_to_five() {
        for (priority, ok) in [(-1, false), (0, true), (5, true), (6, false)] {
            let change = ReleaseChange::UpdateMetadata {
                version_codes: codes(&["1"]),
                update: ReleaseMetadataUpdate {
                    in_app_update_priority: Some(priority),
                    ..ReleaseMetadataUpdate::default()
                },
            };
            assert_eq!(validate(&change, "beta").is_ok(), ok, "{priority}");
        }
    }

    #[test]
    fn promote_needs_another_source_track() {
        let to_itself = promote_from("production", &["1"], NewReleaseStatus::Draft);
        assert!(local_400(validate(&to_itself, "production").unwrap_err()).contains("to itself"));
        let blank = promote_from(" ", &["1"], NewReleaseStatus::Draft);
        local_400(validate(&blank, "production").unwrap_err());
        validate(&promote(NewReleaseStatus::Draft), "production").unwrap();
    }
}
