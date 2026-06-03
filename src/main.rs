mod colorize;
mod colors;
mod config;
mod constants;
#[cfg(test)]
mod tests;
mod types;
mod utils;

use crate::colorize::colorize;
use crate::config::{init, AppError};
use crate::types::AppConfig;

use std::path::Path;
use std::sync::Arc;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use tokio::task;

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{}", err);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), AppError> {
    let config = init().await?;
    let multi_progress = Arc::new(MultiProgress::new());

    let mut handles = Vec::new();

    for (input_path, output_path) in &config.input_output_pairs {
        let config = Arc::clone(&config);
        let multi_progress = Arc::clone(&multi_progress);
        let input_path = input_path.clone();
        let output_path = output_path.clone();

        let handle = task::spawn(async move {
            let pb = multi_progress.add(ProgressBar::new(100));
            pb.set_style(ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {percent_precise}% ({eta}) {msg}")
                .unwrap()
                .progress_chars("#>-"));
            pb.set_message(format!("Processing: {}", input_path));

            let result = process_image(&input_path, &output_path, config, &pb).await;

            if result.is_ok() {
                pb.finish_with_message(format!(
                    "Finished: {} (Saved to: {})",
                    input_path, output_path
                ));
            } else {
                pb.finish_with_message(format!("Failed: {}", input_path));
            }

            result
        });

        handles.push(handle);
    }

    for handle in handles {
        handle
            .await
            .map_err(|err| AppError::Other(format!("Image processing task failed: {}", err)))??;
    }

    Ok(())
}

async fn process_image(
    input_path: &str,
    output_path: &str,
    config: Arc<AppConfig>,
    pb: &ProgressBar,
) -> Result<(), AppError> {
    let img = image::open(input_path)?;
    let final_output = colorize(&img, &config, pb).await?;

    if let Some(parent) = Path::new(output_path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    final_output.save(output_path)?;
    Ok(())
}
