use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use glob::glob;
use image::GenericImageView;
use serde::Deserialize;

use crate::config::{
    AppEventMediaSpec, AppVersionLocalizationSource, CommerceReviewAssetSpec, Config,
    CustomProductPageLocalizationSource, MediaPathList, MediaPreviewSet, MediaScreenshotSet,
    MediaScreenshotSource,
};
use crate::media_render;

const MAX_PREVIEW_SIZE_BYTES: u64 = 500 * 1024 * 1024;
const MIN_PREVIEW_SECONDS: f64 = 15.0;
const MAX_PREVIEW_SECONDS: f64 = 30.0;
const MAX_PREVIEW_FPS: f64 = 30.0;

#[derive(Debug, Default, Clone, Copy)]
pub struct MediaValidationSummary {
    pub screenshot_sets: usize,
    pub screenshots: usize,
    pub preview_sets: usize,
    pub previews: usize,
    pub extra_images: usize,
    pub extra_videos: usize,
}

pub fn validate_config(config_path: &Path, config: &Config) -> Result<MediaValidationSummary> {
    let context = MediaValidationContext::new(config_path);
    let mut summary = MediaValidationSummary::default();

    for (app_key, app) in &config.apps {
        for (page_key, page) in &app.custom_product_pages {
            for (locale, media) in &page.media {
                let source = page.localizations.get(locale).ok_or_else(|| {
                    anyhow::anyhow!(
                        "app.{app_key}.custom_product_pages.{page_key}.media.{locale} requires localizations.{locale}"
                    )
                })?;
                validate_media_locale_paths(
                    &context,
                    &mut summary,
                    &format!("app.{app_key}.custom_product_pages.{page_key}.media.{locale}"),
                    locale,
                    media,
                    ScreenshotRenderContext::CustomProductPage(source),
                )?;
            }
        }

        for (iap_key, iap) in &app.in_app_purchases {
            validate_review_asset(
                &context,
                &mut summary,
                &format!("app.{app_key}.in_app_purchases.{iap_key}.review"),
                iap.review.as_ref(),
            )?;
        }

        for (group_key, group) in &app.subscription_groups {
            for (subscription_key, subscription) in &group.subscriptions {
                validate_review_asset(
                    &context,
                    &mut summary,
                    &format!(
                        "app.{app_key}.subscription_groups.{group_key}.subscriptions.{subscription_key}.review"
                    ),
                    subscription.review.as_ref(),
                )?;
            }
        }

        for (event_key, event) in &app.app_events {
            for (locale, media) in &event.media {
                ensure!(
                    event.localizations.contains_key(locale),
                    "app.{app_key}.app_events.{event_key}.media.{locale} requires localizations.{locale}"
                );
                validate_event_media(
                    &context,
                    &mut summary,
                    &format!("app.{app_key}.app_events.{event_key}.media.{locale}"),
                    media,
                )?;
            }
        }

        for (platform, platform_spec) in &app.platforms {
            for (locale, media) in &platform_spec.version.media {
                validate_media_locale_paths(
                    &context,
                    &mut summary,
                    &format!("app.{app_key}.platform.{platform}.media.{locale}"),
                    locale,
                    media,
                    ScreenshotRenderContext::Version(
                        platform_spec.version.localizations.get(locale),
                    ),
                )?;
            }
        }
    }

    Ok(summary)
}

fn validate_media_locale_paths(
    context: &MediaValidationContext,
    summary: &mut MediaValidationSummary,
    subject: &str,
    locale: &str,
    media: &crate::config::AppMediaLocalizationSpec,
    render_context: ScreenshotRenderContext<'_>,
) -> Result<()> {
    for (set, source) in &media.screenshots {
        let files = context
            .resolve_screenshot_files(locale, *set, source, render_context)
            .with_context(|| {
                format!(
                    "failed to resolve {subject}.screenshots.{}",
                    set.config_key()
                )
            })?;
        validate_screenshots(*set, files.paths())?;
        summary.screenshot_sets += 1;
        summary.screenshots += files.paths().len();
    }

    for (set, paths) in &media.app_previews {
        let files = context.resolve_paths(paths).with_context(|| {
            format!(
                "failed to resolve {subject}.app_previews.{}",
                set.config_key()
            )
        })?;
        validate_previews(*set, &files)?;
        summary.preview_sets += 1;
        summary.previews += files.len();
    }
    Ok(())
}

