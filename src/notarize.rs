use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use tempfile::TempDir;

use crate::{
    auth::StoredAuthRecord, auth_store, cli::NotarizeArgs, config::Config, config_io,
    state::set_private_permissions,
};

pub fn run(args: &NotarizeArgs) -> Result<()> {
    let config = config_io::load_config(&args.config)?;
    run_with_config(&config, &args.file)
}

pub fn run_with_config(config: &Config, file: &Path) -> Result<()> {
    config.validate()?;

    let auth = auth_store::resolve_auth_record(&config.team_id)?;
    let tempdir =
        tempfile::tempdir().context("failed to create temporary notarization workspace")?;
    let key_path = write_private_key(&tempdir, &auth)?;
    let prepared = prepare_submission(file, &tempdir)?;

    let output = Command::new("xcrun")
        .arg("notarytool")
        .arg("submit")
        .arg(&prepared.upload_path)
        .arg("--key")
        .arg(&key_path)
        .arg("--key-id")
        .arg(&auth.key_id)
        .arg("--issuer")
        .arg(&auth.issuer_id)
        .arg("--wait")
        .arg("--output-format")
        .arg("json")
        .output()
        .context("failed to execute xcrun notarytool submit")?;
    ensure!(
        output.status.success(),
        "notarytool submit failed: {}",
        command_failure(&output.stderr, &output.stdout)
    );

    if let Some(target) = prepared.staple_target {
        let staple = Command::new("xcrun")
            .arg("stapler")
            .arg("staple")
            .arg(&target)
            .output()
            .context("failed to execute xcrun stapler staple")?;
        ensure!(
            staple.status.success(),
            "stapler failed for {}: {}",
            target.display(),
            command_failure(&staple.stderr, &staple.stdout)
        );
        println!("Notarized and stapled {}", target.display());
    } else {
        println!("Notarized {}", prepared.upload_path.display());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !stdout.is_empty() {
        println!("{stdout}");
    }
    Ok(())
}

struct PreparedSubmission {
    upload_path: PathBuf,
    staple_target: Option<PathBuf>,
}

fn prepare_submission(path: &Path, tempdir: &TempDir) -> Result<PreparedSubmission> {
    ensure!(path.exists(), "file {} does not exist", path.display());

    if path.is_dir() && path.extension() == Some(OsStr::new("app")) {
        let archive_name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| anyhow::anyhow!("app path {} has no valid file name", path.display()))?;
        let archive_path = tempdir.path().join(format!("{archive_name}.zip"));
        let output = Command::new("ditto")
            .arg("-c")
            .arg("-k")
            .arg("--keepParent")
            .arg(path)
            .arg(&archive_path)
            .output()
            .context("failed to execute ditto for notarization archive")?;
        ensure!(
            output.status.success(),
            "ditto failed for {}: {}",
            path.display(),
            command_failure(&output.stderr, &output.stdout)
        );
        return Ok(PreparedSubmission {
            upload_path: archive_path,
            staple_target: Some(path.to_path_buf()),
        });
    }

    match path.extension().and_then(OsStr::to_str) {
        Some("pkg") | Some("dmg") => Ok(PreparedSubmission {
            upload_path: path.to_path_buf(),
            staple_target: Some(path.to_path_buf()),
        }),
        Some("zip") => Ok(PreparedSubmission {
            upload_path: path.to_path_buf(),
            staple_target: None,
        }),
        _ => bail!(
            "unsupported notarization input {}; expected .app, .pkg, .dmg, or .zip",
            path.display()
        ),
    }
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
