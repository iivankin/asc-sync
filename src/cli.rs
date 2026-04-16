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
    Validate(ValidateArgs),
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
    PrintBuildSettings(SigningArgs),
    Merge(SigningMergeArgs),
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
