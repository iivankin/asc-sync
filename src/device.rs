use std::{
    collections::BTreeMap,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use age::secrecy::{ExposeSecret, SecretString};
use anyhow::{Context, Result, bail, ensure};
use qrcode::{QrCode, render::unicode};
use reqwest::{Url, blocking::Client};

use crate::{
    asc::{AscClient, Device},
    auth_store, bundle, bundle_team,
    cli::{DeviceAddArgs, DeviceAddLocalArgs, DeviceFamilyArg},
    config::{Config, DeviceFamily},
    config_edit, config_io,
    device_discovery::{detect_current_mac, discover_local_devices},
    device_server::{
        CompletedRegistration, CreateRegistrationRequest, CreateRegistrationResponse,
        RegistrationStatus, RegistrationStatusResponse,
    },
    scope::Scope,
    state::ManagedDevice,
    sync::Workspace,
};

const DEVICE_SERVER_URL_ENV: &str = "ASC_DEVICE_SERVER_URL";
const DEFAULT_DEVICE_SERVER_URL: &str = "https://asc.orbitstorage.dev";

pub fn run_add(args: &DeviceAddArgs) -> Result<()> {
    let config = load_validated_config(&args.config)?;
    let server_url = resolve_device_server_url(&config)?;
    let logical_id = args
        .id
        .clone()
        .unwrap_or_else(|| slugify_device_id(&args.name));

    let client = device_server_http_client()?;
    let request = CreateRegistrationRequest {
        logical_id: Some(logical_id.clone()),
        display_name: Some(args.name.clone()),
    };
    let response = create_registration(&client, &server_url, &request)?;

    print_registration_instructions(&response.registration_url);
    let registration = poll_registration(
        &client,
        &server_url,
        &response.token,
        Duration::from_secs(args.timeout_seconds),
    )?;
    let result = registration
        .result
        .ok_or_else(|| anyhow::anyhow!("registration completed without device data"))?;
    let family = args
        .family
        .map(DeviceFamily::from)
        .or_else(|| {
            result
                .product
                .as_deref()
                .and_then(DeviceFamily::infer_from_product)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "failed to infer device family from registration result; rerun with --family"
            )
        })?;

    register_device_from_result(
        &args.config,
        &config,
        &logical_id,
        &args.name,
        family,
        &result,
        args.apply,
    )?;
    print_device_add_outcome(&args.config, &args.name, &result.udid, args.apply);
    Ok(())
}

pub fn run_add_local(args: &DeviceAddLocalArgs) -> Result<()> {
    let config = load_validated_config(&args.config)?;
    let spec = resolve_local_device_spec(args)?;
    let udid = spec.udid.clone();
    register_device_from_result(
        &args.config,
        &config,
        &spec.logical_id,
        &spec.display_name,
        spec.family,
        &CompletedRegistration {
            udid,
            product: None,
            version: None,
        },
        args.apply,
    )?;
    print_device_add_outcome(&args.config, &spec.display_name, &spec.udid, args.apply);
    Ok(())
}

struct LocalDeviceSpec {
    logical_id: String,
    display_name: String,
    family: DeviceFamily,
    udid: String,
}

fn register_device_from_result(
    config_path: &Path,
    config: &Config,
    logical_id: &str,
    display_name: &str,
    family: DeviceFamily,
    registration: &CompletedRegistration,
    apply: bool,
) -> Result<()> {
    let developer_bundle = if apply {
        Some(prepare_developer_bundle_for_apply(
            config_path,
            &config.team_id,
        )?)
    } else {
        None
    };

    config_edit::upsert_device(
        config_path,
        logical_id,
        display_name,
        family,
        &registration.udid,
    )?;
    if apply {
        let device = ensure_device_registered(config, display_name, &registration.udid, family)?;
        let developer_bundle =
            developer_bundle.expect("developer bundle preflight exists when apply=true");
        persist_device_in_bundle(
            &developer_bundle.workspace,
            &developer_bundle.password,
            logical_id,
            &registration.udid,
            &device,
        )
    } else {
        Ok(())
    }
}

fn ensure_device_registered(
    config: &Config,
    display_name: &str,
    udid: &str,
    family: DeviceFamily,
) -> Result<Device> {
    let auth = auth_store::resolve_auth_context(&config.team_id)?;
    let client = AscClient::new(auth)?;
    let current_devices = client.list_devices()?;
    if let Some(existing) = current_devices
        .iter()
        .find(|device| device.attributes.udid == udid)
    {
        ensure!(
            existing.attributes.platform == family.asc_platform().asc_value()
                && family.matches_device_class(existing.attributes.device_class.as_deref()),
            "device {} already exists in ASC with incompatible family/platform ({}, {:?})",
            udid,
            existing.attributes.platform,
            existing.attributes.device_class
        );
        if existing.attributes.name != display_name || existing.attributes.status != "ENABLED" {
            return client.update_device(&existing.id, Some(display_name), Some("ENABLED"));
        }
        return Ok(existing.clone());
    }

    client.create_device(display_name, udid, family)
}

