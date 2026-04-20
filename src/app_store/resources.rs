use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, ensure};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    App, AppPreviewSet, AppScreenshotSet, AppStoreApi, AppStoreSync, JsonApiSingle,
    MediaAssetAttributes, ResolveContext, ResourceIdentifier, UploadedAsset, file_attrs, file_name,
    md5_hex, safe_path_segment, uploaded_assets_match, uploaded_assets_match_ordered,
    uploaded_attrs,
};
use crate::{
    asc::asc_endpoint,
    config::{
        AppAvailabilitySpec, AppEventLocalizationSource, AppEventLocalizationSpec,
        AppEventMediaSpec, AppEventSpec, AppPrivacySpec, AppSpec, CommerceAvailabilitySpec,
        CommerceLocalizationSource, CommerceLocalizationSpec, CommercePricingSpec,
        CommerceReviewAssetSpec, CustomProductPageLocalizationSource, CustomProductPageSpec,
        InAppPurchaseSpec, PriceScheduleEntrySpec, StringSource,
        SubscriptionGroupLocalizationSource, SubscriptionGroupLocalizationSpec,
        SubscriptionGroupSpec, SubscriptionSpec, TerritorySelectionMode, TerritorySelectionSpec,
    },
    media_validate,
    sync::{ChangeKind, Mode},
};

impl<'a> AppStoreSync<'a> {
    pub(super) fn reconcile_resource_families(
        &mut self,
        app_key: &str,
        app_record: &App,
        app: &AppSpec,
    ) -> Result<()> {
        if let Some(availability) = &app.availability {
            self.reconcile_app_availability(app_key, app_record, availability)?;
        }
        if let Some(pricing) = &app.pricing {
            self.reconcile_app_pricing(app_key, app_record, pricing)?;
        }
        for (page_key, page) in &app.custom_product_pages {
            self.reconcile_custom_product_page(app_key, app_record, page_key, page)?;
        }
        for (iap_key, iap) in &app.in_app_purchases {
            self.reconcile_in_app_purchase(app_key, app_record, iap_key, iap)?;
        }
        for (group_key, group) in &app.subscription_groups {
            self.reconcile_subscription_group(app_key, app_record, group_key, group)?;
        }
        for (event_key, event) in &app.app_events {
            self.reconcile_app_event(app_key, app_record, event_key, event)?;
        }
        if let Some(privacy) = &app.privacy {
            self.reconcile_privacy_checklist(app_key, privacy);
        }
        Ok(())
    }

    fn reconcile_app_availability(
        &mut self,
        app_key: &str,
        app_record: &App,
        availability: &AppAvailabilitySpec,
    ) -> Result<()> {
        let Some(selection) = &availability.territories else {
            self.record_manual(
                format!("app.{app_key}.availability"),
                "pre-order settings require App Store Connect review state; verify them manually"
                    .into(),
            );
            return Ok(());
        };
        let current = self.api.get_app_availability(&app_record.id)?;
        if current.is_none() {
            self.record(
                ChangeKind::Create,
                format!("app.{app_key}.availability"),
                "create App Availability V2 from configured territories".into(),
            );
            if self.mode == Mode::Apply {
                let territories = self
                    .desired_available_territories(selection)?
                    .into_iter()
                    .collect::<Vec<_>>();
                self.api.create_app_availability(
                    &app_record.id,
                    availability.available_in_new_territories.unwrap_or(true),
                    &territories,
                )?;
            }
            if availability.pre_order.is_some() {
                self.record_manual(
                    format!("app.{app_key}.availability.pre_order"),
                    "pre-order settings are review-sensitive; verify/apply them manually in App Store Connect".into(),
                );
            }
            return Ok(());
        }

        self.record_manual(
            format!("app.{app_key}.availability"),
            "existing app availability is not changed automatically; territory removals and pre-order changes are review-sensitive".into(),
        );
        Ok(())
    }

    fn reconcile_app_pricing(
        &mut self,
        app_key: &str,
        app_record: &App,
        pricing: &CommercePricingSpec,
    ) -> Result<()> {
        let desired = self.resolve_app_price_schedule(app_record, pricing)?;
        let current = self.api.get_app_price_schedule(&app_record.id)?;
        if current.is_some_and(|current| current.matches(&desired)) {
            return Ok(());
        }
        self.record(
            ChangeKind::Replace,
            format!("app.{app_key}.pricing"),
            "replace app price schedule".into(),
        );
        if self.mode == Mode::Apply {
            ensure!(
                pricing.replace_future_schedule,
                "app {app_key} pricing differs; set pricing.replace_future_schedule=true to replace the ASC price schedule"
            );
            self.api
                .create_app_price_schedule(&app_record.id, &desired)?;
        }
        Ok(())
    }

    fn reconcile_custom_product_page(
        &mut self,
        app_key: &str,
        app_record: &App,
        page_key: &str,
        page: &CustomProductPageSpec,
    ) -> Result<()> {
        let desired = self.context.resolve_custom_product_page(page)?;
        let page_record = match self.api.find_custom_product_page(
            &app_record.id,
            page.asc_id.as_deref(),
            &desired.name,
        )? {
            Some(existing) => {
                let mut attrs = Map::new();
                if existing.attr_str("name") != Some(desired.name.as_str()) {
                    attrs.insert("name".into(), json!(desired.name));
                }
                if desired.visible.is_some() && existing.attr_bool("visible") != desired.visible {
                    attrs.insert("visible".into(), json!(desired.visible));
                }
                if !attrs.is_empty() {
                    self.record(
                        ChangeKind::Update,
                        format!("app.{app_key}.custom_product_pages.{page_key}"),
                        "ensure custom product page attributes match config".into(),
                    );
                    if self.mode == Mode::Apply {
                        self.api.update_custom_product_page(&existing.id, attrs)?;
                    }
                }
                existing
            }
            None => {
                self.record(
                    ChangeKind::Create,
                    format!("app.{app_key}.custom_product_pages.{page_key}"),
                    "create custom product page".into(),
                );
                if self.mode == Mode::Apply {
                    self.api
                        .create_custom_product_page(&app_record.id, &desired.name)?
                } else {
                    RemoteResource::planned("planned-custom-product-page", "appCustomProductPages")
                }
            }
        };

        if self.mode == Mode::Plan && page_record.is_planned() {
            self.record_custom_product_page_children(
                app_key,
                page_key,
                &desired.localizations,
                &page.media,
            );
            return Ok(());
        }

        let version = self.ensure_custom_product_page_version(
            app_key,
            page_key,
            &page_record.id,
            desired.deep_link.as_deref(),
        )?;
        self.reconcile_custom_product_page_localizations(
            app_key,
            page_key,
            &version.id,
            &page.localizations,
            &desired.localizations,
            &page.media,
        )
    }

    fn record_custom_product_page_children(
        &mut self,
        app_key: &str,
        page_key: &str,
        localizations: &BTreeMap<String, ResolvedCustomProductPageLocalization>,
        media_by_locale: &BTreeMap<String, crate::config::AppMediaLocalizationSpec>,
    ) {
        self.record(
            ChangeKind::Create,
            format!("app.{app_key}.custom_product_pages.{page_key}.version"),
            "create editable custom product page version".into(),
        );
        for locale in localizations.keys() {
            self.record(
                ChangeKind::Create,
                format!("app.{app_key}.custom_product_pages.{page_key}.localizations.{locale}"),
                "create custom product page localization".into(),
            );
        }
        for locale in media_by_locale.keys() {
            self.record(
                ChangeKind::Replace,
                format!("app.{app_key}.custom_product_pages.{page_key}.media.{locale}"),
                "replace custom product page media".into(),
            );
        }
    }

    fn ensure_custom_product_page_version(
        &mut self,
        app_key: &str,
        page_key: &str,
        page_id: &str,
        deep_link: Option<&str>,
    ) -> Result<RemoteResource> {
        let versions = self.api.list_custom_product_page_versions(page_id)?;
        let version = versions
            .iter()
            .find(|version| version.is_editable_custom_product_page_version())
            .cloned();
        let version = match version {
            Some(version) => version,
            None => {
                self.record(
                    ChangeKind::Create,
                    format!("app.{app_key}.custom_product_pages.{page_key}.version"),
                    "create editable custom product page version".into(),
                );
                if self.mode == Mode::Apply {
                    self.api
                        .create_custom_product_page_version(page_id, deep_link)?
                } else {
                    RemoteResource::planned(
                        "planned-custom-product-page-version",
                        "appCustomProductPageVersions",
                    )
                }
            }
        };
        if !version.is_planned() && deep_link != version.attr_str("deepLink") {
            self.record(
                ChangeKind::Update,
                format!("app.{app_key}.custom_product_pages.{page_key}.version"),
                "ensure custom product page deep link matches config".into(),
            );
            if self.mode == Mode::Apply {
                self.api
                    .update_custom_product_page_version(&version.id, deep_link)?;
            }
        }
        Ok(version)
    }

