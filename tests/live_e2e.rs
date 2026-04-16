use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use age::secrecy::SecretString;
use asc_sync::{bundle, sync::Workspace};
use serde_json::{Value, json};
use tempfile::TempDir;

#[derive(Debug, Clone)]
struct LiveEnv {
    team_id: String,
    issuer_id: String,
    key_id: String,
    private_key_path: PathBuf,
    developer_bundle_password: String,
    release_bundle_password: String,
}

#[derive(Debug)]
struct CommandOutput {
    stdout: String,
    stderr: String,
}

struct LiveCase {
    _tempdir: TempDir,
    root: PathBuf,
    config_path: PathBuf,
    bundle_path: PathBuf,
    env: LiveEnv,
}

impl LiveEnv {
    fn load() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let env_path = root.join(".env");
        let contents = fs::read_to_string(&env_path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", env_path.display()));

        let mut values = BTreeMap::new();
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (key, value) = trimmed
                .split_once('=')
                .unwrap_or_else(|| panic!("invalid .env line: {trimmed}"));
            values.insert(key.trim().to_owned(), value.trim().to_owned());
        }

        let private_key_path = root.join(values.get("ASC_PRIVATE_KEY_PATH").unwrap_or_else(|| {
            panic!("ASC_PRIVATE_KEY_PATH is missing in {}", env_path.display())
        }));

        Self {
            team_id: values
                .remove("ASC_TEAM_ID")
                .unwrap_or_else(|| panic!("ASC_TEAM_ID is missing in {}", env_path.display())),
            issuer_id: values
                .remove("ASC_ISSUER_ID")
                .unwrap_or_else(|| panic!("ASC_ISSUER_ID is missing in {}", env_path.display())),
            key_id: values
                .remove("ASC_KEY_ID")
                .unwrap_or_else(|| panic!("ASC_KEY_ID is missing in {}", env_path.display())),
            private_key_path,
            developer_bundle_password: "asc-sync-live-developer-password".into(),
            release_bundle_password: "asc-sync-live-release-password".into(),
        }
    }
}

impl LiveCase {
    fn new(env: LiveEnv) -> Self {
        let tempdir = tempfile::tempdir().expect("failed to create live e2e tempdir");
        let root = tempdir.path().to_path_buf();
        let config_path = root.join("asc.json");
        let bundle_path = root.join(bundle::BUNDLE_FILE_NAME);
        Self {
            _tempdir: tempdir,
            root,
            config_path,
            bundle_path,
            env,
        }
    }

    fn run(&self, args: &[&str]) -> CommandOutput {
        let output = Command::new(env!("CARGO_BIN_EXE_asc-sync"))
            .args(args)
            .current_dir(&self.root)
            .env("ASC_SYNC_DISABLE_KEYCHAIN_CACHE", "1")
            .env("ASC_ISSUER_ID", &self.env.issuer_id)
            .env("ASC_KEY_ID", &self.env.key_id)
            .env("ASC_PRIVATE_KEY_PATH", &self.env.private_key_path)
            .env(
                bundle::DEVELOPER_BUNDLE_PASSWORD_ENV,
                &self.env.developer_bundle_password,
            )
            .env(
                bundle::RELEASE_BUNDLE_PASSWORD_ENV,
                &self.env.release_bundle_password,
            )
            .output()
            .unwrap_or_else(|error| panic!("failed to execute asc-sync {:?}: {error}", args));

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "command failed: asc-sync {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            stdout,
            stderr
        );

        CommandOutput { stdout, stderr }
    }

    fn read_config_json(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.config_path).expect("failed to read config"))
            .expect("failed to parse config JSON")
    }

    fn write_config_json(&self, value: &Value) {
        let data = serde_json::to_vec_pretty(value).expect("failed to serialize config");
        fs::write(&self.config_path, data).expect("failed to write config");
    }

    fn initialize_bundle(&self) {
        let workspace = Workspace::from_config_path(&self.config_path);
        let passwords = BTreeMap::from([
            (
                asc_sync::scope::Scope::Developer,
                SecretString::from(self.env.developer_bundle_password.clone()),
            ),
            (
                asc_sync::scope::Scope::Release,
                SecretString::from(self.env.release_bundle_password.clone()),
            ),
        ]);
        bundle::initialize_bundle(&workspace.bundle_path, &self.env.team_id, &passwords)
            .expect("failed to initialize signing bundle");
    }

    fn load_state(&self) -> asc_sync::state::State {
        bundle::load_state(&self.bundle_path).expect("failed to load bundle state")
    }

    fn minimal_config(&self, schema: Option<&str>) -> Value {
        let mut root = serde_json::Map::new();
        if let Some(schema) = schema {
            root.insert("$schema".into(), Value::String(schema.to_owned()));
        }
        root.insert("team_id".into(), Value::String(self.env.team_id.clone()));
        root.insert("bundle_ids".into(), json!({}));
        root.insert("devices".into(), json!({}));
        root.insert("certs".into(), json!({}));
        root.insert("profiles".into(), json!({}));
        Value::Object(root)
    }
}

