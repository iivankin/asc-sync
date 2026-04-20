use std::path::Path;

use age::secrecy::ExposeSecret;
use anyhow::{Result, ensure};
use clap::Parser;

use crate::{
    app_store,
    asc::AscClient,
    auth_store, build_settings, bundle, bundle_team,
    cli::{
        AuthCommand, Cli, Command, DeviceCommand, MediaCommand, MetadataCommand,
        MetadataKeywordsCommand, SigningCommand,
    },
    config::Config,
    config_io, device, init_cmd, media_render, media_validate, metadata, notarize, revoke,
    scope::Scope,
    state::State,
    submit,
    sync::{ChangeKind, Mode, SyncEngine, Workspace},
    system,
};

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Auth(AuthCommand::Import) => auth_store::import_auth_interactively(),
        Command::Device(DeviceCommand::Add(args)) => device::run_add(&args),
        Command::Device(DeviceCommand::AddLocal(args)) => device::run_add_local(&args),
        Command::Init(args) => init_cmd::run(&args),
        Command::Notarize(args) => notarize::run(&args),
        Command::Validate(args) => {
            let config = config_io::load_config(&args.config)?;
            config.validate()?;
            validate_media(&args.config, &config)?;
            validate_signing_bundle(&args.config, &config)?;
            println!("config is valid");
            Ok(())
        }
        Command::Media(MediaCommand::Validate(args)) => {
            let config = config_io::load_config(&args.config)?;
            config.validate()?;
            validate_media(&args.config, &config)?;
            Ok(())
        }
        Command::Metadata(MetadataCommand::Keywords(MetadataKeywordsCommand::Audit(args))) => {
            metadata::run_keywords_audit(&args)
        }
        Command::Media(MediaCommand::Render(args)) => media_render::render(&args),
        Command::Media(MediaCommand::Preview(args)) => media_render::preview(&args),
        Command::Submit(args) => submit::run(&args),
        Command::SubmitForReview(args) => app_store::submit_for_review(&args),
        Command::Signing(SigningCommand::Import(args)) => run_signing_import(&args.config),
        Command::Signing(SigningCommand::PrintBuildSettings(args)) => {
            run_signing_print_build_settings(&args.config)
        }
        Command::Signing(SigningCommand::Merge(args)) => {
            let workspace = Workspace::from_config_path(&args.config);
            bundle::merge_signing_bundle(
                &workspace.bundle_path,
                &args.base,
                &args.ours,
                &args.theirs,
            )?;
            println!(
                "Merged signing bundle into {}",
                workspace.bundle_path.display()
            );
            Ok(())
        }
        Command::Plan(args) => run_sync(Mode::Plan, &args.config),
        Command::Apply(args) => run_sync(Mode::Apply, &args.config),
        Command::Revoke(args) => {
            let config = config_io::load_config(&args.config)?;
            config.validate()?;
            revoke::run(&args, &config)
        }
    }
}

fn run_signing_import(config_path: &Path) -> Result<()> {
    let config = config_io::load_config(config_path)?;
    config.validate()?;
    let workspace = Workspace::from_config_path(config_path);
    let prepared_bundle = bundle_team::prepare_bundle_for_team(
        &workspace,
        &config.team_id,
        bundle_team::BundleAccess::ReadOnly,
    )?;
    print_bundle_reset_notice(
        &workspace,
        &config.team_id,
        &prepared_bundle.reset_from_team_ids,
    );

    for scope in Scope::ALL {
        let Some(password) = prepared_bundle.passwords.get(&scope) else {
            println!("[{scope}] skipped: password unavailable");
            continue;
        };

        let mut runtime = workspace.create_runtime()?;
        let state = bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, password)?;
        let mut imported = 0usize;
        for (logical_name, certificate) in &state.certs {
            if managed_certificate_scope(&certificate.kind) != Some(scope) {
                continue;
            }
            let pkcs12 = runtime.cert_bytes(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing PKCS#12 artifact for cert {logical_name}")
            })?;
            let p12_password = runtime.cert_password(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing PKCS#12 password for cert {logical_name}")
            })?;
            system::import_pkcs12_bytes_into_login_keychain(logical_name, pkcs12, p12_password)?;
            imported += 1;
        }
        let installed_profiles = install_profiles(scope, &runtime, &state)?;
        println!(
            "[{scope}] imported {imported} certificate(s), installed {installed_profiles} profile(s)"
        );
    }

    Ok(())
}

