use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use anyhow::{Result, ensure};
use rand::{RngExt, distr::Alphanumeric};

use crate::{
    asc::{AscClient, BundleCapability, BundleId, Certificate, Device, Profile, ProfileResolved},
    config::{Config, DesiredCapability},
    scope::Scope,
    state::{ManagedBundleId, ManagedCertificate, ManagedDevice, ManagedProfile, State},
    system,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Plan,
    Apply,
}

#[derive(Debug)]
pub struct SyncSummary {
    pub mode: Mode,
    pub changes: Vec<Change>,
}

#[derive(Debug)]
pub struct Change {
    pub kind: ChangeKind,
    pub subject: String,
    pub detail: String,
}

#[derive(Debug)]
pub enum ChangeKind {
    Create,
    Update,
    Replace,
    Delete,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub bundle_path: PathBuf,
}

#[derive(Debug)]
pub struct RuntimeWorkspace {
    certs: BTreeMap<String, Vec<u8>>,
    cert_passwords: BTreeMap<String, String>,
    profiles: BTreeMap<String, Vec<u8>>,
}

pub struct SyncEngine<'a> {
    mode: Mode,
    scope: Scope,
    client: &'a AscClient,
    config: &'a Config,
    workspace: &'a mut RuntimeWorkspace,
    state: &'a mut State,
    changes: Vec<Change>,
}

struct ProfileMatchSpec<'a> {
    profile_name: &'a str,
    profile_kind: &'a str,
    desired_bundle_id_id: &'a str,
    desired_certificate_ids: &'a BTreeSet<String>,
    desired_device_ids: &'a BTreeSet<String>,
}

impl Workspace {
    pub fn from_config_path(config_path: &Path) -> Self {
        let root = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let bundle_path = root.join(crate::bundle::BUNDLE_FILE_NAME);
        Self { root, bundle_path }
    }

    pub fn create_runtime(&self) -> Result<RuntimeWorkspace> {
        let _ = &self.root;
        Ok(RuntimeWorkspace {
            certs: BTreeMap::new(),
            cert_passwords: BTreeMap::new(),
            profiles: BTreeMap::new(),
        })
    }
}

impl RuntimeWorkspace {
    pub fn replace_artifacts(
        &mut self,
        certs: BTreeMap<String, Vec<u8>>,
        cert_passwords: BTreeMap<String, String>,
        profiles: BTreeMap<String, Vec<u8>>,
    ) {
        self.certs = certs;
        self.cert_passwords = cert_passwords;
        self.profiles = profiles;
    }

    pub fn cert_bytes(&self, logical_name: &str) -> Option<&[u8]> {
        self.certs.get(logical_name).map(Vec::as_slice)
    }

    pub fn set_cert(&mut self, logical_name: impl Into<String>, bytes: Vec<u8>) {
        self.certs.insert(logical_name.into(), bytes);
    }

    pub fn cert_password(&self, logical_name: &str) -> Option<&str> {
        self.cert_passwords.get(logical_name).map(String::as_str)
    }

    pub fn set_cert_password(&mut self, logical_name: impl Into<String>, password: String) {
        self.cert_passwords.insert(logical_name.into(), password);
    }

    pub fn remove_cert(&mut self, logical_name: &str) {
        self.certs.remove(logical_name);
        self.cert_passwords.remove(logical_name);
    }

    pub fn cert_artifacts(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.certs
    }

    pub fn profile_bytes(&self, logical_name: &str) -> Option<&[u8]> {
        self.profiles.get(logical_name).map(Vec::as_slice)
    }

    pub fn set_profile(&mut self, logical_name: impl Into<String>, bytes: Vec<u8>) {
        self.profiles.insert(logical_name.into(), bytes);
    }

    pub fn remove_profile(&mut self, logical_name: &str) {
        self.profiles.remove(logical_name);
    }

    pub fn profile_artifacts(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.profiles
    }
}

impl<'a> SyncEngine<'a> {
    pub fn new(
        mode: Mode,
        scope: Scope,
        client: &'a AscClient,
        config: &'a Config,
        workspace: &'a mut RuntimeWorkspace,
        state: &'a mut State,
    ) -> Self {
        Self {
            mode,
            scope,
            client,
            config,
            workspace,
            state,
            changes: Vec::new(),
        }
    }