    fn reconcile_custom_product_page_localizations(
        &mut self,
        app_key: &str,
        page_key: &str,
        version_id: &str,
        localization_sources: &BTreeMap<String, CustomProductPageLocalizationSource>,
        localizations: &BTreeMap<String, ResolvedCustomProductPageLocalization>,
        media_by_locale: &BTreeMap<String, crate::config::AppMediaLocalizationSpec>,
    ) -> Result<()> {
        let current = self
            .api
            .list_custom_product_page_localizations(version_id)?
            .into_iter()
            .map(|localization| {
                (
                    localization
                        .attr_str("locale")
                        .unwrap_or_default()
                        .to_owned(),
                    localization,
                )
            })
            .collect::<BTreeMap<_, _>>();

        for (locale, desired) in localizations {
            let existing = current.get(locale);
            let mut reconciled_localization = existing.cloned();
            if existing.is_some_and(|existing| {
                optional_matches(
                    desired.promotional_text.as_deref(),
                    existing.attr_str("promotionalText"),
                )
            }) {
                // Media and keyword relationships still need separate reconciliation below.
            } else {
                self.record(
                    if existing.is_some() {
                        ChangeKind::Update
                    } else {
                        ChangeKind::Create
                    },
                    format!("app.{app_key}.custom_product_pages.{page_key}.localizations.{locale}"),
                    "ensure custom product page localization matches config".into(),
                );
                if self.mode == Mode::Apply {
                    reconciled_localization = Some(if let Some(existing) = existing {
                        self.api.update_custom_product_page_localization(
                            &existing.id,
                            desired.promotional_text.as_deref(),
                        )?
                    } else {
                        self.api.create_custom_product_page_localization(
                            version_id,
                            locale,
                            desired.promotional_text.as_deref(),
                        )?
                    });
                }
            }

            if let Some(existing) = reconciled_localization.as_ref() {
                self.reconcile_app_keywords(
                    app_key,
                    &format!("custom_product_pages.{page_key}.localizations.{locale}"),
                    &existing.id,
                    &desired.search_keyword_ids,
                )?;
                if let Some(media) = media_by_locale.get(locale) {
                    let localization_source =
                        localization_sources.get(locale).ok_or_else(|| {
                            anyhow::anyhow!(
                                "missing custom product page localization source for {locale}"
                            )
                        })?;
                    self.reconcile_custom_product_page_media(
                        app_key,
                        page_key,
                        locale,
                        &existing.id,
                        localization_source,
                        media,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn reconcile_app_keywords(
        &mut self,
        app_key: &str,
        subject: &str,
        localization_id: &str,
        desired: &[String],
    ) -> Result<()> {
        if desired.is_empty() {
            return Ok(());
        }
        let current = self
            .api
            .list_related_ids(&format!(
                "/appCustomProductPageLocalizations/{localization_id}/relationships/searchKeywords"
            ))?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let missing = desired
            .iter()
            .filter(|id| !current.contains(id.as_str()))
            .map(String::as_str)
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(());
        }
        self.record(
            ChangeKind::Update,
            format!("app.{app_key}.{subject}.search_keyword_ids"),
            "attach missing app keyword relationships".into(),
        );
        if self.mode == Mode::Apply {
            self.api
                .add_app_keyword_relationships(localization_id, &missing)?;
        }
        Ok(())
    }

    fn reconcile_custom_product_page_media(
        &mut self,
        app_key: &str,
        page_key: &str,
        locale: &str,
        localization_id: &str,
        localization_source: &CustomProductPageLocalizationSource,
        media: &crate::config::AppMediaLocalizationSpec,
    ) -> Result<()> {
        for (set, source) in &media.screenshots {
            let output_dir = self
                .media_render_dir
                .path()
                .join(safe_path_segment(app_key))
                .join("custom_product_pages")
                .join(safe_path_segment(page_key))
                .join(safe_path_segment(locale))
                .join(set.config_key());
            let files = self.context.resolve_custom_product_page_screenshot_source(
                locale,
                localization_source,
                *set,
                source,
                &output_dir,
            )?;
            media_validate::validate_screenshots(*set, &files)?;
            let display_type = set.asc_display_type();
            let existing = self.api.find_screenshot_set_for_localization(
                "appCustomProductPageLocalizations",
                localization_id,
                display_type,
            )?;
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
                    "app.{app_key}.custom_product_pages.{page_key}.media.{locale}.screenshots.{display_type}"
                ),
                "replace custom product page screenshot set".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(existing) = existing {
                    self.api.delete_screenshot_set(&existing.id)?;
                }
                let set = self.api.create_screenshot_set_for_relationship(
                    "appCustomProductPageLocalization",
                    "appCustomProductPageLocalizations",
                    localization_id,
                    display_type,
                )?;
                for file in files {
                    self.api.upload_screenshot(&set.id, &file)?;
                }
            }
        }
        for (set, paths) in &media.app_previews {
            let files = self.context.resolve_paths(paths)?;
            media_validate::validate_previews(*set, &files)?;
            let preview_type = set.asc_preview_type();
            let existing = self.api.find_preview_set_for_localization(
                "appCustomProductPageLocalizations",
                localization_id,
                preview_type,
            )?;
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
                    "app.{app_key}.custom_product_pages.{page_key}.media.{locale}.app_previews.{preview_type}"
                ),
                "replace custom product page app preview set".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(existing) = existing {
                    self.api.delete_preview_set(&existing.id)?;
                }
                let set = self.api.create_preview_set_for_relationship(
                    "appCustomProductPageLocalization",
                    "appCustomProductPageLocalizations",
                    localization_id,
                    preview_type,
                )?;
                for file in files {
                    self.api.upload_preview(&set.id, &file)?;
                }
            }
        }
        Ok(())
    }

    fn reconcile_in_app_purchase(
        &mut self,
        app_key: &str,
        app_record: &App,
        iap_key: &str,
        iap: &InAppPurchaseSpec,
    ) -> Result<()> {
        let desired = self.context.resolve_iap(iap)?;
        let iap_record =
            match self
                .api
                .find_iap(&app_record.id, iap.asc_id.as_deref(), &iap.product_id)?
            {
                Some(existing) => {
                    ensure_immutable(
                        existing.attr_str("productId"),
                        Some(iap.product_id.as_str()),
                        &format!("app.{app_key}.in_app_purchases.{iap_key}.product_id"),
                    )?;
                    ensure_immutable(
                        existing.attr_str("inAppPurchaseType"),
                        Some(iap.kind.asc_value()),
                        &format!("app.{app_key}.in_app_purchases.{iap_key}.type"),
                    )?;
                    let mut attrs = Map::new();
                    compare_value_attr(
                        &mut attrs,
                        "name",
                        Some(json!(desired.reference_name)),
                        existing.attr("name"),
                    );
                    compare_optional_string_value(
                        &mut attrs,
                        "reviewNote",
                        desired.review_note.as_deref(),
                        existing.attr_str("reviewNote"),
                    );
                    compare_optional_bool_value(
                        &mut attrs,
                        "familySharable",
                        iap.family_sharable,
                        existing.attr_bool("familySharable"),
                    );
                    if !attrs.is_empty() {
                        self.record(
                            ChangeKind::Update,
                            format!("app.{app_key}.in_app_purchases.{iap_key}"),
                            "ensure in-app purchase attributes match config".into(),
                        );
                        if self.mode == Mode::Apply {
                            self.api.update_iap(&existing.id, attrs)?;
                        }
                    }
                    existing
                }
                None => {
                    self.record(
                        ChangeKind::Create,
                        format!("app.{app_key}.in_app_purchases.{iap_key}"),
                        "create in-app purchase".into(),
                    );
                    if self.mode == Mode::Apply {
                        self.api.create_iap(&app_record.id, iap, &desired)?
                    } else {
                        RemoteResource::planned("planned-iap", "inAppPurchases")
                    }
                }
            };

        if iap_record.is_planned() {
            self.record_commerce_children(
                app_key,
                &format!("in_app_purchases.{iap_key}"),
                &desired.localizations,
                iap.pricing.as_ref(),
                iap.availability.as_ref(),
                iap.review.as_ref(),
            );
            return Ok(());
        }
        self.reconcile_commerce_localizations(
            app_key,
            &format!("in_app_purchases.{iap_key}"),
            &iap_record.id,
            CommerceKind::InAppPurchase,
            &desired.localizations,
        )?;
        self.reconcile_commerce_review_asset(
            app_key,
            &format!("in_app_purchases.{iap_key}"),
            &iap_record.id,
            CommerceKind::InAppPurchase,
            iap.review.as_ref(),
        )?;
        self.reconcile_commerce_followups(
            app_key,
            &format!("in_app_purchases.{iap_key}"),
            iap.pricing.as_ref(),
            iap.availability.as_ref(),
        );
        Ok(())
    }

    fn reconcile_subscription_group(
        &mut self,
        app_key: &str,
        app_record: &App,
        group_key: &str,
        group: &SubscriptionGroupSpec,
    ) -> Result<()> {
        let desired = self.context.resolve_subscription_group(group)?;
        let group_record = match self.api.find_subscription_group(
            &app_record.id,
            group.asc_id.as_deref(),
            &desired.reference_name,
        )? {
            Some(existing) => {
                if existing.attr_str("referenceName") != Some(desired.reference_name.as_str()) {
                    self.record(
                        ChangeKind::Update,
                        format!("app.{app_key}.subscription_groups.{group_key}"),
                        "ensure subscription group reference name matches config".into(),
                    );
                    if self.mode == Mode::Apply {
                        self.api.update_subscription_group(
                            &existing.id,
                            map_with("referenceName", json!(desired.reference_name)),
                        )?;
                    }
                }
                existing
            }
            None => {
                self.record(
                    ChangeKind::Create,
                    format!("app.{app_key}.subscription_groups.{group_key}"),
                    "create subscription group".into(),
                );
                if self.mode == Mode::Apply {
                    self.api
                        .create_subscription_group(&app_record.id, &desired.reference_name)?
                } else {
                    RemoteResource::planned("planned-subscription-group", "subscriptionGroups")
                }
            }
        };
        if group_record.is_planned() {
            for locale in desired.localizations.keys() {
                self.record(
                    ChangeKind::Create,
                    format!("app.{app_key}.subscription_groups.{group_key}.localizations.{locale}"),
                    "create subscription group localization".into(),
                );
            }
            for subscription_key in group.subscriptions.keys() {
                self.record(
                    ChangeKind::Create,
                    format!(
                        "app.{app_key}.subscription_groups.{group_key}.subscriptions.{subscription_key}"
                    ),
                    "create subscription".into(),
                );
            }
            return Ok(());
        }

        self.reconcile_subscription_group_localizations(
            app_key,
            group_key,
            &group_record.id,
            &desired.localizations,
        )?;
        for (subscription_key, subscription) in &group.subscriptions {
            self.reconcile_subscription(
                app_key,
                group_key,
                subscription_key,
                &group_record.id,
                subscription,
            )?;
        }
        Ok(())
    }

    fn reconcile_subscription_group_localizations(
        &mut self,
        app_key: &str,
        group_key: &str,
        group_id: &str,
        localizations: &BTreeMap<String, ResolvedSubscriptionGroupLocalization>,
    ) -> Result<()> {
        let current = self
            .api
            .list_subscription_group_localizations(group_id)?
            .into_iter()
            .map(|localization| {
                (
                    localization
                        .attr_str("locale")
                        .unwrap_or_default()
                        .to_owned(),
                    localization,
                )
            })
            .collect::<BTreeMap<_, _>>();
        for (locale, desired) in localizations {
            let existing = current.get(locale);
            if existing.is_some_and(|existing| desired.matches(existing)) {
                continue;
            }
            self.record(
                if existing.is_some() {
                    ChangeKind::Update
                } else {
                    ChangeKind::Create
                },
                format!("app.{app_key}.subscription_groups.{group_key}.localizations.{locale}"),
                "ensure subscription group localization matches config".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(existing) = existing {
                    self.api
                        .update_subscription_group_localization(&existing.id, desired)?;
                } else {
                    self.api
                        .create_subscription_group_localization(group_id, locale, desired)?;
                }
            }
        }
        Ok(())
    }