fn run_signing_print_build_settings(config_path: &Path) -> Result<()> {
    let config = config_io::load_config(config_path)?;
    config.validate()?;
    let workspace = Workspace::from_config_path(config_path);
    let prepared_bundle = bundle_team::prepare_bundle_for_team(
        &workspace,
        &config.team_id,
        bundle_team::BundleAccess::ReadOnly,
    )?;
    print_bundle_reset_notice(
        &workspace,
        &config.team_id,
        &prepared_bundle.reset_from_team_ids,
    );

    let mut printed_any = false;
    for scope in Scope::ALL {
        let Some(password) = prepared_bundle.passwords.get(&scope) else {
            println!("[{scope}] skipped: password unavailable");
            continue;
        };

        let mut runtime = workspace.create_runtime()?;
        let state = bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, password)?;
        let report = build_settings::collect_scope_build_settings(scope, &state);
        if report.profiles.is_empty() {
            println!("[{scope}] no managed provisioning profiles");
            continue;
        }

        printed_any = true;
        println!("[{scope}]");
        for profile in report.profiles {
            println!("profile: {}", profile.logical_name);
            println!("kind: {}", profile.kind);
            println!("bundleIdRef: {}", profile.bundle_id_ref);
            println!("bundleId: {}", profile.bundle_id);
            println!("uuid: {}", profile.uuid);
            if !profile.certs.is_empty() {
                println!("certs: {}", profile.certs.join(", "));
            }
            println!("CODE_SIGN_STYLE=Manual");
            println!("DEVELOPMENT_TEAM={}", profile.team_id);
            println!("PROVISIONING_PROFILE_SPECIFIER={}", profile.logical_name);
            println!("PROVISIONING_PROFILE={}", profile.uuid);
            if let Some(identity) = profile.code_sign_identity {
                println!("CODE_SIGN_IDENTITY={identity}");
            }
            println!();
        }
    }

    ensure!(printed_any, "no managed provisioning profiles found");
    Ok(())
}

fn run_sync(mode: Mode, config_path: &Path) -> Result<()> {
    let config = config_io::load_config(config_path)?;
    config.validate()?;
    if mode == Mode::Apply {
        validate_media(config_path, &config)?;
    }

    let team_id = config.team_id.as_str();
    let auth = auth_store::resolve_auth_context(team_id)?;
    let client = AscClient::new(auth)?;
    let workspace = Workspace::from_config_path(config_path);

    if workspace.bundle_path.exists() {
        let prepared_bundle = bundle_team::prepare_bundle_for_team(
            &workspace,
            team_id,
            if mode == Mode::Apply {
                bundle_team::BundleAccess::Mutating
            } else {
                bundle_team::BundleAccess::ReadOnly
            },
        )?;
        print_bundle_reset_notice(&workspace, team_id, &prepared_bundle.reset_from_team_ids);

        let active_scopes = Scope::ALL
            .into_iter()
            .filter(|scope| prepared_bundle.passwords.contains_key(scope))
            .collect::<Vec<_>>();

        for scope in Scope::ALL {
            let Some(password) = prepared_bundle.passwords.get(&scope) else {
                println!("[{scope}] skipped: password unavailable");
                continue;
            };
            run_sync_scope(
                mode,
                scope,
                &client,
                &config,
                &workspace,
                team_id,
                password,
                active_scopes.len() > 1,
            )?;
        }
        app_store::run_sync(config_path, &config, &client, mode)?;
        return Ok(());
    }

    let present_scopes = ordered_scopes(&config);
    if mode == Mode::Plan {
        for scope in &present_scopes {
            run_sync_scope_without_bundle(
                mode,
                *scope,
                &client,
                &config,
                &workspace,
                team_id,
                present_scopes.len() > 1,
            )?;
        }
        app_store::run_sync(config_path, &config, &client, mode)?;
        return Ok(());
    }

    let passwords = bundle::bootstrap_bundle(&workspace.bundle_path, team_id)?;
    print_bootstrap_passwords(&workspace, &passwords);

    if present_scopes.is_empty() {
        println!(
            "Initialized signing bundle at {}",
            workspace.bundle_path.display()
        );
        return Ok(());
    }

    for scope in &present_scopes {
        let password = passwords
            .get(scope)
            .expect("bootstrap bundle generated passwords for all scopes");
        run_sync_scope(
            mode,
            *scope,
            &client,
            &config,
            &workspace,
            team_id,
            password,
            present_scopes.len() > 1,
        )?;
    }

    app_store::run_sync(config_path, &config, &client, mode)?;

    Ok(())
}

