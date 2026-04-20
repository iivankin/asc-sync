use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use glob::glob;
use md5::{Digest, Md5};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{
    asc::{App, AscClient, asc_endpoint},
    cli::SubmitForReviewArgs,
    config::{
        AppInfoLocalizationSource, AppInfoLocalizationSpec, AppMediaLocalizationSpec, AppPlatform,
        AppReviewSpec, AppSpec, AppStoreInfoSpec, AppVersionLocalizationSource,
        AppVersionLocalizationSpec, AppVersionSpec, Config, CustomProductPageLocalizationSource,
        KeywordsSpec, MediaPathList, MediaScreenshotSet, MediaScreenshotSource, StringSource,
    },
    config_io, media_render, media_validate,
    sync::{Change, ChangeKind, Mode, SyncSummary},
};

mod resources;

const APP_RECORD_POLL_SECONDS: u64 = 5;
const MEDIA_PROCESSING_POLL_SECONDS: u64 = 3;
const MEDIA_PROCESSING_MAX_ATTEMPTS: usize = 40;

pub fn run_sync(config_path: &Path, config: &Config, client: &AscClient, mode: Mode) -> Result<()> {
    if config.apps.is_empty() {
        return Ok(());
    }

    println!("[app_store]");
    let mut engine = AppStoreSync::new(config_path, config, client, mode)?;
    let summary = engine.run()?;
    print_summary(&summary.changes);
    Ok(())
}

pub fn submit_for_review(args: &SubmitForReviewArgs) -> Result<()> {
    let config = config_io::load_config(&args.config)?;
    config.validate()?;
    let auth = crate::auth_store::resolve_auth_context(&config.team_id)?;
    let client = AscClient::new(auth)?;
    let config_dir = args.config.parent().unwrap_or_else(|| Path::new("."));
    let (app_key, app, platform, version) = resolve_submit_target(
        &config,
        args.app.as_deref(),
        args.platform.map(cli_platform),
    )?;
    let bundle = config
        .bundle_ids
        .get(&app.bundle_id_ref)
        .expect("config validation ensures bundle_id_ref exists");
    let api = AppStoreApi::new(&client);
    let app_record = find_or_wait_for_app_record(&api, &bundle.bundle_id, app_key, Mode::Apply)?;
    let version_record = api
        .find_version(&app_record.id, platform, &version.version_string)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "app {app_key} platform {platform} version {} does not exist; run apply first",
                version.version_string
            )
        })?;

    let context = ResolveContext::new(config_dir)?;
    if let Some(review) = &version.review {
        let resolved = context.resolve_review(review)?;
        api.ensure_review_detail(&version_record.id, &resolved)?;
    }

    let submission = api.find_or_create_review_submission(&app_record.id, platform)?;
    api.ensure_review_submission_item(&submission.id, &version_record.id)?;
    api.submit_review_submission(&submission.id)?;
    println!(
        "Submitted app {app_key} platform {platform} version {} for review.",
        version.version_string
    );
    Ok(())
}

struct AppStoreSync<'a> {
    config: &'a Config,
    api: AppStoreApi<'a>,
    mode: Mode,
    context: ResolveContext,
    media_render_dir: tempfile::TempDir,
    changes: Vec<Change>,
}

impl<'a> AppStoreSync<'a> {
    fn new(
        config_path: &'a Path,
        config: &'a Config,
        client: &'a AscClient,
        mode: Mode,
    ) -> Result<Self> {
        let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        Ok(Self {
            config,
            api: AppStoreApi::new(client),
            mode,
            context: ResolveContext::new(config_dir)?,
            media_render_dir: tempfile::Builder::new()
                .prefix("asc-sync-media-")
                .tempdir()
                .context("failed to create temporary media render directory")?,
            changes: Vec::new(),
        })
    }

    fn run(&mut self) -> Result<SyncSummary> {
        for (app_key, app) in &self.config.apps {
            self.reconcile_app(app_key, app)?;
        }

        Ok(SyncSummary {
            mode: self.mode,
            changes: std::mem::take(&mut self.changes),
        })
    }

    fn reconcile_app(&mut self, app_key: &str, app: &AppSpec) -> Result<()> {
        let bundle = self
            .config
            .bundle_ids
            .get(&app.bundle_id_ref)
            .expect("config validation ensures bundle_id_ref exists");
        let app_record =
            find_or_wait_for_app_record(&self.api, &bundle.bundle_id, app_key, self.mode)?;

        self.reconcile_shared(app_key, &app_record, app)?;
        if let Some(store_info) = &app.store_info {
            self.reconcile_store_info(app_key, &app_record, store_info)?;
        }
        self.reconcile_resource_families(app_key, &app_record, app)?;
        for (platform, platform_spec) in &app.platforms {
            self.reconcile_platform(app_key, &app_record, *platform, &platform_spec.version)?;
        }
        Ok(())
    }

    fn reconcile_shared(&mut self, app_key: &str, app_record: &App, app: &AppSpec) -> Result<()> {
        let Some(shared) = &app.shared else {
            return Ok(());
        };
        let mut attrs = Map::new();
        compare_string_attr(
            &mut attrs,
            "primaryLocale",
            shared.primary_locale.as_ref(),
            Some(app_record.attributes.primary_locale.as_str()),
            &self.context,
        )?;
        compare_string_attr(
            &mut attrs,
            "accessibilityUrl",
            shared.accessibility_url.as_ref(),
            app_record.attributes.accessibility_url.as_deref(),
            &self.context,
        )?;
        if let Some(value) = shared.content_rights_declaration {
            let desired = value.asc_value();
            if app_record.attributes.content_rights_declaration.as_deref() != Some(desired) {
                attrs.insert("contentRightsDeclaration".into(), json!(desired));
            }
        }
        compare_string_attr(
            &mut attrs,
            "subscriptionStatusUrl",
            shared.subscription_status_url.as_ref(),
            app_record.attributes.subscription_status_url.as_deref(),
            &self.context,
        )?;
        compare_string_attr(
            &mut attrs,
            "subscriptionStatusUrlForSandbox",
            shared.subscription_status_url_for_sandbox.as_ref(),
            app_record
                .attributes
                .subscription_status_url_for_sandbox
                .as_deref(),
            &self.context,
        )?;
        if let Some(value) = shared.streamlined_purchasing_enabled
            && app_record.attributes.streamlined_purchasing_enabled != Some(value)
        {
            attrs.insert("streamlinedPurchasingEnabled".into(), json!(value));
        }
        if attrs.is_empty() {
            return Ok(());
        }

        self.record(
            ChangeKind::Update,
            format!("app.{app_key}.shared"),
            "ensure app record attributes match config".into(),
        );
        if self.mode == Mode::Apply {
            self.api.update_app(&app_record.id, attrs)?;
        }
        Ok(())
    }

    fn reconcile_store_info(
        &mut self,
        app_key: &str,
        app_record: &App,
        store_info: &AppStoreInfoSpec,
    ) -> Result<()> {
        let infos = self.api.list_app_infos(&app_record.id)?;
        let Some(info) = infos.first() else {
            bail!("app {app_key} has no appInfos resource in App Store Connect");
        };
        let editable = info.is_editable();

        if let Some(categories) = &store_info.categories
            && !categories.is_empty()
        {
            if self.api.app_info_categories_match(&info.id, categories)? {
                // already in desired state
            } else if editable {
                self.record(
                    ChangeKind::Update,
                    format!("app.{app_key}.store_info.categories"),
                    "ensure App Store categories match config".into(),
                );
                if self.mode == Mode::Apply {
                    self.api.update_app_info_categories(&info.id, categories)?;
                }
            } else {
                println!(
                    "Skipping app.{app_key}.store_info.categories because app info state is {}.",
                    info.attributes.state.as_deref().unwrap_or("unknown")
                );
            }
        }

        if !store_info.localizations.is_empty() {
            self.reconcile_app_info_localizations(app_key, &info.id, editable, store_info)?;
        }

        if let Some(age_rating) = &store_info.age_rating
            && !age_rating.is_empty()
        {
            let declaration = self.api.get_age_rating_declaration(&info.id)?;
            if age_rating_matches(&declaration, age_rating) {
                // already in desired state
            } else if editable {
                self.record(
                    ChangeKind::Update,
                    format!("app.{app_key}.store_info.age_rating"),
                    "ensure age rating declaration matches config".into(),
                );
                if self.mode == Mode::Apply {
                    self.api.update_age_rating(&declaration.id, age_rating)?;
                }
            } else {
                println!(
                    "Skipping app.{app_key}.store_info.age_rating because app info state is {}.",
                    info.attributes.state.as_deref().unwrap_or("unknown")
                );
            }
        }

        Ok(())
    }

    fn reconcile_app_info_localizations(
        &mut self,
        app_key: &str,
        app_info_id: &str,
        editable: bool,
        store_info: &AppStoreInfoSpec,
    ) -> Result<()> {
        let current = self.api.list_app_info_localizations(app_info_id)?;
        let current_by_locale = current
            .iter()
            .map(|localization| (localization.attributes.locale.as_str(), localization))
            .collect::<BTreeMap<_, _>>();

        for (locale, source) in &store_info.localizations {
            let desired = self.context.resolve_app_info_localization(source)?;
            let existing = current_by_locale.get(locale.as_str()).copied();
            if existing.is_some_and(|existing| desired.matches_existing(existing)) {
                continue;
            }
            if !editable {
                println!(
                    "Skipping app.{app_key}.store_info.localizations.{locale} because app info is not editable."
                );
                continue;
            }
            self.record(
                if existing.is_some() {
                    ChangeKind::Update
                } else {
                    ChangeKind::Create
                },
                format!("app.{app_key}.store_info.localizations.{locale}"),
                "ensure app info localization matches config".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(existing) = existing {
                    self.api
                        .update_app_info_localization(&existing.id, &desired)?;
                } else {
                    self.api
                        .create_app_info_localization(app_info_id, locale, &desired)?;
                }
            }
        }
        Ok(())
    }