    pub fn run(mut self) -> Result<SyncSummary> {
        let bundle_ids = self.client.list_bundle_ids()?;
        let devices = self.client.list_devices()?;
        let certificates = self.client.list_certificates()?;
        let profiles = self.client.list_profiles()?;

        let bundle_id_ids = self.resolve_bundle_id_ids(&bundle_ids)?;
        let device_ids = self.resolve_device_ids(&devices)?;
        let certificate_ids = self.reconcile_certificates(&certificates)?;
        self.reconcile_profiles(&profiles, &bundle_id_ids, &device_ids, &certificate_ids)?;
        self.prune_removed_profiles(&profiles)?;
        self.prune_removed_certificates(&certificates)?;
        if self.scope.owns_bundle_ids() {
            self.prune_removed_bundle_ids(&bundle_ids)?;
        }
        if self.scope.owns_devices() {
            self.prune_removed_devices(&devices)?;
        }

        Ok(SyncSummary {
            mode: self.mode,
            changes: self.changes,
        })
    }

    fn resolve_bundle_id_ids(
        &mut self,
        current_bundle_ids: &[BundleId],
    ) -> Result<BTreeMap<String, String>> {
        if self.scope.owns_bundle_ids() {
            return self.reconcile_bundle_ids(current_bundle_ids);
        }

        let mut bundle_id_ids = BTreeMap::new();
        for (logical_name, spec) in self.release_bundle_id_specs()? {
            if let Some(bundle) = current_bundle_ids
                .iter()
                .find(|bundle| bundle.attributes.identifier == spec.bundle_id)
            {
                ensure!(
                    bundle.attributes.platform == spec.platform.asc_value()
                        || bundle.attributes.platform == "UNIVERSAL",
                    "bundleId {logical_name} exists with incompatible platform {}",
                    bundle.attributes.platform
                );
                bundle_id_ids.insert(logical_name.clone(), bundle.id.clone());
                continue;
            }

            if self.mode == Mode::Plan {
                bundle_id_ids.insert(
                    logical_name.clone(),
                    format!("planned-bundle-id-{logical_name}"),
                );
                continue;
            }

            return Err(anyhow::anyhow!(
                "release scope requires existing bundleId {logical_name} ({})",
                spec.bundle_id
            ));
        }

        Ok(bundle_id_ids)
    }

    fn resolve_device_ids(
        &mut self,
        current_devices: &[Device],
    ) -> Result<BTreeMap<String, String>> {
        if self.scope.owns_devices() {
            self.reconcile_devices(current_devices)
        } else {
            Ok(BTreeMap::new())
        }
    }

    fn reconcile_bundle_ids(
        &mut self,
        current_bundle_ids: &[BundleId],
    ) -> Result<BTreeMap<String, String>> {
        let mut bundle_id_ids = BTreeMap::new();

        for (logical_name, spec) in &self.config.bundle_ids {
            let existing = self
                .state
                .bundle_ids
                .get(logical_name)
                .and_then(|managed| {
                    current_bundle_ids
                        .iter()
                        .find(|bundle| bundle.id == managed.apple_id)
                })
                .or_else(|| {
                    current_bundle_ids
                        .iter()
                        .find(|bundle| bundle.attributes.identifier == spec.bundle_id)
                });

            let bundle = if let Some(bundle) = existing {
                ensure!(
                    bundle.attributes.platform == spec.platform.asc_value()
                        || bundle.attributes.platform == "UNIVERSAL",
                    "bundleId {logical_name} exists with incompatible platform {}",
                    bundle.attributes.platform
                );
                if bundle.attributes.name != spec.name {
                    self.record(
                        ChangeKind::Update,
                        format!("bundleId.{logical_name}"),
                        "ensure bundleId name matches config".into(),
                    );
                    if self.mode == Mode::Apply {
                        self.client.update_bundle_id_name(&bundle.id, &spec.name)?
                    } else {
                        let mut updated = bundle.clone();
                        updated.attributes.name = spec.name.clone();
                        updated
                    }
                } else {
                    bundle.clone()
                }
            } else {
                self.record(
                    ChangeKind::Create,
                    format!("bundleId.{logical_name}"),
                    spec.bundle_id.clone(),
                );
                if self.mode == Mode::Apply {
                    self.client
                        .create_bundle_id(&spec.bundle_id, &spec.name, spec.platform)?
                } else {
                    BundleId {
                        id: format!("planned-bundle-id-{logical_name}"),
                        attributes: crate::asc::BundleIdAttributes {
                            identifier: spec.bundle_id.clone(),
                            name: spec.name.clone(),
                            platform: spec.platform.asc_value().into(),
                        },
                    }
                }
            };

            if self.mode == Mode::Apply {
                self.state.bundle_ids.insert(
                    logical_name.clone(),
                    ManagedBundleId {
                        apple_id: bundle.id.clone(),
                        bundle_id: spec.bundle_id.clone(),
                    },
                );
            }

            self.reconcile_capabilities(logical_name, &bundle.id, &spec.capabilities)?;
            bundle_id_ids.insert(logical_name.clone(), bundle.id.clone());
        }

        Ok(bundle_id_ids)
    }

