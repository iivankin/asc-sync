use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use anyhow::{Context, Result, bail, ensure};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    asc::{AscClient, asc_endpoint},
    auth_store,
    cli::{MetadataKeywordsAuditArgs, MetadataOutputArg},
    config_io,
};

const KEYWORD_LIMIT_CHARS: usize = 100;
const UNDERFILLED_REMAINING_CHARS: usize = 25;

pub fn run_keywords_audit(args: &MetadataKeywordsAuditArgs) -> Result<()> {
    let version_selector = resolve_version_selector(args)?;
    let platform = normalize_platform(args.platform.as_deref())?;
    let team_id = resolve_team_id(args)?;
    let auth = auth_store::resolve_auth_context_for_optional_team_id(team_id.as_deref())?;
    let client = AscClient::new(auth)?;
    let api = MetadataApi::new(&client);
    let blocked_terms = load_blocked_terms(args)?;

    let version = api.resolve_version(&args.app, version_selector, platform.as_deref())?;
    let app_info_id = api.resolve_app_info_id(
        &args.app,
        args.app_info.as_deref(),
        &version.version_string,
        version.platform.as_deref(),
        version.state.as_deref(),
    )?;
    let version_localizations = api.list_version_localizations(&version.id)?;
    let app_info_localizations = api.list_app_info_localizations(&app_info_id)?;
    let report = audit_keywords(
        KeywordAuditInput {
            app_id: args.app.clone(),
            version_id: version.id,
            version_string: version.version_string,
            platform: version.platform.unwrap_or_default(),
            blocked_terms,
            version_localizations,
            app_info_localizations,
        },
        args.strict,
    );

    print_report(&report, args.output, args.pretty)?;
    if report.summary.blocking > 0 {
        bail!(
            "metadata keywords audit: found {} blocking issue(s)",
            report.summary.blocking
        );
    }
    Ok(())
}

fn resolve_version_selector(args: &MetadataKeywordsAuditArgs) -> Result<VersionSelector> {
    let version = args
        .version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty());
    let version_id = args
        .version_id
        .as_deref()
        .map(str::trim)
        .filter(|version_id| !version_id.is_empty());

    match (version, version_id) {
        (Some(_), Some(_)) => bail!("--version and --version-id are mutually exclusive"),
        (Some(version), None) => Ok(VersionSelector::VersionString(version.to_owned())),
        (None, Some(version_id)) => Ok(VersionSelector::VersionId(version_id.to_owned())),
        (None, None) => bail!("--version or --version-id is required"),
    }
}

fn resolve_team_id(args: &MetadataKeywordsAuditArgs) -> Result<Option<String>> {
    let explicit = args
        .team_id
        .as_deref()
        .map(str::trim)
        .filter(|team_id| !team_id.is_empty())
        .map(str::to_owned);
    let config_team = args
        .config
        .as_ref()
        .map(|path| {
            config_io::load_config(path)
                .map(|config| config.team_id)
                .with_context(|| format!("failed to load team_id from {}", path.display()))
        })
        .transpose()?;

    if let (Some(explicit), Some(config_team)) = (&explicit, &config_team) {
        ensure!(
            explicit == config_team,
            "--team-id {explicit} does not match team_id {config_team} from --config"
        );
    }

    Ok(explicit.or(config_team))
}

fn normalize_platform(platform: Option<&str>) -> Result<Option<String>> {
    let Some(platform) = platform
        .map(str::trim)
        .filter(|platform| !platform.is_empty())
    else {
        return Ok(None);
    };
    let normalized = platform.replace('-', "_").to_ascii_uppercase();
    let normalized = match normalized.as_str() {
        "IOS" => "IOS",
        "MACOS" | "MAC_OS" => "MAC_OS",
        "TVOS" | "TV_OS" => "TV_OS",
        "VISIONOS" | "VISION_OS" => "VISION_OS",
        _ => bail!("invalid --platform {platform}; expected IOS, MAC_OS, TV_OS, or VISION_OS"),
    };
    Ok(Some(normalized.to_owned()))
}

fn load_blocked_terms(args: &MetadataKeywordsAuditArgs) -> Result<Vec<String>> {
    let mut terms = Vec::new();
    for term in &args.blocked_terms {
        let trimmed = collapse_whitespace(term);
        ensure!(!trimmed.is_empty(), "--blocked-term must not be empty");
        terms.push(trimmed);
    }

    if let Some(path) = &args.blocked_terms_file {
        let data = fs::read_to_string(path)
            .with_context(|| format!("failed to read blocked terms file {}", path.display()))?;
        for line in data.replace("\r\n", "\n").lines() {
            let trimmed_line = line.trim();
            if trimmed_line.is_empty() || trimmed_line.starts_with('#') {
                continue;
            }
            terms.extend(
                trimmed_line
                    .split(',')
                    .map(collapse_whitespace)
                    .filter(|term| !term.is_empty()),
            );
        }

        ensure!(
            !terms.is_empty(),
            "--blocked-terms-file must include at least one blocked term"
        );
    }

    Ok(terms)
}

