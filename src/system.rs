use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use anyhow::{Context, Result, ensure};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use plist::Value as PlistValue;

use crate::{scope::Scope, state::set_private_permissions};

const KEYCHAIN_TOOLS: &[&str] = &[
    "/usr/bin/codesign",
    "/usr/bin/security",
    "/usr/bin/xcodebuild",
    "/usr/bin/productbuild",
];
const ASC_SYNC_DIR_NAME: &str = ".asc-sync";
const AUTH_DIR_NAME: &str = "auth";
const BUNDLE_PASSWORDS_DIR_NAME: &str = "bundle-passwords";

pub struct GeneratedCsr {
    _tempdir: tempfile::TempDir,
    pub key_path: PathBuf,
    pub csr_pem: String,
}

pub fn login_keychain_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Keychains/login.keychain-db"))
}

pub fn provisioning_profiles_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/MobileDevice/Provisioning Profiles"))
}

pub fn generate_csr(common_name: &str) -> Result<GeneratedCsr> {
    let tempdir = tempfile::tempdir().context("failed to create temporary CSR directory")?;
    let key_path = tempdir.path().join("certificate.key");
    let csr_path = tempdir.path().join("certificate.csr");

    let status = Command::new("openssl")
        .arg("req")
        .arg("-new")
        .arg("-newkey")
        .arg("rsa:2048")
        .arg("-nodes")
        .arg("-keyout")
        .arg(&key_path)
        .arg("-subj")
        .arg(format!("/CN={common_name}"))
        .arg("-out")
        .arg(&csr_path)
        .status()
        .context("failed to execute openssl req")?;

    ensure!(status.success(), "openssl req failed with status {status}");
    set_private_permissions(&key_path)?;
    let csr_pem = fs::read_to_string(&csr_path)
        .with_context(|| format!("failed to read {}", csr_path.display()))?;

    Ok(GeneratedCsr {
        _tempdir: tempdir,
        key_path,
        csr_pem,
    })
}

pub fn create_pkcs12(
    key_path: &Path,
    certificate_der_base64: &str,
    output_path: &Path,
    password: &str,
) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let tempdir = tempfile::tempdir().context("failed to create temporary PKCS#12 directory")?;
    let certificate_pem_path = tempdir.path().join("certificate.pem");
    let certificate_der = STANDARD
        .decode(certificate_der_base64)
        .context("failed to decode certificateContent from ASC")?;
    let certificate_pem = der_to_pem(&certificate_der);
    fs::write(&certificate_pem_path, certificate_pem.as_bytes()).with_context(|| {
        format!(
            "failed to write temporary certificate {}",
            certificate_pem_path.display()
        )
    })?;

    let status = Command::new("openssl")
        .arg("pkcs12")
        .arg("-export")
        .arg("-inkey")
        .arg(key_path)
        .arg("-in")
        .arg(&certificate_pem_path)
        .arg("-out")
        .arg(output_path)
        .arg("-passout")
        .arg(format!("pass:{password}"))
        .status()
        .context("failed to execute openssl pkcs12")?;

    ensure!(
        status.success(),
        "openssl pkcs12 failed with status {status}"
    );
    set_private_permissions(output_path)?;
    Ok(())
}

pub fn create_pkcs12_bytes(
    key_path: &Path,
    certificate_der_base64: &str,
    password: &str,
) -> Result<Vec<u8>> {
    let tempdir = tempfile::tempdir().context("failed to create temporary PKCS#12 output")?;
    let output_path = tempdir.path().join("certificate.p12");
    create_pkcs12(key_path, certificate_der_base64, &output_path, password)?;
    fs::read(&output_path).with_context(|| format!("failed to read {}", output_path.display()))
}

pub fn import_into_login_keychain(pkcs12_path: &Path, password: &str) -> Result<()> {
    let keychain = login_keychain_path()?;
    let mut command = Command::new("security");
    command
        .arg("import")
        .arg(pkcs12_path)
        .arg("-k")
        .arg(&keychain)
        .arg("-f")
        .arg("pkcs12")
        .arg("-P")
        .arg(password);
    for tool in KEYCHAIN_TOOLS {
        command.arg("-T").arg(tool);
    }

    let status = command
        .status()
        .context("failed to execute security import")?;
    ensure!(
        status.success(),
        "security import failed with status {status}"
    );
    Ok(())
}

