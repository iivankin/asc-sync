use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{Cursor, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    cli::{MediaPreviewArgs, MediaRenderArgs},
    config::{
        AppVersionLocalizationSource, AppVersionLocalizationSpec,
        CustomProductPageLocalizationSource, CustomProductPageLocalizationSpec, KeywordsSpec,
        MediaPathList, MediaScreenshotRenderSpec, MediaScreenshotSet, StringSource,
    },
    system,
};
use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use glob::glob;
use md5::{Digest, Md5};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tungstenite::{Message, WebSocket, connect, stream::MaybeTlsStream};

#[derive(Debug, Clone)]
struct HtmlTemplate {
    path: PathBuf,
    id: String,
}

#[derive(Debug, Clone)]
struct TemplateInstance {
    template: HtmlTemplate,
    screen: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct TemplateData {
    locale: Option<String>,
    strings: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Viewport {
    width: u32,
    height: u32,
}

#[derive(Debug, Clone)]
struct RenderContext {
    viewport: Viewport,
    frame: Option<FrameSpec>,
}

#[derive(Debug, Clone)]
struct FrameSpec {
    name: String,
    image_path: PathBuf,
    image_size: Viewport,
    screen: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

const CHROME_RENDER_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_DEVICE_FRAMES_URL: &str = "https://orbitstorage.dev/assets/device-frames";
const DEVICE_FRAMES_URL_ENV: &str = "ASC_SYNC_DEVICE_FRAMES_URL";

pub struct GeneratedScreenshots {
    paths: Vec<PathBuf>,
    _tempdir: Option<tempfile::TempDir>,
}

impl GeneratedScreenshots {
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

pub fn render_config_screenshots_to_temp(
    config_dir: &Path,
    locale: &str,
    localization: Option<&AppVersionLocalizationSource>,
    set: MediaScreenshotSet,
    render: &MediaScreenshotRenderSpec,
) -> Result<GeneratedScreenshots> {
    let localization = localization.ok_or_else(|| {
        anyhow::anyhow!(
            "rendered screenshots for locale {locale} require version.localizations.{locale}"
        )
    })?;
    let template_data = load_version_config_template_data(config_dir, locale, localization)?;
    render_config_screenshots_to_temp_with_data(config_dir, set, render, &template_data)
}

pub fn render_custom_product_page_config_screenshots_to_temp(
    config_dir: &Path,
    locale: &str,
    localization: &CustomProductPageLocalizationSource,
    set: MediaScreenshotSet,
    render: &MediaScreenshotRenderSpec,
) -> Result<GeneratedScreenshots> {
    let template_data =
        load_custom_product_page_config_template_data(config_dir, locale, localization)?;
    render_config_screenshots_to_temp_with_data(config_dir, set, render, &template_data)
}

fn render_config_screenshots_to_temp_with_data(
    config_dir: &Path,
    set: MediaScreenshotSet,
    render: &MediaScreenshotRenderSpec,
    template_data: &TemplateData,
) -> Result<GeneratedScreenshots> {
    let tempdir = if render.output_dir.is_some() {
        None
    } else {
        Some(
            tempfile::Builder::new()
                .prefix("asc-sync-media-")
                .tempdir()
                .context("failed to create temporary media render directory")?,
        )
    };
    let fallback_output_dir = tempdir
        .as_ref()
        .map(|tempdir| tempdir.path().to_path_buf())
        .unwrap_or_else(|| resolve_config_render_output_dir(config_dir, render, Path::new(".")));
    let paths = render_config_screenshots_to_dir_with_data(
        config_dir,
        &fallback_output_dir,
        set,
        render,
        template_data,
    )?;
    Ok(GeneratedScreenshots {
        paths,
        _tempdir: tempdir,
    })
}

pub fn render_config_screenshots_to_dir(
    config_dir: &Path,
    output_dir: &Path,
    locale: &str,
    localization: Option<&AppVersionLocalizationSource>,
    set: MediaScreenshotSet,
    render: &MediaScreenshotRenderSpec,
) -> Result<Vec<PathBuf>> {
    let localization = localization.ok_or_else(|| {
        anyhow::anyhow!(
            "rendered screenshots for locale {locale} require version.localizations.{locale}"
        )
    })?;
    let template_data = load_version_config_template_data(config_dir, locale, localization)?;
    render_config_screenshots_to_dir_with_data(config_dir, output_dir, set, render, &template_data)
}

pub fn render_custom_product_page_config_screenshots_to_dir(
    config_dir: &Path,
    output_dir: &Path,
    locale: &str,
    localization: &CustomProductPageLocalizationSource,
    set: MediaScreenshotSet,
    render: &MediaScreenshotRenderSpec,
) -> Result<Vec<PathBuf>> {
    let template_data =
        load_custom_product_page_config_template_data(config_dir, locale, localization)?;
    render_config_screenshots_to_dir_with_data(config_dir, output_dir, set, render, &template_data)
}

fn render_config_screenshots_to_dir_with_data(
    config_dir: &Path,
    output_dir: &Path,
    set: MediaScreenshotSet,
    render: &MediaScreenshotRenderSpec,
    template_data: &TemplateData,
) -> Result<Vec<PathBuf>> {
    let templates = resolve_templates(&resolve_config_path_list(config_dir, &render.template))?;
    let screens = resolve_config_path_list(config_dir, &render.screens);
    let frame_dir = render
        .frame_dir
        .as_ref()
        .map(|path| resolve_config_path(config_dir, path));
    let output_dir = resolve_config_render_output_dir(config_dir, render, output_dir);
    let context = RenderContext {
        viewport: named_viewport(set.config_key())?,
        frame: Some(resolve_frame(&render.frame, frame_dir.as_ref())?),
    };
    let instances = resolve_template_instances(templates, &screens, true)?;
    let chrome = find_chrome(None)?;
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let user_data_dir = tempfile::Builder::new()
        .prefix("asc-sync-chrome-")
        .tempdir()
        .context("failed to create temporary Chrome profile")?;
    let wrapper_dir = tempfile::Builder::new()
        .prefix("asc-sync-html-")
        .tempdir()
        .context("failed to create temporary HTML wrapper directory")?;

    let mut outputs = Vec::new();
    for instance in &instances {
        let output = output_dir.join(format!("{}.png", instance.template.id));
        let url = render_url(instance, &context, template_data, wrapper_dir.path())?;
        render_template(
            &chrome,
            user_data_dir.path(),
            &instance.template,
            context.viewport,
            &url,
            &output,
        )?;
        outputs.push(output);
    }

    Ok(outputs)
}

pub fn render(args: &MediaRenderArgs) -> Result<()> {
    let templates = resolve_templates(&args.input)?;
    let context = resolve_render_context(
        args.size.as_deref(),
        args.viewport.as_deref(),
        args.frame.as_deref(),
        args.frame_dir.as_ref(),
    )?;
    let instances = resolve_template_instances(templates, &args.screen, context.frame.is_some())?;
    let template_data = load_template_data(args.locale.as_deref(), args.strings.as_ref())?;
    let chrome = find_chrome(args.chrome.as_ref())?;
    fs::create_dir_all(&args.output_dir)
        .with_context(|| format!("failed to create {}", args.output_dir.display()))?;

    let user_data_dir = tempfile::Builder::new()
        .prefix("asc-sync-chrome-")
        .tempdir()
        .context("failed to create temporary Chrome profile")?;
    let wrapper_dir = tempfile::Builder::new()
        .prefix("asc-sync-html-")
        .tempdir()
        .context("failed to create temporary HTML wrapper directory")?;

    for instance in &instances {
        let output = args
            .output_dir
            .join(format!("{}.png", instance.template.id));
        let url = render_url(instance, &context, &template_data, wrapper_dir.path())?;
        render_template(
            &chrome,
            user_data_dir.path(),
            &instance.template,
            context.viewport,
            &url,
            &output,
        )?;
        println!("rendered {}", output.display());
    }

    Ok(())
}

pub fn preview(args: &MediaPreviewArgs) -> Result<()> {
    let templates = resolve_templates(&args.input)?;
    let context = resolve_render_context(
        args.size.as_deref(),
        args.viewport.as_deref(),
        args.frame.as_deref(),
        args.frame_dir.as_ref(),
    )?;
    let instances = resolve_template_instances(templates, &args.screen, context.frame.is_some())?;
    let template_data = load_template_data(args.locale.as_deref(), args.strings.as_ref())?;
    let listener = TcpListener::bind(("127.0.0.1", args.port))
        .with_context(|| format!("failed to bind preview server on 127.0.0.1:{}", args.port))?;
    let address = listener
        .local_addr()
        .context("failed to read preview server address")?;
    let url = format!("http://{address}/");
    println!("Preview: {url}");
    println!("Press Ctrl-C to stop.");
    if args.open {
        let _ = Command::new("open").arg(&url).status();
    }

    let server = PreviewServer {
        instances,
        context,
        template_data,
    };
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                if let Err(error) = server.handle(&mut stream) {
                    let _ = write_response(
                        &mut stream,
                        500,
                        "text/plain; charset=utf-8",
                        error.to_string().as_bytes(),
                    );
                }
            }
            Err(error) => eprintln!("preview connection failed: {error}"),
        }
    }
    Ok(())
}

fn render_template(
    chrome: &OsString,
    user_data_dir: &Path,
    template: &HtmlTemplate,
    viewport: Viewport,
    url: &str,
    output: &Path,
) -> Result<()> {
    let port = available_local_port().context("failed to reserve Chrome debugging port")?;
    let mut child = Command::new(chrome)
        .arg("--headless=new")
        .arg("--disable-background-networking")
        .arg("--disable-component-update")
        .arg("--disable-domain-reliability")
        .arg("--disable-extensions")
        .arg("--disable-gpu")
        .arg("--disable-sync")
        .arg("--disable-features=MediaRouter,OptimizationHints,Translate")
        .arg("--hide-scrollbars")
        .arg("--metrics-recording-only")
        .arg("--no-default-browser-check")
        .arg("--no-first-run")
        .arg("--no-service-autorun")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg(format!("--remote-debugging-port={port}"))
        .arg("--force-device-scale-factor=1")
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .arg(format!(
            "--window-size={},{}",
            viewport.width, viewport.height
        ))
        .arg("about:blank")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to run Chrome for {}", template.path.display()))?;

    let result = capture_template_screenshot(port, viewport, url, output)
        .with_context(|| format!("failed to render {}", template.path.display()));
    let _ = child.kill();
    let _ = child.wait();
    result?;
    flatten_png_alpha_to_white(output)?;
    Ok(())
}

fn capture_template_screenshot(
    port: u16,
    viewport: Viewport,
    url: &str,
    output: &Path,
) -> Result<()> {
    let ws_url = wait_for_chrome_target(port)?;
    let mut client = CdpClient::connect(&ws_url)?;
    client.call("Page.enable", json!({}))?;
    client.call("Runtime.enable", json!({}))?;
    client.call(
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": viewport.width,
            "height": viewport.height,
            "deviceScaleFactor": 1,
            "mobile": false,
        }),
    )?;
    client.call("Page.navigate", json!({ "url": url }))?;
    client.wait_event("Page.loadEventFired", CHROME_RENDER_TIMEOUT)?;
    client.call(
        "Runtime.evaluate",
        json!({
            "expression": RENDER_READY_SCRIPT,
            "awaitPromise": true,
            "returnByValue": true,
            "timeout": CHROME_RENDER_TIMEOUT.as_millis(),
        }),
    )?;
    let result = client.call(
        "Page.captureScreenshot",
        json!({
            "format": "png",
            "fromSurface": true,
            "captureBeyondViewport": false,
        }),
    )?;
    let data = result
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Chrome did not return screenshot data"))?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .context("failed to decode Chrome screenshot")?;
    fs::write(output, bytes).with_context(|| format!("failed to write {}", output.display()))?;
    ensure!(
        screenshot_is_ready(output)?,
        "Chrome did not create {}",
        output.display()
    );
    Ok(())
}