    fn reconcile_subscription(
        &mut self,
        app_key: &str,
        group_key: &str,
        subscription_key: &str,
        group_id: &str,
        subscription: &SubscriptionSpec,
    ) -> Result<()> {
        let desired = self.context.resolve_subscription(subscription)?;
        let current = self.api.find_subscription(
            group_id,
            subscription.asc_id.as_deref(),
            &subscription.product_id,
        )?;
        let subscription_record = match current {
            Some(existing) => {
                ensure_immutable(
                    existing.attr_str("productId"),
                    Some(subscription.product_id.as_str()),
                    &format!(
                        "app.{app_key}.subscription_groups.{group_key}.subscriptions.{subscription_key}.product_id"
                    ),
                )?;
                let mut attrs = Map::new();
                compare_value_attr(
                    &mut attrs,
                    "name",
                    Some(json!(desired.reference_name)),
                    existing.attr("name"),
                );
                compare_optional_string_value(
                    &mut attrs,
                    "reviewNote",
                    desired.review_note.as_deref(),
                    existing.attr_str("reviewNote"),
                );
                compare_optional_bool_value(
                    &mut attrs,
                    "familySharable",
                    subscription.family_sharable,
                    existing.attr_bool("familySharable"),
                );
                if let Some(period) = subscription.period {
                    compare_value_attr(
                        &mut attrs,
                        "subscriptionPeriod",
                        Some(json!(period.asc_value())),
                        existing.attr("subscriptionPeriod"),
                    );
                }
                if let Some(group_level) = subscription.group_level {
                    compare_value_attr(
                        &mut attrs,
                        "groupLevel",
                        Some(json!(group_level)),
                        existing.attr("groupLevel"),
                    );
                }
                if !attrs.is_empty() {
                    self.record(
                        ChangeKind::Update,
                        format!(
                            "app.{app_key}.subscription_groups.{group_key}.subscriptions.{subscription_key}"
                        ),
                        "ensure subscription attributes match config".into(),
                    );
                    if self.mode == Mode::Apply {
                        self.api.update_subscription(&existing.id, attrs)?;
                    }
                }
                existing
            }
            None => {
                self.record(
                    ChangeKind::Create,
                    format!(
                        "app.{app_key}.subscription_groups.{group_key}.subscriptions.{subscription_key}"
                    ),
                    "create subscription".into(),
                );
                if self.mode == Mode::Apply {
                    self.api
                        .create_subscription(group_id, subscription, &desired)?
                } else {
                    RemoteResource::planned("planned-subscription", "subscriptions")
                }
            }
        };
        if subscription_record.is_planned() {
            self.record_commerce_children(
                app_key,
                &format!("subscription_groups.{group_key}.subscriptions.{subscription_key}"),
                &desired.localizations,
                subscription.pricing.as_ref(),
                subscription.availability.as_ref(),
                subscription.review.as_ref(),
            );
            return Ok(());
        }
        self.reconcile_commerce_localizations(
            app_key,
            &format!("subscription_groups.{group_key}.subscriptions.{subscription_key}"),
            &subscription_record.id,
            CommerceKind::Subscription,
            &desired.localizations,
        )?;
        self.reconcile_commerce_review_asset(
            app_key,
            &format!("subscription_groups.{group_key}.subscriptions.{subscription_key}"),
            &subscription_record.id,
            CommerceKind::Subscription,
            subscription.review.as_ref(),
        )?;
        self.reconcile_commerce_followups(
            app_key,
            &format!("subscription_groups.{group_key}.subscriptions.{subscription_key}"),
            subscription.pricing.as_ref(),
            subscription.availability.as_ref(),
        );
        Ok(())
    }

    fn record_commerce_children(
        &mut self,
        app_key: &str,
        subject: &str,
        localizations: &BTreeMap<String, ResolvedCommerceLocalization>,
        pricing: Option<&CommercePricingSpec>,
        availability: Option<&CommerceAvailabilitySpec>,
        review: Option<&CommerceReviewAssetSpec>,
    ) {
        for locale in localizations.keys() {
            self.record(
                ChangeKind::Create,
                format!("app.{app_key}.{subject}.localizations.{locale}"),
                "create commerce localization".into(),
            );
        }
        self.reconcile_commerce_followups(app_key, subject, pricing, availability);
        if review
            .as_ref()
            .and_then(|review| review.screenshot.as_ref())
            .is_some()
        {
            self.record(
                ChangeKind::Replace,
                format!("app.{app_key}.{subject}.review.screenshot"),
                "replace review screenshot".into(),
            );
        }
    }

    fn reconcile_commerce_localizations(
        &mut self,
        app_key: &str,
        subject: &str,
        resource_id: &str,
        kind: CommerceKind,
        localizations: &BTreeMap<String, ResolvedCommerceLocalization>,
    ) -> Result<()> {
        let current = self
            .api
            .list_commerce_localizations(resource_id, kind)?
            .into_iter()
            .map(|localization| {
                (
                    localization
                        .attr_str("locale")
                        .unwrap_or_default()
                        .to_owned(),
                    localization,
                )
            })
            .collect::<BTreeMap<_, _>>();
        for (locale, desired) in localizations {
            let existing = current.get(locale);
            if existing.is_some_and(|existing| desired.matches(existing)) {
                continue;
            }
            self.record(
                if existing.is_some() {
                    ChangeKind::Update
                } else {
                    ChangeKind::Create
                },
                format!("app.{app_key}.{subject}.localizations.{locale}"),
                "ensure commerce localization matches config".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(existing) = existing {
                    self.api
                        .update_commerce_localization(&existing.id, kind, desired)?;
                } else {
                    self.api
                        .create_commerce_localization(resource_id, kind, locale, desired)?;
                }
            }
        }
        Ok(())
    }

    fn reconcile_commerce_review_asset(
        &mut self,
        app_key: &str,
        subject: &str,
        resource_id: &str,
        kind: CommerceKind,
        review: Option<&CommerceReviewAssetSpec>,
    ) -> Result<()> {
        let Some(path) = review.and_then(|review| review.screenshot.as_ref()) else {
            return Ok(());
        };
        let path = self.context.resolve_path(path);
        media_validate::validate_image_file(&path)?;
        let current = self.api.get_commerce_review_screenshot(resource_id, kind)?;
        if current.as_ref().is_some_and(|asset| {
            uploaded_assets_match(std::slice::from_ref(asset), std::slice::from_ref(&path))
                .unwrap_or(false)
        }) {
            return Ok(());
        }
        self.record(
            ChangeKind::Replace,
            format!("app.{app_key}.{subject}.review.screenshot"),
            "replace review screenshot".into(),
        );
        if self.mode == Mode::Apply {
            if let Some(current) = current {
                self.api
                    .delete_commerce_review_screenshot(&current.id, kind)?;
            }
            self.api
                .upload_commerce_review_screenshot(resource_id, kind, &path)?;
        }
        Ok(())
    }

    fn reconcile_commerce_followups(
        &mut self,
        app_key: &str,
        subject: &str,
        pricing: Option<&CommercePricingSpec>,
        availability: Option<&CommerceAvailabilitySpec>,
    ) {
        if pricing.is_some() {
            self.record_manual(
                format!("app.{app_key}.{subject}.pricing"),
                "commerce price schedules are typed in config but must be reviewed/applied separately from metadata sync".into(),
            );
        }
        if availability.is_some() {
            self.record_manual(
                format!("app.{app_key}.{subject}.availability"),
                "commerce availability is typed in config but existing availability changes are review-sensitive; verify/apply manually".into(),
            );
        }
    }

    fn reconcile_app_event(
        &mut self,
        app_key: &str,
        app_record: &App,
        event_key: &str,
        event: &AppEventSpec,
    ) -> Result<()> {
        let desired = self.context.resolve_app_event(event)?;
        let event_record = match self.api.find_app_event(
            &app_record.id,
            event.asc_id.as_deref(),
            &desired.reference_name,
        )? {
            Some(existing) => {
                let mut attrs = app_event_attrs(&desired);
                attrs.retain(|key, desired| existing.attr(key) != Some(desired));
                if !attrs.is_empty() {
                    self.record(
                        ChangeKind::Update,
                        format!("app.{app_key}.app_events.{event_key}"),
                        "ensure app event attributes match config".into(),
                    );
                    if self.mode == Mode::Apply {
                        self.api.update_app_event(&existing.id, attrs)?;
                    }
                }
                existing
            }
            None => {
                self.record(
                    ChangeKind::Create,
                    format!("app.{app_key}.app_events.{event_key}"),
                    "create app event".into(),
                );
                if self.mode == Mode::Apply {
                    self.api.create_app_event(&app_record.id, &desired)?
                } else {
                    RemoteResource::planned("planned-app-event", "appEvents")
                }
            }
        };
        if event_record.is_planned() {
            for locale in desired.localizations.keys() {
                self.record(
                    ChangeKind::Create,
                    format!("app.{app_key}.app_events.{event_key}.localizations.{locale}"),
                    "create app event localization".into(),
                );
            }
            for locale in event.media.keys() {
                self.record(
                    ChangeKind::Replace,
                    format!("app.{app_key}.app_events.{event_key}.media.{locale}"),
                    "replace app event media".into(),
                );
            }
            return Ok(());
        }
        self.reconcile_app_event_localizations(
            app_key,
            event_key,
            &event_record.id,
            &desired.localizations,
            &event.media,
        )
    }

    fn reconcile_app_event_localizations(
        &mut self,
        app_key: &str,
        event_key: &str,
        event_id: &str,
        localizations: &BTreeMap<String, ResolvedAppEventLocalization>,
        media_by_locale: &BTreeMap<String, AppEventMediaSpec>,
    ) -> Result<()> {
        let current = self
            .api
            .list_app_event_localizations(event_id)?
            .into_iter()
            .map(|localization| {
                (
                    localization
                        .attr_str("locale")
                        .unwrap_or_default()
                        .to_owned(),
                    localization,
                )
            })
            .collect::<BTreeMap<_, _>>();
        for (locale, desired) in localizations {
            let existing = current.get(locale);
            let mut reconciled_localization = existing.cloned();
            if existing.is_some_and(|existing| desired.matches(existing)) {
                if let Some(media) = media_by_locale.get(locale) {
                    self.reconcile_app_event_media(
                        app_key,
                        event_key,
                        locale,
                        &existing.expect("checked by is_some_and above").id,
                        media,
                    )?;
                }
                continue;
            }
            self.record(
                if existing.is_some() {
                    ChangeKind::Update
                } else {
                    ChangeKind::Create
                },
                format!("app.{app_key}.app_events.{event_key}.localizations.{locale}"),
                "ensure app event localization matches config".into(),
            );
            if self.mode == Mode::Apply {
                reconciled_localization = Some(if let Some(existing) = existing {
                    self.api
                        .update_app_event_localization(&existing.id, desired)?
                } else {
                    self.api
                        .create_app_event_localization(event_id, locale, desired)?
                });
            }
            if let Some(existing) = reconciled_localization.as_ref()
                && let Some(media) = media_by_locale.get(locale)
            {
                self.reconcile_app_event_media(app_key, event_key, locale, &existing.id, media)?;
            }
        }
        Ok(())
    }