fn validate_media(config_path: &Path, config: &Config) -> Result<()> {
    let summary = media_validate::validate_config(config_path, config)?;
    if summary.screenshot_sets > 0
        || summary.preview_sets > 0
        || summary.extra_images > 0
        || summary.extra_videos > 0
    {
        println!(
            "media is valid: {} screenshot set(s), {} screenshot file(s), {} preview set(s), {} preview file(s), {} extra image(s), {} extra video(s)",
            summary.screenshot_sets,
            summary.screenshots,
            summary.preview_sets,
            summary.previews,
            summary.extra_images,
            summary.extra_videos
        );
    }
    Ok(())
}

fn run_sync_scope_without_bundle(
    mode: Mode,
    scope: Scope,
    client: &AscClient,
    config: &Config,
    workspace: &Workspace,
    team_id: &str,
    print_scope_header: bool,
) -> Result<()> {
    if print_scope_header {
        println!("[{scope}]");
    }

    let mut runtime = workspace.create_runtime()?;
    let mut state = State::new(team_id);
    let engine = SyncEngine::new(mode, scope, client, config, &mut runtime, &mut state);
    let summary = engine.run()?;
    print_summary(&summary.changes);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_sync_scope(
    mode: Mode,
    scope: Scope,
    client: &AscClient,
    config: &Config,
    workspace: &Workspace,
    team_id: &str,
    bundle_password: &age::secrecy::SecretString,
    print_scope_header: bool,
) -> Result<()> {
    if print_scope_header {
        println!("[{scope}]");
    }

    let mut runtime = workspace.create_runtime()?;
    let mut state =
        bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, bundle_password)?;
    state.ensure_team(team_id)?;

    let engine = SyncEngine::new(mode, scope, client, config, &mut runtime, &mut state);
    let summary = engine.run()?;
    print_summary(&summary.changes);

    if mode == Mode::Apply {
        bundle::write_scope(
            &workspace.bundle_path,
            &runtime,
            scope,
            &state,
            bundle_password,
        )?;
        let installed_profiles = install_profiles(scope, &runtime, &state)?;
        println!(
            "{} signing bundle saved to {}",
            scope,
            workspace.bundle_path.display()
        );
        println!("[{scope}] installed {installed_profiles} profile(s)");
    }

    Ok(())
}

fn install_profiles(
    scope: Scope,
    runtime: &crate::sync::RuntimeWorkspace,
    state: &State,
) -> Result<usize> {
    let mut installed = 0usize;
    for (logical_name, profile) in &state.profiles {
        if managed_profile_scope(&profile.kind) != Some(scope) {
            continue;
        }
        let profile_bytes = runtime.profile_bytes(logical_name).ok_or_else(|| {
            anyhow::anyhow!("missing provisioning profile artifact for profile {logical_name}")
        })?;
        system::install_profile_bytes(&profile.uuid, profile_bytes)?;
        installed += 1;
    }
    Ok(installed)
}

