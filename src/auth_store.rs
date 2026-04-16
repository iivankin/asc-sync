use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;

use crate::{
    auth::{AuthContext, StoredAuthRecord, read_private_key_pem},
    system,
};

const ISSUER_ID_ENV: &str = "ASC_ISSUER_ID";
const KEY_ID_ENV: &str = "ASC_KEY_ID";
const PRIVATE_KEY_ENV: &str = "ASC_PRIVATE_KEY";
const PRIVATE_KEY_PATH_ENV: &str = "ASC_PRIVATE_KEY_PATH";
const ASC_CLI_KEYCHAIN_SERVICE: &str = "asc";
const ASC_CLI_KEYCHAIN_ACCOUNT_PREFIX: &str = "asc:credential:";

#[derive(Debug, Deserialize, Default)]
struct AscCliConfig {
    #[serde(default)]
    key_id: String,
    #[serde(default)]
    issuer_id: String,
    #[serde(default)]
    private_key_path: String,
    #[serde(default)]
    default_key_name: String,
    #[serde(default)]
    keys: Vec<AscCliKeyEntry>,
    #[serde(default)]
    keychain_metadata: Vec<AscCliKeychainMetadata>,
}

#[derive(Debug, Deserialize, Default)]
struct AscCliKeyEntry {
    name: String,
    key_id: String,
    issuer_id: String,
    private_key_path: String,
}

#[derive(Debug, Deserialize, Default)]
struct AscCliKeychainMetadata {
    name: String,
}

#[derive(Debug, Deserialize)]
struct AscCliKeychainPayload {
    key_id: String,
    issuer_id: String,
    private_key_path: String,
    #[serde(default)]
    private_key_pem: String,
}

pub fn import_auth_interactively() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stderr().is_terminal(),
        "auth import requires an interactive terminal"
    );

    let imported = maybe_import_from_asc_cli()?;
    let record = match imported {
        Some(record) => record,
        None => prompt_manual_auth_record()?,
    };

    let team_id = prompt_required("Team ID")?;
    let _ = record.clone().into_context()?;
    store_auth_record(&team_id, &record)?;

    println!("Stored App Store Connect auth for team {team_id} in ~/.asc-sync.");
    Ok(())
}

pub fn resolve_auth_context(team_id: &str) -> Result<AuthContext> {
    resolve_auth_record(team_id)?.into_context()
}

pub fn resolve_auth_context_if_available(team_id: &str) -> Result<Option<AuthContext>> {
    if let Some(record) = load_auth_record(team_id)? {
        return record.into_context().map(Some);
    }

    if let Some(record) = load_env_auth_record()? {
        return record.into_context().map(Some);
    }

    Ok(None)
}

pub fn stored_team_ids() -> Result<Vec<String>> {
    system::list_stored_asc_auth_team_ids()
}

pub fn resolve_auth_record(team_id: &str) -> Result<StoredAuthRecord> {
    if let Some(record) = load_auth_record(team_id)? {
        return Ok(record);
    }

    if let Some(record) = load_env_auth_record()? {
        return Ok(record);
    }

    bail!(
        "no App Store Connect auth configured for team {team_id}; run `asc-sync auth import` or set {ISSUER_ID_ENV}, {KEY_ID_ENV}, and one of {PRIVATE_KEY_ENV}/{PRIVATE_KEY_PATH_ENV}"
    );
}

fn maybe_import_from_asc_cli() -> Result<Option<StoredAuthRecord>> {
    let Some(config_path) = asc_cli_config_path_if_available()? else {
        return Ok(None);
    };

    let config = load_asc_cli_config(&config_path)?;
    let profiles = asc_cli_profiles(&config);
    if profiles.is_empty() {
        return Ok(None);
    }

    let default_profile = asc_cli_default_profile(&config, &profiles);
    let prompt = if default_profile.is_some() {
        format!(
            "Found asccli auth at {}. Import an existing asccli profile? [Y/n]: ",
            config_path.display()
        )
    } else {
        format!(
            "Found asccli auth at {}. Import it instead of typing credentials? [Y/n]: ",
            config_path.display()
        )
    };
    if !prompt_yes_no(&prompt, true)? {
        return Ok(None);
    }

    let selected_profile = if profiles.len() == 1 {
        profiles[0].clone()
    } else {
        prompt_with_default("asccli profile", default_profile.as_deref())?
    };

    match load_asc_cli_profile(&config, &selected_profile)? {
        Some(record) => Ok(Some(record)),
        None => {
            eprintln!(
                "Could not resolve asccli profile `{selected_profile}`. Falling back to manual input."
            );
            Ok(None)
        }
    }
}