const RENDER_READY_SCRIPT: &str = r#"(async () => {
  if (document.readyState !== "complete") {
    await new Promise((resolve) => {
      window.addEventListener("load", resolve, { once: true });
    });
  }

  const stylesheetLinks = Array.from(document.querySelectorAll('link[rel="stylesheet"]'));
  await Promise.all(stylesheetLinks.map(async (link) => {
    if (link.sheet) {
      return;
    }
    await new Promise((resolve) => {
      link.addEventListener("load", resolve, { once: true });
      link.addEventListener("error", resolve, { once: true });
    });
  }));

  if (document.fonts && document.fonts.ready) {
    try {
      await document.fonts.ready;
    } catch (error) {
      // Match Koubou: font readiness failures should not block rendering forever.
    }
  }

  const images = Array.from(document.images);
  await Promise.all(images.map(async (image) => {
    if (!image.complete) {
      await new Promise((resolve, reject) => {
        image.addEventListener("load", resolve, { once: true });
        image.addEventListener("error", reject, { once: true });
      }).catch(() => undefined);
    }

    if (typeof image.decode === "function") {
      try {
        await image.decode();
      } catch (error) {
        // Use the current layout if decode fails, same as Koubou.
      }
    }
  }));

  await new Promise((resolve) => requestAnimationFrame(() => resolve()));
  return true;
})()"#;

