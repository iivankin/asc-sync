use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, IsTerminal, Read, Write},
    path::Path,
};

use age::{
    Decryptor, Encryptor,
    secrecy::{ExposeSecret, SecretString},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use tar::{Archive, Builder, Header};
use tempfile::NamedTempFile;

use crate::{
    scope::Scope,
    state::{
        ManagedBundleId, ManagedCertificate, ManagedDevice, ManagedProfile, State,
        set_private_permissions,
    },
    sync::RuntimeWorkspace,
    system,
};

pub const BUNDLE_FILE_NAME: &str = "signing.ascbundle";
pub const DEVELOPER_BUNDLE_PASSWORD_ENV: &str = "ASC_DEVELOPER_BUNDLE_PASSWORD";
pub const RELEASE_BUNDLE_PASSWORD_ENV: &str = "ASC_RELEASE_BUNDLE_PASSWORD";

const BUNDLE_VERSION: u32 = 6;
const MANIFEST_PATH: &str = "manifest.json";
const SCOPES_DIR: &str = "scopes";
const PAYLOAD_FILE_NAME: &str = "payload.age";
const STATE_PATH: &str = "state.json";
const CERTS_DIR: &str = "certs";
const CERT_PASSWORDS_PATH: &str = "cert-passwords.json";
const PROFILES_DIR: &str = "profiles";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopeArtifacts {
    certs: BTreeMap<String, Vec<u8>>,
    cert_passwords: BTreeMap<String, String>,
    profiles: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BundleContents {
    state: State,
    scopes: BTreeMap<Scope, Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SharedState {
    version: u32,
    team_id: String,
    bundle_ids: BTreeMap<String, ManagedBundleId>,
    devices: BTreeMap<String, ManagedDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ScopeSigningState {
    certs: BTreeMap<String, ManagedCertificate>,
    profiles: BTreeMap<String, ManagedProfile>,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BundleManifest {
    version: u32,
}

pub fn bootstrap_bundle(
    bundle_path: &Path,
    team_id: &str,
) -> Result<BTreeMap<Scope, SecretString>> {
    ensure!(
        !bundle_path.exists(),
        "signing bundle {} already exists",
        bundle_path.display()
    );

    let passwords = Scope::ALL
        .into_iter()
        .map(|scope| (scope, generate_bundle_password()))
        .collect::<BTreeMap<_, _>>();
    initialize_bundle(bundle_path, team_id, &passwords)?;

    for (scope, password) in &passwords {
        system::store_cached_bundle_password(bundle_path, *scope, password.expose_secret())?;
    }

    Ok(passwords)
}

pub fn initialize_bundle(
    bundle_path: &Path,
    team_id: &str,
    passwords: &BTreeMap<Scope, SecretString>,
) -> Result<()> {
    validate_password_set(passwords)?;

    let mut bundle = BundleContents {
        state: State::new(team_id),
        scopes: BTreeMap::new(),
    };
    for scope in Scope::ALL {
        let payload = encode_scope_payload(&empty_scope_artifacts())?;
        let encrypted = encrypt_scope_payload(&payload, password_for_scope(passwords, scope)?)?;
        bundle.scopes.insert(scope, encrypted);
    }

    write_bundle(bundle_path, &bundle)
}

pub fn resolve_existing_passwords(
    bundle_path: &Path,
    scopes: &[Scope],
) -> Result<BTreeMap<Scope, SecretString>> {
    let bundle = read_bundle(bundle_path)?;
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let mut unlocked = BTreeMap::new();

    for scope in scopes {
        if let Some(password) = resolve_scope_password(bundle_path, &bundle, *scope, interactive)? {
            unlocked.insert(*scope, password);
        }
    }

    Ok(unlocked)
}

pub fn restore_scope(
    runtime: &mut RuntimeWorkspace,
    bundle_path: &Path,
    scope: Scope,
    password: &SecretString,
) -> Result<State> {
    let bundle = read_bundle(bundle_path)?;
    let state = bundle.state.clone();
    let artifacts = unlock_scope_artifacts(&bundle, scope, password)?;
    validate_artifact_completeness(&state, scope, &artifacts)?;

    let runtime_certs = state
        .certs
        .keys()
        .filter(|logical_name| {
            state
                .certs
                .get(*logical_name)
                .is_some_and(|managed| managed_certificate_scope(&managed.kind) == Some(scope))
        })
        .map(|logical_name| {
            let file_name = certificate_file_name(logical_name);
            let bytes = artifacts.certs.get(&file_name).cloned().ok_or_else(|| {
                anyhow::anyhow!("scope payload is missing certificate artifact {file_name}")
            })?;
            Ok((logical_name.clone(), bytes))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let runtime_cert_passwords = state
        .certs
        .keys()
        .filter(|logical_name| {
            state
                .certs
                .get(*logical_name)
                .is_some_and(|managed| managed_certificate_scope(&managed.kind) == Some(scope))
        })
        .map(|logical_name| {
            let password = artifacts
                .cert_passwords
                .get(logical_name)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "scope payload is missing PKCS#12 password for cert {logical_name}"
                    )
                })?;
            Ok((logical_name.clone(), password))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let runtime_profiles = state
        .profiles
        .keys()
        .filter(|logical_name| {
            state
                .profiles
                .get(*logical_name)
                .is_some_and(|managed| managed_profile_scope(&managed.kind) == Some(scope))
        })
        .map(|logical_name| {
            let file_name = profile_file_name(logical_name);
            let bytes = artifacts.profiles.get(&file_name).cloned().ok_or_else(|| {
                anyhow::anyhow!("scope payload is missing profile artifact {file_name}")
            })?;
            Ok((logical_name.clone(), bytes))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    runtime.replace_artifacts(runtime_certs, runtime_cert_passwords, runtime_profiles);
    Ok(state)
}

pub fn load_state(bundle_path: &Path) -> Result<State> {
    let bundle = read_bundle(bundle_path)?;
    Ok(bundle.state)
}

pub fn write_scope(
    bundle_path: &Path,
    runtime: &RuntimeWorkspace,
    scope: Scope,
    state: &State,
    password: &SecretString,
) -> Result<()> {
    ensure!(
        bundle_path.exists(),
        "signing bundle {} does not exist",
        bundle_path.display()
    );

    let mut bundle = read_bundle(bundle_path)?;
    bundle.state = state.clone();
    let artifacts = build_scope_artifacts_from_runtime(runtime, state, scope)?;
    let payload = encode_scope_payload(&artifacts)?;
    let encrypted = encrypt_scope_payload(&payload, password)?;
    bundle.scopes.insert(scope, encrypted);
    write_bundle(bundle_path, &bundle)
}

pub fn merge_signing_bundle(
    bundle_path: &Path,
    base_bundle_path: &Path,
    ours_bundle_path: &Path,
    theirs_bundle_path: &Path,
) -> Result<()> {
    let base = read_bundle(base_bundle_path)?;
    let ours = read_bundle(ours_bundle_path)?;
    let theirs = read_bundle(theirs_bundle_path)?;
    let mut resolver = MergeResolver::new();
    let merged = merge_bundle_contents(base, ours, theirs, &mut resolver)?;
    write_bundle(bundle_path, &merged)
}

fn empty_scope_artifacts() -> ScopeArtifacts {
    ScopeArtifacts {
        certs: BTreeMap::new(),
        cert_passwords: BTreeMap::new(),
        profiles: BTreeMap::new(),
    }
}

fn validate_password_set(passwords: &BTreeMap<Scope, SecretString>) -> Result<()> {
    for scope in Scope::ALL {
        let password = password_for_scope(passwords, scope)?;
        ensure!(
            !password.expose_secret().trim().is_empty(),
            "{scope} bundle password cannot be empty"
        );
    }

    let mut unique = passwords
        .values()
        .map(|password| password.expose_secret().to_owned())
        .collect::<Vec<_>>();
    unique.sort();
    unique.dedup();
    ensure!(
        unique.len() == Scope::ALL.len(),
        "developer and release bundle passwords must be different"
    );

    Ok(())
}

fn password_for_scope(
    passwords: &BTreeMap<Scope, SecretString>,
    scope: Scope,
) -> Result<&SecretString> {
    passwords
        .get(&scope)
        .ok_or_else(|| anyhow::anyhow!("missing {scope} bundle password"))
}

fn resolve_scope_password(
    bundle_path: &Path,
    bundle: &BundleContents,
    scope: Scope,
    interactive: bool,
) -> Result<Option<SecretString>> {
    let env_name = scope_password_env(scope);
    if let Some(password) = env_password(env_name)? {
        validate_scope_password(bundle, scope, &password)
            .with_context(|| format!("{env_name} did not unlock {scope} signing section"))?;
        system::store_cached_bundle_password(bundle_path, scope, password.expose_secret())?;
        return Ok(Some(password));
    }

    if let Some(cached) = system::load_cached_bundle_password(bundle_path, scope)? {
        let password = SecretString::from(cached);
        if validate_scope_password(bundle, scope, &password).is_ok() {
            return Ok(Some(password));
        }
    }

    if !interactive {
        return Ok(None);
    }

    let prompt = format!(
        "{} bundle password (leave blank to skip): ",
        scope_prompt_label(scope)
    );
    let Some(password) = prompt_optional_password(&prompt)? else {
        return Ok(None);
    };
    validate_scope_password(bundle, scope, &password)
        .with_context(|| format!("entered password did not unlock {scope} signing section"))?;
    system::store_cached_bundle_password(bundle_path, scope, password.expose_secret())?;
    Ok(Some(password))
}

fn validate_scope_password(
    bundle: &BundleContents,
    scope: Scope,
    password: &SecretString,
) -> Result<()> {
    unlock_scope_artifacts(bundle, scope, password).map(|_| ())
}

fn build_scope_artifacts_from_runtime(
    runtime: &RuntimeWorkspace,
    state: &State,
    scope: Scope,
) -> Result<ScopeArtifacts> {
    let mut certs = BTreeMap::new();
    let mut cert_passwords = BTreeMap::new();
    for (logical_name, managed) in &state.certs {
        if managed_certificate_scope(&managed.kind) != Some(scope) {
            continue;
        }
        let data = runtime
            .cert_bytes(logical_name)
            .ok_or_else(|| anyhow::anyhow!("missing PKCS#12 artifact for cert {logical_name}"))?;
        certs.insert(certificate_file_name(logical_name), data.to_vec());
        cert_passwords.insert(
            logical_name.clone(),
            runtime
                .cert_password(logical_name)
                .ok_or_else(|| anyhow::anyhow!("missing PKCS#12 password for cert {logical_name}"))?
                .to_owned(),
        );
    }

    let mut profiles = BTreeMap::new();
    for (logical_name, managed) in &state.profiles {
        if managed_profile_scope(&managed.kind) != Some(scope) {
            continue;
        }
        let data = runtime.profile_bytes(logical_name).ok_or_else(|| {
            anyhow::anyhow!("missing profile artifact for profile {logical_name}")
        })?;
        profiles.insert(profile_file_name(logical_name), data.to_vec());
    }

    Ok(ScopeArtifacts {
        certs,
        cert_passwords,
        profiles,
    })
}

fn read_bundle(bundle_path: &Path) -> Result<BundleContents> {
    let bytes = fs::read(bundle_path)
        .with_context(|| format!("failed to read signing bundle {}", bundle_path.display()))?;
    let mut archive = Archive::new(Cursor::new(bytes));
    let mut manifest = None;
    let mut state = None;
    let mut bundle = BundleContents {
        state: State::new(""),
        scopes: BTreeMap::new(),
    };

    for entry in archive
        .entries()
        .context("failed to read signing bundle archive")?
    {
        let mut entry = entry.context("failed to read signing bundle entry")?;
        let path = entry
            .path()
            .context("failed to read signing bundle entry path")?
            .to_path_buf();
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read bundle entry {}", path.display()))?;

        match path_components(&path).as_slice() {
            [name] if *name == MANIFEST_PATH => {
                manifest = Some(
                    serde_json::from_slice::<BundleManifest>(&bytes)
                        .context("failed to parse bundle manifest")?,
                );
            }
            [name] if *name == STATE_PATH => {
                state = Some(State::from_slice(&bytes).context("failed to parse bundle state")?);
            }
            [root, scope_name, name] if *root == SCOPES_DIR && *name == PAYLOAD_FILE_NAME => {
                let scope = parse_scope(scope_name)?;
                bundle.scopes.insert(scope, bytes);
            }
            _ => bail!("unsupported path in signing bundle: {}", path.display()),
        }
    }

    let manifest = manifest.context("signing bundle is missing manifest.json")?;
    ensure!(
        manifest.version == BUNDLE_VERSION,
        "unsupported signing bundle version {}",
        manifest.version
    );
    bundle.state = state.context("signing bundle is missing state.json")?;
    for scope in Scope::ALL {
        ensure!(
            bundle.scopes.contains_key(&scope),
            "signing bundle is missing {scope} scope payload"
        );
    }
    Ok(bundle)
}

fn write_bundle(bundle_path: &Path, contents: &BundleContents) -> Result<()> {
    let archive = encode_bundle_archive(contents)?;

    if let Some(parent) = bundle_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut temp = NamedTempFile::new_in(bundle_path.parent().unwrap_or_else(|| Path::new(".")))
        .context("failed to create temporary signing bundle")?;
    temp.write_all(&archive)
        .context("failed to write signing bundle")?;
    set_private_permissions(temp.path())?;
    temp.persist(bundle_path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to persist signing bundle {}", bundle_path.display()))?;
    set_private_permissions(bundle_path)?;
    Ok(())
}

fn encode_bundle_archive(contents: &BundleContents) -> Result<Vec<u8>> {
    let mut archive = Vec::new();
    {
        let mut builder = Builder::new(&mut archive);
        append_json_entry(
            &mut builder,
            MANIFEST_PATH,
            &BundleManifest {
                version: BUNDLE_VERSION,
            },
        )?;
        append_json_entry(&mut builder, STATE_PATH, &contents.state)?;

        for scope in Scope::ALL {
            let encrypted_payload = contents
                .scopes
                .get(&scope)
                .ok_or_else(|| anyhow::anyhow!("missing {scope} scope payload"))?;
            let path = scoped_entry_path(scope, PAYLOAD_FILE_NAME);
            append_bytes_entry(&mut builder, &path, encrypted_payload)?;
        }

        builder
            .finish()
            .context("failed to finalize signing bundle archive")?;
    }
    Ok(archive)
}

fn encode_scope_payload(contents: &ScopeArtifacts) -> Result<Vec<u8>> {
    let mut archive = Vec::new();
    {
        let mut builder = Builder::new(&mut archive);
        append_json_entry(&mut builder, CERT_PASSWORDS_PATH, &contents.cert_passwords)?;
        append_scope_file_entries(&mut builder, CERTS_DIR, &contents.certs)?;
        append_scope_file_entries(&mut builder, PROFILES_DIR, &contents.profiles)?;
        builder
            .finish()
            .context("failed to finalize scope payload archive")?;
    }
    Ok(archive)
}

fn decode_scope_payload(bytes: &[u8]) -> Result<ScopeArtifacts> {
    let mut archive = Archive::new(Cursor::new(bytes));
    let mut certs = BTreeMap::new();
    let mut cert_passwords = None;
    let mut profiles = BTreeMap::new();

    for entry in archive
        .entries()
        .context("failed to read scope payload archive")?
    {
        let mut entry = entry.context("failed to read scope payload entry")?;
        let path = entry
            .path()
            .context("failed to read scope payload entry path")?
            .to_path_buf();
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed to read scope payload entry {}", path.display()))?;

        match path_components(&path).as_slice() {
            [name] if *name == CERT_PASSWORDS_PATH => {
                cert_passwords = Some(
                    serde_json::from_slice::<BTreeMap<String, String>>(&bytes)
                        .context("failed to parse scope cert-passwords metadata")?,
                );
            }
            [directory, file_name] if *directory == CERTS_DIR => {
                certs.insert((*file_name).to_owned(), bytes);
            }
            [directory, file_name] if *directory == PROFILES_DIR => {
                profiles.insert((*file_name).to_owned(), bytes);
            }
            _ => bail!("unsupported path in scope payload: {}", path.display()),
        }
    }

    let contents = ScopeArtifacts {
        certs,
        cert_passwords: cert_passwords.context("scope payload is missing cert-passwords.json")?,
        profiles,
    };
    Ok(contents)
}

fn append_json_entry<T: Serialize>(
    builder: &mut Builder<&mut Vec<u8>>,
    path: &str,
    value: &T,
) -> Result<()> {
    let data = serde_json::to_vec_pretty(value).context("failed to serialize bundle entry")?;
    append_bytes_entry(builder, path, &data)
}

fn append_scope_file_entries(
    builder: &mut Builder<&mut Vec<u8>>,
    directory: &str,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    for (file_name, data) in files {
        let path = format!("{directory}/{file_name}");
        append_bytes_entry(builder, &path, data)?;
    }
    Ok(())
}

fn append_bytes_entry(builder: &mut Builder<&mut Vec<u8>>, path: &str, data: &[u8]) -> Result<()> {
    let mut header = Header::new_gnu();
    header.set_mode(0o600);
    header.set_size(data.len() as u64);
    header.set_cksum();
    builder
        .append_data(&mut header, path, Cursor::new(data))
        .with_context(|| format!("failed to append {path} to bundle archive"))?;
    Ok(())
}

fn encrypt_scope_payload(archive: &[u8], password: &SecretString) -> Result<Vec<u8>> {
    let encryptor = Encryptor::with_user_passphrase(password.clone());
    let mut encrypted = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut encrypted)
        .context("failed to initialize scope payload encryption")?;
    writer
        .write_all(archive)
        .context("failed to encrypt scope payload")?;
    writer
        .finish()
        .context("failed to finalize scope payload encryption")?;
    Ok(encrypted)
}

fn decrypt_scope_payload(encrypted: &[u8], password: &SecretString) -> Result<Vec<u8>> {
    let decryptor = Decryptor::new(encrypted).context("failed to parse encrypted scope payload")?;
    let identity = age::scrypt::Identity::new(password.clone());
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .context("failed to decrypt scope payload")?;
    let mut decrypted = Vec::new();
    reader
        .read_to_end(&mut decrypted)
        .context("failed to read decrypted scope payload")?;
    Ok(decrypted)
}

fn unlock_scope_artifacts(
    bundle: &BundleContents,
    scope: Scope,
    password: &SecretString,
) -> Result<ScopeArtifacts> {
    let encrypted_payload = bundle
        .scopes
        .get(&scope)
        .ok_or_else(|| anyhow::anyhow!("signing bundle is missing {scope} scope payload"))?;
    let payload = decrypt_scope_payload(encrypted_payload, password)?;
    decode_scope_payload(&payload)
}

fn merge_bundle_contents(
    base: BundleContents,
    ours: BundleContents,
    theirs: BundleContents,
    resolver: &mut MergeResolver,
) -> Result<BundleContents> {
    let merged_shared = merge_shared_state(
        shared_state_view(&base.state),
        shared_state_view(&ours.state),
        shared_state_view(&theirs.state),
        resolver,
    )?;
    let mut scopes = BTreeMap::new();
    let mut certs = BTreeMap::new();
    let mut profiles = BTreeMap::new();

    for scope in Scope::ALL {
        let merged_scope = merge_scope_signing_state(
            scope,
            scope_signing_state(&base, scope)?,
            scope_signing_state(&ours, scope)?,
            scope_signing_state(&theirs, scope)?,
            resolver,
        )?;
        certs.extend(merged_scope.certs);
        profiles.extend(merged_scope.profiles);
        scopes.insert(scope, merged_scope.payload);
    }

    Ok(BundleContents {
        state: State {
            version: merged_shared.version,
            team_id: merged_shared.team_id,
            bundle_ids: merged_shared.bundle_ids,
            devices: merged_shared.devices,
            certs,
            profiles,
        },
        scopes,
    })
}

fn merge_shared_state(
    base: SharedState,
    ours: SharedState,
    theirs: SharedState,
    resolver: &mut MergeResolver,
) -> Result<SharedState> {
    Ok(SharedState {
        version: merge_scalar(
            "state.version",
            &base.version,
            &ours.version,
            &theirs.version,
            resolver,
        )?,
        team_id: merge_scalar(
            "state.team_id",
            &base.team_id,
            &ours.team_id,
            &theirs.team_id,
            resolver,
        )?,
        bundle_ids: merge_map(
            "state.bundle_ids",
            &base.bundle_ids,
            &ours.bundle_ids,
            &theirs.bundle_ids,
            resolver,
        )?,
        devices: merge_map(
            "state.devices",
            &base.devices,
            &ours.devices,
            &theirs.devices,
            resolver,
        )?,
    })
}

fn merge_scope_signing_state(
    scope: Scope,
    base: ScopeSigningState,
    ours: ScopeSigningState,
    theirs: ScopeSigningState,
    resolver: &mut MergeResolver,
) -> Result<ScopeSigningState> {
    merge_optional(
        &format!("state+payload.{scope}"),
        Some(&base),
        Some(&ours),
        Some(&theirs),
        resolver,
    )?
    .ok_or_else(|| anyhow::anyhow!("state+payload.{scope} unexpectedly resolved to no value"))
}

fn shared_state_view(state: &State) -> SharedState {
    SharedState {
        version: state.version,
        team_id: state.team_id.clone(),
        bundle_ids: state.bundle_ids.clone(),
        devices: state.devices.clone(),
    }
}

fn scope_signing_state(bundle: &BundleContents, scope: Scope) -> Result<ScopeSigningState> {
    let payload = bundle
        .scopes
        .get(&scope)
        .ok_or_else(|| anyhow::anyhow!("bundle is missing {scope} scope"))?
        .clone();
    let certs = bundle
        .state
        .certs
        .iter()
        .filter(|(_, managed)| managed_certificate_scope(&managed.kind) == Some(scope))
        .map(|(logical_name, managed)| (logical_name.clone(), managed.clone()))
        .collect();
    let profiles = bundle
        .state
        .profiles
        .iter()
        .filter(|(_, managed)| managed_profile_scope(&managed.kind) == Some(scope))
        .map(|(logical_name, managed)| (logical_name.clone(), managed.clone()))
        .collect();

    Ok(ScopeSigningState {
        certs,
        profiles,
        payload,
    })
}

fn merge_map<T: Clone + Eq + Serialize>(
    label: &str,
    base: &BTreeMap<String, T>,
    ours: &BTreeMap<String, T>,
    theirs: &BTreeMap<String, T>,
    resolver: &mut MergeResolver,
) -> Result<BTreeMap<String, T>> {
    let mut merged = BTreeMap::new();
    let keys = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    for key in keys {
        if let Some(value) = merge_optional(
            &format!("{label}.{key}"),
            base.get(&key),
            ours.get(&key),
            theirs.get(&key),
            resolver,
        )? {
            merged.insert(key, value);
        }
    }

    Ok(merged)
}

fn merge_optional<T: Clone + Eq + Serialize>(
    label: &str,
    base: Option<&T>,
    ours: Option<&T>,
    theirs: Option<&T>,
    resolver: &mut MergeResolver,
) -> Result<Option<T>> {
    if ours == theirs {
        return Ok(ours.cloned());
    }
    if ours == base {
        return Ok(theirs.cloned());
    }
    if theirs == base {
        return Ok(ours.cloned());
    }

    resolver.resolve_value_conflict(label, base, ours, theirs)
}

fn merge_scalar<T: Clone + Eq + Serialize>(
    label: &str,
    base: &T,
    ours: &T,
    theirs: &T,
    resolver: &mut MergeResolver,
) -> Result<T> {
    merge_optional(label, Some(base), Some(ours), Some(theirs), resolver)?
        .ok_or_else(|| anyhow::anyhow!("{label} unexpectedly resolved to no value"))
}

struct MergeResolver {
    interactive: bool,
}

impl MergeResolver {
    fn new() -> Self {
        Self {
            interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        }
    }

    fn resolve_value_conflict<T: Clone + Serialize>(
        &mut self,
        label: &str,
        base: Option<&T>,
        ours: Option<&T>,
        theirs: Option<&T>,
    ) -> Result<Option<T>> {
        let choice = self.prompt_choice(
            label,
            &render_merge_value("base", base)?,
            &render_merge_value("ours", ours)?,
            &render_merge_value("theirs", theirs)?,
        )?;
        Ok(match choice {
            MergeChoice::Base => base.cloned(),
            MergeChoice::Ours => ours.cloned(),
            MergeChoice::Theirs => theirs.cloned(),
        })
    }

    fn prompt_choice(
        &mut self,
        label: &str,
        base: &str,
        ours: &str,
        theirs: &str,
    ) -> Result<MergeChoice> {
        if !self.interactive {
            bail!(
                "{label} has conflicting changes in ours and theirs; rerun `signing merge` in an interactive terminal to choose"
            );
        }

        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "Conflict: {label}")?;
        writeln!(stdout, "{base}")?;
        writeln!(stdout, "{ours}")?;
        writeln!(stdout, "{theirs}")?;

        loop {
            write!(stdout, "Choose [b]ase/[o]urs/[t]heirs: ")?;
            stdout.flush()?;

            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            match input.trim().to_ascii_lowercase().as_str() {
                "b" | "base" => return Ok(MergeChoice::Base),
                "o" | "ours" => return Ok(MergeChoice::Ours),
                "t" | "theirs" => return Ok(MergeChoice::Theirs),
                _ => {
                    writeln!(stdout, "Enter one of: b, o, t")?;
                }
            }
        }
    }
}

enum MergeChoice {
    Base,
    Ours,
    Theirs,
}

fn render_merge_value<T: Serialize>(label: &str, value: Option<&T>) -> Result<String> {
    let rendered = match value {
        Some(value) => {
            serde_json::to_string_pretty(value).context("failed to render merge value")?
        }
        None => "<absent>".to_owned(),
    };
    Ok(format!("{label}: {rendered}"))
}

fn validate_artifact_completeness(
    state: &State,
    scope: Scope,
    artifacts: &ScopeArtifacts,
) -> Result<()> {
    for (logical_name, managed) in &state.certs {
        if managed_certificate_scope(&managed.kind) != Some(scope) {
            continue;
        }
        let file_name = certificate_file_name(logical_name);
        ensure!(
            artifacts.certs.contains_key(&file_name),
            "scope payload is missing certificate artifact {file_name}"
        );
        ensure!(
            artifacts.cert_passwords.contains_key(logical_name),
            "scope payload is missing PKCS#12 password for cert {logical_name}"
        );
    }
    for (logical_name, managed) in &state.profiles {
        if managed_profile_scope(&managed.kind) != Some(scope) {
            continue;
        }
        let file_name = profile_file_name(logical_name);
        ensure!(
            artifacts.profiles.contains_key(&file_name),
            "scope payload is missing profile artifact {file_name}"
        );
    }
    Ok(())
}

fn managed_certificate_scope(kind: &str) -> Option<Scope> {
    match kind {
        "DEVELOPMENT" => Some(Scope::Developer),
        "DISTRIBUTION" | "DEVELOPER_ID_APPLICATION" => Some(Scope::Release),
        _ => None,
    }
}

fn managed_profile_scope(kind: &str) -> Option<Scope> {
    match kind {
        "IOS_APP_DEVELOPMENT"
        | "IOS_APP_ADHOC"
        | "TVOS_APP_DEVELOPMENT"
        | "TVOS_APP_ADHOC"
        | "MAC_APP_DEVELOPMENT"
        | "MAC_CATALYST_APP_DEVELOPMENT" => Some(Scope::Developer),
        "IOS_APP_STORE"
        | "IOS_APP_INHOUSE"
        | "TVOS_APP_STORE"
        | "TVOS_APP_INHOUSE"
        | "MAC_APP_STORE"
        | "MAC_APP_DIRECT"
        | "MAC_CATALYST_APP_STORE"
        | "MAC_CATALYST_APP_DIRECT" => Some(Scope::Release),
        _ => None,
    }
}

fn env_password(name: &str) -> Result<Option<SecretString>> {
    match std::env::var(name) {
        Ok(value) => {
            ensure!(!value.trim().is_empty(), "{name} cannot be empty");
            Ok(Some(SecretString::from(value)))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {name}")),
    }
}

fn prompt_optional_password(prompt: &str) -> Result<Option<SecretString>> {
    let password = rpassword::prompt_password(prompt)
        .with_context(|| format!("failed to read password prompt {prompt:?}"))?;
    if password.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(SecretString::from(password)))
}

fn scope_prompt_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Developer => "Developer",
        Scope::Release => "Release",
    }
}

fn scope_password_env(scope: Scope) -> &'static str {
    match scope {
        Scope::Developer => DEVELOPER_BUNDLE_PASSWORD_ENV,
        Scope::Release => RELEASE_BUNDLE_PASSWORD_ENV,
    }
}

fn generate_bundle_password() -> SecretString {
    SecretString::from(URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()))
}

fn path_components(path: &Path) -> Vec<&str> {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect()
}

fn parse_scope(segment: &str) -> Result<Scope> {
    Scope::from_bundle_segment(segment)
        .ok_or_else(|| anyhow::anyhow!("unsupported signing bundle scope {segment}"))
}

fn scoped_entry_path(scope: Scope, relative_path: &str) -> String {
    format!("{SCOPES_DIR}/{}/{relative_path}", scope.bundle_segment())
}

fn certificate_file_name(logical_name: &str) -> String {
    format!("{logical_name}.p12")
}

fn profile_file_name(logical_name: &str) -> String {
    format!("{logical_name}.mobileprovision")
}

#[cfg(test)]
mod tests {
    use super::{Scope, initialize_bundle, merge_signing_bundle, restore_scope, write_scope};
    use crate::{
        state::{ManagedCertificate, State},
        sync::Workspace,
    };
    use age::secrecy::SecretString;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn passwords() -> BTreeMap<Scope, SecretString> {
        BTreeMap::from([
            (
                Scope::Developer,
                SecretString::from("developer-password".to_owned()),
            ),
            (
                Scope::Release,
                SecretString::from("release-password".to_owned()),
            ),
        ])
    }

    #[test]
    fn roundtrips_scope_bundle_with_runtime_workspace() {
        let tempdir = tempdir().unwrap();
        let workspace = Workspace::from_config_path(&tempdir.path().join("asc.json"));
        let passwords = passwords();
        initialize_bundle(&workspace.bundle_path, "TEAM123", &passwords).unwrap();

        let mut runtime = workspace.create_runtime().unwrap();

        let mut state = State::new("TEAM123");
        runtime.set_cert("dev", b"fake-p12".to_vec());
        runtime.set_cert_password("dev", "secret".into());
        state.certs.insert(
            "dev".into(),
            ManagedCertificate {
                apple_id: "cert".into(),
                kind: "DEVELOPMENT".into(),
                name: "Dev".into(),
                serial_number: "serial".into(),
                p12_password: "secret".into(),
            },
        );

        write_scope(
            &workspace.bundle_path,
            &runtime,
            Scope::Developer,
            &state,
            &passwords[&Scope::Developer],
        )
        .unwrap();

        let mut restored_runtime = workspace.create_runtime().unwrap();
        let restored = restore_scope(
            &mut restored_runtime,
            &workspace.bundle_path,
            Scope::Developer,
            &passwords[&Scope::Developer],
        )
        .unwrap();

        assert_eq!(restored.team_id, "TEAM123");
        assert_eq!(restored_runtime.cert_bytes("dev").unwrap(), b"fake-p12");
    }

    #[test]
    fn merge_combines_non_overlapping_scope_changes() {
        let tempdir = tempdir().unwrap();
        let workspace = Workspace::from_config_path(&tempdir.path().join("asc.json"));
        let base_bundle = tempdir.path().join("base.ascbundle");
        let our_bundle = tempdir.path().join("ours.ascbundle");
        let their_bundle = tempdir.path().join("theirs.ascbundle");
        let merged_bundle = tempdir.path().join("merged.ascbundle");
        let passwords = passwords();

        initialize_bundle(&base_bundle, "TEAM123", &passwords).unwrap();
        std::fs::copy(&base_bundle, &our_bundle).unwrap();
        std::fs::copy(&base_bundle, &their_bundle).unwrap();

        let mut runtime_a = workspace.create_runtime().unwrap();
        runtime_a.set_cert("dev", b"bundle-a".to_vec());
        runtime_a.set_cert_password("dev", "secret-a".into());
        let mut state_a = State::new("TEAM123");
        state_a.certs.insert(
            "dev".into(),
            ManagedCertificate {
                apple_id: "cert-a".into(),
                kind: "DEVELOPMENT".into(),
                name: "Dev".into(),
                serial_number: "serial-a".into(),
                p12_password: "secret-a".into(),
            },
        );
        write_scope(
            &our_bundle,
            &runtime_a,
            Scope::Developer,
            &state_a,
            &passwords[&Scope::Developer],
        )
        .unwrap();

        let release_workspace = Workspace::from_config_path(&tempdir.path().join("release.json"));
        let mut runtime_b = release_workspace.create_runtime().unwrap();
        runtime_b.set_cert("dist", b"bundle-b".to_vec());
        runtime_b.set_cert_password("dist", "secret-b".into());
        let mut state_b = State::new("TEAM123");
        state_b.certs.insert(
            "dist".into(),
            ManagedCertificate {
                apple_id: "cert-b".into(),
                kind: "DISTRIBUTION".into(),
                name: "Dist".into(),
                serial_number: "serial-b".into(),
                p12_password: "secret-b".into(),
            },
        );
        write_scope(
            &their_bundle,
            &runtime_b,
            Scope::Release,
            &state_b,
            &passwords[&Scope::Release],
        )
        .unwrap();

        merge_signing_bundle(&merged_bundle, &base_bundle, &our_bundle, &their_bundle).unwrap();

        let mut merged_runtime = workspace.create_runtime().unwrap();
        let merged_dev = restore_scope(
            &mut merged_runtime,
            &merged_bundle,
            Scope::Developer,
            &passwords[&Scope::Developer],
        )
        .unwrap();
        assert_eq!(merged_dev.team_id, "TEAM123");
        assert_eq!(merged_runtime.cert_bytes("dev").unwrap(), b"bundle-a");

        let mut merged_release_runtime = release_workspace.create_runtime().unwrap();
        let merged_release = restore_scope(
            &mut merged_release_runtime,
            &merged_bundle,
            Scope::Release,
            &passwords[&Scope::Release],
        )
        .unwrap();
        assert_eq!(merged_release.team_id, "TEAM123");
        assert_eq!(
            merged_release_runtime.cert_bytes("dist").unwrap(),
            b"bundle-b"
        );
    }

    #[test]
    fn merge_rejects_conflicting_scope_payloads() {
        let tempdir = tempdir().unwrap();
        let base_bundle = tempdir.path().join("base.ascbundle");
        let our_bundle = tempdir.path().join("ours.ascbundle");
        let their_bundle = tempdir.path().join("theirs.ascbundle");
        let merged_bundle = tempdir.path().join("merged.ascbundle");
        let passwords = passwords();

        initialize_bundle(&base_bundle, "TEAM123", &passwords).unwrap();
        std::fs::copy(&base_bundle, &our_bundle).unwrap();
        std::fs::copy(&base_bundle, &their_bundle).unwrap();

        let workspace = Workspace::from_config_path(&tempdir.path().join("ours.json"));
        let mut runtime_a = workspace.create_runtime().unwrap();
        runtime_a.set_cert("dev", b"ours".to_vec());
        runtime_a.set_cert_password("dev", "secret-a".into());
        let mut state_a = State::new("TEAM123");
        state_a.certs.insert(
            "dev".into(),
            ManagedCertificate {
                apple_id: "cert-a".into(),
                kind: "DEVELOPMENT".into(),
                name: "Dev".into(),
                serial_number: "serial-a".into(),
                p12_password: "secret-a".into(),
            },
        );
        write_scope(
            &our_bundle,
            &runtime_a,
            Scope::Developer,
            &state_a,
            &passwords[&Scope::Developer],
        )
        .unwrap();

        let mut runtime_b = workspace.create_runtime().unwrap();
        runtime_b.set_cert("dev", b"theirs".to_vec());
        runtime_b.set_cert_password("dev", "secret-b".into());
        let mut state_b = State::new("TEAM123");
        state_b.certs.insert(
            "dev".into(),
            ManagedCertificate {
                apple_id: "cert-b".into(),
                kind: "DEVELOPMENT".into(),
                name: "Dev".into(),
                serial_number: "serial-b".into(),
                p12_password: "secret-b".into(),
            },
        );
        write_scope(
            &their_bundle,
            &runtime_b,
            Scope::Developer,
            &state_b,
            &passwords[&Scope::Developer],
        )
        .unwrap();

        assert!(
            merge_signing_bundle(&merged_bundle, &base_bundle, &our_bundle, &their_bundle).is_err()
        );
    }
}