impl Drop for LiveCase {
    fn drop(&mut self) {
        if !self.config_path.exists() || !self.bundle_path.exists() {
            return;
        }

        let schema = self
            .read_config_json()
            .get("$schema")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let minimal = self.minimal_config(schema.as_deref());
        self.write_config_json(&minimal);

        let _ = Command::new(env!("CARGO_BIN_EXE_asc-sync"))
            .args(["revoke", "all", "--config"])
            .arg(&self.config_path)
            .current_dir(&self.root)
            .env("ASC_SYNC_DISABLE_KEYCHAIN_CACHE", "1")
            .env("ASC_ISSUER_ID", &self.env.issuer_id)
            .env("ASC_KEY_ID", &self.env.key_id)
            .env("ASC_PRIVATE_KEY_PATH", &self.env.private_key_path)
            .env(
                bundle::DEVELOPER_BUNDLE_PASSWORD_ENV,
                &self.env.developer_bundle_password,
            )
            .env(
                bundle::RELEASE_BUNDLE_PASSWORD_ENV,
                &self.env.release_bundle_password,
            )
            .output();

        let _ = Command::new(env!("CARGO_BIN_EXE_asc-sync"))
            .args(["apply", "--config"])
            .arg(&self.config_path)
            .current_dir(&self.root)
            .env("ASC_SYNC_DISABLE_KEYCHAIN_CACHE", "1")
            .env("ASC_ISSUER_ID", &self.env.issuer_id)
            .env("ASC_KEY_ID", &self.env.key_id)
            .env("ASC_PRIVATE_KEY_PATH", &self.env.private_key_path)
            .env(
                bundle::DEVELOPER_BUNDLE_PASSWORD_ENV,
                &self.env.developer_bundle_password,
            )
            .env(
                bundle::RELEASE_BUNDLE_PASSWORD_ENV,
                &self.env.release_bundle_password,
            )
            .output();
    }
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    format!("e2e{:x}", nanos)
}

fn schema_from_config(config: &Value) -> Option<&str> {
    config.get("$schema").and_then(Value::as_str)
}

