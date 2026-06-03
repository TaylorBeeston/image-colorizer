use crate::config::{AppError, ServeConfig};

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Multipart, State};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::{HeaderValue, Response, StatusCode};
use axum::response::Html;
use axum::routing::{get, post};
use axum::Router;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder};
use image_colorizer_core::{GpuColorizer, RenderedImage};
use tokio::sync::Mutex;

struct AppState {
    colorizer: Mutex<GpuColorizer>,
}

pub async fn serve(config: &ServeConfig) -> Result<(), AppError> {
    let colorizer = GpuColorizer::new(&config.colorizer).await?;
    let state = Arc::new(AppState {
        colorizer: Mutex::new(colorizer),
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/colorize", post(colorize_upload))
        .with_state(state);

    eprintln!("Serving Image Colorizer at http://{}", config.bind);

    axum::Server::bind(&config.bind)
        .serve(app.into_make_service())
        .await
        .map_err(|err| AppError::Other(format!("Web server failed: {}", err)))
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn colorize_upload(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Response<Body>, (StatusCode, String)> {
    let mut image_bytes = None;
    let mut file_name = None;

    while let Some(field) = multipart.next_field().await.map_err(bad_request)? {
        if field.name() != Some("image") {
            continue;
        }

        file_name = field.file_name().map(ToOwned::to_owned);
        image_bytes = Some(field.bytes().await.map_err(bad_request)?);
    }

    let image_bytes = image_bytes.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "Missing multipart file field named 'image'".to_string(),
        )
    })?;
    let image = image::load_from_memory(&image_bytes).map_err(bad_request)?;
    let mut colorizer = state.colorizer.lock().await;
    let rendered = colorizer.colorize(&image).await.map_err(server_error)?;
    let png = encode_png(&rendered).map_err(server_error)?;

    colorizer.recycle_output_buffer(rendered.data);

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, HeaderValue::from_static("image/png"))
        .header(
            CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!(
                "attachment; filename=\"{}\"",
                output_file_name(file_name.as_deref())
            ))
            .map_err(server_error)?,
        )
        .body(Body::from(png))
        .map_err(server_error)
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

fn bad_request(err: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, err.to_string())
}

fn server_error(err: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Image Colorizer</title>
  <style>
    :root { color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; }
    body { min-height: 100vh; margin: 0; display: grid; place-items: center; background: #16161d; color: #dcd7ba; }
    main { width: min(900px, calc(100vw - 32px)); padding: 32px; border: 1px solid #363646; border-radius: 24px; background: #1f1f28; box-shadow: 0 24px 80px #0008; }
    h1 { margin: 0 0 8px; font-size: clamp(2rem, 6vw, 4rem); letter-spacing: -0.06em; }
    p { color: #c8c093; line-height: 1.6; }
    form { display: grid; gap: 16px; margin: 28px 0; }
    input[type=file] { padding: 24px; border: 1px dashed #54546d; border-radius: 18px; background: #181820; color: #dcd7ba; }
    button, a.download { width: fit-content; border: 0; border-radius: 999px; padding: 12px 18px; background: #7e9cd8; color: #16161d; font-weight: 700; cursor: pointer; text-decoration: none; }
    button:disabled { opacity: 0.55; cursor: wait; }
    .preview { display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 18px; margin-top: 24px; }
    figure { margin: 0; }
    figcaption { margin-bottom: 8px; color: #938aa9; font-size: 0.9rem; }
    img { max-width: 100%; border-radius: 16px; background: #0d0c0c; }
    .error { color: #e46876; white-space: pre-wrap; }
  </style>
</head>
<body>
  <main>
    <h1>Image Colorizer</h1>
    <p>Upload an image and colorize it locally through the native GPU pipeline. The file stays on this machine; this page talks only to the local server started by <code>image-colorizer serve</code>.</p>
    <form id="form">
      <input id="file" name="image" type="file" accept="image/*" required>
      <button id="submit" type="submit">Colorize image</button>
    </form>
    <p id="status"></p>
    <section class="preview">
      <figure id="inputFigure" hidden>
        <figcaption>Original</figcaption>
        <img id="inputPreview" alt="Original image preview">
      </figure>
      <figure id="outputFigure" hidden>
        <figcaption>Colorized</figcaption>
        <img id="outputPreview" alt="Colorized image preview">
      </figure>
    </section>
    <p><a id="download" class="download" hidden>Download result</a></p>
  </main>
  <script>
    const form = document.querySelector('#form');
    const file = document.querySelector('#file');
    const submit = document.querySelector('#submit');
    const status = document.querySelector('#status');
    const inputFigure = document.querySelector('#inputFigure');
    const outputFigure = document.querySelector('#outputFigure');
    const inputPreview = document.querySelector('#inputPreview');
    const outputPreview = document.querySelector('#outputPreview');
    const download = document.querySelector('#download');
    let inputUrl;
    let outputUrl;

    file.addEventListener('change', () => {
      if (inputUrl) URL.revokeObjectURL(inputUrl);
      if (!file.files.length) return;
      inputUrl = URL.createObjectURL(file.files[0]);
      inputPreview.src = inputUrl;
      inputFigure.hidden = false;
    });

    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      status.className = '';
      status.textContent = 'Colorizing...';
      submit.disabled = true;
      outputFigure.hidden = true;
      download.hidden = true;

      if (outputUrl) URL.revokeObjectURL(outputUrl);

      try {
        const body = new FormData(form);
        const response = await fetch('/colorize', { method: 'POST', body });
        if (!response.ok) throw new Error(await response.text());

        const blob = await response.blob();
        outputUrl = URL.createObjectURL(blob);
        outputPreview.src = outputUrl;
        outputFigure.hidden = false;
        download.href = outputUrl;
        download.download = file.files[0]?.name?.replace(/\.[^.]*$/, '_colorized.png') || 'colorized.png';
        download.hidden = false;
        status.textContent = 'Done.';
      } catch (error) {
        status.className = 'error';
        status.textContent = error.message || String(error);
      } finally {
        submit.disabled = false;
      }
    });
  </script>
</body>
</html>
"#;
