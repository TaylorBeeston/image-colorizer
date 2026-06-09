use crate::config::{AppError, ServeConfig};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path as AxumPath, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, Response, StatusCode};
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{stream, StreamExt};
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{ColorType, DynamicImage, ImageEncoder};
use image_colorizer_core::utils::{hex_to_rgb, interpolate_color};
use image_colorizer_core::{ColorizerConfig, GpuColorizer, RenderedImage};
use palette::{color_difference::ImprovedCiede2000, FromColor, Lab};
use serde_derive::{Deserialize, Serialize};
use tokio::sync::Mutex;

const FAVICON_PNG: &[u8] = include_bytes!("favicon.png");
const WEB_PREVIEW_MAX_PIXELS: u64 = 8_000_000;
const SAMPLE_WEBP: &[u8] = include_bytes!("sample.webp");

struct AppState {
    session: Mutex<Session>,
    defaults: WebDefaults,
}

struct Session {
    colorizer: GpuColorizer,
    image: Option<Arc<DynamicImage>>,
    palette_key: String,
}

struct WebDefaults {
    config_dir: PathBuf,
    colorscheme: String,
    colorscheme_text: String,
    blend_factor: f32,
    dither_amount: f32,
    spatial_averaging_radius: u32,
    interpolate_colors: bool,
    interpolation_threshold: f32,
}

struct WebRequest {
    image: Option<DynamicImage>,
    file_name: Option<String>,
    colorscheme_name: String,
    colorscheme_text: String,
    blend_factor: f32,
    dither_amount: f32,
    spatial_averaging_radius: u32,
    interpolate_colors: bool,
    interpolation_threshold: f32,
}

#[derive(Debug, Serialize)]
struct ColorschemeSummary {
    name: String,
    colors: Vec<String>,
    source: ColorschemeSource,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum ColorschemeSource {
    Local,
    Remote,
}

#[derive(Debug, Serialize)]
struct ColorschemeDetail {
    name: String,
    text: String,
    colors: Vec<String>,
    source: ColorschemeSource,
}

#[derive(Debug, Deserialize)]
struct GithubContent {
    name: String,
    #[serde(default)]
    download_url: Option<String>,
}

pub async fn serve(config: &ServeConfig) -> Result<(), AppError> {
    let colorizer = GpuColorizer::new(&config.colorizer).await?;
    let state = Arc::new(AppState {
        session: Mutex::new(Session {
            colorizer,
            image: None,
            palette_key: palette_key(
                &config.colorscheme_text,
                config.interpolate_colors,
                config.interpolation_threshold,
            ),
        }),
        defaults: WebDefaults {
            config_dir: config.config_dir.clone(),
            colorscheme: config.colorscheme.clone(),
            colorscheme_text: config.colorscheme_text.clone(),
            blend_factor: config.colorizer.blend_factor,
            dither_amount: config.colorizer.dither_amount,
            spatial_averaging_radius: config.colorizer.spatial_averaging_radius,
            interpolate_colors: config.interpolate_colors,
            interpolation_threshold: config.interpolation_threshold,
        },
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/favicon.png", get(favicon))
        .route("/sample.webp", get(sample))
        .route("/colorize", post(colorize_upload))
        .route("/colorschemes", get(list_colorschemes))
        .route("/colorschemes/:name", get(fetch_colorscheme))
        .route("/save-colorscheme", post(save_colorscheme))
        .route("/save-config", post(save_config))
        .layer(DefaultBodyLimit::max(512 * 1024 * 1024))
        .with_state(state);

    eprintln!("Serving Image Colorizer at http://{}", config.bind);

    axum::Server::bind(&config.bind)
        .serve(app.into_make_service())
        .await
        .map_err(|err| AppError::Other(format!("Web server failed: {}", err)))
}

async fn index(State(state): State<Arc<AppState>>) -> Html<String> {
    Html(render_index(&state.defaults))
}

async fn favicon() -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("image/png"))
        .body(Body::from(FAVICON_PNG))
        .expect("favicon response is valid")
}

async fn sample() -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("image/webp"))
        .body(Body::from(SAMPLE_WEBP))
        .expect("sample response is valid")
}

