use serde::Deserialize;

use crate::error::StackError;

/// Subset of a Google service-account JSON needed to mint OAuth tokens. Unknown
/// fields (`type`, `project_id`, `client_id`, cert URLs, …) are ignored by serde.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ServiceAccount {
    pub(crate) private_key_id: String,
    pub(crate) private_key: String,
    pub(crate) client_email: String,
    #[serde(default = "default_token_uri")]
    pub(crate) token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

impl ServiceAccount {
    pub(crate) fn from_json(json: &str) -> Result<Self, StackError> {
        serde_json::from_str(json)
            .map_err(|e| StackError::invalid_credentials(format!("service-account JSON: {e}")))
    }
}
