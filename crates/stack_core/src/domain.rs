/// Cross-provider app metadata. Mirrors the Swift `AppInfo` from `StackProtocols`,
/// which is dissolved into the core (see RUST_CORE_PLAN.md §4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, uniffi::Record)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub bundle_id: String,
    pub platform: Option<String>,
}