    fn reconcile_platform(
        &mut self,
        app_key: &str,
        app_record: &App,
        platform: AppPlatform,
        version: &AppVersionSpec,
    ) -> Result<()> {
        let mut planned_new_version = false;
        let version_record =
            match self
                .api
                .find_version(&app_record.id, platform, &version.version_string)?
            {
                Some(version_record) => version_record,
                None => {
                    self.record(
                        ChangeKind::Create,
                        format!("app.{app_key}.platform.{platform}.version"),
                        version.version_string.clone(),
                    );
                    if self.mode == Mode::Apply {
                        self.api.create_version(&app_record.id, platform, version)?
                    } else {
                        planned_new_version = true;
                        AppStoreVersion::planned(platform, &version.version_string)
                    }
                }
            };

        if planned_new_version {
            self.record_planned_version_children(app_key, platform, version);
            return Ok(());
        }

        let editable = version_record.is_editable();
        if editable {
            self.reconcile_version_attrs(app_key, platform, &version_record, version)?;
            self.reconcile_build(app_key, app_record, platform, &version_record, version)?;
            self.reconcile_version_localizations(
                app_key,
                platform,
                &version_record,
                version,
                true,
            )?;
            self.reconcile_review(app_key, platform, &version_record, version)?;
            self.reconcile_media(app_key, platform, &version_record, version)?;
        } else {
            println!(
                "Version app.{app_key}.platform.{platform}.version {} is not editable; only always-editable metadata will be considered.",
                version.version_string
            );
            self.reconcile_version_localizations(
                app_key,
                platform,
                &version_record,
                version,
                false,
            )?;
        }

        Ok(())
    }

    fn record_planned_version_children(
        &mut self,
        app_key: &str,
        platform: AppPlatform,
        version: &AppVersionSpec,
    ) {
        if version.build_number.is_some() {
            self.record(
                ChangeKind::Update,
                format!("app.{app_key}.platform.{platform}.version.build"),
                "attach configured build".into(),
            );
        }
        for locale in version.localizations.keys() {
            self.record(
                ChangeKind::Create,
                format!("app.{app_key}.platform.{platform}.localizations.{locale}"),
                "create version localization".into(),
            );
        }
        if version.review.is_some() {
            self.record(
                ChangeKind::Create,
                format!("app.{app_key}.platform.{platform}.review"),
                "create review details".into(),
            );
        }
        for (locale, media) in &version.media {
            for set in media.screenshots.keys() {
                self.record(
                    ChangeKind::Replace,
                    format!(
                        "app.{app_key}.platform.{platform}.media.{locale}.screenshots.{}",
                        set.asc_display_type()
                    ),
                    "replace screenshot set".into(),
                );
            }
            for set in media.app_previews.keys() {
                self.record(
                    ChangeKind::Replace,
                    format!(
                        "app.{app_key}.platform.{platform}.media.{locale}.app_previews.{}",
                        set.asc_preview_type()
                    ),
                    "replace app preview set".into(),
                );
            }
        }
    }

    fn reconcile_version_attrs(
        &mut self,
        app_key: &str,
        platform: AppPlatform,
        current: &AppStoreVersion,
        spec: &AppVersionSpec,
    ) -> Result<()> {
        let mut attrs = Map::new();
        compare_string_attr(
            &mut attrs,
            "copyright",
            spec.copyright.as_ref(),
            current.attributes.copyright.as_deref(),
            &self.context,
        )?;
        if let Some(release) = &spec.release {
            if let Some(kind) = release.kind {
                let desired = kind.asc_value();
                if current.attributes.release_type.as_deref() != Some(desired) {
                    attrs.insert("releaseType".into(), json!(desired));
                }
            }
            if let Some(date) = &release.earliest_release_date
                && current.attributes.earliest_release_date.as_deref() != Some(date.as_str())
            {
                attrs.insert("earliestReleaseDate".into(), json!(date));
            }
        }
        if attrs.is_empty() {
            return Ok(());
        }
        self.record(
            ChangeKind::Update,
            format!("app.{app_key}.platform.{platform}.version"),
            "ensure version attributes match config".into(),
        );
        if self.mode == Mode::Apply {
            self.api.update_version(&current.id, attrs, None)?;
        }
        Ok(())
    }

    fn reconcile_build(
        &mut self,
        app_key: &str,
        app_record: &App,
        platform: AppPlatform,
        version_record: &AppStoreVersion,
        spec: &AppVersionSpec,
    ) -> Result<()> {
        let Some(build_number) = &spec.build_number else {
            return Ok(());
        };
        let build = self
            .api
            .find_build(&app_record.id, platform, build_number)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "app {app_key} platform {platform} build {build_number} is missing or not VALID in App Store Connect"
                )
            })?;
        if version_record
            .relationships
            .as_ref()
            .and_then(|relationships| relationships.build.as_ref())
            .and_then(|relationship| relationship.data.as_ref())
            .is_some_and(|current| current.id == build.id)
        {
            return Ok(());
        }

        self.record(
            ChangeKind::Update,
            format!("app.{app_key}.platform.{platform}.version.build"),
            format!("attach build {build_number}"),
        );
        if self.mode == Mode::Apply {
            self.api
                .update_version(&version_record.id, Map::new(), Some(build.id.as_str()))?;
        }
        Ok(())
    }

    fn reconcile_version_localizations(
        &mut self,
        app_key: &str,
        platform: AppPlatform,
        version_record: &AppStoreVersion,
        spec: &AppVersionSpec,
        full_editable: bool,
    ) -> Result<()> {
        if spec.localizations.is_empty() {
            return Ok(());
        }
        let current = self.api.list_version_localizations(&version_record.id)?;
        let current_by_locale = current
            .iter()
            .map(|localization| (localization.attributes.locale.as_str(), localization))
            .collect::<BTreeMap<_, _>>();

        for (locale, source) in &spec.localizations {
            let mut desired = self.context.resolve_version_localization(source)?;
            if !full_editable {
                desired = desired.promotional_text_only();
                if desired.is_empty() {
                    continue;
                }
            }
            let existing = current_by_locale.get(locale.as_str()).copied();
            if existing.is_some_and(|existing| desired.matches_existing(existing)) {
                continue;
            }

            self.record(
                if existing.is_some() {
                    ChangeKind::Update
                } else {
                    ChangeKind::Create
                },
                format!("app.{app_key}.platform.{platform}.localizations.{locale}"),
                "ensure version localization matches config".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(existing) = existing {
                    self.api
                        .update_version_localization(&existing.id, &desired)?;
                } else {
                    self.api
                        .create_version_localization(&version_record.id, locale, &desired)?;
                }
            }
        }
        Ok(())
    }

    fn reconcile_review(
        &mut self,
        app_key: &str,
        platform: AppPlatform,
        version_record: &AppStoreVersion,
        spec: &AppVersionSpec,
    ) -> Result<()> {
        let Some(review) = &spec.review else {
            return Ok(());
        };
        let desired = self.context.resolve_review(review)?;
        let current = self.api.get_review_detail(&version_record.id)?;
        if current
            .as_ref()
            .is_some_and(|current| desired.matches_existing(current))
        {
            if !review.attachments.is_empty() {
                self.reconcile_review_attachments(app_key, platform, version_record, review)?;
            }
            return Ok(());
        }
        self.record(
            if current.is_some() {
                ChangeKind::Update
            } else {
                ChangeKind::Create
            },
            format!("app.{app_key}.platform.{platform}.review"),
            "ensure review details match config".into(),
        );
        if self.mode == Mode::Apply {
            self.api
                .ensure_review_detail(&version_record.id, &desired)?;
        }
        if !review.attachments.is_empty() {
            self.reconcile_review_attachments(app_key, platform, version_record, review)?;
        }
        Ok(())
    }

    fn reconcile_review_attachments(
        &mut self,
        app_key: &str,
        platform: AppPlatform,
        version_record: &AppStoreVersion,
        review: &AppReviewSpec,
    ) -> Result<()> {
        let review_detail = self
            .api
            .get_review_detail(&version_record.id)?
            .ok_or_else(|| anyhow::anyhow!("review detail was not created"))?;
        let files = self
            .context
            .resolve_paths(&MediaPathList::Many(review.attachments.to_vec()))?;
        let current = self.api.list_review_attachments(&review_detail.id)?;
        if uploaded_assets_match(&current, &files)? {
            return Ok(());
        }
        self.record(
            ChangeKind::Replace,
            format!("app.{app_key}.platform.{platform}.review.attachments"),
            "replace review attachments".into(),
        );
        if self.mode == Mode::Apply {
            for attachment in current {
                self.api.delete_review_attachment(&attachment.id)?;
            }
            for file in files {
                self.api
                    .upload_review_attachment(&review_detail.id, &file)?;
            }
        }
        Ok(())
    }

    fn reconcile_media(
        &mut self,
        app_key: &str,
        platform: AppPlatform,
        version_record: &AppStoreVersion,
        spec: &AppVersionSpec,
    ) -> Result<()> {
        if spec.media.is_empty() {
            return Ok(());
        }
        let current_localizations = self.api.list_version_localizations(&version_record.id)?;
        let current_by_locale = current_localizations
            .iter()
            .map(|localization| (localization.attributes.locale.as_str(), localization))
            .collect::<BTreeMap<_, _>>();

        for (locale, media) in &spec.media {
            let localization = current_by_locale.get(locale.as_str()).ok_or_else(|| {
                anyhow::anyhow!(
                    "app {app_key} platform {platform} media locale {locale} requires a matching version localization"
                )
            })?;
            self.reconcile_media_locale(
                app_key,
                platform,
                locale,
                localization,
                media,
                spec.localizations.get(locale),
            )?;
        }
        Ok(())
    }

    fn reconcile_media_locale(
        &mut self,
        app_key: &str,
        platform: AppPlatform,
        locale: &str,
        localization: &AppStoreVersionLocalization,
        media: &AppMediaLocalizationSpec,
        localization_source: Option<&AppVersionLocalizationSource>,
    ) -> Result<()> {
        for (set, source) in &media.screenshots {
            let output_dir = self
                .media_render_dir
                .path()
                .join(safe_path_segment(app_key))
                .join(platform.to_string())
                .join(safe_path_segment(locale))
                .join(set.config_key());
            let files = self.context.resolve_version_screenshot_source(
                locale,
                localization_source,
                *set,
                source,
                &output_dir,
            )?;
            media_validate::validate_screenshots(*set, &files)?;
            let display_type = set.asc_display_type();
            let existing = self
                .api
                .find_screenshot_set(&localization.id, display_type)?;
            let current_assets = if let Some(existing) = &existing {
                self.api.list_screenshots(&existing.id)?
            } else {
                Vec::new()
            };
            if uploaded_assets_match_ordered(&current_assets, &files)? {
                continue;
            }
            self.record(
                ChangeKind::Replace,
                format!(
                    "app.{app_key}.platform.{platform}.media.{locale}.screenshots.{display_type}"
                ),
                "replace screenshot set".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(existing) = existing {
                    self.api.delete_screenshot_set(&existing.id)?;
                }
                let set = self
                    .api
                    .create_screenshot_set(&localization.id, display_type)?;
                for file in files {
                    self.api.upload_screenshot(&set.id, &file)?;
                }
            }
        }

        for (set, paths) in &media.app_previews {
            let files = self.context.resolve_paths(paths)?;
            media_validate::validate_previews(*set, &files)?;
            let preview_type = set.asc_preview_type();
            let existing = self.api.find_preview_set(&localization.id, preview_type)?;
            let current_assets = if let Some(existing) = &existing {
                self.api.list_previews(&existing.id)?
            } else {
                Vec::new()
            };
            if uploaded_assets_match_ordered(&current_assets, &files)? {
                continue;
            }
            self.record(
                ChangeKind::Replace,
                format!(
                    "app.{app_key}.platform.{platform}.media.{locale}.app_previews.{preview_type}"
                ),
                "replace app preview set".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(existing) = existing {
                    self.api.delete_preview_set(&existing.id)?;
                }
                let set = self
                    .api
                    .create_preview_set(&localization.id, preview_type)?;
                for file in files {
                    self.api.upload_preview(&set.id, &file)?;
                }
            }
        }
        Ok(())
    }

    fn record(&mut self, kind: ChangeKind, subject: String, detail: String) {
        self.changes.push(Change {
            kind,
            subject,
            detail,
        });
    }
}

