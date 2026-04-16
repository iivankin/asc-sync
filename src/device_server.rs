use std::{
    collections::BTreeMap,
    fs,
    io::Cursor,
    process::Command,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use plist::Value as PlistValue;
use rand::{RngExt, distr::Alphanumeric};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

const PUBLIC_URL_ENV: &str = "ASC_DEVICE_SERVER_PUBLIC_URL";
const LISTEN_ENV: &str = "ASC_DEVICE_SERVER_LISTEN";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRegistrationRequest {
    pub logical_id: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateRegistrationResponse {
    pub token: String,
    pub registration_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrationStatusResponse {
    pub token: String,
    pub status: RegistrationStatus,
    pub logical_id: Option<String>,
    pub display_name: Option<String>,
    pub result: Option<CompletedRegistration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedRegistration {
    pub udid: String,
    pub product: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationStatus {
    Pending,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistrationRecord {
    token: String,
    challenge: String,
    created_at: u64,
    logical_id: Option<String>,
    display_name: Option<String>,
    status: RegistrationStatus,
    result: Option<CompletedRegistration>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistrationStore {
    registrations: BTreeMap<String, RegistrationRecord>,
}

#[derive(Debug, Clone)]
struct DeviceServerSettings {
    listen: String,
    public_url: String,
}

#[derive(Clone)]
struct AppState {
    settings: DeviceServerSettings,
    store: Arc<Mutex<RegistrationStore>>,
}

pub fn run_from_env() -> Result<()> {
    let settings = DeviceServerSettings::from_env()?;
    let app_state = AppState {
        settings: settings.clone(),
        store: Arc::new(Mutex::new(RegistrationStore::default())),
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime for device server")?;

    runtime.block_on(async move {
        let app = Router::new()
            .route("/healthz", get(healthz))
            .route("/api/registrations", post(create_registration))
            .route("/api/registrations/{token}", get(get_registration))
            .route("/register/{token}", get(download_registration_profile))
            .route("/profile/{token}", post(complete_registration))
            .with_state(app_state);

        let listener = TcpListener::bind(&settings.listen)
            .await
            .with_context(|| format!("failed to bind device server on {}", settings.listen))?;
        println!("device server listening on {}", settings.listen);
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("device server exited with error")
    })
}

impl DeviceServerSettings {
    fn from_env() -> Result<Self> {
        let public_url = std::env::var(PUBLIC_URL_ENV)
            .with_context(|| format!("{PUBLIC_URL_ENV} is not set"))?;
        ensure!(
            !public_url.trim().is_empty(),
            "{PUBLIC_URL_ENV} cannot be empty"
        );
        let listen = std::env::var(LISTEN_ENV).unwrap_or_else(|_| "0.0.0.0:3000".to_owned());
        ensure!(!listen.trim().is_empty(), "{LISTEN_ENV} cannot be empty");

        Ok(Self {
            listen,
            public_url: public_url.trim_end_matches('/').to_owned(),
        })
    }
}

async fn healthz() -> &'static str {
    "ok"
}

async fn create_registration(
    State(state): State<AppState>,
    Json(request): Json<CreateRegistrationRequest>,
) -> Result<Json<CreateRegistrationResponse>, ApiError> {
    let mut store = state
        .store
        .lock()
        .map_err(|_| ApiError::internal("failed to lock registration store"))?;

    let token = generate_secret(32);
    let challenge = generate_secret(48);
    let record = RegistrationRecord {
        token: token.clone(),
        challenge,
        created_at: unix_timestamp()?,
        logical_id: request.logical_id,
        display_name: request.display_name,
        status: RegistrationStatus::Pending,
        result: None,
    };
    store.registrations.insert(token.clone(), record);

    Ok(Json(CreateRegistrationResponse {
        registration_url: format!("{}/register/{}", state.settings.public_url, token),
        token,
    }))
}

async fn get_registration(
    State(state): State<AppState>,
    AxumPath(token): AxumPath<String>,
) -> Result<Json<RegistrationStatusResponse>, ApiError> {
    let store = state
        .store
        .lock()
        .map_err(|_| ApiError::internal("failed to lock registration store"))?;
    let record = store
        .registrations
        .get(&token)
        .ok_or_else(|| ApiError::not_found("registration token was not found"))?;

    Ok(Json(RegistrationStatusResponse {
        token: record.token.clone(),
        status: record.status,
        logical_id: record.logical_id.clone(),
        display_name: record.display_name.clone(),
        result: record.result.clone(),
    }))
}

async fn download_registration_profile(
    State(state): State<AppState>,
    AxumPath(token): AxumPath<String>,
) -> Result<Response, ApiError> {
    let record = {
        let store = state
            .store
            .lock()
            .map_err(|_| ApiError::internal("failed to lock registration store"))?;
        store
            .registrations
            .get(&token)
            .cloned()
            .ok_or_else(|| ApiError::not_found("registration token was not found"))?
    };

    let payload = build_profile_service_payload(&state.settings.public_url, &record);
    let mut response = Response::new(payload.into_bytes().into());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-apple-aspen-config"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str("attachment; filename=\"register-device.mobileconfig\"")
            .map_err(|error| ApiError::internal(error.to_string()))?,
    );
    Ok(response)
}

async fn complete_registration(
    State(state): State<AppState>,
    AxumPath(token): AxumPath<String>,
    body: Bytes,
) -> Result<Html<String>, ApiError> {
    let plist_bytes = verify_signed_profile_response(&body).map_err(ApiError::from)?;
    let completed = parse_registration_response(&plist_bytes).map_err(ApiError::from)?;

    let mut store = state
        .store
        .lock()
        .map_err(|_| ApiError::internal("failed to lock registration store"))?;
    let record = store
        .registrations
        .get_mut(&token)
        .ok_or_else(|| ApiError::not_found("registration token was not found"))?;

    if record.challenge != completed.challenge {
        return Err(ApiError::from(anyhow::anyhow!(
            "device response challenge did not match registration token"
        )));
    }

    record.status = RegistrationStatus::Completed;
    record.result = Some(CompletedRegistration {
        udid: completed.udid,
        product: completed.product,
        version: completed.version,
    });

    Ok(Html(
        "<html><body><h1>Device registered</h1><p>You can return to the desktop CLI.</p></body></html>"
            .to_owned(),
    ))
}

#[derive(Debug)]
struct ParsedRegistrationResponse {
    challenge: String,
    udid: String,
    product: Option<String>,
    version: Option<String>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

fn build_profile_service_payload(public_url: &str, record: &RegistrationRecord) -> String {
    let callback_url = format!("{public_url}/profile/{}", record.token);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <dict>
    <key>Challenge</key>
    <string>{challenge}</string>
    <key>DeviceAttributes</key>
    <array>
      <string>UDID</string>
      <string>VERSION</string>
      <string>PRODUCT</string>
    </array>
    <key>URL</key>
    <string>{callback_url}</string>
  </dict>
  <key>PayloadDisplayName</key>
  <string>Register Device</string>
  <key>PayloadIdentifier</key>
  <string>com.asc-sync.device-registration.{identifier}</string>
  <key>PayloadOrganization</key>
  <string>asc-sync</string>
  <key>PayloadType</key>
  <string>Profile Service</string>
  <key>PayloadUUID</key>
  <string>{uuid}</string>
  <key>PayloadVersion</key>
  <integer>1</integer>
</dict>
</plist>
"#,
        challenge = xml_escape(&record.challenge),
        callback_url = xml_escape(&callback_url),
        identifier = xml_escape(&record.token),
        uuid = pseudo_uuid(),
    )
}

fn verify_signed_profile_response(body: &[u8]) -> Result<Vec<u8>> {
    let tempdir =
        tempfile::tempdir().context("failed to create temporary verification directory")?;
    let input_path = tempdir.path().join("device-response.der");
    let output_path = tempdir.path().join("device-response.plist");
    fs::write(&input_path, body)
        .with_context(|| format!("failed to write {}", input_path.display()))?;

    let status = Command::new("openssl")
        .arg("smime")
        .arg("-verify")
        .arg("-inform")
        .arg("der")
        .arg("-in")
        .arg(&input_path)
        .arg("-out")
        .arg(&output_path)
        .arg("-noverify")
        .status()
        .context("failed to execute openssl smime -verify")?;

    ensure!(
        status.success(),
        "openssl smime -verify failed with status {status}"
    );
    fs::read(&output_path).with_context(|| format!("failed to read {}", output_path.display()))
}

fn parse_registration_response(payload: &[u8]) -> Result<ParsedRegistrationResponse> {
    let plist = PlistValue::from_reader_xml(Cursor::new(payload))
        .context("failed to parse device response plist")?;
    let dictionary = plist
        .into_dictionary()
        .ok_or_else(|| anyhow::anyhow!("device response plist root is not a dictionary"))?;

    Ok(ParsedRegistrationResponse {
        challenge: required_plist_string(&dictionary, "CHALLENGE")?,
        udid: required_plist_string(&dictionary, "UDID")?,
        product: optional_plist_string(&dictionary, "PRODUCT"),
        version: optional_plist_string(&dictionary, "VERSION"),
    })
}

fn required_plist_string(dictionary: &plist::Dictionary, key: &str) -> Result<String> {
    optional_plist_string(dictionary, key)
        .ok_or_else(|| anyhow::anyhow!("device response did not include {key}"))
}

fn optional_plist_string(dictionary: &plist::Dictionary, key: &str) -> Option<String> {
    dictionary
        .get(key)
        .and_then(PlistValue::as_string)
        .map(ToOwned::to_owned)
}

fn generate_secret(length: usize) -> String {
    let rng = rand::rng();
    rng.sample_iter(Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

fn pseudo_uuid() -> String {
    let hex = generate_secret(32).to_lowercase();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn unix_timestamp() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_secs())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
