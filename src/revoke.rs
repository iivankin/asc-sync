use anyhow::{Result, ensure};

use crate::{
    asc::{AscClient, Certificate, Profile},
    auth_store, bundle, bundle_team,
    cli::{RevokeArgs, RevokeTarget},
    config::Config,
    scope::Scope,
    sync::Workspace,
};

pub fn run(args: &RevokeArgs, config: &Config) -> Result<()> {
    let auth = auth_store::resolve_auth_context(&config.team_id)?;
    let client = AscClient::new(auth)?;
    let workspace = Workspace::from_config_path(&args.config);
    ensure!(
        workspace.bundle_path.exists(),
        "signing bundle {} does not exist",
        workspace.bundle_path.display()
    );

    let scopes = target_scopes(args.target);
    let prepared_bundle = bundle_team::prepare_bundle_for_team(
        &workspace,
        &config.team_id,
        bundle_team::BundleAccess::Mutating,
    )?;
    if !prepared_bundle.reset_from_team_ids.is_empty() {
        println!(
            "Reset {} from team(s) {} to {}.",
            workspace.bundle_path.display(),
            prepared_bundle.reset_from_team_ids.join(", "),
            config.team_id
        );
    }
    for &scope in &scopes {
        ensure!(
            prepared_bundle.passwords.contains_key(&scope),
            "missing {scope} bundle password; cannot revoke {scope} scope"
        );
    }

    let current_profiles = client.list_profiles()?;
    let current_certificates = client.list_certificates()?;

    for scope in scopes {
        let password = prepared_bundle
            .passwords
            .get(&scope)
            .expect("required scope password is present");
        let mut runtime = workspace.create_runtime()?;
        let mut state =
            bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, password)?;
        state.ensure_team(&config.team_id)?;

        let changes = revoke_scope(
            &client,
            scope,
            &mut state,
            &current_profiles,
            &current_certificates,
        )?;
        if changes.is_empty() {
            println!("[{scope}] No managed certificates or profiles to revoke.");
        } else {
            println!("[{scope}]");
            for change in &changes {
                println!("{:<7} {:<40} {}", "delete", change.subject, change.detail);
            }
        }

        bundle::write_scope(&workspace.bundle_path, &runtime, scope, &state, password)?;
    }

    Ok(())
}

#[derive(Debug)]
struct RevokeChange {
    subject: String,
    detail: String,
}

fn revoke_scope(
    client: &AscClient,
    scope: Scope,
    state: &mut crate::state::State,
    current_profiles: &[Profile],
    current_certificates: &[Certificate],
) -> Result<Vec<RevokeChange>> {
    let mut changes = Vec::new();

    let profiles = scoped_profiles(state, scope);
    for (name, profile) in profiles {
        if current_profiles
            .iter()
            .any(|current| current.id == profile.apple_id)
        {
            client.delete_profile(&profile.apple_id)?;
        }
        state.profiles.remove(&name);
        changes.push(RevokeChange {
            subject: format!("profile.{name}"),
            detail: "delete managed profile".into(),
        });
    }

    let certificates = scoped_certificates(state, scope);
    for (name, certificate) in certificates {
        if let Some(apple_id) = certificate.apple_id.as_deref()
            && current_certificates
                .iter()
                .any(|current| current.id == apple_id)
        {
            client.revoke_certificate(apple_id)?;
        }
        state.certs.remove(&name);
        changes.push(RevokeChange {
            subject: format!("cert.{name}"),
            detail: "revoke managed certificate".into(),
        });
    }

    Ok(changes)
}

fn scoped_profiles(
    state: &crate::state::State,
    scope: Scope,
) -> Vec<(String, crate::state::ManagedProfile)> {
    state
        .profiles
        .iter()
        .filter(|(_, profile)| managed_profile_scope(&profile.kind) == Some(scope))
        .map(|(name, profile)| (name.clone(), profile.clone()))
        .collect()
}

fn scoped_certificates(
    state: &crate::state::State,
    scope: Scope,
) -> Vec<(String, crate::state::ManagedCertificate)> {
    state
        .certs
        .iter()
        .filter(|(_, certificate)| managed_certificate_scope(&certificate.kind) == Some(scope))
        .map(|(name, certificate)| (name.clone(), certificate.clone()))
        .collect()
}

fn managed_certificate_scope(kind: &str) -> Option<Scope> {
    match kind {
        "DEVELOPMENT" => Some(Scope::Developer),
        "DISTRIBUTION" | "DEVELOPER_ID_APPLICATION_G2" | "DEVELOPER_ID_INSTALLER" => {
            Some(Scope::Release)
        }
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

fn target_scopes(target: RevokeTarget) -> Vec<Scope> {
    match target {
        RevokeTarget::Dev => vec![Scope::Developer],
        RevokeTarget::Release => vec![Scope::Release],
        RevokeTarget::All => Scope::ALL.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{scoped_certificates, scoped_profiles};
    use crate::{
        scope::Scope,
        state::{ManagedCertificate, ManagedProfile, State},
    };

    #[test]
    fn scope_helpers_only_return_entries_for_requested_scope() {
        let mut state = State::new("TEAM123");
        state.certs.insert(
            "dev".into(),
            ManagedCertificate {
                apple_id: Some("dev-cert".into()),
                kind: "DEVELOPMENT".into(),
                name: "Dev".into(),
                serial_number: "serial-dev".into(),
                p12_password: "secret-dev".into(),
            },
        );
        state.certs.insert(
            "dist".into(),
            ManagedCertificate {
                apple_id: Some("dist-cert".into()),
                kind: "DISTRIBUTION".into(),
                name: "Dist".into(),
                serial_number: "serial-dist".into(),
                p12_password: "secret-dist".into(),
            },
        );
        state.certs.insert(
            "installer".into(),
            ManagedCertificate {
                apple_id: None,
                kind: "DEVELOPER_ID_INSTALLER".into(),
                name: "Installer".into(),
                serial_number: "serial-installer".into(),
                p12_password: "secret-installer".into(),
            },
        );
        state.profiles.insert(
            "ios-dev".into(),
            ManagedProfile {
                apple_id: "ios-dev".into(),
                name: "iOS Dev".into(),
                kind: "IOS_APP_DEVELOPMENT".into(),
                bundle_id: "app".into(),
                certs: vec!["dev".into()],
                devices: vec!["phone".into()],
                uuid: "uuid-dev".into(),
            },
        );
        state.profiles.insert(
            "ios-store".into(),
            ManagedProfile {
                apple_id: "ios-store".into(),
                name: "iOS Store".into(),
                kind: "IOS_APP_STORE".into(),
                bundle_id: "app".into(),
                certs: vec!["dist".into()],
                devices: Vec::new(),
                uuid: "uuid-store".into(),
            },
        );

        let developer_certs = scoped_certificates(&state, Scope::Developer);
        let release_certs = scoped_certificates(&state, Scope::Release);
        let developer_profiles = scoped_profiles(&state, Scope::Developer);
        let release_profiles = scoped_profiles(&state, Scope::Release);

        assert_eq!(developer_certs.len(), 1);
        assert_eq!(developer_certs[0].0, "dev");
        assert_eq!(release_certs.len(), 2);
        assert_eq!(release_certs[0].0, "dist");
        assert_eq!(release_certs[1].0, "installer");
        assert_eq!(developer_profiles.len(), 1);
        assert_eq!(developer_profiles[0].0, "ios-dev");
        assert_eq!(release_profiles.len(), 1);
        assert_eq!(release_profiles[0].0, "ios-store");
    }
}
