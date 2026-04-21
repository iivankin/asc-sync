use std::{
    collections::{BTreeMap, BTreeSet},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode, blocking::Client, header::RETRY_AFTER};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    auth::AuthContext,
    config::{BundleIdPlatform, DesiredCapability, DeviceFamily},
};

const ASC_BASE_URL: &str = "https://api.appstoreconnect.apple.com/v1";

fn asc_base_url() -> String {
    std::env::var("ORBI_ASC_BASE_URL").unwrap_or_else(|_| ASC_BASE_URL.to_owned())
}

pub(crate) fn asc_endpoint(path: &str) -> String {
    let base = asc_base_url();
    if (path.starts_with("/v2/") || path.starts_with("/v3/"))
        && let Some(root) = base.strip_suffix("/v1")
    {
        return format!("{root}{path}");
    }
    format!("{base}{path}")
}

#[derive(Debug)]
pub struct AscClient {
    http: Client,
    auth: AuthContext,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleId {
    pub id: String,
    pub attributes: BundleIdAttributes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleIdAttributes {
    pub identifier: String,
    pub name: String,
    pub platform: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleCapability {
    pub id: String,
    pub attributes: BundleCapabilityAttributes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleCapabilityAttributes {
    #[serde(rename = "capabilityType")]
    pub capability_type: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_null_default")]
    pub settings: Vec<RemoteCapabilitySetting>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteCapabilitySetting {
    pub key: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_null_default")]
    pub options: Vec<RemoteCapabilityOption>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteCapabilityOption {
    pub key: String,
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Device {
    pub id: String,
    pub attributes: DeviceAttributes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceAttributes {
    pub name: String,
    pub platform: String,
    #[serde(rename = "deviceClass", default)]
    pub device_class: Option<String>,
    pub status: String,
    pub udid: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Certificate {
    pub id: String,
    pub attributes: CertificateAttributes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CertificateAttributes {
    #[serde(rename = "certificateType")]
    pub certificate_type: String,
    #[serde(rename = "certificateContent", default)]
    pub certificate_content: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "serialNumber")]
    pub serial_number: String,
    #[serde(rename = "expirationDate", default)]
    pub expiration_date: Option<String>,
    #[serde(default)]
    pub activated: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub id: String,
    pub attributes: ProfileAttributes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileAttributes {
    pub name: String,
    pub platform: String,
    #[serde(rename = "profileType")]
    pub profile_type: String,
    #[serde(rename = "profileState", default)]
    pub profile_state: Option<String>,
    #[serde(rename = "profileContent", default)]
    pub profile_content: String,
    pub uuid: String,
    #[serde(rename = "expirationDate", default)]
    pub expiration_date: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProfileResolved {
    pub profile: Profile,
    pub bundle_id_id: String,
    pub certificate_ids: BTreeSet<String>,
    pub device_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct App {
    pub id: String,
    pub attributes: AppAttributes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppAttributes {
    pub name: String,
    pub sku: String,
    #[serde(rename = "bundleId")]
    pub bundle_id: String,
    #[serde(rename = "primaryLocale")]
    pub primary_locale: String,
    #[serde(rename = "accessibilityUrl", default)]
    pub accessibility_url: Option<String>,
    #[serde(rename = "contentRightsDeclaration", default)]
    pub content_rights_declaration: Option<String>,
    #[serde(rename = "subscriptionStatusUrl", default)]
    pub subscription_status_url: Option<String>,
    #[serde(rename = "subscriptionStatusUrlForSandbox", default)]
    pub subscription_status_url_for_sandbox: Option<String>,
    #[serde(rename = "streamlinedPurchasingEnabled", default)]
    pub streamlined_purchasing_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct JsonApiList<T> {
    data: Vec<T>,
    #[serde(default)]
    links: PaginationLinks,
}

#[derive(Debug, Deserialize, Default)]
struct PaginationLinks {
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JsonApiSingle<T> {
    data: T,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    errors: Vec<AscErrorItem>,
}

#[derive(Debug, Deserialize)]
struct AscErrorItem {
    status: Option<String>,
    code: Option<String>,
    title: Option<String>,
    detail: Option<String>,
}

fn deserialize_vec_or_null_default<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

fn is_expired_at(timestamp: Option<&str>) -> Result<bool> {
    let Some(timestamp) = timestamp else {
        return Ok(false);
    };
    let parsed = OffsetDateTime::parse(timestamp, &Rfc3339)
        .with_context(|| format!("failed to parse ASC timestamp {timestamp}"))?;
    Ok(parsed <= OffsetDateTime::now_utc())
}

impl Certificate {
    pub fn is_active(&self) -> Result<bool> {
        if self.attributes.activated == Some(false) {
            return Ok(false);
        }
        Ok(!is_expired_at(self.attributes.expiration_date.as_deref())?)
    }
}

impl Profile {
    pub fn is_active(&self) -> Result<bool> {
        if let Some(state) = self.attributes.profile_state.as_deref()
            && state != "ACTIVE"
        {
            return Ok(false);
        }
        Ok(!is_expired_at(self.attributes.expiration_date.as_deref())?)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
enum ProfileIncluded {
    #[serde(rename = "bundleIds")]
    BundleId(BundleId),
    #[serde(rename = "certificates")]
    Certificate(Certificate),
    #[serde(rename = "devices")]
    Device(Device),
}

#[derive(Debug, Deserialize)]
struct ProfileResponseWithIncluded {
    data: Profile,
    #[serde(default)]
    included: Vec<ProfileIncluded>,
}

impl AscClient {
    pub fn new(auth: AuthContext) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("asc-sync/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(180))
            .build()
            .context("failed to create ASC HTTP client")?;
        Ok(Self { http, auth })
    }

    pub fn list_bundle_ids(&self) -> Result<Vec<BundleId>> {
        self.get_paginated(
            format!("{}/bundleIds", asc_base_url()),
            vec![
                (
                    "fields[bundleIds]".into(),
                    "identifier,name,platform".into(),
                ),
                ("limit".into(), "200".into()),
            ],
        )
    }

    pub fn create_bundle_id(
        &self,
        identifier: &str,
        name: &str,
        platform: BundleIdPlatform,
    ) -> Result<BundleId> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            data: Data<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Data<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            attributes: Attributes<'a>,
        }
        #[derive(Serialize)]
        struct Attributes<'a> {
            identifier: &'a str,
            name: &'a str,
            platform: &'a str,
        }

        let request = Request {
            data: Data {
                kind: "bundleIds",
                attributes: Attributes {
                    identifier,
                    name,
                    platform: platform.asc_value(),
                },
            },
        };

        self.request_json(
            Method::POST,
            format!("{}/bundleIds", asc_base_url()),
            &[],
            Some(&request),
        )
        .map(|response: JsonApiSingle<BundleId>| response.data)
    }

    pub fn delete_bundle_id(&self, bundle_id_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            format!("{}/bundleIds/{bundle_id_id}", asc_base_url()),
            &[],
            None::<&Value>,
        )
    }

    pub fn find_bundle_id_by_identifier(&self, identifier: &str) -> Result<Option<BundleId>> {
        let bundle_ids = self.get_paginated(
            format!("{}/bundleIds", asc_base_url()),
            vec![
                ("filter[identifier]".into(), identifier.into()),
                (
                    "fields[bundleIds]".into(),
                    "identifier,name,platform".into(),
                ),
                ("limit".into(), "2".into()),
            ],
        )?;
        Ok(bundle_ids.into_iter().next())
    }

    pub fn find_app_by_bundle_id(&self, bundle_id_identifier: &str) -> Result<Option<App>> {
        let apps = self.get_paginated(
            format!("{}/apps", asc_base_url()),
            vec![
                ("limit".into(), "2".into()),
                ("filter[bundleId]".into(), bundle_id_identifier.into()),
                (
                    "fields[apps]".into(),
                    "name,sku,primaryLocale,bundleId,accessibilityUrl,contentRightsDeclaration,subscriptionStatusUrl,subscriptionStatusUrlForSandbox,streamlinedPurchasingEnabled".into(),
                ),
            ],
        )?;
        Ok(apps.into_iter().next())
    }

    pub fn update_bundle_id_name(&self, bundle_id_id: &str, name: &str) -> Result<BundleId> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            data: Data<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Data<'a> {
            id: &'a str,
            #[serde(rename = "type")]
            kind: &'static str,
            attributes: Attributes<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Attributes<'a> {
            name: &'a str,
        }

        let request = Request {
            data: Data {
                id: bundle_id_id,
                kind: "bundleIds",
                attributes: Attributes { name },
            },
        };

        self.request_json(
            Method::PATCH,
            format!("{}/bundleIds/{bundle_id_id}", asc_base_url()),
            &[],
            Some(&request),
        )
        .map(|response: JsonApiSingle<BundleId>| response.data)
    }

    pub fn list_bundle_capabilities(&self, bundle_id_id: &str) -> Result<Vec<BundleCapability>> {
        self.get_paginated(
            format!(
                "{}/bundleIds/{bundle_id_id}/bundleIdCapabilities",
                asc_base_url()
            ),
            vec![(
                "fields[bundleIdCapabilities]".into(),
                "capabilityType,settings".into(),
            )],
        )
    }

    pub fn create_bundle_capability(
        &self,
        bundle_id_id: &str,
        capability: &DesiredCapability,
    ) -> Result<()> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            data: Data<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Data<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            attributes: Attributes<'a>,
            relationships: Relationships<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Attributes<'a> {
            capability_type: &'a str,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            settings: Vec<Setting<'a>>,
        }
        #[derive(Serialize)]
        struct Setting<'a> {
            key: &'a str,
            options: Vec<OptionItem<'a>>,
        }
        #[derive(Serialize)]
        struct OptionItem<'a> {
            key: &'a str,
            enabled: bool,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Relationships<'a> {
            bundle_id: Relationship<'a>,
        }
        #[derive(Serialize)]
        struct Relationship<'a> {
            data: RelationshipData<'a>,
        }
        #[derive(Serialize)]
        struct RelationshipData<'a> {
            id: &'a str,
            #[serde(rename = "type")]
            kind: &'static str,
        }

        let settings = capability
            .settings
            .iter()
            .map(|setting| Setting {
                key: setting.key,
                options: setting
                    .options
                    .iter()
                    .map(|option| OptionItem {
                        key: option.key,
                        enabled: option.enabled,
                    })
                    .collect(),
            })
            .collect();
        let request = Request {
            data: Data {
                kind: "bundleIdCapabilities",
                attributes: Attributes {
                    capability_type: capability.capability_type,
                    settings,
                },
                relationships: Relationships {
                    bundle_id: Relationship {
                        data: RelationshipData {
                            id: bundle_id_id,
                            kind: "bundleIds",
                        },
                    },
                },
            },
        };

        self.request_empty(
            Method::POST,
            format!("{}/bundleIdCapabilities", asc_base_url()),
            &[],
            Some(&request),
        )
    }

    pub fn update_bundle_capability(
        &self,
        capability_id: &str,
        capability: &DesiredCapability,
    ) -> Result<()> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            data: Data<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Data<'a> {
            id: &'a str,
            #[serde(rename = "type")]
            kind: &'static str,
            attributes: Attributes<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Attributes<'a> {
            settings: Vec<Setting<'a>>,
        }
        #[derive(Serialize)]
        struct Setting<'a> {
            key: &'a str,
            options: Vec<OptionItem<'a>>,
        }
        #[derive(Serialize)]
        struct OptionItem<'a> {
            key: &'a str,
            enabled: bool,
        }

        let request = Request {
            data: Data {
                id: capability_id,
                kind: "bundleIdCapabilities",
                attributes: Attributes {
                    settings: capability
                        .settings
                        .iter()
                        .map(|setting| Setting {
                            key: setting.key,
                            options: setting
                                .options
                                .iter()
                                .map(|option| OptionItem {
                                    key: option.key,
                                    enabled: option.enabled,
                                })
                                .collect(),
                        })
                        .collect(),
                },
            },
        };

        self.request_empty(
            Method::PATCH,
            format!("{}/bundleIdCapabilities/{capability_id}", asc_base_url()),
            &[],
            Some(&request),
        )
    }