struct CdpClient {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl CdpClient {
    fn connect(ws_url: &str) -> Result<Self> {
        let (mut socket, _) =
            connect(ws_url).with_context(|| format!("failed to connect to {ws_url}"))?;
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(CHROME_RENDER_TIMEOUT))
                .context("failed to set Chrome websocket read timeout")?;
            stream
                .set_write_timeout(Some(CHROME_RENDER_TIMEOUT))
                .context("failed to set Chrome websocket write timeout")?;
        }
        Ok(Self { socket, next_id: 0 })
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.socket
            .send(Message::Text(
                json!({
                    "id": id,
                    "method": method,
                    "params": params,
                })
                .to_string()
                .into(),
            ))
            .with_context(|| format!("failed to send Chrome command {method}"))?;
        loop {
            let value = self.read_json_message()?;
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                bail!("Chrome command {method} failed: {error}");
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn wait_event(&mut self, method: &str, timeout: Duration) -> Result<Value> {
        let started_at = Instant::now();
        while started_at.elapsed() < timeout {
            let value = self.read_json_message()?;
            if value.get("method").and_then(Value::as_str) == Some(method) {
                return Ok(value.get("params").cloned().unwrap_or(Value::Null));
            }
        }
        bail!(
            "timed out waiting for Chrome event {method} after {} seconds",
            timeout.as_secs()
        )
    }

    fn read_json_message(&mut self) -> Result<Value> {
        loop {
            match self
                .socket
                .read()
                .context("failed to read Chrome websocket message")?
            {
                Message::Text(text) => {
                    return serde_json::from_str(&text)
                        .context("failed to parse Chrome websocket JSON");
                }
                Message::Binary(bytes) => {
                    return serde_json::from_slice(&bytes)
                        .context("failed to parse Chrome websocket JSON");
                }
                Message::Ping(bytes) => self
                    .socket
                    .send(Message::Pong(bytes))
                    .context("failed to respond to Chrome websocket ping")?,
                Message::Close(_) => bail!("Chrome websocket closed"),
                Message::Pong(_) | Message::Frame(_) => {}
            }
        }
    }
}

fn wait_for_chrome_target(port: u16) -> Result<String> {
    let started_at = Instant::now();
    let endpoint = format!("http://127.0.0.1:{port}/json/list");
    while started_at.elapsed() < CHROME_RENDER_TIMEOUT {
        if let Ok(response) = reqwest::blocking::get(&endpoint)
            && response.status().is_success()
            && let Ok(value) = response.json::<Value>()
            && let Some(targets) = value.as_array()
        {
            for target in targets {
                if target.get("type").and_then(Value::as_str) == Some("page")
                    && let Some(ws_url) = target.get("webSocketDebuggerUrl").and_then(Value::as_str)
                {
                    return Ok(ws_url.to_owned());
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    bail!(
        "Chrome debugging endpoint did not become ready after {} seconds",
        CHROME_RENDER_TIMEOUT.as_secs()
    )
}

fn available_local_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn screenshot_is_ready(output: &Path) -> Result<bool> {
    match output.metadata() {
        Ok(metadata) => Ok(metadata.is_file() && metadata.len() > 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", output.display())),
    }
}

fn render_url(
    instance: &TemplateInstance,
    context: &RenderContext,
    template_data: &TemplateData,
    wrapper_dir: &Path,
) -> Result<String> {
    let base_href = directory_file_url(
        instance
            .template
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("template has no parent directory"))?,
    )?;
    let frame_asset_dir =
        wrapper_dir.join(format!("{}-assets", safe_file_stem(&instance.template.id)));
    let html = prepare_template_html(
        instance,
        context,
        template_data,
        &base_href,
        |placeholder_index, fit| {
            let frame = context
                .frame
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("frame is not configured"))?;
            let screen = instance
                .screen
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("screen image is not configured"))?;
            fs::create_dir_all(&frame_asset_dir)
                .with_context(|| format!("failed to create {}", frame_asset_dir.display()))?;
            let path = frame_asset_dir.join(format!("frame-{placeholder_index}.png"));
            write_framed_screen_asset(frame, screen, fit, &path)?;
            file_url(&path)
        },
    )?;
    let wrapper_path = wrapper_dir.join(format!("{}.html", safe_file_stem(&instance.template.id)));
    fs::write(&wrapper_path, html)
        .with_context(|| format!("failed to write {}", wrapper_path.display()))?;
    file_url(&wrapper_path)
}

fn template_url(base_url: &str, template_id: &str, width: u32, height: u32) -> String {
    format!(
        "{base_url}?asc_width={width}&asc_height={height}&asc_id={}",
        percent_encode(template_id)
    )
}

fn prepare_template_html(
    instance: &TemplateInstance,
    context: &RenderContext,
    template_data: &TemplateData,
    base_href: &str,
    mut framed_asset_src: impl FnMut(usize, FrameFit) -> Result<String>,
) -> Result<String> {
    let html = fs::read_to_string(&instance.template.path)
        .with_context(|| format!("failed to read {}", instance.template.path.display()))?;
    let html = apply_template_variables(&html, template_data, &instance.template.id)?;
    let html = inject_base(&html, base_href);
    let html = inject_template_runtime(&html, context, template_data, &instance.template.id)?;

    let Some(frame) = &context.frame else {
        return Ok(html);
    };
    ensure!(
        instance.screen.is_some(),
        "template {} uses --frame but no --screen image was resolved",
        instance.template.path.display()
    );
    let html = inject_frame_styles(&html);
    replace_frame_placeholders(&html, frame, &instance.template.path, |index, fit| {
        framed_asset_src(index, fit)
    })
}

fn inject_template_runtime(
    html: &str,
    context: &RenderContext,
    template_data: &TemplateData,
    template_id: &str,
) -> Result<String> {
    let payload = serde_json::json!({
        "id": template_id,
        "locale": template_data.locale,
        "viewport": {
            "width": context.viewport.width,
            "height": context.viewport.height,
        },
        "frame": context.frame.as_ref().map(|frame| serde_json::json!({
            "name": frame.name,
            "image": {
                "width": frame.image_size.width,
                "height": frame.image_size.height,
            },
            "screen": {
                "x": frame.screen.x,
                "y": frame.screen.y,
                "width": frame.screen.width,
                "height": frame.screen.height,
            },
        })),
        "strings": template_data.strings,
    });
    let payload = serde_json::to_string(&payload)
        .context("failed to serialize template runtime payload")?
        .replace("</script", "<\\/script");
    let script = format!(
        r#"<script>
window.ASC_SYNC = {payload};
if (window.ASC_SYNC.locale && !document.documentElement.lang) {{
  document.documentElement.lang = window.ASC_SYNC.locale;
}}
</script>"#
    );
    Ok(inject_head_content(html, &script))
}

fn inject_frame_styles(html: &str) -> String {
    inject_head_content(
        html,
        r#"<style data-asc-sync-frame>
asc-device-frame[data-asc-rendered] {
  display: block;
}
asc-device-frame[data-asc-rendered] > .asc-device-frame__image {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: contain;
  pointer-events: none;
  user-select: none;
}
</style>"#,
    )
}

fn replace_frame_placeholders(
    html: &str,
    frame: &FrameSpec,
    template_path: &Path,
    mut framed_asset_src: impl FnMut(usize, FrameFit) -> Result<String>,
) -> Result<String> {
    let mut output = String::new();
    let mut rest = html;
    let mut replaced = 0;

    while let Some(start) = find_ascii_case_insensitive(rest, "<asc-device-frame") {
        output.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let opening_end = after_start
            .find('>')
            .ok_or_else(|| anyhow::anyhow!("unclosed <asc-device-frame> tag"))?;
        let opening = &after_start[..=opening_end];
        let self_closing = opening.trim_end().ends_with("/>");
        let attrs = frame_placeholder_attrs(opening);
        let fit = frame_screen_fit(opening)?;
        let framed_src = framed_asset_src(replaced, fit)?;

        if self_closing {
            output.push_str(&frame_placeholder_markup(attrs, frame, &framed_src));
            rest = &after_start[opening_end + 1..];
        } else {
            let after_opening = &after_start[opening_end + 1..];
            let closing = find_ascii_case_insensitive(after_opening, "</asc-device-frame>")
                .ok_or_else(|| anyhow::anyhow!("missing </asc-device-frame> closing tag"))?;
            output.push_str(&frame_placeholder_markup(attrs, frame, &framed_src));
            rest = &after_opening[closing + "</asc-device-frame>".len()..];
        }

        replaced += 1;
    }

    output.push_str(rest);
    ensure!(
        replaced > 0,
        "template {} uses --frame but does not contain <asc-device-frame></asc-device-frame>",
        template_path.display()
    );
    Ok(output)
}

fn frame_placeholder_attrs(opening: &str) -> &str {
    let mut attrs = opening
        .trim_start()
        .trim_start_matches('<')
        .trim_start_matches("asc-device-frame")
        .trim_end()
        .trim_end_matches('>')
        .trim_end()
        .trim_end_matches('/')
        .trim_end();
    if !attrs.is_empty() && !attrs.starts_with(char::is_whitespace) {
        attrs = "";
    }
    attrs
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameFit {
    Cover,
    Contain,
    Fill,
}

impl FrameFit {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "cover" => Ok(Self::Cover),
            "contain" => Ok(Self::Contain),
            "fill" => Ok(Self::Fill),
            other => {
                bail!(
                    "unsupported asc-device-frame fit value {other:?}; use cover, contain, or fill"
                )
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Cover => "cover",
            Self::Contain => "contain",
            Self::Fill => "fill",
        }
    }
}

fn frame_screen_fit(opening: &str) -> Result<FrameFit> {
    let value = extract_attr(opening, "fit")
        .or_else(|| extract_attr(opening, "data-screen-fit"))
        .unwrap_or_else(|| "cover".to_owned());
    FrameFit::parse(&value)
}

fn frame_placeholder_markup(attrs: &str, frame: &FrameSpec, framed_src: &str) -> String {
    format!(
        r#"<asc-device-frame{attrs} data-asc-rendered="true" data-asc-frame="{frame_name}">
  <img class="asc-device-frame__image" src="{framed_src}" alt="" style="aspect-ratio:{frame_width}/{frame_height};">
</asc-device-frame>"#,
        attrs = attrs,
        frame_name = html_escape(&frame.name),
        frame_width = frame.image_size.width,
        frame_height = frame.image_size.height,
        framed_src = html_escape(framed_src),
    )
}

fn write_framed_screen_asset(
    frame: &FrameSpec,
    screen_path: &Path,
    fit: FrameFit,
    output: &Path,
) -> Result<()> {
    let image = compose_framed_screen(frame, screen_path, fit)?;
    image
        .save(output)
        .with_context(|| format!("failed to write framed screen {}", output.display()))
}

fn framed_screen_png_bytes(
    frame: &FrameSpec,
    screen_path: &Path,
    fit: FrameFit,
) -> Result<Vec<u8>> {
    let image = compose_framed_screen(frame, screen_path, fit)?;
    let mut cursor = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .context("failed to encode framed screen PNG")?;
    Ok(cursor.into_inner())
}

fn compose_framed_screen(
    frame: &FrameSpec,
    screen_path: &Path,
    fit: FrameFit,
) -> Result<image::RgbaImage> {
    let frame_image = image::open(&frame.image_path)
        .with_context(|| format!("failed to open frame image {}", frame.image_path.display()))?
        .to_rgba8();
    let screen = image::open(screen_path)
        .with_context(|| format!("failed to open screen image {}", screen_path.display()))?
        .to_rgba8();
    ensure!(
        screen.width() > 0 && screen.height() > 0,
        "screen image {} is empty",
        screen_path.display()
    );

    let fitted = fit_screen_to_frame_rect(&screen, frame.screen, fit);
    let (offset_x, offset_y) = screen_offset(frame.screen, fitted.width(), fitted.height());
    let mut content = image::RgbaImage::from_pixel(
        frame_image.width(),
        frame_image.height(),
        image::Rgba([255, 255, 255, 0]),
    );
    image::imageops::overlay(&mut content, &fitted, offset_x.into(), offset_y.into());
    apply_frame_screen_mask(&mut content, &frame_image, frame.screen);
    image::imageops::overlay(&mut content, &frame_image, 0, 0);
    Ok(content)
}

fn fit_screen_to_frame_rect(
    screen: &image::RgbaImage,
    rect: Rect,
    fit: FrameFit,
) -> image::RgbaImage {
    match fit {
        FrameFit::Fill => image::imageops::resize(
            screen,
            rect.width,
            rect.height,
            image::imageops::FilterType::Lanczos3,
        ),
        FrameFit::Contain => {
            let scale = (rect.width as f64 / screen.width() as f64)
                .min(rect.height as f64 / screen.height() as f64);
            resize_by_scale(screen, scale)
        }
        FrameFit::Cover => {
            let scale = (rect.width as f64 / screen.width() as f64)
                .max(rect.height as f64 / screen.height() as f64);
            let resized = resize_by_scale(screen, scale);
            let crop_x = resized.width().saturating_sub(rect.width) / 2;
            let crop_y = resized.height().saturating_sub(rect.height) / 2;
            image::imageops::crop_imm(&resized, crop_x, crop_y, rect.width, rect.height).to_image()
        }
    }
}

fn resize_by_scale(screen: &image::RgbaImage, scale: f64) -> image::RgbaImage {
    let width = ((screen.width() as f64 * scale).round() as u32).max(1);
    let height = ((screen.height() as f64 * scale).round() as u32).max(1);
    image::imageops::resize(screen, width, height, image::imageops::FilterType::Lanczos3)
}

fn screen_offset(rect: Rect, width: u32, height: u32) -> (u32, u32) {
    (
        rect.x + rect.width.saturating_sub(width) / 2,
        rect.y + rect.height.saturating_sub(height) / 2,
    )
}

fn apply_frame_screen_mask(
    content: &mut image::RgbaImage,
    frame_image: &image::RgbaImage,
    fallback_rect: Rect,
) {
    let mask = frame_screen_mask(frame_image, fallback_rect);
    for (pixel, mask) in content.pixels_mut().zip(mask) {
        let alpha = (u16::from(pixel.0[3]) * u16::from(mask) / 255) as u8;
        pixel.0[3] = alpha;
    }
}

fn frame_screen_mask(frame_image: &image::RgbaImage, fallback_rect: Rect) -> Vec<u8> {
    let width = frame_image.width();
    let height = frame_image.height();
    let mut visited = vec![false; (width as usize) * (height as usize)];
    let mut queue = VecDeque::new();
    for x in 0..width {
        queue.push_back((x, 0));
        queue.push_back((x, height - 1));
    }
    for y in 0..height {
        queue.push_back((0, y));
        queue.push_back((width - 1, y));
    }

    while let Some((x, y)) = queue.pop_front() {
        if x >= width || y >= height {
            continue;
        }
        let index = pixel_index(width, x, y);
        if visited[index] || frame_image.get_pixel(x, y).0[3] > 50 {
            continue;
        }
        visited[index] = true;
        if x > 0 {
            queue.push_back((x - 1, y));
        }
        if x + 1 < width {
            queue.push_back((x + 1, y));
        }
        if y > 0 {
            queue.push_back((x, y - 1));
        }
        if y + 1 < height {
            queue.push_back((x, y + 1));
        }
    }

    let mut mask = vec![0_u8; (width as usize) * (height as usize)];
    let mut visible_pixels = 0_usize;
    for y in 0..height {
        for x in 0..width {
            let index = pixel_index(width, x, y);
            if visited[index] {
                continue;
            }
            let alpha = frame_image.get_pixel(x, y).0[3];
            if alpha <= 50 {
                mask[index] = 255_u8.saturating_sub(alpha);
                if mask[index] > 0 {
                    visible_pixels += 1;
                }
            }
        }
    }

    if visible_pixels == 0 {
        for y in fallback_rect.y
            ..fallback_rect
                .y
                .saturating_add(fallback_rect.height)
                .min(height)
        {
            for x in fallback_rect.x
                ..fallback_rect
                    .x
                    .saturating_add(fallback_rect.width)
                    .min(width)
            {
                mask[pixel_index(width, x, y)] = 255;
            }
        }
    }
    mask
}

fn flatten_png_alpha_to_white(path: &Path) -> Result<()> {
    let image = image::open(path)
        .with_context(|| format!("failed to open rendered image {}", path.display()))?
        .to_rgba8();
    let mut flattened = image::RgbImage::new(image.width(), image.height());
    for (x, y, pixel) in image.enumerate_pixels() {
        let alpha = u16::from(pixel.0[3]);
        let blend = |channel: u8| -> u8 {
            ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
        };
        flattened.put_pixel(
            x,
            y,
            image::Rgb([blend(pixel.0[0]), blend(pixel.0[1]), blend(pixel.0[2])]),
        );
    }
    flattened
        .save(path)
        .with_context(|| format!("failed to rewrite flattened PNG {}", path.display()))
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn extract_attr(opening: &str, name: &str) -> Option<String> {
    let pattern = format!("{name}=");
    let start = find_ascii_case_insensitive(opening, &pattern)? + pattern.len();
    let quote = opening[start..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_start = start + quote.len_utf8();
    let value_end = opening[value_start..].find(quote)? + value_start;
    Some(opening[value_start..value_end].to_owned())
}

fn safe_file_stem(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn resolve_templates(inputs: &[PathBuf]) -> Result<Vec<HtmlTemplate>> {
    let mut paths = Vec::new();
    for input in inputs {
        let value = input.to_string_lossy();
        if contains_glob_meta(&value) {
            let mut matched = glob(&value)
                .with_context(|| format!("invalid glob pattern {value}"))?
                .map(|entry| entry.with_context(|| format!("failed to read glob entry {value}")))
                .collect::<Result<Vec<_>>>()?;
            matched.sort();
            paths.extend(matched);
        } else if input.is_dir() {
            let mut entries = fs::read_dir(input)
                .with_context(|| format!("failed to read {}", input.display()))?
                .map(|entry| {
                    let entry = entry
                        .with_context(|| format!("failed to read entry in {}", input.display()))?;
                    Ok(entry.path())
                })
                .collect::<Result<Vec<_>>>()?;
            entries.retain(|path| lower_extension(path).as_deref() == Some("html"));
            entries.sort();
            paths.extend(entries);
        } else {
            paths.push(input.clone());
        }
    }

    ensure!(
        !paths.is_empty(),
        "media render input did not match any HTML files"
    );

    let mut seen = BTreeSet::new();
    let mut templates = Vec::new();
    for path in paths {
        ensure!(path.exists(), "template {} does not exist", path.display());
        ensure!(path.is_file(), "template {} is not a file", path.display());
        ensure!(
            lower_extension(&path).as_deref() == Some("html"),
            "template {} must be an .html file",
            path.display()
        );
        let path = path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()))?;
        ensure!(
            seen.insert(path.clone()),
            "template {} is listed more than once",
            path.display()
        );
        let id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("template {} has no UTF-8 file stem", path.display()))?
            .to_owned();
        templates.push(HtmlTemplate { path, id });
    }
    Ok(templates)
}

fn resolve_template_instances(
    templates: Vec<HtmlTemplate>,
    screen_inputs: &[PathBuf],
    frame_enabled: bool,
) -> Result<Vec<TemplateInstance>> {
    if !frame_enabled {
        ensure!(
            screen_inputs.is_empty(),
            "--screen can only be used together with --frame"
        );
        return Ok(templates
            .into_iter()
            .map(|template| TemplateInstance {
                template,
                screen: None,
            })
            .collect());
    }

    ensure!(
        !screen_inputs.is_empty(),
        "--frame requires --screen IMAGE_OR_GLOB"
    );
    let screens = resolve_screen_images(screen_inputs)?;
    if screens.len() == 1 {
        let screen = screens[0].clone();
        return Ok(templates
            .into_iter()
            .map(|template| TemplateInstance {
                template,
                screen: Some(screen.clone()),
            })
            .collect());
    }

    ensure!(
        screens.len() == templates.len(),
        "--screen resolved {} image(s), but --input resolved {} HTML template(s); pass one screen for all templates or the same number of screens as templates",
        screens.len(),
        templates.len()
    );
    Ok(templates
        .into_iter()
        .zip(screens)
        .map(|(template, screen)| TemplateInstance {
            template,
            screen: Some(screen),
        })
        .collect())
}

fn resolve_screen_images(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for input in inputs {
        let value = input.to_string_lossy();
        if contains_glob_meta(&value) {
            let mut matched = glob(&value)
                .with_context(|| format!("invalid glob pattern {value}"))?
                .map(|entry| entry.with_context(|| format!("failed to read glob entry {value}")))
                .collect::<Result<Vec<_>>>()?;
            matched.sort();
            paths.extend(matched);
        } else if input.is_dir() {
            let mut entries = fs::read_dir(input)
                .with_context(|| format!("failed to read {}", input.display()))?
                .map(|entry| {
                    let entry = entry
                        .with_context(|| format!("failed to read entry in {}", input.display()))?;
                    Ok(entry.path())
                })
                .collect::<Result<Vec<_>>>()?;
            entries.retain(|path| is_screen_image_path(path));
            entries.sort();
            paths.extend(entries);
        } else {
            paths.push(input.clone());
        }
    }

    ensure!(!paths.is_empty(), "--screen did not match any image files");
    let mut seen = BTreeSet::new();
    let mut images = Vec::new();
    for path in paths {
        ensure!(
            path.exists(),
            "screen image {} does not exist",
            path.display()
        );
        ensure!(
            path.is_file(),
            "screen image {} is not a file",
            path.display()
        );
        ensure!(
            is_screen_image_path(&path),
            "screen image {} must be .png, .jpg, .jpeg, or .webp",
            path.display()
        );
        let path = path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()))?;
        ensure!(
            seen.insert(path.clone()),
            "screen image {} is listed more than once",
            path.display()
        );
        images.push(path);
    }
    Ok(images)
}

fn is_screen_image_path(path: &Path) -> bool {
    matches!(
        lower_extension(path).as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp")
    )
}

fn load_template_data(
    locale: Option<&str>,
    strings_path: Option<&PathBuf>,
) -> Result<TemplateData> {
    let strings = match strings_path {
        Some(path) => {
            let content = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let value: Value = json5::from_str(&content)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            ensure!(
                value.is_object(),
                "template strings file {} must contain a JSON object",
                path.display()
            );
            value
        }
        None => Value::Object(Map::new()),
    };
    Ok(TemplateData {
        locale: locale.map(str::to_owned),
        strings,
    })
}

fn load_version_config_template_data(
    config_dir: &Path,
    locale: &str,
    source: &AppVersionLocalizationSource,
) -> Result<TemplateData> {
    let resolver = RenderValueResolver::new(config_dir)?;
    let spec = match source {
        AppVersionLocalizationSource::Inline(spec) => (**spec).clone(),
        AppVersionLocalizationSource::Path(path) => {
            let path = resolve_config_path(config_dir, path);
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            json5::from_str::<AppVersionLocalizationSpec>(&content)
                .with_context(|| format!("failed to parse JSON5 {}", path.display()))?
        }
    };

    let mut strings = Map::new();
    insert_optional_render_string(
        &mut strings,
        "description",
        spec.description.as_ref(),
        &resolver,
    )?;
    insert_optional_render_string(
        &mut strings,
        "marketing_url",
        spec.marketing_url.as_ref(),
        &resolver,
    )?;
    insert_optional_render_string(
        &mut strings,
        "promotional_text",
        spec.promotional_text.as_ref(),
        &resolver,
    )?;
    insert_optional_render_string(
        &mut strings,
        "support_url",
        spec.support_url.as_ref(),
        &resolver,
    )?;
    insert_optional_render_string(
        &mut strings,
        "whats_new",
        spec.whats_new.as_ref(),
        &resolver,
    )?;
    if let Some(keywords) = &spec.keywords {
        let value = match keywords {
            KeywordsSpec::String(source) => resolver.resolve_string(source)?,
            KeywordsSpec::List(values) => values.join(","),
        };
        strings.insert("keywords".to_owned(), Value::String(value));
    }
    for (key, value) in spec.render_strings {
        strings.insert(key, resolver.resolve_json_value(value)?);
    }

    Ok(TemplateData {
        locale: Some(locale.to_owned()),
        strings: Value::Object(strings),
    })
}

fn load_custom_product_page_config_template_data(
    config_dir: &Path,
    locale: &str,
    source: &CustomProductPageLocalizationSource,
) -> Result<TemplateData> {
    let resolver = RenderValueResolver::new(config_dir)?;
    let spec = match source {
        CustomProductPageLocalizationSource::Inline(spec) => spec.clone(),
        CustomProductPageLocalizationSource::Path(path) => {
            let path = resolve_config_path(config_dir, path);
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            json5::from_str::<CustomProductPageLocalizationSpec>(&content)
                .with_context(|| format!("failed to parse JSON5 {}", path.display()))?
        }
    };

    let mut strings = Map::new();
    insert_optional_render_string(
        &mut strings,
        "promotional_text",
        spec.promotional_text.as_ref(),
        &resolver,
    )?;
    for (key, value) in spec.render_strings {
        strings.insert(key, resolver.resolve_json_value(value)?);
    }

    Ok(TemplateData {
        locale: Some(locale.to_owned()),
        strings: Value::Object(strings),
    })
}

fn insert_optional_render_string(
    strings: &mut Map<String, Value>,
    key: &str,
    source: Option<&StringSource>,
    resolver: &RenderValueResolver,
) -> Result<()> {
    if let Some(source) = source {
        strings.insert(
            key.to_owned(),
            Value::String(resolver.resolve_string(source)?),
        );
    }
    Ok(())
}

struct RenderValueResolver {
    dotenv: BTreeMap<String, String>,
}

impl RenderValueResolver {
    fn new(config_dir: &Path) -> Result<Self> {
        let dotenv_path = config_dir.join(".env");
        let dotenv = if dotenv_path.exists() {
            dotenvy::from_path_iter(&dotenv_path)
                .with_context(|| format!("failed to read {}", dotenv_path.display()))?
                .map(|item| item.context("failed to parse .env entry"))
                .collect::<Result<BTreeMap<_, _>>>()?
        } else {
            BTreeMap::new()
        };
        Ok(Self { dotenv })
    }

