use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;

use super::service_account::ServiceAccount;
use crate::error::StackError;

#[derive(Serialize)]
struct Claims {
    iss: String,
    scope: String,
    aud: String,
    iat: i64,
    exp: i64,
}

/// JWT lifetime cap, matching the Swift `min(expirationDuration, 3600)`.
const MAX_TTL_SECS: i64 = 3600;

/// Builds the RS256-signed JWT assertion for the OAuth2 jwt-bearer grant.
///
/// `now` is the Unix time in seconds, injected for deterministic tests. Unlike the
/// Swift implementation (manual ASN.1 PKCS#8→PKCS#1 stripping + `SecKeyCreateSignature`),
/// `EncodingKey::from_rsa_pem` accepts the raw PKCS#1/PKCS#8 PEM directly.
pub(crate) fn signed_assertion(
    account: &ServiceAccount,
    scopes: &[String],
    now: i64,
) -> Result<String, StackError> {
    let claims = Claims {
        iss: account.client_email.clone(),
        scope: scopes.join(" "),
        aud: account.token_uri.clone(),
        iat: now,
        exp: now + MAX_TTL_SECS,
    };

    let mut header = Header::new(Algorithm::RS256);
    header.typ = Some("JWT".to_string());
    header.kid = Some(account.private_key_id.clone());

    let key = EncodingKey::from_rsa_pem(account.private_key.as_bytes())
        .map_err(|e| StackError::auth(format!("invalid RSA private key: {e}")))?;

    encode(&header, &claims, &key).map_err(|e| StackError::auth(format!("JWT signing failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{decode, DecodingKey, Validation};

    #[derive(serde::Deserialize)]
    struct Decoded {
        iss: String,
        scope: String,
        aud: String,
        iat: i64,
        exp: i64,
    }

    fn test_account() -> ServiceAccount {
        ServiceAccount {
            private_key_id: "test-kid".into(),
            private_key: include_str!("../../tests/fixtures/test_rsa_private.pem").into(),
            client_email: "svc@example.iam.gserviceaccount.com".into(),
            token_uri: "https://oauth2.googleapis.com/token".into(),
        }
    }

    #[test]
    fn signs_verifiable_rs256_assertion() {
        let scopes = vec!["https://www.googleapis.com/auth/androidpublisher".to_string()];
        let now = 1_700_000_000;
        let jwt = signed_assertion(&test_account(), &scopes, now).unwrap();

        let header = jsonwebtoken::decode_header(&jwt).unwrap();
        assert_eq!(header.alg, Algorithm::RS256);
        assert_eq!(header.typ.as_deref(), Some("JWT"));
        assert_eq!(header.kid.as_deref(), Some("test-kid"));

        let pubkey = DecodingKey::from_rsa_pem(
            include_str!("../../tests/fixtures/test_rsa_public.pem").as_bytes(),
        )
        .unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&["https://oauth2.googleapis.com/token"]);
        validation.validate_exp = false; // fixed `now` is in the past

        let data = decode::<Decoded>(&jwt, &pubkey, &validation).unwrap();
        assert_eq!(data.claims.iss, "svc@example.iam.gserviceaccount.com");
        assert_eq!(data.claims.aud, "https://oauth2.googleapis.com/token");
        assert_eq!(
            data.claims.scope,
            "https://www.googleapis.com/auth/androidpublisher"
        );
        assert_eq!(data.claims.iat, now);
        assert_eq!(data.claims.exp, now + 3600);
    }
}
