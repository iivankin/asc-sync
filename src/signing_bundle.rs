use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::Path,
};

use age::secrecy::{ExposeSecret, SecretString};
use anyhow::{Context, Result, ensure};

use crate::{
    bundle,
    config::Config,
    scope::Scope,
    state::{ManagedCertificate, State},
    sync::Workspace,
    system,
};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AdoptSummary {
    pub adopted_certs: usize,
    pub skipped_certs: Vec<String>,
}

#[derive(Debug, Clone)]
struct CertificateAdoptPlan {
    logical_name: String,
    target_name: String,
    scope: Scope,
    source: ManagedCertificate,
}

pub fn adopt_certificates(
    workspace: &Workspace,
    config: &Config,
    source_bundle_path: &Path,
    source_passwords: &BTreeMap<Scope, SecretString>,
    target_passwords: &BTreeMap<Scope, SecretString>,
) -> Result<AdoptSummary> {
    ensure!(
        target_passwords.len() == Scope::ALL.len(),
        "adopt requires both target bundle passwords"
    );

    let source_state = bundle::load_state(source_bundle_path)?;
    source_state.ensure_team(&config.team_id)?;
    let plans = plan_certificate_adoptions(config, &source_state)?;
    ensure!(
        !plans.is_empty(),
        "source signing bundle has no reusable certificates matching current certs"
    );

    let planned_scopes = plans.iter().map(|plan| plan.scope).collect::<BTreeSet<_>>();
    for scope in &planned_scopes {
        ensure!(
            source_passwords.contains_key(scope),
            "missing source {scope} bundle password; cannot reuse certificates from that scope"
        );
    }

    let mut target_runtime = workspace.create_runtime()?;
    let mut target_state = State::new(&config.team_id);
    let mut summary = AdoptSummary::default();

    for scope in planned_scopes {
        let password = source_passwords
            .get(&scope)
            .expect("planned_scopes were validated against source_passwords");
        let mut source_runtime = workspace.create_runtime()?;
        bundle::restore_scope(&mut source_runtime, source_bundle_path, scope, password)?;

        for plan in plans.iter().filter(|plan| plan.scope == scope) {
            let pkcs12 = source_runtime
                .cert_bytes(&plan.logical_name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "source signing bundle is missing PKCS#12 artifact for cert {}",
                        plan.logical_name
                    )
                })?
                .to_vec();
            let p12_password = source_runtime
                .cert_password(&plan.logical_name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "source signing bundle is missing PKCS#12 password for cert {}",
                        plan.logical_name
                    )
                })?
                .to_owned();

            if system::pkcs12_is_expired(&pkcs12, &p12_password)? {
                summary.skipped_certs.push(plan.logical_name.clone());
                continue;
            }

            target_runtime.set_cert(plan.logical_name.clone(), pkcs12);
            target_runtime.set_cert_password(plan.logical_name.clone(), p12_password.clone());
            target_state.certs.insert(
                plan.logical_name.clone(),
                ManagedCertificate {
                    apple_id: plan.source.apple_id.clone(),
                    kind: plan.source.kind.clone(),
                    name: plan.target_name.clone(),
                    serial_number: plan.source.serial_number.clone(),
                    p12_password,
                },
            );
            summary.adopted_certs += 1;
        }
    }

    ensure!(
        summary.adopted_certs > 0,
        "source signing bundle had matching certificates, but all of them were expired"
    );

    bundle::initialize_bundle(&workspace.bundle_path, &config.team_id, target_passwords)?;
    for (&scope, password) in target_passwords {
        system::store_cached_bundle_password(
            &workspace.bundle_path,
            scope,
            password.expose_secret(),
        )?;
    }
    for scope in Scope::ALL {
        let password = target_passwords
            .get(&scope)
            .expect("target_passwords includes all scopes");
        bundle::write_scope(
            &workspace.bundle_path,
            &target_runtime,
            scope,
            &target_state,
            password,
        )?;
    }

    Ok(summary)
}

