use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::config::Config;

pub fn load_config(path: &Path) -> Result<Config> {
    let data =
        fs::read(path).with_context(|| format!("failed to read config {}", path.display()))?;
    serde_json::from_slice::<Config>(&data)
        .with_context(|| format!("failed to parse config {}", path.display()))
}

pub fn load_config_value(path: &Path) -> Result<Value> {
    let data =
        fs::read(path).with_context(|| format!("failed to read config {}", path.display()))?;
    serde_json::from_slice::<Value>(&data)
        .with_context(|| format!("failed to parse config {}", path.display()))
}

pub fn write_pretty_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let data = serde_json::to_vec_pretty(value).context("failed to serialize config JSON")?;
    let mut temp = NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))
        .context("failed to create temporary config file")?;
    std::io::Write::write_all(&mut temp, &data).context("failed to write temporary config")?;
    std::io::Write::write_all(&mut temp, b"\n").context("failed to finalize config file")?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to persist config {}", path.display()))?;
    Ok(())
}
