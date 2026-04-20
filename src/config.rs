use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Deserializer, de::Error as DeError};
use serde_json::Value;

use crate::scope::Scope;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct Config {
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    #[serde(default, rename = "_description")]
    pub description: Option<String>,
    pub team_id: String,
    #[serde(default)]
    pub bundle_ids: BTreeMap<String, BundleIdSpec>,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceSpec>,
    #[serde(default)]
    pub certs: BTreeMap<String, CertificateSpec>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct BundleIdSpec {
    pub bundle_id: String,
    pub name: String,
    pub platform: BundleIdPlatform,
    #[serde(default)]
    pub capabilities: Vec<CapabilitySpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSpec {
    pub family: DeviceFamily,
    pub udid: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertificateSpec {
    #[serde(rename = "type")]
    pub kind: CertificateKind,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProfileSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ProfileKind,
    pub bundle_id: String,
    pub certs: Vec<String>,
    #[serde(default)]
    pub devices: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleIdPlatform {
    Ios,
    MacOs,
    Universal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceFamily {
    Ios,
    Ipados,
    Watchos,
    Tvos,
    Visionos,
    Macos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificateKind {
    Development,
    Distribution,
    DeveloperIdApplication,
    DeveloperIdInstaller,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    IosAppDevelopment,
    IosAppStore,
    IosAppAdhoc,
    IosAppInhouse,
    TvosAppDevelopment,
    TvosAppStore,
    TvosAppAdhoc,
    TvosAppInhouse,
    MacAppDevelopment,
    MacAppStore,
    MacAppDirect,
    MacCatalystAppDevelopment,
    MacCatalystAppStore,
    MacCatalystAppDirect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimpleCapability {
    InAppPurchase,
    GameCenter,
    PushNotifications,
    Wallet,
    InterAppAudio,
    Maps,
    AssociatedDomains,
    PersonalVpn,
    AppGroups,
    Healthkit,
    Homekit,
    WirelessAccessoryConfiguration,
    ApplePay,
    Sirikit,
    NetworkExtensions,
    Multipath,
    HotSpot,
    NfcTagReading,
    Classkit,
    AutofillCredentialProvider,
    AccessWifiInformation,
    NetworkCustomProtocol,
    CoremediaHlsLowLatency,
    SystemExtensionInstall,
    UserManagement,
}

#[derive(Debug, Clone)]
pub enum CapabilitySpec {
    Simple(SimpleCapability),
    Icloud(IcloudCapabilitySpec),
    DataProtection(DataProtectionCapabilitySpec),
    AppleIdAuth(AppleIdAuthCapabilitySpec),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IcloudCapabilitySpec {
    pub version: IcloudVersion,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataProtectionCapabilitySpec {
    pub level: DataProtectionLevel,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppleIdAuthCapabilitySpec {
    pub app_consent: AppleIdAuthAppConsent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum IcloudVersion {
    #[serde(rename = "xcode_5")]
    Xcode5,
    #[serde(rename = "xcode_6")]
    Xcode6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataProtectionLevel {
    CompleteProtection,
    ProtectedUnlessOpen,
    ProtectedUntilFirstUserAuth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppleIdAuthAppConsent {
    PrimaryAppConsent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredCapability {
    pub capability_type: &'static str,
    pub settings: Vec<CapabilitySetting>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySetting {
    pub key: &'static str,
    pub options: Vec<CapabilityOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityOption {
    pub key: &'static str,
    pub enabled: bool,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.team_id.trim().is_empty(), "team_id cannot be empty");

        for (name, bundle_id) in &self.bundle_ids {
            validate_logical_key("bundle_id", name)?;
            ensure!(
                !bundle_id.bundle_id.trim().is_empty(),
                "bundle_id {name} bundle_id cannot be empty"
            );
            ensure!(
                !bundle_id.name.trim().is_empty(),
                "bundle_id {name} name cannot be empty"
            );
            validate_capabilities(name, &bundle_id.capabilities)?;
        }

        for (name, device) in &self.devices {
            validate_logical_key("device", name)?;
            ensure!(
                !device.udid.trim().is_empty(),
                "device {name} udid cannot be empty"
            );
            ensure!(
                !device.name.trim().is_empty(),
                "device {name} name cannot be empty"
            );
        }

        for (name, cert) in &self.certs {
            validate_logical_key("cert", name)?;
            ensure!(
                !cert.name.trim().is_empty(),
                "cert {name} name cannot be empty"
            );
        }

        for (name, profile) in &self.profiles {
            validate_logical_key("profile", name)?;
            ensure!(
                !profile.name.trim().is_empty(),
                "profile {name} name cannot be empty"
            );
            let bundle_id = self.bundle_ids.get(&profile.bundle_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "profile {name} references unknown bundle_id {}",
                    profile.bundle_id
                )
            })?;
            validate_profile_bundle_id_platform(name, profile.kind, bundle_id.platform)?;
            ensure!(
                !profile.certs.is_empty(),
                "profile {name} must reference at least one certificate"
            );
            for cert_name in &profile.certs {
                let cert = self.certs.get(cert_name).ok_or_else(|| {
                    anyhow::anyhow!("profile {name} references unknown cert {cert_name}")
                })?;
                validate_profile_certificate_kind(name, profile.kind, cert_name, cert.kind)?;
            }
            for device_name in &profile.devices {
                let device = self.devices.get(device_name).ok_or_else(|| {
                    anyhow::anyhow!("profile {name} references unknown device {device_name}")
                })?;
                validate_profile_device_platform(name, profile.kind, device)?;
            }

            if profile.kind.requires_devices() {
                ensure!(
                    !profile.devices.is_empty(),
                    "profile {name} of type {} must reference at least one device",
                    profile.kind
                );
            } else {
                ensure!(
                    profile.devices.is_empty(),
                    "profile {name} of type {} cannot reference devices",
                    profile.kind
                );
            }
        }

        Ok(())
    }

    pub fn present_scopes(&self) -> BTreeSet<Scope> {
        let mut scopes = BTreeSet::new();
        if !self.bundle_ids.is_empty() || !self.devices.is_empty() {
            scopes.insert(Scope::Developer);
        }
        for certificate in self.certs.values() {
            scopes.insert(certificate.kind.scope());
        }
        for profile in self.profiles.values() {
            scopes.insert(profile.kind.scope());
        }
        scopes
    }
}

impl CapabilitySpec {
    pub fn into_desired(self) -> DesiredCapability {
        match self {
            Self::Simple(simple) => DesiredCapability {
                capability_type: simple.asc_capability_type(),
                settings: Vec::new(),
            },
            Self::Icloud(icloud) => DesiredCapability {
                capability_type: "ICLOUD",
                settings: vec![CapabilitySetting {
                    key: "ICLOUD_VERSION",
                    options: vec![CapabilityOption {
                        key: icloud.version.asc_option_key(),
                        enabled: true,
                    }],
                }],
            },
            Self::DataProtection(data_protection) => DesiredCapability {
                capability_type: "DATA_PROTECTION",
                settings: vec![CapabilitySetting {
                    key: "DATA_PROTECTION_PERMISSION_LEVEL",
                    options: vec![CapabilityOption {
                        key: data_protection.level.asc_option_key(),
                        enabled: true,
                    }],
                }],
            },
            Self::AppleIdAuth(apple_id_auth) => DesiredCapability {
                capability_type: "APPLE_ID_AUTH",
                settings: vec![CapabilitySetting {
                    key: "APPLE_ID_AUTH_APP_CONSENT",
                    options: vec![CapabilityOption {
                        key: apple_id_auth.app_consent.asc_option_key(),
                        enabled: true,
                    }],
                }],
            },
        }
    }
}

impl BundleIdPlatform {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::Ios => "IOS",
            Self::MacOs => "MAC_OS",
            Self::Universal => "UNIVERSAL",
        }
    }
}

impl DeviceFamily {
    pub fn asc_platform(self) -> BundleIdPlatform {
        match self {
            Self::Ios | Self::Ipados | Self::Watchos | Self::Tvos | Self::Visionos => {
                BundleIdPlatform::Ios
            }
            Self::Macos => BundleIdPlatform::MacOs,
        }
    }

    pub fn matches_device_class(self, device_class: Option<&str>) -> bool {
        let Some(device_class) = device_class else {
            return true;
        };

        match self {
            Self::Ios => matches!(device_class, "IPHONE" | "IPOD"),
            Self::Ipados => device_class == "IPAD",
            Self::Watchos => device_class == "APPLE_WATCH",
            Self::Tvos => device_class == "APPLE_TV",
            Self::Visionos => matches!(device_class, "APPLE_VISION_PRO" | "VISION"),
            Self::Macos => device_class == "MAC",
        }
    }

    pub fn infer_from_product(product: &str) -> Option<Self> {
        if product.starts_with("iPhone") || product.starts_with("iPod") {
            return Some(Self::Ios);
        }
        if product.starts_with("iPad") {
            return Some(Self::Ipados);
        }
        if product.starts_with("Watch") {
            return Some(Self::Watchos);
        }
        if product.starts_with("AppleTV") {
            return Some(Self::Tvos);
        }
        if product.starts_with("Mac") {
            return Some(Self::Macos);
        }
        if product.starts_with("Reality")
            || product.starts_with("Vision")
            || product.starts_with("AppleVision")
        {
            return Some(Self::Visionos);
        }
        None
    }
}

impl CertificateKind {
    pub fn managed_kind(self) -> &'static str {
        match self {
            Self::Development => "DEVELOPMENT",
            Self::Distribution => "DISTRIBUTION",
            Self::DeveloperIdApplication => "DEVELOPER_ID_APPLICATION_G2",
            Self::DeveloperIdInstaller => "DEVELOPER_ID_INSTALLER",
        }
    }

    pub fn asc_create_value(self) -> Option<&'static str> {
        match self {
            Self::Development => Some("DEVELOPMENT"),
            Self::Distribution => Some("DISTRIBUTION"),
            Self::DeveloperIdApplication => None,
            Self::DeveloperIdInstaller => None,
        }
    }

    pub fn is_manually_provisioned(self) -> bool {
        self.asc_create_value().is_none()
    }

    pub fn manual_portal_display_name(self) -> Option<&'static str> {
        match self {
            Self::DeveloperIdApplication => Some("Developer ID Application"),
            Self::DeveloperIdInstaller => Some("Developer ID Installer"),
            Self::Development | Self::Distribution => None,
        }
    }

    pub fn allows_missing_apple_id(self) -> bool {
        matches!(self, Self::DeveloperIdInstaller)
    }

    pub fn scope(self) -> Scope {
        match self {
            Self::Development => Scope::Developer,
            Self::Distribution | Self::DeveloperIdApplication | Self::DeveloperIdInstaller => {
                Scope::Release
            }
        }
    }
}

impl ProfileKind {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::IosAppDevelopment => "IOS_APP_DEVELOPMENT",
            Self::IosAppStore => "IOS_APP_STORE",
            Self::IosAppAdhoc => "IOS_APP_ADHOC",
            Self::IosAppInhouse => "IOS_APP_INHOUSE",
            Self::TvosAppDevelopment => "TVOS_APP_DEVELOPMENT",
            Self::TvosAppStore => "TVOS_APP_STORE",
            Self::TvosAppAdhoc => "TVOS_APP_ADHOC",
            Self::TvosAppInhouse => "TVOS_APP_INHOUSE",
            Self::MacAppDevelopment => "MAC_APP_DEVELOPMENT",
            Self::MacAppStore => "MAC_APP_STORE",
            Self::MacAppDirect => "MAC_APP_DIRECT",
            Self::MacCatalystAppDevelopment => "MAC_CATALYST_APP_DEVELOPMENT",
            Self::MacCatalystAppStore => "MAC_CATALYST_APP_STORE",
            Self::MacCatalystAppDirect => "MAC_CATALYST_APP_DIRECT",
        }
    }

    pub fn requires_devices(self) -> bool {
        matches!(
            self,
            Self::IosAppDevelopment
                | Self::IosAppAdhoc
                | Self::TvosAppDevelopment
                | Self::TvosAppAdhoc
                | Self::MacAppDevelopment
                | Self::MacCatalystAppDevelopment
        )
    }

    pub fn expected_device_platform(self) -> Option<BundleIdPlatform> {
        match self {
            Self::IosAppDevelopment
            | Self::IosAppAdhoc
            | Self::TvosAppDevelopment
            | Self::TvosAppAdhoc => Some(BundleIdPlatform::Ios),
            Self::MacAppDevelopment | Self::MacCatalystAppDevelopment => {
                Some(BundleIdPlatform::MacOs)
            }
            _ => None,
        }
    }

    pub fn required_certificate_kind(self) -> CertificateKind {
        match self {
            Self::IosAppDevelopment
            | Self::TvosAppDevelopment
            | Self::MacAppDevelopment
            | Self::MacCatalystAppDevelopment => CertificateKind::Development,
            Self::MacAppDirect | Self::MacCatalystAppDirect => {
                CertificateKind::DeveloperIdApplication
            }
            Self::IosAppStore
            | Self::IosAppAdhoc
            | Self::IosAppInhouse
            | Self::TvosAppStore
            | Self::TvosAppAdhoc
            | Self::TvosAppInhouse
            | Self::MacAppStore
            | Self::MacCatalystAppStore => CertificateKind::Distribution,
        }
    }

    pub fn scope(self) -> Scope {
        match self {
            Self::IosAppDevelopment
            | Self::IosAppAdhoc
            | Self::TvosAppDevelopment
            | Self::TvosAppAdhoc
            | Self::MacAppDevelopment
            | Self::MacCatalystAppDevelopment => Scope::Developer,
            Self::IosAppStore
            | Self::IosAppInhouse
            | Self::TvosAppStore
            | Self::TvosAppInhouse
            | Self::MacAppStore
            | Self::MacAppDirect
            | Self::MacCatalystAppStore
            | Self::MacCatalystAppDirect => Scope::Release,
        }
    }

    pub fn supports_bundle_id_platform(self, bundle_id_platform: BundleIdPlatform) -> bool {
        match self {
            Self::IosAppDevelopment
            | Self::IosAppStore
            | Self::IosAppAdhoc
            | Self::IosAppInhouse
            | Self::TvosAppDevelopment
            | Self::TvosAppStore
            | Self::TvosAppAdhoc
            | Self::TvosAppInhouse => {
                matches!(
                    bundle_id_platform,
                    BundleIdPlatform::Ios | BundleIdPlatform::Universal
                )
            }
            Self::MacAppDevelopment | Self::MacAppStore | Self::MacAppDirect => {
                matches!(
                    bundle_id_platform,
                    BundleIdPlatform::MacOs | BundleIdPlatform::Universal
                )
            }
            Self::MacCatalystAppDevelopment
            | Self::MacCatalystAppStore
            | Self::MacCatalystAppDirect => {
                matches!(
                    bundle_id_platform,
                    BundleIdPlatform::Ios | BundleIdPlatform::Universal
                )
            }
        }
    }

    pub fn supports_device_family(self, device_family: DeviceFamily) -> bool {
        match self {
            Self::IosAppDevelopment | Self::IosAppAdhoc => {
                matches!(device_family, DeviceFamily::Ios | DeviceFamily::Ipados)
            }
            Self::TvosAppDevelopment | Self::TvosAppAdhoc => device_family == DeviceFamily::Tvos,
            Self::MacAppDevelopment | Self::MacCatalystAppDevelopment => {
                device_family == DeviceFamily::Macos
            }
            _ => false,
        }
    }
}

impl fmt::Display for ProfileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IosAppDevelopment => write!(f, "ios_app_development"),
            Self::IosAppStore => write!(f, "ios_app_store"),
            Self::IosAppAdhoc => write!(f, "ios_app_adhoc"),
            Self::IosAppInhouse => write!(f, "ios_app_inhouse"),
            Self::TvosAppDevelopment => write!(f, "tvos_app_development"),
            Self::TvosAppStore => write!(f, "tvos_app_store"),
            Self::TvosAppAdhoc => write!(f, "tvos_app_adhoc"),
            Self::TvosAppInhouse => write!(f, "tvos_app_inhouse"),
            Self::MacAppDevelopment => write!(f, "mac_app_development"),
            Self::MacAppStore => write!(f, "mac_app_store"),
            Self::MacAppDirect => write!(f, "mac_app_direct"),
            Self::MacCatalystAppDevelopment => write!(f, "mac_catalyst_app_development"),
            Self::MacCatalystAppStore => write!(f, "mac_catalyst_app_store"),
            Self::MacCatalystAppDirect => write!(f, "mac_catalyst_app_direct"),
        }
    }
}

impl fmt::Display for BundleIdPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ios => write!(f, "ios"),
            Self::MacOs => write!(f, "mac_os"),
            Self::Universal => write!(f, "universal"),
        }
    }
}