fn validate_review_asset(
    context: &MediaValidationContext,
    summary: &mut MediaValidationSummary,
    subject: &str,
    review: Option<&CommerceReviewAssetSpec>,
) -> Result<()> {
    let Some(review) = review else {
        return Ok(());
    };
    if let Some(path) = &review.screenshot {
        let path = context.resolve_path(path);
        validate_image_file(&path)
            .with_context(|| format!("failed to validate {subject}.screenshot"))?;
        summary.extra_images += 1;
    }
    Ok(())
}

fn validate_event_media(
    context: &MediaValidationContext,
    summary: &mut MediaValidationSummary,
    subject: &str,
    media: &AppEventMediaSpec,
) -> Result<()> {
    for (field, path) in [
        ("card_image", media.card_image.as_ref()),
        ("details_image", media.details_image.as_ref()),
    ] {
        if let Some(path) = path {
            let path = context.resolve_path(path);
            validate_event_image(&path)
                .with_context(|| format!("failed to validate {subject}.{field}"))?;
            summary.extra_images += 1;
        }
    }
    for (field, path) in [
        ("card_video", media.card_video.as_ref()),
        ("details_video", media.details_video.as_ref()),
    ] {
        if let Some(path) = path {
            let path = context.resolve_path(path);
            validate_event_video(&path)
                .with_context(|| format!("failed to validate {subject}.{field}"))?;
            summary.extra_videos += 1;
        }
    }
    Ok(())
}

pub fn validate_screenshots(set: MediaScreenshotSet, files: &[PathBuf]) -> Result<()> {
    ensure!(
        (1..=10).contains(&files.len()),
        "screenshot set {} must contain 1 to 10 files",
        set.config_key()
    );

    let sizes = screenshot_sizes(set);
    for file in files {
        let extension = lower_extension(file)?;
        ensure!(
            matches!(extension.as_str(), "png" | "jpg" | "jpeg"),
            "screenshot {} must be .png, .jpg, or .jpeg",
            file.display()
        );
        let dimensions = image::open(file)
            .with_context(|| format!("failed to inspect image {}", file.display()))?
            .dimensions();
        ensure!(
            sizes.contains(&dimensions),
            "screenshot {} is {}x{}, expected one of {} for {}",
            file.display(),
            dimensions.0,
            dimensions.1,
            format_sizes(sizes),
            set.config_key()
        );
    }
    Ok(())
}