    fn reconcile_app_event_media(
        &mut self,
        app_key: &str,
        event_key: &str,
        locale: &str,
        localization_id: &str,
        media: &AppEventMediaSpec,
    ) -> Result<()> {
        for (field, asset_type, path) in [
            ("card_image", "EVENT_CARD", media.card_image.as_ref()),
            (
                "details_image",
                "EVENT_DETAILS_PAGE",
                media.details_image.as_ref(),
            ),
        ] {
            let Some(path) = path else {
                continue;
            };
            let path = self.context.resolve_path(path);
            let current = self
                .api
                .list_app_event_screenshots(localization_id)?
                .into_iter()
                .find(|asset| asset.attr_str("appEventAssetType") == Some(asset_type));
            if current.as_ref().is_some_and(|asset| {
                event_asset_matches_path(asset, &path)
                    .with_context(|| format!("failed to compare {}", path.display()))
                    .unwrap_or(false)
            }) {
                continue;
            }
            self.record(
                ChangeKind::Replace,
                format!("app.{app_key}.app_events.{event_key}.media.{locale}.{field}"),
                "replace app event image".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(current) = current {
                    self.api.delete_app_event_screenshot(&current.id)?;
                }
                self.api
                    .upload_app_event_screenshot(localization_id, asset_type, &path)?;
            }
        }

        for (field, asset_type, path) in [
            ("card_video", "EVENT_CARD", media.card_video.as_ref()),
            (
                "details_video",
                "EVENT_DETAILS_PAGE",
                media.details_video.as_ref(),
            ),
        ] {
            let Some(path) = path else {
                continue;
            };
            let path = self.context.resolve_path(path);
            let current = self
                .api
                .list_app_event_video_clips(localization_id)?
                .into_iter()
                .find(|asset| asset.attr_str("appEventAssetType") == Some(asset_type));
            if current.as_ref().is_some_and(|asset| {
                event_asset_matches_path(asset, &path)
                    .with_context(|| format!("failed to compare {}", path.display()))
                    .unwrap_or(false)
            }) {
                continue;
            }
            self.record(
                ChangeKind::Replace,
                format!("app.{app_key}.app_events.{event_key}.media.{locale}.{field}"),
                "replace app event video".into(),
            );
            if self.mode == Mode::Apply {
                if let Some(current) = current {
                    self.api.delete_app_event_video_clip(&current.id)?;
                }
                self.api.upload_app_event_video_clip(
                    localization_id,
                    asset_type,
                    media.preview_frame_time_code.as_deref(),
                    &path,
                )?;
            }
        }
        Ok(())
    }

    fn reconcile_privacy_checklist(&mut self, app_key: &str, privacy: &AppPrivacySpec) {
        if privacy.data_types.is_empty()
            && privacy.uses_tracking.is_none()
            && privacy.tracking_domains.is_empty()
        {
            return;
        }
        self.record_manual(
            format!("app.{app_key}.privacy"),
            "App Privacy nutrition label is recorded as a typed checklist; verify/update it manually in App Store Connect".into(),
        );
    }

    fn desired_available_territories(
        &self,
        selection: &TerritorySelectionSpec,
    ) -> Result<BTreeSet<String>> {
        match selection.mode {
            TerritorySelectionMode::All => self.api.list_territory_ids().map(BTreeSet::from_iter),
            TerritorySelectionMode::Only | TerritorySelectionMode::Include => {
                Ok(selection.values.iter().cloned().collect())
            }
        }
    }

    fn resolve_app_price_schedule(
        &self,
        app_record: &App,
        pricing: &CommercePricingSpec,
    ) -> Result<ResolvedPriceSchedule> {
        let entries = pricing
            .schedule
            .iter()
            .enumerate()
            .map(|(index, entry)| self.resolve_app_price_entry(app_record, pricing, index, entry))
            .collect::<Result<Vec<_>>>()?;
        Ok(ResolvedPriceSchedule {
            base_territory: pricing.base_territory.clone(),
            entries,
        })
    }