    fn reconcile_capabilities(
        &mut self,
        bundle_id_name: &str,
        bundle_id_id: &str,
        desired_capabilities: &[crate::config::CapabilitySpec],
    ) -> Result<()> {
        if self.mode == Mode::Plan && bundle_id_id.starts_with("planned-bundle-id-") {
            for capability in desired_capabilities.iter().cloned() {
                let desired = capability.into_desired();
                self.record(
                    ChangeKind::Create,
                    format!(
                        "bundleId.{bundle_id_name}.capability.{}",
                        desired.capability_type
                    ),
                    "enable capability".into(),
                );
            }
            return Ok(());
        }

        let current = self.client.list_bundle_capabilities(bundle_id_id)?;
        let desired: BTreeMap<&'static str, DesiredCapability> = desired_capabilities
            .iter()
            .cloned()
            .map(|capability| {
                let desired = capability.into_desired();
                (desired.capability_type, desired)
            })
            .collect();

        let current_by_type: BTreeMap<&str, &BundleCapability> = current
            .iter()
            .map(|capability| (capability.attributes.capability_type.as_str(), capability))
            .collect();

        for current_capability in &current {
            let current_type = current_capability.attributes.capability_type.as_str();
            if let Some(desired_capability) = desired.get(current_type) {
                let current_normalized = self
                    .client
                    .normalize_capability(current_type, &current_capability.attributes.settings);
                let desired_normalized =
                    self.client.normalize_desired_capability(desired_capability);
                if current_normalized == desired_normalized {
                    continue;
                }

                self.record(
                    ChangeKind::Replace,
                    format!("bundleId.{bundle_id_name}.capability.{current_type}"),
                    "configuration drift".into(),
                );
                if self.mode == Mode::Apply {
                    self.client
                        .update_bundle_capability(&current_capability.id, desired_capability)?;
                }
                continue;
            }

            self.record(
                ChangeKind::Delete,
                format!("bundleId.{bundle_id_name}.capability.{current_type}"),
                "not present in config".into(),
            );
            if self.mode == Mode::Apply {
                self.client
                    .delete_bundle_capability(&current_capability.id)?;
            }
        }

        for (capability_type, desired_capability) in desired {
            if current_by_type.contains_key(capability_type) {
                continue;
            }
            self.record(
                ChangeKind::Create,
                format!("bundleId.{bundle_id_name}.capability.{capability_type}"),
                "enable capability".into(),
            );
            if self.mode == Mode::Apply {
                self.client
                    .create_bundle_capability(bundle_id_id, &desired_capability)?;
            }
        }

        Ok(())
    }

