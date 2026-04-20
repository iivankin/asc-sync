use serde_json::{Value, json};

pub const PUBLISHED_SCHEMA_URL: &str = concat!(
    "https://orbitstorage.dev/schemas/asc-sync.schema-",
    env!("CARGO_PKG_VERSION"),
    ".json"
);

pub const INIT_DESCRIPTION: &str =
    "This file is documented by its `$schema`. Start with `ascs --help` for the common workflow.";

pub fn init_config_template(team_id: &str) -> Value {
    json!({
        "$schema": PUBLISHED_SCHEMA_URL,
        "_description": INIT_DESCRIPTION,
        "team_id": team_id,
        "bundle_ids": {},
        "devices": {},
        "certs": {},
        "profiles": {},
        "apps": {},
    })
}

#[cfg(test)]
mod tests {
    use super::{INIT_DESCRIPTION, PUBLISHED_SCHEMA_URL, init_config_template};

    #[test]
    fn init_template_includes_description_and_schema() {
        let template = init_config_template("TEAM123");

        assert_eq!(
            template.get("$schema").and_then(|value| value.as_str()),
            Some(PUBLISHED_SCHEMA_URL)
        );
        assert_eq!(
            template
                .get("_description")
                .and_then(|value| value.as_str()),
            Some(INIT_DESCRIPTION)
        );
        assert_eq!(
            template.get("team_id").and_then(|value| value.as_str()),
            Some("TEAM123")
        );
    }
}