    fn resolve_app_price_entry(
        &self,
        app_record: &App,
        pricing: &CommercePricingSpec,
        index: usize,
        entry: &PriceScheduleEntrySpec,
    ) -> Result<ResolvedPriceEntry> {
        let price_point_id = self.resolve_app_price_point_id(
            app_record,
            &pricing.base_territory,
            entry.price.as_ref(),
            entry.price_point_id.as_deref(),
        )?;
        let territory_prices = entry
            .territory_prices
            .iter()
            .map(|(territory, price)| {
                Ok((
                    territory.clone(),
                    self.resolve_app_price_point_id(
                        app_record,
                        territory,
                        price.price.as_ref(),
                        price.price_point_id.as_deref(),
                    )
                    .with_context(|| {
                        format!(
                            "failed to resolve app pricing schedule[{index}] territory {territory}"
                        )
                    })?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(ResolvedPriceEntry {
            start_date: entry.start_date.clone(),
            end_date: entry.end_date.clone(),
            territory: pricing.base_territory.clone(),
            price_point_id,
            territory_prices,
        })
    }

    fn resolve_app_price_point_id(
        &self,
        app_record: &App,
        territory: &str,
        price: Option<&StringSource>,
        price_point_id: Option<&str>,
    ) -> Result<String> {
        if let Some(price_point_id) = price_point_id {
            return Ok(price_point_id.to_owned());
        }
        let price = self
            .context
            .resolve_string(price.expect("config validation requires price or price_point_id"))?;
        self.api
            .find_app_price_point(&app_record.id, territory, &price)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no app price point found for app {} territory {territory} customer price {price}",
                    app_record.attributes.name
                )
            })
    }

    fn record_manual(&mut self, subject: String, detail: String) {
        self.record(ChangeKind::Update, subject, detail);
    }
}

impl ResolveContext {
    fn resolve_custom_product_page(
        &self,
        page: &CustomProductPageSpec,
    ) -> Result<ResolvedCustomProductPage> {
        let localizations = page
            .localizations
            .iter()
            .map(|(locale, source)| {
                Ok((
                    locale.clone(),
                    self.resolve_custom_product_page_localization(source)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(ResolvedCustomProductPage {
            name: self.resolve_string(&page.name)?,
            deep_link: self.resolve_optional_string(page.deep_link.as_ref())?,
            visible: page.visible,
            localizations,
        })
    }

    fn resolve_custom_product_page_localization(
        &self,
        source: &CustomProductPageLocalizationSource,
    ) -> Result<ResolvedCustomProductPageLocalization> {
        let spec = match source {
            CustomProductPageLocalizationSource::Inline(spec) => spec.clone(),
            CustomProductPageLocalizationSource::Path(path) => self.load_json5(path)?,
        };
        Ok(ResolvedCustomProductPageLocalization {
            promotional_text: self.resolve_optional_string(spec.promotional_text.as_ref())?,
            search_keyword_ids: spec.search_keyword_ids,
        })
    }

    fn resolve_iap(&self, iap: &InAppPurchaseSpec) -> Result<ResolvedCommerceProduct> {
        Ok(ResolvedCommerceProduct {
            reference_name: self.resolve_string(&iap.reference_name)?,
            review_note: self.resolve_optional_string(iap.review_note.as_ref())?,
            localizations: self.resolve_commerce_localizations(&iap.localizations)?,
        })
    }

    fn resolve_subscription(
        &self,
        subscription: &SubscriptionSpec,
    ) -> Result<ResolvedCommerceProduct> {
        Ok(ResolvedCommerceProduct {
            reference_name: self.resolve_string(&subscription.reference_name)?,
            review_note: self.resolve_optional_string(subscription.review_note.as_ref())?,
            localizations: self.resolve_commerce_localizations(&subscription.localizations)?,
        })
    }

    fn resolve_commerce_localizations(
        &self,
        localizations: &BTreeMap<String, CommerceLocalizationSource>,
    ) -> Result<BTreeMap<String, ResolvedCommerceLocalization>> {
        localizations
            .iter()
            .map(|(locale, source)| {
                Ok((locale.clone(), self.resolve_commerce_localization(source)?))
            })
            .collect()
    }

    fn resolve_commerce_localization(
        &self,
        source: &CommerceLocalizationSource,
    ) -> Result<ResolvedCommerceLocalization> {
        let spec: CommerceLocalizationSpec = match source {
            CommerceLocalizationSource::Inline(spec) => spec.clone(),
            CommerceLocalizationSource::Path(path) => self.load_json5(path)?,
        };
        Ok(ResolvedCommerceLocalization {
            name: self.resolve_string(&spec.name)?,
            description: self.resolve_optional_string(spec.description.as_ref())?,
        })
    }

    fn resolve_subscription_group(
        &self,
        group: &SubscriptionGroupSpec,
    ) -> Result<ResolvedSubscriptionGroup> {
        let localizations = group
            .localizations
            .iter()
            .map(|(locale, source)| {
                Ok((
                    locale.clone(),
                    self.resolve_subscription_group_localization(source)?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(ResolvedSubscriptionGroup {
            reference_name: self.resolve_string(&group.reference_name)?,
            localizations,
        })
    }

    fn resolve_subscription_group_localization(
        &self,
        source: &SubscriptionGroupLocalizationSource,
    ) -> Result<ResolvedSubscriptionGroupLocalization> {
        let spec: SubscriptionGroupLocalizationSpec = match source {
            SubscriptionGroupLocalizationSource::Inline(spec) => spec.clone(),
            SubscriptionGroupLocalizationSource::Path(path) => self.load_json5(path)?,
        };
        Ok(ResolvedSubscriptionGroupLocalization {
            name: self.resolve_string(&spec.name)?,
            custom_app_name: self.resolve_optional_string(spec.custom_app_name.as_ref())?,
        })
    }

    fn resolve_app_event(&self, event: &AppEventSpec) -> Result<ResolvedAppEvent> {
        let localizations = event
            .localizations
            .iter()
            .map(|(locale, source)| {
                Ok((locale.clone(), self.resolve_app_event_localization(source)?))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(ResolvedAppEvent {
            reference_name: self.resolve_string(&event.reference_name)?,
            badge: event.badge.map(|badge| badge.asc_value().to_owned()),
            deep_link: self.resolve_optional_string(event.deep_link.as_ref())?,
            purchase_requirement: self
                .resolve_optional_string(event.purchase_requirement.as_ref())?,
            primary_locale: event.primary_locale.clone(),
            priority: event
                .priority
                .map(|priority| priority.asc_value().to_owned()),
            purpose: event.purpose.map(|purpose| purpose.asc_value().to_owned()),
            territory_schedules: event
                .territory_schedules
                .iter()
                .map(|schedule| {
                    json!({
                        "territories": schedule.territories,
                        "publishStart": schedule.publish_start,
                        "eventStart": schedule.event_start,
                        "eventEnd": schedule.event_end,
                    })
                })
                .collect(),
            localizations,
        })
    }

    fn resolve_app_event_localization(
        &self,
        source: &AppEventLocalizationSource,
    ) -> Result<ResolvedAppEventLocalization> {
        let spec: AppEventLocalizationSpec = match source {
            AppEventLocalizationSource::Inline(spec) => (**spec).clone(),
            AppEventLocalizationSource::Path(path) => self.load_json5(path)?,
        };
        Ok(ResolvedAppEventLocalization {
            name: self.resolve_optional_string(spec.name.as_ref())?,
            short_description: self.resolve_optional_string(spec.short_description.as_ref())?,
            long_description: self.resolve_optional_string(spec.long_description.as_ref())?,
        })
    }
}

struct AssetUploadTarget<'a> {
    collection_path: &'a str,
    resource_type: &'a str,
    relationship_key: &'a str,
    relationship_type: &'a str,
    relationship_id: &'a str,
    path: &'a Path,
    extra_attrs: Option<Map<String, Value>>,
}

impl AppStoreApi<'_> {
    fn get_app_availability(&self, app_id: &str) -> Result<Option<RemoteResource>> {
        self.get_optional_related_single(&format!("/apps/{app_id}/appAvailabilityV2"))
    }

    fn create_app_availability(
        &self,
        app_id: &str,
        available_in_new_territories: bool,
        territories: &[String],
    ) -> Result<RemoteResource> {
        let included = territories
            .iter()
            .map(|territory| {
                json!({
                    "type": "territoryAvailabilities",
                    "id": territory,
                    "attributes": { "available": true },
                    "relationships": {
                        "territory": { "data": { "type": "territories", "id": territory } }
                    }
                })
            })
            .collect::<Vec<_>>();
        self.post_resource_with_included(
            "/v2/appAvailabilities",
            "appAvailabilities",
            map_with(
                "availableInNewTerritories",
                json!(available_in_new_territories),
            ),
            json!({
                "app": { "data": { "type": "apps", "id": app_id } },
                "territoryAvailabilities": {
                    "data": territories.iter().map(|territory| {
                        json!({ "type": "territoryAvailabilities", "id": territory })
                    }).collect::<Vec<_>>()
                }
            })
            .as_object()
            .cloned(),
            included,
        )
    }

    fn get_app_price_schedule(&self, app_id: &str) -> Result<Option<ResolvedPriceSchedule>> {
        let Some(response) = self.get_optional_single_with_included(
            &format!("/apps/{app_id}/appPriceSchedule"),
            vec![
                ("include".into(), "baseTerritory,manualPrices".into()),
                (
                    "fields[appPrices]".into(),
                    "startDate,endDate,appPricePoint,territory".into(),
                ),
                ("limit[manualPrices]".into(), "50".into()),
            ],
        )?
        else {
            return Ok(None);
        };
        Ok(Some(price_schedule_from_included(response)))
    }

    fn create_app_price_schedule(
        &self,
        app_id: &str,
        schedule: &ResolvedPriceSchedule,
    ) -> Result<RemoteResource> {
        let (data, included) = price_schedule_payload(
            "app",
            "apps",
            app_id,
            "appPrices",
            "appPricePoint",
            "appPricePoints",
            schedule,
        );
        self.client
            .request_json(
                Method::POST,
                asc_endpoint("/appPriceSchedules"),
                &[],
                Some(&json!({
                    "data": {
                        "type": "appPriceSchedules",
                        "relationships": data
                    },
                    "included": included
                })),
            )
            .map(|response: JsonApiSingle<RemoteResource>| response.data)
    }

    fn find_app_price_point(
        &self,
        app_id: &str,
        territory: &str,
        customer_price: &str,
    ) -> Result<Option<String>> {
        let points = self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/appPricePoints")),
            vec![
                ("filter[territory]".into(), territory.into()),
                ("fields[appPricePoints]".into(), "customerPrice".into()),
                ("limit".into(), "8000".into()),
            ],
        )?;
        Ok(points
            .into_iter()
            .find(|point: &RemoteResource| point.attr_str("customerPrice") == Some(customer_price))
            .map(|point| point.id))
    }

    fn find_custom_product_page(
        &self,
        app_id: &str,
        asc_id: Option<&str>,
        name: &str,
    ) -> Result<Option<RemoteResource>> {
        if let Some(asc_id) = asc_id {
            return self.get_optional_instance("/appCustomProductPages", asc_id);
        }
        let pages = self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/appCustomProductPages")),
            vec![
                (
                    "fields[appCustomProductPages]".into(),
                    "name,visible".into(),
                ),
                ("limit".into(), "200".into()),
            ],
        )?;
        Ok(pages
            .into_iter()
            .find(|page: &RemoteResource| page.attr_str("name") == Some(name)))
    }

    fn create_custom_product_page(&self, app_id: &str, name: &str) -> Result<RemoteResource> {
        self.post_resource(
            "/appCustomProductPages",
            "appCustomProductPages",
            map_with("name", json!(name)),
            json!({ "app": { "data": { "type": "apps", "id": app_id } } })
                .as_object()
                .cloned(),
        )
    }

    fn update_custom_product_page(
        &self,
        page_id: &str,
        attrs: Map<String, Value>,
    ) -> Result<RemoteResource> {
        self.patch_resource(
            "/appCustomProductPages",
            "appCustomProductPages",
            page_id,
            attrs,
            None,
        )
    }

    fn list_custom_product_page_versions(&self, page_id: &str) -> Result<Vec<RemoteResource>> {
        self.client.get_paginated(
            asc_endpoint(&format!(
                "/appCustomProductPages/{page_id}/appCustomProductPageVersions"
            )),
            vec![
                (
                    "fields[appCustomProductPageVersions]".into(),
                    "state,deepLink".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn create_custom_product_page_version(
        &self,
        page_id: &str,
        deep_link: Option<&str>,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        if let Some(deep_link) = deep_link {
            attrs.insert("deepLink".into(), json!(deep_link));
        }
        self.post_resource(
            "/appCustomProductPageVersions",
            "appCustomProductPageVersions",
            attrs,
            json!({
                "appCustomProductPage": {
                    "data": { "type": "appCustomProductPages", "id": page_id }
                }
            })
            .as_object()
            .cloned(),
        )
    }

    fn update_custom_product_page_version(
        &self,
        version_id: &str,
        deep_link: Option<&str>,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        attrs.insert(
            "deepLink".into(),
            deep_link.map_or(Value::Null, Value::from),
        );
        self.patch_resource(
            "/appCustomProductPageVersions",
            "appCustomProductPageVersions",
            version_id,
            attrs,
            None,
        )
    }

    fn list_custom_product_page_localizations(
        &self,
        version_id: &str,
    ) -> Result<Vec<RemoteResource>> {
        self.client.get_paginated(
            asc_endpoint(&format!(
                "/appCustomProductPageVersions/{version_id}/appCustomProductPageLocalizations"
            )),
            vec![
                (
                    "fields[appCustomProductPageLocalizations]".into(),
                    "locale,promotionalText".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn create_custom_product_page_localization(
        &self,
        version_id: &str,
        locale: &str,
        promotional_text: Option<&str>,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        attrs.insert("locale".into(), json!(locale));
        insert_optional(&mut attrs, "promotionalText", promotional_text);
        self.post_resource(
            "/appCustomProductPageLocalizations",
            "appCustomProductPageLocalizations",
            attrs,
            json!({
                "appCustomProductPageVersion": {
                    "data": { "type": "appCustomProductPageVersions", "id": version_id }
                }
            })
            .as_object()
            .cloned(),
        )
    }

    fn update_custom_product_page_localization(
        &self,
        localization_id: &str,
        promotional_text: Option<&str>,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        insert_optional(&mut attrs, "promotionalText", promotional_text);
        self.patch_resource(
            "/appCustomProductPageLocalizations",
            "appCustomProductPageLocalizations",
            localization_id,
            attrs,
            None,
        )
    }

    fn add_app_keyword_relationships(&self, localization_id: &str, ids: &[&str]) -> Result<()> {
        self.client.request_empty(
            Method::POST,
            asc_endpoint(&format!(
                "/appCustomProductPageLocalizations/{localization_id}/relationships/searchKeywords"
            )),
            &[],
            Some(&json!({
                "data": ids.iter().map(|id| json!({ "type": "appKeywords", "id": id })).collect::<Vec<_>>()
            })),
        )
    }

    fn find_screenshot_set_for_localization(
        &self,
        parent_resource: &str,
        localization_id: &str,
        display_type: &str,
    ) -> Result<Option<AppScreenshotSet>> {
        let sets: Vec<AppScreenshotSet> = self.client.get_paginated(
            asc_endpoint(&format!(
                "/{parent_resource}/{localization_id}/appScreenshotSets"
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

    fn create_screenshot_set_for_relationship(
        &self,
        relationship_key: &str,
        relationship_type: &str,
        localization_id: &str,
        display_type: &str,
    ) -> Result<AppScreenshotSet> {
        self.post_resource(
            "/appScreenshotSets",
            "appScreenshotSets",
            map_with("screenshotDisplayType", json!(display_type)),
            json!({
                relationship_key: {
                    "data": { "type": relationship_type, "id": localization_id }
                }
            })
            .as_object()
            .cloned(),
        )
    }

    fn find_preview_set_for_localization(
        &self,
        parent_resource: &str,
        localization_id: &str,
        preview_type: &str,
    ) -> Result<Option<AppPreviewSet>> {
        let sets: Vec<AppPreviewSet> = self.client.get_paginated(
            asc_endpoint(&format!(
                "/{parent_resource}/{localization_id}/appPreviewSets"
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

    fn create_preview_set_for_relationship(
        &self,
        relationship_key: &str,
        relationship_type: &str,
        localization_id: &str,
        preview_type: &str,
    ) -> Result<AppPreviewSet> {
        self.post_resource(
            "/appPreviewSets",
            "appPreviewSets",
            map_with("previewType", json!(preview_type)),
            json!({
                relationship_key: {
                    "data": { "type": relationship_type, "id": localization_id }
                }
            })
            .as_object()
            .cloned(),
        )
    }

    fn find_iap(
        &self,
        app_id: &str,
        asc_id: Option<&str>,
        product_id: &str,
    ) -> Result<Option<RemoteResource>> {
        if let Some(asc_id) = asc_id {
            return self.get_optional_instance("/v2/inAppPurchases", asc_id);
        }
        let iaps = self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/inAppPurchasesV2")),
            vec![
                ("filter[productId]".into(), product_id.into()),
                (
                    "fields[inAppPurchases]".into(),
                    "name,productId,inAppPurchaseType,reviewNote,familySharable".into(),
                ),
                ("limit".into(), "2".into()),
            ],
        )?;
        ensure!(
            iaps.len() <= 1,
            "ASC returned multiple in-app purchases for product id {product_id}"
        );
        Ok(iaps.into_iter().next())
    }

    fn create_iap(
        &self,
        app_id: &str,
        spec: &InAppPurchaseSpec,
        desired: &ResolvedCommerceProduct,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        attrs.insert("name".into(), json!(desired.reference_name));
        attrs.insert("productId".into(), json!(spec.product_id));
        attrs.insert("inAppPurchaseType".into(), json!(spec.kind.asc_value()));
        insert_optional(&mut attrs, "reviewNote", desired.review_note.as_deref());
        if let Some(value) = spec.family_sharable {
            attrs.insert("familySharable".into(), json!(value));
        }
        self.post_resource(
            "/v2/inAppPurchases",
            "inAppPurchases",
            attrs,
            json!({ "app": { "data": { "type": "apps", "id": app_id } } })
                .as_object()
                .cloned(),
        )
    }

    fn update_iap(&self, iap_id: &str, attrs: Map<String, Value>) -> Result<RemoteResource> {
        self.patch_resource("/v2/inAppPurchases", "inAppPurchases", iap_id, attrs, None)
    }

    fn find_subscription_group(
        &self,
        app_id: &str,
        asc_id: Option<&str>,
        reference_name: &str,
    ) -> Result<Option<RemoteResource>> {
        if let Some(asc_id) = asc_id {
            return self.get_optional_instance("/subscriptionGroups", asc_id);
        }
        let groups = self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/subscriptionGroups")),
            vec![
                ("filter[referenceName]".into(), reference_name.into()),
                ("fields[subscriptionGroups]".into(), "referenceName".into()),
                ("limit".into(), "2".into()),
            ],
        )?;
        ensure!(
            groups.len() <= 1,
            "ASC returned multiple subscription groups for reference name {reference_name}"
        );
        Ok(groups.into_iter().next())
    }

    fn create_subscription_group(
        &self,
        app_id: &str,
        reference_name: &str,
    ) -> Result<RemoteResource> {
        self.post_resource(
            "/subscriptionGroups",
            "subscriptionGroups",
            map_with("referenceName", json!(reference_name)),
            json!({ "app": { "data": { "type": "apps", "id": app_id } } })
                .as_object()
                .cloned(),
        )
    }

    fn update_subscription_group(
        &self,
        group_id: &str,
        attrs: Map<String, Value>,
    ) -> Result<RemoteResource> {
        self.patch_resource(
            "/subscriptionGroups",
            "subscriptionGroups",
            group_id,
            attrs,
            None,
        )
    }

    fn list_subscription_group_localizations(&self, group_id: &str) -> Result<Vec<RemoteResource>> {
        self.client.get_paginated(
            asc_endpoint(&format!(
                "/subscriptionGroups/{group_id}/subscriptionGroupLocalizations"
            )),
            vec![
                (
                    "fields[subscriptionGroupLocalizations]".into(),
                    "locale,name,customAppName".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn create_subscription_group_localization(
        &self,
        group_id: &str,
        locale: &str,
        desired: &ResolvedSubscriptionGroupLocalization,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        attrs.insert("locale".into(), json!(locale));
        attrs.insert("name".into(), json!(desired.name));
        insert_optional(
            &mut attrs,
            "customAppName",
            desired.custom_app_name.as_deref(),
        );
        self.post_resource(
            "/subscriptionGroupLocalizations",
            "subscriptionGroupLocalizations",
            attrs,
            json!({
                "subscriptionGroup": {
                    "data": { "type": "subscriptionGroups", "id": group_id }
                }
            })
            .as_object()
            .cloned(),
        )
    }

    fn update_subscription_group_localization(
        &self,
        localization_id: &str,
        desired: &ResolvedSubscriptionGroupLocalization,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        attrs.insert("name".into(), json!(desired.name));
        insert_optional(
            &mut attrs,
            "customAppName",
            desired.custom_app_name.as_deref(),
        );
        self.patch_resource(
            "/subscriptionGroupLocalizations",
            "subscriptionGroupLocalizations",
            localization_id,
            attrs,
            None,
        )
    }

    fn find_subscription(
        &self,
        group_id: &str,
        asc_id: Option<&str>,
        product_id: &str,
    ) -> Result<Option<RemoteResource>> {
        if let Some(asc_id) = asc_id {
            return self.get_optional_instance("/subscriptions", asc_id);
        }
        let subscriptions = self.client.get_paginated(
            asc_endpoint(&format!("/subscriptionGroups/{group_id}/subscriptions")),
            vec![
                ("filter[productId]".into(), product_id.into()),
                (
                    "fields[subscriptions]".into(),
                    "name,productId,familySharable,subscriptionPeriod,reviewNote,groupLevel".into(),
                ),
                ("limit".into(), "2".into()),
            ],
        )?;
        ensure!(
            subscriptions.len() <= 1,
            "ASC returned multiple subscriptions for product id {product_id}"
        );
        Ok(subscriptions.into_iter().next())
    }

    fn create_subscription(
        &self,
        group_id: &str,
        spec: &SubscriptionSpec,
        desired: &ResolvedCommerceProduct,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        attrs.insert("name".into(), json!(desired.reference_name));
        attrs.insert("productId".into(), json!(spec.product_id));
        insert_optional(&mut attrs, "reviewNote", desired.review_note.as_deref());
        if let Some(value) = spec.family_sharable {
            attrs.insert("familySharable".into(), json!(value));
        }
        if let Some(period) = spec.period {
            attrs.insert("subscriptionPeriod".into(), json!(period.asc_value()));
        }
        if let Some(group_level) = spec.group_level {
            attrs.insert("groupLevel".into(), json!(group_level));
        }
        self.post_resource(
            "/subscriptions",
            "subscriptions",
            attrs,
            json!({ "group": { "data": { "type": "subscriptionGroups", "id": group_id } } })
                .as_object()
                .cloned(),
        )
    }

    fn update_subscription(
        &self,
        subscription_id: &str,
        attrs: Map<String, Value>,
    ) -> Result<RemoteResource> {
        self.patch_resource(
            "/subscriptions",
            "subscriptions",
            subscription_id,
            attrs,
            None,
        )
    }

    fn list_commerce_localizations(
        &self,
        resource_id: &str,
        kind: CommerceKind,
    ) -> Result<Vec<RemoteResource>> {
        self.client.get_paginated(
            asc_endpoint(&format!(
                "{}/{resource_id}/{}",
                kind.resource_path(),
                kind.localizations_path()
            )),
            vec![
                (
                    format!("fields[{}]", kind.localization_resource_type()),
                    "locale,name,description".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn create_commerce_localization(
        &self,
        resource_id: &str,
        kind: CommerceKind,
        locale: &str,
        desired: &ResolvedCommerceLocalization,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        attrs.insert("locale".into(), json!(locale));
        attrs.insert("name".into(), json!(desired.name));
        insert_optional(&mut attrs, "description", desired.description.as_deref());
        self.post_resource(
            kind.localization_collection_path(),
            kind.localization_resource_type(),
            attrs,
            json!({
                kind.relationship_key(): {
                    "data": { "type": kind.resource_type(), "id": resource_id }
                }
            })
            .as_object()
            .cloned(),
        )
    }

    fn update_commerce_localization(
        &self,
        localization_id: &str,
        kind: CommerceKind,
        desired: &ResolvedCommerceLocalization,
    ) -> Result<RemoteResource> {
        let mut attrs = Map::new();
        attrs.insert("name".into(), json!(desired.name));
        insert_optional(&mut attrs, "description", desired.description.as_deref());
        self.patch_resource(
            kind.localization_collection_path(),
            kind.localization_resource_type(),
            localization_id,
            attrs,
            None,
        )
    }

    fn get_commerce_review_screenshot(
        &self,
        resource_id: &str,
        kind: CommerceKind,
    ) -> Result<Option<ReviewScreenshot>> {
        self.get_optional_related_single(&format!(
            "{}/{resource_id}/appStoreReviewScreenshot",
            kind.resource_path()
        ))
    }

    fn upload_commerce_review_screenshot(
        &self,
        resource_id: &str,
        kind: CommerceKind,
        path: &Path,
    ) -> Result<()> {
        self.upload_one_asset(AssetUploadTarget {
            collection_path: kind.review_screenshot_collection_path(),
            resource_type: kind.review_screenshot_resource_type(),
            relationship_key: kind.relationship_key(),
            relationship_type: kind.resource_type(),
            relationship_id: resource_id,
            path,
            extra_attrs: None,
        })
    }

    fn delete_commerce_review_screenshot(&self, id: &str, kind: CommerceKind) -> Result<()> {
        self.client.request_empty(
            Method::DELETE,
            asc_endpoint(&format!(
                "{}/{}",
                kind.review_screenshot_collection_path(),
                id
            )),
            &[],
            None::<&Value>,
        )
    }

    fn find_app_event(
        &self,
        app_id: &str,
        asc_id: Option<&str>,
        reference_name: &str,
    ) -> Result<Option<RemoteResource>> {
        if let Some(asc_id) = asc_id {
            return self.get_optional_instance("/appEvents", asc_id);
        }
        let events: Vec<RemoteResource> = self.client.get_paginated(
            asc_endpoint(&format!("/apps/{app_id}/appEvents")),
            vec![
                (
                    "fields[appEvents]".into(),
                    "referenceName,badge,deepLink,purchaseRequirement,primaryLocale,priority,purpose,territorySchedules,eventState".into(),
                ),
                ("limit".into(), "200".into()),
            ],
        )?;
        Ok(events.into_iter().find(|event| {
            event.attr_str("referenceName") == Some(reference_name)
                && !matches!(
                    event.attr_str("eventState"),
                    Some("ARCHIVED") | Some("PAST")
                )
        }))
    }

    fn create_app_event(&self, app_id: &str, desired: &ResolvedAppEvent) -> Result<RemoteResource> {
        self.post_resource(
            "/appEvents",
            "appEvents",
            app_event_attrs(desired),
            json!({ "app": { "data": { "type": "apps", "id": app_id } } })
                .as_object()
                .cloned(),
        )
    }

    fn update_app_event(
        &self,
        event_id: &str,
        attrs: Map<String, Value>,
    ) -> Result<RemoteResource> {
        self.patch_resource("/appEvents", "appEvents", event_id, attrs, None)
    }

    fn list_app_event_localizations(&self, event_id: &str) -> Result<Vec<RemoteResource>> {
        self.client.get_paginated(
            asc_endpoint(&format!("/appEvents/{event_id}/localizations")),
            vec![
                (
                    "fields[appEventLocalizations]".into(),
                    "locale,name,shortDescription,longDescription".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn create_app_event_localization(
        &self,
        event_id: &str,
        locale: &str,
        desired: &ResolvedAppEventLocalization,
    ) -> Result<RemoteResource> {
        let mut attrs = app_event_localization_attrs(desired);
        attrs.insert("locale".into(), json!(locale));
        self.post_resource(
            "/appEventLocalizations",
            "appEventLocalizations",
            attrs,
            json!({ "appEvent": { "data": { "type": "appEvents", "id": event_id } } })
                .as_object()
                .cloned(),
        )
    }

    fn update_app_event_localization(
        &self,
        localization_id: &str,
        desired: &ResolvedAppEventLocalization,
    ) -> Result<RemoteResource> {
        self.patch_resource(
            "/appEventLocalizations",
            "appEventLocalizations",
            localization_id,
            app_event_localization_attrs(desired),
            None,
        )
    }

    fn list_app_event_screenshots(&self, localization_id: &str) -> Result<Vec<EventAsset>> {
        self.client.get_paginated(
            asc_endpoint(&format!(
                "/appEventLocalizations/{localization_id}/appEventScreenshots"
            )),
            vec![
                (
                    "fields[appEventScreenshots]".into(),
                    "fileName,fileSize,uploadOperations,assetDeliveryState,appEventAssetType"
                        .into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn upload_app_event_screenshot(
        &self,
        localization_id: &str,
        asset_type: &str,
        path: &Path,
    ) -> Result<()> {
        self.upload_one_asset(AssetUploadTarget {
            collection_path: "/appEventScreenshots",
            resource_type: "appEventScreenshots",
            relationship_key: "appEventLocalization",
            relationship_type: "appEventLocalizations",
            relationship_id: localization_id,
            path,
            extra_attrs: Some(map_with("appEventAssetType", json!(asset_type))),
        })
    }

    fn delete_app_event_screenshot(&self, id: &str) -> Result<()> {
        self.client.request_empty(
            Method::DELETE,
            asc_endpoint(&format!("/appEventScreenshots/{id}")),
            &[],
            None::<&Value>,
        )
    }

    fn list_app_event_video_clips(&self, localization_id: &str) -> Result<Vec<EventAsset>> {
        self.client.get_paginated(
            asc_endpoint(&format!(
                "/appEventLocalizations/{localization_id}/appEventVideoClips"
            )),
            vec![
                (
                    "fields[appEventVideoClips]".into(),
                    "fileName,fileSize,uploadOperations,assetDeliveryState,videoDeliveryState,appEventAssetType".into(),
                ),
                ("limit".into(), "50".into()),
            ],
        )
    }

    fn upload_app_event_video_clip(
        &self,
        localization_id: &str,
        asset_type: &str,
        preview_frame_time_code: Option<&str>,
        path: &Path,
    ) -> Result<()> {
        let mut attrs = map_with("appEventAssetType", json!(asset_type));
        insert_optional(&mut attrs, "previewFrameTimeCode", preview_frame_time_code);
        self.upload_one_asset(AssetUploadTarget {
            collection_path: "/appEventVideoClips",
            resource_type: "appEventVideoClips",
            relationship_key: "appEventLocalization",
            relationship_type: "appEventLocalizations",
            relationship_id: localization_id,
            path,
            extra_attrs: Some(attrs),
        })
    }

    fn delete_app_event_video_clip(&self, id: &str) -> Result<()> {
        self.client.request_empty(
            Method::DELETE,
            asc_endpoint(&format!("/appEventVideoClips/{id}")),
            &[],
            None::<&Value>,
        )
    }

    fn list_territory_ids(&self) -> Result<Vec<String>> {
        let territories: Vec<RemoteResource> = self.client.get_paginated(
            asc_endpoint("/territories"),
            vec![("limit".into(), "200".into())],
        )?;
        Ok(territories
            .into_iter()
            .map(|territory| territory.id)
            .collect())
    }

    fn list_related_ids(&self, path: &str) -> Result<Vec<String>> {
        self.client
            .request_json(Method::GET, asc_endpoint(path), &[], None::<&Value>)
            .map(|response: JsonApiListNoLinks<ResourceIdentifier>| {
                response
                    .data
                    .into_iter()
                    .map(|resource| resource.id)
                    .collect()
            })
    }

    fn get_optional_instance<T>(&self, path: &str, id: &str) -> Result<Option<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        match self.get_instance(path, id) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.to_string().contains("returned 404") => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn get_optional_related_single<T>(&self, path: &str) -> Result<Option<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        match self.get_related_single(path) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.to_string().contains("returned 404") => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn get_optional_single_with_included(
        &self,
        path: &str,
        params: Vec<(String, String)>,
    ) -> Result<Option<JsonApiSingleWithIncluded<RemoteResource>>> {
        match self
            .client
            .request_json(Method::GET, asc_endpoint(path), &params, None::<&Value>)
        {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.to_string().contains("returned 404") => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn post_resource_with_included<T>(
        &self,
        path: &str,
        kind: &str,
        attributes: Map<String, Value>,
        relationships: Option<Map<String, Value>>,
        included: Vec<Value>,
    ) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut request = super::build_resource_request(None, kind, attributes, relationships);
        request["included"] = Value::Array(included);
        self.client
            .request_json(Method::POST, asc_endpoint(path), &[], Some(&request))
            .map(|response: JsonApiSingle<T>| response.data)
    }

    fn upload_one_asset(&self, target: AssetUploadTarget<'_>) -> Result<()> {
        let bytes = fs::read(target.path)
            .with_context(|| format!("failed to read {}", target.path.display()))?;
        let mut attrs = file_attrs(file_name(target.path)?, bytes.len());
        if let Some(extra_attrs) = target.extra_attrs {
            attrs.extend(extra_attrs);
        }
        let asset: UploadableAsset = self.post_resource(
            target.collection_path,
            target.resource_type,
            attrs,
            json!({
                target.relationship_key: {
                    "data": { "type": target.relationship_type, "id": target.relationship_id }
                }
            })
            .as_object()
            .cloned(),
        )?;
        self.upload_operations(
            asset.attributes.upload_operations.as_deref().unwrap_or(&[]),
            &bytes,
        )?;
        let uploaded = if target.resource_type.starts_with("appEvent") {
            map_with("uploaded", json!(true))
        } else {
            uploaded_attrs(&md5_hex(&bytes))
        };
        self.patch_resource(
            target.collection_path,
            target.resource_type,
            &asset.id,
            uploaded,
            None,
        )
        .map(|_: UploadableAsset| ())
    }
}

fn optional_matches(desired: Option<&str>, existing: Option<&str>) -> bool {
    desired.is_none_or(|desired| existing == Some(desired))
}

fn compare_value_attr(
    attrs: &mut Map<String, Value>,
    key: &str,
    desired: Option<Value>,
    current: Option<&Value>,
) {
    if let Some(desired) = desired
        && current != Some(&desired)
    {
        attrs.insert(key.into(), desired);
    }
}

fn compare_optional_string_value(
    attrs: &mut Map<String, Value>,
    key: &str,
    desired: Option<&str>,
    current: Option<&str>,
) {
    if let Some(desired) = desired
        && current != Some(desired)
    {
        attrs.insert(key.into(), json!(desired));
    }
}

fn compare_optional_bool_value(
    attrs: &mut Map<String, Value>,
    key: &str,
    desired: Option<bool>,
    current: Option<bool>,
) {
    if let Some(desired) = desired
        && current != Some(desired)
    {
        attrs.insert(key.into(), json!(desired));
    }
}

fn ensure_immutable(current: Option<&str>, desired: Option<&str>, subject: &str) -> Result<()> {
    ensure!(
        current == desired,
        "{subject} is immutable after creation: ASC has {:?}, config has {:?}",
        current,
        desired
    );
    Ok(())
}

fn insert_optional(attrs: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        attrs.insert(key.into(), json!(value));
    }
}

fn map_with(key: &str, value: Value) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(key.into(), value);
    map
}

fn app_event_attrs(desired: &ResolvedAppEvent) -> Map<String, Value> {
    let mut attrs = Map::new();
    attrs.insert("referenceName".into(), json!(desired.reference_name));
    insert_optional(&mut attrs, "badge", desired.badge.as_deref());
    insert_optional(&mut attrs, "deepLink", desired.deep_link.as_deref());
    insert_optional(
        &mut attrs,
        "purchaseRequirement",
        desired.purchase_requirement.as_deref(),
    );
    insert_optional(
        &mut attrs,
        "primaryLocale",
        desired.primary_locale.as_deref(),
    );
    insert_optional(&mut attrs, "priority", desired.priority.as_deref());
    insert_optional(&mut attrs, "purpose", desired.purpose.as_deref());
    if !desired.territory_schedules.is_empty() {
        attrs.insert(
            "territorySchedules".into(),
            Value::Array(desired.territory_schedules.clone()),
        );
    }
    attrs
}

fn app_event_localization_attrs(desired: &ResolvedAppEventLocalization) -> Map<String, Value> {
    let mut attrs = Map::new();
    insert_optional(&mut attrs, "name", desired.name.as_deref());
    insert_optional(
        &mut attrs,
        "shortDescription",
        desired.short_description.as_deref(),
    );
    insert_optional(
        &mut attrs,
        "longDescription",
        desired.long_description.as_deref(),
    );
    attrs
}

fn price_schedule_payload(
    owner_key: &str,
    owner_type: &str,
    owner_id: &str,
    price_resource_type: &str,
    price_point_key: &str,
    price_point_type: &str,
    schedule: &ResolvedPriceSchedule,
) -> (Map<String, Value>, Vec<Value>) {
    let mut relationships = Map::new();
    relationships.insert(
        owner_key.into(),
        json!({ "data": { "type": owner_type, "id": owner_id } }),
    );
    relationships.insert(
        "baseTerritory".into(),
        json!({ "data": { "type": "territories", "id": schedule.base_territory } }),
    );

    let mut included = Vec::new();
    let mut manual_prices = Vec::new();
    for (index, entry) in schedule.entries.iter().enumerate() {
        let id = format!("manual-price-{index}-{}", entry.territory);
        manual_prices.push(json!({ "type": price_resource_type, "id": id }));
        included.push(price_entry_json(PriceEntryJsonArgs {
            id: &id,
            price_resource_type,
            price_point_key,
            price_point_type,
            start_date: entry.start_date.as_deref(),
            end_date: entry.end_date.as_deref(),
            territory: &entry.territory,
            price_point_id: &entry.price_point_id,
        }));
        for (territory, price_point_id) in &entry.territory_prices {
            let id = format!("manual-price-{index}-{territory}");
            manual_prices.push(json!({ "type": price_resource_type, "id": id }));
            included.push(price_entry_json(PriceEntryJsonArgs {
                id: &id,
                price_resource_type,
                price_point_key,
                price_point_type,
                start_date: entry.start_date.as_deref(),
                end_date: entry.end_date.as_deref(),
                territory,
                price_point_id,
            }));
        }
    }
    relationships.insert("manualPrices".into(), json!({ "data": manual_prices }));
    (relationships, included)
}

struct PriceEntryJsonArgs<'a> {
    id: &'a str,
    price_resource_type: &'a str,
    price_point_key: &'a str,
    price_point_type: &'a str,
    start_date: Option<&'a str>,
    end_date: Option<&'a str>,
    territory: &'a str,
    price_point_id: &'a str,
}

fn price_entry_json(args: PriceEntryJsonArgs<'_>) -> Value {
    let mut attrs = Map::new();
    if let Some(start_date) = args.start_date {
        attrs.insert("startDate".into(), json!(start_date));
    }
    if let Some(end_date) = args.end_date {
        attrs.insert("endDate".into(), json!(end_date));
    }
    json!({
        "type": args.price_resource_type,
        "id": args.id,
        "attributes": attrs,
        "relationships": {
            "territory": { "data": { "type": "territories", "id": args.territory } },
            args.price_point_key: { "data": { "type": args.price_point_type, "id": args.price_point_id } }
        }
    })
}

fn price_schedule_from_included(
    response: JsonApiSingleWithIncluded<RemoteResource>,
) -> ResolvedPriceSchedule {
    let base_territory = response
        .included
        .iter()
        .find(|resource| resource.kind == "territories")
        .map(|territory| territory.id.clone())
        .unwrap_or_default();
    let entries = response
        .included
        .iter()
        .filter(|resource| resource.kind == "appPrices")
        .filter_map(|price| {
            Some(ResolvedPriceEntry {
                start_date: price.attr_str("startDate").map(str::to_owned),
                end_date: price.attr_str("endDate").map(str::to_owned),
                territory: price.relationship_id("territory")?,
                price_point_id: price.relationship_id("appPricePoint")?,
                territory_prices: BTreeMap::new(),
            })
        })
        .collect();
    ResolvedPriceSchedule {
        base_territory,
        entries,
    }
}

#[derive(Debug, Clone)]
struct ResolvedCustomProductPage {
    name: String,
    deep_link: Option<String>,
    visible: Option<bool>,
    localizations: BTreeMap<String, ResolvedCustomProductPageLocalization>,
}

#[derive(Debug, Clone)]
struct ResolvedCustomProductPageLocalization {
    promotional_text: Option<String>,
    search_keyword_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct ResolvedCommerceProduct {
    reference_name: String,
    review_note: Option<String>,
    localizations: BTreeMap<String, ResolvedCommerceLocalization>,
}

#[derive(Debug, Clone)]
struct ResolvedCommerceLocalization {
    name: String,
    description: Option<String>,
}

impl ResolvedCommerceLocalization {
    fn matches(&self, existing: &RemoteResource) -> bool {
        existing.attr_str("name") == Some(self.name.as_str())
            && optional_matches(
                self.description.as_deref(),
                existing.attr_str("description"),
            )
    }
}

#[derive(Debug, Clone)]
struct ResolvedSubscriptionGroup {
    reference_name: String,
    localizations: BTreeMap<String, ResolvedSubscriptionGroupLocalization>,
}

#[derive(Debug, Clone)]
struct ResolvedSubscriptionGroupLocalization {
    name: String,
    custom_app_name: Option<String>,
}

impl ResolvedSubscriptionGroupLocalization {
    fn matches(&self, existing: &RemoteResource) -> bool {
        existing.attr_str("name") == Some(self.name.as_str())
            && optional_matches(
                self.custom_app_name.as_deref(),
                existing.attr_str("customAppName"),
            )
    }
}

#[derive(Debug, Clone)]
struct ResolvedAppEvent {
    reference_name: String,
    badge: Option<String>,
    deep_link: Option<String>,
    purchase_requirement: Option<String>,
    primary_locale: Option<String>,
    priority: Option<String>,
    purpose: Option<String>,
    territory_schedules: Vec<Value>,
    localizations: BTreeMap<String, ResolvedAppEventLocalization>,
}

#[derive(Debug, Clone)]
struct ResolvedAppEventLocalization {
    name: Option<String>,
    short_description: Option<String>,
    long_description: Option<String>,
}

impl ResolvedAppEventLocalization {
    fn matches(&self, existing: &RemoteResource) -> bool {
        optional_matches(self.name.as_deref(), existing.attr_str("name"))
            && optional_matches(
                self.short_description.as_deref(),
                existing.attr_str("shortDescription"),
            )
            && optional_matches(
                self.long_description.as_deref(),
                existing.attr_str("longDescription"),
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedPriceSchedule {
    base_territory: String,
    entries: Vec<ResolvedPriceEntry>,
}

impl ResolvedPriceSchedule {
    fn matches(&self, other: &Self) -> bool {
        self == other
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedPriceEntry {
    start_date: Option<String>,
    end_date: Option<String>,
    territory: String,
    price_point_id: String,
    territory_prices: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
enum CommerceKind {
    InAppPurchase,
    Subscription,
}

impl CommerceKind {
    fn resource_path(self) -> &'static str {
        match self {
            Self::InAppPurchase => "/v2/inAppPurchases",
            Self::Subscription => "/subscriptions",
        }
    }

    fn resource_type(self) -> &'static str {
        match self {
            Self::InAppPurchase => "inAppPurchases",
            Self::Subscription => "subscriptions",
        }
    }

    fn relationship_key(self) -> &'static str {
        match self {
            Self::InAppPurchase => "inAppPurchaseV2",
            Self::Subscription => "subscription",
        }
    }

    fn localizations_path(self) -> &'static str {
        match self {
            Self::InAppPurchase => "inAppPurchaseLocalizations",
            Self::Subscription => "subscriptionLocalizations",
        }
    }

    fn localization_collection_path(self) -> &'static str {
        match self {
            Self::InAppPurchase => "/inAppPurchaseLocalizations",
            Self::Subscription => "/subscriptionLocalizations",
        }
    }

    fn localization_resource_type(self) -> &'static str {
        match self {
            Self::InAppPurchase => "inAppPurchaseLocalizations",
            Self::Subscription => "subscriptionLocalizations",
        }
    }

    fn review_screenshot_collection_path(self) -> &'static str {
        match self {
            Self::InAppPurchase => "/inAppPurchaseAppStoreReviewScreenshots",
            Self::Subscription => "/subscriptionAppStoreReviewScreenshots",
        }
    }

    fn review_screenshot_resource_type(self) -> &'static str {
        match self {
            Self::InAppPurchase => "inAppPurchaseAppStoreReviewScreenshots",
            Self::Subscription => "subscriptionAppStoreReviewScreenshots",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RemoteResource {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    attributes: Map<String, Value>,
    #[serde(default)]
    relationships: BTreeMap<String, RelationshipValue>,
}

impl RemoteResource {
    fn planned(id: &str, kind: &str) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            attributes: Map::new(),
            relationships: BTreeMap::new(),
        }
    }

    fn is_planned(&self) -> bool {
        self.id.starts_with("planned-")
    }

    fn attr(&self, key: &str) -> Option<&Value> {
        self.attributes.get(key)
    }

    fn attr_str(&self, key: &str) -> Option<&str> {
        self.attr(key).and_then(Value::as_str)
    }

    fn attr_bool(&self, key: &str) -> Option<bool> {
        self.attr(key).and_then(Value::as_bool)
    }

    fn relationship_id(&self, key: &str) -> Option<String> {
        self.relationships
            .get(key)
            .and_then(|relationship| relationship.data.as_ref())
            .and_then(RelationshipData::single_id)
    }

    fn is_editable_custom_product_page_version(&self) -> bool {
        matches!(
            self.attr_str("state"),
            Some("PREPARE_FOR_SUBMISSION") | Some("REJECTED")
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RelationshipValue {
    #[serde(default)]
    data: Option<RelationshipData>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RelationshipData {
    One(ResourceIdentifier),
    Many(Vec<ResourceIdentifier>),
}

impl RelationshipData {
    fn single_id(&self) -> Option<String> {
        match self {
            Self::One(resource) => Some(resource.id.clone()),
            Self::Many(resources) => resources.first().map(|resource| resource.id.clone()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct JsonApiSingleWithIncluded<T> {
    #[serde(rename = "data")]
    _data: T,
    #[serde(default)]
    included: Vec<RemoteResource>,
}

#[derive(Debug, Deserialize)]
struct JsonApiListNoLinks<T> {
    data: Vec<T>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewScreenshot {
    id: String,
    attributes: MediaAssetAttributes,
}

impl UploadedAsset for ReviewScreenshot {
    fn file_name(&self) -> Option<&str> {
        self.attributes.file_name.as_deref()
    }

    fn checksum(&self) -> Option<&str> {
        self.attributes.source_file_checksum.as_deref()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct EventAsset {
    id: String,
    attributes: MediaAssetAttributes,
}

impl EventAsset {
    fn attr_str(&self, key: &str) -> Option<&str> {
        match key {
            "appEventAssetType" => self.attributes.app_event_asset_type.as_deref(),
            _ => None,
        }
    }
}

impl UploadedAsset for EventAsset {
    fn file_name(&self) -> Option<&str> {
        self.attributes.file_name.as_deref()
    }

    fn checksum(&self) -> Option<&str> {
        self.attributes.source_file_checksum.as_deref()
    }
}

fn event_asset_matches_path(asset: &EventAsset, path: &Path) -> Result<bool> {
    let file_size = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len();
    Ok(
        asset.file_name() == Some(file_name(path)?)
            && asset.attributes.file_size == Some(file_size),
    )
}

#[derive(Debug, Clone, Deserialize)]
struct UploadableAsset {
    id: String,
    attributes: MediaAssetAttributes,
}