    fn reconcile_devices(
        &mut self,
        current_devices: &[Device],
    ) -> Result<BTreeMap<String, String>> {
        let mut device_ids = BTreeMap::new();

        for (logical_name, spec) in &self.config.devices {
            let existing = self
                .state
                .devices
                .get(logical_name)
                .and_then(|managed| {
                    current_devices
                        .iter()
                        .find(|device| device.id == managed.apple_id)
                })
                .or_else(|| {
                    current_devices
                        .iter()
                        .find(|device| device.attributes.udid == spec.udid)
                });

            let device = if let Some(device) = existing {
                ensure!(
                    device.attributes.platform == spec.family.asc_platform().asc_value()
                        && spec
                            .family
                            .matches_device_class(device.attributes.device_class.as_deref()),
                    "device {logical_name} exists with incompatible family/platform ({}, {:?})",
                    device.attributes.platform,
                    device.attributes.device_class
                );
                if device.attributes.name != spec.name || device.attributes.status != "ENABLED" {
                    self.record(
                        ChangeKind::Update,
                        format!("device.{logical_name}"),
                        "ensure device is enabled with expected name".into(),
                    );
                    if self.mode == Mode::Apply {
                        self.client.update_device(
                            &device.id,
                            Some(spec.name.as_str()),
                            Some("ENABLED"),
                        )?
                    } else {
                        let mut device = device.clone();
                        device.attributes.name = spec.name.clone();
                        device.attributes.status = "ENABLED".into();
                        device
                    }
                } else {
                    device.clone()
                }
            } else {
                self.record(
                    ChangeKind::Create,
                    format!("device.{logical_name}"),
                    spec.udid.clone(),
                );
                if self.mode == Mode::Apply {
                    self.client
                        .create_device(&spec.name, &spec.udid, spec.family)?
                } else {
                    Device {
                        id: format!("planned-device-{logical_name}"),
                        attributes: crate::asc::DeviceAttributes {
                            name: spec.name.clone(),
                            platform: spec.family.asc_platform().asc_value().into(),
                            device_class: None,
                            status: "ENABLED".into(),
                            udid: spec.udid.clone(),
                        },
                    }
                }
            };

            if self.mode == Mode::Apply {
                self.state.devices.insert(
                    logical_name.clone(),
                    ManagedDevice {
                        apple_id: device.id.clone(),
                        udid: spec.udid.clone(),
                    },
                );
            }
            device_ids.insert(logical_name.clone(), device.id.clone());
        }

        Ok(device_ids)
    }

    fn reconcile_certificates(
        &mut self,
        current_certificates: &[Certificate],
    ) -> Result<BTreeMap<String, String>> {
        let mut certificate_ids = BTreeMap::new();

        for (logical_name, spec) in self.scoped_cert_specs() {
            let managed = self.state.certs.get(&logical_name).cloned();
            let mut needs_rotation = false;
            if let Some(managed) = &managed {
                let existing_remote = current_certificates
                    .iter()
                    .find(|certificate| certificate.id == managed.apple_id);
                let remote_invalid = match existing_remote {
                    Some(certificate) => !certificate.is_active()?,
                    None => false,
                };
                needs_rotation = existing_remote.is_none()
                    || remote_invalid
                    || managed.kind != spec.kind.asc_value()
                    || managed.name != spec.name
                    || self.workspace.cert_bytes(&logical_name).is_none();
            }

            if !needs_rotation && let Some(managed) = managed {
                certificate_ids.insert(logical_name.clone(), managed.apple_id.clone());
                continue;
            }

            if managed.is_some() {
                self.record(
                    ChangeKind::Replace,
                    format!("cert.{logical_name}"),
                    "rotate managed certificate".into(),
                );
            } else {
                self.record(
                    ChangeKind::Create,
                    format!("cert.{logical_name}"),
                    spec.kind.asc_value().into(),
                );
            }

            if self.mode == Mode::Apply {
                let generated = system::generate_csr(&spec.name)?;
                let certificate = self
                    .client
                    .create_certificate(spec.kind.asc_value(), &generated.csr_pem)?;
                let p12_password = random_password();
                let pkcs12 = system::create_pkcs12_bytes(
                    &generated.key_path,
                    &certificate.attributes.certificate_content,
                    &p12_password,
                )?;
                system::import_pkcs12_bytes_into_login_keychain(
                    &logical_name,
                    &pkcs12,
                    &p12_password,
                )?;
                self.workspace.set_cert(logical_name.clone(), pkcs12);
                self.workspace
                    .set_cert_password(logical_name.clone(), p12_password.clone());

                let previous = self.state.certs.insert(
                    logical_name.clone(),
                    ManagedCertificate {
                        apple_id: certificate.id.clone(),
                        kind: spec.kind.asc_value().into(),
                        name: spec.name.clone(),
                        serial_number: certificate.attributes.serial_number.clone(),
                        p12_password,
                    },
                );
                if let Some(previous) = previous
                    && previous.apple_id != certificate.id
                {
                    self.client.revoke_certificate(&previous.apple_id)?;
                }
                certificate_ids.insert(logical_name.clone(), certificate.id);
            } else {
                certificate_ids
                    .insert(logical_name.clone(), format!("planned-cert-{logical_name}"));
            }
        }

        Ok(certificate_ids)
    }