pub fn validate_previews(set: MediaPreviewSet, files: &[PathBuf]) -> Result<()> {
    ensure!(
        (1..=3).contains(&files.len()),
        "app preview set {} must contain 1 to 3 files",
        set.config_key()
    );

    let sizes = preview_sizes(set);
    ensure!(
        !sizes.is_empty(),
        "app previews for {} are not supported by current Apple specifications",
        set.config_key()
    );

    for file in files {
        let extension = lower_extension(file)?;
        ensure!(
            matches!(extension.as_str(), "mov" | "m4v" | "mp4"),
            "app preview {} must be .mov, .m4v, or .mp4",
            file.display()
        );
        let size = fs::metadata(file)
            .with_context(|| format!("failed to stat {}", file.display()))?
            .len();
        ensure!(
            size <= MAX_PREVIEW_SIZE_BYTES,
            "app preview {} exceeds 500 MB",
            file.display()
        );

        let probe = probe_video(file)?;
        ensure!(
            sizes.contains(&(probe.width, probe.height)),
            "app preview {} is {}x{}, expected one of {} for {}",
            file.display(),
            probe.width,
            probe.height,
            format_sizes(sizes),
            set.config_key()
        );
        ensure!(
            !set.requires_landscape() || probe.width > probe.height,
            "app preview {} for {} must be landscape",
            file.display(),
            set.config_key()
        );
        ensure!(
            (MIN_PREVIEW_SECONDS..=MAX_PREVIEW_SECONDS).contains(&probe.duration_seconds),
            "app preview {} duration is {:.2}s, expected 15-30s",
            file.display(),
            probe.duration_seconds
        );
        ensure!(
            probe.frame_rate <= MAX_PREVIEW_FPS + 0.01,
            "app preview {} frame rate is {:.2}fps, expected at most 30fps",
            file.display(),
            probe.frame_rate
        );
        ensure!(
            probe.is_progressive(),
            "app preview {} must be progressive, got field_order={:?}",
            file.display(),
            probe.field_order
        );
        validate_preview_codec(file, extension.as_str(), &probe)?;
    }
    Ok(())
}

pub fn lower_extension(path: &Path) -> Result<String> {
    Ok(path
        .extension()
        .and_then(|extension| extension.to_str())
        .ok_or_else(|| anyhow::anyhow!("path {} has no extension", path.display()))?
        .to_ascii_lowercase())
}

pub fn validate_image_file(path: &Path) -> Result<(u32, u32)> {
    let extension = lower_extension(path)?;
    ensure!(
        matches!(extension.as_str(), "png" | "jpg" | "jpeg"),
        "image {} must be .png, .jpg, or .jpeg",
        path.display()
    );
    let dimensions = image::open(path)
        .with_context(|| format!("failed to inspect image {}", path.display()))?
        .dimensions();
    Ok(dimensions)
}

fn validate_event_image(path: &Path) -> Result<()> {
    let dimensions = validate_image_file(path)?;
    let sizes = &[(1920, 1080), (3840, 2160)];
    ensure!(
        sizes.contains(&dimensions),
        "app event image {} is {}x{}, expected one of {}",
        path.display(),
        dimensions.0,
        dimensions.1,
        format_sizes(sizes)
    );
    Ok(())
}

fn validate_event_video(path: &Path) -> Result<()> {
    let extension = lower_extension(path)?;
    ensure!(
        matches!(extension.as_str(), "mov" | "m4v" | "mp4"),
        "app event video {} must be .mov, .m4v, or .mp4",
        path.display()
    );
    let probe = probe_video(path)?;
    ensure!(
        matches!((probe.width, probe.height), (1920, 1080) | (3840, 2160)),
        "app event video {} is {}x{}, expected 1920x1080 or 3840x2160",
        path.display(),
        probe.width,
        probe.height
    );
    ensure!(
        probe.frame_rate <= 60.0 + 0.01,
        "app event video {} frame rate is {:.2}fps, expected at most 60fps",
        path.display(),
        probe.frame_rate
    );
    ensure!(
        probe.is_progressive(),
        "app event video {} must be progressive, got field_order={:?}",
        path.display(),
        probe.field_order
    );
    validate_preview_codec(path, extension.as_str(), &probe)
}

fn validate_preview_codec(path: &Path, extension: &str, probe: &VideoProbe) -> Result<()> {
    match probe.codec_name.as_deref() {
        Some("h264") => Ok(()),
        Some("prores") => {
            ensure!(
                extension == "mov",
                "ProRes app preview {} must use .mov",
                path.display()
            );
            let profile = probe
                .profile
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            ensure!(
                profile.contains("hq"),
                "ProRes app preview {} must be ProRes 422 HQ, got profile {:?}",
                path.display(),
                probe.profile
            );
            Ok(())
        }
        Some(codec) => bail!(
            "app preview {} codec is {codec}, expected H.264 or ProRes 422 HQ",
            path.display()
        ),
        None => bail!("app preview {} has no video codec", path.display()),
    }
}