fn plan_certificate_adoptions(
    config: &Config,
    source_state: &State,
) -> Result<Vec<CertificateAdoptPlan>> {
    let mut plans = Vec::new();
    for (logical_name, spec) in &config.certs {
        let Some(source) = source_state.certs.get(logical_name) else {
            continue;
        };
        let expected_kind = spec.kind.managed_kind();
        ensure!(
            source.kind == expected_kind,
            "source signing bundle cert {logical_name} is {}, but current certs expects {expected_kind}",
            source.kind
        );
        let scope = managed_certificate_scope(&source.kind).ok_or_else(|| {
            anyhow::anyhow!(
                "source cert {logical_name} has unsupported kind {}",
                source.kind
            )
        })?;
        plans.push(CertificateAdoptPlan {
            logical_name: logical_name.clone(),
            target_name: spec.name.clone(),
            scope,
            source: source.clone(),
        });
    }
    Ok(plans)
}

pub fn inspect_bundle(
    workspace: &Workspace,
    bundle_path: &Path,
    current_config: &Config,
) -> Result<String> {
    ensure!(
        bundle_path.exists(),
        "signing bundle {} does not exist",
        bundle_path.display()
    );
    let state = bundle::load_state(bundle_path)
        .with_context(|| format!("failed to read signing bundle {}", bundle_path.display()))?;
    let passwords = bundle::resolve_existing_passwords(bundle_path, &scope_array())?;
    format_bundle_inspection(workspace, bundle_path, current_config, &state, &passwords)
}

#[derive(Debug, Clone)]
struct ScopeInspection {
    state: ScopeInspectionState,
    certs: BTreeMap<String, ArtifactInspection>,
    profiles: BTreeMap<String, ArtifactInspection>,
}

#[derive(Debug, Clone)]
enum ScopeInspectionState {
    Locked,
    Failed(String),
    Unlocked,
}

#[derive(Debug, Clone)]
struct ArtifactInspection {
    bytes_present: bool,
    password_present: Option<bool>,
    expiration: ExpirationInspection,
}

#[derive(Debug, Clone)]
enum ExpirationInspection {
    NotChecked,
    Valid,
    Expired,
    Unknown(String),
}

pub fn format_bundle_inspection(
    workspace: &Workspace,
    bundle_path: &Path,
    current_config: &Config,
    state: &State,
    passwords: &BTreeMap<Scope, SecretString>,
) -> Result<String> {
    let scopes = inspect_bundle_scopes(workspace, bundle_path, state, passwords)?;
    let warnings = collect_bundle_warnings(current_config, state, &scopes);
    let mut out = String::new();

    writeln!(out, "Signing bundle: {}", bundle_path.display())?;
    writeln!(out, "team_id: {}", state.team_id)?;
    writeln!(out, "state_version: {}", state.version)?;
    writeln!(
        out,
        "summary: {} bundle id(s), {} device(s), {} certificate(s), {} profile(s)",
        state.bundle_ids.len(),
        state.devices.len(),
        state.certs.len(),
        state.profiles.len()
    )?;
    writeln!(out)?;

    write_bundle_ids(&mut out, state)?;
    write_devices(&mut out, state)?;
    write_certificates(&mut out, state, &scopes)?;
    write_profiles(&mut out, state, &scopes)?;
    write_scope_status(&mut out, &scopes)?;
    write_warnings(&mut out, &warnings)?;
    write_adopt_hint(&mut out, bundle_path, current_config, state)?;

    Ok(out)
}