async fn colorize_upload(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Response<Body>, (StatusCode, String)> {
    let request = parse_web_request(multipart, &state.defaults).await?;
    let palette_key = palette_key(
        &request.colorscheme_text,
        request.interpolate_colors,
        request.interpolation_threshold,
    );
    let colorizer_config = request.to_colorizer_config()?;
    let mut session = state.session.lock().await;

    let max_preview_pixels = WEB_PREVIEW_MAX_PIXELS.min(session.colorizer.max_colorizable_pixels());

    if let Some(image) = request.image {
        session.image = Some(Arc::new(resize_for_web_preview(image, max_preview_pixels)));
    }

    if palette_key == session.palette_key {
        session.colorizer.update_parameters(
            colorizer_config.blend_factor,
            colorizer_config.dither_amount,
            colorizer_config.spatial_averaging_radius,
        );
    } else {
        session.colorizer.update_config(&colorizer_config);
        session.palette_key = palette_key;
    }

    let image = session
        .image
        .as_ref()
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "Upload an image before previewing colorization settings".to_string(),
            )
        })?
        .clone();
    let rendered = session
        .colorizer
        .colorize(&image)
        .await
        .map_err(server_error)?;
    let png = encode_png(&rendered).map_err(server_error)?;

    session.colorizer.recycle_output_buffer(rendered.data);

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("image/png"))
        .header(
            CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!(
                "inline; filename=\"{}\"",
                output_file_name(request.file_name.as_deref())
            ))
            .map_err(server_error)?,
        )
        .body(Body::from(png))
        .map_err(server_error)
}

