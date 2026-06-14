//! Generic sync orchestrator over the [`BlobStore`] port. Pulls entities from a
//! connected [`Provider`] and persists each as a JSON blob through the
//! host-implemented store, keeping the core stateless. This first slice syncs
//! apps; reviews/builds will be added the same way (one method, one blob type).

use std::sync::Arc;

use crate::error::StackError;
use crate::ports::BlobStore;
use crate::service::provider::Provider;

/// Stable [`BlobStore`] `type_name` for persisted apps. The host (iOS) maps this
/// string to its SwiftData entity. Keep in sync with the iOS `PersistentStorable`
/// mapping.
pub(crate) const BLOB_TYPE_APP: &str = "app";

/// Outcome of a sync pass. Grows as more capabilities are synced.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SyncSummary {
    /// How many apps were upserted into the store.
    pub apps_synced: u32,
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
}

impl SyncService {
    /// Wires a connected provider to the host store. Synchronous; the returned
    /// object does the async work.
    pub(crate) fn new(provider: Arc<Provider>, store: Arc<dyn BlobStore>) -> Arc<Self> {
        Arc::new(Self { provider, store })
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl SyncService {
    /// Fetches every visible app and upserts each as a JSON blob under
    /// [`BLOB_TYPE_APP`], keyed by app id. Returns how many were persisted.
    ///
    /// # Errors
    /// Propagates whatever [`Provider::fetch_apps`] returns (HTTP/Decode/Network),
    /// or [`StackError::Decode`] if an app fails to serialize.
    pub async fn sync_apps(&self) -> Result<SyncSummary, StackError> {
        let apps = self.provider.fetch_apps().await?;
        let mut count: u32 = 0;
        for app in &apps {
            let json = serde_json::to_string(app)
                .map_err(|e| StackError::decode(format!("serialize app {}: {e}", app.id)))?;
            self.store
                .save(BLOB_TYPE_APP.to_string(), app.id.clone(), json);
            count += 1;
        }
        Ok(SyncSummary { apps_synced: count })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::domain::AppInfo;
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

    fn service_with(apps: Vec<AppInfo>) -> (Arc<SyncService>, Arc<InMemoryStore>) {
        let provider = Provider::new(Box::new(FakeProvider { apps }));
        let store = Arc::new(InMemoryStore::default());
        let svc = SyncService::new(provider, store.clone());
        (svc, store)
    }

    #[tokio::test]
    async fn persists_each_app_under_app_type_keyed_by_id() {
        let (svc, store) =
            service_with(vec![app("1", "Foo", "com.foo"), app("2", "Bar", "com.bar")]);

        let summary = svc.sync_apps().await.expect("sync should succeed");

        assert_eq!(summary, SyncSummary { apps_synced: 2 });
        assert_eq!(store.fetch_all(BLOB_TYPE_APP.to_string()).len(), 2);

        let blob = store
            .fetch(BLOB_TYPE_APP.to_string(), "1".to_string())
            .expect("app 1 should be persisted");
        // The persisted JSON must use the iOS-facing camelCase contract.
        assert!(blob.contains("\"bundleId\":\"com.foo\""));
        assert!(!blob.contains("bundle_id"));
    }

    #[tokio::test]
    async fn round_trips_through_appinfo_serde() {
        let original = app("42", "Answer", "com.answer");
        let (svc, store) = service_with(vec![original.clone()]);

        svc.sync_apps().await.expect("sync should succeed");

        let blob = store
            .fetch(BLOB_TYPE_APP.to_string(), "42".to_string())
            .expect("app 42 should be persisted");
        let decoded: AppInfo = serde_json::from_str(&blob).expect("blob should decode as AppInfo");
        assert_eq!(decoded, original);
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
        let svc2 = SyncService::new(provider, store.clone());
        let summary = svc2.sync_apps().await.expect("second sync should succeed");

        assert_eq!(summary, SyncSummary { apps_synced: 1 });
        assert_eq!(store.fetch_all(BLOB_TYPE_APP.to_string()).len(), 1);
        let blob = store
            .fetch(BLOB_TYPE_APP.to_string(), "1".to_string())
            .expect("app 1 should be persisted");
        assert!(blob.contains("\"name\":\"New\""));
    }

    #[tokio::test]
    async fn empty_provider_persists_nothing() {
        let (svc, store) = service_with(vec![]);

        let summary = svc.sync_apps().await.expect("sync should succeed");

        assert_eq!(summary, SyncSummary { apps_synced: 0 });
        assert!(store.fetch_all(BLOB_TYPE_APP.to_string()).is_empty());
    }
}