fn probe_video(path: &Path) -> Result<VideoProbe> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,profile,width,height,r_frame_rate,avg_frame_rate,field_order",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .with_context(|| {
            format!(
                "failed to run ffprobe for {}; install ffmpeg to validate app previews",
                path.display()
            )
        })?;

    ensure!(
        output.status.success(),
        "ffprobe failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let raw: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("failed to parse ffprobe JSON for {}", path.display()))?;
    let stream = raw
        .streams
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("app preview {} has no video stream", path.display()))?;
    let duration_seconds = raw
        .format
        .and_then(|format| format.duration)
        .and_then(|duration| duration.parse::<f64>().ok())
        .ok_or_else(|| anyhow::anyhow!("app preview {} has no duration", path.display()))?;

    Ok(VideoProbe {
        codec_name: stream.codec_name,
        profile: stream.profile,
        width: stream.width.unwrap_or(0),
        height: stream.height.unwrap_or(0),
        frame_rate: parse_frame_rate(
            stream
                .avg_frame_rate
                .as_deref()
                .or(stream.r_frame_rate.as_deref())
                .unwrap_or("0/0"),
        ),
        field_order: stream.field_order,
        duration_seconds,
    })
}

fn parse_frame_rate(value: &str) -> f64 {
    if let Some((numerator, denominator)) = value.split_once('/') {
        let numerator = numerator.parse::<f64>().unwrap_or(0.0);
        let denominator = denominator.parse::<f64>().unwrap_or(0.0);
        if denominator > 0.0 {
            return numerator / denominator;
        }
        return 0.0;
    }
    value.parse::<f64>().unwrap_or(0.0)
}

fn screenshot_sizes(set: MediaScreenshotSet) -> &'static [(u32, u32)] {
    match set {
        MediaScreenshotSet::Iphone | MediaScreenshotSet::Iphone67 => &[
            (1260, 2736),
            (2736, 1260),
            (1290, 2796),
            (2796, 1290),
            (1320, 2868),
            (2868, 1320),
        ],
        MediaScreenshotSet::Iphone65 => &[(1284, 2778), (2778, 1284), (1242, 2688), (2688, 1242)],
        MediaScreenshotSet::Iphone61 => &[
            (1170, 2532),
            (2532, 1170),
            (1125, 2436),
            (2436, 1125),
            (1080, 2340),
            (2340, 1080),
        ],
        MediaScreenshotSet::Iphone58 => &[(1125, 2436), (2436, 1125)],
        MediaScreenshotSet::Iphone55 => &[(1242, 2208), (2208, 1242)],
        MediaScreenshotSet::Iphone47 => &[(750, 1334), (1334, 750)],
        MediaScreenshotSet::Iphone40 => &[(640, 1096), (640, 1136), (1136, 600), (1136, 640)],
        MediaScreenshotSet::Iphone35 => &[(640, 920), (640, 960), (960, 600), (960, 640)],
        MediaScreenshotSet::Ipad | MediaScreenshotSet::Ipad13 => {
            &[(2064, 2752), (2752, 2064), (2048, 2732), (2732, 2048)]
        }
        MediaScreenshotSet::Ipad129 => &[(2048, 2732), (2732, 2048)],
        MediaScreenshotSet::Ipad11 => &[
            (1488, 2266),
            (2266, 1488),
            (1668, 2420),
            (2420, 1668),
            (1668, 2388),
            (2388, 1668),
            (1640, 2360),
            (2360, 1640),
        ],
        MediaScreenshotSet::Ipad105 => &[(1668, 2224), (2224, 1668)],
        MediaScreenshotSet::Ipad97 => &[
            (1536, 2008),
            (1536, 2048),
            (2048, 1496),
            (2048, 1536),
            (768, 1004),
            (768, 1024),
            (1024, 748),
            (1024, 768),
        ],
        MediaScreenshotSet::Mac => &[(1280, 800), (1440, 900), (2560, 1600), (2880, 1800)],
        MediaScreenshotSet::AppleTv => &[(1920, 1080), (3840, 2160)],
        MediaScreenshotSet::VisionPro => &[(3840, 2160)],
        MediaScreenshotSet::Watch => &[(416, 496)],
        MediaScreenshotSet::WatchUltra => &[(422, 514), (410, 502)],
        MediaScreenshotSet::WatchSeries10 => &[(416, 496)],
        MediaScreenshotSet::WatchSeries7 => &[(396, 484)],
        MediaScreenshotSet::WatchSeries4 => &[(368, 448)],
        MediaScreenshotSet::WatchSeries3 => &[(312, 390)],
    }
}