fn find_or_wait_for_app_record(
    api: &AppStoreApi<'_>,
    bundle_id: &str,
    app_key: &str,
    mode: Mode,
) -> Result<App> {
    if let Some(app) = api.client.find_app_by_bundle_id(bundle_id)? {
        return Ok(app);
    }
    if mode == Mode::Plan {
        bail!(
            "App Store Connect app record for app {app_key} ({bundle_id}) does not exist; create it in App Store Connect before apply"
        );
    }

    println!("Manual action required for app {app_key}.");
    println!("Create an App Store Connect app record for bundle ID {bundle_id}.");
    println!(
        "Waiting for App Store Connect to expose the app record so asc-sync can continue automatically."
    );
    loop {
        if let Some(app) = api.client.find_app_by_bundle_id(bundle_id)? {
            return Ok(app);
        }
        thread::sleep(Duration::from_secs(APP_RECORD_POLL_SECONDS));
    }
}

fn resolve_submit_target<'a>(
    config: &'a Config,
    explicit_app: Option<&'a str>,
    explicit_platform: Option<AppPlatform>,
) -> Result<(&'a str, &'a AppSpec, AppPlatform, &'a AppVersionSpec)> {
    let (app_key, app) = if let Some(app_key) = explicit_app {
        let app = config
            .apps
            .get(app_key)
            .ok_or_else(|| anyhow::anyhow!("unknown app key {app_key}"))?;
        (app_key, app)
    } else {
        let mut apps = config.apps.iter();
        let Some((app_key, app)) = apps.next() else {
            bail!("submit_for_review requires at least one app in asc.json");
        };
        ensure!(
            apps.next().is_none(),
            "submit_for_review requires --app when asc.json contains multiple apps"
        );
        (app_key.as_str(), app)
    };

    let (platform, platform_spec) = if let Some(platform) = explicit_platform {
        let platform_spec = app
            .platforms
            .get(&platform)
            .ok_or_else(|| anyhow::anyhow!("app {app_key} does not define platform {platform}"))?;
        (platform, platform_spec)
    } else {
        let mut platforms = app.platforms.iter();
        let Some((platform, platform_spec)) = platforms.next() else {
            bail!("app {app_key} does not define any platforms");
        };
        ensure!(
            platforms.next().is_none(),
            "submit_for_review requires --platform when app {app_key} contains multiple platforms"
        );
        (*platform, platform_spec)
    };
    Ok((app_key, app, platform, &platform_spec.version))
}

fn cli_platform(platform: crate::cli::AppPlatformArg) -> AppPlatform {
    match platform {
        crate::cli::AppPlatformArg::Ios => AppPlatform::Ios,
        crate::cli::AppPlatformArg::MacOs => AppPlatform::MacOs,
        crate::cli::AppPlatformArg::Tvos => AppPlatform::Tvos,
        crate::cli::AppPlatformArg::VisionOs => AppPlatform::VisionOs,
    }
}

fn compare_string_attr(
    attrs: &mut Map<String, Value>,
    key: &str,
    desired: Option<&StringSource>,
    current: Option<&str>,
    context: &ResolveContext,
) -> Result<()> {
    let Some(desired) = desired else {
        return Ok(());
    };
    let desired = context.resolve_string(desired)?;
    if current != Some(desired.as_str()) {
        attrs.insert(key.to_owned(), json!(desired));
    }
    Ok(())
}

fn print_summary(changes: &[Change]) {
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

fn render_change_kind(kind: &ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Create => "create",
        ChangeKind::Update => "update",
        ChangeKind::Replace => "replace",
        ChangeKind::Delete => "delete",
    }
}

struct ResolveContext {
    config_dir: PathBuf,
    dotenv: BTreeMap<String, String>,
}