fn print_summary(changes: &[crate::sync::Change]) {
    if changes.is_empty() {
        println!("No changes.");
        return;
    }

    for change in changes {
        println!(
            "{:<7} {:<40} {}",
            render_change_kind(&change.kind),
            change.subject,
            change.detail
        );
    }
}

fn ordered_scopes(config: &Config) -> Vec<Scope> {
    let present = config.present_scopes();
    Scope::ALL
        .into_iter()
        .filter(|scope| present.contains(scope))
        .collect()
}

fn print_bootstrap_passwords(
    workspace: &Workspace,
    passwords: &std::collections::BTreeMap<Scope, age::secrecy::SecretString>,
) {
    println!(
        "Generated bundle passwords for {}:",
        workspace.bundle_path.display()
    );
    for scope in Scope::ALL {
        let password = passwords
            .get(&scope)
            .expect("bootstrap passwords contain both scopes");
        println!("{scope}: {}", password.expose_secret());
    }
    println!("Passwords were saved to ~/.asc-sync.");
}

fn print_bundle_reset_notice(workspace: &Workspace, team_id: &str, previous_team_ids: &[String]) {
    if previous_team_ids.is_empty() {
        return;
    }

    println!(
        "Reset {} from team(s) {} to {}.",
        workspace.bundle_path.display(),
        previous_team_ids.join(", "),
        team_id
    );
}

fn validate_signing_bundle(config_path: &Path, config: &Config) -> Result<()> {
    let workspace = Workspace::from_config_path(config_path);
    if !workspace.bundle_path.exists() {
        return Ok(());
    }

    let shared_state = bundle::load_state(&workspace.bundle_path)?;
    shared_state.ensure_team(&config.team_id)?;
    let required_scopes = signing_scopes_in_state(&shared_state);
    if required_scopes.is_empty() {
        return Ok(());
    }

    let unlocked = bundle::resolve_existing_passwords(&workspace.bundle_path, &required_scopes)?;
    for scope in &required_scopes {
        ensure!(
            unlocked.contains_key(scope),
            "missing {scope} bundle password; cannot validate {scope} signing artifacts"
        );
    }

    for scope in required_scopes {
        let password = unlocked
            .get(&scope)
            .expect("required scope password is present");
        let mut runtime = workspace.create_runtime()?;
        let state = bundle::restore_scope(&mut runtime, &workspace.bundle_path, scope, password)?;

        for (logical_name, certificate) in &state.certs {
            if managed_certificate_scope(&certificate.kind) != Some(scope) {
                continue;
            }
            let pkcs12 = runtime.cert_bytes(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing PKCS#12 artifact for cert {logical_name}")
            })?;
            let p12_password = runtime.cert_password(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing PKCS#12 password for cert {logical_name}")
            })?;
            ensure!(
                !system::pkcs12_is_expired(pkcs12, p12_password)?,
                "certificate {logical_name} is expired"
            );
        }

        for (logical_name, profile) in &state.profiles {
            if managed_profile_scope(&profile.kind) != Some(scope) {
                continue;
            }
            let profile_bytes = runtime.profile_bytes(logical_name).ok_or_else(|| {
                anyhow::anyhow!("missing provisioning profile artifact for profile {logical_name}")
            })?;
            ensure!(
                !system::provisioning_profile_is_expired(profile_bytes)?,
                "provisioning profile {logical_name} ({}) is expired",
                profile.uuid
            );
        }
    }

    if let Some(auth) = auth_store::resolve_auth_context_if_available(&config.team_id)? {
        let client = AscClient::new(auth)?;
        validate_live_signing_state(&client, &shared_state)?;
    }

    Ok(())
}