#[test]
#[ignore = "requires live App Store Connect access and real credentials from .env"]
fn live_cli_macos_roundtrip_uses_env_auth_and_bundle() {
    let env = LiveEnv::load();

    let device_case = LiveCase::new(env.clone());
    device_case.run(&[
        "init",
        "--config",
        device_case.config_path.to_str().unwrap(),
        "--team-id",
        &env.team_id,
    ]);

    let init_config = device_case.read_config_json();
    assert_eq!(
        init_config.get("team_id").and_then(Value::as_str),
        Some(env.team_id.as_str())
    );
    assert!(
        schema_from_config(&init_config).is_some(),
        "init must write $schema"
    );

    let output = device_case.run(&[
        "device",
        "add-local",
        "--config",
        device_case.config_path.to_str().unwrap(),
        "--current-mac",
        "--id",
        "current-mac",
        "--name",
        "ASC Sync Live Current Mac",
    ]);
    assert!(
        output
            .stdout
            .contains("Wrote device ASC Sync Live Current Mac"),
        "unexpected device add-local output: {}{}",
        output.stdout,
        output.stderr
    );

    let updated_config = device_case.read_config_json();
    assert_eq!(
        updated_config["devices"]["current-mac"]["family"].as_str(),
        Some("macos")
    );

    let case = LiveCase::new(env.clone());
    case.run(&[
        "init",
        "--config",
        case.config_path.to_str().unwrap(),
        "--team-id",
        &env.team_id,
    ]);

    let suffix = unique_suffix();
    let bundle_id = format!("dev.orbitstorage.ascsync.{suffix}");
    let schema = schema_from_config(&case.read_config_json()).map(str::to_owned);
    let config = json!({
        "$schema": schema,
        "team_id": env.team_id,
        "bundle_ids": {
            "live-app": {
                "bundle_id": bundle_id,
                "name": format!("ASC Sync Live {suffix}"),
                "platform": "mac_os",
                "capabilities": ["in_app_purchase"]
            }
        },
        "certs": {
            "dev-cert": {
                "type": "development",
                "name": format!("ASC Sync Dev {suffix}")
            },
            "dist-cert": {
                "type": "distribution",
                "name": format!("ASC Sync Dist {suffix}")
            }
        },
        "profiles": {
            "mac-store": {
                "name": format!("ASC Sync Mac Store {suffix}"),
                "type": "mac_app_store",
                "bundle_id": "live-app",
                "certs": ["dist-cert"]
            }
        }
    });
    case.write_config_json(&config);
    case.initialize_bundle();

    case.run(&["validate", "--config", case.config_path.to_str().unwrap()]);

    let plan = case.run(&["plan", "--config", case.config_path.to_str().unwrap()]);
    assert!(
        plan.stdout.contains("bundle_id.live-app")
            && plan.stdout.contains("cert.dev-cert")
            && plan.stdout.contains("cert.dist-cert")
            && plan.stdout.contains("profile.mac-store"),
        "unexpected plan output:\n{}\n{}",
        plan.stdout,
        plan.stderr
    );

    case.run(&["apply", "--config", case.config_path.to_str().unwrap()]);
    case.run(&["validate", "--config", case.config_path.to_str().unwrap()]);

    let mut drifted_config = case.read_config_json();
    drifted_config["bundle_ids"]["live-app"]["capabilities"] =
        json!(["in_app_purchase", "associated_domains"]);
    case.write_config_json(&drifted_config);

    let second_plan = case.run(&["plan", "--config", case.config_path.to_str().unwrap()]);
    assert!(
        second_plan
            .stdout
            .contains("bundle_id.live-app.capability.ASSOCIATED_DOMAINS"),
        "unexpected second plan output:\n{}\n{}",
        second_plan.stdout,
        second_plan.stderr
    );

    case.run(&["apply", "--config", case.config_path.to_str().unwrap()]);
    case.run(&["validate", "--config", case.config_path.to_str().unwrap()]);

    let state = case.load_state();
    assert_eq!(state.team_id, env.team_id);
    assert_eq!(
        state.bundle_ids["live-app"].bundle_id, bundle_id,
        "apply should persist managed bundle_id"
    );
    assert!(
        state.certs.contains_key("dev-cert") && state.certs.contains_key("dist-cert"),
        "apply should persist both managed certificates"
    );
    assert!(
        state.profiles.contains_key("mac-store"),
        "apply should persist managed provisioning profile"
    );

    let build_settings = case.run(&[
        "signing",
        "print-build-settings",
        "--config",
        case.config_path.to_str().unwrap(),
    ]);
    assert!(
        build_settings
            .stdout
            .contains("PROVISIONING_PROFILE_SPECIFIER=mac-store")
            && build_settings
                .stdout
                .contains(&format!("DEVELOPMENT_TEAM={}", env.team_id)),
        "unexpected build settings output:\n{}\n{}",
        build_settings.stdout,
        build_settings.stderr
    );

    case.run(&[
        "signing",
        "import",
        "--config",
        case.config_path.to_str().unwrap(),
    ]);

    case.run(&[
        "revoke",
        "all",
        "--config",
        case.config_path.to_str().unwrap(),
    ]);
    let revoked = case.load_state();
    assert!(revoked.certs.is_empty(), "revoke must clear managed certs");
    assert!(
        revoked.profiles.is_empty(),
        "revoke must clear managed profiles"
    );

    let empty_config = case.minimal_config(schema.as_deref());
    case.write_config_json(&empty_config);
    case.run(&["apply", "--config", case.config_path.to_str().unwrap()]);
    let cleaned = case.load_state();
    assert!(
        cleaned.bundle_ids.is_empty()
            && cleaned.devices.is_empty()
            && cleaned.certs.is_empty()
            && cleaned.profiles.is_empty(),
        "cleanup apply must remove managed state"
    );
}