impl ResolveContext {
    fn new(config_dir: &Path) -> Result<Self> {
        let dotenv_path = config_dir.join(".env");
        let dotenv = if dotenv_path.exists() {
            dotenvy::from_path_iter(&dotenv_path)
                .with_context(|| format!("failed to read {}", dotenv_path.display()))?
                .map(|item| item.context("failed to parse .env entry"))
                .collect::<Result<BTreeMap<_, _>>>()?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            config_dir: config_dir.to_path_buf(),
            dotenv,
        })
    }

    fn resolve_string(&self, source: &StringSource) -> Result<String> {
        match source {
            StringSource::Literal(value) => Ok(value.clone()),
            StringSource::Env { env: key } => env::var(key)
                .ok()
                .or_else(|| self.dotenv.get(key).cloned())
                .ok_or_else(|| anyhow::anyhow!("missing environment value {key}")),
        }
    }

    fn resolve_app_info_localization(
        &self,
        source: &AppInfoLocalizationSource,
    ) -> Result<ResolvedAppInfoLocalization> {
        let spec = match source {
            AppInfoLocalizationSource::Inline(spec) => spec.clone(),
            AppInfoLocalizationSource::Path(path) => self.load_json5(path)?,
        };
        ResolvedAppInfoLocalization::from_spec(self, &spec)
    }

    fn resolve_version_localization(
        &self,
        source: &AppVersionLocalizationSource,
    ) -> Result<ResolvedVersionLocalization> {
        let spec = match source {
            AppVersionLocalizationSource::Inline(spec) => (**spec).clone(),
            AppVersionLocalizationSource::Path(path) => self.load_json5(path)?,
        };
        ResolvedVersionLocalization::from_spec(self, &spec)
    }

    fn resolve_review(&self, review: &AppReviewSpec) -> Result<ResolvedReview> {
        Ok(ResolvedReview {
            contact_first_name: self.resolve_optional_string(review.contact_first_name.as_ref())?,
            contact_last_name: self.resolve_optional_string(review.contact_last_name.as_ref())?,
            contact_phone: self.resolve_optional_string(review.contact_phone.as_ref())?,
            contact_email: self.resolve_optional_string(review.contact_email.as_ref())?,
            demo_account_name: self.resolve_optional_string(review.demo_account_name.as_ref())?,
            demo_account_password: self
                .resolve_optional_string(review.demo_account_password.as_ref())?,
            demo_account_required: review.demo_account_required,
            notes: self.resolve_optional_string(review.notes.as_ref())?,
        })
    }

    fn resolve_optional_string(&self, source: Option<&StringSource>) -> Result<Option<String>> {
        source.map(|source| self.resolve_string(source)).transpose()
    }

    fn load_json5<T>(&self, path: &Path) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let path = self.resolve_path(path);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        json5::from_str(&text).with_context(|| format!("failed to parse JSON5 {}", path.display()))
    }

    fn resolve_paths(&self, list: &MediaPathList) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for path in list.paths() {
            let path_string = path.to_string_lossy();
            if contains_glob_meta(&path_string) {
                let pattern = self.resolve_path(path).to_string_lossy().into_owned();
                let mut matched = glob(&pattern)
                    .with_context(|| format!("invalid glob pattern {pattern}"))?
                    .map(|entry| {
                        entry.with_context(|| format!("failed to read glob entry {pattern}"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                matched.sort();
                paths.extend(matched);
            } else {
                paths.push(self.resolve_path(path));
            }
        }
        ensure!(!paths.is_empty(), "media path list resolved to no files");
        for path in &paths {
            ensure!(
                path.exists(),
                "media file {} does not exist",
                path.display()
            );
            ensure!(
                path.is_file(),
                "media path {} is not a file",
                path.display()
            );
        }
        Ok(paths)
    }

    fn resolve_version_screenshot_source(
        &self,
        locale: &str,
        localization: Option<&AppVersionLocalizationSource>,
        set: MediaScreenshotSet,
        source: &MediaScreenshotSource,
        output_dir: &Path,
    ) -> Result<Vec<PathBuf>> {
        match source {
            MediaScreenshotSource::Paths(paths) => self.resolve_paths(paths),
            MediaScreenshotSource::Render(render) => {
                media_render::render_config_screenshots_to_dir(
                    &self.config_dir,
                    output_dir,
                    locale,
                    localization,
                    set,
                    &render.render,
                )
            }
        }
    }

    fn resolve_custom_product_page_screenshot_source(
        &self,
        locale: &str,
        localization: &CustomProductPageLocalizationSource,
        set: MediaScreenshotSet,
        source: &MediaScreenshotSource,
        output_dir: &Path,
    ) -> Result<Vec<PathBuf>> {
        match source {
            MediaScreenshotSource::Paths(paths) => self.resolve_paths(paths),
            MediaScreenshotSource::Render(render) => {
                media_render::render_custom_product_page_config_screenshots_to_dir(
                    &self.config_dir,
                    output_dir,
                    locale,
                    localization,
                    set,
                    &render.render,
                )
            }
        }
    }

    fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.config_dir.join(path)
        }
    }
}

fn contains_glob_meta(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.contains('[')
}

fn safe_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
struct ResolvedAppInfoLocalization {
    name: String,
    subtitle: Option<String>,
    privacy_policy_url: Option<String>,
    privacy_choices_url: Option<String>,
    privacy_policy_text: Option<String>,
}

impl ResolvedAppInfoLocalization {
    fn from_spec(context: &ResolveContext, spec: &AppInfoLocalizationSpec) -> Result<Self> {
        Ok(Self {
            name: context.resolve_string(&spec.name)?,
            subtitle: context.resolve_optional_string(spec.subtitle.as_ref())?,
            privacy_policy_url: context
                .resolve_optional_string(spec.privacy_policy_url.as_ref())?,
            privacy_choices_url: context
                .resolve_optional_string(spec.privacy_choices_url.as_ref())?,
            privacy_policy_text: context
                .resolve_optional_string(spec.privacy_policy_text.as_ref())?,
        })
    }

    fn matches_existing(&self, existing: &AppInfoLocalization) -> bool {
        existing.attributes.name == self.name
            && existing.attributes.subtitle == self.subtitle
            && existing.attributes.privacy_policy_url == self.privacy_policy_url
            && existing.attributes.privacy_choices_url == self.privacy_choices_url
            && existing.attributes.privacy_policy_text == self.privacy_policy_text
    }
}

#[derive(Debug, Clone)]
struct ResolvedVersionLocalization {
    description: Option<String>,
    keywords: Option<String>,
    marketing_url: Option<String>,
    promotional_text: Option<String>,
    support_url: Option<String>,
    whats_new: Option<String>,
}

impl ResolvedVersionLocalization {
    fn from_spec(context: &ResolveContext, spec: &AppVersionLocalizationSpec) -> Result<Self> {
        let keywords = match &spec.keywords {
            Some(KeywordsSpec::String(source)) => Some(context.resolve_string(source)?),
            Some(KeywordsSpec::List(values)) => Some(values.join(",")),
            None => None,
        };
        Ok(Self {
            description: context.resolve_optional_string(spec.description.as_ref())?,
            keywords,
            marketing_url: context.resolve_optional_string(spec.marketing_url.as_ref())?,
            promotional_text: context.resolve_optional_string(spec.promotional_text.as_ref())?,
            support_url: context.resolve_optional_string(spec.support_url.as_ref())?,
            whats_new: context.resolve_optional_string(spec.whats_new.as_ref())?,
        })
    }

    fn promotional_text_only(self) -> Self {
        Self {
            promotional_text: self.promotional_text,
            description: None,
            keywords: None,
            marketing_url: None,
            support_url: None,
            whats_new: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.description.is_none()
            && self.keywords.is_none()
            && self.marketing_url.is_none()
            && self.promotional_text.is_none()
            && self.support_url.is_none()
            && self.whats_new.is_none()
    }

    fn matches_existing(&self, existing: &AppStoreVersionLocalization) -> bool {
        optional_matches(
            self.description.as_deref(),
            existing.attributes.description.as_deref(),
        ) && optional_matches(
            self.keywords.as_deref(),
            existing.attributes.keywords.as_deref(),
        ) && optional_matches(
            self.marketing_url.as_deref(),
            existing.attributes.marketing_url.as_deref(),
        ) && optional_matches(
            self.promotional_text.as_deref(),
            existing.attributes.promotional_text.as_deref(),
        ) && optional_matches(
            self.support_url.as_deref(),
            existing.attributes.support_url.as_deref(),
        ) && optional_matches(
            self.whats_new.as_deref(),
            existing.attributes.whats_new.as_deref(),
        )
    }
}

fn optional_matches(desired: Option<&str>, existing: Option<&str>) -> bool {
    desired.is_none_or(|desired| existing == Some(desired))
}

#[derive(Debug, Clone)]
struct ResolvedReview {
    contact_first_name: Option<String>,
    contact_last_name: Option<String>,
    contact_phone: Option<String>,
    contact_email: Option<String>,
    demo_account_name: Option<String>,
    demo_account_password: Option<String>,
    demo_account_required: Option<bool>,
    notes: Option<String>,
}

impl ResolvedReview {
    fn matches_existing(&self, existing: &AppStoreReviewDetail) -> bool {
        optional_matches(
            self.contact_first_name.as_deref(),
            existing.attributes.contact_first_name.as_deref(),
        ) && optional_matches(
            self.contact_last_name.as_deref(),
            existing.attributes.contact_last_name.as_deref(),
        ) && optional_matches(
            self.contact_phone.as_deref(),
            existing.attributes.contact_phone.as_deref(),
        ) && optional_matches(
            self.contact_email.as_deref(),
            existing.attributes.contact_email.as_deref(),
        ) && optional_matches(
            self.demo_account_name.as_deref(),
            existing.attributes.demo_account_name.as_deref(),
        ) && optional_matches(
            self.demo_account_password.as_deref(),
            existing.attributes.demo_account_password.as_deref(),
        ) && self
            .demo_account_required
            .is_none_or(|desired| existing.attributes.demo_account_required == Some(desired))
            && optional_matches(self.notes.as_deref(), existing.attributes.notes.as_deref())
    }
}

struct AppStoreApi<'a> {
    client: &'a AscClient,
}

impl<'a> AppStoreApi<'a> {
    fn new(client: &'a AscClient) -> Self {
        Self { client }
    }

    fn update_app(&self, app_id: &str, attributes: Map<String, Value>) -> Result<App> {
        self.patch_resource("/apps", "apps", app_id, attributes, None)
    }