impl fmt::Display for DeviceFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ios => write!(f, "ios"),
            Self::Ipados => write!(f, "ipados"),
            Self::Watchos => write!(f, "watchos"),
            Self::Tvos => write!(f, "tvos"),
            Self::Visionos => write!(f, "visionos"),
            Self::Macos => write!(f, "macos"),
        }
    }
}

impl fmt::Display for CertificateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Development => write!(f, "development"),
            Self::Distribution => write!(f, "distribution"),
            Self::DeveloperIdApplication => write!(f, "developer_id_application"),
            Self::DeveloperIdInstaller => write!(f, "developer_id_installer"),
        }
    }
}

impl SimpleCapability {
    pub fn asc_capability_type(self) -> &'static str {
        match self {
            Self::InAppPurchase => "IN_APP_PURCHASE",
            Self::GameCenter => "GAME_CENTER",
            Self::PushNotifications => "PUSH_NOTIFICATIONS",
            Self::Wallet => "WALLET",
            Self::InterAppAudio => "INTER_APP_AUDIO",
            Self::Maps => "MAPS",
            Self::AssociatedDomains => "ASSOCIATED_DOMAINS",
            Self::PersonalVpn => "PERSONAL_VPN",
            Self::AppGroups => "APP_GROUPS",
            Self::Healthkit => "HEALTHKIT",
            Self::Homekit => "HOMEKIT",
            Self::WirelessAccessoryConfiguration => "WIRELESS_ACCESSORY_CONFIGURATION",
            Self::ApplePay => "APPLE_PAY",
            Self::Sirikit => "SIRIKIT",
            Self::NetworkExtensions => "NETWORK_EXTENSIONS",
            Self::Multipath => "MULTIPATH",
            Self::HotSpot => "HOT_SPOT",
            Self::NfcTagReading => "NFC_TAG_READING",
            Self::Classkit => "CLASSKIT",
            Self::AutofillCredentialProvider => "AUTOFILL_CREDENTIAL_PROVIDER",
            Self::AccessWifiInformation => "ACCESS_WIFI_INFORMATION",
            Self::NetworkCustomProtocol => "NETWORK_CUSTOM_PROTOCOL",
            Self::CoremediaHlsLowLatency => "COREMEDIA_HLS_LOW_LATENCY",
            Self::SystemExtensionInstall => "SYSTEM_EXTENSION_INSTALL",
            Self::UserManagement => "USER_MANAGEMENT",
        }
    }
}

