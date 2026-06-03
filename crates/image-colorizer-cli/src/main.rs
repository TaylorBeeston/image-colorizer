mod config;
mod constants;

use crate::config::{init, AppError};
use image_colorizer_core::{ColorizeStage, GpuColorizer, RenderedImage};

use std::path::Path;

use image::{ColorType, DynamicImage};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use tokio::task::{self, JoinHandle};

struct DecodedImage {
    input_path: String,
    output_path: String,
    image: DynamicImage,
}

struct PendingSave {
    input_path: String,
    output_path: String,
    pb: ProgressBar,
    handle: JoinHandle<Result<Vec<u8>, AppError>>,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{}", err);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), AppError> {
    let config = init().await?;
    let multi_progress = MultiProgress::new();
    let mut colorizer = GpuColorizer::new(&config.colorizer).await?;
    let mut input_output_pairs = config.input_output_pairs.iter();
    let Some((input_path, output_path)) = input_output_pairs.next() else {
        return Ok(());
    };

    let mut next_decode = Some(spawn_decode(input_path.clone(), output_path.clone()));
    let mut pending_save = None;

    while let Some(decode) = next_decode.take() {
        let decoded = match await_decode(decode).await {
            Ok(decoded) => decoded,
            Err(err) => {
                finish_pending_save(&mut colorizer, pending_save.take()).await?;

                return Err(err);
            }
        };

        next_decode = input_output_pairs
            .next()
            .map(|(input_path, output_path)| spawn_decode(input_path.clone(), output_path.clone()));

        let pb = progress_bar(&multi_progress, &decoded.input_path);
        let output = match colorizer
            .colorize_with_progress(&decoded.image, |_| pb.inc(1))
            .await
        {
            Ok(output) => output,
            Err(err) => {
                pb.finish_with_message(format!("Failed: {}", decoded.input_path));
                finish_pending_save(&mut colorizer, pending_save.take()).await?;

                return Err(err.into());
            }
        };

        finish_pending_save(&mut colorizer, pending_save.take()).await?;

        pending_save = Some(PendingSave {
            input_path: decoded.input_path,
            output_path: decoded.output_path.clone(),
            pb,
            handle: spawn_save(decoded.output_path, output),
        });
    }

    finish_pending_save(&mut colorizer, pending_save).await?;

    Ok(())
}

fn progress_bar(multi_progress: &MultiProgress, input_path: &str) -> ProgressBar {
    let pb = multi_progress.add(ProgressBar::new(ColorizeStage::COUNT));

    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {percent_precise}% ({eta}) {msg}",
            )
            .unwrap()
            .progress_chars("#>-"),
    );
    pb.set_message(format!("Processing: {}", input_path));

    pb
}

fn spawn_decode(
    input_path: String,
    output_path: String,
) -> JoinHandle<Result<DecodedImage, AppError>> {
    task::spawn_blocking(move || {
        let image = image::open(&input_path)?;

        Ok(DecodedImage {
            input_path,
            output_path,
            image,
        })
    })
}

async fn await_decode(
    handle: JoinHandle<Result<DecodedImage, AppError>>,
) -> Result<DecodedImage, AppError> {
    handle
        .await
        .map_err(|err| AppError::Other(format!("Image decoding task failed: {}", err)))?
}

fn spawn_save(output_path: String, image: RenderedImage) -> JoinHandle<Result<Vec<u8>, AppError>> {
    task::spawn_blocking(move || save_image(&output_path, image))
}

async fn finish_pending_save(
    colorizer: &mut GpuColorizer,
    save: Option<PendingSave>,
) -> Result<(), AppError> {
    if let Some(save) = save {
        let data = finish_save(save).await?;

        colorizer.recycle_output_buffer(data);
    }

    Ok(())
}

async fn finish_save(save: PendingSave) -> Result<Vec<u8>, AppError> {
    let result = save
        .handle
        .await
        .map_err(|err| AppError::Other(format!("Image saving task failed: {}", err)))?;

    if result.is_ok() {
        save.pb.finish_with_message(format!(
            "Finished: {} (Saved to: {})",
            save.input_path, save.output_path
        ));
    } else {
        save.pb
            .finish_with_message(format!("Failed: {}", save.input_path));
    }

    result
}

fn save_image(output_path: &str, image: RenderedImage) -> Result<Vec<u8>, AppError> {
    if let Some(parent) = Path::new(output_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    image::save_buffer(
        output_path,
        &image.data,
        image.width,
        image.height,
        ColorType::Rgb8,
    )?;

    Ok(image.data)
}