async fn save_config(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Response<Body>, (StatusCode, String)> {
    let request = parse_web_request(multipart, &state.defaults).await?;
    request.to_colorizer_config()?;

    let colorscheme_name = sanitize_config_name(&request.colorscheme_name);
    let config_path = state.defaults.config_dir.join("config.toml");

    tokio::fs::create_dir_all(&state.defaults.config_dir)
        .await
        .map_err(server_error)?;
    tokio::fs::write(
        &config_path,
        format!(
            "blend_factor = \"{}\"\ncolorscheme = \"{}\"\ninterpolate_colors = {}\ninterpolation_threshold = \"{}\"\ndither_amount = \"{}\"\nspatial_averaging_radius = \"{}\"\n",
            request.blend_factor,
            colorscheme_name,
            request.interpolate_colors,
            request.interpolation_threshold,
            request.dither_amount,
            request.spatial_averaging_radius,
        ),
    )
    .await
    .map_err(server_error)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(Body::from(format!(
            "Saved config to {}",
            config_path.display()
        )))
        .map_err(server_error)
}

async fn save_colorscheme(
    State(state): State<Arc<AppState>>,
    multipart: Multipart,
) -> Result<Response<Body>, (StatusCode, String)> {
    let request = parse_web_request(multipart, &state.defaults).await?;
    parse_colorscheme_hex(&request.colorscheme_text)?;

    let colorscheme_name = sanitize_config_name(&request.colorscheme_name);
    let colorscheme_path = state
        .defaults
        .config_dir
        .join(format!("{}.txt", colorscheme_name));

    tokio::fs::create_dir_all(&state.defaults.config_dir)
        .await
        .map_err(server_error)?;
    tokio::fs::write(&colorscheme_path, request.colorscheme_text.as_bytes())
        .await
        .map_err(server_error)?;

    Response::builder()
        .status(StatusCode::OK)
        .header(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )
        .body(Body::from(format!(
            "Saved colorscheme to {}",
            colorscheme_path.display()
        )))
        .map_err(server_error)
}

async fn list_colorschemes(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ColorschemeSummary>>, (StatusCode, String)> {
    let mut schemes = Vec::new();

    if let Ok(mut entries) = tokio::fs::read_dir(&state.defaults.config_dir).await {
        while let Some(entry) = entries.next_entry().await.map_err(server_error)? {
            let path = entry.path();

            if path.extension().and_then(|extension| extension.to_str()) != Some("txt") {
                continue;
            }

            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let content = tokio::fs::read_to_string(&path)
                .await
                .map_err(server_error)?;

            if let Ok(colors) = parse_colorscheme_hex(&content) {
                schemes.push(ColorschemeSummary {
                    name: name.to_string(),
                    colors,
                    source: ColorschemeSource::Local,
                });
            }
        }
    }

    for scheme in remote_colorschemes().await? {
        if schemes.iter().any(|existing| existing.name == scheme.name) {
            continue;
        }

        schemes.push(scheme);
    }

    schemes.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Json(schemes))
}

async fn fetch_colorscheme(
    AxumPath(name): AxumPath<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<ColorschemeDetail>, (StatusCode, String)> {
    let name = sanitize_config_name(&name);
    let local_path = state.defaults.config_dir.join(format!("{}.txt", name));

    if let Ok(text) = tokio::fs::read_to_string(&local_path).await {
        let colors = parse_colorscheme_hex(&text)?;

        return Ok(Json(ColorschemeDetail {
            name,
            text,
            colors,
            source: ColorschemeSource::Local,
        }));
    }

    let text = download_remote_colorscheme(&name).await?;
    let colors = parse_colorscheme_hex(&text)?;

    Ok(Json(ColorschemeDetail {
        name,
        text,
        colors,
        source: ColorschemeSource::Remote,
    }))
}

impl WebRequest {
    fn to_colorizer_config(&self) -> Result<ColorizerConfig, (StatusCode, String)> {
        let colors = parse_colorscheme(&self.colorscheme_text)?;
        let colors = if self.interpolate_colors {
            interpolate_colors(colors, self.interpolation_threshold)
        } else {
            colors
        };

        Ok(ColorizerConfig {
            blend_factor: self.blend_factor,
            colors,
            dither_amount: self.dither_amount,
            spatial_averaging_radius: self.spatial_averaging_radius,
        })
    }
}

async fn parse_web_request(
    mut multipart: Multipart,
    defaults: &WebDefaults,
) -> Result<WebRequest, (StatusCode, String)> {
    let mut image = None;
    let mut file_name = None;
    let mut colorscheme_name = defaults.colorscheme.clone();
    let mut colorscheme_text = defaults.colorscheme_text.clone();
    let mut blend_factor = defaults.blend_factor;
    let mut dither_amount = defaults.dither_amount;
    let mut spatial_averaging_radius = defaults.spatial_averaging_radius;
    let mut interpolate_colors = defaults.interpolate_colors;
    let mut interpolation_threshold = defaults.interpolation_threshold;

    while let Some(field) = multipart.next_field().await.map_err(bad_request)? {
        let name = field.name().unwrap_or("").to_string();
        let field_file_name = field.file_name().map(ToOwned::to_owned);
        let bytes = field.bytes().await.map_err(bad_request)?;

        match name.as_str() {
            "image" if !bytes.is_empty() => {
                file_name = field_file_name;
                image = Some(image::load_from_memory(&bytes).map_err(bad_request)?);
            }
            "colorscheme_name" => colorscheme_name = field_text(bytes)?,
            "colorscheme_text" => colorscheme_text = field_text(bytes)?,
            "blend_factor" => {
                blend_factor = parse_f32("blend_factor", &field_text(bytes)?, 0.0, 1.0)?
            }
            "dither_amount" => {
                dither_amount = parse_f32("dither_amount", &field_text(bytes)?, 0.0, 1.0)?
            }
            "spatial_averaging_radius" => {
                spatial_averaging_radius =
                    parse_u32("spatial_averaging_radius", &field_text(bytes)?, 0, 100)?
            }
            "interpolate_colors" => interpolate_colors = field_text(bytes)? == "true",
            "interpolation_threshold" => {
                interpolation_threshold = parse_f32(
                    "interpolation_threshold",
                    &field_text(bytes)?,
                    f32::EPSILON,
                    100.0,
                )?
            }
            _ => {}
        }
    }

    Ok(WebRequest {
        image,
        file_name,
        colorscheme_name,
        colorscheme_text,
        blend_factor,
        dither_amount,
        spatial_averaging_radius,
        interpolate_colors,
        interpolation_threshold,
    })
}

async fn remote_colorschemes() -> Result<Vec<ColorschemeSummary>, (StatusCode, String)> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://api.github.com/repos/tinted-theming/schemes/contents/base16")
        .header("User-Agent", "image-colorizer")
        .send()
        .await
        .map_err(server_error)?;

    if !response.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("Tinted colorscheme list returned {}", response.status()),
        ));
    }

    let contents = response
        .json::<Vec<GithubContent>>()
        .await
        .map_err(server_error)?;
    let files = contents.into_iter().filter_map(|item| {
        let name = item
            .name
            .strip_suffix(".yaml")
            .or_else(|| item.name.strip_suffix(".yml"))?
            .to_string();
        let url = item.download_url?;

        Some((name, url))
    });
    let mut downloads = stream::iter(files)
        .map(|(name, url)| {
            let client = &client;

            async move {
                let text = download_url(client, &url).await.ok()?;
                let colors = parse_base16_yaml_hex(&text).ok()?;

                Some(ColorschemeSummary {
                    name,
                    colors,
                    source: ColorschemeSource::Remote,
                })
            }
        })
        .buffer_unordered(24);
    let mut schemes = Vec::new();

    while let Some(scheme) = downloads.next().await {
        let Some(scheme) = scheme else {
            continue;
        };

        schemes.push(scheme);
    }

    Ok(schemes)
}