fn inspect_bundle_scopes(
    workspace: &Workspace,
    bundle_path: &Path,
    state: &State,
    passwords: &BTreeMap<Scope, SecretString>,
) -> Result<BTreeMap<Scope, ScopeInspection>> {
    let mut scopes = BTreeMap::new();
    for scope in Scope::ALL {
        let Some(password) = passwords.get(&scope) else {
            scopes.insert(
                scope,
                ScopeInspection {
                    state: ScopeInspectionState::Locked,
                    certs: BTreeMap::new(),
                    profiles: BTreeMap::new(),
                },
            );
            continue;
        };

        let mut runtime = workspace.create_runtime()?;
        match bundle::restore_scope(&mut runtime, bundle_path, scope, password) {
            Ok(_) => {
                let mut certs = BTreeMap::new();
                for (logical_name, cert) in &state.certs {
                    if managed_certificate_scope(&cert.kind) != Some(scope) {
                        continue;
                    }
                    let bytes = runtime.cert_bytes(logical_name);
                    let p12_password = runtime.cert_password(logical_name);
                    let expiration = match (bytes, p12_password) {
                        (Some(bytes), Some(p12_password)) => {
                            match system::pkcs12_is_expired(bytes, p12_password) {
                                Ok(true) => ExpirationInspection::Expired,
                                Ok(false) => ExpirationInspection::Valid,
                                Err(error) => ExpirationInspection::Unknown(error.to_string()),
                            }
                        }
                        _ => ExpirationInspection::NotChecked,
                    };
                    certs.insert(
                        logical_name.clone(),
                        ArtifactInspection {
                            bytes_present: bytes.is_some(),
                            password_present: Some(p12_password.is_some()),
                            expiration,
                        },
                    );
                }

                let mut profiles = BTreeMap::new();
                for (logical_name, profile) in &state.profiles {
                    if managed_profile_scope(&profile.kind) != Some(scope) {
                        continue;
                    }
                    let bytes = runtime.profile_bytes(logical_name);
                    let expiration = match bytes {
                        Some(bytes) => match system::provisioning_profile_is_expired(bytes) {
                            Ok(true) => ExpirationInspection::Expired,
                            Ok(false) => ExpirationInspection::Valid,
                            Err(error) => ExpirationInspection::Unknown(error.to_string()),
                        },
                        None => ExpirationInspection::NotChecked,
                    };
                    profiles.insert(
                        logical_name.clone(),
                        ArtifactInspection {
                            bytes_present: bytes.is_some(),
                            password_present: None,
                            expiration,
                        },
                    );
                }

                scopes.insert(
                    scope,
                    ScopeInspection {
                        state: ScopeInspectionState::Unlocked,
                        certs,
                        profiles,
                    },
                );
            }
            Err(error) => {
                scopes.insert(
                    scope,
                    ScopeInspection {
                        state: ScopeInspectionState::Failed(error.to_string()),
                        certs: BTreeMap::new(),
                        profiles: BTreeMap::new(),
                    },
                );
            }
        }
    }
    Ok(scopes)
}

fn write_bundle_ids(out: &mut String, state: &State) -> Result<()> {
    writeln!(out, "bundle_ids:")?;
    if state.bundle_ids.is_empty() {
        writeln!(out, "  none")?;
    } else {
        for (logical_name, bundle_id) in &state.bundle_ids {
            writeln!(out, "  {logical_name}")?;
            writeln!(out, "    apple_id: {}", bundle_id.apple_id)?;
            writeln!(out, "    bundle_id: {}", bundle_id.bundle_id)?;
        }
    }
    writeln!(out)?;
    Ok(())
}

fn write_devices(out: &mut String, state: &State) -> Result<()> {
    writeln!(out, "devices:")?;
    if state.devices.is_empty() {
        writeln!(out, "  none")?;
    } else {
        for (logical_name, device) in &state.devices {
            writeln!(out, "  {logical_name}")?;
            writeln!(out, "    apple_id: {}", device.apple_id)?;
            writeln!(out, "    udid: {}", device.udid)?;
        }
    }
    writeln!(out)?;
    Ok(())
}