    pub fn delete_bundle_capability(&self, capability_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            format!("{}/bundleIdCapabilities/{capability_id}", asc_base_url()),
            &[],
            None::<&Value>,
        )
    }

    pub fn list_devices(&self) -> Result<Vec<Device>> {
        self.get_paginated(
            format!("{}/devices", asc_base_url()),
            vec![
                (
                    "fields[devices]".into(),
                    "name,platform,deviceClass,status,udid".into(),
                ),
                ("limit".into(), "200".into()),
            ],
        )
    }

    pub fn create_device(&self, name: &str, udid: &str, family: DeviceFamily) -> Result<Device> {
        #[derive(Serialize)]
        struct Request<'a> {
            data: Data<'a>,
        }
        #[derive(Serialize)]
        struct Data<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            attributes: Attributes<'a>,
        }
        #[derive(Serialize)]
        struct Attributes<'a> {
            name: &'a str,
            platform: &'a str,
            udid: &'a str,
        }

        let request = Request {
            data: Data {
                kind: "devices",
                attributes: Attributes {
                    name,
                    platform: family.asc_platform().asc_value(),
                    udid,
                },
            },
        };

        self.request_json(
            Method::POST,
            format!("{}/devices", asc_base_url()),
            &[],
            Some(&request),
        )
        .map(|response: JsonApiSingle<Device>| response.data)
    }

    pub fn update_device(
        &self,
        device_id: &str,
        name: Option<&str>,
        status: Option<&str>,
    ) -> Result<Device> {
        #[derive(Serialize)]
        struct Request<'a> {
            data: Data<'a>,
        }
        #[derive(Serialize)]
        struct Data<'a> {
            id: &'a str,
            #[serde(rename = "type")]
            kind: &'static str,
            attributes: Attributes<'a>,
        }
        #[derive(Serialize)]
        struct Attributes<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            name: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            status: Option<&'a str>,
        }

        let request = Request {
            data: Data {
                id: device_id,
                kind: "devices",
                attributes: Attributes { name, status },
            },
        };

        self.request_json(
            Method::PATCH,
            format!("{}/devices/{device_id}", asc_base_url()),
            &[],
            Some(&request),
        )
        .map(|response: JsonApiSingle<Device>| response.data)
    }

    pub fn list_certificates(&self) -> Result<Vec<Certificate>> {
        self.list_certificates_with_fields(
            "certificateType,displayName,serialNumber,expirationDate,activated",
        )
    }

    pub fn list_certificates_with_content(&self) -> Result<Vec<Certificate>> {
        self.list_certificates_with_fields(
            "certificateType,displayName,serialNumber,expirationDate,certificateContent,activated",
        )
    }

    fn list_certificates_with_fields(&self, fields: &str) -> Result<Vec<Certificate>> {
        self.get_paginated(
            format!("{}/certificates", asc_base_url()),
            vec![
                ("fields[certificates]".into(), fields.into()),
                ("limit".into(), "200".into()),
            ],
        )
    }

    pub fn create_certificate(
        &self,
        certificate_type: &str,
        csr_content: &str,
    ) -> Result<Certificate> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            data: Data<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Data<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            attributes: Attributes<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Attributes<'a> {
            certificate_type: &'a str,
            csr_content: &'a str,
        }

        let request = Request {
            data: Data {
                kind: "certificates",
                attributes: Attributes {
                    certificate_type,
                    csr_content,
                },
            },
        };

        self.request_json(
            Method::POST,
            format!("{}/certificates", asc_base_url()),
            &[],
            Some(&request),
        )
        .map(|response: JsonApiSingle<Certificate>| response.data)
    }

    pub fn revoke_certificate(&self, certificate_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            format!("{}/certificates/{certificate_id}", asc_base_url()),
            &[],
            None::<&Value>,
        )
    }

    pub fn list_profiles(&self) -> Result<Vec<Profile>> {
        self.get_paginated(
            format!("{}/profiles", asc_base_url()),
            vec![
                (
                    "fields[profiles]".into(),
                    "name,platform,profileType,profileState,uuid,expirationDate".into(),
                ),
                ("limit".into(), "200".into()),
            ],
        )
    }

    pub fn get_profile(&self, profile_id: &str) -> Result<Profile> {
        self.request_json(
            Method::GET,
            format!("{}/profiles/{profile_id}", asc_base_url()),
            &[(
                "fields[profiles]".into(),
                "name,platform,profileType,profileState,profileContent,uuid,expirationDate".into(),
            )],
            None::<&Value>,
        )
        .map(|response: JsonApiSingle<Profile>| response.data)
    }

    pub fn resolve_profile(&self, profile: Profile) -> Result<ProfileResolved> {
        let response: ProfileResponseWithIncluded = self.request_json(
            Method::GET,
            format!("{}/profiles/{}", asc_base_url(), profile.id),
            &[
                (
                    "fields[profiles]".into(),
                    "name,platform,profileType,profileState,profileContent,uuid,expirationDate"
                        .into(),
                ),
                (
                    "fields[bundleIds]".into(),
                    "identifier,name,platform".into(),
                ),
                (
                    "fields[certificates]".into(),
                    "certificateType,displayName,serialNumber,expirationDate,activated".into(),
                ),
                (
                    "fields[devices]".into(),
                    "name,platform,deviceClass,status,udid".into(),
                ),
                ("include".into(), "bundleId,certificates,devices".into()),
            ],
            None::<&Value>,
        )?;

        let mut bundle_id_id = None;
        let mut certificate_ids = BTreeSet::new();
        let mut device_ids = BTreeSet::new();
        for included in response.included {
            match included {
                ProfileIncluded::BundleId(bundle_id) => {
                    bundle_id_id = Some(bundle_id.id);
                }
                ProfileIncluded::Certificate(certificate) => {
                    certificate_ids.insert(certificate.id);
                }
                ProfileIncluded::Device(device) => {
                    device_ids.insert(device.id);
                }
            }
        }

        let bundle_id_id = bundle_id_id
            .ok_or_else(|| anyhow::anyhow!("profile {} is missing bundleId include", profile.id))?;

        Ok(ProfileResolved {
            profile: response.data,
            bundle_id_id,
            certificate_ids,
            device_ids,
        })
    }

    pub fn create_profile(
        &self,
        name: &str,
        profile_type: &str,
        bundle_id_id: &str,
        certificate_ids: &[String],
        device_ids: &[String],
    ) -> Result<Profile> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            data: Data<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Data<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            attributes: Attributes<'a>,
            relationships: Relationships<'a>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Attributes<'a> {
            name: &'a str,
            profile_type: &'a str,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Relationships<'a> {
            bundle_id: Relationship<'a>,
            certificates: RelationshipList<'a>,
            #[serde(skip_serializing_if = "RelationshipList::is_empty")]
            devices: RelationshipList<'a>,
        }
        #[derive(Serialize)]
        struct Relationship<'a> {
            data: RelationshipData<'a>,
        }
        #[derive(Serialize)]
        struct RelationshipList<'a> {
            data: Vec<RelationshipData<'a>>,
        }
        impl<'a> RelationshipList<'a> {
            fn is_empty(&self) -> bool {
                self.data.is_empty()
            }
        }
        #[derive(Serialize)]
        struct RelationshipData<'a> {
            id: &'a str,
            #[serde(rename = "type")]
            kind: &'static str,
        }

        let request = Request {
            data: Data {
                kind: "profiles",
                attributes: Attributes { name, profile_type },
                relationships: Relationships {
                    bundle_id: Relationship {
                        data: RelationshipData {
                            id: bundle_id_id,
                            kind: "bundleIds",
                        },
                    },
                    certificates: RelationshipList {
                        data: certificate_ids
                            .iter()
                            .map(|id| RelationshipData {
                                id: id.as_str(),
                                kind: "certificates",
                            })
                            .collect(),
                    },
                    devices: RelationshipList {
                        data: device_ids
                            .iter()
                            .map(|id| RelationshipData {
                                id: id.as_str(),
                                kind: "devices",
                            })
                            .collect(),
                    },
                },
            },
        };

        self.request_json(
            Method::POST,
            format!("{}/profiles", asc_base_url()),
            &[],
            Some(&request),
        )
        .map(|response: JsonApiSingle<Profile>| response.data)
    }

    pub fn delete_profile(&self, profile_id: &str) -> Result<()> {
        self.request_empty(
            Method::DELETE,
            format!("{}/profiles/{profile_id}", asc_base_url()),
            &[],
            None::<&Value>,
        )
    }

    pub fn normalize_capability(
        &self,
        capability_type: &str,
        settings: &[RemoteCapabilitySetting],
    ) -> BTreeMap<String, BTreeMap<String, bool>> {
        let _ = capability_type;
        settings
            .iter()
            .map(|setting| {
                (
                    setting.key.clone(),
                    setting
                        .options
                        .iter()
                        .map(|option| (option.key.clone(), option.enabled))
                        .collect(),
                )
            })
            .collect()
    }

    pub fn normalize_desired_capability(
        &self,
        capability: &DesiredCapability,
    ) -> BTreeMap<String, BTreeMap<String, bool>> {
        capability
            .settings
            .iter()
            .map(|setting| {
                (
                    setting.key.to_owned(),
                    setting
                        .options
                        .iter()
                        .map(|option| (option.key.to_owned(), option.enabled))
                        .collect(),
                )
            })
            .collect()
    }

    pub(crate) fn get_paginated<T>(
        &self,
        url: String,
        query: Vec<(String, String)>,
    ) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
    {
        let mut items = Vec::new();
        let mut next_url = Some(url);
        let mut first = true;

        while let Some(current_url) = next_url.take() {
            let response: JsonApiList<T> = if first {
                first = false;
                self.request_json(Method::GET, current_url, &query, None::<&Value>)?
            } else {
                self.request_json(Method::GET, current_url, &[], None::<&Value>)?
            };
            items.extend(response.data);
            next_url = response.links.next;
        }

        Ok(items)
    }

    pub(crate) fn request_json<T, B>(
        &self,
        method: Method,
        url: String,
        query: &[(String, String)],
        body: Option<&B>,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let max_attempts = if method == Method::GET || method == Method::DELETE {
            4
        } else {
            1
        };

        for attempt in 1..=max_attempts {
            let response = self.send(method.clone(), &url, query, body)?;
            match response.json::<T>() {
                Ok(value) => return Ok(value),
                Err(error)
                    if attempt < max_attempts && should_retry_response_decode(&method, &error) =>
                {
                    thread::sleep(Duration::from_secs(2u64.saturating_pow(attempt - 1)));
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to decode ASC response from {url}"));
                }
            }
        }

        unreachable!("request_json decode loop must return or error")
    }

    pub(crate) fn request_empty<B>(
        &self,
        method: Method,
        url: String,
        query: &[(String, String)],
        body: Option<&B>,
    ) -> Result<()>
    where
        B: Serialize + ?Sized,
    {
        self.send(method, &url, query, body).map(|_| ())
    }

    pub(crate) fn upload_asset_part(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<()> {
        let method = Method::from_bytes(method.as_bytes())
            .with_context(|| format!("invalid upload method {method}"))?;
        let mut request = self.http.request(method.clone(), url);
        for (name, value) in headers {
            request = request.header(name, value);
        }

        let response = request
            .body(body)
            .send()
            .with_context(|| format!("asset upload request failed: {method} {url}"))?;
        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let body = response.text().with_context(|| {
            format!("failed to read asset upload error response: {method} {url}")
        })?;
        bail!("asset upload {method} {url} returned {status}: {body}");
    }

    fn send<B>(
        &self,
        method: Method,
        url: &str,
        query: &[(String, String)],
        body: Option<&B>,
    ) -> Result<reqwest::blocking::Response>
    where
        B: Serialize + ?Sized,
    {
        const MAX_ATTEMPTS: u32 = 4;

        for attempt in 1..=MAX_ATTEMPTS {
            let token = self.auth.bearer_token()?;
            let mut request = self
                .http
                .request(method.clone(), url)
                .bearer_auth(token)
                .header("Accept", "application/json");
            if !query.is_empty() {
                request = request.query(query);
            }
            if let Some(body) = body {
                request = request
                    .header("Content-Type", "application/json")
                    .json(body);
            }

            let response = match request.send() {
                Ok(response) => response,
                Err(error)
                    if attempt < MAX_ATTEMPTS && should_retry_transport_error(&method, &error) =>
                {
                    thread::sleep(Duration::from_secs(2u64.saturating_pow(attempt - 1)));
                    continue;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("ASC request failed: {method} {url}"));
                }
            };

            if response.status().is_success() {
                return Ok(response);
            }

            let status = response.status();
            if attempt < MAX_ATTEMPTS && is_retryable_status(status) {
                let delay = retry_delay(&response, attempt);
                thread::sleep(delay);
                continue;
            }

            let body = response
                .text()
                .with_context(|| format!("failed to read ASC error response: {method} {url}"))?;
            if let Ok(parsed) = serde_json::from_str::<ErrorResponse>(&body) {
                let message = parsed
                    .errors
                    .into_iter()
                    .map(|error| {
                        format!(
                            "[{} {}] {} {}",
                            error.status.unwrap_or_else(|| "?".into()),
                            error.code.unwrap_or_else(|| "?".into()),
                            error.title.unwrap_or_else(|| "ASC error".into()),
                            error.detail.unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                bail!("ASC {method} {url} returned {status}: {message}");
            }

            bail!("ASC {method} {url} returned {status}: {body}");
        }

        unreachable!("send loop must return or bail")
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retry_delay(response: &reqwest::blocking::Response, attempt: u32) -> Duration {
    if let Some(value) = response.headers().get(RETRY_AFTER)
        && let Ok(value) = value.to_str()
        && let Ok(seconds) = value.trim().parse::<u64>()
    {
        return Duration::from_secs(seconds.max(1));
    }

    Duration::from_secs(2u64.saturating_pow(attempt - 1))
}

fn should_retry_transport_error(method: &Method, error: &reqwest::Error) -> bool {
    if *method != Method::GET && *method != Method::DELETE {
        return error.is_timeout() || error.is_connect();
    }

    true
}

fn should_retry_response_decode(method: &Method, error: &reqwest::Error) -> bool {
    if *method != Method::GET && *method != Method::DELETE {
        return false;
    }

    error.is_body() || error.is_decode() || error.is_timeout()
}