async fn download_remote_colorscheme(name: &str) -> Result<String, (StatusCode, String)> {
    let client = reqwest::Client::new();

    for extension in ["yaml", "yml"] {
        let url = format!(
            "https://raw.githubusercontent.com/tinted-theming/schemes/spec-0.11/base16/{}.{}",
            name.to_lowercase(),
            extension
        );
        let Ok(text) = download_url(&client, &url).await else {
            continue;
        };
        let Ok(colors) = parse_base16_yaml_hex(&text) else {
            continue;
        };

        return Ok(colors.join("\n"));
    }

    Err((
        StatusCode::BAD_GATEWAY,
        format!("Could not download colorscheme '{}'", name),
    ))
}

async fn download_url(client: &reqwest::Client, url: &str) -> Result<String, (StatusCode, String)> {
    let response = client
        .get(url)
        .header("User-Agent", "image-colorizer")
        .send()
        .await
        .map_err(server_error)?;

    if response.status().is_success() {
        response.text().await.map_err(server_error)
    } else {
        Err((
            StatusCode::BAD_GATEWAY,
            format!("Could not download colorscheme: {}", response.status()),
        ))
    }
}

fn parse_colorscheme_hex(content: &str) -> Result<Vec<String>, (StatusCode, String)> {
    let colors = content
        .lines()
        .filter_map(|line| {
            let trimmed = line.split("//").next().unwrap_or("").trim();

            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .map(normalize_hex_color)
        .collect::<Result<Vec<_>, _>>()?;

    if colors.is_empty() {
        Err((
            StatusCode::BAD_REQUEST,
            "Colorscheme must contain at least one color".to_string(),
        ))
    } else {
        Ok(colors)
    }
}

fn parse_base16_yaml_hex(content: &str) -> Result<Vec<String>, (StatusCode, String)> {
    let mut colors = BTreeMap::new();

    for line in content.lines() {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let key = key.trim();

        if !key.starts_with("base") {
            continue;
        }

        let value = value
            .trim()
            .split(" #")
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .trim_matches('\'');

        if value.is_empty() {
            continue;
        }

        colors.insert(key.to_string(), normalize_hex_color(value)?);
    }

    if colors.is_empty() {
        Err((
            StatusCode::BAD_REQUEST,
            "Base16 colorscheme must contain base colors".to_string(),
        ))
    } else {
        Ok(colors.into_values().collect())
    }
}

fn normalize_hex_color(hex: &str) -> Result<String, (StatusCode, String)> {
    let hex = hex.trim();
    let normalized = if hex.starts_with('#') {
        hex.to_string()
    } else {
        format!("#{}", hex)
    };

    hex_to_rgb(&normalized).map(|_| normalized).map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid color '{}': {}", hex, err),
        )
    })
}

