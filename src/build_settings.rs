use std::collections::BTreeSet;

use crate::{
    scope::Scope,
    state::{ManagedProfile, State},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeBuildSettings {
    pub scope: Scope,
    pub profiles: Vec<ProfileBuildSettings>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileBuildSettings {
    pub logical_name: String,
    pub kind: String,
    pub team_id: String,
    pub bundle_id_ref: String,
    pub bundle_id: String,
    pub uuid: String,
    pub certs: Vec<String>,
    pub code_sign_identity: Option<String>,
}

pub fn collect_scope_build_settings(scope: Scope, state: &State) -> ScopeBuildSettings {
    let profiles = state
        .profiles
        .iter()
        .filter(|(_, profile)| managed_profile_scope(&profile.kind) == Some(scope))
        .map(|(logical_name, profile)| build_profile_settings(logical_name, profile, state))
        .collect();

    ScopeBuildSettings { scope, profiles }
}

fn build_profile_settings(
    logical_name: &str,
    profile: &ManagedProfile,
    state: &State,
) -> ProfileBuildSettings {
    let bundle_id = state
        .bundle_ids
        .get(&profile.bundle_id)
        .map(|bundle_id| bundle_id.bundle_id.clone())
        .unwrap_or_else(|| profile.bundle_id.clone());

    ProfileBuildSettings {
        logical_name: logical_name.to_owned(),
        kind: profile.kind.clone(),
        team_id: state.team_id.clone(),
        bundle_id_ref: profile.bundle_id.clone(),
        bundle_id,
        uuid: profile.uuid.clone(),
        certs: profile.certs.clone(),
        code_sign_identity: recommended_code_sign_identity(profile, state),
    }
}

fn recommended_code_sign_identity(profile: &ManagedProfile, state: &State) -> Option<String> {
    let identities = profile
        .certs
        .iter()
        .filter_map(|logical_name| state.certs.get(logical_name))
        .filter_map(|certificate| code_sign_identity_for_kind(&certificate.kind))
        .collect::<BTreeSet<_>>();

    if identities.is_empty() {
        None
    } else {
        Some(identities.into_iter().collect::<Vec<_>>().join(", "))
    }
}

fn code_sign_identity_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "DEVELOPMENT" => Some("Apple Development"),
        "DISTRIBUTION" => Some("Apple Distribution"),
        "DEVELOPER_ID_APPLICATION_G2" => Some("Developer ID Application"),
        "DEVELOPER_ID_INSTALLER" => Some("Developer ID Installer"),
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
    use std::collections::BTreeMap;

    use crate::state::{ManagedBundleId, ManagedCertificate, ManagedProfile, State};

    use super::{code_sign_identity_for_kind, collect_scope_build_settings};

    #[test]
    fn maps_certificate_kind_to_code_sign_identity() {
        assert_eq!(
            code_sign_identity_for_kind("DEVELOPMENT"),
            Some("Apple Development")
        );
        assert_eq!(
            code_sign_identity_for_kind("DISTRIBUTION"),
            Some("Apple Distribution")
        );
        assert_eq!(
            code_sign_identity_for_kind("DEVELOPER_ID_APPLICATION_G2"),
            Some("Developer ID Application")
        );
        assert_eq!(
            code_sign_identity_for_kind("DEVELOPER_ID_INSTALLER"),
            Some("Developer ID Installer")
        );
        assert_eq!(code_sign_identity_for_kind("UNKNOWN"), None);
    }

    #[test]
    fn collects_profile_build_settings_from_state() {
        let mut state = State::new("TEAM123");
        state.bundle_ids.insert(
            "main".into(),
            ManagedBundleId {
                apple_id: "bundle-1".into(),
                bundle_id: "com.example.app".into(),
            },
        );
        state.certs.insert(
            "dist-a".into(),
            ManagedCertificate {
                apple_id: Some("cert-1".into()),
                kind: "DISTRIBUTION".into(),
                name: "Dist A".into(),
                serial_number: "001".into(),
                p12_password: "secret".into(),
            },
        );
        state.certs.insert(
            "dist-b".into(),
            ManagedCertificate {
                apple_id: Some("cert-2".into()),
                kind: "DISTRIBUTION".into(),
                name: "Dist B".into(),
                serial_number: "002".into(),
                p12_password: "secret".into(),
            },
        );
        state.profiles = BTreeMap::from([
            (
                "ios-app-store".into(),
                ManagedProfile {
                    apple_id: "profile-1".into(),
                    name: "Acme App Store".into(),
                    kind: "IOS_APP_STORE".into(),
                    bundle_id: "main".into(),
                    certs: vec!["dist-a".into(), "dist-b".into()],
                    devices: Vec::new(),
                    uuid: "UUID-123".into(),
                },
            ),
            (
                "ios-development".into(),
                ManagedProfile {
                    apple_id: "profile-2".into(),
                    name: "Acme Development".into(),
                    kind: "IOS_APP_DEVELOPMENT".into(),
                    bundle_id: "main".into(),
                    certs: vec!["dist-a".into()],
                    devices: vec!["iphone".into()],
                    uuid: "UUID-456".into(),
                },
            ),
        ]);

        let report = collect_scope_build_settings(crate::scope::Scope::Release, &state);
        assert_eq!(report.profiles.len(), 1);
        let profile = &report.profiles[0];

        assert_eq!(report.scope, crate::scope::Scope::Release);
        assert_eq!(profile.logical_name, "ios-app-store");
        assert_eq!(profile.bundle_id_ref, "main");
        assert_eq!(profile.bundle_id, "com.example.app");
        assert_eq!(profile.team_id, "TEAM123");
        assert_eq!(profile.uuid, "UUID-123");
        assert_eq!(
            profile.code_sign_identity.as_deref(),
            Some("Apple Distribution")
        );
    }
}