enum VersionSelector {
    VersionString(String),
    VersionId(String),
}

struct ResolvedVersion {
    id: String,
    version_string: String,
    platform: Option<String>,
    state: Option<String>,
}

struct MetadataApi<'a> {
    client: &'a AscClient,
}

impl<'a> MetadataApi<'a> {
    fn new(client: &'a AscClient) -> Self {
        Self { client }
    }

    fn resolve_version(
        &self,
        app_id: &str,
        selector: VersionSelector,
        platform: Option<&str>,
    ) -> Result<ResolvedVersion> {
        match selector {
            VersionSelector::VersionId(version_id) => {
                let version = self.get_version(&version_id)?;
                let related_app_id = version
                    .relationships
                    .as_ref()
                    .and_then(|relationships| relationships.app.as_ref())
                    .map(|relationship| relationship.data.id.trim())
                    .filter(|related_app_id| !related_app_id.is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "app relationship missing for app store version {version_id}"
                        )
                    })?;
                ensure!(
                    related_app_id.eq_ignore_ascii_case(app_id.trim()),
                    "version {version_id} belongs to app {related_app_id}, not {app_id}"
                );
                if let Some(platform) = platform
                    && !version.attributes.platform.eq_ignore_ascii_case(platform)
                {
                    bail!(
                        "version {version_id} is on platform {}, not {platform}",
                        version.attributes.platform
                    );
                }
                Ok(version.into_resolved())
            }
            VersionSelector::VersionString(version_string) => {
                let versions = self.find_versions(app_id, &version_string, platform)?;
                ensure!(
                    !versions.is_empty(),
                    "app store version not found for version {version_string}"
                );
                ensure!(
                    versions.len() == 1,
                    "--platform is required when multiple app store versions match --version {version_string}"
                );
                Ok(versions
                    .into_iter()
                    .next()
                    .expect("one version")
                    .into_resolved())
            }
        }
    }

    fn get_version(&self, version_id: &str) -> Result<MetadataAppStoreVersion> {
        let response: JsonApiSingle<MetadataAppStoreVersion> = self.client.request_json(
            Method::GET,
            asc_endpoint(&format!("/appStoreVersions/{version_id}")),
            &[
                (
                    "fields[appStoreVersions]".into(),
                    "platform,versionString,appStoreState,appVersionState,app".into(),
                ),
                ("include".into(), "app".into()),
            ],
            None::<&Value>,
        )?;
        Ok(response.data)
    }

    fn find_versions(
        &self,
        app_id: &str,
        version_string: &str,
        platform: Option<&str>,
    ) -> Result<Vec<MetadataAppStoreVersion>> {
        let mut query = vec![
            ("filter[versionString]".into(), version_string.into()),
            (
                "fields[appStoreVersions]".into(),
                "platform,versionString,appStoreState,appVersionState".into(),
            ),
            ("limit".into(), "200".into()),
        ];
        if let Some(platform) = platform {
            query.push(("filter[platform]".into(), platform.into()));
        }

        self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/appStoreVersions")),
            query,
        )
    }

    fn resolve_app_info_id(
        &self,
        app_id: &str,
        app_info_id: Option<&str>,
        version: &str,
        platform: Option<&str>,
        version_state: Option<&str>,
    ) -> Result<String> {
        if let Some(app_info_id) = app_info_id.map(str::trim).filter(|value| !value.is_empty()) {
            return Ok(app_info_id.to_owned());
        }

        let app_infos = self.list_app_infos(app_id)?;
        ensure!(!app_infos.is_empty(), "no app info found for app {app_id}");
        if app_infos.len() == 1 {
            return Ok(app_infos[0].id.clone());
        }

        if let Some(app_info_id) = auto_resolve_app_info_id_by_version_state(
            app_infos.iter().map(AppInfoCandidate::from).collect(),
            version_state.unwrap_or_default(),
        ) {
            return Ok(app_info_id);
        }

        let candidates = format_app_info_candidates(&app_infos);
        let platform_arg = platform
            .map(|platform| format!(" --platform {platform}"))
            .unwrap_or_default();
        bail!(
            "multiple app infos found for app {app_id} ({candidates}). Run `asc apps info list --app {app_id}` to inspect candidates, then re-run with --app-info. Example: asc metadata keywords audit --app \"{app_id}\" --version \"{version}\"{platform_arg} --app-info \"{}\"",
            app_infos[0].id
        );
    }

    fn list_app_infos(&self, app_id: &str) -> Result<Vec<MetadataAppInfo>> {
        self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/appInfos")),
            vec![
                (
                    "fields[appInfos]".into(),
                    "state,appStoreState,appInfoLocalizations".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn list_app_info_localizations(&self, app_info_id: &str) -> Result<Vec<AppInfoLocalization>> {
        let remote: Vec<MetadataAppInfoLocalization> = self.client.get_paginated(
            asc_endpoint(&format!("/appInfos/{app_info_id}/appInfoLocalizations")),
            vec![
                (
                    "fields[appInfoLocalizations]".into(),
                    "locale,name,subtitle".into(),
                ),
                ("limit".into(), "200".into()),
            ],
        )?;
        Ok(remote.into_iter().map(Into::into).collect())
    }

    fn list_version_localizations(&self, version_id: &str) -> Result<Vec<VersionLocalization>> {
        let remote: Vec<MetadataAppStoreVersionLocalization> = self.client.get_paginated(
            asc_endpoint(&format!(
                "/appStoreVersions/{version_id}/appStoreVersionLocalizations"
            )),
            vec![
                (
                    "fields[appStoreVersionLocalizations]".into(),
                    "locale,keywords".into(),
                ),
                ("limit".into(), "200".into()),
            ],
        )?;
        Ok(remote.into_iter().map(Into::into).collect())
    }
}

#[derive(Debug, Deserialize)]
struct JsonApiSingle<T> {
    data: T,
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataAppInfo {
    id: String,
    #[serde(default)]
    attributes: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataAppInfoLocalization {
    id: String,
    attributes: MetadataAppInfoLocalizationAttributes,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataAppInfoLocalizationAttributes {
    locale: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    subtitle: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataAppStoreVersion {
    id: String,
    attributes: MetadataAppStoreVersionAttributes,
    #[serde(default)]
    relationships: Option<MetadataRelationships>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataAppStoreVersionAttributes {
    #[serde(default)]
    platform: String,
    #[serde(default)]
    version_string: String,
    #[serde(default)]
    app_store_state: Option<String>,
    #[serde(default)]
    app_version_state: Option<String>,
}

impl MetadataAppStoreVersion {
    fn into_resolved(self) -> ResolvedVersion {
        ResolvedVersion {
            id: self.id,
            version_string: self.attributes.version_string,
            platform: Some(self.attributes.platform).filter(|platform| !platform.is_empty()),
            state: self
                .attributes
                .app_version_state
                .filter(|state| !state.trim().is_empty())
                .or(self.attributes.app_store_state),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataRelationships {
    #[serde(default)]
    app: Option<MetadataRelationship>,
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataRelationship {
    data: MetadataRelationshipData,
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataRelationshipData {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MetadataAppStoreVersionLocalization {
    id: String,
    attributes: MetadataAppStoreVersionLocalizationAttributes,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataAppStoreVersionLocalizationAttributes {
    locale: String,
    #[serde(default)]
    keywords: String,
}

#[derive(Debug, Clone)]
struct AppInfoCandidate {
    id: String,
    state: String,
}

impl From<&MetadataAppInfo> for AppInfoCandidate {
    fn from(info: &MetadataAppInfo) -> Self {
        Self {
            id: info.id.trim().to_owned(),
            state: app_info_state(&info.attributes),
        }
    }
}

fn app_info_state(attributes: &BTreeMap<String, Value>) -> String {
    ["state", "appStoreState"]
        .into_iter()
        .find_map(|key| attributes.get(key).and_then(Value::as_str).map(str::trim))
        .filter(|state| !state.is_empty())
        .unwrap_or_default()
        .to_owned()
}

fn auto_resolve_app_info_id_by_version_state(
    mut candidates: Vec<AppInfoCandidate>,
    version_state: &str,
) -> Option<String> {
    let acceptable = acceptable_app_info_states_for_version_state(version_state);
    if acceptable.is_empty() {
        return None;
    }
    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    let matches = candidates
        .into_iter()
        .filter(|candidate| {
            !candidate.id.is_empty()
                && acceptable
                    .iter()
                    .any(|state| candidate.state.eq_ignore_ascii_case(state))
        })
        .map(|candidate| candidate.id)
        .collect::<Vec<_>>();
    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

fn acceptable_app_info_states_for_version_state(version_state: &str) -> Vec<String> {
    match version_state.trim() {
        "" => Vec::new(),
        "PENDING_DEVELOPER_RELEASE" => vec!["PENDING_DEVELOPER_RELEASE", "PENDING_RELEASE"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "PENDING_APPLE_RELEASE" => vec!["PENDING_APPLE_RELEASE", "PENDING_RELEASE"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "REPLACED_WITH_NEW_VERSION" => vec!["REPLACED_WITH_NEW_VERSION", "REPLACED_WITH_NEW_INFO"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "READY_FOR_SALE" => vec!["READY_FOR_SALE", "READY_FOR_DISTRIBUTION"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        "PREORDER_READY_FOR_SALE" => vec!["PREORDER_READY_FOR_SALE", "READY_FOR_DISTRIBUTION"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        other => vec![other.to_owned()],
    }
}

fn format_app_info_candidates(app_infos: &[MetadataAppInfo]) -> String {
    if app_infos.is_empty() {
        return "none".into();
    }
    let mut candidates = app_infos
        .iter()
        .map(|info| {
            let state = app_info_state(&info.attributes);
            format!(
                "{}[state={}]",
                info.id,
                if state.is_empty() { "unknown" } else { &state }
            )
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.join(", ")
}

#[derive(Debug)]
struct KeywordAuditInput {
    app_id: String,
    version_id: String,
    version_string: String,
    platform: String,
    blocked_terms: Vec<String>,
    version_localizations: Vec<VersionLocalization>,
    app_info_localizations: Vec<AppInfoLocalization>,
}

#[derive(Debug, Clone)]
struct VersionLocalization {
    id: String,
    locale: String,
    keywords: String,
}

impl From<MetadataAppStoreVersionLocalization> for VersionLocalization {
    fn from(localization: MetadataAppStoreVersionLocalization) -> Self {
        Self {
            id: localization.id,
            locale: localization.attributes.locale,
            keywords: localization.attributes.keywords,
        }
    }
}

#[derive(Debug, Clone)]
struct AppInfoLocalization {
    id: String,
    locale: String,
    name: String,
    subtitle: String,
}

impl From<MetadataAppInfoLocalization> for AppInfoLocalization {
    fn from(localization: MetadataAppInfoLocalization) -> Self {
        Self {
            id: localization.id,
            locale: localization.attributes.locale,
            name: localization.attributes.name,
            subtitle: localization.attributes.subtitle,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct KeywordAuditReport {
    app_id: String,
    version_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    version_string: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    platform: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    blocked_terms: Vec<String>,
    summary: AuditSummary,
    locales: Vec<KeywordAuditLocale>,
    checks: Vec<KeywordAuditCheck>,
    #[serde(skip_serializing_if = "bool_is_false")]
    strict: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
struct AuditSummary {
    errors: usize,
    warnings: usize,
    infos: usize,
    blocking: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct KeywordAuditLocale {
    locale: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    version_localization_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    app_info_localization_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    keyword_field: String,
    keyword_count: usize,
    #[serde(rename = "usedBytes")]
    used_characters: usize,
    #[serde(rename = "remainingBytes")]
    remaining_characters: usize,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    subtitle: String,
    errors: usize,
    warnings: usize,
    infos: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct KeywordAuditCheck {
    id: String,
    severity: Severity,
    message: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    remediation: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    locale: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    field: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    keyword: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    matched_term: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    related_locales: Vec<String>,
    #[serde(rename = "usedBytes", skip_serializing_if = "usize_is_zero")]
    used_characters: usize,
    #[serde(rename = "remainingBytes", skip_serializing_if = "usize_is_zero")]
    remaining_characters: usize,
}

impl KeywordAuditCheck {
    fn new(id: &str, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            id: id.to_owned(),
            severity,
            message: message.into(),
            remediation: String::new(),
            locale: String::new(),
            field: String::new(),
            keyword: String::new(),
            matched_term: String::new(),
            related_locales: Vec::new(),
            used_characters: 0,
            remaining_characters: 0,
        }
    }

    fn remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = remediation.into();
        self
    }

    fn locale(mut self, locale: &str) -> Self {
        self.locale = locale.to_owned();
        self
    }

    fn field(mut self, field: &str) -> Self {
        self.field = field.to_owned();
        self
    }

    fn keyword(mut self, keyword: impl Into<String>) -> Self {
        self.keyword = keyword.into();
        self
    }

    fn matched_term(mut self, matched_term: impl Into<String>) -> Self {
        self.matched_term = matched_term.into();
        self
    }

    fn related_locales(mut self, related_locales: Vec<String>) -> Self {
        self.related_locales = related_locales;
        self
    }

    fn character_usage(mut self, used_characters: usize, remaining_characters: usize) -> Self {
        self.used_characters = used_characters;
        self.remaining_characters = remaining_characters;
        self
    }
}

fn usize_is_zero(value: &usize) -> bool {
    *value == 0
}

fn bool_is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

type CrossLocaleKeywords = BTreeMap<String, CrossLocaleEntry>;

#[derive(Debug, Default)]
struct CrossLocaleEntry {
    locales: BTreeSet<String>,
    phrases: BTreeSet<String>,
}

#[derive(Debug)]
struct KeywordFieldScan {
    tokens: Vec<String>,
    empty_segments: bool,
    noncanonical_separators: bool,
}

fn audit_keywords(input: KeywordAuditInput, strict: bool) -> KeywordAuditReport {
    let blocked_terms = normalize_blocked_terms(input.blocked_terms);
    let app_info_by_locale = input
        .app_info_localizations
        .into_iter()
        .filter(|localization| !localization.locale.trim().is_empty())
        .map(|localization| (localization.locale.trim().to_owned(), localization))
        .collect::<BTreeMap<_, _>>();

    let mut locale_summaries = BTreeMap::new();
    let mut checks = Vec::new();
    let mut cross_locale_keywords = CrossLocaleKeywords::new();

    for localization in input.version_localizations {
        let locale = localization.locale.trim();
        if locale.is_empty() {
            continue;
        }

        let scan = scan_keyword_field(&localization.keywords);
        let (normalized, duplicates) = normalize_keyword_tokens(&scan.tokens);
        let used_characters = keyword_field_length(&localization.keywords);
        let remaining_characters = KEYWORD_LIMIT_CHARS.saturating_sub(used_characters);
        let app_info = app_info_by_locale.get(locale);

        locale_summaries.insert(
            locale.to_owned(),
            KeywordAuditLocale {
                locale: locale.to_owned(),
                version_localization_id: localization.id.trim().to_owned(),
                app_info_localization_id: app_info
                    .map(|app_info| app_info.id.trim().to_owned())
                    .unwrap_or_default(),
                keyword_field: localization.keywords.clone(),
                keyword_count: normalized.len(),
                used_characters,
                remaining_characters,
                name: app_info
                    .map(|app_info| app_info.name.clone())
                    .unwrap_or_default(),
                subtitle: app_info
                    .map(|app_info| app_info.subtitle.clone())
                    .unwrap_or_default(),
                errors: 0,
                warnings: 0,
                infos: 0,
            },
        );

        if used_characters > KEYWORD_LIMIT_CHARS {
            checks.push(
                KeywordAuditCheck::new(
                    "metadata.keywords.length",
                    Severity::Error,
                    format!("keywords exceed {KEYWORD_LIMIT_CHARS} characters"),
                )
                .locale(locale)
                .field("keywords")
                .remediation(format!(
                    "Shorten keywords to {KEYWORD_LIMIT_CHARS} characters or fewer"
                ))
                .character_usage(used_characters, 0),
            );
        }

        if scan.empty_segments {
            checks.push(
                KeywordAuditCheck::new(
                    "metadata.keywords.empty_segments",
                    Severity::Warning,
                    "keyword field contains empty phrase segments",
                )
                .locale(locale)
                .field("keywords")
                .remediation(
                    "Remove repeated, leading, or trailing separators from the keyword field",
                ),
            );
        }

        if scan.noncanonical_separators {
            checks.push(
                KeywordAuditCheck::new(
                    "metadata.keywords.noncanonical_separators",
                    Severity::Warning,
                    "keyword field uses non-canonical separators",
                )
                .locale(locale)
                .field("keywords")
                .remediation("Use a comma-separated keyword field"),
            );
        }

        if !duplicates.is_empty() {
            checks.push(
                KeywordAuditCheck::new(
                    "metadata.keywords.locale_duplicates",
                    Severity::Warning,
                    format!(
                        "keywords repeat {} phrase(s) within the locale",
                        duplicates.len()
                    ),
                )
                .locale(locale)
                .field("keywords")
                .keyword(duplicates.join(", "))
                .remediation("Remove duplicated phrases from the keyword field"),
            );
        }

        if remaining_characters >= UNDERFILLED_REMAINING_CHARS {
            checks.push(
                KeywordAuditCheck::new(
                    "metadata.keywords.underfilled",
                    Severity::Info,
                    format!("keyword field leaves {remaining_characters} characters unused"),
                )
                .locale(locale)
                .field("keywords")
                .remediation(
                    "Consider using more of the keyword budget if the missing space is intentional and safe",
                )
                .character_usage(used_characters, remaining_characters),
            );
        }

        if let Some(app_info) = app_info {
            let name_text = normalize_keyword_text(&app_info.name);
            let subtitle_text = normalize_keyword_text(&app_info.subtitle);
            for phrase in &normalized {
                let phrase_text = normalize_keyword_text(phrase);
                if phrase_text.is_empty() {
                    continue;
                }
                if contains_normalized_phrase(&name_text, &phrase_text) {
                    checks.push(
                        KeywordAuditCheck::new(
                            "metadata.keywords.overlap_name",
                            Severity::Warning,
                            format!("keyword phrase {phrase:?} overlaps the localized app name"),
                        )
                        .locale(locale)
                        .field("name")
                        .keyword(phrase.clone())
                        .remediation("Avoid repeating name terms inside the keyword field"),
                    );
                }
                if contains_normalized_phrase(&subtitle_text, &phrase_text) {
                    checks.push(
                        KeywordAuditCheck::new(
                            "metadata.keywords.overlap_subtitle",
                            Severity::Warning,
                            format!("keyword phrase {phrase:?} overlaps the localized subtitle"),
                        )
                        .locale(locale)
                        .field("subtitle")
                        .keyword(phrase.clone())
                        .remediation("Avoid repeating subtitle terms inside the keyword field"),
                    );
                }
            }
        }

        for phrase in &normalized {
            let phrase_text = normalize_keyword_text(phrase);
            if phrase_text.is_empty() {
                continue;
            }
            let entry = cross_locale_keywords
                .entry(phrase_text.clone())
                .or_default();
            entry.locales.insert(locale.to_owned());
            entry.phrases.insert(phrase.clone());

            for term in &blocked_terms {
                let term_text = normalize_keyword_text(term);
                if term_text.is_empty() || !contains_normalized_phrase(&phrase_text, &term_text) {
                    continue;
                }
                checks.push(
                    KeywordAuditCheck::new(
                        "metadata.keywords.blocked_term",
                        Severity::Warning,
                        format!("keyword phrase {phrase:?} matches blocked term {term:?}"),
                    )
                    .locale(locale)
                    .field("keywords")
                    .keyword(phrase.clone())
                    .matched_term(term.clone())
                    .remediation("Remove or replace the blocked phrase"),
                );
            }
        }
    }

    if locale_summaries.is_empty() {
        checks.push(
            KeywordAuditCheck::new(
                "metadata.keywords.localizations_missing",
                Severity::Error,
                "no version localizations were available for keyword audit",
            )
            .field("keywords")
            .remediation(
                "Create or fetch at least one version localization before auditing keywords",
            ),
        );
    }

    for entry in cross_locale_keywords.into_values() {
        if entry.locales.len() < 2 {
            continue;
        }
        let related_locales = entry.locales.into_iter().collect::<Vec<_>>();
        let display_phrase = entry
            .phrases
            .into_iter()
            .min_by(|left, right| {
                let lower_cmp = left.to_lowercase().cmp(&right.to_lowercase());
                lower_cmp.then_with(|| left.cmp(right))
            })
            .unwrap_or_default();
        checks.push(
            KeywordAuditCheck::new(
                "metadata.keywords.cross_locale_duplicates",
                Severity::Info,
                format!("keyword phrase {display_phrase:?} appears in multiple locales"),
            )
            .field("keywords")
            .keyword(display_phrase)
            .related_locales(related_locales)
            .remediation("Confirm the repeated phrase is intentional across the listed locales"),
        );
    }

    checks.sort_by(|left, right| {
        left.locale
            .cmp(&right.locale)
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.keyword.cmp(&right.keyword))
            .then_with(|| left.message.cmp(&right.message))
    });

    for check in &checks {
        increment_locale_severity(locale_summaries.get_mut(&check.locale), check.severity);
        for locale in &check.related_locales {
            if locale == &check.locale {
                continue;
            }
            increment_locale_severity(locale_summaries.get_mut(locale), check.severity);
        }
    }

    KeywordAuditReport {
        app_id: input.app_id.trim().to_owned(),
        version_id: input.version_id.trim().to_owned(),
        version_string: input.version_string.trim().to_owned(),
        platform: input.platform.trim().to_owned(),
        blocked_terms,
        summary: summarize_keyword_audit(&checks, strict),
        locales: locale_summaries.into_values().collect(),
        checks,
        strict,
    }
}

fn summarize_keyword_audit(checks: &[KeywordAuditCheck], strict: bool) -> AuditSummary {
    let mut summary = AuditSummary::default();
    for check in checks {
        match check.severity {
            Severity::Error => summary.errors += 1,
            Severity::Warning => summary.warnings += 1,
            Severity::Info => summary.infos += 1,
        }
    }
    summary.blocking = summary.errors;
    if strict {
        summary.blocking += summary.warnings;
    }
    summary
}

fn increment_locale_severity(locale: Option<&mut KeywordAuditLocale>, severity: Severity) {
    let Some(locale) = locale else {
        return;
    };
    match severity {
        Severity::Error => locale.errors += 1,
        Severity::Warning => locale.warnings += 1,
        Severity::Info => locale.infos += 1,
    }
}

fn normalize_blocked_terms(terms: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    let mut seen = BTreeSet::new();
    for term in terms {
        let trimmed = collapse_whitespace(&term);
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_lowercase()) {
            normalized.push(trimmed);
        }
    }
    normalized.sort_by_key(|term| term.to_lowercase());
    normalized
}

fn scan_keyword_field(value: &str) -> KeywordFieldScan {
    let mut scan = KeywordFieldScan {
        tokens: Vec::new(),
        empty_segments: false,
        noncanonical_separators: false,
    };
    let mut current = String::new();

    for character in value.chars() {
        match character {
            ',' => flush_keyword_segment(&mut scan, &mut current, Some(',')),
            '，' | '、' | ';' | '；' | '\n' | '\r' => {
                scan.noncanonical_separators = true;
                flush_keyword_segment(&mut scan, &mut current, Some(character));
            }
            _ => current.push(character),
        }
    }

    if !current.is_empty() {
        flush_keyword_segment(&mut scan, &mut current, None);
    } else if value.ends_with(',') {
        scan.empty_segments = true;
    }

    scan
}

fn flush_keyword_segment(
    scan: &mut KeywordFieldScan,
    current: &mut String,
    separator: Option<char>,
) {
    let token = collapse_whitespace(current);
    if token.is_empty() {
        if !scan.tokens.is_empty() || !current.is_empty() || separator == Some(',') {
            scan.empty_segments = true;
        }
    } else {
        scan.tokens.push(token);
    }
    current.clear();
}

fn normalize_keyword_tokens(tokens: &[String]) -> (Vec<String>, Vec<String>) {
    let mut normalized = Vec::new();
    let mut duplicates = Vec::new();
    let mut seen = BTreeSet::new();
    for token in tokens {
        let trimmed = collapse_whitespace(token);
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_lowercase()) {
            normalized.push(trimmed);
        } else {
            duplicates.push(trimmed);
        }
    }
    (normalized, duplicates)
}

fn normalize_keyword_text(value: &str) -> String {
    let mut result = String::new();
    let mut last_space = false;
    for character in value.trim().chars() {
        if character.is_alphanumeric() {
            result.extend(character.to_lowercase());
            last_space = false;
        } else if !last_space && !result.is_empty() {
            result.push(' ');
            last_space = true;
        }
    }
    result.trim().to_owned()
}

fn contains_normalized_phrase(haystack: &str, needle: &str) -> bool {
    let haystack = haystack.trim();
    let needle = needle.trim();
    if haystack.is_empty() || needle.is_empty() {
        return false;
    }
    format!(" {haystack} ").contains(&format!(" {needle} "))
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn keyword_field_length(value: &str) -> usize {
    value.chars().count()
}

fn print_report(
    report: &KeywordAuditReport,
    output: MetadataOutputArg,
    pretty: bool,
) -> Result<()> {
    match output {
        MetadataOutputArg::Json => {
            let value = if pretty {
                serde_json::to_string_pretty(report)
            } else {
                serde_json::to_string(report)
            }
            .context("failed to serialize keyword audit report")?;
            println!("{value}");
        }
        MetadataOutputArg::Table => print_report_table(report),
        MetadataOutputArg::Markdown => print_report_markdown(report),
    }
    Ok(())
}

fn print_report_table(report: &KeywordAuditReport) {
    println!(
        "app\tversion\tversion id\tplatform\tstrict\tlocales\terrors\twarnings\tinfos\tblocking\tblocked terms"
    );
    println!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        report.app_id,
        report.version_string,
        report.version_id,
        report.platform,
        report.strict,
        report.locales.len(),
        report.summary.errors,
        report.summary.warnings,
        report.summary.infos,
        report.summary.blocking,
        report.blocked_terms.join(",")
    );

    println!();
    println!("locale\tcount\tused chars\tremaining\terrors\twarnings\tinfos\tkeywords");
    if report.locales.is_empty() {
        println!("\t0\t0\t0\t0\t0\t0\t");
    } else {
        for locale in &report.locales {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                locale.locale,
                locale.keyword_count,
                locale.used_characters,
                locale.remaining_characters,
                locale.errors,
                locale.warnings,
                locale.infos,
                sanitize_cell(&locale.keyword_field)
            );
        }
    }

    println!();
    println!("severity\tlocale\trelated locales\tid\tkeyword\tterm\tmessage");
    if report.checks.is_empty() {
        println!("\t\t\t\t\t\tno findings");
    } else {
        for check in &report.checks {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                check.severity.as_str(),
                check.locale,
                check.related_locales.join(","),
                check.id,
                sanitize_cell(&check.keyword),
                sanitize_cell(&check.matched_term),
                sanitize_cell(&check.message)
            );
        }
    }
}

fn print_report_markdown(report: &KeywordAuditReport) {
    println!(
        "| app | version | version id | platform | strict | locales | errors | warnings | infos | blocking | blocked terms |"
    );
    println!("| --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- |");
    println!(
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        markdown_cell(&report.app_id),
        markdown_cell(&report.version_string),
        markdown_cell(&report.version_id),
        markdown_cell(&report.platform),
        report.strict,
        report.locales.len(),
        report.summary.errors,
        report.summary.warnings,
        report.summary.infos,
        report.summary.blocking,
        markdown_cell(&report.blocked_terms.join(","))
    );

    println!();
    println!("| locale | count | used chars | remaining | errors | warnings | infos | keywords |");
    println!("| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |");
    if report.locales.is_empty() {
        println!("|  | 0 | 0 | 0 | 0 | 0 | 0 |  |");
    } else {
        for locale in &report.locales {
            println!(
                "| {} | {} | {} | {} | {} | {} | {} | {} |",
                markdown_cell(&locale.locale),
                locale.keyword_count,
                locale.used_characters,
                locale.remaining_characters,
                locale.errors,
                locale.warnings,
                locale.infos,
                markdown_cell(&locale.keyword_field)
            );
        }
    }

    println!();
    println!("| severity | locale | related locales | id | keyword | term | message |");
    println!("| --- | --- | --- | --- | --- | --- | --- |");
    if report.checks.is_empty() {
        println!("|  |  |  |  |  |  | no findings |");
    } else {
        for check in &report.checks {
            println!(
                "| {} | {} | {} | {} | {} | {} | {} |",
                check.severity.as_str(),
                markdown_cell(&check.locale),
                markdown_cell(&check.related_locales.join(",")),
                markdown_cell(&check.id),
                markdown_cell(&check.keyword),
                markdown_cell(&check.matched_term),
                markdown_cell(&check.message)
            );
        }
    }
}

fn sanitize_cell(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

fn markdown_cell(value: &str) -> String {
    sanitize_cell(value).replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_keyword_audit_findings_like_asccli() {
        let report = audit_keywords(
            KeywordAuditInput {
                app_id: "app-1".into(),
                version_id: "ver-1".into(),
                version_string: "1.2.3".into(),
                platform: "IOS".into(),
                blocked_terms: vec!["free".into()],
                version_localizations: vec![
                    VersionLocalization {
                        id: "ver-loc-en".into(),
                        locale: "en-US".into(),
                        keywords: "Habit Tracker,mood journal,free trial,free trial,,".into(),
                    },
                    VersionLocalization {
                        id: "ver-loc-fr".into(),
                        locale: "fr-FR".into(),
                        keywords: "habit-tracker,journal humeur".into(),
                    },
                ],
                app_info_localizations: vec![
                    AppInfoLocalization {
                        id: "info-loc-en".into(),
                        locale: "en-US".into(),
                        name: "Habit Tracker".into(),
                        subtitle: "Daily Mood Journal".into(),
                    },
                    AppInfoLocalization {
                        id: "info-loc-fr".into(),
                        locale: "fr-FR".into(),
                        name: "Journal Humeur".into(),
                        subtitle: "Suivi quotidien".into(),
                    },
                ],
            },
            false,
        );

        assert_eq!(report.summary.errors, 0);
        assert!(report.summary.warnings > 0);
        assert!(report.summary.infos > 0);
        assert_eq!(report.summary.blocking, 0);
        assert!(has_check(&report, "metadata.keywords.locale_duplicates"));
        assert!(has_check(&report, "metadata.keywords.empty_segments"));
        assert!(has_check(&report, "metadata.keywords.overlap_name"));
        assert!(has_check(&report, "metadata.keywords.overlap_subtitle"));
        assert!(has_check(&report, "metadata.keywords.blocked_term"));
        assert!(has_check(
            &report,
            "metadata.keywords.cross_locale_duplicates"
        ));
        let cross_locale = report
            .checks
            .iter()
            .find(|check| check.id == "metadata.keywords.cross_locale_duplicates")
            .expect("cross locale check");
        assert_eq!(cross_locale.keyword, "Habit Tracker");
        assert_eq!(report.locales.len(), 2);
        assert_eq!(report.locales[0].locale, "en-US");
        assert!(report.locales[0].warnings > 0);
        assert!(report.locales[0].remaining_characters > 0);
    }

    #[test]
    fn strict_turns_warnings_blocking() {
        let report = audit_keywords(
            KeywordAuditInput {
                app_id: "app-1".into(),
                version_id: "ver-1".into(),
                version_string: String::new(),
                platform: String::new(),
                blocked_terms: vec!["free".into()],
                version_localizations: vec![VersionLocalization {
                    id: "ver-loc-en".into(),
                    locale: "en-US".into(),
                    keywords: "free trial".into(),
                }],
                app_info_localizations: Vec::new(),
            },
            true,
        );

        assert!(has_check(&report, "metadata.keywords.blocked_term"));
        assert!(report.summary.warnings > 0);
        assert_eq!(report.summary.blocking, report.summary.warnings);
    }

    #[test]
    fn reports_underfilled_budget_as_info() {
        let report = audit_keywords(
            KeywordAuditInput {
                app_id: "app-1".into(),
                version_id: "ver-1".into(),
                version_string: String::new(),
                platform: String::new(),
                blocked_terms: Vec::new(),
                version_localizations: vec![VersionLocalization {
                    id: "ver-loc-en".into(),
                    locale: "en-US".into(),
                    keywords: "alpha,beta".into(),
                }],
                app_info_localizations: Vec::new(),
            },
            false,
        );

        assert!(has_check(&report, "metadata.keywords.underfilled"));
        assert!(report.summary.infos > 0);
    }

    #[test]
    fn scans_empty_segments_and_noncanonical_separators() {
        let scan = scan_keyword_field("alpha； beta,,");
        assert_eq!(scan.tokens, vec!["alpha", "beta"]);
        assert!(scan.empty_segments);
        assert!(scan.noncanonical_separators);
    }

    #[test]
    fn blocked_terms_match_normalized_phrase_boundaries() {
        assert!(contains_normalized_phrase(
            &normalize_keyword_text("private tracker"),
            &normalize_keyword_text("tracker")
        ));
        assert!(!contains_normalized_phrase(
            &normalize_keyword_text("tracking-safe"),
            &normalize_keyword_text("rack")
        ));
    }

    fn has_check(report: &KeywordAuditReport, id: &str) -> bool {
        report.checks.iter().any(|check| check.id == id)
    }
}