    fn reconcile_profiles(
        &mut self,
        current_profiles: &[Profile],
        bundle_id_ids: &BTreeMap<String, String>,
        device_ids: &BTreeMap<String, String>,
        certificate_ids: &BTreeMap<String, String>,
    ) -> Result<()> {
        for (logical_name, spec) in self.scoped_profile_specs() {
            let desired_bundle_id_id = bundle_id_ids
                .get(&spec.bundle_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("missing bundleId id for profile {logical_name}"))?;
            let desired_certificate_ids: Vec<String> = spec
                .certs
                .iter()
                .map(|name| {
                    certificate_ids.get(name).cloned().ok_or_else(|| {
                        anyhow::anyhow!("missing certificate id for profile {logical_name}: {name}")
                    })
                })
                .collect::<Result<_>>()?;
            let desired_device_ids: Vec<String> = spec
                .devices
                .iter()
                .map(|name| {
                    device_ids.get(name).cloned().ok_or_else(|| {
                        anyhow::anyhow!("missing device id for profile {logical_name}: {name}")
                    })
                })
                .collect::<Result<_>>()?;
            let desired_certificate_set: BTreeSet<String> =
                desired_certificate_ids.iter().cloned().collect();
            let desired_device_set: BTreeSet<String> = desired_device_ids.iter().cloned().collect();

            let existing = self.find_matching_profile(
                &logical_name,
                current_profiles,
                ProfileMatchSpec {
                    profile_name: &spec.name,
                    profile_kind: spec.kind.asc_value(),
                    desired_bundle_id_id: &desired_bundle_id_id,
                    desired_certificate_ids: &desired_certificate_set,
                    desired_device_ids: &desired_device_set,
                },
            )?;

            if let Some(profile) = existing {
                if self.mode == Mode::Apply {
                    let profile_bytes =
                        system::decode_profile(&profile.profile.attributes.profile_content)?;
                    self.workspace
                        .set_profile(logical_name.clone(), profile_bytes);
                    self.state.profiles.insert(
                        logical_name.clone(),
                        ManagedProfile {
                            apple_id: profile.profile.id,
                            name: spec.name.clone(),
                            kind: spec.kind.asc_value().into(),
                            bundle_id: spec.bundle_id.clone(),
                            certs: spec.certs.clone(),
                            devices: spec.devices.clone(),
                            uuid: profile.profile.attributes.uuid,
                        },
                    );
                }
                continue;
            }

            let existing_managed =
                self.state
                    .profiles
                    .get(&logical_name)
                    .cloned()
                    .filter(|profile| {
                        current_profiles
                            .iter()
                            .any(|current| current.id == profile.apple_id)
                    });
            if let Some(existing_managed) = existing_managed {
                self.record(
                    ChangeKind::Replace,
                    format!("profile.{logical_name}"),
                    "profile membership changed".into(),
                );
                if self.mode == Mode::Apply {
                    self.client.delete_profile(&existing_managed.apple_id)?;
                    self.state.profiles.remove(&logical_name);
                }
            } else {
                self.record(
                    ChangeKind::Create,
                    format!("profile.{logical_name}"),
                    spec.kind.asc_value().into(),
                );
            }

            if self.mode == Mode::Apply {
                let profile = self.client.create_profile(
                    &spec.name,
                    spec.kind.asc_value(),
                    &desired_bundle_id_id,
                    &desired_certificate_ids,
                    &desired_device_ids,
                )?;
                let profile_bytes = system::decode_profile(&profile.attributes.profile_content)?;
                self.workspace
                    .set_profile(logical_name.clone(), profile_bytes);
                self.state.profiles.insert(
                    logical_name.clone(),
                    ManagedProfile {
                        apple_id: profile.id,
                        name: spec.name.clone(),
                        kind: spec.kind.asc_value().into(),
                        bundle_id: spec.bundle_id.clone(),
                        certs: spec.certs.clone(),
                        devices: spec.devices.clone(),
                        uuid: profile.attributes.uuid,
                    },
                );
            }
        }

        Ok(())
    }

