//! Generic sync orchestrator over the [`BlobStore`] port. Pulls entities from a
//! connected [`Provider`] and persists each as a JSON blob through the
//! host-implemented store, keeping the core stateless. This first slice syncs
//! apps; reviews/builds will be added the same way (one method, one blob type).

use std::sync::Arc;

use crate::domain::AppInfo;
use crate::error::StackError;
use crate::ports::BlobStore;
use crate::service::provider::Provider;

/// Stable [`BlobStore`] `type_name` for persisted apps. The host (iOS) maps this
/// string to its SwiftData entity. Keep in sync with the iOS `PersistentStorable`
/// mapping.
pub(crate) const BLOB_TYPE_APP: &str = "app";

/// Serialize-only view of an [`AppInfo`] plus the owning account, persisted as the
/// AppModel-compatible base blob. Emits exactly the base fields the core owns plus
/// `accountId`, in the iOS-facing camelCase contract:
/// `{"id","name","bundleId","platform","accountId"}`.
///
/// The Swift adapter MERGES these base fields into its rich `AppModel`, preserving
/// enrichment/user-owned fields, and builds the iOS composite key
/// `"<accountId>.<appId>"` itself from the `accountId` carried in this JSON. The
/// core therefore keys the blob by the bare app id, never a composite.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppBlob<'a> {
    id: &'a str,
    name: &'a str,
    bundle_id: &'a str,
    platform: Option<&'a str>,
    account_id: &'a str,
}

/// Generic sync orchestrator: pulls from a connected [`Provider`] and persists
/// each entity as a JSON blob through the host [`BlobStore`]. The core stays
/// stateless — all persistence lives behind the foreign trait.
///
/// Reached from Swift via [`crate::facade::make_sync_service`].
#[derive(uniffi::Object)]
pub struct SyncService {
    provider: Arc<Provider>,
    store: Arc<dyn BlobStore>,
    account_id: String,
}