    fn resolve_string(&self, source: &StringSource) -> Result<String> {
        match source {
            StringSource::Literal(value) => Ok(value.clone()),
            StringSource::Env { env: key } => env::var(key)
                .ok()
                .or_else(|| self.dotenv.get(key).cloned())
                .ok_or_else(|| anyhow::anyhow!("missing environment value {key}")),
        }
    }

    fn resolve_json_value(&self, value: Value) -> Result<Value> {
        match value {
            Value::Object(object) => self.resolve_json_object(object),
            Value::Array(values) => values
                .into_iter()
                .map(|value| self.resolve_json_value(value))
                .collect::<Result<Vec<_>>>()
                .map(Value::Array),
            value => Ok(value),
        }
    }

    fn resolve_json_object(&self, object: Map<String, Value>) -> Result<Value> {
        if object.len() == 1
            && let Some(Value::String(key)) = object.get("$env")
        {
            return Ok(Value::String(
                env::var(key)
                    .ok()
                    .or_else(|| self.dotenv.get(key).cloned())
                    .ok_or_else(|| anyhow::anyhow!("missing environment value {key}"))?,
            ));
        }

        let mut resolved = Map::new();
        for (key, value) in object {
            resolved.insert(
                key,
                self.resolve_json_value(value)
                    .context("failed to resolve render string value")?,
            );
        }
        Ok(Value::Object(resolved))
    }
}

fn resolve_config_path_list(config_dir: &Path, list: &MediaPathList) -> Vec<PathBuf> {
    list.paths()
        .into_iter()
        .map(|path| resolve_config_path(config_dir, path))
        .collect()
}

fn resolve_config_path(config_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.join(path)
    }
}

