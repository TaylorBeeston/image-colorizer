mod colorize;
mod colors;
mod config;
mod constants;
mod types;
mod utils;

use crate::colorize::{colorize, ColorMap};
use crate::config::{init, AppError};
use crate::types::AppConfig;

use std::sync::Arc;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rayon::prelude::*;

fn main() -> Result<(), AppError> {
    let config = Arc::new(init()?);
    let color_map = Arc::new(ColorMap::new());
    let multi_progress = Arc::new(MultiProgress::new());

    let results: Vec<Result<(), AppError>> = config.input_output_pairs
        .par_iter()
        .map(|(input_path, output_path)| {
            let pb = multi_progress.add(ProgressBar::new(100));
            pb.set_style(ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {percent_precise}% ({eta}) {msg}")
                .unwrap()
                .progress_chars("#>-"));
            pb.set_message(format!("Processing: {}", input_path));

            let result = process_image(input_path, output_path, Arc::clone(&config), Arc::clone(&color_map), pb.clone());

            if result.is_ok() {
                pb.finish_with_message(format!("Finished: {} (Saved to: {})", input_path, output_path));
            } else {
                pb.finish_with_message(format!("Failed: {}", input_path));
            }

            result
        })
        .collect();

    // Check for any errors
    results.into_iter().collect::<Result<Vec<_>, _>>()?;

    Ok(())
}

fn process_image(
    input_path: &str,
    output_path: &str,
    config: Arc<AppConfig>,
    color_map: Arc<ColorMap>,
    pb: ProgressBar,
) -> Result<(), AppError> {
    let img = image::open(input_path)?;
    let final_output = colorize(&img, &config, color_map, pb);
    final_output.save(output_path)?;
    Ok(())
}