fn preview_sizes(set: MediaPreviewSet) -> &'static [(u32, u32)] {
    match set {
        MediaPreviewSet::Iphone
        | MediaPreviewSet::IphonePortrait
        | MediaPreviewSet::IphoneLandscape
        | MediaPreviewSet::Iphone67
        | MediaPreviewSet::Iphone65
        | MediaPreviewSet::Iphone61
        | MediaPreviewSet::Iphone58 => &[(886, 1920), (1920, 886)],
        MediaPreviewSet::Iphone55 | MediaPreviewSet::Iphone40 => &[(1080, 1920), (1920, 1080)],
        MediaPreviewSet::Iphone47 => &[(750, 1334), (1334, 750)],
        MediaPreviewSet::Iphone35 => &[],
        MediaPreviewSet::Ipad
        | MediaPreviewSet::IpadPortrait
        | MediaPreviewSet::IpadLandscape
        | MediaPreviewSet::Ipad13
        | MediaPreviewSet::Ipad11
        | MediaPreviewSet::Ipad105 => &[(1200, 1600), (1600, 1200)],
        MediaPreviewSet::Ipad129 => &[(1200, 1600), (1600, 1200), (900, 1200), (1200, 900)],
        MediaPreviewSet::Ipad97 => &[(900, 1200), (1200, 900)],
        MediaPreviewSet::Desktop => &[(1920, 1080)],
        MediaPreviewSet::Tv => &[(1920, 1080)],
        MediaPreviewSet::Vision => &[(3840, 2160)],
    }
}

fn format_sizes(sizes: &[(u32, u32)]) -> String {
    sizes
        .iter()
        .map(|(width, height)| format!("{width}x{height}"))
        .collect::<Vec<_>>()
        .join(", ")
}

struct MediaValidationContext {
    config_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
enum ScreenshotRenderContext<'a> {
    Version(Option<&'a AppVersionLocalizationSource>),
    CustomProductPage(&'a CustomProductPageLocalizationSource),
}

struct ResolvedMediaFiles {
    paths: Vec<PathBuf>,
    _generated: Option<media_render::GeneratedScreenshots>,
}

impl ResolvedMediaFiles {
    fn from_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            _generated: None,
        }
    }

    fn from_generated(generated: media_render::GeneratedScreenshots) -> Self {
        Self {
            paths: generated.paths().to_vec(),
            _generated: Some(generated),
        }
    }

    fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

impl MediaValidationContext {
    fn new(config_path: &Path) -> Self {
        Self {
            config_dir: config_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
        }
    }