fn load_auth_record(team_id: &str) -> Result<Option<StoredAuthRecord>> {
    let Some(encoded) = system::load_stored_asc_auth(team_id)? else {
        return Ok(None);
    };
    let json = STANDARD
        .decode(encoded)
        .context("stored App Store Connect auth is not valid base64")?;
    let record = serde_json::from_slice::<StoredAuthRecord>(&json)
        .context("stored App Store Connect auth is not valid JSON")?;
    Ok(Some(record))
}

fn store_auth_record(team_id: &str, record: &StoredAuthRecord) -> Result<()> {
    ensure!(!team_id.trim().is_empty(), "team ID cannot be empty");
    let json = serde_json::to_vec(record).context("failed to serialize auth record")?;
    let encoded = STANDARD.encode(json);
    system::store_stored_asc_auth(team_id, &encoded)
}

fn load_env_auth_record() -> Result<Option<StoredAuthRecord>> {
    let issuer_id = std::env::var(ISSUER_ID_ENV).ok();
    let key_id = std::env::var(KEY_ID_ENV).ok();
    let private_key = std::env::var(PRIVATE_KEY_ENV).ok();
    let private_key_path = std::env::var(PRIVATE_KEY_PATH_ENV).ok();

    if issuer_id.is_none()
        && key_id.is_none()
        && private_key.is_none()
        && private_key_path.is_none()
    {
        return Ok(None);
    }

    let issuer_id = issuer_id.ok_or_else(|| anyhow::anyhow!("{ISSUER_ID_ENV} is required"))?;
    let key_id = key_id.ok_or_else(|| anyhow::anyhow!("{KEY_ID_ENV} is required"))?;
    let private_key_pem = match (private_key, private_key_path) {
        (Some(value), _) => normalize_env_private_key(value),
        (None, Some(path)) => {
            let pem = read_private_key_pem(Path::new(&path))?;
            String::from_utf8(pem).context("private key file is not valid UTF-8 PEM")?
        }
        (None, None) => bail!(
            "either {PRIVATE_KEY_ENV} or {PRIVATE_KEY_PATH_ENV} must be set for App Store Connect auth"
        ),
    };

    Ok(Some(StoredAuthRecord {
        issuer_id,
        key_id,
        private_key_pem,
    }))
}

fn prompt_manual_auth_record() -> Result<StoredAuthRecord> {
    let issuer_id = prompt_required("Issuer ID")?;
    let key_id = prompt_required("Key ID")?;
    let private_key_path = prompt_required("Private key path")?;
    let private_key_pem = read_private_key_pem(Path::new(&private_key_path))?;
    let private_key_pem =
        String::from_utf8(private_key_pem).context("private key file is not valid UTF-8 PEM")?;

    Ok(StoredAuthRecord {
        issuer_id,
        key_id,
        private_key_pem,
    })
}

fn normalize_env_private_key(value: String) -> String {
    if value.contains("\\n") && !value.contains('\n') {
        value.replace("\\n", "\n")
    } else {
        value
    }
}

fn asc_cli_config_path_if_available() -> Result<Option<PathBuf>> {
    if !asc_cli_installed()? {
        return Ok(None);
    }

    let home = std::env::var("HOME").context("HOME is not set")?;
    let path = PathBuf::from(home).join(".asc/config.json");
    if path.exists() {
        Ok(Some(path))
    } else {
        Ok(None)
    }
}

fn asc_cli_installed() -> Result<bool> {
    let status = Command::new("which")
        .arg("asc")
        .status()
        .context("failed to check whether asccli is installed")?;
    Ok(status.success())
}

fn load_asc_cli_config(path: &Path) -> Result<AscCliConfig> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read asccli config {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse asccli config {}", path.display()))
}

fn asc_cli_profiles(config: &AscCliConfig) -> Vec<String> {
    let mut profiles = BTreeSet::new();
    if !config.default_key_name.trim().is_empty() {
        profiles.insert(config.default_key_name.trim().to_owned());
    }
    for key in &config.keys {
        if !key.name.trim().is_empty() {
            profiles.insert(key.name.trim().to_owned());
        }
    }
    for metadata in &config.keychain_metadata {
        if !metadata.name.trim().is_empty() {
            profiles.insert(metadata.name.trim().to_owned());
        }
    }
    if has_asc_cli_legacy_credentials(config) {
        profiles.insert("default".to_owned());
    }
    profiles.into_iter().collect()
}