pub fn import_pkcs12_bytes_into_login_keychain(
    logical_name: &str,
    pkcs12_bytes: &[u8],
    password: &str,
) -> Result<()> {
    let tempdir = tempfile::tempdir().context("failed to create temporary keychain import dir")?;
    let file_name = format!("{logical_name}.p12");
    let pkcs12_path = tempdir.path().join(file_name);
    fs::write(&pkcs12_path, pkcs12_bytes)
        .with_context(|| format!("failed to write {}", pkcs12_path.display()))?;
    set_private_permissions(&pkcs12_path)?;
    import_into_login_keychain(&pkcs12_path, password)
}

pub fn decode_profile(profile_content_base64: &str) -> Result<Vec<u8>> {
    STANDARD
        .decode(profile_content_base64)
        .context("failed to decode profileContent from ASC")
}

pub fn install_profile_bytes(uuid: &str, profile_bytes: &[u8]) -> Result<PathBuf> {
    ensure!(!uuid.trim().is_empty(), "profile uuid cannot be empty");
    let output_dir = provisioning_profiles_dir()?;
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    let output_path = output_dir.join(format!("{uuid}.mobileprovision"));
    fs::write(&output_path, profile_bytes)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    set_private_permissions(&output_path)?;
    Ok(output_path)
}

pub fn pkcs12_is_expired(pkcs12_bytes: &[u8], password: &str) -> Result<bool> {
    let tempdir =
        tempfile::tempdir().context("failed to create temporary PKCS#12 validation dir")?;
    let pkcs12_path = tempdir.path().join("certificate.p12");
    let certificate_pem_path = tempdir.path().join("certificate.pem");
    fs::write(&pkcs12_path, pkcs12_bytes)
        .with_context(|| format!("failed to write {}", pkcs12_path.display()))?;
    set_private_permissions(&pkcs12_path)?;

    let extract_status = Command::new("openssl")
        .arg("pkcs12")
        .arg("-in")
        .arg(&pkcs12_path)
        .arg("-clcerts")
        .arg("-nokeys")
        .arg("-out")
        .arg(&certificate_pem_path)
        .arg("-passin")
        .arg(format!("pass:{password}"))
        .status()
        .context("failed to execute openssl pkcs12 for validation")?;
    ensure!(
        extract_status.success(),
        "openssl pkcs12 failed with status {extract_status}"
    );

    let check_output = Command::new("openssl")
        .arg("x509")
        .arg("-in")
        .arg(&certificate_pem_path)
        .arg("-noout")
        .arg("-checkend")
        .arg("0")
        .output()
        .context("failed to execute openssl x509 -checkend")?;
    if check_output.status.success() {
        return Ok(false);
    }
    if check_output.status.code() == Some(1) {
        return Ok(true);
    }

    Err(anyhow::anyhow!(
        "openssl x509 -checkend failed: {}",
        String::from_utf8_lossy(&check_output.stderr)
    ))
}