fn resolve_config_render_output_dir(
    config_dir: &Path,
    render: &MediaScreenshotRenderSpec,
    fallback_output_dir: &Path,
) -> PathBuf {
    render
        .output_dir
        .as_ref()
        .map(|path| resolve_config_path(config_dir, path))
        .unwrap_or_else(|| fallback_output_dir.to_path_buf())
}

fn apply_template_variables(html: &str, data: &TemplateData, template_id: &str) -> Result<String> {
    let mut output = String::with_capacity(html.len());
    let mut rest = html;

    while let Some(start) = rest.find("{{") {
        output.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find("}}") else {
            bail!("unclosed template variable in {template_id}");
        };
        let key = after_start[..end].trim();
        ensure!(!key.is_empty(), "empty template variable in {template_id}");
        let value = resolve_template_variable(data, template_id, key)?;
        output.push_str(&html_escape(&value));
        rest = &after_start[end + 2..];
    }

    output.push_str(rest);
    Ok(output)
}

fn resolve_template_variable(data: &TemplateData, template_id: &str, key: &str) -> Result<String> {
    match key {
        "asc_id" | "id" => return Ok(template_id.to_owned()),
        "locale" => return Ok(data.locale.clone().unwrap_or_default()),
        _ => {}
    }

    let value = lookup_json_path(&data.strings, key)
        .ok_or_else(|| anyhow::anyhow!("template variable {{{{{key}}}}} was not found"))?;
    json_value_to_template_string(value, key)
}

fn lookup_json_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        if segment.is_empty() {
            return None;
        }
        current = match current {
            Value::Object(object) => object.get(segment)?,
            Value::Array(values) => values.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn json_value_to_template_string(value: &Value, key: &str) -> Result<String> {
    match value {
        Value::Null => Ok(String::new()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::String(value) => Ok(value.clone()),
        Value::Array(_) | Value::Object(_) => {
            bail!("template variable {{{{{key}}}}} must resolve to a scalar value")
        }
    }
}

fn resolve_render_context(
    size: Option<&str>,
    viewport: Option<&str>,
    frame: Option<&str>,
    frame_dir: Option<&PathBuf>,
) -> Result<RenderContext> {
    let viewport = resolve_viewport(size, viewport)?;
    let frame = match frame {
        Some(frame) => Some(resolve_frame(frame, frame_dir)?),
        None => None,
    };
    Ok(RenderContext { viewport, frame })
}

fn resolve_viewport(size: Option<&str>, viewport: Option<&str>) -> Result<Viewport> {
    match (size, viewport) {
        (Some(size), None) => named_viewport(size),
        (None, Some(viewport)) => parse_viewport(viewport),
        (None, None) => named_viewport("iphone67"),
        (Some(_), Some(_)) => bail!("use either --size or --viewport, not both"),
    }
}

fn named_viewport(size: &str) -> Result<Viewport> {
    let viewport = match size {
        "iphone" | "iphone67" => (1320, 2868),
        "iphone65" => (1284, 2778),
        "iphone61" | "iphone58" => (1170, 2532),
        "iphone55" => (1242, 2208),
        "iphone47" => (750, 1334),
        "iphone40" => (640, 1136),
        "iphone35" => (640, 960),
        "ipad" | "ipad13" => (2064, 2752),
        "ipad129" => (2048, 2732),
        "ipad11" => (1668, 2420),
        "ipad105" => (1668, 2224),
        "ipad97" => (1536, 2048),
        "mac" => (2880, 1800),
        "apple_tv" => (3840, 2160),
        "vision_pro" => (3840, 2160),
        "watch" | "watch_series10" => (416, 496),
        "watch_ultra" => (422, 514),
        "watch_series7" => (396, 484),
        "watch_series4" => (368, 448),
        "watch_series3" => (312, 390),
        other => {
            bail!("unknown render size {other:?}; use --viewport WIDTHxHEIGHT for custom sizes")
        }
    };
    Ok(Viewport {
        width: viewport.0,
        height: viewport.1,
    })
}

fn parse_viewport(value: &str) -> Result<Viewport> {
    let Some((width, height)) = value.split_once('x') else {
        bail!("viewport must use WIDTHxHEIGHT, got {value:?}");
    };
    let width = width
        .parse::<u32>()
        .with_context(|| format!("invalid viewport width in {value:?}"))?;
    let height = height
        .parse::<u32>()
        .with_context(|| format!("invalid viewport height in {value:?}"))?;
    ensure!(
        width > 0 && height > 0,
        "viewport dimensions must be positive"
    );
    Ok(Viewport { width, height })
}

#[derive(Debug, Deserialize)]
struct FrameAssetManifest {
    files: Vec<FrameAssetManifestFile>,
}

#[derive(Debug, Clone, Deserialize)]
struct FrameAssetManifestFile {
    path: String,
    size: u64,
    md5: String,
}

fn resolve_frame(name: &str, explicit_dir: Option<&PathBuf>) -> Result<FrameSpec> {
    let frame_dir = resolve_frame_dir(name, explicit_dir)?;
    let image_path = resolve_frame_image_path(&frame_dir, name)?;
    let (width, height) = image::image_dimensions(&image_path)
        .with_context(|| format!("failed to read frame image {}", image_path.display()))?;
    let image_size = Viewport { width, height };
    let screen = resolve_frame_screen_bounds(&frame_dir, name, image_size, &image_path)?;
    Ok(FrameSpec {
        name: name.to_owned(),
        image_path,
        image_size,
        screen,
    })
}

fn resolve_frame_dir(frame_name: &str, explicit_dir: Option<&PathBuf>) -> Result<PathBuf> {
    let direct_frame = Path::new(frame_name);
    if direct_frame.is_file() {
        let parent = direct_frame.parent().unwrap_or_else(|| Path::new("."));
        return parent
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", parent.display()));
    }

    let mut candidates = Vec::new();
    if let Some(explicit_dir) = explicit_dir {
        candidates.push(explicit_dir.clone());
    }
    if let Some(value) = env::var_os("ASC_SYNC_FRAMES_DIR") {
        candidates.push(PathBuf::from(value));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/device-frames"));
    for path in [
        "assets/device-frames",
        "device-frames",
        "frames",
        "vendor/device-frames",
    ] {
        candidates.push(PathBuf::from(path));
    }

    for candidate in candidates {
        if candidate.is_dir() {
            return candidate
                .canonicalize()
                .with_context(|| format!("failed to canonicalize {}", candidate.display()));
        }
    }

    ensure_cached_frame_dir(frame_name)
}

fn ensure_cached_frame_dir(frame_name: &str) -> Result<PathBuf> {
    let base_url = device_frames_base_url();
    let manifest_url = format!("{base_url}/manifest.json");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("failed to build device frame download client")?;
    let manifest = download_frame_manifest(&client, &manifest_url).with_context(|| {
        format!(
            "failed to load device frame manifest from {manifest_url}; pass --frame-dir or set ASC_SYNC_FRAMES_DIR for local frames"
        )
    })?;
    let cache_dir = system::global_asc_sync_dir()?.join("device-frames");
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("failed to create {}", cache_dir.display()))?;

    for asset in required_frame_assets(&manifest, frame_name)? {
        ensure_cached_frame_asset(&client, &base_url, &cache_dir, &asset)?;
    }

    Ok(cache_dir)
}

fn device_frames_base_url() -> String {
    env::var(DEVICE_FRAMES_URL_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_DEVICE_FRAMES_URL.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

fn download_frame_manifest(
    client: &reqwest::blocking::Client,
    manifest_url: &str,
) -> Result<FrameAssetManifest> {
    let response = client
        .get(manifest_url)
        .send()
        .with_context(|| format!("failed to request {manifest_url}"))?
        .error_for_status()
        .with_context(|| format!("device frame manifest request failed: {manifest_url}"))?;
    response
        .json::<FrameAssetManifest>()
        .with_context(|| format!("failed to parse device frame manifest from {manifest_url}"))
}

fn required_frame_assets(
    manifest: &FrameAssetManifest,
    frame_name: &str,
) -> Result<Vec<FrameAssetManifestFile>> {
    let mut assets = Vec::new();
    for path in ["Frames.json", "Sizes.json"] {
        if let Some(asset) = manifest.files.iter().find(|asset| asset.path == path) {
            assets.push(asset.clone());
        }
    }

    let png = format!("{frame_name}.png");
    let uppercase_png = format!("{frame_name}.PNG");
    let frame_asset = manifest
        .files
        .iter()
        .find(|asset| asset.path == png || asset.path == uppercase_png)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "device frame {frame_name:?} was not found in remote device frame manifest"
            )
        })?;
    assets.push(frame_asset);
    Ok(assets)
}

fn ensure_cached_frame_asset(
    client: &reqwest::blocking::Client,
    base_url: &str,
    cache_dir: &Path,
    asset: &FrameAssetManifestFile,
) -> Result<()> {
    let relative_path = safe_manifest_relative_path(&asset.path)?;
    let path = cache_dir.join(relative_path);
    if cached_frame_asset_matches(&path, asset)? {
        return Ok(());
    }

    let url = format!("{base_url}/{}", percent_encode(&asset.path));
    println!("downloading device frame asset {}", asset.path);
    let bytes = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("device frame asset request failed: {url}"))?
        .bytes()
        .with_context(|| format!("failed to read device frame asset from {url}"))?;
    ensure!(
        bytes.len() as u64 == asset.size,
        "downloaded device frame asset {} has size {}, expected {}",
        asset.path,
        bytes.len(),
        asset.size
    );
    let md5 = md5_hex(&bytes);
    ensure!(
        md5 == asset.md5,
        "downloaded device frame asset {} has md5 {}, expected {}",
        asset.path,
        md5,
        asset.md5
    );

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("invalid cached asset path {}", path.display()))?;
    let temp_path = path.with_file_name(format!(".{file_name}.download"));
    fs::write(&temp_path, &bytes)
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("failed to replace {}", path.display()))?;
    }
    fs::rename(&temp_path, &path).with_context(|| {
        format!(
            "failed to move {} to {}",
            temp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn safe_manifest_relative_path(path: &str) -> Result<PathBuf> {
    ensure!(!path.is_empty(), "device frame asset path cannot be empty");
    ensure!(
        !path.starts_with('/') && !path.contains('\\'),
        "device frame asset path cannot be absolute: {path}"
    );
    let mut output = PathBuf::new();
    for component in path.split('/') {
        ensure!(
            !component.is_empty() && component != "." && component != "..",
            "device frame asset path cannot escape cache directory: {path}"
        );
        output.push(component);
    }
    Ok(output)
}

fn cached_frame_asset_matches(path: &Path, asset: &FrameAssetManifestFile) -> Result<bool> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(false);
    };
    if !metadata.is_file() || metadata.len() != asset.size {
        return Ok(false);
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(md5_hex(&bytes) == asset.md5)
}

fn md5_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Md5::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn resolve_frame_image_path(frame_dir: &Path, name: &str) -> Result<PathBuf> {
    let direct = Path::new(name);
    if direct.is_file() {
        return direct
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", direct.display()));
    }

    for extension in ["png", "PNG"] {
        let path = frame_dir.join(format!("{name}.{extension}"));
        if path.is_file() {
            return path
                .canonicalize()
                .with_context(|| format!("failed to canonicalize {}", path.display()));
        }
    }

    bail!(
        "device frame {name:?} was not found in {}",
        frame_dir.display()
    )
}

fn resolve_frame_screen_bounds(
    frame_dir: &Path,
    name: &str,
    image_size: Viewport,
    image_path: &Path,
) -> Result<Rect> {
    let metadata_path = frame_dir.join("Frames.json");
    if metadata_path.exists() {
        let value: Value = serde_json::from_str(
            &fs::read_to_string(&metadata_path)
                .with_context(|| format!("failed to read {}", metadata_path.display()))?,
        )
        .with_context(|| format!("failed to parse {}", metadata_path.display()))?;
        if let Some(entry) = find_frame_metadata(&value, name) {
            return parse_frame_bounds(entry, image_size)
                .with_context(|| format!("invalid metadata for device frame {name:?}"));
        }
    }

    detect_screen_bounds_from_alpha(image_path)
        .with_context(|| format!("failed to detect screen bounds for device frame {name:?}"))
}

fn find_frame_metadata<'a>(value: &'a Value, name: &str) -> Option<&'a Map<String, Value>> {
    let object = value.as_object()?;
    if object.get("name").and_then(Value::as_str) == Some(name) {
        return Some(object);
    }
    object
        .values()
        .find_map(|child| find_frame_metadata(child, name))
}