    fn prune_removed_profiles(&mut self, current_profiles: &[Profile]) -> Result<()> {
        let removed: Vec<(String, ManagedProfile)> = self
            .state
            .profiles
            .iter()
            .filter(|(name, profile)| {
                managed_profile_scope(&profile.kind) == Some(self.scope)
                    && !self.scope_contains_profile(name)
            })
            .map(|(name, profile)| (name.clone(), profile.clone()))
            .collect();

        for (name, profile) in removed {
            self.record(
                ChangeKind::Delete,
                format!("profile.{name}"),
                "removed from config".into(),
            );
            if self.mode == Mode::Apply
                && current_profiles
                    .iter()
                    .any(|current| current.id == profile.apple_id)
            {
                self.client.delete_profile(&profile.apple_id)?;
            }
            if self.mode == Mode::Apply {
                self.state.profiles.remove(&name);
                self.workspace.remove_profile(&name);
            }
        }

        Ok(())
    }

    fn prune_removed_certificates(&mut self, current_certificates: &[Certificate]) -> Result<()> {
        let removed: Vec<(String, ManagedCertificate)> = self
            .state
            .certs
            .iter()
            .filter(|(name, cert)| {
                managed_certificate_scope(&cert.kind) == Some(self.scope)
                    && !self.scope_contains_cert(name)
            })
            .map(|(name, cert)| (name.clone(), cert.clone()))
            .collect();

        for (name, certificate) in removed {
            self.record(
                ChangeKind::Delete,
                format!("cert.{name}"),
                "removed from config".into(),
            );
            if self.mode == Mode::Apply
                && current_certificates
                    .iter()
                    .any(|current| current.id == certificate.apple_id)
            {
                self.client.revoke_certificate(&certificate.apple_id)?;
            }
            if self.mode == Mode::Apply {
                self.state.certs.remove(&name);
                self.workspace.remove_cert(&name);
            }
        }

        Ok(())
    }

    fn prune_removed_bundle_ids(&mut self, current_bundle_ids: &[BundleId]) -> Result<()> {
        let removed: Vec<(String, ManagedBundleId)> = self
            .state
            .bundle_ids
            .iter()
            .filter(|(name, _)| !self.config.bundle_ids.contains_key(*name))
            .map(|(name, bundle_id)| (name.clone(), bundle_id.clone()))
            .collect();

        for (name, bundle_id) in removed {
            self.record(
                ChangeKind::Delete,
                format!("bundleId.{name}"),
                "removed from config".into(),
            );
            if self.mode == Mode::Apply
                && current_bundle_ids
                    .iter()
                    .any(|current| current.id == bundle_id.apple_id)
            {
                self.client.delete_bundle_id(&bundle_id.apple_id)?;
            }
            if self.mode == Mode::Apply {
                self.state.bundle_ids.remove(&name);
            }
        }

        Ok(())
    }

    fn prune_removed_devices(&mut self, current_devices: &[Device]) -> Result<()> {
        let removed: Vec<(String, ManagedDevice)> = self
            .state
            .devices
            .iter()
            .filter(|(name, _)| !self.config.devices.contains_key(*name))
            .map(|(name, device)| (name.clone(), device.clone()))
            .collect();

        for (name, device) in removed {
            self.record(
                ChangeKind::Delete,
                format!("device.{name}"),
                "disable device removed from config".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(current) = current_devices
                    .iter()
                    .find(|current| current.id == device.apple_id)
                    && current.attributes.status != "DISABLED"
                {
                    self.client
                        .update_device(&device.apple_id, None, Some("DISABLED"))?;
                }
                self.state.devices.remove(&name);
            }
        }

        Ok(())
    }

