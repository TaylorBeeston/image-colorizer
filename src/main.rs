mod colors;

use colors::KANAGAWA;

use clap::{App, Arg};
use config::builder::DefaultState;
use config::{ConfigBuilder, File};
use image::{GenericImageView, ImageBuffer, RgbImage};
use indicatif::{ProgressBar, ProgressStyle};
use palette::color_difference::ImprovedCiede2000;
use palette::{FromColor, IntoColor, Lab, Lch, Srgb};
use rayon::prelude::*;
use serde_derive::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct AppConfig {
    blend_factor: String,
    colorscheme: String,
}

fn hex_to_rgb(hex: &str) -> Srgb<f32> {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap() as f32 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap() as f32 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap() as f32 / 255.0;
    Srgb::new(r, g, b)
}

fn adjust_luminance(color: Srgb<f32>, target_luminance: f32) -> Srgb<f32> {
    let mut lch: Lch = color.into_color();
    lch.l = target_luminance * 100.0;
    lch.into_color()
}

fn load_config(config_path: Option<&str>) -> Result<AppConfig, config::ConfigError> {
    let mut builder = ConfigBuilder::default();

    builder = builder
        .set_default("blend_factor", "0.9")?
        .set_default("colorscheme", "kanagawa")?;

    let default_config_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from(""))
        .join(".config/colorizer/config.toml");

    if default_config_path.exists() {
        builder = ConfigBuilder::<DefaultState>::add_source(
            builder,
            File::from(default_config_path).required(false),
        );
    }

    if let Some(path) = config_path {
        builder = builder.add_source(File::with_name(path).required(true));
    }

    let config = builder.build()?;

    config.try_deserialize()
}

fn load_colorscheme(
    name: &str,
    config_dir: &Path,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let colorscheme_path = config_dir.join(format!("{}.toml", name));
    if colorscheme_path.exists() {
        let colorscheme_str = fs::read_to_string(colorscheme_path)?;
        let colorscheme: Vec<String> = toml::from_str(&colorscheme_str)?;
        Ok(colorscheme)
    } else if name == "kanagawa" {
        Ok(KANAGAWA.iter().map(|&s| s.to_string()).collect())
    } else {
        Err(format!("Colorscheme '{}' not found", name).into())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = App::new("Image Colorizer")
        .version("1.0")
        .author("Taylor Beeston")
        .about("Applies color schemes to images")
        .after_help("Config should be a TOML that contains a colorscheme and a Blend Factor.\n\nBlend Factor is a [0.0-1.0] float. Higher values will make the image adhere more strictly to the colorscheme. Lower values will make artifacting less visible. Colorscheme is a string that should be the name of a TOML file (minus the extension) in the same directory as the config file. For example if 'kanagawa' is used as the name of the colorscheme string, there should be a 'kanagawa.toml' file in the same directory as the config file.")
        .arg(
            Arg::with_name("Blend Factor")
                .short('b')
                .long("blend-factor")
                .value_name("FACTOR")
                .help("Overrides the blend factor set in config")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("config")
                .short('c')
                .long("config")
                .value_name("/path/to/image.png")
                .help("Sets a custom config file")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Image Path")
                .help("Path to the image you'd like to colorize")
                .required(true)
                .index(1),
        )
        .get_matches();

    let config = load_config(matches.value_of("config"))?;
    let input_path = matches.value_of("Image Path").unwrap();

    let config_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from(""))
        .join(".config/colorizer");

    let colorscheme = load_colorscheme(&config.colorscheme, &config_dir)?;
    let colors: Vec<Srgb<f32>> = colorscheme.iter().map(|hex| hex_to_rgb(hex)).collect();

    let string_blend_factor = matches
        .value_of("Blend Factor")
        .unwrap_or(&config.blend_factor);

    let blend_factor: f32 = string_blend_factor
        .parse()
        .map_err(|e| format!("Failed to parse blend_factor: {}", e))?;

    let img = image::open(input_path)?;
    let (width, height) = img.dimensions();
    let total_pixels = (width * height) as u64;

    let pb = ProgressBar::new(total_pixels);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
    );

    let progress = Arc::new(AtomicU64::new(0));
    let pb_arc = Arc::new(pb);

    let mut output: RgbImage = ImageBuffer::new(width, height);

    output.par_chunks_mut(3).enumerate().for_each(|(i, chunk)| {
        let x = (i as u32) % width;
        let y = (i as u32) / width;
        let pixel = img.get_pixel(x, y);
        let original_rgb = Srgb::new(
            pixel[0] as f32 / 255.0,
            pixel[1] as f32 / 255.0,
            pixel[2] as f32 / 255.0,
        );
        let original_lab: Lab = original_rgb.into_color();
        let original_lch: Lch = original_lab.into_color();

        let (closest_color, _) = colors
            .iter()
            .map(|&c| (c, Lab::from_color(c)))
            .min_by(|&(_, a), &(_, b)| {
                original_lab
                    .improved_difference(a)
                    .partial_cmp(&original_lab.improved_difference(b))
                    .unwrap()
            })
            .unwrap();

        let adjusted_color = adjust_luminance(closest_color, original_lch.l / 100.0);

        let final_color = Srgb::new(
            adjusted_color.red * blend_factor + original_rgb.red * (1.0 - blend_factor),
            adjusted_color.green * blend_factor + original_rgb.green * (1.0 - blend_factor),
            adjusted_color.blue * blend_factor + original_rgb.blue * (1.0 - blend_factor),
        );

        chunk[0] = (final_color.red * 255.0) as u8;
        chunk[1] = (final_color.green * 255.0) as u8;
        chunk[2] = (final_color.blue * 255.0) as u8;

        let prev_count = progress.fetch_add(1, Ordering::Relaxed);
        if prev_count % 10000 == 0 {
            pb_arc.set_position(prev_count);
        }
    });

    pb_arc.finish_with_message("Processing complete");

    let output_path = format!(
        "{}_{}_{}.png",
        input_path.trim_end_matches(".png"),
        config.colorscheme,
        blend_factor
    );
    output.save(output_path)?;

    println!("Processed image saved successfully!");
    Ok(())
}