fn parse_frame_bounds(metadata: &Map<String, Value>, image_size: Viewport) -> Result<Rect> {
    if let Some(bounds) = metadata.get("screen_bounds").and_then(Value::as_object) {
        return Ok(Rect {
            x: metadata_u32(bounds, "x")?,
            y: metadata_u32(bounds, "y")?,
            width: metadata_u32(bounds, "width")?,
            height: metadata_u32(bounds, "height")?,
        });
    }

    let x = metadata_u32(metadata, "x")?;
    let y = metadata_u32(metadata, "y")?;
    ensure!(
        x.saturating_mul(2) < image_size.width && y.saturating_mul(2) < image_size.height,
        "legacy x/y bounds do not fit frame image"
    );
    Ok(Rect {
        x,
        y,
        width: image_size.width - (x * 2),
        height: image_size.height - (y * 2),
    })
}

fn metadata_u32(metadata: &Map<String, Value>, key: &str) -> Result<u32> {
    let value = metadata
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("metadata field {key:?} is missing"))?;
    match value {
        Value::Number(number) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| anyhow::anyhow!("metadata field {key:?} must be a u32")),
        Value::String(value) => value
            .parse::<u32>()
            .with_context(|| format!("metadata field {key:?} must be a u32")),
        _ => bail!("metadata field {key:?} must be a u32"),
    }
}

