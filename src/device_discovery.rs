use std::{collections::BTreeMap, process::Command};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;

use crate::config::DeviceFamily;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedDevice {
    pub name: String,
    pub udid: String,
    pub family: DeviceFamily,
}

pub fn detect_current_mac() -> Result<DetectedDevice> {
    let output = Command::new("system_profiler")
        .arg("SPHardwareDataType")
        .arg("-json")
        .output()
        .context("failed to execute system_profiler")?;
    ensure!(
        output.status.success(),
        "system_profiler failed with status {}",
        output.status
    );

    let root: Value = serde_json::from_slice(&output.stdout)
        .context("failed to parse system_profiler JSON output")?;
    let overview = root
        .get("SPHardwareDataType")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| anyhow::anyhow!("system_profiler did not return SPHardwareDataType"))?;

    let udid = overview
        .get("provisioning_UDID")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("current Mac does not expose provisioning_UDID"))?
        .to_owned();
    let name = overview
        .get("machine_name")
        .and_then(Value::as_str)
        .unwrap_or("Current Mac")
        .to_owned();

    Ok(DetectedDevice {
        name,
        udid,
        family: DeviceFamily::Macos,
    })
}

pub fn discover_local_devices() -> Result<Vec<DetectedDevice>> {
    let mut devices = BTreeMap::<String, DetectedDevice>::new();
    let mut errors = Vec::new();

    match discover_with_devicectl() {
        Ok(found) => merge_devices(&mut devices, found),
        Err(error) => errors.push(format!("devicectl: {error:#}")),
    }

    match discover_with_xcdevice() {
        Ok(found) => merge_devices(&mut devices, found),
        Err(error) => errors.push(format!("xcdevice: {error:#}")),
    }

    if let Ok(current_mac) = detect_current_mac() {
        devices
            .entry(current_mac.udid.clone())
            .or_insert(current_mac);
    }

    if devices.is_empty() {
        let details = if errors.is_empty() {
            "no physical Apple devices were detected".to_owned()
        } else {
            errors.join("; ")
        };
        bail!("failed to discover local devices ({details})");
    }

    Ok(devices.into_values().collect())
}

fn merge_devices(target: &mut BTreeMap<String, DetectedDevice>, discovered: Vec<DetectedDevice>) {
    for device in discovered {
        target.entry(device.udid.clone()).or_insert(device);
    }
}

fn discover_with_devicectl() -> Result<Vec<DetectedDevice>> {
    #[derive(Debug, Deserialize)]
    struct Root {
        result: DeviceList,
    }

    #[derive(Debug, Deserialize)]
    struct DeviceList {
        devices: Vec<Device>,
    }

    #[derive(Debug, Deserialize)]
    struct Device {
        #[serde(rename = "deviceProperties")]
        device_properties: DeviceProperties,
        #[serde(rename = "hardwareProperties")]
        hardware_properties: HardwareProperties,
    }

    #[derive(Debug, Deserialize)]
    struct DeviceProperties {
        name: String,
    }

    #[derive(Debug, Deserialize)]
    struct HardwareProperties {
        platform: String,
        udid: Option<String>,
        reality: Option<String>,
        #[serde(rename = "deviceType")]
        device_type: Option<String>,
        #[serde(rename = "productType")]
        product_type: Option<String>,
    }

    let output = Command::new("xcrun")
        .args(["devicectl", "list", "devices", "--json-output", "-"])
        .output()
        .context("failed to execute xcrun devicectl")?;
    ensure!(
        output.status.success(),
        "xcrun devicectl failed with status {}",
        output.status
    );

    let root: Root =
        serde_json::from_slice(&output.stdout).context("failed to parse devicectl JSON output")?;

    let mut devices = Vec::new();
    for device in root.result.devices {
        if device.hardware_properties.reality.as_deref() != Some("physical") {
            continue;
        }
        let Some(udid) = device.hardware_properties.udid else {
            continue;
        };
        let Some(family) = infer_family_from_devicectl(
            &device.hardware_properties.platform,
            device.hardware_properties.device_type.as_deref(),
            device.hardware_properties.product_type.as_deref(),
        ) else {
            continue;
        };
        devices.push(DetectedDevice {
            name: device.device_properties.name,
            udid,
            family,
        });
    }

    Ok(devices)
}