impl IcloudVersion {
    pub fn asc_option_key(self) -> &'static str {
        match self {
            Self::Xcode5 => "XCODE_5",
            Self::Xcode6 => "XCODE_6",
        }
    }
}

impl DataProtectionLevel {
    pub fn asc_option_key(self) -> &'static str {
        match self {
            Self::CompleteProtection => "COMPLETE_PROTECTION",
            Self::ProtectedUnlessOpen => "PROTECTED_UNLESS_OPEN",
            Self::ProtectedUntilFirstUserAuth => "PROTECTED_UNTIL_FIRST_USER_AUTH",
        }
    }
}

impl AppleIdAuthAppConsent {
    pub fn asc_option_key(self) -> &'static str {
        match self {
            Self::PrimaryAppConsent => "PRIMARY_APP_CONSENT",
        }
    }
}

fn validate_profile_certificate_kind(
    profile_name: &str,
    profile_kind: ProfileKind,
    cert_name: &str,
    cert_kind: CertificateKind,
) -> Result<()> {
    let required = profile_kind.required_certificate_kind();
    if cert_kind != required {
        bail!(
            "profile {profile_name} requires {} certificates, but {cert_name} is {}",
            required,
            cert_kind
        );
    }

    Ok(())
}

fn validate_capabilities(bundle_id_name: &str, capabilities: &[CapabilitySpec]) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for capability in capabilities {
        let capability_type = match capability {
            CapabilitySpec::Simple(simple) => simple.asc_capability_type(),
            CapabilitySpec::Icloud(_) => "ICLOUD",
            CapabilitySpec::DataProtection(_) => "DATA_PROTECTION",
            CapabilitySpec::AppleIdAuth(_) => "APPLE_ID_AUTH",
        };

        ensure!(
            seen.insert(capability_type),
            "bundle_id {bundle_id_name} defines capability {capability_type} more than once"
        );
    }

    Ok(())
}