fn validate_live_signing_state(client: &AscClient, state: &State) -> Result<()> {
    let bundle_ids = client
        .list_bundle_ids()?
        .into_iter()
        .map(|bundle_id| (bundle_id.id.clone(), bundle_id))
        .collect::<std::collections::BTreeMap<_, _>>();
    let devices = client
        .list_devices()?
        .into_iter()
        .map(|device| (device.id.clone(), device))
        .collect::<std::collections::BTreeMap<_, _>>();
    let certificates = client
        .list_certificates()?
        .into_iter()
        .map(|certificate| (certificate.id.clone(), certificate))
        .collect::<std::collections::BTreeMap<_, _>>();
    let profiles = client
        .list_profiles()?
        .into_iter()
        .map(|profile| (profile.id.clone(), profile))
        .collect::<std::collections::BTreeMap<_, _>>();

    for (logical_name, bundle_id) in &state.bundle_ids {
        let live = bundle_ids.get(&bundle_id.apple_id).ok_or_else(|| {
            anyhow::anyhow!("bundle_id {logical_name} is missing in App Store Connect")
        })?;
        ensure!(
            live.attributes.identifier == bundle_id.bundle_id,
            "bundle_id {logical_name} points to {}, but App Store Connect now reports {}",
            bundle_id.bundle_id,
            live.attributes.identifier
        );
    }

    for (logical_name, device) in &state.devices {
        let live = devices.get(&device.apple_id).ok_or_else(|| {
            anyhow::anyhow!("device {logical_name} is missing in App Store Connect")
        })?;
        ensure!(
            live.attributes.udid == device.udid,
            "device {logical_name} points to {}, but App Store Connect now reports {}",
            device.udid,
            live.attributes.udid
        );
        ensure!(
            live.attributes.status == "ENABLED",
            "device {logical_name} is {} in App Store Connect",
            live.attributes.status
        );
    }

    for (logical_name, certificate) in &state.certs {
        let live = certificate
            .apple_id
            .as_deref()
            .and_then(|apple_id| certificates.get(apple_id))
            .or_else(|| {
                certificates.values().find(|live| {
                    live.attributes
                        .serial_number
                        .eq_ignore_ascii_case(&certificate.serial_number)
                })
            });
        if certificate.apple_id.is_some() {
            let live = live.ok_or_else(|| {
                anyhow::anyhow!("certificate {logical_name} is missing in App Store Connect")
            })?;
            ensure!(
                live.is_active()?,
                "certificate {logical_name} is inactive or expired in App Store Connect"
            );
        } else if let Some(live) = live {
            // Manual Developer ID certificates may not expose an ASC certificate ID.
            ensure!(
                live.is_active()?,
                "certificate {logical_name} is inactive or expired in App Store Connect"
            );
        }
    }

    for (logical_name, profile) in &state.profiles {
        let live = profiles.get(&profile.apple_id).ok_or_else(|| {
            anyhow::anyhow!("provisioning profile {logical_name} is missing in App Store Connect")
        })?;
        ensure!(
            live.attributes.name == profile.name,
            "provisioning profile {logical_name} is named {}, but App Store Connect now reports {}",
            profile.name,
            live.attributes.name
        );
        ensure!(
            live.is_active()?,
            "provisioning profile {logical_name} ({}) is invalid or expired in App Store Connect",
            profile.uuid
        );
    }

    Ok(())
}

fn signing_scopes_in_state(state: &State) -> Vec<Scope> {
    Scope::ALL
        .into_iter()
        .filter(|scope| {
            state
                .certs
                .values()
                .any(|certificate| managed_certificate_scope(&certificate.kind) == Some(*scope))
                || state
                    .profiles
                    .values()
                    .any(|profile| managed_profile_scope(&profile.kind) == Some(*scope))
        })
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

fn render_change_kind(kind: &ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Create => "create",
        ChangeKind::Update => "update",
        ChangeKind::Replace => "replace",
        ChangeKind::Delete => "delete",
    }
}