fn detect_screen_bounds_from_alpha(path: &Path) -> Result<Rect> {
    let image = image::open(path)
        .with_context(|| format!("failed to open frame image {}", path.display()))?
        .to_rgba8();
    let (width, height) = image.dimensions();
    ensure!(width > 0 && height > 0, "frame image is empty");

    let mut visited = vec![false; (width as usize) * (height as usize)];
    let mut queue = VecDeque::new();
    for x in 0..width {
        queue.push_back((x, 0));
        queue.push_back((x, height - 1));
    }
    for y in 0..height {
        queue.push_back((0, y));
        queue.push_back((width - 1, y));
    }

    while let Some((x, y)) = queue.pop_front() {
        let index = pixel_index(width, x, y);
        if visited[index] {
            continue;
        }
        if image.get_pixel(x, y).0[3] > 50 {
            continue;
        }
        visited[index] = true;
        if x > 0 {
            queue.push_back((x - 1, y));
        }
        if x + 1 < width {
            queue.push_back((x + 1, y));
        }
        if y > 0 {
            queue.push_back((x, y - 1));
        }
        if y + 1 < height {
            queue.push_back((x, y + 1));
        }
    }

    let mut left = width;
    let mut top = height;
    let mut right = 0;
    let mut bottom = 0;
    for y in 0..height {
        for x in 0..width {
            let alpha = image.get_pixel(x, y).0[3];
            if alpha <= 50 && !visited[pixel_index(width, x, y)] {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }

    ensure!(
        left < right && top < bottom,
        "frame image does not contain an inner transparent screen area"
    );
    Ok(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn pixel_index(width: u32, x: u32, y: u32) -> usize {
    ((y * width) + x) as usize
}

fn find_chrome(explicit: Option<&PathBuf>) -> Result<OsString> {
    let mut candidates = Vec::new();
    if let Some(explicit) = explicit {
        push_chrome_candidate(&mut candidates, explicit.as_os_str());
    }
    for key in [
        "CHROME",
        "CHROME_BIN",
        "CHROME_PATH",
        "GOOGLE_CHROME",
        "CHROMIUM_BIN",
    ] {
        if let Some(value) = env::var_os(key) {
            push_chrome_candidate(&mut candidates, &value);
        }
    }
    push_macos_chrome_app_candidates(&mut candidates);
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "google-chrome-beta",
        "google-chrome-unstable",
        "google-chrome-canary",
        "chromium",
        "chromium-browser",
        "chrome",
    ] {
        push_unique_candidate(&mut candidates, OsString::from(name));
    }

    for candidate in candidates {
        if command_works(&candidate) {
            return Ok(candidate);
        }
    }
    bail!(
        "Chrome/Chromium was not found; pass --chrome or set CHROME_BIN/CHROME_PATH/CHROME/GOOGLE_CHROME/CHROMIUM_BIN"
    )
}

fn push_chrome_candidate(candidates: &mut Vec<OsString>, candidate: &OsStr) {
    if candidate.is_empty() {
        return;
    }
    if let Some(executable) = macos_app_bundle_executable(candidate) {
        push_unique_candidate(candidates, executable.into_os_string());
    }
    push_unique_candidate(candidates, candidate.to_os_string());
}

fn push_macos_chrome_app_candidates(candidates: &mut Vec<OsString>) {
    push_macos_chrome_app_candidates_from_roots(candidates, macos_app_roots());
}

fn push_macos_chrome_app_candidates_from_roots(
    candidates: &mut Vec<OsString>,
    roots: impl IntoIterator<Item = PathBuf>,
) {
    for root in roots {
        for (bundle, executable) in MACOS_CHROME_APPS {
            push_unique_candidate(
                candidates,
                root.join(bundle)
                    .join("Contents")
                    .join("MacOS")
                    .join(executable)
                    .into_os_string(),
            );
        }
    }
}

fn macos_app_roots() -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    roots
}

fn macos_app_bundle_executable(candidate: &OsStr) -> Option<PathBuf> {
    let path = Path::new(candidate);
    if path.extension()? != OsStr::new("app") {
        return None;
    }
    let executable = path.file_stem()?;
    Some(path.join("Contents").join("MacOS").join(executable))
}

fn push_unique_candidate(candidates: &mut Vec<OsString>, candidate: OsString) {
    if !candidates.iter().any(|existing| existing == &candidate) {
        candidates.push(candidate);
    }
}

fn command_works(command: &OsString) -> bool {
    if Path::new(command).components().count() > 1 && !Path::new(command).exists() {
        return false;
    }
    Command::new(command)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

const MACOS_CHROME_APPS: &[(&str, &str)] = &[
    ("Google Chrome.app", "Google Chrome"),
    ("Google Chrome Beta.app", "Google Chrome Beta"),
    ("Google Chrome Dev.app", "Google Chrome Dev"),
    ("Google Chrome Canary.app", "Google Chrome Canary"),
    ("Google Chrome for Testing.app", "Google Chrome for Testing"),
    ("Chromium.app", "Chromium"),
];

struct PreviewServer {
    instances: Vec<TemplateInstance>,
    context: RenderContext,
    template_data: TemplateData,
}

impl PreviewServer {
    fn handle(&self, stream: &mut TcpStream) -> Result<()> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("failed to set preview read timeout")?;
        let mut buffer = [0_u8; 8192];
        let read = stream.read(&mut buffer).context("failed to read request")?;
        let request = String::from_utf8_lossy(&buffer[..read]);
        let path = parse_request_path(&request)?;

        if path == "/" {
            return write_response(
                stream,
                200,
                "text/html; charset=utf-8",
                self.dashboard().as_bytes(),
            );
        }
        if let Some(index) = path.strip_prefix("/template/") {
            let index = parse_index(index)?;
            let instance = self
                .instances
                .get(index)
                .ok_or_else(|| anyhow::anyhow!("template index {index} not found"))?;
            let html = prepare_template_html(
                instance,
                &self.context,
                &self.template_data,
                &format!("/assets/{index}/"),
                |_, fit| Ok(format!("/framed/{index}/{}", fit.as_str())),
            )?;
            return write_response(stream, 200, "text/html; charset=utf-8", html.as_bytes());
        }
        if let Some(rest) = path.strip_prefix("/framed/") {
            let Some((index, fit)) = rest.split_once('/') else {
                bail!("invalid framed asset path");
            };
            let index = parse_index(index)?;
            let fit = FrameFit::parse(fit)?;
            let instance = self
                .instances
                .get(index)
                .ok_or_else(|| anyhow::anyhow!("template index {index} not found"))?;
            let frame = self
                .context
                .frame
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("preview frame is not configured"))?;
            let screen = instance
                .screen
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("template index {index} has no screen image"))?;
            let bytes = framed_screen_png_bytes(frame, screen, fit)?;
            return write_response(stream, 200, "image/png", &bytes);
        }
        if let Some(index) = path.strip_prefix("/screen/") {
            let index = parse_index(index)?;
            let instance = self
                .instances
                .get(index)
                .ok_or_else(|| anyhow::anyhow!("template index {index} not found"))?;
            let screen = instance
                .screen
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("template index {index} has no screen image"))?;
            let bytes = fs::read(screen)
                .with_context(|| format!("failed to read screen {}", screen.display()))?;
            return write_response(stream, 200, content_type(screen), &bytes);
        }
        if let Some(rest) = path.strip_prefix("/assets/") {
            let Some((index, asset_path)) = rest.split_once('/') else {
                bail!("invalid asset path");
            };
            let index = parse_index(index)?;
            let instance = self
                .instances
                .get(index)
                .ok_or_else(|| anyhow::anyhow!("template index {index} not found"))?;
            let root = instance
                .template
                .path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("template has no parent directory"))?;
            let asset = safe_join(root, asset_path)?;
            let bytes = fs::read(&asset)
                .with_context(|| format!("failed to read asset {}", asset.display()))?;
            return write_response(stream, 200, content_type(&asset), &bytes);
        }

        write_response(stream, 404, "text/plain; charset=utf-8", b"not found")
    }

    fn dashboard(&self) -> String {
        let mut cards = String::new();
        for (index, instance) in self.instances.iter().enumerate() {
            let source = template_url(
                &format!("/template/{index}"),
                &instance.template.id,
                self.context.viewport.width,
                self.context.viewport.height,
            );
            cards.push_str(&format!(
                r#"<article class="card">
  <header>{}</header>
  <iframe style="width:{}px;height:{}px" src="{}"></iframe>
</article>"#,
                html_escape(&instance.template.id),
                self.context.viewport.width,
                self.context.viewport.height,
                html_escape(&source),
            ));
        }

        format!(
            r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>asc-sync media preview</title>
  <style>
    body {{ margin: 0; background: #151515; color: #f3f0e8; font: 15px/1.4 ui-sans-serif, system-ui; }}
    main {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 24px; padding: 24px; }}
    .card {{ background: #222; border: 1px solid #393939; border-radius: 18px; padding: 14px; overflow: auto; }}
    header {{ margin: 0 0 12px; color: #c8b88a; font-weight: 700; }}
    iframe {{ border: 0; background: white; transform: scale(var(--scale)); transform-origin: top left; }}
    .card {{ --scale: min(1, calc((100vw - 80px) / {})); }}
  </style>
</head>
<body>
  <main>{cards}</main>
</body>
</html>"#,
            self.context.viewport.width
        )
    }
}

fn parse_request_path(request: &str) -> Result<String> {
    let line = request
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty request"))?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    ensure!(
        method == "GET" || method == "HEAD",
        "unsupported method {method}"
    );
    let raw_path = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("request path is missing"))?;
    Ok(raw_path.split('?').next().unwrap_or(raw_path).to_owned())
}

fn parse_index(value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .with_context(|| format!("invalid preview index {value:?}"))
}

fn safe_join(root: &Path, request_path: &str) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    for component in request_path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        ensure!(
            component != "..",
            "asset path cannot escape template directory"
        );
        path.push(percent_decode(component)?);
    }
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", root.display()))?;
    let canonical_path = path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))?;
    ensure!(
        canonical_path.starts_with(canonical_root),
        "asset path cannot escape template directory"
    );
    Ok(canonical_path)
}

fn inject_base(html: &str, href: &str) -> String {
    inject_head_content(html, &format!(r#"<base href="{href}">"#))
}

fn inject_head_content(html: &str, content: &str) -> String {
    if let Some(head_start) = find_ascii_case_insensitive(html, "<head")
        && let Some(head_end) = html[head_start..].find('>')
    {
        let insert_at = head_start + head_end + 1;
        let mut output = String::with_capacity(html.len() + content.len());
        output.push_str(&html[..insert_at]);
        output.push_str(content);
        output.push_str(&html[insert_at..]);
        return output;
    }
    format!("{content}{html}")
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "OK",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .context("failed to write response headers")?;
    stream
        .write_all(body)
        .context("failed to write response body")
}

fn content_type(path: &Path) -> &'static str {
    match lower_extension(path).as_deref() {
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn file_url(path: &Path) -> Result<String> {
    let path = path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))?;
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("path {} is not UTF-8", path.display()))?;
    Ok(format!("file://{}", percent_encode(value)))
}

fn directory_file_url(path: &Path) -> Result<String> {
    let mut url = file_url(path)?;
    if !url.ends_with('/') {
        url.push('/');
    }
    Ok(url)
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn percent_decode(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            ensure!(index + 2 < bytes.len(), "invalid percent escape");
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .context("invalid percent escape")?;
            output.push(u8::from_str_radix(hex, 16).context("invalid percent escape")?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).context("decoded path is not UTF-8")
}

fn html_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            '&' => "&amp;".chars().collect::<Vec<_>>(),
            '<' => "&lt;".chars().collect(),
            '>' => "&gt;".chars().collect(),
            '"' => "&quot;".chars().collect(),
            '\'' => "&#39;".chars().collect(),
            _ => vec![ch],
        })
        .collect()
}

fn lower_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

fn contains_glob_meta(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.contains('[')
}

#[cfg(test)]
mod tests {
    use super::{
        FrameAssetManifest, FrameAssetManifestFile, FrameSpec, Rect, TemplateData, Viewport,
        apply_template_variables, cached_frame_asset_matches, compose_framed_screen,
        detect_screen_bounds_from_alpha, file_url, find_frame_metadata, flatten_png_alpha_to_white,
        load_custom_product_page_config_template_data, load_version_config_template_data,
        macos_app_bundle_executable, md5_hex, parse_frame_bounds, parse_viewport, percent_decode,
        percent_encode, push_macos_chrome_app_candidates_from_roots, replace_frame_placeholders,
        required_frame_assets, resolve_config_render_output_dir, safe_manifest_relative_path,
    };
    use crate::config::{
        AppVersionLocalizationSource, CustomProductPageLocalizationSource, MediaPathList,
        MediaScreenshotRenderSpec,
    };
    use image::{Rgba, RgbaImage};
    use serde_json::json;
    use std::{
        ffi::OsStr,
        fs,
        path::{Path, PathBuf},
    };

    #[test]
    fn parses_viewport() {
        assert_eq!(
            parse_viewport("1320x2868").unwrap(),
            Viewport {
                width: 1320,
                height: 2868
            }
        );
        assert!(parse_viewport("1320").is_err());
    }

    #[test]
    fn percent_roundtrip_handles_spaces() {
        let encoded = percent_encode("/tmp/app screenshots/01 hero.html");
        assert_eq!(encoded, "/tmp/app%20screenshots/01%20hero.html".to_owned());
        assert_eq!(
            percent_decode("01%20hero.html").unwrap(),
            "01 hero.html".to_owned()
        );
    }

