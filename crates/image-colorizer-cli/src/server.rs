use crate::config::{AppError, ServeConfig};

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Multipart, Path as AxumPath, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, Response, StatusCode};
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use image::codecs::png::PngEncoder;
use image::{ColorType, DynamicImage, ImageEncoder};
use image_colorizer_core::utils::{hex_to_rgb, interpolate_color};
use image_colorizer_core::{ColorizerConfig, GpuColorizer, RenderedImage};
use palette::{color_difference::ImprovedCiede2000, FromColor, Lab};
use serde_derive::{Deserialize, Serialize};
use tokio::sync::Mutex;

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
        .route("/colorize", post(colorize_upload))
        .route("/colorschemes", get(list_colorschemes))
        .route("/colorschemes/:name", get(fetch_colorscheme))
        .route("/save-config", post(save_config))
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

    if let Some(image) = request.image {
        session.image = Some(Arc::new(image));
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
    let colorscheme_path = state
        .defaults
        .config_dir
        .join(format!("{}.txt", colorscheme_name));
    let config_path = state.defaults.config_dir.join("config.toml");

    tokio::fs::create_dir_all(&state.defaults.config_dir)
        .await
        .map_err(server_error)?;
    tokio::fs::write(&colorscheme_path, request.colorscheme_text.as_bytes())
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
            "Saved {} and {}",
            config_path.display(),
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
        .get("https://api.github.com/repos/TaylorBeeston/image-colorizer/contents/colorschemes?ref=main")
        .header("User-Agent", "image-colorizer")
        .send()
        .await
        .map_err(server_error)?;

    if !response.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("GitHub colorscheme list returned {}", response.status()),
        ));
    }

    let contents = response
        .json::<Vec<GithubContent>>()
        .await
        .map_err(server_error)?;
    let mut schemes = Vec::new();

    for item in contents {
        let Some(name) = item.name.strip_suffix(".txt") else {
            continue;
        };
        let Some(url) = item.download_url else {
            continue;
        };
        let Ok(text) = download_url(&client, &url).await else {
            continue;
        };
        let Ok(colors) = parse_colorscheme_hex(&text) else {
            continue;
        };

        schemes.push(ColorschemeSummary {
            name: name.to_string(),
            colors,
            source: ColorschemeSource::Remote,
        });
    }

    Ok(schemes)
}

