use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};

#[derive(Debug, Parser)]
#[command(
    name = "asc-sync",
    version,
    about = "Terraform-like App Store Connect provisioning sync"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(subcommand)]
    Auth(AuthCommand),
    #[command(subcommand)]
    Device(DeviceCommand),
    Init(InitArgs),
    Notarize(NotarizeArgs),
    Plan(RunArgs),
    Apply(RunArgs),
    Revoke(RevokeArgs),
    Submit(SubmitArgs),
    SubmitForReview(SubmitForReviewArgs),
    Validate(ValidateArgs),
    #[command(subcommand)]
    Metadata(MetadataCommand),
    #[command(subcommand)]
    Media(MediaCommand),
    #[command(subcommand)]
    Signing(SigningCommand),
}

#[derive(Debug, Args, Clone)]
pub struct SigningArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum SigningCommand {
    Import(SigningArgs),
    /// Print signing bundle contents without revealing passwords.
    Inspect(SigningInspectArgs),
    PrintBuildSettings(SigningArgs),
    /// Reuse matching certificates from another same-team signing bundle.
    Adopt(SigningAdoptArgs),
    Merge(SigningMergeArgs),
}

#[derive(Debug, Args, Clone)]
pub struct SigningInspectArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
    #[arg(
        long,
        value_name = "FILE",
        value_hint = ValueHint::FilePath,
        help = "Inspect this bundle instead of the config-adjacent signing.ascbundle."
    )]
    pub from: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub struct SigningAdoptArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
    #[arg(
        long,
        value_name = "FILE",
        value_hint = ValueHint::FilePath,
        help = "Source signing.ascbundle to adopt reusable certificates from."
    )]
    pub from: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    Import,
}

#[derive(Debug, Args, Clone)]
pub struct InitArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath, default_value = "asc.json")]
    pub config: PathBuf,
    #[arg(long, value_name = "TEAM_ID")]
    pub team_id: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    Add(DeviceAddArgs),
    AddLocal(DeviceAddLocalArgs),
}

#[derive(Debug, Subcommand)]
pub enum MetadataCommand {
    #[command(subcommand)]
    Keywords(MetadataKeywordsCommand),
}

#[derive(Debug, Subcommand)]
pub enum MetadataKeywordsCommand {
    /// Audit live App Store version keyword metadata across locales.
    Audit(MetadataKeywordsAuditArgs),
}

#[derive(Debug, Args, Clone)]
pub struct MetadataKeywordsAuditArgs {
    #[arg(long, value_name = "APP_ID")]
    pub app: String,
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
    #[arg(long = "version-id", value_name = "VERSION_ID")]
    pub version_id: Option<String>,
    #[arg(long = "app-info", value_name = "APP_INFO_ID")]
    pub app_info: Option<String>,
    #[arg(long, value_name = "PLATFORM")]
    pub platform: Option<String>,
    #[arg(long = "team-id", value_name = "TEAM_ID")]
    pub team_id: Option<String>,
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: Option<PathBuf>,
    #[arg(long = "blocked-term", value_name = "TERM")]
    pub blocked_terms: Vec<String>,
    #[arg(
        long = "blocked-terms-file",
        value_name = "FILE",
        value_hint = ValueHint::FilePath
    )]
    pub blocked_terms_file: Option<PathBuf>,
    #[arg(long, default_value_t = false)]
    pub strict: bool,
    #[arg(long, value_enum, default_value = "json")]
    pub output: MetadataOutputArg,
    #[arg(long, default_value_t = false)]
    pub pretty: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MetadataOutputArg {
    Json,
    Table,
    Markdown,
}

#[derive(Debug, Args, Clone)]
pub struct RunArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
}

#[derive(Debug, Args, Clone)]
pub struct NotarizeArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub file: PathBuf,
}

#[derive(Debug, Args, Clone)]
pub struct SubmitArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub file: PathBuf,
    #[arg(long = "bundle-id", value_name = "LOGICAL_ID")]
    pub bundle_id: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub struct SubmitForReviewArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
    #[arg(long, value_name = "APP_LOGICAL_ID")]
    pub app: Option<String>,
    #[arg(long, value_enum)]
    pub platform: Option<AppPlatformArg>,
}

#[derive(Debug, Subcommand)]
pub enum MediaCommand {
    /// Validate App Store media referenced by config.
    Validate(MediaValidateArgs),
    /// Render PNG screenshots from HTML templates.
    Render(MediaRenderArgs),
    /// Serve an HTML preview page for screenshot templates.
    Preview(MediaPreviewArgs),
}

#[derive(Debug, Args, Clone)]
pub struct MediaValidateArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
}