    #[test]
    fn manifest_paths_cannot_escape_cache_dir() {
        assert_eq!(
            safe_manifest_relative_path("Frames.json").unwrap(),
            PathBuf::from("Frames.json")
        );
        assert!(safe_manifest_relative_path("../Frames.json").is_err());
        assert!(safe_manifest_relative_path("/tmp/Frames.json").is_err());
        assert!(safe_manifest_relative_path("nested\\Frames.json").is_err());
    }

    #[test]
    fn required_frame_assets_include_metadata_and_requested_png() {
        let manifest = FrameAssetManifest {
            files: vec![
                FrameAssetManifestFile {
                    path: "Other.png".to_owned(),
                    size: 1,
                    md5: "other".to_owned(),
                },
                FrameAssetManifestFile {
                    path: "Frames.json".to_owned(),
                    size: 2,
                    md5: "frames".to_owned(),
                },
                FrameAssetManifestFile {
                    path: "iPhone 16.png".to_owned(),
                    size: 3,
                    md5: "iphone".to_owned(),
                },
            ],
        };

        let assets = required_frame_assets(&manifest, "iPhone 16").unwrap();
        let paths = assets
            .iter()
            .map(|asset| asset.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["Frames.json", "iPhone 16.png"]);
    }

    #[test]
    fn cached_frame_asset_match_checks_size_and_hash() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("Frame.png");
        fs::write(&path, b"frame").unwrap();
        let asset = FrameAssetManifestFile {
            path: "Frame.png".to_owned(),
            size: 5,
            md5: md5_hex(b"frame"),
        };

        assert!(cached_frame_asset_matches(&path, &asset).unwrap());
        let stale_asset = FrameAssetManifestFile {
            md5: md5_hex(b"other"),
            ..asset
        };
        assert!(!cached_frame_asset_matches(&path, &stale_asset).unwrap());
    }

    #[test]
    fn expands_macos_app_bundle_to_executable_path() {
        assert_eq!(
            macos_app_bundle_executable(OsStr::new("/Applications/Google Chrome Canary.app"))
                .unwrap(),
            PathBuf::from(
                "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary"
            )
        );
    }

    #[test]
    fn macos_chrome_candidates_include_channels_and_user_apps() {
        let mut candidates = Vec::new();
        push_macos_chrome_app_candidates_from_roots(
            &mut candidates,
            [
                PathBuf::from("/Applications"),
                PathBuf::from("/Users/example/Applications"),
            ],
        );

        for path in [
            "/Applications/Google Chrome Beta.app/Contents/MacOS/Google Chrome Beta",
            "/Applications/Google Chrome Dev.app/Contents/MacOS/Google Chrome Dev",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
            "/Users/example/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ] {
            assert!(candidates.contains(&PathBuf::from(path).into_os_string()));
        }
    }

    #[test]
    fn config_render_output_dir_prefers_configured_path() {
        let render = MediaScreenshotRenderSpec {
            template: MediaPathList::One(PathBuf::from("./templates/*.html")),
            screens: MediaPathList::One(PathBuf::from("./screens/*.png")),
            frame: "Phone".to_owned(),
            frame_dir: None,
            output_dir: Some(PathBuf::from("./media/en-US/iphone67")),
        };

        assert_eq!(
            resolve_config_render_output_dir(Path::new("/repo"), &render, Path::new("/tmp/media")),
            Path::new("/repo").join("./media/en-US/iphone67")
        );
    }

    #[test]
    fn file_url_uses_file_scheme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("example.html");
        fs::write(&path, "").unwrap();
        let url = file_url(Path::new(&path)).unwrap();
        assert!(url.starts_with("file:///"));
        assert!(url.ends_with("/example.html"));
    }

    #[test]
    fn substitutes_template_variables_and_escapes_html() {
        let data = TemplateData {
            locale: Some("en-US".to_owned()),
            strings: json!({
                "hero": { "title": "A < B" },
                "count": 3
            }),
        };
        let html = "<h1>{{ hero.title }}</h1><p>{{locale}}</p><p>{{asc_id}}</p><p>{{count}}</p>";
        let output = apply_template_variables(html, &data, "01-home").unwrap();
        assert_eq!(
            output,
            "<h1>A &lt; B</h1><p>en-US</p><p>01-home</p><p>3</p>"
        );
    }

    #[test]
    fn config_template_data_uses_version_localization_and_env_refs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), "HERO_TITLE=\"Localized title\"\n").unwrap();
        fs::write(
            dir.path().join("en-US.json5"),
            r#"{
                description: "Long description",
                keywords: ["one", "two"],
                hero: {
                    title: { $env: "HERO_TITLE" }
                }
            }"#,
        )
        .unwrap();

        let source = AppVersionLocalizationSource::Path("en-US.json5".into());
        let data = load_version_config_template_data(dir.path(), "en-US", &source).unwrap();
        assert_eq!(data.locale.as_deref(), Some("en-US"));
        assert_eq!(
            data.strings,
            json!({
                "description": "Long description",
                "keywords": "one,two",
                "hero": {
                    "title": "Localized title"
                }
            })
        );
    }

    #[test]
    fn config_template_data_uses_custom_product_page_localization() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), "CPP_TITLE=\"Seasonal page\"\n").unwrap();
        fs::write(
            dir.path().join("en-US.json5"),
            r#"{
                promotional_text: "Try the new flow",
                hero: {
                    title: { $env: "CPP_TITLE" }
                }
            }"#,
        )
        .unwrap();

        let source = CustomProductPageLocalizationSource::Path("en-US.json5".into());
        let data =
            load_custom_product_page_config_template_data(dir.path(), "en-US", &source).unwrap();
        assert_eq!(data.locale.as_deref(), Some("en-US"));
        assert_eq!(
            data.strings,
            json!({
                "promotional_text": "Try the new flow",
                "hero": {
                    "title": "Seasonal page"
                }
            })
        );
    }

    #[test]
    fn replaces_frame_placeholder_with_screen_and_frame_markup() {
        let frame = FrameSpec {
            name: "Phone".to_owned(),
            image_path: Path::new("/tmp/frame.png").to_path_buf(),
            image_size: Viewport {
                width: 1000,
                height: 2000,
            },
            screen: Rect {
                x: 100,
                y: 200,
                width: 800,
                height: 1600,
            },
        };
        let output = replace_frame_placeholders(
            r#"<section><asc-device-frame class="phone" fit="contain"></asc-device-frame></section>"#,
            &frame,
            Path::new("template.html"),
            |_, fit| {
                assert_eq!(fit.as_str(), "contain");
                Ok("framed.png".to_owned())
            },
        )
        .unwrap();
        assert!(
            output.contains(
                r#"<asc-device-frame class="phone" fit="contain" data-asc-rendered="true""#
            )
        );
        assert!(output.contains(r#"src="framed.png""#));
        assert!(output.contains("aspect-ratio:1000/2000"));
    }

    #[test]
    fn composes_framed_screen_with_alpha_mask() {
        let dir = tempfile::tempdir().unwrap();
        let frame_path = dir.path().join("frame.png");
        let screen_path = dir.path().join("screen.png");

        let mut frame_image = RgbaImage::from_pixel(5, 5, Rgba([0, 0, 0, 255]));
        for y in 1..4 {
            for x in 1..4 {
                frame_image.put_pixel(x, y, Rgba([0, 0, 0, 0]));
            }
        }
        frame_image.save(&frame_path).unwrap();
        RgbaImage::from_pixel(3, 3, Rgba([200, 10, 20, 255]))
            .save(&screen_path)
            .unwrap();

        let frame = FrameSpec {
            name: "Phone".to_owned(),
            image_path: frame_path,
            image_size: Viewport {
                width: 5,
                height: 5,
            },
            screen: Rect {
                x: 1,
                y: 1,
                width: 3,
                height: 3,
            },
        };
        let output = compose_framed_screen(&frame, &screen_path, super::FrameFit::Contain).unwrap();
        assert_eq!(output.get_pixel(2, 2).0, [200, 10, 20, 255]);
        assert_eq!(output.get_pixel(0, 0).0, [0, 0, 0, 255]);
    }

    #[test]
    fn flatten_png_alpha_uses_white_background() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alpha.png");
        let mut image = RgbaImage::from_pixel(1, 1, Rgba([255, 0, 0, 128]));
        image.put_pixel(0, 0, Rgba([255, 0, 0, 128]));
        image.save(&path).unwrap();

        flatten_png_alpha_to_white(&path).unwrap();
        let flattened = image::open(&path).unwrap().to_rgb8();
        assert_eq!(flattened.get_pixel(0, 0).0, [255, 127, 127]);
    }

    #[test]
    fn parses_legacy_frame_metadata() {
        let metadata = json!({
            "iPhone": {
                "16": {
                    "Black": {
                        "Portrait": {
                            "x": "80",
                            "y": "70",
                            "name": "iPhone 16 - Black - Portrait"
                        }
                    }
                }
            }
        });
        let entry = find_frame_metadata(&metadata, "iPhone 16 - Black - Portrait").unwrap();
        let bounds = parse_frame_bounds(
            entry,
            Viewport {
                width: 1200,
                height: 2600,
            },
        )
        .unwrap();
        assert_eq!(
            bounds,
            Rect {
                x: 80,
                y: 70,
                width: 1040,
                height: 2460
            }
        );
    }

    #[test]
    fn detects_inner_transparent_screen_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frame.png");
        let mut image = RgbaImage::from_pixel(7, 7, Rgba([0, 0, 0, 0]));
        for index in 1..=5 {
            image.put_pixel(index, 1, Rgba([0, 0, 0, 255]));
            image.put_pixel(index, 5, Rgba([0, 0, 0, 255]));
            image.put_pixel(1, index, Rgba([0, 0, 0, 255]));
            image.put_pixel(5, index, Rgba([0, 0, 0, 255]));
        }
        image.save(&path).unwrap();

        let bounds = detect_screen_bounds_from_alpha(&path).unwrap();
        assert_eq!(
            bounds,
            Rect {
                x: 2,
                y: 2,
                width: 3,
                height: 3
            }
        );
    }
}