fn write_certificates(
    out: &mut String,
    state: &State,
    scopes: &BTreeMap<Scope, ScopeInspection>,
) -> Result<()> {
    writeln!(out, "certificates:")?;
    if state.certs.is_empty() {
        writeln!(out, "  none")?;
    } else {
        for (logical_name, certificate) in &state.certs {
            writeln!(out, "  {logical_name}")?;
            writeln!(out, "    kind: {}", certificate.kind)?;
            writeln!(
                out,
                "    scope: {}",
                render_optional_scope(managed_certificate_scope(&certificate.kind))
            )?;
            writeln!(
                out,
                "    apple_id: {}",
                certificate.apple_id.as_deref().unwrap_or("(none)")
            )?;
            writeln!(out, "    name: {}", certificate.name)?;
            writeln!(out, "    serial: {}", certificate.serial_number)?;
            write_artifact_status(
                out,
                "p12",
                managed_certificate_scope(&certificate.kind),
                scopes,
                logical_name,
            )?;
        }
    }
    writeln!(out)?;
    Ok(())
}

fn write_profiles(
    out: &mut String,
    state: &State,
    scopes: &BTreeMap<Scope, ScopeInspection>,
) -> Result<()> {
    writeln!(out, "profiles:")?;
    if state.profiles.is_empty() {
        writeln!(out, "  none")?;
    } else {
        for (logical_name, profile) in &state.profiles {
            writeln!(out, "  {logical_name}")?;
            writeln!(out, "    kind: {}", profile.kind)?;
            writeln!(
                out,
                "    scope: {}",
                render_optional_scope(managed_profile_scope(&profile.kind))
            )?;
            writeln!(out, "    apple_id: {}", profile.apple_id)?;
            writeln!(out, "    name: {}", profile.name)?;
            writeln!(out, "    uuid: {}", profile.uuid)?;
            writeln!(out, "    bundle_id_ref: {}", profile.bundle_id)?;
            writeln!(
                out,
                "    bundle_id: {}",
                state
                    .bundle_ids
                    .get(&profile.bundle_id)
                    .map(|bundle_id| bundle_id.bundle_id.as_str())
                    .unwrap_or("(missing)")
            )?;
            writeln!(out, "    certs: {}", render_list(&profile.certs))?;
            writeln!(out, "    devices: {}", render_list(&profile.devices))?;
            write_artifact_status(
                out,
                "profile",
                managed_profile_scope(&profile.kind),
                scopes,
                logical_name,
            )?;
        }
    }
    writeln!(out)?;
    Ok(())
}

fn write_artifact_status(
    out: &mut String,
    label: &str,
    scope: Option<Scope>,
    scopes: &BTreeMap<Scope, ScopeInspection>,
    logical_name: &str,
) -> Result<()> {
    let Some(scope) = scope else {
        writeln!(out, "    {label}: n/a (unsupported scope)")?;
        return Ok(());
    };
    let Some(scope_report) = scopes.get(&scope) else {
        writeln!(out, "    {label}: unknown (scope was not inspected)")?;
        return Ok(());
    };
    match &scope_report.state {
        ScopeInspectionState::Locked => {
            writeln!(out, "    {label}: unknown ({scope} password unavailable)")?;
        }
        ScopeInspectionState::Failed(error) => {
            writeln!(out, "    {label}: unknown ({scope} unlock failed: {error})")?;
        }
        ScopeInspectionState::Unlocked => {
            let artifacts = if label == "p12" {
                &scope_report.certs
            } else {
                &scope_report.profiles
            };
            if let Some(artifact) = artifacts.get(logical_name) {
                writeln!(
                    out,
                    "    {label}: {}",
                    if artifact.bytes_present {
                        "present"
                    } else {
                        "missing"
                    }
                )?;
                if let Some(password_present) = artifact.password_present {
                    writeln!(
                        out,
                        "    p12_password: {}",
                        if password_present {
                            "present"
                        } else {
                            "missing"
                        }
                    )?;
                }
                writeln!(
                    out,
                    "    expired: {}",
                    render_expiration(&artifact.expiration)
                )?;
            } else {
                writeln!(out, "    {label}: missing")?;
            }
        }
    }
    Ok(())
}

