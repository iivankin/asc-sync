use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAuthRecord {
    #[serde(default)]
    pub display_name: String,
    pub issuer_id: String,
    pub key_id: String,
    pub private_key_pem: String,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    issuer_id: String,
    key_id: String,
    encoding_key: EncodingKey,
}

#[derive(Debug, Serialize)]
struct Claims<'a> {
    iss: &'a str,
    iat: u64,
    exp: u64,
    aud: &'static str,
}

impl StoredAuthRecord {
    pub fn into_context(self) -> Result<AuthContext> {
        AuthContext::from_pem(
            self.issuer_id,
            self.key_id,
            self.private_key_pem.into_bytes(),
        )
    }
}

impl AuthContext {
    pub fn from_pem(
        issuer_id: impl Into<String>,
        key_id: impl Into<String>,
        private_key_pem: impl Into<Vec<u8>>,
    ) -> Result<Self> {
        let encoding_key = EncodingKey::from_ec_pem(&private_key_pem.into())
            .context("failed to parse App Store Connect private key as PKCS#8 EC key")?;

        Ok(Self {
            issuer_id: issuer_id.into(),
            key_id: key_id.into(),
            encoding_key,
        })
    }

    pub fn bearer_token(&self) -> Result<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX_EPOCH")?
            .as_secs();
        let claims = Claims {
            iss: &self.issuer_id,
            iat: now,
            exp: now + Duration::from_secs(300).as_secs(),
            aud: "appstoreconnect-v1",
        };

        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(self.key_id.clone());

        encode(&header, &claims, &self.encoding_key).context("failed to sign ASC JWT")
    }
}

pub fn read_private_key_pem(path: &Path) -> Result<Vec<u8>> {
    let resolved = resolve_input_path(path)?;
    fs::read(&resolved).with_context(|| {
        format!(
            "failed to read App Store Connect private key {}",
            resolved.display()
        )
    })
}

pub fn resolve_input_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return expand_home(path);
    }

    let path = expand_home(path)?;
    Ok(std::env::current_dir()
        .context("failed to resolve current directory")?
        .join(path))
}

fn expand_home(path: &Path) -> Result<PathBuf> {
    let raw = path.to_string_lossy();
    if let Some(stripped) = raw.strip_prefix("~/") {
        let home = std::env::var("HOME").context("HOME is not set")?;
        return Ok(PathBuf::from(home).join(stripped));
    }

    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::StoredAuthRecord;

    #[test]
    fn stored_auth_record_serializes_display_name() {
        let record = StoredAuthRecord {
            display_name: "Example Team".to_owned(),
            issuer_id: "issuer".to_owned(),
            key_id: "key".to_owned(),
            private_key_pem: "pem".to_owned(),
        };

        assert_eq!(
            serde_json::to_value(record).unwrap(),
            json!({
                "displayName": "Example Team",
                "issuerId": "issuer",
                "keyId": "key",
                "privateKeyPem": "pem"
            })
        );
    }
}