fn discover_with_xcdevice() -> Result<Vec<DetectedDevice>> {
    #[derive(Debug, Deserialize)]
    struct Device {
        available: bool,
        simulator: bool,
        name: String,
        identifier: String,
        platform: String,
        #[serde(rename = "modelCode")]
        model_code: Option<String>,
    }

    let output = Command::new("xcrun")
        .args(["xcdevice", "list"])
        .output()
        .context("failed to execute xcrun xcdevice")?;
    ensure!(
        output.status.success(),
        "xcrun xcdevice failed with status {}",
        output.status
    );

    let parsed: Vec<Device> =
        serde_json::from_slice(&output.stdout).context("failed to parse xcdevice JSON output")?;

    let mut devices = Vec::new();
    for device in parsed {
        if !device.available || device.simulator {
            continue;
        }
        let Some(family) =
            infer_family_from_xcdevice(&device.platform, device.model_code.as_deref())
        else {
            continue;
        };
        devices.push(DetectedDevice {
            name: device.name,
            udid: device.identifier,
            family,
        });
    }

    Ok(devices)
}

fn infer_family_from_devicectl(
    platform: &str,
    device_type: Option<&str>,
    product_type: Option<&str>,
) -> Option<DeviceFamily> {
    match platform {
        "iOS" => match device_type
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "ipad" => Some(DeviceFamily::Ipados),
            "iphone" | "ipod" => Some(DeviceFamily::Ios),
            _ => product_type.and_then(DeviceFamily::infer_from_product),
        },
        "watchOS" => Some(DeviceFamily::Watchos),
        "tvOS" => Some(DeviceFamily::Tvos),
        "visionOS" | "xrOS" => Some(DeviceFamily::Visionos),
        "macOS" => Some(DeviceFamily::Macos),
        _ => product_type.and_then(DeviceFamily::infer_from_product),
    }
}

fn infer_family_from_xcdevice(platform: &str, model_code: Option<&str>) -> Option<DeviceFamily> {
    if platform.contains("watchos") {
        return Some(DeviceFamily::Watchos);
    }
    if platform.contains("appletvos") || platform.contains("tvos") {
        return Some(DeviceFamily::Tvos);
    }
    if platform.contains("visionos") || platform.contains("xros") {
        return Some(DeviceFamily::Visionos);
    }
    if platform.contains("macosx") || platform.contains("macos") {
        return Some(DeviceFamily::Macos);
    }
    if platform.contains("iphoneos") {
        if let Some(model_code) = model_code {
            return DeviceFamily::infer_from_product(model_code);
        }
        return Some(DeviceFamily::Ios);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{infer_family_from_devicectl, infer_family_from_xcdevice};
    use crate::config::DeviceFamily;

    #[test]
    fn infers_ios_and_ipados_from_devicectl() {
        assert_eq!(
            infer_family_from_devicectl("iOS", Some("iPhone"), Some("iPhone16,2")),
            Some(DeviceFamily::Ios)
        );
        assert_eq!(
            infer_family_from_devicectl("iOS", Some("iPad"), Some("iPad16,3")),
            Some(DeviceFamily::Ipados)
        );
    }

    #[test]
    fn infers_watch_and_vision_from_xcdevice() {
        assert_eq!(
            infer_family_from_xcdevice("com.apple.platform.watchos", Some("Watch7,5")),
            Some(DeviceFamily::Watchos)
        );
        assert_eq!(
            infer_family_from_xcdevice("com.apple.platform.xros", Some("RealityDevice14,1")),
            Some(DeviceFamily::Visionos)
        );
    }
}