    fn list_app_infos(&self, app_id: &str) -> Result<Vec<AppInfo>> {
        self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/appInfos")),
            vec![
                (
                    "fields[appInfos]".into(),
                    "state,appInfoLocalizations,ageRatingDeclaration,primaryCategory,primarySubcategoryOne,primarySubcategoryTwo,secondaryCategory,secondarySubcategoryOne,secondarySubcategoryTwo".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn update_app_info_categories(
        &self,
        app_info_id: &str,
        categories: &crate::config::AppCategoriesSpec,
    ) -> Result<AppInfo> {
        let mut relationships = Map::new();
        insert_category_relationship(
            &mut relationships,
            "primaryCategory",
            categories.primary.as_deref(),
        );
        insert_category_relationship(
            &mut relationships,
            "primarySubcategoryOne",
            categories.primary_subcategory_one.as_deref(),
        );
        insert_category_relationship(
            &mut relationships,
            "primarySubcategoryTwo",
            categories.primary_subcategory_two.as_deref(),
        );
        insert_category_relationship(
            &mut relationships,
            "secondaryCategory",
            categories.secondary.as_deref(),
        );
        insert_category_relationship(
            &mut relationships,
            "secondarySubcategoryOne",
            categories.secondary_subcategory_one.as_deref(),
        );
        insert_category_relationship(
            &mut relationships,
            "secondarySubcategoryTwo",
            categories.secondary_subcategory_two.as_deref(),
        );
        self.patch_resource(
            "/appInfos",
            "appInfos",
            app_info_id,
            Map::new(),
            Some(relationships),
        )
    }

    fn app_info_categories_match(
        &self,
        app_info_id: &str,
        categories: &crate::config::AppCategoriesSpec,
    ) -> Result<bool> {
        Ok(category_matches(
            categories.primary.as_deref(),
            self.get_related_id(&format!(
                "/appInfos/{app_info_id}/relationships/primaryCategory"
            ))?,
        ) && category_matches(
            categories.primary_subcategory_one.as_deref(),
            self.get_related_id(&format!(
                "/appInfos/{app_info_id}/relationships/primarySubcategoryOne"
            ))?,
        ) && category_matches(
            categories.primary_subcategory_two.as_deref(),
            self.get_related_id(&format!(
                "/appInfos/{app_info_id}/relationships/primarySubcategoryTwo"
            ))?,
        ) && category_matches(
            categories.secondary.as_deref(),
            self.get_related_id(&format!(
                "/appInfos/{app_info_id}/relationships/secondaryCategory"
            ))?,
        ) && category_matches(
            categories.secondary_subcategory_one.as_deref(),
            self.get_related_id(&format!(
                "/appInfos/{app_info_id}/relationships/secondarySubcategoryOne"
            ))?,
        ) && category_matches(
            categories.secondary_subcategory_two.as_deref(),
            self.get_related_id(&format!(
                "/appInfos/{app_info_id}/relationships/secondarySubcategoryTwo"
            ))?,
        ))
    }

    fn list_app_info_localizations(&self, app_info_id: &str) -> Result<Vec<AppInfoLocalization>> {
        self.client.get_paginated(
            asc_endpoint(&format!("/appInfos/{app_info_id}/appInfoLocalizations")),
            vec![
                (
                    "fields[appInfoLocalizations]".into(),
                    "locale,name,subtitle,privacyPolicyUrl,privacyChoicesUrl,privacyPolicyText"
                        .into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn create_app_info_localization(
        &self,
        app_info_id: &str,
        locale: &str,
        desired: &ResolvedAppInfoLocalization,
    ) -> Result<AppInfoLocalization> {
        let mut attributes = app_info_localization_attrs(desired);
        attributes.insert("locale".into(), json!(locale));
        let relationships = json!({
            "appInfo": {
                "data": { "type": "appInfos", "id": app_info_id }
            }
        });
        self.post_resource(
            "/appInfoLocalizations",
            "appInfoLocalizations",
            attributes,
            relationships.as_object().cloned(),
        )
    }

    fn update_app_info_localization(
        &self,
        localization_id: &str,
        desired: &ResolvedAppInfoLocalization,
    ) -> Result<AppInfoLocalization> {
        self.patch_resource(
            "/appInfoLocalizations",
            "appInfoLocalizations",
            localization_id,
            app_info_localization_attrs(desired),
            None,
        )
    }

    fn get_age_rating_declaration(&self, app_info_id: &str) -> Result<AgeRatingDeclaration> {
        self.get_related_single(&format!("/appInfos/{app_info_id}/ageRatingDeclaration"))
    }

    fn update_age_rating(
        &self,
        declaration_id: &str,
        attrs: &BTreeMap<String, Value>,
    ) -> Result<AgeRatingDeclaration> {
        self.patch_resource(
            "/ageRatingDeclarations",
            "ageRatingDeclarations",
            declaration_id,
            age_rating_attrs(attrs),
            None,
        )
    }

    fn find_version(
        &self,
        app_id: &str,
        platform: AppPlatform,
        version_string: &str,
    ) -> Result<Option<AppStoreVersion>> {
        let versions: Vec<AppStoreVersion> = self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/appStoreVersions")),
            vec![
                ("filter[platform]".into(), platform.asc_value().into()),
                ("filter[versionString]".into(), version_string.into()),
                (
                    "fields[appStoreVersions]".into(),
                    "platform,versionString,appStoreState,appVersionState,copyright,releaseType,earliestReleaseDate,build,appStoreReviewDetail,appStoreVersionLocalizations".into(),
                ),
                ("include".into(), "build".into()),
                ("limit".into(), "2".into()),
            ],
        )?;
        ensure!(
            versions.len() <= 1,
            "App Store Connect returned multiple appStoreVersions for platform {} version {}",
            platform,
            version_string
        );
        Ok(versions.into_iter().next())
    }

    fn create_version(
        &self,
        app_id: &str,
        platform: AppPlatform,
        spec: &AppVersionSpec,
    ) -> Result<AppStoreVersion> {
        let mut attributes = Map::new();
        attributes.insert("platform".into(), json!(platform.asc_value()));
        attributes.insert("versionString".into(), json!(spec.version_string));
        if let Some(release) = &spec.release {
            if let Some(kind) = release.kind {
                attributes.insert("releaseType".into(), json!(kind.asc_value()));
            }
            if let Some(date) = &release.earliest_release_date {
                attributes.insert("earliestReleaseDate".into(), json!(date));
            }
        }
        let relationships = json!({
            "app": { "data": { "type": "apps", "id": app_id } }
        });
        self.post_resource(
            "/appStoreVersions",
            "appStoreVersions",
            attributes,
            relationships.as_object().cloned(),
        )
    }

    fn update_version(
        &self,
        version_id: &str,
        attributes: Map<String, Value>,
        build_id: Option<&str>,
    ) -> Result<AppStoreVersion> {
        let relationships = build_id.map(|build_id| {
            json!({
                "build": { "data": { "type": "builds", "id": build_id } }
            })
            .as_object()
            .cloned()
            .expect("object")
        });
        self.patch_resource(
            "/appStoreVersions",
            "appStoreVersions",
            version_id,
            attributes,
            relationships,
        )
    }

    fn find_build(
        &self,
        app_id: &str,
        platform: AppPlatform,
        build_number: &str,
    ) -> Result<Option<Build>> {
        let builds: Vec<Build> = self.client.get_paginated(
            asc_endpoint("/builds"),
            vec![
                ("filter[app]".into(), app_id.into()),
                ("filter[version]".into(), build_number.into()),
                (
                    "filter[preReleaseVersion.platform]".into(),
                    platform.asc_value().into(),
                ),
                ("filter[processingState]".into(), "VALID".into()),
                ("fields[builds]".into(), "version,processingState".into()),
                ("limit".into(), "2".into()),
            ],
        )?;
        ensure!(
            builds.len() <= 1,
            "App Store Connect returned multiple VALID builds for app {app_id} platform {platform} build {build_number}"
        );
        Ok(builds.into_iter().next())
    }

    fn list_version_localizations(
        &self,
        version_id: &str,
    ) -> Result<Vec<AppStoreVersionLocalization>> {
        self.client.get_paginated(
            asc_endpoint(&format!(
                "/appStoreVersions/{version_id}/appStoreVersionLocalizations"
            )),
            vec![
                (
                    "fields[appStoreVersionLocalizations]".into(),
                    "locale,description,keywords,marketingUrl,promotionalText,supportUrl,whatsNew,appScreenshotSets,appPreviewSets".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn create_version_localization(
        &self,
        version_id: &str,
        locale: &str,
        desired: &ResolvedVersionLocalization,
    ) -> Result<AppStoreVersionLocalization> {
        let mut attributes = version_localization_attrs(desired);
        attributes.insert("locale".into(), json!(locale));
        let relationships = json!({
            "appStoreVersion": {
                "data": { "type": "appStoreVersions", "id": version_id }
            }
        });
        self.post_resource(
            "/appStoreVersionLocalizations",
            "appStoreVersionLocalizations",
            attributes,
            relationships.as_object().cloned(),
        )
    }

    fn update_version_localization(
        &self,
        localization_id: &str,
        desired: &ResolvedVersionLocalization,
    ) -> Result<AppStoreVersionLocalization> {
        self.patch_resource(
            "/appStoreVersionLocalizations",
            "appStoreVersionLocalizations",
            localization_id,
            version_localization_attrs(desired),
            None,
        )
    }

    fn get_review_detail(&self, version_id: &str) -> Result<Option<AppStoreReviewDetail>> {
        match self.get_related_single(&format!(
            "/appStoreVersions/{version_id}/appStoreReviewDetail"
        )) {
            Ok(detail) => Ok(Some(detail)),
            Err(error) if error.to_string().contains("returned 404") => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn ensure_review_detail(
        &self,
        version_id: &str,
        desired: &ResolvedReview,
    ) -> Result<AppStoreReviewDetail> {
        if let Some(existing) = self.get_review_detail(version_id)? {
            return self.patch_resource(
                "/appStoreReviewDetails",
                "appStoreReviewDetails",
                &existing.id,
                review_attrs(desired),
                None,
            );
        }
        let relationships = json!({
            "appStoreVersion": {
                "data": { "type": "appStoreVersions", "id": version_id }
            }
        });
        self.post_resource(
            "/appStoreReviewDetails",
            "appStoreReviewDetails",
            review_attrs(desired),
            relationships.as_object().cloned(),
        )
    }

    fn list_review_attachments(
        &self,
        review_detail_id: &str,
    ) -> Result<Vec<AppStoreReviewAttachment>> {
        self.client.get_paginated(
            asc_endpoint(&format!(
                "/appStoreReviewDetails/{review_detail_id}/appStoreReviewAttachments"
            )),
            vec![
                (
                    "fields[appStoreReviewAttachments]".into(),
                    "fileName,fileSize,sourceFileChecksum,assetDeliveryState".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn delete_review_attachment(&self, attachment_id: &str) -> Result<()> {
        self.client.request_empty(
            Method::DELETE,
            asc_endpoint(&format!("/appStoreReviewAttachments/{attachment_id}")),
            &[],
            None::<&Value>,
        )
    }

    fn upload_review_attachment(&self, review_detail_id: &str, path: &Path) -> Result<()> {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let file_name = file_name(path)?;
        let asset: AppStoreReviewAttachment = self.post_resource(
            "/appStoreReviewAttachments",
            "appStoreReviewAttachments",
            file_attrs(file_name, bytes.len()),
            json!({
                "appStoreReviewDetail": {
                    "data": { "type": "appStoreReviewDetails", "id": review_detail_id }
                }
            })
            .as_object()
            .cloned(),
        )?;
        self.upload_operations(
            asset.attributes.upload_operations.as_deref().unwrap_or(&[]),
            &bytes,
        )?;
        let checksum = md5_hex(&bytes);
        self.patch_resource(
            "/appStoreReviewAttachments",
            "appStoreReviewAttachments",
            &asset.id,
            uploaded_attrs(&checksum),
            None,
        )
        .map(|_: AppStoreReviewAttachment| ())
    }

    fn find_screenshot_set(
        &self,
        localization_id: &str,
        display_type: &str,
    ) -> Result<Option<AppScreenshotSet>> {
        let sets: Vec<AppScreenshotSet> = self.client.get_paginated(
            asc_endpoint(&format!(
                "/appStoreVersionLocalizations/{localization_id}/appScreenshotSets"
            )),
            vec![
                (
                    "fields[appScreenshotSets]".into(),
                    "screenshotDisplayType".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )?;
        Ok(sets
            .into_iter()
            .find(|set| set.attributes.screenshot_display_type == display_type))
    }

    fn create_screenshot_set(
        &self,
        localization_id: &str,
        display_type: &str,
    ) -> Result<AppScreenshotSet> {
        let relationships = json!({
            "appStoreVersionLocalization": {
                "data": { "type": "appStoreVersionLocalizations", "id": localization_id }
            }
        });
        self.post_resource(
            "/appScreenshotSets",
            "appScreenshotSets",
            map_with("screenshotDisplayType", json!(display_type)),
            relationships.as_object().cloned(),
        )
    }

    fn delete_screenshot_set(&self, set_id: &str) -> Result<()> {
        self.client.request_empty(
            Method::DELETE,
            asc_endpoint(&format!("/appScreenshotSets/{set_id}")),
            &[],
            None::<&Value>,
        )
    }

    fn list_screenshots(&self, set_id: &str) -> Result<Vec<AppScreenshot>> {
        self.client.get_paginated(
            asc_endpoint(&format!("/appScreenshotSets/{set_id}/appScreenshots")),
            vec![
                (
                    "fields[appScreenshots]".into(),
                    "fileName,fileSize,sourceFileChecksum,assetDeliveryState".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn upload_screenshot(&self, set_id: &str, path: &Path) -> Result<()> {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let file_name = file_name(path)?;
        let asset: AppScreenshot = self.post_resource(
            "/appScreenshots",
            "appScreenshots",
            file_attrs(file_name, bytes.len()),
            json!({
                "appScreenshotSet": {
                    "data": { "type": "appScreenshotSets", "id": set_id }
                }
            })
            .as_object()
            .cloned(),
        )?;
        self.upload_operations(
            asset.attributes.upload_operations.as_deref().unwrap_or(&[]),
            &bytes,
        )?;
        let checksum = md5_hex(&bytes);
        self.patch_resource(
            "/appScreenshots",
            "appScreenshots",
            &asset.id,
            uploaded_attrs(&checksum),
            None,
        )
        .map(|_: AppScreenshot| ())?;
        self.wait_for_image_asset(&asset.id)
    }

    fn find_preview_set(
        &self,
        localization_id: &str,
        preview_type: &str,
    ) -> Result<Option<AppPreviewSet>> {
        let sets: Vec<AppPreviewSet> = self.client.get_paginated(
            asc_endpoint(&format!(
                "/appStoreVersionLocalizations/{localization_id}/appPreviewSets"
            )),
            vec![
                ("fields[appPreviewSets]".into(), "previewType".into()),
                ("limit".into(), "50".into()),
            ],
        )?;
        Ok(sets
            .into_iter()
            .find(|set| set.attributes.preview_type == preview_type))
    }

    fn create_preview_set(
        &self,
        localization_id: &str,
        preview_type: &str,
    ) -> Result<AppPreviewSet> {
        let relationships = json!({
            "appStoreVersionLocalization": {
                "data": { "type": "appStoreVersionLocalizations", "id": localization_id }
            }
        });
        self.post_resource(
            "/appPreviewSets",
            "appPreviewSets",
            map_with("previewType", json!(preview_type)),
            relationships.as_object().cloned(),
        )
    }

    fn delete_preview_set(&self, set_id: &str) -> Result<()> {
        self.client.request_empty(
            Method::DELETE,
            asc_endpoint(&format!("/appPreviewSets/{set_id}")),
            &[],
            None::<&Value>,
        )
    }

    fn list_previews(&self, set_id: &str) -> Result<Vec<AppPreview>> {
        self.client.get_paginated(
            asc_endpoint(&format!("/appPreviewSets/{set_id}/appPreviews")),
            vec![
                (
                    "fields[appPreviews]".into(),
                    "fileName,fileSize,sourceFileChecksum,videoDeliveryState".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn upload_preview(&self, set_id: &str, path: &Path) -> Result<()> {
        let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let file_name = file_name(path)?;
        let mime_type = preview_mime_type(path)?;
        let mut attrs = file_attrs(file_name, bytes.len());
        attrs.insert("mimeType".into(), json!(mime_type));
        let asset: AppPreview = self.post_resource(
            "/appPreviews",
            "appPreviews",
            attrs,
            json!({
                "appPreviewSet": {
                    "data": { "type": "appPreviewSets", "id": set_id }
                }
            })
            .as_object()
            .cloned(),
        )?;
        self.upload_operations(
            asset.attributes.upload_operations.as_deref().unwrap_or(&[]),
            &bytes,
        )?;
        let checksum = md5_hex(&bytes);
        self.patch_resource(
            "/appPreviews",
            "appPreviews",
            &asset.id,
            uploaded_attrs(&checksum),
            None,
        )
        .map(|_: AppPreview| ())?;
        self.wait_for_video_asset(&asset.id)
    }

    fn find_or_create_review_submission(
        &self,
        app_id: &str,
        platform: AppPlatform,
    ) -> Result<ReviewSubmission> {
        let submissions: Vec<ReviewSubmission> = self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/reviewSubmissions")),
            vec![
                ("filter[platform]".into(), platform.asc_value().into()),
                (
                    "fields[reviewSubmissions]".into(),
                    "platform,state,items".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )?;
        if let Some(submission) = submissions
            .iter()
            .find(|submission| submission.attributes.state.as_deref() == Some("READY_FOR_REVIEW"))
        {
            return Ok(submission.clone());
        }

        self.post_resource(
            "/reviewSubmissions",
            "reviewSubmissions",
            map_with("platform", json!(platform.asc_value())),
            json!({
                "app": { "data": { "type": "apps", "id": app_id } }
            })
            .as_object()
            .cloned(),
        )
    }

    fn ensure_review_submission_item(
        &self,
        submission_id: &str,
        version_id: &str,
    ) -> Result<ReviewSubmissionItem> {
        let items: Vec<ReviewSubmissionItem> = self.client.get_paginated(
            asc_endpoint(&format!("/reviewSubmissions/{submission_id}/items")),
            vec![
                (
                    "fields[reviewSubmissionItems]".into(),
                    "appStoreVersion".into(),
                ),
                ("include".into(), "appStoreVersion".into()),
                ("limit".into(), "50".into()),
            ],
        )?;
        if let Some(item) = items.iter().find(|item| {
            item.relationships
                .as_ref()
                .and_then(|relationships| relationships.app_store_version.as_ref())
                .and_then(|relationship| relationship.data.as_ref())
                .is_some_and(|data| data.id == version_id)
        }) {
            return Ok(item.clone());
        }

        self.post_resource(
            "/reviewSubmissionItems",
            "reviewSubmissionItems",
            Map::new(),
            json!({
                "reviewSubmission": {
                    "data": { "type": "reviewSubmissions", "id": submission_id }
                },
                "appStoreVersion": {
                    "data": { "type": "appStoreVersions", "id": version_id }
                }
            })
            .as_object()
            .cloned(),
        )
    }

    fn submit_review_submission(&self, submission_id: &str) -> Result<ReviewSubmission> {
        self.patch_resource(
            "/reviewSubmissions",
            "reviewSubmissions",
            submission_id,
            map_with("submitted", json!(true)),
            None,
        )
    }

    fn upload_operations(&self, operations: &[UploadOperation], bytes: &[u8]) -> Result<()> {
        ensure!(
            !operations.is_empty(),
            "ASC did not return upload operations"
        );
        for operation in operations {
            let offset = operation.offset.unwrap_or(0);
            let length = operation
                .length
                .unwrap_or(bytes.len().saturating_sub(offset));
            let end = offset
                .checked_add(length)
                .ok_or_else(|| anyhow::anyhow!("upload operation range overflows"))?;
            ensure!(
                end <= bytes.len(),
                "upload operation range {}..{} exceeds asset size {}",
                offset,
                end,
                bytes.len()
            );
            self.client.upload_asset_part(
                operation.method.as_deref().unwrap_or("PUT"),
                operation
                    .url
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("upload operation is missing url"))?,
                &operation.headers(),
                bytes[offset..end].to_vec(),
            )?;
        }
        Ok(())
    }

    fn wait_for_image_asset(&self, asset_id: &str) -> Result<()> {
        for _ in 0..MEDIA_PROCESSING_MAX_ATTEMPTS {
            let screenshot: AppScreenshot = self.get_instance("/appScreenshots", asset_id)?;
            if screenshot.attributes.delivery_state_complete() {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(MEDIA_PROCESSING_POLL_SECONDS));
        }
        bail!("screenshot {asset_id} did not finish processing in time")
    }

    fn wait_for_video_asset(&self, asset_id: &str) -> Result<()> {
        for _ in 0..MEDIA_PROCESSING_MAX_ATTEMPTS {
            let preview: AppPreview = self.get_instance("/appPreviews", asset_id)?;
            if preview.attributes.video_state_complete() {
                return Ok(());
            }
            thread::sleep(Duration::from_secs(MEDIA_PROCESSING_POLL_SECONDS));
        }
        bail!("app preview {asset_id} did not finish processing in time")
    }

    fn get_instance<T>(&self, path: &str, id: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.client
            .request_json(
                Method::GET,
                asc_endpoint(&format!("{path}/{id}")),
                &[],
                None::<&Value>,
            )
            .map(|response: JsonApiSingle<T>| response.data)
    }

    fn get_related_single<T>(&self, path: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.client
            .request_json(Method::GET, asc_endpoint(path), &[], None::<&Value>)
            .map(|response: JsonApiSingle<T>| response.data)
    }

    fn get_related_id(&self, path: &str) -> Result<Option<String>> {
        self.client
            .request_json(Method::GET, asc_endpoint(path), &[], None::<&Value>)
            .map(|response: JsonApiSingle<Option<ResourceIdentifier>>| {
                response.data.map(|resource| resource.id)
            })
    }

    fn post_resource<T>(
        &self,
        path: &str,
        kind: &str,
        attributes: Map<String, Value>,
        relationships: Option<Map<String, Value>>,
    ) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let request = build_resource_request(None, kind, attributes, relationships);
        self.client
            .request_json(Method::POST, asc_endpoint(path), &[], Some(&request))
            .map(|response: JsonApiSingle<T>| response.data)
    }

    fn patch_resource<T>(
        &self,
        path: &str,
        kind: &str,
        id: &str,
        attributes: Map<String, Value>,
        relationships: Option<Map<String, Value>>,
    ) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let request = build_resource_request(Some(id), kind, attributes, relationships);
        self.client
            .request_json(
                Method::PATCH,
                asc_endpoint(&format!("{path}/{id}")),
                &[],
                Some(&request),
            )
            .map(|response: JsonApiSingle<T>| response.data)
    }
}

fn build_resource_request(
    id: Option<&str>,
    kind: &str,
    attributes: Map<String, Value>,
    relationships: Option<Map<String, Value>>,
) -> Value {
    let mut data = Map::new();
    data.insert("type".into(), json!(kind));
    if let Some(id) = id {
        data.insert("id".into(), json!(id));
    }
    if !attributes.is_empty() {
        data.insert("attributes".into(), Value::Object(attributes));
    }
    if let Some(relationships) = relationships
        && !relationships.is_empty()
    {
        data.insert("relationships".into(), Value::Object(relationships));
    }
    json!({ "data": data })
}

fn insert_category_relationship(
    relationships: &mut Map<String, Value>,
    key: &str,
    id: Option<&str>,
) {
    if let Some(id) = id {
        relationships.insert(
            key.into(),
            json!({ "data": { "type": "appCategories", "id": id } }),
        );
    }
}

fn category_matches(desired: Option<&str>, current: Option<String>) -> bool {
    desired.is_none_or(|desired| current.as_deref() == Some(desired))
}

fn age_rating_attrs(attrs: &BTreeMap<String, Value>) -> Map<String, Value> {
    attrs
        .iter()
        .map(|(key, value)| (snake_to_camel(key), value.clone()))
        .collect()
}

fn age_rating_matches(current: &AgeRatingDeclaration, desired: &BTreeMap<String, Value>) -> bool {
    age_rating_attrs(desired)
        .iter()
        .all(|(key, value)| current.attributes.get(key) == Some(value))
}

fn app_info_localization_attrs(desired: &ResolvedAppInfoLocalization) -> Map<String, Value> {
    let mut attrs = Map::new();
    attrs.insert("name".into(), json!(desired.name));
    insert_optional(&mut attrs, "subtitle", desired.subtitle.as_deref());
    insert_optional(
        &mut attrs,
        "privacyPolicyUrl",
        desired.privacy_policy_url.as_deref(),
    );
    insert_optional(
        &mut attrs,
        "privacyChoicesUrl",
        desired.privacy_choices_url.as_deref(),
    );
    insert_optional(
        &mut attrs,
        "privacyPolicyText",
        desired.privacy_policy_text.as_deref(),
    );
    attrs
}

fn version_localization_attrs(desired: &ResolvedVersionLocalization) -> Map<String, Value> {
    let mut attrs = Map::new();
    insert_optional(&mut attrs, "description", desired.description.as_deref());
    insert_optional(&mut attrs, "keywords", desired.keywords.as_deref());
    insert_optional(&mut attrs, "marketingUrl", desired.marketing_url.as_deref());
    insert_optional(
        &mut attrs,
        "promotionalText",
        desired.promotional_text.as_deref(),
    );
    insert_optional(&mut attrs, "supportUrl", desired.support_url.as_deref());
    insert_optional(&mut attrs, "whatsNew", desired.whats_new.as_deref());
    attrs
}

fn review_attrs(desired: &ResolvedReview) -> Map<String, Value> {
    let mut attrs = Map::new();
    insert_optional(
        &mut attrs,
        "contactFirstName",
        desired.contact_first_name.as_deref(),
    );
    insert_optional(
        &mut attrs,
        "contactLastName",
        desired.contact_last_name.as_deref(),
    );
    insert_optional(&mut attrs, "contactPhone", desired.contact_phone.as_deref());
    insert_optional(&mut attrs, "contactEmail", desired.contact_email.as_deref());
    insert_optional(
        &mut attrs,
        "demoAccountName",
        desired.demo_account_name.as_deref(),
    );
    insert_optional(
        &mut attrs,
        "demoAccountPassword",
        desired.demo_account_password.as_deref(),
    );
    if let Some(value) = desired.demo_account_required {
        attrs.insert("demoAccountRequired".into(), json!(value));
    }
    insert_optional(&mut attrs, "notes", desired.notes.as_deref());
    attrs
}

fn file_attrs(file_name: &str, file_size: usize) -> Map<String, Value> {
    let mut attrs = Map::new();
    attrs.insert("fileName".into(), json!(file_name));
    attrs.insert("fileSize".into(), json!(file_size));
    attrs
}

fn uploaded_attrs(checksum: &str) -> Map<String, Value> {
    let mut attrs = Map::new();
    attrs.insert("sourceFileChecksum".into(), json!(checksum));
    attrs.insert("uploaded".into(), json!(true));
    attrs
}

fn map_with(key: &str, value: Value) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(key.into(), value);
    map
}

fn insert_optional(attrs: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        attrs.insert(key.into(), json!(value));
    }
}

fn snake_to_camel(key: &str) -> String {
    let mut output = String::new();
    let mut uppercase_next = false;
    for ch in key.chars() {
        if ch == '_' {
            uppercase_next = true;
            continue;
        }
        if uppercase_next {
            output.extend(ch.to_uppercase());
            uppercase_next = false;
        } else {
            output.push(ch);
        }
    }
    output
}

fn file_name(path: &Path) -> Result<&str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("path {} has no UTF-8 file name", path.display()))
}

fn md5_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Md5::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn uploaded_assets_match<T>(current: &[T], desired_paths: &[PathBuf]) -> Result<bool>
where
    T: UploadedAsset,
{
    if current.len() != desired_paths.len() {
        return Ok(false);
    }
    let mut current = current_asset_signatures(current);
    current.sort_by(|left, right| left.0.cmp(&right.0));

    let mut desired = desired_asset_signatures(desired_paths)?;
    desired.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(current == desired)
}

fn uploaded_assets_match_ordered<T>(current: &[T], desired_paths: &[PathBuf]) -> Result<bool>
where
    T: UploadedAsset,
{
    if current.len() != desired_paths.len() {
        return Ok(false);
    }
    Ok(current_asset_signatures(current) == desired_asset_signatures(desired_paths)?)
}

fn current_asset_signatures<T>(current: &[T]) -> Vec<(String, Option<String>)>
where
    T: UploadedAsset,
{
    current
        .iter()
        .map(|asset| {
            (
                asset.file_name().unwrap_or_default().to_owned(),
                asset.checksum().map(str::to_owned),
            )
        })
        .collect()
}

fn desired_asset_signatures(desired_paths: &[PathBuf]) -> Result<Vec<(String, Option<String>)>> {
    desired_paths
        .iter()
        .map(|path| {
            let bytes =
                fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
            Ok((file_name(path)?.to_owned(), Some(md5_hex(&bytes))))
        })
        .collect()
}

fn preview_mime_type(path: &Path) -> Result<&'static str> {
    match media_validate::lower_extension(path)?.as_str() {
        "mp4" => Ok("video/mp4"),
        "m4v" => Ok("video/x-m4v"),
        "mov" => Ok("video/quicktime"),
        _ => bail!("unsupported app preview extension for {}", path.display()),
    }
}

#[derive(Debug, Deserialize)]
struct JsonApiSingle<T> {
    data: T,
}

#[derive(Debug, Clone, Deserialize)]
struct ResourceIdentifier {
    id: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct Relationships {
    #[serde(default)]
    build: Option<Relationship>,
    #[serde(default, rename = "appStoreVersion")]
    app_store_version: Option<Relationship>,
}

#[derive(Debug, Clone, Deserialize)]
struct Relationship {
    #[serde(default)]
    data: Option<ResourceIdentifier>,
}

#[derive(Debug, Clone, Deserialize)]
struct AppInfo {
    id: String,
    attributes: AppInfoAttributes,
}

#[derive(Debug, Clone, Deserialize)]
struct AppInfoAttributes {
    #[serde(default)]
    state: Option<String>,
}

impl AppInfo {
    fn is_editable(&self) -> bool {
        matches!(
            self.attributes.state.as_deref(),
            Some("PREPARE_FOR_SUBMISSION")
                | Some("DEVELOPER_REJECTED")
                | Some("REJECTED")
                | Some("READY_FOR_REVIEW")
                | Some("WAITING_FOR_REVIEW")
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AgeRatingDeclaration {
    id: String,
    #[serde(default)]
    attributes: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct AppInfoLocalization {
    id: String,
    attributes: AppInfoLocalizationAttributes,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppInfoLocalizationAttributes {
    locale: String,
    name: String,
    #[serde(default)]
    subtitle: Option<String>,
    #[serde(default)]
    privacy_policy_url: Option<String>,
    #[serde(default)]
    privacy_choices_url: Option<String>,
    #[serde(default)]
    privacy_policy_text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct AppStoreVersion {
    id: String,
    attributes: AppStoreVersionAttributes,
    #[serde(default)]
    relationships: Option<Relationships>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppStoreVersionAttributes {
    #[serde(default)]
    app_store_state: Option<String>,
    #[serde(default)]
    app_version_state: Option<String>,
    #[serde(default)]
    copyright: Option<String>,
    #[serde(default)]
    release_type: Option<String>,
    #[serde(default)]
    earliest_release_date: Option<String>,
}

impl AppStoreVersion {
    fn planned(platform: AppPlatform, version_string: &str) -> Self {
        Self {
            id: format!(
                "planned-app-store-version-{}-{version_string}",
                platform.asc_value()
            ),
            attributes: AppStoreVersionAttributes {
                app_store_state: Some("PREPARE_FOR_SUBMISSION".into()),
                app_version_state: Some("PREPARE_FOR_SUBMISSION".into()),
                copyright: None,
                release_type: None,
                earliest_release_date: None,
            },
            relationships: None,
        }
    }

    fn is_editable(&self) -> bool {
        let app_store_state = self.attributes.app_store_state.as_deref();
        let app_version_state = self.attributes.app_version_state.as_deref();
        !matches!(
            app_store_state,
            Some("READY_FOR_SALE")
                | Some("PREORDER_READY_FOR_SALE")
                | Some("DEVELOPER_REMOVED_FROM_SALE")
                | Some("REMOVED_FROM_SALE")
                | Some("REPLACED_WITH_NEW_VERSION")
        ) && !matches!(
            app_version_state,
            Some("READY_FOR_DISTRIBUTION") | Some("REPLACED_WITH_NEW_VERSION")
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Build {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AppStoreVersionLocalization {
    id: String,
    attributes: AppStoreVersionLocalizationAttributes,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppStoreVersionLocalizationAttributes {
    locale: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    keywords: Option<String>,
    #[serde(default)]
    marketing_url: Option<String>,
    #[serde(default)]
    promotional_text: Option<String>,
    #[serde(default)]
    support_url: Option<String>,
    #[serde(default)]
    whats_new: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct AppStoreReviewDetail {
    id: String,
    attributes: AppStoreReviewDetailAttributes,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppStoreReviewDetailAttributes {
    #[serde(default)]
    contact_first_name: Option<String>,
    #[serde(default)]
    contact_last_name: Option<String>,
    #[serde(default)]
    contact_phone: Option<String>,
    #[serde(default)]
    contact_email: Option<String>,
    #[serde(default)]
    demo_account_name: Option<String>,
    #[serde(default)]
    demo_account_password: Option<String>,
    #[serde(default)]
    demo_account_required: Option<bool>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct AppScreenshotSet {
    id: String,
    attributes: AppScreenshotSetAttributes,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppScreenshotSetAttributes {
    screenshot_display_type: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AppPreviewSet {
    id: String,
    attributes: AppPreviewSetAttributes,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppPreviewSetAttributes {
    preview_type: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AppScreenshot {
    id: String,
    attributes: MediaAssetAttributes,
}

#[derive(Debug, Clone, Deserialize)]
struct AppPreview {
    id: String,
    attributes: MediaAssetAttributes,
}

#[derive(Debug, Clone, Deserialize)]
struct AppStoreReviewAttachment {
    id: String,
    attributes: MediaAssetAttributes,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaAssetAttributes {
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    file_size: Option<u64>,
    #[serde(default)]
    source_file_checksum: Option<String>,
    #[serde(default)]
    upload_operations: Option<Vec<UploadOperation>>,
    #[serde(default)]
    asset_delivery_state: Option<DeliveryState>,
    #[serde(default)]
    video_delivery_state: Option<DeliveryState>,
    #[serde(default)]
    app_event_asset_type: Option<String>,
}

impl MediaAssetAttributes {
    fn delivery_state_complete(&self) -> bool {
        self.asset_delivery_state
            .as_ref()
            .is_none_or(DeliveryState::is_complete)
    }

    fn video_state_complete(&self) -> bool {
        self.video_delivery_state
            .as_ref()
            .is_none_or(DeliveryState::is_complete)
    }
}

trait UploadedAsset {
    fn file_name(&self) -> Option<&str>;
    fn checksum(&self) -> Option<&str>;
}

impl UploadedAsset for AppScreenshot {
    fn file_name(&self) -> Option<&str> {
        self.attributes.file_name.as_deref()
    }

    fn checksum(&self) -> Option<&str> {
        self.attributes.source_file_checksum.as_deref()
    }
}

impl UploadedAsset for AppPreview {
    fn file_name(&self) -> Option<&str> {
        self.attributes.file_name.as_deref()
    }

    fn checksum(&self) -> Option<&str> {
        self.attributes.source_file_checksum.as_deref()
    }
}

impl UploadedAsset for AppStoreReviewAttachment {
    fn file_name(&self) -> Option<&str> {
        self.attributes.file_name.as_deref()
    }

    fn checksum(&self) -> Option<&str> {
        self.attributes.source_file_checksum.as_deref()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeliveryState {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    errors: Option<Vec<Value>>,
}

impl DeliveryState {
    fn is_complete(&self) -> bool {
        if self
            .errors
            .as_ref()
            .is_some_and(|errors| !errors.is_empty())
        {
            return false;
        }
        matches!(
            self.state.as_deref(),
            None | Some("COMPLETE") | Some("UPLOADED") | Some("READY")
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadOperation {
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    length: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    request_headers: Vec<HttpHeader>,
}

impl UploadOperation {
    fn headers(&self) -> Vec<(String, String)> {
        self.request_headers
            .iter()
            .filter_map(|header| Some((header.name.clone()?, header.value.clone()?)))
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct HttpHeader {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewSubmission {
    id: String,
    attributes: ReviewSubmissionAttributes,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewSubmissionAttributes {
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewSubmissionItem {
    #[serde(default)]
    relationships: Option<Relationships>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Map, json};

    use super::{AgeRatingDeclaration, age_rating_matches, category_matches, md5_hex};

    #[test]
    fn asset_checksum_uses_apple_required_md5_hex() {
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn category_match_ignores_unspecified_fields() {
        assert!(category_matches(None, Some("GAMES".into())));
        assert!(category_matches(Some("GAMES"), Some("GAMES".into())));
        assert!(!category_matches(Some("GAMES"), Some("BUSINESS".into())));
    }

    #[test]
    fn age_rating_match_compares_configured_fields_only() {
        let mut current = Map::new();
        current.insert("alcoholTobaccoOrDrugUseOrReferences".into(), json!("NONE"));
        current.insert("gambling".into(), json!(false));
        let declaration = AgeRatingDeclaration {
            id: "age-rating".into(),
            attributes: current,
        };
        let desired = [
            (
                "alcohol_tobacco_or_drug_use_or_references".into(),
                json!("NONE"),
            ),
            ("gambling".into(), json!(false)),
        ]
        .into_iter()
        .collect();

        assert!(age_rating_matches(&declaration, &desired));
    }
}