fn write_scope_status(out: &mut String, scopes: &BTreeMap<Scope, ScopeInspection>) -> Result<()> {
    writeln!(out, "scopes:")?;
    for (scope, report) in scopes {
        match &report.state {
            ScopeInspectionState::Locked => {
                writeln!(out, "  {scope}: locked (password unavailable)")?;
            }
            ScopeInspectionState::Failed(error) => {
                writeln!(out, "  {scope}: unlock failed: {error}")?;
            }
            ScopeInspectionState::Unlocked => {
                writeln!(
                    out,
                    "  {scope}: unlocked ({} p12 artifact(s), {} profile artifact(s))",
                    report.certs.len(),
                    report.profiles.len()
                )?;
            }
        }
    }
    writeln!(out)?;
    Ok(())
}

fn write_warnings(out: &mut String, warnings: &[String]) -> Result<()> {
    writeln!(out, "warnings:")?;
    if warnings.is_empty() {
        writeln!(out, "  none")?;
    } else {
        for warning in warnings {
            writeln!(out, "  - {warning}")?;
        }
    }
    writeln!(out)?;
    Ok(())
}

fn write_adopt_hint(
    out: &mut String,
    bundle_path: &Path,
    current_config: &Config,
    state: &State,
) -> Result<()> {
    if state.certs.is_empty() {
        return Ok(());
    }

    writeln!(out, "hint:")?;
    if state.team_id == current_config.team_id {
        writeln!(
            out,
            "  Certificates from this bundle can be reused in another project for team {} with:",
            state.team_id
        )?;
        writeln!(
            out,
            "    asc-sync signing adopt --config <target asc.json> --from {}",
            bundle_path.display()
        )?;
        writeln!(
            out,
            "  Then run `asc-sync apply --config <target asc.json>` and `asc-sync signing import --config <target asc.json>` in the target project."
        )?;
        writeln!(
            out,
            "  Orbi can use the same bundle later through `orbi asc signing adopt --from ...`."
        )?;
        writeln!(
            out,
            "  The target project's cert logical names and certificate kinds must match."
        )?;
    } else {
        writeln!(
            out,
            "  `adopt` requires the same team_id; this bundle is {}, current config is {}.",
            state.team_id, current_config.team_id
        )?;
    }
    Ok(())
}

fn collect_bundle_warnings(
    current_config: &Config,
    state: &State,
    scopes: &BTreeMap<Scope, ScopeInspection>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if state.team_id != current_config.team_id {
        warnings.push(format!(
            "bundle team_id {} does not match current config team_id {}",
            state.team_id, current_config.team_id
        ));
    }
    for (scope, report) in scopes {
        if let ScopeInspectionState::Failed(error) = &report.state {
            warnings.push(format!("{scope} scope could not be unlocked: {error}"));
        }
    }
    for (logical_name, certificate) in &state.certs {
        if managed_certificate_scope(&certificate.kind).is_none() {
            warnings.push(format!(
                "certificate {logical_name} has unsupported kind {}",
                certificate.kind
            ));
        }
    }
    for (logical_name, profile) in &state.profiles {
        if managed_profile_scope(&profile.kind).is_none() {
            warnings.push(format!(
                "profile {logical_name} has unsupported kind {}",
                profile.kind
            ));
        }
        if !state.bundle_ids.contains_key(&profile.bundle_id) {
            warnings.push(format!(
                "profile {logical_name} references missing bundle_id {}",
                profile.bundle_id
            ));
        }
        for cert in &profile.certs {
            if !state.certs.contains_key(cert) {
                warnings.push(format!(
                    "profile {logical_name} references missing cert {cert}"
                ));
            }
        }
        for device in &profile.devices {
            if !state.devices.contains_key(device) {
                warnings.push(format!(
                    "profile {logical_name} references missing device {device}"
                ));
            }
        }
    }
    warnings
}

fn render_optional_scope(scope: Option<Scope>) -> String {
    scope
        .map(|scope| scope.to_string())
        .unwrap_or_else(|| "unsupported".to_owned())
}

fn render_list(values: &[String]) -> String {
    if values.is_empty() {
        "(none)".to_owned()
    } else {
        values.join(", ")
    }
}

