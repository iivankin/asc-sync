use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub team_id: String,
    #[serde(default)]
    pub bundle_ids: BTreeMap<String, ManagedBundleId>,
    #[serde(default)]
    pub devices: BTreeMap<String, ManagedDevice>,
    #[serde(default)]
    pub certs: BTreeMap<String, ManagedCertificate>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ManagedProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedBundleId {
    pub apple_id: String,
    pub bundle_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedDevice {
    pub apple_id: String,
    pub udid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedCertificate {
    pub apple_id: String,
    pub kind: String,
    pub name: String,
    pub serial_number: String,
    // The PKCS#12 password is part of encrypted signing payload data, not shared state.
    // Keep it on the in-memory model so the current command can still import or validate
    // freshly-created certificates before the bundle is written back.
    #[serde(skip, default)]
    pub p12_password: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedProfile {
    pub apple_id: String,
    pub name: String,
    pub kind: String,
    pub bundle_id: String,
    pub certs: Vec<String>,
    pub devices: Vec<String>,
    pub uuid: String,
}

impl State {
    pub fn new(team_id: &str) -> Self {
        Self {
            version: 4,
            team_id: team_id.to_owned(),
            bundle_ids: BTreeMap::new(),
            devices: BTreeMap::new(),
            certs: BTreeMap::new(),
            profiles: BTreeMap::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let data = fs::read(path)
            .with_context(|| format!("failed to read state file {}", path.display()))?;
        Self::from_slice(&data)
            .with_context(|| format!("failed to parse state file {}", path.display()))
    }

    pub fn load_for_team(path: &Path, team_id: &str) -> Result<Self> {
        let state = Self::load(path)?;
        state.ensure_team(team_id)?;
        Ok(state)
    }

    pub fn from_slice(data: &[u8]) -> Result<Self> {
        let state: Self = serde_json::from_slice(data).context("failed to deserialize state")?;
        state.validate_format()?;
        Ok(state)
    }

    pub fn ensure_team(&self, team_id: &str) -> Result<()> {
        if self.team_id != team_id {
            bail!(
                "state file belongs to team {}, but current config/auth resolves to {}",
                self.team_id,
                team_id
            );
        }

        Ok(())
    }

    fn validate_format(&self) -> Result<()> {
        ensure!(
            self.version == 4,
            "unsupported state version {}",
            self.version
        );
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let data = serde_json::to_vec_pretty(self).context("failed to serialize state")?;
        let mut temp = NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))
            .context("failed to create temporary state file")?;
        std::io::Write::write_all(&mut temp, &data).context("failed to write temporary state")?;
        set_private_permissions(temp.path())?;
        temp.persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to persist state file {}", path.display()))?;
        set_private_permissions(path)?;
        Ok(())
    }
}

pub fn set_private_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let permissions = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}