#[derive(Debug, Args, Clone)]
pub struct MediaRenderArgs {
    #[arg(
        long,
        value_name = "HTML_OR_GLOB",
        required = true,
        value_hint = ValueHint::AnyPath,
        help = "HTML file, directory, or glob pattern to render"
    )]
    pub input: Vec<PathBuf>,
    #[arg(long, value_name = "DIR", value_hint = ValueHint::DirPath, help = "Directory for rendered PNG files")]
    pub output_dir: PathBuf,
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with = "viewport",
        help = "Named App Store viewport, for example iphone67 or ipad13"
    )]
    pub size: Option<String>,
    #[arg(
        long,
        value_name = "WIDTHxHEIGHT",
        conflicts_with = "size",
        help = "Exact viewport size, for example 1320x2868"
    )]
    pub viewport: Option<String>,
    #[arg(
        long,
        value_name = "NAME",
        requires = "screen",
        help = "Device frame name to wrap around each HTML template"
    )]
    pub frame: Option<String>,
    #[arg(
        long,
        value_name = "IMAGE_OR_GLOB",
        value_hint = ValueHint::AnyPath,
        requires = "frame",
        help = "Screen image file, directory, or glob pattern to place inside --frame"
    )]
    pub screen: Vec<PathBuf>,
    #[arg(
        long,
        value_name = "DIR",
        value_hint = ValueHint::DirPath,
        requires = "frame",
        help = "Directory with device frame PNG files plus Frames.json"
    )]
    pub frame_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "LOCALE",
        help = "Locale code exposed to the HTML template"
    )]
    pub locale: Option<String>,
    #[arg(
        long,
        value_name = "JSON_OR_JSON5",
        value_hint = ValueHint::FilePath,
        help = "Localization/template strings file used for {{key}} substitutions"
    )]
    pub strings: Option<PathBuf>,
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath, help = "Path to Chrome or Chromium")]
    pub chrome: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub struct MediaPreviewArgs {
    #[arg(
        long,
        value_name = "HTML_OR_GLOB",
        required = true,
        value_hint = ValueHint::AnyPath,
        help = "HTML file, directory, or glob pattern to preview"
    )]
    pub input: Vec<PathBuf>,
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with = "viewport",
        help = "Named App Store viewport, for example iphone67 or ipad13"
    )]
    pub size: Option<String>,
    #[arg(
        long,
        value_name = "WIDTHxHEIGHT",
        conflicts_with = "size",
        help = "Exact viewport size, for example 1320x2868"
    )]
    pub viewport: Option<String>,
    #[arg(
        long,
        value_name = "NAME",
        requires = "screen",
        help = "Device frame name to wrap around each HTML template"
    )]
    pub frame: Option<String>,
    #[arg(
        long,
        value_name = "IMAGE_OR_GLOB",
        value_hint = ValueHint::AnyPath,
        requires = "frame",
        help = "Screen image file, directory, or glob pattern to place inside --frame"
    )]
    pub screen: Vec<PathBuf>,
    #[arg(
        long,
        value_name = "DIR",
        value_hint = ValueHint::DirPath,
        requires = "frame",
        help = "Directory with device frame PNG files plus Frames.json"
    )]
    pub frame_dir: Option<PathBuf>,
    #[arg(
        long,
        value_name = "LOCALE",
        help = "Locale code exposed to the HTML template"
    )]
    pub locale: Option<String>,
    #[arg(
        long,
        value_name = "JSON_OR_JSON5",
        value_hint = ValueHint::FilePath,
        help = "Localization/template strings file used for {{key}} substitutions"
    )]
    pub strings: Option<PathBuf>,
    #[arg(long, default_value_t = 5173, help = "Local preview server port")]
    pub port: u16,
    #[arg(
        long,
        default_value_t = false,
        help = "Open the preview URL in the default browser"
    )]
    pub open: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AppPlatformArg {
    #[value(name = "ios")]
    Ios,
    #[value(name = "mac_os")]
    MacOs,
    #[value(name = "tvos")]
    Tvos,
    #[value(name = "vision_os")]
    VisionOs,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RevokeTarget {
    Dev,
    Release,
    All,
}

#[derive(Debug, Args, Clone)]
pub struct RevokeArgs {
    #[arg(value_enum)]
    pub target: RevokeTarget,
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DeviceFamilyArg {
    #[value(name = "ios")]
    Ios,
    #[value(name = "ipados")]
    Ipados,
    #[value(name = "watchos")]
    Watchos,
    #[value(name = "tvos")]
    Tvos,
    #[value(name = "visionos")]
    Visionos,
    #[value(name = "macos")]
    Macos,
}

#[derive(Debug, Args, Clone)]
pub struct DeviceAddArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
    #[arg(long, value_name = "NAME")]
    pub name: String,
    #[arg(long, value_name = "LOGICAL_ID")]
    pub id: Option<String>,
    #[arg(long, value_enum)]
    pub family: Option<DeviceFamilyArg>,
    #[arg(long, default_value_t = false)]
    pub apply: bool,
    #[arg(long, default_value_t = 300)]
    pub timeout_seconds: u64,
}

#[derive(Debug, Args, Clone)]
pub struct DeviceAddLocalArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    #[arg(long, value_name = "LOGICAL_ID")]
    pub id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub current_mac: bool,
    #[arg(long, value_enum)]
    pub family: Option<DeviceFamilyArg>,
    #[arg(long, value_name = "UDID")]
    pub udid: Option<String>,
    #[arg(long, default_value_t = false)]
    pub apply: bool,
}

#[derive(Debug, Args, Clone)]
pub struct ValidateArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
}

#[derive(Debug, Args, Clone)]
pub struct SigningMergeArgs {
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub config: PathBuf,
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub base: PathBuf,
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub ours: PathBuf,
    #[arg(long, value_name = "FILE", value_hint = ValueHint::FilePath)]
    pub theirs: PathBuf,
}
