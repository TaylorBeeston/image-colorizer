mod colorize;
mod colors;
mod config;
mod constants;
mod types;
mod utils;

use crate::colorize::colorize;
use crate::config::init;

use std::fs;
use std::path::Path;

use image::ImageFormat;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = init()?;

    let img = image::open(&config.input_path)?;

    let final_output = colorize(&img, &config);

    let input_path = Path::new(&config.input_path);
    let output_path = if let Some(output_filename) = &config.output_filename {
        Path::new(output_filename).to_path_buf()
    } else {
        let file_stem = input_path.file_stem().unwrap().to_str().unwrap();
        let extension = input_path.extension().unwrap_or_default().to_str().unwrap();
        input_path.with_file_name(format!(
            "{}_{}.{}",
            file_stem, config.colorscheme, extension
        ))
    };

    let mut output_file = fs::File::create(&output_path)?;
    let format = ImageFormat::from_extension(output_path.extension().unwrap_or_default())
        .unwrap_or(ImageFormat::Png);
    final_output.write_to(&mut output_file, format)?;

    println!("Saved to: {:?}", output_path);

    Ok(())
}