    fn scoped_cert_specs(&self) -> Vec<(String, crate::config::CertificateSpec)> {
        self.config
            .certs
            .iter()
            .filter(|(_, spec)| spec.kind.scope() == self.scope)
            .map(|(logical_name, spec)| (logical_name.clone(), spec.clone()))
            .collect()
    }

    fn scoped_profile_specs(&self) -> Vec<(String, crate::config::ProfileSpec)> {
        self.config
            .profiles
            .iter()
            .filter(|(_, spec)| spec.kind.scope() == self.scope)
            .map(|(logical_name, spec)| (logical_name.clone(), spec.clone()))
            .collect()
    }

    fn scope_contains_cert(&self, logical_name: &str) -> bool {
        self.config
            .certs
            .get(logical_name)
            .is_some_and(|spec| spec.kind.scope() == self.scope)
    }

    fn scope_contains_profile(&self, logical_name: &str) -> bool {
        self.config
            .profiles
            .get(logical_name)
            .is_some_and(|spec| spec.kind.scope() == self.scope)
    }

    fn release_bundle_id_specs(&self) -> Result<BTreeMap<String, crate::config::BundleIdSpec>> {
        let mut scoped = BTreeMap::new();
        for (_, profile) in self.scoped_profile_specs() {
            let logical_name = profile.bundle_id.clone();
            let spec = self.config.bundle_ids.get(&logical_name).ok_or_else(|| {
                anyhow::anyhow!(
                    "release scope profile references unknown bundleId {}",
                    profile.bundle_id
                )
            })?;
            scoped.entry(logical_name).or_insert_with(|| spec.clone());
        }
        Ok(scoped)
    }

    fn find_matching_profile(
        &self,
        logical_name: &str,
        current_profiles: &[Profile],
        spec: ProfileMatchSpec<'_>,
    ) -> Result<Option<ProfileResolved>> {
        if let Some(managed) = self.state.profiles.get(logical_name)
            && let Some(profile) = current_profiles
                .iter()
                .find(|profile| profile.id == managed.apple_id)
        {
            let resolved = self.client.resolve_profile(profile.clone())?;
            if resolved.profile.is_active()? && self.profile_matches(&resolved, &spec) {
                return Ok(Some(resolved));
            }
        }

        for profile in current_profiles.iter().filter(|profile| {
            profile.attributes.name == spec.profile_name
                && profile.attributes.profile_type == spec.profile_kind
        }) {
            let resolved = self.client.resolve_profile(profile.clone())?;
            if resolved.profile.is_active()? && self.profile_matches(&resolved, &spec) {
                return Ok(Some(resolved));
            }
        }

        Ok(None)
    }

    fn profile_matches(&self, profile: &ProfileResolved, spec: &ProfileMatchSpec<'_>) -> bool {
        profile.profile.attributes.name == spec.profile_name
            && profile.profile.attributes.profile_type == spec.profile_kind
            && profile.bundle_id_id == spec.desired_bundle_id_id
            && &profile.certificate_ids == spec.desired_certificate_ids
            && &profile.device_ids == spec.desired_device_ids
    }

    fn record(&mut self, kind: ChangeKind, subject: String, detail: String) {
        self.changes.push(Change {
            kind,
            subject,
            detail,
        });
    }
}

fn random_password() -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(24)
        .map(char::from)
        .collect()
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

#[cfg(test)]
mod tests {
    use super::Workspace;
    use tempfile::tempdir;

    #[test]
    fn bundle_path_is_next_to_config() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join("asc.json");
        let workspace = Workspace::from_config_path(&config_path);

        assert_eq!(workspace.bundle_path, temp.path().join("signing.ascbundle"));
    }

    #[test]
    fn runtime_workspace_starts_empty() {
        let temp = tempdir().unwrap();
        let config_path = temp.path().join("asc.json");
        let workspace = Workspace::from_config_path(&config_path);
        let runtime = workspace.create_runtime().unwrap();

        assert!(runtime.cert_artifacts().is_empty());
        assert!(runtime.profile_artifacts().is_empty());
    }
}