fn parse_colorscheme(content: &str) -> Result<Vec<Lab>, (StatusCode, String)> {
    let colors = content
        .lines()
        .filter_map(|line| {
            let trimmed = line.split("//").next().unwrap_or("").trim();

            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        })
        .map(|hex| {
            hex_to_rgb(hex).map(Lab::from_color).map_err(|err| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("Invalid color '{}': {}", hex, err),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    if colors.is_empty() {
        Err((
            StatusCode::BAD_REQUEST,
            "Colorscheme must contain at least one color".to_string(),
        ))
    } else {
        Ok(colors)
    }
}

fn interpolate_colors(mut colors: Vec<Lab>, threshold: f32) -> Vec<Lab> {
    if colors.len() < 2 {
        return colors;
    }

    colors.sort_by(|a, b| a.l.total_cmp(&b.l));
    let mut interpolated = Vec::new();

    for window in colors.windows(2) {
        let color1 = &window[0];
        let color2 = &window[1];
        interpolated.push(*color1);

        let distance = color1.improved_difference(*color2);

        if distance > threshold {
            let steps = (distance / threshold).ceil() as usize;

            for i in 1..steps {
                interpolated.push(interpolate_color(color1, color2, i as f32 / steps as f32));
            }
        }
    }

    interpolated.push(*colors.last().expect("colors are not empty"));
    interpolated
}

fn resize_for_web_preview(image: DynamicImage, max_pixels: u64) -> DynamicImage {
    let pixels = image.width() as u64 * image.height() as u64;
    if pixels <= max_pixels {
        return image;
    }

    let scale = (max_pixels as f64 / pixels as f64).sqrt();
    let width = ((image.width() as f64 * scale).round() as u32).max(1);
    let height = ((image.height() as f64 * scale).round() as u32).max(1);

    image.resize(width, height, FilterType::Lanczos3)
}

fn encode_png(image: &RenderedImage) -> Result<Vec<u8>, image::ImageError> {
    let mut png = Vec::new();
    let encoder = PngEncoder::new(&mut png);

    encoder.write_image(&image.data, image.width, image.height, ColorType::Rgb8)?;

    Ok(png)
}

fn output_file_name(input: Option<&str>) -> String {
    let Some(input) = input else {
        return "colorized.png".to_string();
    };

    let Some((stem, _)) = input.rsplit_once('.') else {
        return format!("{}_colorized.png", sanitize_file_name(input));
    };

    format!("{}_colorized.png", sanitize_file_name(stem))
}

fn sanitize_file_name(input: &str) -> String {
    let mut output = String::with_capacity(input.len());

    for char in input.chars() {
        if char.is_ascii_alphanumeric() || char == '-' || char == '_' {
            output.push(char);
        }
    }

    if output.is_empty() {
        "colorized".to_string()
    } else {
        output
    }
}

fn sanitize_config_name(input: &str) -> String {
    let sanitized = sanitize_file_name(input);

    if sanitized.is_empty() || sanitized == "colorized" {
        "custom".to_string()
    } else {
        sanitized
    }
}

fn field_text(bytes: axum::body::Bytes) -> Result<String, (StatusCode, String)> {
    String::from_utf8(bytes.to_vec()).map_err(bad_request)
}

fn parse_f32(name: &str, value: &str, min: f32, max: f32) -> Result<f32, (StatusCode, String)> {
    let parsed = value.parse::<f32>().map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid {}: {}", name, err),
        )
    })?;

    if parsed.is_finite() && parsed >= min && parsed <= max {
        Ok(parsed)
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            format!("{} must be between {} and {}", name, min, max),
        ))
    }
}