fn validate_logical_key(kind: &str, key: &str) -> Result<()> {
    ensure!(!key.trim().is_empty(), "{kind} name key cannot be empty");
    ensure!(
        key != "." && key != "..",
        "{kind} name key {key:?} is reserved"
    );
    ensure!(
        key.chars()
            .all(|character| character.is_ascii_alphanumeric()
                || matches!(character, '.' | '-' | '_')),
        "{kind} name key {key:?} must contain only ASCII letters, digits, '.', '-', or '_'"
    );
    Ok(())
}

fn validate_profile_device_platform(
    profile_name: &str,
    profile_kind: ProfileKind,
    device: &DeviceSpec,
) -> Result<()> {
    if let Some(expected) = profile_kind.expected_device_platform()
        && (device.family.asc_platform() != expected
            || !profile_kind.supports_device_family(device.family))
    {
        bail!(
            "profile {profile_name} of type {} requires compatible {} devices, but one device is {}",
            profile_kind,
            expected,
            device.family
        );
    }

    Ok(())
}

fn validate_profile_bundle_id_platform(
    profile_name: &str,
    profile_kind: ProfileKind,
    bundle_id_platform: BundleIdPlatform,
) -> Result<()> {
    if !profile_kind.supports_bundle_id_platform(bundle_id_platform) {
        bail!(
            "profile {profile_name} of type {} is incompatible with bundle_id platform {}",
            profile_kind,
            bundle_id_platform
        );
    }

    Ok(())
}