impl SyncService {
    /// Wires a connected provider to the host store for the given `account_id`.
    /// Synchronous; the returned object does the async work.
    pub(crate) fn new(
        provider: Arc<Provider>,
        store: Arc<dyn BlobStore>,
        account_id: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            provider,
            store,
            account_id,
        })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl SyncService {
    /// Fetches every visible app and persists each as an AppModel-compatible base
    /// blob under [`BLOB_TYPE_APP`], keyed by the bare app id (never a composite
    /// key). Each blob carries `{id,name,bundleId,platform,accountId}` — the base
    /// fields the core owns plus this service's `account_id`. Returns the fetched
    /// apps so the host can drive post-sync enrichment without re-fetching.
    ///
    /// The Swift side merges these base fields into its rich `AppModel`, preserving
    /// enrichment/user-owned fields, and derives the iOS composite key
    /// `"<accountId>.<appId>"` from the `accountId` carried in the JSON.
    ///
    /// # Errors
    /// Propagates whatever [`Provider::fetch_apps`] returns (HTTP/Decode/Network),
    /// or [`StackError::Decode`] if an app fails to serialize.
    pub async fn sync_apps(&self) -> Result<Vec<AppInfo>, StackError> {
        let apps = self.provider.fetch_apps().await?;
        for app in &apps {
            let blob = AppBlob {
                id: &app.id,
                name: &app.name,
                bundle_id: &app.bundle_id,
                platform: app.platform.as_deref(),
                account_id: &self.account_id,
            };
            let json = serde_json::to_string(&blob)
                .map_err(|e| StackError::decode(format!("serialize app {}: {e}", app.id)))?;
            self.store
                .save(BLOB_TYPE_APP.to_string(), app.id.clone(), json);
        }
        Ok(apps)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::service::kind::ServiceKind;
    use crate::service::provider::{Capability, ProviderImpl};

    /// In-memory [`BlobStore`] backed by a `Mutex<HashMap>`, so tests can assert
    /// exactly what the sync loop persisted with no host involvement.
    #[derive(Default)]
    struct InMemoryStore {
        blobs: Mutex<HashMap<(String, String), String>>,
    }

    impl BlobStore for InMemoryStore {
        fn save(&self, type_name: String, id: String, json: String) {
            self.blobs.lock().unwrap().insert((type_name, id), json);
        }

        fn fetch(&self, type_name: String, id: String) -> Option<String> {
            self.blobs.lock().unwrap().get(&(type_name, id)).cloned()
        }

        fn fetch_all(&self, type_name: String) -> Vec<String> {
            self.blobs
                .lock()
                .unwrap()
                .iter()
                .filter(|((t, _), _)| *t == type_name)
                .map(|(_, json)| json.clone())
                .collect()
        }

        fn delete(&self, type_name: String, id: String) {
            self.blobs.lock().unwrap().remove(&(type_name, id));
        }
    }

    /// A [`ProviderImpl`] that returns canned apps with no network, so the sync
    /// loop can be driven deterministically.
    struct FakeProvider {
        apps: Vec<AppInfo>,
    }

    #[async_trait]
    impl ProviderImpl for FakeProvider {
        fn kind(&self) -> ServiceKind {
            ServiceKind::AppStoreConnect
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability::Apps]
        }

        async fn validate(&self) -> Result<(), StackError> {
            Ok(())
        }

        async fn fetch_apps(&self) -> Result<Vec<AppInfo>, StackError> {
            Ok(self.apps.clone())
        }
    }

    fn app(id: &str, name: &str, bundle_id: &str) -> AppInfo {
        AppInfo {
            id: id.to_string(),
            name: name.to_string(),
            bundle_id: bundle_id.to_string(),
            platform: Some("IOS".to_string()),
        }
    }

    const ACCOUNT_ID: &str = "acct-1";

    fn service_with(apps: Vec<AppInfo>) -> (Arc<SyncService>, Arc<InMemoryStore>) {
        let provider = Provider::new(Box::new(FakeProvider { apps }));
        let store = Arc::new(InMemoryStore::default());
        let svc = SyncService::new(provider, store.clone(), ACCOUNT_ID.to_string());
        (svc, store)
    }

    #[tokio::test]
    async fn returns_fetched_apps_and_persists_each_keyed_by_id() {
        let apps = vec![app("1", "Foo", "com.foo"), app("2", "Bar", "com.bar")];
        let (svc, store) = service_with(apps.clone());

        let returned = svc.sync_apps().await.expect("sync should succeed");

        // The host drives enrichment from the returned list without re-fetching.
        assert_eq!(returned, apps);
        assert_eq!(store.fetch_all(BLOB_TYPE_APP.to_string()).len(), 2);

        // Keyed by the bare app id, not a composite "<accountId>.<appId>".
        assert!(store
            .fetch(BLOB_TYPE_APP.to_string(), "acct-1.1".to_string())
            .is_none());
        let blob = store
            .fetch(BLOB_TYPE_APP.to_string(), "1".to_string())
            .expect("app 1 should be persisted");

        // The persisted JSON uses the iOS-facing camelCase contract and carries
        // the owning account id.
        assert!(blob.contains("\"bundleId\":\"com.foo\""));
        assert!(blob.contains("\"accountId\":\"acct-1\""));
        assert!(!blob.contains("bundle_id"));
        assert!(!blob.contains("account_id"));
    }

    #[tokio::test]
    async fn persisted_blob_has_exactly_the_appmodel_base_fields() {
        let (svc, store) = service_with(vec![app("42", "Answer", "com.answer")]);

        svc.sync_apps().await.expect("sync should succeed");

        let blob = store
            .fetch(BLOB_TYPE_APP.to_string(), "42".to_string())
            .expect("app 42 should be persisted");
        let value: serde_json::Value =
            serde_json::from_str(&blob).expect("blob should be valid JSON");
        let obj = value.as_object().expect("blob should be a JSON object");

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["accountId", "bundleId", "id", "name", "platform"]);

        assert_eq!(obj["id"], "42");
        assert_eq!(obj["name"], "Answer");
        assert_eq!(obj["bundleId"], "com.answer");
        assert_eq!(obj["platform"], "IOS");
        assert_eq!(obj["accountId"], "acct-1");
    }

    #[tokio::test]
    async fn upsert_replaces_existing_blob_for_same_id() {
        // Second pass with a renamed app under the same id must overwrite, not
        // duplicate.
        let (svc1, store) = service_with(vec![app("1", "Old", "com.app")]);
        svc1.sync_apps().await.expect("first sync should succeed");

        let provider = Provider::new(Box::new(FakeProvider {
            apps: vec![app("1", "New", "com.app")],
        }));
        let svc2 = SyncService::new(provider, store.clone(), ACCOUNT_ID.to_string());
        let returned = svc2.sync_apps().await.expect("second sync should succeed");

        assert_eq!(returned.len(), 1);
        assert_eq!(store.fetch_all(BLOB_TYPE_APP.to_string()).len(), 1);
        let blob = store
            .fetch(BLOB_TYPE_APP.to_string(), "1".to_string())
            .expect("app 1 should be persisted");
        assert!(blob.contains("\"name\":\"New\""));
    }

    #[tokio::test]
    async fn empty_provider_persists_nothing() {
        let (svc, store) = service_with(vec![]);

        let returned = svc.sync_apps().await.expect("sync should succeed");

        assert!(returned.is_empty());
        assert!(store.fetch_all(BLOB_TYPE_APP.to_string()).is_empty());
    }
}