    fn resolve_paths(&self, list: &MediaPathList) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for path in list.paths() {
            let path_string = path.to_string_lossy();
            if contains_glob_meta(&path_string) {
                let pattern = self.resolve_path(path).to_string_lossy().into_owned();
                let mut matched = glob(&pattern)
                    .with_context(|| format!("invalid glob pattern {pattern}"))?
                    .map(|entry| {
                        entry.with_context(|| format!("failed to read glob entry {pattern}"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                matched.sort();
                paths.extend(matched);
            } else {
                paths.push(self.resolve_path(path));
            }
        }
        ensure!(!paths.is_empty(), "media path list resolved to no files");

        let mut seen = BTreeSet::new();
        for path in &paths {
            ensure!(
                seen.insert(path.clone()),
                "media path {} is listed more than once",
                path.display()
            );
            ensure!(
                path.exists(),
                "media file {} does not exist",
                path.display()
            );
            ensure!(
                path.is_file(),
                "media path {} is not a file",
                path.display()
            );
        }
        Ok(paths)
    }

    fn resolve_screenshot_files(
        &self,
        locale: &str,
        set: MediaScreenshotSet,
        source: &MediaScreenshotSource,
        render_context: ScreenshotRenderContext<'_>,
    ) -> Result<ResolvedMediaFiles> {
        match source {
            MediaScreenshotSource::Paths(paths) => {
                Ok(ResolvedMediaFiles::from_paths(self.resolve_paths(paths)?))
            }
            MediaScreenshotSource::Render(render) => {
                let generated = match render_context {
                    ScreenshotRenderContext::Version(version_localization) => {
                        media_render::render_config_screenshots_to_temp(
                            &self.config_dir,
                            locale,
                            version_localization,
                            set,
                            &render.render,
                        )?
                    }
                    ScreenshotRenderContext::CustomProductPage(localization) => {
                        media_render::render_custom_product_page_config_screenshots_to_temp(
                            &self.config_dir,
                            locale,
                            localization,
                            set,
                            &render.render,
                        )?
                    }
                };
                Ok(ResolvedMediaFiles::from_generated(generated))
            }
        }
    }

    fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.config_dir.join(path)
        }
    }
}

fn contains_glob_meta(value: &str) -> bool {
    value.contains('*') || value.contains('?') || value.contains('[')
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    #[serde(default)]
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    r_frame_rate: Option<String>,
    #[serde(default)]
    avg_frame_rate: Option<String>,
    #[serde(default)]
    field_order: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    #[serde(default)]
    duration: Option<String>,
}

#[derive(Debug)]
struct VideoProbe {
    codec_name: Option<String>,
    profile: Option<String>,
    width: u32,
    height: u32,
    frame_rate: f64,
    field_order: Option<String>,
    duration_seconds: f64,
}

impl VideoProbe {
    fn is_progressive(&self) -> bool {
        matches!(
            self.field_order.as_deref(),
            None | Some("unknown") | Some("progressive")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_frame_rate, preview_sizes, screenshot_sizes};
    use crate::config::{MediaPreviewSet, MediaScreenshotSet};

    #[test]
    fn screenshot_specs_include_current_primary_devices() {
        assert!(screenshot_sizes(MediaScreenshotSet::Iphone).contains(&(1320, 2868)));
        assert!(screenshot_sizes(MediaScreenshotSet::Ipad).contains(&(2064, 2752)));
        assert!(screenshot_sizes(MediaScreenshotSet::Mac).contains(&(2880, 1800)));
        assert!(screenshot_sizes(MediaScreenshotSet::VisionPro).contains(&(3840, 2160)));
    }

    #[test]
    fn preview_specs_include_current_primary_devices() {
        assert!(preview_sizes(MediaPreviewSet::Iphone).contains(&(886, 1920)));
        assert!(preview_sizes(MediaPreviewSet::Ipad).contains(&(1200, 1600)));
        assert!(preview_sizes(MediaPreviewSet::Desktop).contains(&(1920, 1080)));
        assert!(preview_sizes(MediaPreviewSet::Vision).contains(&(3840, 2160)));
    }

    #[test]
    fn parses_fractional_frame_rates() {
        assert!((parse_frame_rate("30000/1001") - 29.970).abs() < 0.01);
        assert_eq!(parse_frame_rate("30/1"), 30.0);
        assert_eq!(parse_frame_rate("0/0"), 0.0);
    }
}