impl<'de> Deserialize<'de> for CapabilitySpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(_) => {
                let simple =
                    serde_json::from_value::<SimpleCapability>(value).map_err(D::Error::custom)?;
                Ok(Self::Simple(simple))
            }
            Value::Object(map) => {
                if map.len() != 1 {
                    return Err(D::Error::custom(
                        "capability object must contain exactly one key",
                    ));
                }

                let (key, inner) = map.into_iter().next().expect("map len checked above");
                match key.as_str() {
                    "icloud" => {
                        let spec = serde_json::from_value::<IcloudCapabilitySpec>(inner)
                            .map_err(D::Error::custom)?;
                        Ok(Self::Icloud(spec))
                    }
                    "data_protection" => {
                        let spec = serde_json::from_value::<DataProtectionCapabilitySpec>(inner)
                            .map_err(D::Error::custom)?;
                        Ok(Self::DataProtection(spec))
                    }
                    "apple_id_auth" => {
                        let spec = serde_json::from_value::<AppleIdAuthCapabilitySpec>(inner)
                            .map_err(D::Error::custom)?;
                        Ok(Self::AppleIdAuth(spec))
                    }
                    other => Err(D::Error::custom(format!(
                        "unsupported configurable capability object key {other}"
                    ))),
                }
            }
            _ => Err(D::Error::custom(
                "capability must be a string or a single-key object",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CertificateKind, Config};

    #[test]
    fn accepts_published_schema_field() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "$schema": "https://orbitstorage.dev/schemas/asc-sync.schema-0.1.0.json",
                "team_id": "TEAM123"
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(
            config.schema.as_deref(),
            Some("https://orbitstorage.dev/schemas/asc-sync.schema-0.1.0.json")
        );
    }

    #[test]
    fn accepts_description_field() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "_description": "This file is documented by its `$schema`.",
                "team_id": "TEAM123"
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(
            config.description.as_deref(),
            Some("This file is documented by its `$schema`.")
        );
    }

    #[test]
    fn rejects_legacy_camel_case_fields() {
        assert!(
            serde_json::from_str::<Config>(
                r#"{
                    "teamId": "TEAM123"
                }"#,
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Config>(
                r#"{
                    "team_id": "TEAM123",
                    "bundle_ids": {
                        "main": {
                            "bundleId": "com.example.app",
                            "name": "App",
                            "platform": "ios"
                        }
                    }
                }"#,
            )
            .is_err()
        );
    }

    #[test]
    fn parses_multiple_profile_certificates() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": { "bundle_id": "com.example.app", "name": "App", "platform": "ios" }
                },
                "devices": {
                    "iphone": { "family": "ios", "udid": "abc", "name": "My Phone" }
                },
                "certs": {
                    "dist-a": { "type": "distribution", "name": "Dist A" },
                    "dist-b": { "type": "distribution", "name": "Dist B" }
                },
                "profiles": {
                    "adhoc": {
                        "name": "Ad Hoc",
                        "type": "ios_app_adhoc",
                        "bundle_id": "main",
                        "certs": ["dist-a", "dist-b"],
                        "devices": ["iphone"]
                    }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(config.profiles["adhoc"].certs.len(), 2);
    }

    #[test]
    fn parses_configurable_capabilities() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": {
                        "bundle_id": "com.example.app",
                        "name": "App",
                        "platform": "ios",
                        "capabilities": [
                            { "icloud": { "version": "xcode_6" } },
                            { "data_protection": { "level": "protected_until_first_user_auth" } },
                            { "apple_id_auth": { "app_consent": "primary_app_consent" } }
                        ]
                    }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(config.bundle_ids["main"].capabilities.len(), 3);
    }

    #[test]
    fn rejects_duplicate_capabilities() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": {
                        "bundle_id": "com.example.app",
                        "name": "App",
                        "platform": "ios",
                        "capabilities": [
                            "push_notifications",
                            "push_notifications"
                        ]
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_direct_distribution_profile() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "desktop": { "bundle_id": "com.example.desktop", "name": "Desktop", "platform": "mac_os" }
                },
                "certs": {
                    "direct": { "type": "developer_id_application", "name": "Developer ID" }
                },
                "profiles": {
                    "direct": {
                        "name": "Direct",
                        "type": "mac_app_direct",
                        "bundle_id": "desktop",
                        "certs": ["direct"]
                    }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
    }

    #[test]
    fn accepts_developer_id_installer_certificate() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "certs": {
                    "installer": { "type": "developer_id_installer", "name": "Installer" }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
    }

    #[test]
    fn developer_id_application_uses_g2_manual_provisioning() {
        assert_eq!(
            CertificateKind::DeveloperIdApplication.managed_kind(),
            "DEVELOPER_ID_APPLICATION_G2"
        );
        assert_eq!(
            CertificateKind::DeveloperIdApplication.asc_create_value(),
            None
        );
        assert_eq!(
            CertificateKind::DeveloperIdApplication.manual_portal_display_name(),
            Some("Developer ID Application")
        );
        assert!(!CertificateKind::DeveloperIdApplication.allows_missing_apple_id());
    }

    #[test]
    fn developer_id_installer_requires_manual_provisioning() {
        assert_eq!(
            CertificateKind::DeveloperIdInstaller.managed_kind(),
            "DEVELOPER_ID_INSTALLER"
        );
        assert_eq!(
            CertificateKind::DeveloperIdInstaller.asc_create_value(),
            None
        );
        assert_eq!(
            CertificateKind::DeveloperIdInstaller.manual_portal_display_name(),
            Some("Developer ID Installer")
        );
        assert!(CertificateKind::DeveloperIdInstaller.allows_missing_apple_id());
    }

    #[test]
    fn rejects_installer_certificate_for_direct_distribution_profile() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "desktop": { "bundle_id": "com.example.desktop", "name": "Desktop", "platform": "mac_os" }
                },
                "certs": {
                    "installer": { "type": "developer_id_installer", "name": "Installer" }
                },
                "profiles": {
                    "direct": {
                        "name": "Direct",
                        "type": "mac_app_direct",
                        "bundle_id": "desktop",
                        "certs": ["installer"]
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_incompatible_bundle_id_platform() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": { "bundle_id": "com.example.app", "name": "App", "platform": "ios" }
                },
                "certs": {
                    "direct": { "type": "developer_id_application", "name": "Developer ID" }
                },
                "profiles": {
                    "direct": {
                        "name": "Direct",
                        "type": "mac_app_direct",
                        "bundle_id": "main",
                        "certs": ["direct"]
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_ipados_device_for_ios_profile() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": { "bundle_id": "com.example.app", "name": "App", "platform": "ios" }
                },
                "devices": {
                    "ipad": { "family": "ipados", "udid": "abc", "name": "My iPad" }
                },
                "certs": {
                    "dev": { "type": "development", "name": "Dev" }
                },
                "profiles": {
                    "development": {
                        "name": "Development",
                        "type": "ios_app_development",
                        "bundle_id": "main",
                        "certs": ["dev"],
                        "devices": ["ipad"]
                    }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
    }

    #[test]
    fn rejects_tvos_device_for_ios_profile() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": { "bundle_id": "com.example.app", "name": "App", "platform": "ios" }
                },
                "devices": {
                    "appletv": { "family": "tvos", "udid": "abc", "name": "Apple TV" }
                },
                "certs": {
                    "dev": { "type": "development", "name": "Dev" }
                },
                "profiles": {
                    "development": {
                        "name": "Development",
                        "type": "ios_app_development",
                        "bundle_id": "main",
                        "certs": ["dev"],
                        "devices": ["appletv"]
                    }
                }
            }"#,
        )
        .unwrap();

        assert!(config.validate().is_err());
    }

    #[test]
    fn accepts_tvos_development_profile() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "tv-app": { "bundle_id": "com.example.tv", "name": "TV App", "platform": "ios" }
                },
                "devices": {
                    "living-room": { "family": "tvos", "udid": "abc", "name": "Apple TV" }
                },
                "certs": {
                    "dev": { "type": "development", "name": "Dev" }
                },
                "profiles": {
                    "tv-development": {
                        "name": "TV Development",
                        "type": "tvos_app_development",
                        "bundle_id": "tv-app",
                        "certs": ["dev"],
                        "devices": ["living-room"]
                    }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
    }

    #[test]
    fn accepts_ios_inhouse_profile() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": { "bundle_id": "com.example.app", "name": "App", "platform": "ios" }
                },
                "certs": {
                    "dist": { "type": "distribution", "name": "Dist" }
                },
                "profiles": {
                    "inhouse": {
                        "name": "In House",
                        "type": "ios_app_inhouse",
                        "bundle_id": "main",
                        "certs": ["dist"]
                    }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
    }

    #[test]
    fn rejects_logical_keys_with_path_separators() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "bad/name": { "bundle_id": "com.example.app", "name": "App", "platform": "ios" }
                }
            }"#,
        )
        .unwrap();

        assert!(config.validate().is_err());
    }
}
