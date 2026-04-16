use std::path::Path;

use anyhow::{Result, ensure};
use serde_json::{Map, Value, json};

use crate::{config::DeviceFamily, config_io};

pub fn upsert_device(
    config_path: &Path,
    logical_name: &str,
    display_name: &str,
    family: DeviceFamily,
    udid: &str,
) -> Result<()> {
    ensure!(
        !logical_name.trim().is_empty(),
        "device logical name cannot be empty"
    );
    ensure!(
        !display_name.trim().is_empty(),
        "device name cannot be empty"
    );
    ensure!(!udid.trim().is_empty(), "device udid cannot be empty");

    let mut root = config_io::load_config_value(config_path)?;
    let root_object = root
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config root must be a JSON object"))?;

    let devices = root_object
        .entry("devices".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config devices field must be an object"))?;

    devices.insert(
        logical_name.to_owned(),
        json!({
            "family": family.to_string(),
            "udid": udid,
            "name": display_name,
        }),
    );

    config_io::write_pretty_json(config_path, &root)
}
