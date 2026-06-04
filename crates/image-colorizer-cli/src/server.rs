use crate::config::{AppError, ServeConfig};

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Multipart, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, Response, StatusCode};
use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;
use image::codecs::png::PngEncoder;
use image::{ColorType, DynamicImage, ImageEncoder};
use image_colorizer_core::utils::{hex_to_rgb, interpolate_color};
use image_colorizer_core::{ColorizerConfig, GpuColorizer, RenderedImage};
use palette::{color_difference::ImprovedCiede2000, FromColor, Lab};
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
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Image Colorizer</title>
  <style>
    :root {{ color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; }}
    body {{ min-height: 100vh; margin: 0; display: grid; place-items: center; background: #16161d; color: #dcd7ba; }}
    main {{ width: min(1160px, calc(100vw - 32px)); padding: 32px; border: 1px solid #363646; border-radius: 24px; background: #1f1f28; box-shadow: 0 24px 80px #0008; }}
    h1 {{ margin: 0 0 8px; font-size: clamp(2rem, 6vw, 4rem); letter-spacing: -0.06em; }}
    p {{ color: #c8c093; line-height: 1.6; }}
    form {{ display: grid; gap: 18px; margin: 28px 0; }}
    fieldset {{ display: grid; gap: 12px; border: 1px solid #363646; border-radius: 18px; padding: 16px; }}
    label {{ display: grid; gap: 6px; color: #c8c093; }}
    input, textarea {{ color: #dcd7ba; background: #181820; border: 1px solid #54546d; border-radius: 12px; padding: 10px; }}
    input[type=file] {{ padding: 24px; border-style: dashed; }}
    input[type=range] {{ padding: 0; }}
    textarea {{ min-height: 180px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }}
    button, a.download {{ width: fit-content; border: 0; border-radius: 999px; padding: 12px 18px; background: #7e9cd8; color: #16161d; font-weight: 700; cursor: pointer; text-decoration: none; }}
    button.secondary {{ background: #98bb6c; }}
    button:disabled {{ opacity: 0.55; cursor: wait; }}
    .grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 18px; }}
    .preview {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 18px; margin-top: 24px; }}
    figure {{ margin: 0; }}
    figcaption {{ margin-bottom: 8px; color: #938aa9; font-size: 0.9rem; }}
    img {{ max-width: 100%; border-radius: 16px; background: #0d0c0c; }}
    .error {{ color: #e46876; white-space: pre-wrap; }}
    .value {{ color: #7e9cd8; font-variant-numeric: tabular-nums; }}
  </style>
</head>
<body>
  <main>
    <h1>Image Colorizer</h1>
    <p>Upload an image, then tune parameters live. Most changes re-render immediately using the local native GPU pipeline.</p>
    <form id="form">
      <input id="file" name="image" type="file" accept="image/*">
      <section class="grid">
        <fieldset>
          <legend>Parameters</legend>
          <label>Blend <span class="value" id="blendValue"></span><input id="blend" name="blend_factor" type="range" min="0" max="1" step="0.01" value="{blend_factor}"></label>
          <label>Dither <span class="value" id="ditherValue"></span><input id="dither" name="dither_amount" type="range" min="0" max="1" step="0.01" value="{dither_amount}"></label>
          <label>Spatial radius <span class="value" id="radiusValue"></span><input id="radius" name="spatial_averaging_radius" type="range" min="0" max="100" step="1" value="{spatial_averaging_radius}"></label>
          <label>Interpolation threshold <span class="value" id="thresholdValue"></span><input id="threshold" name="interpolation_threshold" type="range" min="0.1" max="100" step="0.1" value="{interpolation_threshold}"></label>
          <label><input id="interpolate" name="interpolate_colors" type="checkbox" value="true" {interpolate_checked}> Interpolate colorscheme</label>
        </fieldset>
        <fieldset>
          <legend>Colorscheme</legend>
          <label>Name<input id="schemeName" name="colorscheme_name" value="{colorscheme}"></label>
          <label>Colors<textarea id="schemeText" name="colorscheme_text" spellcheck="false">{colorscheme_text}</textarea></label>
        </fieldset>
      </section>
      <p>
        <button id="render" type="submit">Colorize image</button>
        <button id="saveConfig" class="secondary" type="button">Save config and colorscheme</button>
      </p>
    </form>
    <p id="status"></p>
    <section class="preview">
      <figure id="inputFigure" hidden><figcaption>Original</figcaption><img id="inputPreview" alt="Original image preview"></figure>
      <figure id="outputFigure" hidden><figcaption>Colorized</figcaption><img id="outputPreview" alt="Colorized image preview"></figure>
    </section>
    <p><a id="download" class="download" hidden>Download result</a></p>
  </main>
  <script>
    const form = document.querySelector('#form');
    const file = document.querySelector('#file');
    const status = document.querySelector('#status');
    const inputFigure = document.querySelector('#inputFigure');
    const outputFigure = document.querySelector('#outputFigure');
    const inputPreview = document.querySelector('#inputPreview');
    const outputPreview = document.querySelector('#outputPreview');
    const download = document.querySelector('#download');
    const render = document.querySelector('#render');
    const saveConfig = document.querySelector('#saveConfig');
    const controls = ['blend', 'dither', 'radius', 'threshold'];
    let inputUrl;
    let outputUrl;
    let requestId = 0;
    let debounceTimer;

    function syncValues() {{
      blendValue.textContent = blend.value;
      ditherValue.textContent = dither.value;
      radiusValue.textContent = radius.value;
      thresholdValue.textContent = threshold.value;
    }}

    function formData(includeImage) {{
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
    }}

    async function renderPreview(includeImage = false) {{
      if (!file.files.length && includeImage) return;
      const id = ++requestId;
      status.className = '';
      status.textContent = 'Colorizing...';
      render.disabled = true;
      try {{
        const response = await fetch('/colorize', {{ method: 'POST', body: formData(includeImage) }});
        if (!response.ok) throw new Error(await response.text());
        if (id !== requestId) return;
        const blob = await response.blob();
        if (outputUrl) URL.revokeObjectURL(outputUrl);
        outputUrl = URL.createObjectURL(blob);
        outputPreview.src = outputUrl;
        outputFigure.hidden = false;
        download.href = outputUrl;
        download.download = file.files[0]?.name?.replace(/\.[^.]*$/, '_colorized.png') || 'colorized.png';
        download.hidden = false;
        status.textContent = 'Done.';
      }} catch (error) {{
        if (id !== requestId) return;
        status.className = 'error';
        status.textContent = error.message || String(error);
      }} finally {{
        if (id === requestId) render.disabled = false;
      }}
    }}

    function schedulePreview() {{
      syncValues();
      clearTimeout(debounceTimer);
      debounceTimer = setTimeout(() => renderPreview(false), 120);
    }}

    file.addEventListener('change', () => {{
      if (inputUrl) URL.revokeObjectURL(inputUrl);
      if (!file.files.length) return;
      inputUrl = URL.createObjectURL(file.files[0]);
      inputPreview.src = inputUrl;
      inputFigure.hidden = false;
      renderPreview(true);
    }});

    form.addEventListener('submit', (event) => {{
      event.preventDefault();
      renderPreview(true);
    }});

    for (const id of controls) document.querySelector('#' + id).addEventListener('input', schedulePreview);
    interpolate.addEventListener('change', schedulePreview);
    schemeText.addEventListener('input', schedulePreview);
    schemeName.addEventListener('input', syncValues);

    saveConfig.addEventListener('click', async () => {{
      status.className = '';
      status.textContent = 'Saving config...';
      try {{
        const response = await fetch('/save-config', {{ method: 'POST', body: formData(false) }});
        if (!response.ok) throw new Error(await response.text());
        status.textContent = await response.text();
      }} catch (error) {{
        status.className = 'error';
        status.textContent = error.message || String(error);
      }}
    }});

    syncValues();
  </script>
</body>
</html>
"#,
        blend_factor = defaults.blend_factor,
        dither_amount = defaults.dither_amount,
        spatial_averaging_radius = defaults.spatial_averaging_radius,
        interpolation_threshold = defaults.interpolation_threshold,
        interpolate_checked = if defaults.interpolate_colors {
            "checked"
        } else {
            ""
        },
        colorscheme = escape_html(&defaults.colorscheme),
        colorscheme_text = escape_html(&defaults.colorscheme_text),
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