fn render_expiration(expiration: &ExpirationInspection) -> String {
    match expiration {
        ExpirationInspection::NotChecked => "not checked".to_owned(),
        ExpirationInspection::Valid => "no".to_owned(),
        ExpirationInspection::Expired => "yes".to_owned(),
        ExpirationInspection::Unknown(error) => format!("unknown ({error})"),
    }
}

pub fn scope_array() -> Vec<Scope> {
    Scope::ALL.to_vec()
}

pub fn same_existing_file(left: &Path, right: &Path) -> Result<bool> {
    if !left.exists() || !right.exists() {
        return Ok(false);
    }
    Ok(fs::canonicalize(left)
        .with_context(|| format!("failed to canonicalize {}", left.display()))?
        == fs::canonicalize(right)
            .with_context(|| format!("failed to canonicalize {}", right.display()))?)
}

pub fn managed_certificate_scope(kind: &str) -> Option<Scope> {
    match kind {
        "DEVELOPMENT" => Some(Scope::Developer),
        "DISTRIBUTION"
        | "DEVELOPER_ID_APPLICATION"
        | "DEVELOPER_ID_APPLICATION_G2"
        | "DEVELOPER_ID_INSTALLER" => Some(Scope::Release),
        _ => None,
    }
}

pub fn managed_profile_scope(kind: &str) -> Option<Scope> {
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, process::Command as ProcessCommand};

    use age::secrecy::SecretString;
    use serde_json::json;
    use tempfile::tempdir;

    use super::{adopt_certificates, format_bundle_inspection};

    #[test]
    fn adopt_copies_matching_certs_without_project_state() {
        let temp = tempdir().unwrap();
        let source_bundle = temp.path().join("source.ascbundle");
        let target_workspace = crate::sync::Workspace::new(temp.path().join("target"));
        let source_passwords = test_bundle_passwords("source");
        let target_passwords = test_bundle_passwords("target");
        let (pkcs12, serial_number) = test_pkcs12("asc-sync Adopt Development");

        crate::bundle::initialize_bundle(&source_bundle, "TEAM123456", &source_passwords).unwrap();
        let mut source_runtime = target_workspace.create_runtime().unwrap();
        source_runtime.set_cert("development", pkcs12.clone());
        source_runtime.set_cert_password("development", "secret".to_owned());
        source_runtime.set_profile("old-profile", b"old-profile".to_vec());

        let mut source_state = crate::state::State::new("TEAM123456");
        source_state.bundle_ids.insert(
            "old-app".to_owned(),
            crate::state::ManagedBundleId {
                apple_id: "OLDAPP123".to_owned(),
                bundle_id: "dev.asc-sync.old".to_owned(),
            },
        );
        source_state.devices.insert(
            "old-device".to_owned(),
            crate::state::ManagedDevice {
                apple_id: "OLDDEVICE123".to_owned(),
                udid: "00000000-0000000000000000".to_owned(),
            },
        );
        source_state.certs.insert(
            "development".to_owned(),
            crate::state::ManagedCertificate {
                apple_id: Some("CERT123".to_owned()),
                kind: "DEVELOPMENT".to_owned(),
                name: "Old Development".to_owned(),
                serial_number: serial_number.clone(),
                p12_password: "secret".to_owned(),
            },
        );
        source_state.profiles.insert(
            "old-profile".to_owned(),
            crate::state::ManagedProfile {
                apple_id: "PROFILE123".to_owned(),
                name: "Old Profile".to_owned(),
                kind: "MAC_APP_DEVELOPMENT".to_owned(),
                bundle_id: "old-app".to_owned(),
                certs: vec!["development".to_owned()],
                devices: vec!["old-device".to_owned()],
                uuid: "OLD-PROFILE-UUID".to_owned(),
            },
        );
        for scope in crate::scope::Scope::ALL {
            crate::bundle::write_scope(
                &source_bundle,
                &source_runtime,
                scope,
                &source_state,
                &source_passwords[&scope],
            )
            .unwrap();
        }

        let config = serde_json::from_value(json!({
            "team_id": "TEAM123456",
            "certs": {
                "development": {
                    "type": "development",
                    "name": "New Development"
                }
            }
        }))
        .unwrap();

        let inspection = format_bundle_inspection(
            &target_workspace,
            &source_bundle,
            &config,
            &source_state,
            &source_passwords,
        )
        .unwrap();
        assert!(inspection.contains("Signing bundle:"));
        assert!(inspection.contains("team_id: TEAM123456"));
        assert!(inspection.contains("old-app"));
        assert!(inspection.contains("old-device"));
        assert!(inspection.contains("development"));
        assert!(inspection.contains("p12: present"));
        assert!(inspection.contains("p12_password: present"));
        assert!(inspection.contains("expired: no"));
        assert!(inspection.contains("old-profile"));
        assert!(inspection.contains("profile: present"));
        assert!(inspection.contains("asc-sync signing adopt --config <target asc.json> --from"));
        assert!(inspection.contains("orbi asc signing adopt --from ..."));

        let summary = adopt_certificates(
            &target_workspace,
            &config,
            &source_bundle,
            &source_passwords,
            &target_passwords,
        )
        .unwrap();

        assert_eq!(summary.adopted_certs, 1);
        assert!(summary.skipped_certs.is_empty());

        let mut restored_runtime = target_workspace.create_runtime().unwrap();
        let restored = crate::bundle::restore_scope(
            &mut restored_runtime,
            &target_workspace.bundle_path,
            crate::scope::Scope::Developer,
            &target_passwords[&crate::scope::Scope::Developer],
        )
        .unwrap();
        assert!(restored.bundle_ids.is_empty());
        assert!(restored.devices.is_empty());
        assert!(restored.profiles.is_empty());
        assert_eq!(restored.certs.len(), 1);
        assert_eq!(
            restored.certs["development"].apple_id.as_deref(),
            Some("CERT123")
        );
        assert_eq!(restored.certs["development"].name, "New Development");
        assert_eq!(restored.certs["development"].serial_number, serial_number);
        assert_eq!(restored_runtime.cert_bytes("development").unwrap(), pkcs12);

        let mut release_runtime = target_workspace.create_runtime().unwrap();
        let release_state = crate::bundle::restore_scope(
            &mut release_runtime,
            &target_workspace.bundle_path,
            crate::scope::Scope::Release,
            &target_passwords[&crate::scope::Scope::Release],
        )
        .unwrap();
        assert_eq!(release_state.certs.len(), 1);
        assert!(release_runtime.cert_artifacts().is_empty());
    }

    fn test_bundle_passwords(prefix: &str) -> BTreeMap<crate::scope::Scope, SecretString> {
        BTreeMap::from([
            (
                crate::scope::Scope::Developer,
                SecretString::from(format!("{prefix}-developer-password")),
            ),
            (
                crate::scope::Scope::Release,
                SecretString::from(format!("{prefix}-release-password")),
            ),
        ])
    }

    fn test_pkcs12(common_name: &str) -> (Vec<u8>, String) {
        let temp = tempdir().unwrap();
        let generated = crate::system::generate_csr(common_name).unwrap();
        let cert_path = temp.path().join("certificate.cer");
        let status = ProcessCommand::new("openssl")
            .arg("x509")
            .arg("-req")
            .arg("-days")
            .arg("365")
            .arg("-in")
            .arg(&generated.csr_path)
            .arg("-signkey")
            .arg(&generated.key_path)
            .arg("-out")
            .arg(&cert_path)
            .status()
            .unwrap();
        assert!(status.success());
        let pkcs12 = crate::system::create_pkcs12_bytes_from_certificate_file(
            &generated.key_path,
            &cert_path,
            "secret",
        )
        .unwrap();
        let serial_number = crate::system::read_certificate_serial_number_from_file(&cert_path)
            .unwrap()
            .to_uppercase();
        (pkcs12, serial_number)
    }
}
