use serde_json::{Value, json};

pub const PUBLISHED_SCHEMA_URL: &str = concat!(
    "https://orbitstorage.dev/schemas/asc-sync.schema-",
    env!("CARGO_PKG_VERSION"),
    ".json"
);

pub fn init_config_template(team_id: &str) -> Value {
    json!({
        "$schema": PUBLISHED_SCHEMA_URL,
        "team_id": team_id,
        "bundle_ids": {},
        "devices": {},
        "certs": {},
        "profiles": {},
    })
}