fn resolve_local_device_spec(args: &DeviceAddLocalArgs) -> Result<LocalDeviceSpec> {
    if args.current_mac {
        ensure!(
            args.family.is_none() && args.udid.is_none(),
            "--current-mac cannot be combined with --family or --udid"
        );
        let detected = detect_current_mac()?;
        let display_name = args.name.clone().unwrap_or(detected.name);
        let logical_id = args
            .id
            .clone()
            .unwrap_or_else(|| slugify_device_id(&display_name));
        return Ok(LocalDeviceSpec {
            logical_id,
            display_name,
            family: DeviceFamily::Macos,
            udid: detected.udid,
        });
    }

    if let Some(udid) = &args.udid {
        if let Some(detected) = discover_local_devices()?
            .into_iter()
            .find(|device| device.udid == *udid)
        {
            let display_name = args.name.clone().unwrap_or(detected.name);
            let logical_id = args
                .id
                .clone()
                .unwrap_or_else(|| slugify_device_id(&display_name));
            return Ok(LocalDeviceSpec {
                logical_id,
                display_name,
                family: args
                    .family
                    .map(DeviceFamily::from)
                    .unwrap_or(detected.family),
                udid: udid.clone(),
            });
        }

        let family = args.family.ok_or_else(|| {
            anyhow::anyhow!("--family is required when --udid is not discoverable locally")
        })?;
        let display_name = args.name.clone().ok_or_else(|| {
            anyhow::anyhow!("--name is required when --udid is not discoverable locally")
        })?;
        let logical_id = args
            .id
            .clone()
            .unwrap_or_else(|| slugify_device_id(&display_name));

        return Ok(LocalDeviceSpec {
            logical_id,
            display_name,
            family: family.into(),
            udid: udid.clone(),
        });
    }

    ensure!(
        args.family.is_none(),
        "--family requires --udid unless you use auto-discovery"
    );

    let discovered = discover_local_devices()?;
    let detected = match discovered.as_slice() {
        [only] => only.clone(),
        _ => bail!(
            "multiple local devices detected:\n{}",
            discovered
                .iter()
                .map(|device| format!("  - {} [{}] {}", device.name, device.family, device.udid))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    };

    let display_name = args.name.clone().unwrap_or(detected.name);
    let logical_id = args
        .id
        .clone()
        .unwrap_or_else(|| slugify_device_id(&display_name));

    Ok(LocalDeviceSpec {
        logical_id,
        display_name,
        family: detected.family,
        udid: detected.udid,
    })
}

fn create_registration(
    client: &Client,
    server_url: &Url,
    request: &CreateRegistrationRequest,
) -> Result<CreateRegistrationResponse> {
    let url = server_url
        .join("api/registrations")
        .context("failed to build registration creation URL")?;
    let response = client
        .post(url)
        .json(request)
        .send()
        .context("failed to create device registration")?;

    if response.status().is_success() {
        return response
            .json::<CreateRegistrationResponse>()
            .context("failed to decode registration creation response");
    }

    let status = response.status();
    let body = response
        .text()
        .unwrap_or_else(|_| "<unreadable body>".into());
    bail!("device server returned {status} while creating registration: {body}");
}

fn poll_registration(
    client: &Client,
    server_url: &Url,
    token: &str,
    timeout: Duration,
) -> Result<RegistrationStatusResponse> {
    let deadline = Instant::now() + timeout;
    let url = server_url
        .join(&format!("api/registrations/{token}"))
        .context("failed to build registration polling URL")?;

    loop {
        let response = client
            .get(url.clone())
            .send()
            .context("failed to poll device registration")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<unreadable body>".into());
            bail!("device server returned {status} while polling registration: {body}");
        }

        let registration = response
            .json::<RegistrationStatusResponse>()
            .context("failed to decode registration status response")?;
        if registration.status == RegistrationStatus::Completed {
            return Ok(registration);
        }

        ensure!(
            Instant::now() < deadline,
            "timed out waiting for device registration after {} seconds",
            timeout.as_secs()
        );
        thread::sleep(Duration::from_secs(2));
    }
}

