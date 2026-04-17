use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use tempfile::TempDir;

use crate::{
    asc::AscClient,
    auth::StoredAuthRecord,
    auth_store,
    cli::SubmitArgs,
    config::{BundleIdSpec, Config},
    config_io,
    state::set_private_permissions,
};

pub fn run(args: &SubmitArgs) -> Result<()> {
    let config = config_io::load_config(&args.config)?;
    config.validate()?;

    let (logical_name, bundle_spec) = resolve_bundle_spec(&config, args.bundle_id.as_deref())?;
    let auth_record = auth_store::resolve_auth_record(&config.team_id)?;
    let client = AscClient::new(auth_record.clone().into_context()?)?;

    client
        .find_bundle_id_by_identifier(&bundle_spec.bundle_id)?
        .with_context(|| {
            format!(
                "bundle_id {} ({}) does not exist in App Store Connect; run your ASC apply workflow first",
                logical_name, bundle_spec.bundle_id
            )
        })?;
    ensure_existing_app_record(&client, &bundle_spec.bundle_id)?;

    let tempdir = tempfile::tempdir().context("failed to create temporary submit workspace")?;
    let key_path = write_private_key(&tempdir, &auth_record)?;
    let upload_mode = upload_mode(&args.file)?;

    let output = Command::new("xcrun")
        .arg("altool")
        .arg("--upload-app")
        .arg("-f")
        .arg(&args.file)
        .arg("--type")
        .arg(upload_mode.platform)
        .arg("--api-key")
        .arg(&auth_record.key_id)
        .arg("--api-issuer")
        .arg(&auth_record.issuer_id)
        .arg("--p8-file-path")
        .arg(&key_path)
        .arg("--output-format")
        .arg("json")
        .arg("--wait")
        .output()
        .context("failed to execute xcrun altool --upload-app")?;
    ensure!(
        output.status.success(),
        "submit failed for {}: {}",
        args.file.display(),
        command_failure(&output.stderr, &output.stdout)
    );

    println!(
        "Submitted {} for bundle_id {} using {} upload flow.",
        args.file.display(),
        bundle_spec.bundle_id,
        upload_mode.description
    );
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !stdout.is_empty() {
        println!("{stdout}");
    }
    Ok(())
}

fn resolve_bundle_spec<'a>(
    config: &'a Config,
    explicit_logical_name: Option<&'a str>,
) -> Result<(&'a str, &'a BundleIdSpec)> {
    if let Some(logical_name) = explicit_logical_name {
        let spec = config
            .bundle_ids
            .get(logical_name)
            .ok_or_else(|| anyhow::anyhow!("unknown bundle_id logical key {logical_name}"))?;
        return Ok((logical_name, spec));
    }

    let mut bundle_ids = config.bundle_ids.iter();
    let Some((logical_name, spec)) = bundle_ids.next() else {
        bail!("submit requires at least one bundle_id in asc.json");
    };
    ensure!(
        bundle_ids.next().is_none(),
        "submit requires --bundle-id when asc.json contains multiple bundle_ids"
    );
    Ok((logical_name.as_str(), spec))
}

struct UploadMode {
    description: &'static str,
    platform: &'static str,
}

fn upload_mode(file: &Path) -> Result<UploadMode> {
    if file.is_dir() && file.extension().and_then(OsStr::to_str) == Some("app") {
        return Ok(UploadMode {
            description: "macOS App Store Connect app bundle",
            platform: "macos",
        });
    }

    match file.extension().and_then(OsStr::to_str) {
        Some("ipa") => Ok(UploadMode {
            description: "iOS/App Store Connect",
            platform: "ios",
        }),
        Some("app") => Ok(UploadMode {
            description: "macOS App Store Connect app bundle",
            platform: "macos",
        }),
        _ => bail!(
            "unsupported submit input {}; expected .ipa or .app",
            file.display()
        ),
    }
}

fn ensure_existing_app_record(client: &AscClient, bundle_id: &str) -> Result<()> {
    if client.find_app_by_bundle_id(bundle_id)?.is_some() {
        return Ok(());
    }

    bail!(
        "App Store Connect app record for {} does not exist; create it first in App Store Connect before running submit",
        bundle_id
    );
}

fn write_private_key(tempdir: &TempDir, auth: &StoredAuthRecord) -> Result<PathBuf> {
    ensure!(
        !auth.private_key_pem.trim().is_empty(),
        "App Store Connect private key is empty"
    );
    let key_path = tempdir.path().join(format!("AuthKey_{}.p8", auth.key_id));
    fs::write(&key_path, auth.private_key_pem.as_bytes())
        .with_context(|| format!("failed to write {}", key_path.display()))?;
    set_private_permissions(&key_path)?;
    Ok(key_path)
}

fn command_failure(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    String::from_utf8_lossy(stdout).trim().to_owned()
}