async fn download_remote_colorscheme(name: &str) -> Result<String, (StatusCode, String)> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://raw.githubusercontent.com/TaylorBeeston/image-colorizer/main/colorschemes/{}.txt",
        name.to_lowercase()
    );
    let text = download_url(&client, &url).await?;

    parse_colorscheme_hex(&text)?;

    Ok(text)
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
        .map(|hex| {
            hex_to_rgb(hex).map(|_| hex.to_string()).map_err(|err| {
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
    let html = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Image Colorizer Workstation</title>
  <style>
    :root {
      color-scheme: dark;
      --bg: #111118;
      --panel: color-mix(in srgb, #1f1f28 90%, #7e9cd8 10%);
      --panel-2: #181820;
      --line: #363646;
      --text: #dcd7ba;
      --muted: #c8c093;
      --quiet: #938aa9;
      --blue: #7e9cd8;
      --green: #98bb6c;
      --red: #e46876;
      --orange: #ffa066;
      --shadow: 0 28px 90px #0009;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }

    * { box-sizing: border-box; }

    body {
      min-height: 100vh;
      margin: 0;
      background:
        radial-gradient(circle at 20% 0%, #2d4f6755, transparent 34rem),
        radial-gradient(circle at 100% 10%, #957fb855, transparent 30rem),
        var(--bg);
      color: var(--text);
    }

    button, input, textarea { font: inherit; }

    button, a.download {
      border: 0;
      border-radius: 999px;
      padding: 0.72rem 1rem;
      background: var(--blue);
      color: #111118;
      font-weight: 800;
      cursor: pointer;
      text-decoration: none;
      box-shadow: 0 8px 24px #0005;
    }

    button.secondary { background: var(--green); }
    button.ghost { background: #2a2a37; color: var(--text); }
    button.danger { background: var(--red); }
    button:disabled { opacity: 0.55; cursor: wait; }

    .app {
      width: min(1720px, calc(100vw - 28px));
      margin: 14px auto;
      display: grid;
      grid-template-columns: minmax(340px, 430px) minmax(0, 1fr);
      gap: 14px;
    }

    .panel {
      border: 1px solid var(--line);
      border-radius: 28px;
      background: color-mix(in srgb, var(--panel) 94%, transparent);
      box-shadow: var(--shadow);
      backdrop-filter: blur(18px);
    }

    aside.panel {
      position: sticky;
      top: 14px;
      height: calc(100vh - 28px);
      overflow: hidden;
      padding: 16px;
    }

    main.panel {
      min-height: calc(100vh - 28px);
      padding: 22px;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr);
      gap: 16px;
    }

    h1 { margin: 0; font-size: clamp(1.65rem, 3vw, 2.5rem); letter-spacing: -0.06em; }
    h2 { margin: 0 0 0.75rem; font-size: 1rem; color: var(--text); }
    p { color: var(--muted); line-height: 1.4; }
    .lede { margin: 0.15rem 0 0.55rem; font-size: 0.88rem; }

    form { display: grid; gap: 9px; }
    fieldset { display: grid; gap: 8px; border: 1px solid var(--line); border-radius: 18px; padding: 10px; }
    legend { padding: 0 0.35rem; color: var(--quiet); font-size: 0.78rem; text-transform: uppercase; letter-spacing: 0.14em; }

    label.control { display: grid; gap: 4px; }
    .control-head { display: flex; justify-content: space-between; gap: 10px; align-items: baseline; }
    .control-title { font-weight: 800; }
    .value { color: var(--blue); font-variant-numeric: tabular-nums; font-weight: 800; }
    .hint { margin: 0; color: var(--quiet); font-size: 0.75rem; line-height: 1.25; }

    input[type=file], input[type=text], textarea {
      width: 100%;
      color: var(--text);
      background: var(--panel-2);
      border: 1px solid #54546d;
      border-radius: 14px;
      padding: 0.55rem;
    }

    input[type=file] { border-style: dashed; }
    textarea { min-height: 120px; resize: vertical; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 0.88rem; line-height: 1.45; }
    input[type=range] { width: 100%; accent-color: var(--blue); margin: 0; }

    .row { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }
    .toggle { display: flex; gap: 0.5rem; align-items: center; color: var(--muted); }

    .swatches {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(22px, 1fr));
      gap: 6px;
      max-height: 76px;
      overflow: auto;
    }

    .swatch {
      aspect-ratio: 1;
      border: 1px solid #ffffff33;
      border-radius: 999px;
      cursor: pointer;
      box-shadow: inset 0 0 0 2px #0005;
    }

    details {
      border: 1px solid #363646;
      border-radius: 14px;
      padding: 8px;
      background: #18182099;
    }

    summary {
      cursor: pointer;
      color: var(--text);
      font-weight: 800;
    }

    .scheme-browser {
      display: grid;
      gap: 7px;
      max-height: 220px;
      overflow: auto;
      padding-top: 8px;
    }

    .scheme-card {
      display: grid;
      gap: 7px;
      padding: 10px;
      border: 1px solid #363646;
      border-radius: 14px;
      background: #111118;
      color: var(--text);
      text-align: left;
      cursor: pointer;
    }

    .scheme-card strong { text-transform: capitalize; }

    .scheme-strip {
      display: flex;
      height: 16px;
      overflow: hidden;
      border-radius: 999px;
      border: 1px solid #ffffff22;
    }

    .scheme-strip span { flex: 1; }

    .workspace {
      min-height: 0;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr) auto;
      gap: 14px;
    }

    .toolbar { display: flex; justify-content: space-between; gap: 12px; flex-wrap: wrap; align-items: center; }
    .status { margin: 0; min-height: 1.5em; color: var(--muted); }
    .status.error { color: var(--red); white-space: pre-wrap; }

    .compare {
      position: relative;
      min-height: 520px;
      border: 1px solid var(--line);
      border-radius: 24px;
      overflow: hidden;
      background:
        linear-gradient(45deg, #0d0c0c 25%, transparent 25%),
        linear-gradient(-45deg, #0d0c0c 25%, transparent 25%),
        linear-gradient(45deg, transparent 75%, #0d0c0c 75%),
        linear-gradient(-45deg, transparent 75%, #0d0c0c 75%);
      background-size: 28px 28px;
      background-position: 0 0, 0 14px, 14px -14px, -14px 0;
      display: grid;
      place-items: center;
    }

    .compare.empty::before {
      content: "Drop in an image to begin";
      color: var(--quiet);
      font-weight: 800;
      letter-spacing: 0.02em;
    }

    .compare img {
      width: 100%;
      height: 100%;
      object-fit: contain;
      user-select: none;
      pointer-events: none;
    }

    .image-layer {
      position: absolute;
      inset: 0;
      display: grid;
      place-items: center;
    }

    .after-layer { clip-path: inset(0 0 0 50%); }

    .divider {
      position: absolute;
      top: 0;
      bottom: 0;
      left: 50%;
      width: 3px;
      transform: translateX(-50%);
      background: var(--blue);
      box-shadow: 0 0 0 1px #111118, 0 0 28px var(--blue);
      cursor: ew-resize;
    }

    .divider::after {
      content: "↔";
      position: absolute;
      top: 50%;
      left: 50%;
      transform: translate(-50%, -50%);
      width: 44px;
      height: 44px;
      border-radius: 999px;
      display: grid;
      place-items: center;
      background: var(--blue);
      color: #111118;
      font-weight: 900;
    }

    .badge {
      position: absolute;
      top: 14px;
      padding: 0.38rem 0.65rem;
      border-radius: 999px;
      background: #111118cc;
      color: var(--text);
      font-weight: 800;
      font-size: 0.78rem;
      letter-spacing: 0.08em;
      text-transform: uppercase;
    }

    .badge.before { left: 14px; }
    .badge.after { right: 14px; }

    .loupe {
      position: fixed;
      width: 190px;
      height: 190px;
      border: 2px solid var(--blue);
      border-radius: 999px;
      box-shadow: 0 20px 50px #000a, inset 0 0 0 1px #fff4;
      background-repeat: no-repeat;
      background-color: #111118;
      pointer-events: none;
      z-index: 20;
      display: none;
    }

    .loupe.on { display: block; }

    .small { font-size: 0.85rem; color: var(--quiet); }

    @media (max-width: 980px) {
      .app { grid-template-columns: 1fr; }
      aside.panel { position: static; height: auto; }
      .compare { min-height: 420px; }
    }
  </style>
</head>
<body>
  <div class="app">
    <aside class="panel">
      <h1>Image Colorizer</h1>
      <p class="lede">A local workspace for palette-driven image colorization. Upload, tune, compare, and save the look.</p>

      <form id="form">
        <fieldset>
          <legend>Source</legend>
          <input id="file" name="image" type="file" accept="image/*">
          <p class="hint">Processed locally by this app.</p>
        </fieldset>

        <fieldset>
          <legend>Parameters</legend>

          <label class="control">
            <span class="control-head"><span class="control-title">Blend</span><span class="value" id="blendValue"></span></span>
            <input id="blend" name="blend_factor" type="range" min="0" max="1" step="0.01" value="__BLEND__">
          </label>

          <label class="control">
            <span class="control-head"><span class="control-title">Dither</span><span class="value" id="ditherValue"></span></span>
            <input id="dither" name="dither_amount" type="range" min="0" max="1" step="0.01" value="__DITHER__">
          </label>

          <label class="control">
            <span class="control-head"><span class="control-title">Spatial radius</span><span class="value" id="radiusValue"></span></span>
            <input id="radius" name="spatial_averaging_radius" type="range" min="0" max="100" step="1" value="__RADIUS__">
          </label>

          <label class="control">
            <span class="control-head"><span class="control-title">Palette detail</span><span class="value" id="thresholdValue"></span></span>
            <input id="threshold" name="interpolation_threshold" type="range" min="0.1" max="100" step="0.1" value="__THRESHOLD__">
          </label>

          <label class="toggle"><input id="interpolate" name="interpolate_colors" type="checkbox" value="true" __INTERPOLATE_CHECKED__> Smooth palette ramps</label>

          <details>
            <summary>What do these controls do?</summary>
            <p class="hint"><strong>Blend</strong> controls how much of the colorized result replaces the original.</p>
            <p class="hint"><strong>Dither</strong> adds subtle variation to reduce flat bands.</p>
            <p class="hint"><strong>Spatial radius</strong> smooths chroma using nearby pixels.</p>
            <p class="hint"><strong>Palette detail</strong> controls how many in-between palette colors are inserted.</p>
          </details>
        </fieldset>

        <fieldset>
          <legend>Colorscheme</legend>
          <label class="control">
            <span class="control-title">Name</span>
            <input id="schemeName" name="colorscheme_name" type="text" value="__COLORSCHEME__">
          </label>
          <details>
            <summary>Browse available colorschemes</summary>
            <div class="scheme-browser" id="schemeBrowser">
              <p class="hint">Loading colorschemes…</p>
            </div>
          </details>
          <div class="swatches" id="swatches"></div>
          <details>
            <summary>Edit colors</summary>
            <div class="row">
              <input id="newColor" type="text" placeholder="#7e9cd8" aria-label="New color">
              <button id="addColor" class="ghost" type="button">Add color</button>
              <button id="sortColors" class="ghost" type="button">Sort</button>
            </div>
            <label class="control">
              <span class="control-title">Colors</span>
              <textarea id="schemeText" name="colorscheme_text" spellcheck="false">__COLORSCHEME_TEXT__</textarea>
              <p class="hint">One hex color per line. Comments with <code>//</code> are ignored. Click a swatch to remove it.</p>
            </label>
          </details>
        </fieldset>

        <div class="row">
          <button id="render" type="submit">Render now</button>
          <button id="saveConfig" class="secondary" type="button">Save config</button>
        </div>
      </form>
    </aside>

    <main class="panel workspace">
      <div class="toolbar">
        <p id="status" class="status">Ready.</p>
        <div class="row">
          <button id="toggleLoupe" class="ghost" type="button">Loupe: off</button>
          <a id="download" class="download" hidden>Download result</a>
        </div>
      </div>

      <section id="compare" class="compare empty">
        <div class="image-layer before-layer"><img id="inputPreview" alt="Original image preview"></div>
        <div class="image-layer after-layer" id="afterLayer"><img id="outputPreview" alt="Colorized image preview"></div>
        <span class="badge before">Original</span>
        <span class="badge after">Colorized</span>
        <div id="divider" class="divider" role="slider" aria-label="Comparison split" aria-valuemin="0" aria-valuemax="100" aria-valuenow="50"></div>
      </section>

      <p class="small">Drag the center handle to A/B the image. Turn on the loupe and move over the preview to inspect pixels.</p>
    </main>
  </div>

  <div id="loupe" class="loupe"></div>

  <script>
    const form = document.querySelector('#form');
    const file = document.querySelector('#file');
    const compare = document.querySelector('#compare');
    const afterLayer = document.querySelector('#afterLayer');
    const schemeBrowser = document.querySelector('#schemeBrowser');
    const divider = document.querySelector('#divider');
    const loupe = document.querySelector('#loupe');
    const toggleLoupe = document.querySelector('#toggleLoupe');
    const status = document.querySelector('#status');
    const inputPreview = document.querySelector('#inputPreview');
    const outputPreview = document.querySelector('#outputPreview');
    const download = document.querySelector('#download');
    const render = document.querySelector('#render');
    const saveConfig = document.querySelector('#saveConfig');
    const schemeText = document.querySelector('#schemeText');
    const schemeName = document.querySelector('#schemeName');
    const swatches = document.querySelector('#swatches');
    const newColor = document.querySelector('#newColor');
    const controls = ['blend', 'dither', 'radius', 'threshold'];
    let currentSplit = 50;
    let inputUrl;
    let outputUrl;
    let requestId = 0;
    let debounceTimer;
    let loupeEnabled = false;

    function syncValues() {
      blendValue.textContent = blend.value;
      ditherValue.textContent = dither.value;
      radiusValue.textContent = radius.value;
      thresholdValue.textContent = threshold.value;
      renderSwatches();
    }

    function colors() {
      return schemeText.value.split('\n')
        .map(line => line.split('//')[0].trim())
        .filter(line => /^#[0-9a-fA-F]{6}$/.test(line) || /^#[0-9a-fA-F]{3}$/.test(line));
    }

    function renderSwatches() {
      swatches.innerHTML = '';
      for (const color of colors()) {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'swatch';
        button.style.background = color;
        button.title = `${color} — click to remove`;
        button.addEventListener('click', () => {
          schemeText.value = schemeText.value.split('\n').filter(line => line.trim() !== color).join('\n');
          schedulePreview();
        });
        swatches.append(button);
      }
    }

    function renderSchemeStrip(colors) {
      return `<div class="scheme-strip">${colors.slice(0, 16).map(color => `<span style="background:${color}"></span>`).join('')}</div>`;
    }

    async function loadSchemeBrowser() {
      try {
        const response = await fetch('/colorschemes');
        if (!response.ok) throw new Error(await response.text());
        const schemes = await response.json();
        schemeBrowser.innerHTML = '';

        for (const scheme of schemes) {
          const card = document.createElement('button');
          card.type = 'button';
          card.className = 'scheme-card';
          card.innerHTML = `<strong>${scheme.name}</strong>${renderSchemeStrip(scheme.colors)}<span class="hint">${scheme.source}</span>`;
          card.addEventListener('click', () => loadScheme(scheme.name));
          schemeBrowser.append(card);
        }
      } catch (error) {
        schemeBrowser.innerHTML = `<p class="hint">Could not load colorscheme browser: ${error.message || error}</p>`;
      }
    }

    async function loadScheme(name) {
      status.className = 'status';
      status.textContent = `Loading ${name}…`;
      const response = await fetch(`/colorschemes/${encodeURIComponent(name)}`);
      if (!response.ok) {
        status.className = 'status error';
        status.textContent = await response.text();
        return;
      }

      const scheme = await response.json();
      schemeName.value = scheme.name;
      schemeText.value = scheme.text;
      syncValues();
      schedulePreview();
    }

    function formData(includeImage) {
      const data = new FormData();
      if (includeImage && file.files[0]) data.append('image', file.files[0]);
      data.append('blend_factor', blend.value);
      data.append('dither_amount', dither.value);
      data.append('spatial_averaging_radius', radius.value);
      data.append('interpolation_threshold', threshold.value);
      data.append('interpolate_colors', interpolate.checked ? 'true' : 'false');
      data.append('colorscheme_name', schemeName.value);
      data.append('colorscheme_text', schemeText.value);
      return data;
    }

    async function renderPreview(includeImage = false) {
      if (!file.files.length && includeImage) return;
      const id = ++requestId;
      status.className = 'status';
      status.textContent = 'Rendering…';
      render.disabled = true;
      try {
        const response = await fetch('/colorize', { method: 'POST', body: formData(includeImage) });
        if (!response.ok) throw new Error(await response.text());
        if (id !== requestId) return;
        const blob = await response.blob();
        if (outputUrl) URL.revokeObjectURL(outputUrl);
        outputUrl = URL.createObjectURL(blob);
        outputPreview.src = outputUrl;
        download.href = outputUrl;
        download.download = file.files[0]?.name?.replace(/\.[^.]*$/, '_colorized.png') || 'colorized.png';
        download.hidden = false;
        compare.classList.remove('empty');
        status.textContent = 'Rendered.';
      } catch (error) {
        if (id !== requestId) return;
        status.className = 'status error';
        status.textContent = error.message || String(error);
      } finally {
        if (id === requestId) render.disabled = false;
      }
    }

    function schedulePreview() {
      syncValues();
      clearTimeout(debounceTimer);
      debounceTimer = setTimeout(() => renderPreview(false), 120);
    }

    function setSplit(percent) {
      const clamped = Math.max(0, Math.min(100, percent));
      currentSplit = clamped;
      afterLayer.style.clipPath = `inset(0 0 0 ${clamped}%)`;
      divider.style.left = `${clamped}%`;
      divider.setAttribute('aria-valuenow', String(Math.round(clamped)));
    }

    compare.addEventListener('pointerdown', event => {
      if (compare.classList.contains('empty')) return;
      const move = event => {
        const rect = compare.getBoundingClientRect();
        setSplit(((event.clientX - rect.left) / rect.width) * 100);
      };
      move(event);
      document.addEventListener('pointermove', move);
      document.addEventListener('pointerup', () => document.removeEventListener('pointermove', move), { once: true });
    });

    compare.addEventListener('pointermove', event => {
      if (!loupeEnabled || compare.classList.contains('empty') || !outputUrl) return;
      const rect = compare.getBoundingClientRect();
      const x = event.clientX - rect.left;
      const y = event.clientY - rect.top;
      const sourceUrl = x / rect.width * 100 < currentSplit ? inputUrl : outputUrl;
      if (!sourceUrl) return;
      loupe.style.left = `${event.clientX + 18}px`;
      loupe.style.top = `${event.clientY + 18}px`;
      loupe.style.backgroundImage = `url(${sourceUrl})`;
      loupe.style.backgroundSize = `${rect.width * 5}px ${rect.height * 5}px`;
      loupe.style.backgroundPosition = `${-(x * 5 - 95)}px ${-(y * 5 - 95)}px`;
    });

    compare.addEventListener('pointerleave', () => loupe.classList.remove('on'));
    compare.addEventListener('pointerenter', () => { if (loupeEnabled) loupe.classList.add('on'); });

    toggleLoupe.addEventListener('click', () => {
      loupeEnabled = !loupeEnabled;
      toggleLoupe.textContent = `Loupe: ${loupeEnabled ? 'on' : 'off'}`;
      loupe.classList.toggle('on', loupeEnabled);
    });

    file.addEventListener('change', () => {
      if (inputUrl) URL.revokeObjectURL(inputUrl);
      if (!file.files.length) return;
      inputUrl = URL.createObjectURL(file.files[0]);
      inputPreview.src = inputUrl;
      compare.classList.remove('empty');
      renderPreview(true);
    });

    form.addEventListener('submit', event => {
      event.preventDefault();
      renderPreview(true);
    });

    for (const id of controls) document.querySelector('#' + id).addEventListener('input', schedulePreview);
    interpolate.addEventListener('change', schedulePreview);
    schemeText.addEventListener('input', schedulePreview);
    schemeName.addEventListener('input', syncValues);

    addColor.addEventListener('click', () => {
      const color = newColor.value.trim();
      if (!/^#[0-9a-fA-F]{6}$/.test(color) && !/^#[0-9a-fA-F]{3}$/.test(color)) {
        status.className = 'status error';
        status.textContent = 'Use a hex color like #7e9cd8.';
        return;
      }
      schemeText.value = `${schemeText.value.trim()}\n${color}`.trim();
      newColor.value = '';
      schedulePreview();
    });

    sortColors.addEventListener('click', () => {
      schemeText.value = colors().sort().join('\n');
      schedulePreview();
    });

    saveConfig.addEventListener('click', async () => {
      status.className = 'status';
      status.textContent = 'Saving config…';
      try {
        const response = await fetch('/save-config', { method: 'POST', body: formData(false) });
        if (!response.ok) throw new Error(await response.text());
        status.textContent = await response.text();
      } catch (error) {
        status.className = 'status error';
        status.textContent = error.message || String(error);
      }
    });

    syncValues();
    loadSchemeBrowser();
    setSplit(50);
  </script>
</body>
</html>"##;

    html.replace("__BLEND__", &defaults.blend_factor.to_string())
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