fn parse_u32(name: &str, value: &str, min: u32, max: u32) -> Result<u32, (StatusCode, String)> {
    let parsed = value.parse::<u32>().map_err(|err| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid {}: {}", name, err),
        )
    })?;

    if parsed >= min && parsed <= max {
        Ok(parsed)
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            format!("{} must be between {} and {}", name, min, max),
        ))
    }
}

fn palette_key(
    colorscheme_text: &str,
    interpolate_colors: bool,
    interpolation_threshold: f32,
) -> String {
    format!(
        "{}\0{}\0{}",
        colorscheme_text, interpolate_colors, interpolation_threshold
    )
}

fn bad_request(err: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, err.to_string())
}

fn server_error(err: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn render_index(defaults: &WebDefaults) -> String {
    include_str!("server_index.html")
        .replace("__BLEND__", &defaults.blend_factor.to_string())
        .replace("__DITHER__", &defaults.dither_amount.to_string())
        .replace("__RADIUS__", &defaults.spatial_averaging_radius.to_string())
        .replace(
            "__THRESHOLD__",
            &defaults.interpolation_threshold.to_string(),
        )
        .replace(
            "__INTERPOLATE_CHECKED__",
            if defaults.interpolate_colors {
                "checked"
            } else {
                ""
            },
        )
        .replace("__COLORSCHEME__", &escape_html(&defaults.colorscheme))
        .replace(
            "__COLORSCHEME_TEXT__",
            &escape_html(&defaults.colorscheme_text),
        )
}

fn escape_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());

    for char in input.chars() {
        match char {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(char),
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base16_yaml_into_ordered_plain_colors() {
        let colors = parse_base16_yaml_hex(
            r##"
scheme: "Example"
base01: "111111"
base00: "000000"
base0A: "aaaaaa"
"##,
        )
        .unwrap();

        assert_eq!(colors, ["#000000", "#111111", "#aaaaaa"]);
    }

    #[test]
    fn normalizes_plain_colorscheme_lines() {
        let colors = parse_colorscheme_hex(
            r##"
7e9cd8 // blue
#98bb6c
"##,
        )
        .unwrap();

        assert_eq!(colors, ["#7e9cd8", "#98bb6c"]);
    }

    #[test]
    fn web_preview_resize_keeps_small_images_unchanged() {
        let image = DynamicImage::new_rgb8(100, 100);
        let resized = resize_for_web_preview(image, WEB_PREVIEW_MAX_PIXELS);

        assert_eq!((resized.width(), resized.height()), (100, 100));
    }

    #[test]
    fn web_preview_resize_caps_large_images() {
        let image = DynamicImage::new_rgb8(3840, 1920);
        let resized = resize_for_web_preview(image, 5_000_000);

        assert!(resized.width() as u64 * resized.height() as u64 <= 5_000_000);
        assert_eq!(resized.width() * 1920, resized.height() * 3840);
    }

    #[test]
    fn web_preview_resize_allows_super_ultrawide_1440p() {
        let image = DynamicImage::new_rgb8(5120, 1440);
        let resized = resize_for_web_preview(image, WEB_PREVIEW_MAX_PIXELS);

        assert_eq!((resized.width(), resized.height()), (5120, 1440));
    }
}