fn asc_cli_default_profile(config: &AscCliConfig, profiles: &[String]) -> Option<String> {
    if !config.default_key_name.trim().is_empty() {
        return Some(config.default_key_name.trim().to_owned());
    }
    if profiles.len() == 1 {
        return profiles.first().cloned();
    }
    if has_asc_cli_legacy_credentials(config) {
        return Some("default".to_owned());
    }
    None
}

fn has_asc_cli_legacy_credentials(config: &AscCliConfig) -> bool {
    !config.key_id.trim().is_empty()
        && !config.issuer_id.trim().is_empty()
        && !config.private_key_path.trim().is_empty()
}

fn load_asc_cli_profile(config: &AscCliConfig, profile: &str) -> Result<Option<StoredAuthRecord>> {
    if let Some(record) = load_asc_cli_profile_from_keychain(profile)? {
        return Ok(Some(record));
    }

    if profile == "default" && has_asc_cli_legacy_credentials(config) {
        return load_asc_cli_file_record(
            &config.key_id,
            &config.issuer_id,
            &config.private_key_path,
        )
        .map(Some);
    }

    if let Some(entry) = config.keys.iter().find(|entry| entry.name == profile) {
        return load_asc_cli_file_record(&entry.key_id, &entry.issuer_id, &entry.private_key_path)
            .map(Some);
    }

    Ok(None)
}

fn load_asc_cli_profile_from_keychain(profile: &str) -> Result<Option<StoredAuthRecord>> {
    let account = format!("{ASC_CLI_KEYCHAIN_ACCOUNT_PREFIX}{profile}");
    let Some(secret) = system::load_external_generic_password(ASC_CLI_KEYCHAIN_SERVICE, &account)?
    else {
        return Ok(None);
    };
    let payload: AscCliKeychainPayload = serde_json::from_str(&secret).with_context(|| {
        format!("failed to parse asccli keychain payload for profile {profile}")
    })?;
    let private_key_pem = if !payload.private_key_pem.trim().is_empty() {
        payload.private_key_pem
    } else {
        let pem = read_private_key_pem(Path::new(&payload.private_key_path))?;
        String::from_utf8(pem).context("asccli private key file is not valid UTF-8 PEM")?
    };

    Ok(Some(StoredAuthRecord {
        issuer_id: payload.issuer_id,
        key_id: payload.key_id,
        private_key_pem,
    }))
}

fn load_asc_cli_file_record(
    key_id: &str,
    issuer_id: &str,
    private_key_path: &str,
) -> Result<StoredAuthRecord> {
    let pem = read_private_key_pem(Path::new(private_key_path))?;
    Ok(StoredAuthRecord {
        issuer_id: issuer_id.to_owned(),
        key_id: key_id.to_owned(),
        private_key_pem: String::from_utf8(pem)
            .context("asccli private key file is not valid UTF-8 PEM")?,
    })
}

fn prompt_required(label: &str) -> Result<String> {
    let mut stdout = io::stdout().lock();
    loop {
        write!(stdout, "{label}: ").with_context(|| format!("failed to write {label} prompt"))?;
        stdout.flush().context("failed to flush prompt")?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .with_context(|| format!("failed to read {label}"))?;
        let value = input.trim().to_owned();
        if !value.is_empty() {
            return Ok(value);
        }
        eprintln!("{label} cannot be empty.");
    }
}

fn prompt_with_default(label: &str, default: Option<&str>) -> Result<String> {
    let mut stdout = io::stdout().lock();
    loop {
        if let Some(default) = default {
            write!(stdout, "{label} [{default}]: ")
                .with_context(|| format!("failed to write {label} prompt"))?;
        } else {
            write!(stdout, "{label}: ")
                .with_context(|| format!("failed to write {label} prompt"))?;
        }
        stdout.flush().context("failed to flush prompt")?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .with_context(|| format!("failed to read {label}"))?;
        let value = input.trim();
        if !value.is_empty() {
            return Ok(value.to_owned());
        }
        if let Some(default) = default {
            return Ok(default.to_owned());
        }
        eprintln!("{label} cannot be empty.");
    }
}

fn prompt_yes_no(prompt: &str, default_yes: bool) -> Result<bool> {
    let mut stdout = io::stdout().lock();
    loop {
        write!(stdout, "{prompt}").context("failed to write prompt")?;
        stdout.flush().context("failed to flush prompt")?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read yes/no answer")?;
        let trimmed = input.trim().to_ascii_lowercase();
        if trimmed.is_empty() {
            return Ok(default_yes);
        }
        match trimmed.as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => eprintln!("Please answer y or n."),
        }
    }
}