pub fn provisioning_profile_is_expired(profile_bytes: &[u8]) -> Result<bool> {
    let tempdir =
        tempfile::tempdir().context("failed to create temporary profile validation dir")?;
    let profile_path = tempdir.path().join("profile.mobileprovision");
    fs::write(&profile_path, profile_bytes)
        .with_context(|| format!("failed to write {}", profile_path.display()))?;
    set_private_permissions(&profile_path)?;

    let output = Command::new("security")
        .arg("cms")
        .arg("-D")
        .arg("-i")
        .arg(&profile_path)
        .output()
        .context("failed to execute security cms -D")?;
    ensure!(
        output.status.success(),
        "security cms -D failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let plist = PlistValue::from_reader(Cursor::new(output.stdout))
        .context("failed to parse provisioning profile plist")?;
    let dictionary = plist
        .as_dictionary()
        .ok_or_else(|| anyhow::anyhow!("provisioning profile root plist is not a dictionary"))?;
    let expiration = dictionary
        .get("ExpirationDate")
        .and_then(PlistValue::as_date)
        .ok_or_else(|| anyhow::anyhow!("provisioning profile is missing ExpirationDate"))?;

    let expiration_time: SystemTime = expiration.to_owned().into();
    Ok(expiration_time <= SystemTime::now())
}

pub fn load_cached_bundle_password(bundle_path: &Path, scope: Scope) -> Result<Option<String>> {
    let path = bundle_password_cache_path(bundle_path, scope)?;
    read_secret_file(&path)
}

pub fn store_cached_bundle_password(
    bundle_path: &Path,
    scope: Scope,
    password: &str,
) -> Result<()> {
    ensure!(
        !password.trim().is_empty(),
        "bundle password cannot be empty"
    );
    let path = bundle_password_cache_path(bundle_path, scope)?;
    write_secret_file(&path, password)
}

pub fn load_stored_asc_auth(team_id: &str) -> Result<Option<String>> {
    ensure!(!team_id.trim().is_empty(), "team ID cannot be empty");
    let path = auth_record_path(team_id)?;
    read_secret_file(&path)
}

pub fn store_stored_asc_auth(team_id: &str, secret: &str) -> Result<()> {
    ensure!(!team_id.trim().is_empty(), "team ID cannot be empty");
    ensure!(!secret.trim().is_empty(), "auth payload cannot be empty");
    let path = auth_record_path(team_id)?;
    write_secret_file(&path, secret)
}

pub fn list_stored_asc_auth_team_ids() -> Result<Vec<String>> {
    let directory = auth_storage_dir()?;
    if !directory.exists() {
        return Ok(Vec::new());
    }

    let mut team_ids = Vec::new();
    for entry in fs::read_dir(&directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read {}", directory.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            && !stem.trim().is_empty()
        {
            team_ids.push(stem.to_owned());
        }
    }
    team_ids.sort();
    team_ids.dedup();
    Ok(team_ids)
}

pub fn load_external_generic_password(service: &str, account: &str) -> Result<Option<String>> {
    ensure!(!service.trim().is_empty(), "service cannot be empty");
    ensure!(!account.trim().is_empty(), "account cannot be empty");
    load_generic_password(service, account)
}

fn load_generic_password(service: &str, account: &str) -> Result<Option<String>> {
    let output = Command::new("security")
        .arg("find-generic-password")
        .arg("-s")
        .arg(service)
        .arg("-a")
        .arg(account)
        .arg("-w")
        .output()
        .context("failed to execute security find-generic-password")?;

    if output.status.success() {
        let password = String::from_utf8(output.stdout)
            .context("generic password is not valid UTF-8")?
            .trim_end_matches(['\n', '\r'])
            .to_owned();
        ensure!(!password.trim().is_empty(), "generic password is empty");
        return Ok(Some(password));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(44) || stderr.contains("could not be found") {
        return Ok(None);
    }

    Err(anyhow::anyhow!(
        "security find-generic-password failed for service {service}: {stderr}"
    ))
}

fn global_asc_sync_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(ASC_SYNC_DIR_NAME))
}

fn auth_storage_dir() -> Result<PathBuf> {
    Ok(global_asc_sync_dir()?.join(AUTH_DIR_NAME))
}

fn auth_record_path(team_id: &str) -> Result<PathBuf> {
    Ok(auth_storage_dir()?.join(format!("{team_id}.json")))
}

fn bundle_password_cache_path(bundle_path: &Path, scope: Scope) -> Result<PathBuf> {
    let absolute_path = if bundle_path.is_absolute() {
        bundle_path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for bundle password cache")?
            .join(bundle_path)
    };
    let encoded = URL_SAFE_NO_PAD.encode(absolute_path.to_string_lossy().as_bytes());
    Ok(global_asc_sync_dir()?
        .join(BUNDLE_PASSWORDS_DIR_NAME)
        .join(format!("{encoded}-{}.txt", scope.bundle_segment())))
}

fn write_secret_file(path: &Path, secret: &str) -> Result<()> {
    ensure!(!secret.trim().is_empty(), "secret cannot be empty");
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent)?;
    }
    fs::write(path, secret).with_context(|| format!("failed to write {}", path.display()))?;
    set_private_permissions(path)?;
    Ok(())
}

fn read_secret_file(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let secret =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let secret = secret.trim_end_matches(['\n', '\r']).to_owned();
    ensure!(
        !secret.trim().is_empty(),
        "secret file {} is empty",
        path.display()
    );
    Ok(Some(secret))
}

fn create_private_dir_all(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    set_private_dir_permissions(path)?;
    Ok(())
}

fn set_private_dir_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let permissions = fs::Permissions::from_mode(0o700);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }

    Ok(())
}

fn der_to_pem(der: &[u8]) -> String {
    let body = STANDARD.encode(der);
    let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END CERTIFICATE-----\n");
    pem
}