fn print_registration_instructions(registration_url: &str) {
    println!("Open this URL on the iPhone or iPad:");
    println!("{registration_url}");
    println!();

    match QrCode::new(registration_url.as_bytes()) {
        Ok(code) => {
            let rendered = code.render::<unicode::Dense1x2>().quiet_zone(false).build();
            println!("{rendered}");
        }
        Err(error) => {
            eprintln!("failed to render QR code: {error}");
        }
    }
}

fn load_validated_config(path: &Path) -> Result<Config> {
    let config = config_io::load_config(path)?;
    config.validate()?;
    Ok(config)
}

fn resolve_device_server_url(config: &Config) -> Result<Url> {
    let _ = config;
    let candidate =
        std::env::var(DEVICE_SERVER_URL_ENV).unwrap_or_else(|_| DEFAULT_DEVICE_SERVER_URL.into());
    let mut url =
        Url::parse(&candidate).with_context(|| format!("invalid device server URL {candidate}"))?;
    if !url.path().ends_with('/') {
        let new_path = format!("{}/", url.path().trim_end_matches('/'));
        url.set_path(&new_path);
    }
    Ok(url)
}

fn device_server_http_client() -> Result<Client> {
    Client::builder()
        .user_agent("asc-sync/0.1.0")
        .build()
        .context("failed to create device server HTTP client")
}

fn slugify_device_id(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for character in name.chars() {
        let lower = character.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            slug.push(lower);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "device".to_owned()
    } else {
        slug.to_owned()
    }
}

fn print_device_add_outcome(config_path: &Path, display_name: &str, udid: &str, apply: bool) {
    if apply {
        println!(
            "Registered device {} ({}) in ASC, wrote it into {}, and updated developer state in signing.ascbundle.",
            display_name,
            udid,
            config_path.display()
        );
        println!(
            "Run `cargo run -- apply --config {}` to refresh development/ad-hoc profiles.",
            config_path.display()
        );
    } else {
        println!(
            "Wrote device {} ({}) into {}.",
            display_name,
            udid,
            config_path.display()
        );
        println!(
            "Run `cargo run -- apply --config {}` when you want ASC registration and updated profiles.",
            config_path.display()
        );
    }
}

impl From<DeviceFamilyArg> for DeviceFamily {
    fn from(value: DeviceFamilyArg) -> Self {
        match value {
            DeviceFamilyArg::Ios => Self::Ios,
            DeviceFamilyArg::Ipados => Self::Ipados,
            DeviceFamilyArg::Watchos => Self::Watchos,
            DeviceFamilyArg::Tvos => Self::Tvos,
            DeviceFamilyArg::Visionos => Self::Visionos,
            DeviceFamilyArg::Macos => Self::Macos,
        }
    }
}

struct PreparedDeveloperBundle {
    workspace: Workspace,
    password: SecretString,
}

fn prepare_developer_bundle_for_apply(
    config_path: &Path,
    team_id: &str,
) -> Result<PreparedDeveloperBundle> {
    let workspace = Workspace::from_config_path(config_path);
    if workspace.bundle_path.exists() {
        let prepared_bundle = bundle_team::prepare_bundle_for_team(
            &workspace,
            team_id,
            bundle_team::BundleAccess::Mutating,
        )?;
        print_bundle_reset_notice(&workspace, team_id, &prepared_bundle.reset_from_team_ids);
        let password =
            prepared_bundle
                .passwords
                .get(&Scope::Developer)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "developer bundle password is required for `device add --apply`; release access alone cannot update device ownership state"
                    )
                })?;
        return Ok(PreparedDeveloperBundle {
            workspace,
            password,
        });
    }

    let passwords = bundle::bootstrap_bundle(&workspace.bundle_path, team_id)?;
    print_bootstrap_passwords(&workspace, &passwords);
    let password = passwords
        .get(&Scope::Developer)
        .cloned()
        .expect("bootstrap passwords contain developer scope");
    Ok(PreparedDeveloperBundle {
        workspace,
        password,
    })
}

fn persist_device_in_bundle(
    workspace: &Workspace,
    developer_password: &SecretString,
    logical_id: &str,
    udid: &str,
    device: &Device,
) -> Result<()> {
    let mut runtime = workspace.create_runtime()?;
    let mut state = bundle::restore_scope(
        &mut runtime,
        &workspace.bundle_path,
        Scope::Developer,
        developer_password,
    )?;
    state.devices.insert(
        logical_id.to_owned(),
        ManagedDevice {
            apple_id: device.id.clone(),
            udid: udid.to_owned(),
        },
    );
    bundle::write_scope(
        &workspace.bundle_path,
        &runtime,
        Scope::Developer,
        &state,
        developer_password,
    )
}

fn print_bootstrap_passwords(workspace: &Workspace, passwords: &BTreeMap<Scope, SecretString>) {
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
