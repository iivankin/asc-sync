use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
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
    #[serde(default)]
    pub apps: BTreeMap<String, AppSpec>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppSpec {
    pub bundle_id_ref: String,
    #[serde(default)]
    pub shared: Option<AppSharedSpec>,
    #[serde(default)]
    pub store_info: Option<AppStoreInfoSpec>,
    #[serde(default)]
    pub availability: Option<AppAvailabilitySpec>,
    #[serde(default)]
    pub pricing: Option<CommercePricingSpec>,
    #[serde(default)]
    pub custom_product_pages: BTreeMap<String, CustomProductPageSpec>,
    #[serde(default)]
    pub in_app_purchases: BTreeMap<String, InAppPurchaseSpec>,
    #[serde(default)]
    pub subscription_groups: BTreeMap<String, SubscriptionGroupSpec>,
    #[serde(default)]
    pub app_events: BTreeMap<String, AppEventSpec>,
    #[serde(default)]
    pub privacy: Option<AppPrivacySpec>,
    pub platforms: BTreeMap<AppPlatform, AppPlatformSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppSharedSpec {
    #[serde(default)]
    pub primary_locale: Option<StringSource>,
    #[serde(default)]
    pub accessibility_url: Option<StringSource>,
    #[serde(default)]
    pub content_rights_declaration: Option<ContentRightsDeclaration>,
    #[serde(default)]
    pub subscription_status_url: Option<StringSource>,
    #[serde(default)]
    pub subscription_status_url_for_sandbox: Option<StringSource>,
    #[serde(default)]
    pub streamlined_purchasing_enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppStoreInfoSpec {
    #[serde(default)]
    pub categories: Option<AppCategoriesSpec>,
    #[serde(default)]
    pub localizations: BTreeMap<String, AppInfoLocalizationSource>,
    #[serde(default)]
    pub age_rating: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppCategoriesSpec {
    #[serde(default)]
    pub primary: Option<String>,
    #[serde(default)]
    pub primary_subcategory_one: Option<String>,
    #[serde(default)]
    pub primary_subcategory_two: Option<String>,
    #[serde(default)]
    pub secondary: Option<String>,
    #[serde(default)]
    pub secondary_subcategory_one: Option<String>,
    #[serde(default)]
    pub secondary_subcategory_two: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppAvailabilitySpec {
    #[serde(default)]
    pub territories: Option<TerritorySelectionSpec>,
    #[serde(default)]
    pub available_in_new_territories: Option<bool>,
    #[serde(default)]
    pub pre_order: Option<AppPreOrderSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TerritorySelectionSpec {
    pub mode: TerritorySelectionMode,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub allow_removals: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerritorySelectionMode {
    All,
    Only,
    Include,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppPreOrderSpec {
    pub enabled: bool,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub publish_date: Option<String>,
    #[serde(default)]
    pub territories: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CommercePricingSpec {
    pub base_territory: String,
    #[serde(default)]
    pub schedule: Vec<PriceScheduleEntrySpec>,
    #[serde(default)]
    pub replace_future_schedule: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PriceScheduleEntrySpec {
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub price: Option<StringSource>,
    #[serde(default)]
    pub price_point_id: Option<String>,
    #[serde(default)]
    pub territory_prices: BTreeMap<String, PricePointSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PricePointSpec {
    #[serde(default)]
    pub price: Option<StringSource>,
    #[serde(default)]
    pub price_point_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CustomProductPageSpec {
    #[serde(default)]
    pub asc_id: Option<String>,
    pub name: StringSource,
    #[serde(default)]
    pub deep_link: Option<StringSource>,
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub localizations: BTreeMap<String, CustomProductPageLocalizationSource>,
    #[serde(default)]
    pub media: BTreeMap<String, AppMediaLocalizationSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum CustomProductPageLocalizationSource {
    Path(PathBuf),
    Inline(CustomProductPageLocalizationSpec),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CustomProductPageLocalizationSpec {
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    #[serde(default)]
    pub promotional_text: Option<StringSource>,
    #[serde(default)]
    pub search_keyword_ids: Vec<String>,
    #[serde(default, flatten)]
    pub render_strings: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct InAppPurchaseSpec {
    #[serde(default)]
    pub asc_id: Option<String>,
    pub product_id: String,
    #[serde(rename = "type")]
    pub kind: InAppPurchaseType,
    pub reference_name: StringSource,
    #[serde(default)]
    pub family_sharable: Option<bool>,
    #[serde(default)]
    pub review_note: Option<StringSource>,
    #[serde(default)]
    pub localizations: BTreeMap<String, CommerceLocalizationSource>,
    #[serde(default)]
    pub pricing: Option<CommercePricingSpec>,
    #[serde(default)]
    pub availability: Option<CommerceAvailabilitySpec>,
    #[serde(default)]
    pub review: Option<CommerceReviewAssetSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InAppPurchaseType {
    Consumable,
    NonConsumable,
    NonRenewingSubscription,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CommerceAvailabilitySpec {
    pub territories: TerritorySelectionSpec,
    #[serde(default)]
    pub available_in_new_territories: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum CommerceLocalizationSource {
    Path(PathBuf),
    Inline(CommerceLocalizationSpec),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CommerceLocalizationSpec {
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    pub name: StringSource,
    #[serde(default)]
    pub description: Option<StringSource>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CommerceReviewAssetSpec {
    #[serde(default)]
    pub screenshot: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SubscriptionGroupSpec {
    #[serde(default)]
    pub asc_id: Option<String>,
    pub reference_name: StringSource,
    #[serde(default)]
    pub localizations: BTreeMap<String, SubscriptionGroupLocalizationSource>,
    #[serde(default)]
    pub subscriptions: BTreeMap<String, SubscriptionSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SubscriptionGroupLocalizationSource {
    Path(PathBuf),
    Inline(SubscriptionGroupLocalizationSpec),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SubscriptionGroupLocalizationSpec {
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    pub name: StringSource,
    #[serde(default)]
    pub custom_app_name: Option<StringSource>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SubscriptionSpec {
    #[serde(default)]
    pub asc_id: Option<String>,
    pub product_id: String,
    pub reference_name: StringSource,
    #[serde(default)]
    pub family_sharable: Option<bool>,
    #[serde(default)]
    pub period: Option<SubscriptionPeriod>,
    #[serde(default)]
    pub group_level: Option<u32>,
    #[serde(default)]
    pub review_note: Option<StringSource>,
    #[serde(default)]
    pub localizations: BTreeMap<String, CommerceLocalizationSource>,
    #[serde(default)]
    pub pricing: Option<CommercePricingSpec>,
    #[serde(default)]
    pub availability: Option<CommerceAvailabilitySpec>,
    #[serde(default)]
    pub review: Option<CommerceReviewAssetSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionPeriod {
    OneWeek,
    OneMonth,
    TwoMonths,
    ThreeMonths,
    SixMonths,
    OneYear,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppEventSpec {
    #[serde(default)]
    pub asc_id: Option<String>,
    pub reference_name: StringSource,
    #[serde(default)]
    pub badge: Option<AppEventBadge>,
    #[serde(default)]
    pub deep_link: Option<StringSource>,
    #[serde(default)]
    pub purchase_requirement: Option<StringSource>,
    #[serde(default)]
    pub primary_locale: Option<String>,
    #[serde(default)]
    pub priority: Option<AppEventPriority>,
    #[serde(default)]
    pub purpose: Option<AppEventPurpose>,
    #[serde(default)]
    pub territory_schedules: Vec<AppEventTerritoryScheduleSpec>,
    #[serde(default)]
    pub localizations: BTreeMap<String, AppEventLocalizationSource>,
    #[serde(default)]
    pub media: BTreeMap<String, AppEventMediaSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEventBadge {
    LiveEvent,
    Premiere,
    Challenge,
    Competition,
    NewSeason,
    MajorUpdate,
    SpecialEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEventPriority {
    High,
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEventPurpose {
    AppropriateForAllUsers,
    AttractNewUsers,
    KeepActiveUsersInformed,
    BringBackLapsedUsers,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppEventTerritoryScheduleSpec {
    #[serde(default)]
    pub territories: Vec<String>,
    pub publish_start: String,
    pub event_start: String,
    pub event_end: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AppEventLocalizationSource {
    Path(PathBuf),
    Inline(Box<AppEventLocalizationSpec>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppEventLocalizationSpec {
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    #[serde(default)]
    pub name: Option<StringSource>,
    #[serde(default)]
    pub short_description: Option<StringSource>,
    #[serde(default)]
    pub long_description: Option<StringSource>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppEventMediaSpec {
    #[serde(default)]
    pub card_image: Option<PathBuf>,
    #[serde(default)]
    pub details_image: Option<PathBuf>,
    #[serde(default)]
    pub card_video: Option<PathBuf>,
    #[serde(default)]
    pub details_video: Option<PathBuf>,
    #[serde(default)]
    pub preview_frame_time_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppPrivacySpec {
    #[serde(default)]
    pub data_types: Vec<AppPrivacyDataTypeSpec>,
    #[serde(default)]
    pub uses_tracking: Option<bool>,
    #[serde(default)]
    pub tracking_domains: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppPrivacyDataTypeSpec {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub linked_to_user: bool,
    #[serde(default)]
    pub tracking: bool,
    #[serde(default)]
    pub purposes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppPlatformSpec {
    pub version: AppVersionSpec,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppVersionSpec {
    pub version_string: String,
    #[serde(default)]
    pub build_number: Option<String>,
    #[serde(default)]
    pub copyright: Option<StringSource>,
    #[serde(default)]
    pub release: Option<AppVersionReleaseSpec>,
    #[serde(default)]
    pub localizations: BTreeMap<String, AppVersionLocalizationSource>,
    #[serde(default)]
    pub review: Option<AppReviewSpec>,
    #[serde(default)]
    pub media: BTreeMap<String, AppMediaLocalizationSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppVersionReleaseSpec {
    #[serde(default, rename = "type")]
    pub kind: Option<AppReleaseType>,
    #[serde(default)]
    pub earliest_release_date: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppReviewSpec {
    #[serde(default)]
    pub contact_first_name: Option<StringSource>,
    #[serde(default)]
    pub contact_last_name: Option<StringSource>,
    #[serde(default)]
    pub contact_phone: Option<StringSource>,
    #[serde(default)]
    pub contact_email: Option<StringSource>,
    #[serde(default)]
    pub demo_account_name: Option<StringSource>,
    #[serde(default)]
    pub demo_account_password: Option<StringSource>,
    #[serde(default)]
    pub demo_account_required: Option<bool>,
    #[serde(default)]
    pub notes: Option<StringSource>,
    #[serde(default)]
    pub attachments: Vec<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppMediaLocalizationSpec {
    #[serde(default)]
    pub screenshots: BTreeMap<MediaScreenshotSet, MediaScreenshotSource>,
    #[serde(default)]
    pub app_previews: BTreeMap<MediaPreviewSet, MediaPathList>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AppInfoLocalizationSource {
    Path(PathBuf),
    Inline(AppInfoLocalizationSpec),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AppInfoLocalizationSpec {
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    pub name: StringSource,
    #[serde(default)]
    pub subtitle: Option<StringSource>,
    #[serde(default)]
    pub privacy_policy_url: Option<StringSource>,
    #[serde(default)]
    pub privacy_choices_url: Option<StringSource>,
    #[serde(default)]
    pub privacy_policy_text: Option<StringSource>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AppVersionLocalizationSource {
    Path(PathBuf),
    Inline(Box<AppVersionLocalizationSpec>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AppVersionLocalizationSpec {
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    #[serde(default)]
    pub description: Option<StringSource>,
    #[serde(default)]
    pub keywords: Option<KeywordsSpec>,
    #[serde(default)]
    pub marketing_url: Option<StringSource>,
    #[serde(default)]
    pub promotional_text: Option<StringSource>,
    #[serde(default)]
    pub support_url: Option<StringSource>,
    #[serde(default)]
    pub whats_new: Option<StringSource>,
    #[serde(default, flatten)]
    pub render_strings: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum KeywordsSpec {
    String(StringSource),
    List(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StringSource {
    Literal(String),
    Env {
        #[serde(rename = "$env")]
        env: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MediaPathList {
    One(PathBuf),
    Many(Vec<PathBuf>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum MediaScreenshotSource {
    Paths(MediaPathList),
    Render(MediaScreenshotRenderSource),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MediaScreenshotRenderSource {
    pub render: MediaScreenshotRenderSpec,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct MediaScreenshotRenderSpec {
    pub template: MediaPathList,
    pub screens: MediaPathList,
    pub frame: String,
    #[serde(default)]
    pub frame_dir: Option<PathBuf>,
    #[serde(default)]
    pub output_dir: Option<PathBuf>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppPlatform {
    Ios,
    MacOs,
    Tvos,
    VisionOs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentRightsDeclaration {
    DoesNotUseThirdPartyContent,
    UsesThirdPartyContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppReleaseType {
    Manual,
    AfterApproval,
    Scheduled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaScreenshotSet {
    Iphone,
    Ipad,
    Mac,
    AppleTv,
    VisionPro,
    Watch,
    Iphone67,
    Iphone65,
    Iphone61,
    Iphone58,
    Iphone55,
    Iphone47,
    Iphone40,
    Iphone35,
    Ipad13,
    Ipad129,
    Ipad11,
    Ipad105,
    Ipad97,
    WatchUltra,
    WatchSeries10,
    WatchSeries7,
    WatchSeries4,
    WatchSeries3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaPreviewSet {
    Iphone,
    IphonePortrait,
    IphoneLandscape,
    Ipad,
    IpadPortrait,
    IpadLandscape,
    Iphone67,
    Iphone65,
    Iphone61,
    Iphone58,
    Iphone55,
    Iphone47,
    Iphone40,
    Iphone35,
    Ipad13,
    Ipad129,
    Ipad11,
    Ipad105,
    Ipad97,
    Desktop,
    Tv,
    Vision,
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

        for (name, app) in &self.apps {
            validate_logical_key("app", name)?;
            ensure!(
                !app.bundle_id_ref.trim().is_empty(),
                "app {name} bundle_id_ref cannot be empty"
            );
            ensure!(
                self.bundle_ids.contains_key(&app.bundle_id_ref),
                "app {name} references unknown bundle_id {}",
                app.bundle_id_ref
            );
            ensure!(
                !app.platforms.is_empty(),
                "app {name} must define at least one platform"
            );
            if let Some(availability) = &app.availability {
                validate_app_availability(name, availability)?;
            }
            if let Some(pricing) = &app.pricing {
                validate_pricing(&format!("app {name}.pricing"), pricing)?;
            }
            for (page_key, page) in &app.custom_product_pages {
                validate_logical_key("custom_product_page", page_key)?;
                validate_string_source(
                    &format!("app {name}.custom_product_pages.{page_key}.name"),
                    &page.name,
                )?;
                validate_optional_string_source(
                    &format!("app {name}.custom_product_pages.{page_key}.deep_link"),
                    page.deep_link.as_ref(),
                )?;
                validate_optional_id(
                    &format!("app {name}.custom_product_pages.{page_key}.asc_id"),
                    page.asc_id.as_deref(),
                )?;
                for (locale, localization) in &page.localizations {
                    validate_locale(
                        &format!("app {name}.custom_product_pages.{page_key}.localizations"),
                        locale,
                    )?;
                    validate_custom_product_page_localization(
                        name,
                        page_key,
                        locale,
                        localization,
                    )?;
                }
                for (locale, media) in &page.media {
                    validate_locale(
                        &format!("app {name}.custom_product_pages.{page_key}.media"),
                        locale,
                    )?;
                    ensure!(
                        page.localizations.contains_key(locale),
                        "app {name}.custom_product_pages.{page_key}.media.{locale} requires localizations.{locale}"
                    );
                    validate_media_aliases(
                        &format!("app {name}.custom_product_pages.{page_key}.media.{locale}"),
                        media,
                    )?;
                }
            }
            for (iap_key, iap) in &app.in_app_purchases {
                validate_logical_key("in_app_purchase", iap_key)?;
                validate_iap(name, iap_key, iap)?;
            }
            for (group_key, group) in &app.subscription_groups {
                validate_logical_key("subscription_group", group_key)?;
                validate_subscription_group(name, group_key, group)?;
            }
            for (event_key, event) in &app.app_events {
                validate_logical_key("app_event", event_key)?;
                validate_app_event(name, event_key, event)?;
            }
            if let Some(privacy) = &app.privacy {
                validate_privacy(name, privacy)?;
            }
            for (platform, platform_spec) in &app.platforms {
                let version = &platform_spec.version;
                ensure!(
                    !version.version_string.trim().is_empty(),
                    "app {name} platform {platform} version_string cannot be empty"
                );
                if let Some(build_number) = &version.build_number {
                    ensure!(
                        !build_number.trim().is_empty(),
                        "app {name} platform {platform} build_number cannot be empty"
                    );
                }
                for (locale, paths) in &version.media {
                    ensure!(
                        !locale.trim().is_empty(),
                        "app {name} platform {platform} media locale cannot be empty"
                    );

                    let mut screenshot_types = BTreeMap::new();
                    for (set, list) in &paths.screenshots {
                        ensure!(
                            !list.is_empty(),
                            "app {name} platform {platform} screenshots list cannot be empty"
                        );
                        if matches!(list, MediaScreenshotSource::Render(_)) {
                            ensure!(
                                version.localizations.contains_key(locale),
                                "app {name} platform {platform} media {locale} screenshots {} render requires version.localizations.{locale}",
                                set.config_key()
                            );
                        }
                        let display_type = set.asc_display_type();
                        ensure!(
                            screenshot_types.insert(display_type, *set).is_none(),
                            "app {name} platform {platform} media {locale} screenshots {} conflicts with another screenshot key for ASC display type {display_type}",
                            set.config_key()
                        );
                    }

                    let mut preview_types = BTreeMap::new();
                    for (set, list) in &paths.app_previews {
                        ensure!(
                            !list.is_empty(),
                            "app {name} platform {platform} app_previews list cannot be empty"
                        );
                        let preview_type = set.asc_preview_type();
                        ensure!(
                            preview_types.insert(preview_type, *set).is_none(),
                            "app {name} platform {platform} media {locale} app_previews {} conflicts with another preview key for ASC preview type {preview_type}",
                            set.config_key()
                        );
                    }
                }
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

impl AppPlatform {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::Ios => "IOS",
            Self::MacOs => "MAC_OS",
            Self::Tvos => "TV_OS",
            Self::VisionOs => "VISION_OS",
        }
    }
}

impl fmt::Display for AppPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ios => write!(f, "ios"),
            Self::MacOs => write!(f, "mac_os"),
            Self::Tvos => write!(f, "tvos"),
            Self::VisionOs => write!(f, "vision_os"),
        }
    }
}

impl ContentRightsDeclaration {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::DoesNotUseThirdPartyContent => "DOES_NOT_USE_THIRD_PARTY_CONTENT",
            Self::UsesThirdPartyContent => "USES_THIRD_PARTY_CONTENT",
        }
    }
}

impl AppCategoriesSpec {
    pub fn is_empty(&self) -> bool {
        self.primary.is_none()
            && self.primary_subcategory_one.is_none()
            && self.primary_subcategory_two.is_none()
            && self.secondary.is_none()
            && self.secondary_subcategory_one.is_none()
            && self.secondary_subcategory_two.is_none()
    }
}

impl AppReleaseType {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::Manual => "MANUAL",
            Self::AfterApproval => "AFTER_APPROVAL",
            Self::Scheduled => "SCHEDULED",
        }
    }
}

impl InAppPurchaseType {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::Consumable => "CONSUMABLE",
            Self::NonConsumable => "NON_CONSUMABLE",
            Self::NonRenewingSubscription => "NON_RENEWING_SUBSCRIPTION",
        }
    }
}

impl SubscriptionPeriod {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::OneWeek => "ONE_WEEK",
            Self::OneMonth => "ONE_MONTH",
            Self::TwoMonths => "TWO_MONTHS",
            Self::ThreeMonths => "THREE_MONTHS",
            Self::SixMonths => "SIX_MONTHS",
            Self::OneYear => "ONE_YEAR",
        }
    }
}

impl AppEventBadge {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::LiveEvent => "LIVE_EVENT",
            Self::Premiere => "PREMIERE",
            Self::Challenge => "CHALLENGE",
            Self::Competition => "COMPETITION",
            Self::NewSeason => "NEW_SEASON",
            Self::MajorUpdate => "MAJOR_UPDATE",
            Self::SpecialEvent => "SPECIAL_EVENT",
        }
    }
}

impl AppEventPriority {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::High => "HIGH",
            Self::Normal => "NORMAL",
        }
    }
}

impl AppEventPurpose {
    pub fn asc_value(self) -> &'static str {
        match self {
            Self::AppropriateForAllUsers => "APPROPRIATE_FOR_ALL_USERS",
            Self::AttractNewUsers => "ATTRACT_NEW_USERS",
            Self::KeepActiveUsersInformed => "KEEP_ACTIVE_USERS_INFORMED",
            Self::BringBackLapsedUsers => "BRING_BACK_LAPSED_USERS",
        }
    }
}

impl MediaScreenshotSet {
    pub fn config_key(self) -> &'static str {
        match self {
            Self::Iphone => "iphone",
            Self::Ipad => "ipad",
            Self::Mac => "mac",
            Self::AppleTv => "apple_tv",
            Self::VisionPro => "vision_pro",
            Self::Watch => "watch",
            Self::Iphone67 => "iphone67",
            Self::Iphone65 => "iphone65",
            Self::Iphone61 => "iphone61",
            Self::Iphone58 => "iphone58",
            Self::Iphone55 => "iphone55",
            Self::Iphone47 => "iphone47",
            Self::Iphone40 => "iphone40",
            Self::Iphone35 => "iphone35",
            Self::Ipad13 => "ipad13",
            Self::Ipad129 => "ipad129",
            Self::Ipad11 => "ipad11",
            Self::Ipad105 => "ipad105",
            Self::Ipad97 => "ipad97",
            Self::WatchUltra => "watch_ultra",
            Self::WatchSeries10 => "watch_series10",
            Self::WatchSeries7 => "watch_series7",
            Self::WatchSeries4 => "watch_series4",
            Self::WatchSeries3 => "watch_series3",
        }
    }

    pub fn asc_display_type(self) -> &'static str {
        match self {
            Self::Iphone | Self::Iphone67 => "APP_IPHONE_67",
            Self::Iphone65 => "APP_IPHONE_65",
            Self::Iphone61 => "APP_IPHONE_61",
            Self::Iphone58 => "APP_IPHONE_58",
            Self::Iphone55 => "APP_IPHONE_55",
            Self::Iphone47 => "APP_IPHONE_47",
            Self::Iphone40 => "APP_IPHONE_40",
            Self::Iphone35 => "APP_IPHONE_35",
            Self::Ipad | Self::Ipad13 => "APP_IPAD_PRO_3GEN_129",
            Self::Ipad129 => "APP_IPAD_PRO_129",
            Self::Ipad11 => "APP_IPAD_PRO_3GEN_11",
            Self::Ipad105 => "APP_IPAD_105",
            Self::Ipad97 => "APP_IPAD_97",
            Self::Mac => "APP_DESKTOP",
            Self::AppleTv => "APP_APPLE_TV",
            Self::VisionPro => "APP_APPLE_VISION_PRO",
            Self::Watch | Self::WatchSeries10 => "APP_WATCH_SERIES_10",
            Self::WatchUltra => "APP_WATCH_ULTRA",
            Self::WatchSeries7 => "APP_WATCH_SERIES_7",
            Self::WatchSeries4 => "APP_WATCH_SERIES_4",
            Self::WatchSeries3 => "APP_WATCH_SERIES_3",
        }
    }
}

impl MediaPreviewSet {
    pub fn config_key(self) -> &'static str {
        match self {
            Self::Iphone => "iphone",
            Self::IphonePortrait => "iphone_portrait",
            Self::IphoneLandscape => "iphone_landscape",
            Self::Ipad => "ipad",
            Self::IpadPortrait => "ipad_portrait",
            Self::IpadLandscape => "ipad_landscape",
            Self::Iphone67 => "iphone67",
            Self::Iphone65 => "iphone65",
            Self::Iphone61 => "iphone61",
            Self::Iphone58 => "iphone58",
            Self::Iphone55 => "iphone55",
            Self::Iphone47 => "iphone47",
            Self::Iphone40 => "iphone40",
            Self::Iphone35 => "iphone35",
            Self::Ipad13 => "ipad13",
            Self::Ipad129 => "ipad129",
            Self::Ipad11 => "ipad11",
            Self::Ipad105 => "ipad105",
            Self::Ipad97 => "ipad97",
            Self::Desktop => "desktop",
            Self::Tv => "tv",
            Self::Vision => "vision",
        }
    }

    pub fn asc_preview_type(self) -> &'static str {
        match self {
            Self::Iphone | Self::IphonePortrait | Self::IphoneLandscape | Self::Iphone67 => {
                "IPHONE_67"
            }
            Self::Iphone65 => "IPHONE_65",
            Self::Iphone61 => "IPHONE_61",
            Self::Iphone58 => "IPHONE_58",
            Self::Iphone55 => "IPHONE_55",
            Self::Iphone47 => "IPHONE_47",
            Self::Iphone40 => "IPHONE_40",
            Self::Iphone35 => "IPHONE_35",
            Self::Ipad | Self::IpadPortrait | Self::IpadLandscape | Self::Ipad13 => {
                "IPAD_PRO_3GEN_129"
            }
            Self::Ipad129 => "IPAD_PRO_129",
            Self::Ipad11 => "IPAD_PRO_3GEN_11",
            Self::Ipad105 => "IPAD_105",
            Self::Ipad97 => "IPAD_97",
            Self::Desktop => "DESKTOP",
            Self::Tv => "APPLE_TV",
            Self::Vision => "APPLE_VISION_PRO",
        }
    }

    pub fn requires_landscape(self) -> bool {
        matches!(self, Self::Desktop | Self::Tv | Self::Vision)
            || matches!(self, Self::IphoneLandscape | Self::IpadLandscape)
    }
}

impl MediaPathList {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::One(path) => path.as_os_str().is_empty(),
            Self::Many(paths) => {
                paths.is_empty() || paths.iter().any(|path| path.as_os_str().is_empty())
            }
        }
    }

    pub fn paths(&self) -> Vec<&PathBuf> {
        match self {
            Self::One(path) => vec![path],
            Self::Many(paths) => paths.iter().collect(),
        }
    }
}

impl MediaScreenshotSource {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Paths(paths) => paths.is_empty(),
            Self::Render(render) => {
                render.render.template.is_empty()
                    || render.render.screens.is_empty()
                    || render.render.frame.trim().is_empty()
                    || render
                        .render
                        .frame_dir
                        .as_ref()
                        .is_some_and(|path| path.as_os_str().is_empty())
                    || render
                        .render
                        .output_dir
                        .as_ref()
                        .is_some_and(|path| path.as_os_str().is_empty())
            }
        }
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

fn validate_locale(context: &str, locale: &str) -> Result<()> {
    ensure!(
        !locale.trim().is_empty(),
        "{context} locale cannot be empty"
    );
    Ok(())
}

fn validate_optional_id(context: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        ensure!(!value.trim().is_empty(), "{context} cannot be empty");
    }
    Ok(())
}

fn validate_string_source(context: &str, source: &StringSource) -> Result<()> {
    match source {
        StringSource::Literal(value) => {
            ensure!(!value.trim().is_empty(), "{context} cannot be empty")
        }
        StringSource::Env { env } => {
            ensure!(!env.trim().is_empty(), "{context} $env cannot be empty")
        }
    }
    Ok(())
}

fn validate_optional_string_source(context: &str, source: Option<&StringSource>) -> Result<()> {
    if let Some(source) = source {
        match source {
            StringSource::Literal(_) => {}
            StringSource::Env { env } => {
                ensure!(!env.trim().is_empty(), "{context} $env cannot be empty");
            }
        }
    }
    Ok(())
}

fn validate_path(context: &str, path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty(),
        "{context} path cannot be empty"
    );
    Ok(())
}

fn validate_territory_id(context: &str, territory: &str) -> Result<()> {
    ensure!(
        !territory.trim().is_empty(),
        "{context} territory cannot be empty"
    );
    ensure!(
        territory.len() == 3 && territory.chars().all(|ch| ch.is_ascii_uppercase()),
        "{context} territory {territory:?} must be a three-letter uppercase App Store territory id like USA"
    );
    Ok(())
}

fn validate_territory_selection(context: &str, selection: &TerritorySelectionSpec) -> Result<()> {
    match selection.mode {
        TerritorySelectionMode::All => ensure!(
            selection.values.is_empty(),
            "{context}.values must be empty when mode is all"
        ),
        TerritorySelectionMode::Only | TerritorySelectionMode::Include => ensure!(
            !selection.values.is_empty(),
            "{context}.values cannot be empty when mode is not all"
        ),
    }
    for territory in &selection.values {
        validate_territory_id(context, territory)?;
    }
    Ok(())
}

fn validate_app_availability(app_name: &str, availability: &AppAvailabilitySpec) -> Result<()> {
    if let Some(selection) = &availability.territories {
        validate_territory_selection(
            &format!("app {app_name}.availability.territories"),
            selection,
        )?;
    }
    if let Some(pre_order) = &availability.pre_order {
        if pre_order.enabled {
            ensure!(
                pre_order
                    .release_date
                    .as_ref()
                    .is_some_and(|date| !date.trim().is_empty()),
                "app {app_name}.availability.pre_order.release_date is required when pre_order is enabled"
            );
        }
        validate_optional_nonempty(
            &format!("app {app_name}.availability.pre_order.release_date"),
            pre_order.release_date.as_deref(),
        )?;
        validate_optional_nonempty(
            &format!("app {app_name}.availability.pre_order.publish_date"),
            pre_order.publish_date.as_deref(),
        )?;
        for territory in &pre_order.territories {
            validate_territory_id(&format!("app {app_name}.availability.pre_order"), territory)?;
        }
    }
    Ok(())
}

fn validate_optional_nonempty(context: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        ensure!(!value.trim().is_empty(), "{context} cannot be empty");
    }
    Ok(())
}

fn validate_pricing(context: &str, pricing: &CommercePricingSpec) -> Result<()> {
    validate_territory_id(
        &format!("{context}.base_territory"),
        &pricing.base_territory,
    )?;
    ensure!(
        !pricing.schedule.is_empty(),
        "{context}.schedule cannot be empty"
    );
    for (index, entry) in pricing.schedule.iter().enumerate() {
        validate_optional_nonempty(
            &format!("{context}.schedule[{index}].start_date"),
            entry.start_date.as_deref(),
        )?;
        validate_optional_nonempty(
            &format!("{context}.schedule[{index}].end_date"),
            entry.end_date.as_deref(),
        )?;
        validate_price_point_choice(
            &format!("{context}.schedule[{index}]"),
            entry.price.as_ref(),
            entry.price_point_id.as_deref(),
        )?;
        for (territory, price) in &entry.territory_prices {
            validate_territory_id(&format!("{context}.schedule[{index}]"), territory)?;
            validate_price_point_choice(
                &format!("{context}.schedule[{index}].territory_prices.{territory}"),
                price.price.as_ref(),
                price.price_point_id.as_deref(),
            )?;
        }
    }
    Ok(())
}

fn validate_price_point_choice(
    context: &str,
    price: Option<&StringSource>,
    price_point_id: Option<&str>,
) -> Result<()> {
    match (price, price_point_id) {
        (Some(price), None) => validate_string_source(&format!("{context}.price"), price),
        (None, Some(price_point_id)) => {
            ensure!(
                !price_point_id.trim().is_empty(),
                "{context}.price_point_id cannot be empty"
            );
            Ok(())
        }
        (Some(_), Some(_)) => bail!("{context} must not define both price and price_point_id"),
        (None, None) => bail!("{context} must define price or price_point_id"),
    }
}

fn validate_custom_product_page_localization(
    app_name: &str,
    page_key: &str,
    locale: &str,
    source: &CustomProductPageLocalizationSource,
) -> Result<()> {
    match source {
        CustomProductPageLocalizationSource::Path(path) => validate_path(
            &format!("app {app_name}.custom_product_pages.{page_key}.localizations.{locale}"),
            path,
        ),
        CustomProductPageLocalizationSource::Inline(spec) => {
            validate_optional_string_source(
                &format!(
                    "app {app_name}.custom_product_pages.{page_key}.localizations.{locale}.promotional_text"
                ),
                spec.promotional_text.as_ref(),
            )?;
            for keyword_id in &spec.search_keyword_ids {
                ensure!(
                    !keyword_id.trim().is_empty(),
                    "app {app_name}.custom_product_pages.{page_key}.localizations.{locale}.search_keyword_ids cannot contain an empty id"
                );
            }
            Ok(())
        }
    }
}

fn validate_iap(app_name: &str, iap_key: &str, iap: &InAppPurchaseSpec) -> Result<()> {
    validate_optional_id(
        &format!("app {app_name}.in_app_purchases.{iap_key}.asc_id"),
        iap.asc_id.as_deref(),
    )?;
    ensure!(
        !iap.product_id.trim().is_empty(),
        "app {app_name}.in_app_purchases.{iap_key}.product_id cannot be empty"
    );
    validate_string_source(
        &format!("app {app_name}.in_app_purchases.{iap_key}.reference_name"),
        &iap.reference_name,
    )?;
    validate_optional_string_source(
        &format!("app {app_name}.in_app_purchases.{iap_key}.review_note"),
        iap.review_note.as_ref(),
    )?;
    validate_commerce_children(
        &format!("app {app_name}.in_app_purchases.{iap_key}"),
        &iap.localizations,
        iap.pricing.as_ref(),
        iap.availability.as_ref(),
        iap.review.as_ref(),
    )?;
    Ok(())
}

fn validate_subscription_group(
    app_name: &str,
    group_key: &str,
    group: &SubscriptionGroupSpec,
) -> Result<()> {
    validate_optional_id(
        &format!("app {app_name}.subscription_groups.{group_key}.asc_id"),
        group.asc_id.as_deref(),
    )?;
    validate_string_source(
        &format!("app {app_name}.subscription_groups.{group_key}.reference_name"),
        &group.reference_name,
    )?;
    for (locale, source) in &group.localizations {
        validate_locale(
            &format!("app {app_name}.subscription_groups.{group_key}.localizations"),
            locale,
        )?;
        match source {
            SubscriptionGroupLocalizationSource::Path(path) => validate_path(
                &format!("app {app_name}.subscription_groups.{group_key}.localizations.{locale}"),
                path,
            )?,
            SubscriptionGroupLocalizationSource::Inline(spec) => {
                validate_string_source(
                    &format!(
                        "app {app_name}.subscription_groups.{group_key}.localizations.{locale}.name"
                    ),
                    &spec.name,
                )?;
                validate_optional_string_source(
                    &format!(
                        "app {app_name}.subscription_groups.{group_key}.localizations.{locale}.custom_app_name"
                    ),
                    spec.custom_app_name.as_ref(),
                )?;
            }
        }
    }
    for (subscription_key, subscription) in &group.subscriptions {
        validate_logical_key("subscription", subscription_key)?;
        validate_optional_id(
            &format!(
                "app {app_name}.subscription_groups.{group_key}.subscriptions.{subscription_key}.asc_id"
            ),
            subscription.asc_id.as_deref(),
        )?;
        ensure!(
            !subscription.product_id.trim().is_empty(),
            "app {app_name}.subscription_groups.{group_key}.subscriptions.{subscription_key}.product_id cannot be empty"
        );
        validate_string_source(
            &format!(
                "app {app_name}.subscription_groups.{group_key}.subscriptions.{subscription_key}.reference_name"
            ),
            &subscription.reference_name,
        )?;
        if let Some(group_level) = subscription.group_level {
            ensure!(
                group_level > 0,
                "app {app_name}.subscription_groups.{group_key}.subscriptions.{subscription_key}.group_level must be greater than zero"
            );
        }
        validate_optional_string_source(
            &format!(
                "app {app_name}.subscription_groups.{group_key}.subscriptions.{subscription_key}.review_note"
            ),
            subscription.review_note.as_ref(),
        )?;
        validate_commerce_children(
            &format!(
                "app {app_name}.subscription_groups.{group_key}.subscriptions.{subscription_key}"
            ),
            &subscription.localizations,
            subscription.pricing.as_ref(),
            subscription.availability.as_ref(),
            subscription.review.as_ref(),
        )?;
    }
    Ok(())
}

fn validate_commerce_children(
    context: &str,
    localizations: &BTreeMap<String, CommerceLocalizationSource>,
    pricing: Option<&CommercePricingSpec>,
    availability: Option<&CommerceAvailabilitySpec>,
    review: Option<&CommerceReviewAssetSpec>,
) -> Result<()> {
    for (locale, source) in localizations {
        validate_locale(&format!("{context}.localizations"), locale)?;
        match source {
            CommerceLocalizationSource::Path(path) => {
                validate_path(&format!("{context}.localizations.{locale}"), path)?
            }
            CommerceLocalizationSource::Inline(spec) => {
                validate_string_source(
                    &format!("{context}.localizations.{locale}.name"),
                    &spec.name,
                )?;
                validate_optional_string_source(
                    &format!("{context}.localizations.{locale}.description"),
                    spec.description.as_ref(),
                )?;
            }
        }
    }
    if let Some(pricing) = pricing {
        validate_pricing(&format!("{context}.pricing"), pricing)?;
    }
    if let Some(availability) = availability {
        validate_territory_selection(
            &format!("{context}.availability.territories"),
            &availability.territories,
        )?;
    }
    if let Some(review) = review
        && let Some(screenshot) = &review.screenshot
    {
        validate_path(&format!("{context}.review.screenshot"), screenshot)?;
    }
    Ok(())
}

fn validate_app_event(app_name: &str, event_key: &str, event: &AppEventSpec) -> Result<()> {
    validate_optional_id(
        &format!("app {app_name}.app_events.{event_key}.asc_id"),
        event.asc_id.as_deref(),
    )?;
    validate_string_source(
        &format!("app {app_name}.app_events.{event_key}.reference_name"),
        &event.reference_name,
    )?;
    validate_optional_string_source(
        &format!("app {app_name}.app_events.{event_key}.deep_link"),
        event.deep_link.as_ref(),
    )?;
    validate_optional_string_source(
        &format!("app {app_name}.app_events.{event_key}.purchase_requirement"),
        event.purchase_requirement.as_ref(),
    )?;
    validate_optional_nonempty(
        &format!("app {app_name}.app_events.{event_key}.primary_locale"),
        event.primary_locale.as_deref(),
    )?;
    for (index, schedule) in event.territory_schedules.iter().enumerate() {
        validate_optional_nonempty(
            &format!(
                "app {app_name}.app_events.{event_key}.territory_schedules[{index}].publish_start"
            ),
            Some(schedule.publish_start.as_str()),
        )?;
        validate_optional_nonempty(
            &format!(
                "app {app_name}.app_events.{event_key}.territory_schedules[{index}].event_start"
            ),
            Some(schedule.event_start.as_str()),
        )?;
        validate_optional_nonempty(
            &format!(
                "app {app_name}.app_events.{event_key}.territory_schedules[{index}].event_end"
            ),
            Some(schedule.event_end.as_str()),
        )?;
        for territory in &schedule.territories {
            validate_territory_id(
                &format!("app {app_name}.app_events.{event_key}.territory_schedules[{index}]"),
                territory,
            )?;
        }
    }
    for (locale, source) in &event.localizations {
        validate_locale(
            &format!("app {app_name}.app_events.{event_key}.localizations"),
            locale,
        )?;
        match source {
            AppEventLocalizationSource::Path(path) => validate_path(
                &format!("app {app_name}.app_events.{event_key}.localizations.{locale}"),
                path,
            )?,
            AppEventLocalizationSource::Inline(spec) => {
                validate_optional_string_source(
                    &format!("app {app_name}.app_events.{event_key}.localizations.{locale}.name"),
                    spec.name.as_ref(),
                )?;
                validate_optional_string_source(
                    &format!(
                        "app {app_name}.app_events.{event_key}.localizations.{locale}.short_description"
                    ),
                    spec.short_description.as_ref(),
                )?;
                validate_optional_string_source(
                    &format!(
                        "app {app_name}.app_events.{event_key}.localizations.{locale}.long_description"
                    ),
                    spec.long_description.as_ref(),
                )?;
            }
        }
    }
    for (locale, media) in &event.media {
        validate_locale(
            &format!("app {app_name}.app_events.{event_key}.media"),
            locale,
        )?;
        ensure!(
            event.localizations.contains_key(locale),
            "app {app_name}.app_events.{event_key}.media.{locale} requires localizations.{locale}"
        );
        validate_app_event_media_paths(
            &format!("app {app_name}.app_events.{event_key}.media.{locale}"),
            media,
        )?;
    }
    Ok(())
}

fn validate_app_event_media_paths(context: &str, media: &AppEventMediaSpec) -> Result<()> {
    validate_optional_path(&format!("{context}.card_image"), media.card_image.as_ref())?;
    validate_optional_path(
        &format!("{context}.details_image"),
        media.details_image.as_ref(),
    )?;
    validate_optional_path(&format!("{context}.card_video"), media.card_video.as_ref())?;
    validate_optional_path(
        &format!("{context}.details_video"),
        media.details_video.as_ref(),
    )?;
    Ok(())
}

fn validate_optional_path(context: &str, path: Option<&PathBuf>) -> Result<()> {
    if let Some(path) = path {
        validate_path(context, path)?;
    }
    Ok(())
}

fn validate_privacy(app_name: &str, privacy: &AppPrivacySpec) -> Result<()> {
    for (index, data_type) in privacy.data_types.iter().enumerate() {
        ensure!(
            !data_type.kind.trim().is_empty(),
            "app {app_name}.privacy.data_types[{index}].type cannot be empty"
        );
        ensure!(
            !data_type.purposes.is_empty(),
            "app {app_name}.privacy.data_types[{index}].purposes cannot be empty"
        );
        for purpose in &data_type.purposes {
            ensure!(
                !purpose.trim().is_empty(),
                "app {app_name}.privacy.data_types[{index}].purposes cannot contain an empty purpose"
            );
        }
    }
    for domain in &privacy.tracking_domains {
        ensure!(
            !domain.trim().is_empty(),
            "app {app_name}.privacy.tracking_domains cannot contain an empty domain"
        );
    }
    Ok(())
}

fn validate_media_aliases(context: &str, media: &AppMediaLocalizationSpec) -> Result<()> {
    let mut screenshot_types = BTreeMap::new();
    for (set, list) in &media.screenshots {
        ensure!(
            !list.is_empty(),
            "{context}.screenshots list cannot be empty"
        );
        let display_type = set.asc_display_type();
        ensure!(
            screenshot_types.insert(display_type, *set).is_none(),
            "{context}.screenshots {} conflicts with another screenshot key for ASC display type {display_type}",
            set.config_key()
        );
    }

    let mut preview_types = BTreeMap::new();
    for (set, list) in &media.app_previews {
        ensure!(
            !list.is_empty(),
            "{context}.app_previews list cannot be empty"
        );
        let preview_type = set.asc_preview_type();
        ensure!(
            preview_types.insert(preview_type, *set).is_none(),
            "{context}.app_previews {} conflicts with another preview key for ASC preview type {preview_type}",
            set.config_key()
        );
    }
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
    use std::path::Path;

    use super::{
        CertificateKind, Config, CustomProductPageLocalizationSource, MediaScreenshotSource,
    };

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
    fn accepts_app_store_version_config() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": {
                        "bundle_id": "com.example.app",
                        "name": "App",
                        "platform": "ios"
                    }
                },
                "apps": {
                    "main": {
                        "bundle_id_ref": "main",
                        "shared": {
                            "primary_locale": "en-US",
                            "content_rights_declaration": "does_not_use_third_party_content"
                        },
                        "platforms": {
                            "ios": {
                                "version": {
                                    "version_string": "1.2.3",
                                    "build_number": "42",
                                    "release": {
                                        "type": "manual"
                                    },
                                    "localizations": {
                                        "en-US": "./locale/en-US.json5"
                                    },
                                    "review": {
                                        "contact_email": { "$env": "ASC_REVIEW_EMAIL" },
                                        "demo_account_required": false
                                    },
                                    "media": {
                                        "en-US": {
                                            "screenshots": {
                                                "iphone": "./media/iphone/*.png"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(config.apps["main"].platforms.len(), 1);
    }

    #[test]
    fn accepts_rendered_app_store_screenshots() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": {
                        "bundle_id": "com.example.app",
                        "name": "App",
                        "platform": "ios"
                    }
                },
                "apps": {
                    "main": {
                        "bundle_id_ref": "main",
                        "platforms": {
                            "ios": {
                                "version": {
                                    "version_string": "1.2.3",
                                    "localizations": {
                                        "en-US": {
                                            "description": "Description",
                                            "hero": { "title": "Title" }
                                        }
                                    },
                                    "media": {
                                        "en-US": {
                                            "screenshots": {
                                                "iphone67": {
                                                    "render": {
                                                        "template": "./screenshots/app-store/*.html",
                                                        "screens": "./screens/en-US/*.png",
                                                        "frame": "iPhone 16 Pro - Black Titanium - Portrait",
                                                        "output_dir": "./media/en-US/iphone67"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
        let source = &config.apps["main"].platforms[&super::AppPlatform::Ios]
            .version
            .media["en-US"]
            .screenshots[&super::MediaScreenshotSet::Iphone67];
        let render = match source {
            MediaScreenshotSource::Render(render) => render,
            MediaScreenshotSource::Paths(_) => panic!("expected rendered screenshots"),
        };
        assert_eq!(
            render.render.output_dir.as_deref(),
            Some(Path::new("./media/en-US/iphone67"))
        );
        let localization = match &config.apps["main"].platforms[&super::AppPlatform::Ios]
            .version
            .localizations["en-US"]
        {
            super::AppVersionLocalizationSource::Inline(localization) => localization,
            super::AppVersionLocalizationSource::Path(_) => panic!("expected inline localization"),
        };
        assert!(localization.render_strings.contains_key("hero"));
    }

    #[test]
    fn accepts_extended_app_store_resource_config() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": {
                        "bundle_id": "com.example.app",
                        "name": "App",
                        "platform": "ios"
                    }
                },
                "apps": {
                    "main": {
                        "bundle_id_ref": "main",
                        "availability": {
                            "territories": { "mode": "include", "values": ["USA"] }
                        },
                        "pricing": {
                            "base_territory": "USA",
                            "schedule": [{ "price": "0.99" }]
                        },
                        "custom_product_pages": {
                            "summer": {
                                "name": "Summer",
                                "visible": true,
                                "localizations": {
                                    "en-US": {
                                        "promotional_text": "Try the summer flow",
                                        "search_keyword_ids": ["kw-1"],
                                        "headline": "Summer headline"
                                    }
                                },
                                "media": {
                                    "en-US": {
                                        "screenshots": {
                                            "iphone67": {
                                                "render": {
                                                    "template": "./screenshots/cpp/summer/*.html",
                                                    "screens": "./screens/en-US/*.png",
                                                    "frame": "iPhone 16 Pro - Black Titanium - Portrait",
                                                    "output_dir": "./media/cpp/summer/en-US/iphone67"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        "in_app_purchases": {
                            "coins": {
                                "product_id": "com.example.app.coins",
                                "type": "consumable",
                                "reference_name": "Coins",
                                "review_note": { "$env": "IAP_REVIEW_NOTE" },
                                "localizations": {
                                    "en-US": { "name": "Coins", "description": "Coin pack" }
                                },
                                "availability": {
                                    "territories": { "mode": "only", "values": ["USA"] }
                                }
                            }
                        },
                        "subscription_groups": {
                            "premium": {
                                "reference_name": "Premium",
                                "localizations": {
                                    "en-US": { "name": "Premium" }
                                },
                                "subscriptions": {
                                    "monthly": {
                                        "product_id": "com.example.app.premium.monthly",
                                        "reference_name": "Premium Monthly",
                                        "period": "one_month",
                                        "group_level": 1,
                                        "localizations": {
                                            "en-US": { "name": "Monthly", "description": "Full access" }
                                        }
                                    }
                                }
                            }
                        },
                        "app_events": {
                            "launch": {
                                "reference_name": "Launch Event",
                                "badge": "challenge",
                                "priority": "normal",
                                "purpose": "attract_new_users",
                                "territory_schedules": [{
                                    "territories": ["USA"],
                                    "publish_start": "2026-06-01T10:00:00Z",
                                    "event_start": "2026-06-02T10:00:00Z",
                                    "event_end": "2026-06-10T10:00:00Z"
                                }],
                                "localizations": {
                                    "en-US": {
                                        "name": "Launch Event",
                                        "short_description": "Try the launch event",
                                        "long_description": "Complete the launch challenge."
                                    }
                                },
                                "media": {
                                    "en-US": {
                                        "card_image": "./events/launch/card.png"
                                    }
                                }
                            }
                        },
                        "privacy": {
                            "uses_tracking": false,
                            "data_types": [{
                                "type": "precise_location",
                                "linked_to_user": true,
                                "tracking": false,
                                "purposes": ["app_functionality"]
                            }]
                        },
                        "platforms": {
                            "ios": {
                                "version": {
                                    "version_string": "1.2.3"
                                }
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        config.validate().unwrap();
        assert_eq!(config.apps["main"].custom_product_pages.len(), 1);
        let CustomProductPageLocalizationSource::Inline(localization) =
            &config.apps["main"].custom_product_pages["summer"].localizations["en-US"]
        else {
            panic!("expected inline custom product page localization");
        };
        assert!(localization.render_strings.contains_key("headline"));
        assert!(
            config.apps["main"].custom_product_pages["summer"]
                .media
                .contains_key("en-US")
        );
        assert_eq!(config.apps["main"].in_app_purchases.len(), 1);
        assert_eq!(config.apps["main"].subscription_groups.len(), 1);
        assert_eq!(config.apps["main"].app_events.len(), 1);
        assert!(
            config.apps["main"].app_events["launch"]
                .media
                .contains_key("en-US")
        );
    }

    #[test]
    fn rejects_duplicate_media_aliases() {
        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": {
                        "bundle_id": "com.example.app",
                        "name": "App",
                        "platform": "ios"
                    }
                },
                "apps": {
                    "main": {
                        "bundle_id_ref": "main",
                        "platforms": {
                            "ios": {
                                "version": {
                                    "version_string": "1.2.3",
                                    "media": {
                                        "en-US": {
                                            "screenshots": {
                                                "iphone": "./media/iphone/*.png",
                                                "iphone67": "./media/iphone67/*.png"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("ASC display type APP_IPHONE_67"));

        let config = serde_json::from_str::<Config>(
            r#"{
                "team_id": "TEAM123",
                "bundle_ids": {
                    "main": {
                        "bundle_id": "com.example.app",
                        "name": "App",
                        "platform": "ios"
                    }
                },
                "apps": {
                    "main": {
                        "bundle_id_ref": "main",
                        "platforms": {
                            "ios": {
                                "version": {
                                    "version_string": "1.2.3",
                                    "media": {
                                        "en-US": {
                                            "app_previews": {
                                                "iphone_portrait": "./media/portrait/*.mp4",
                                                "iphone_landscape": "./media/landscape/*.mp4"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("ASC preview type IPHONE_67"));
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
