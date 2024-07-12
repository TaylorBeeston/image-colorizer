use crate::colors::KANAGAWA;
use crate::constants::VERSION;
use crate::types::AppConfig;
use crate::utils::{hex_to_rgb, interpolate_color};

use clap::{App, Arg};
use config::builder::DefaultState;
use config::{ConfigBuilder, File};
use palette::{color_difference::ImprovedCiede2000, FromColor, Lab, Srgb};
use serde_derive::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct SerializedAppConfig {
    blend_factor: String,
    colorscheme: String,
    interpolation_threshold: String,
    dither_amount: String,
    spatial_averaging_radius: String,
}

fn load_config(config_path: Option<&str>) -> Result<SerializedAppConfig, config::ConfigError> {
    let mut builder = ConfigBuilder::default();

    builder = builder
        .set_default("blend_factor", "0.9")?
        .set_default("colorscheme", "kanagawa")?
        .set_default("interpolation_threshold", "2.5")?
        .set_default("dither_amount", "0.1")?
        .set_default("spatial_averaging_radius", "10")?;

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

fn interpolate_colors(colors: &[Srgb<f32>], threshold: f32) -> Vec<Lab> {
    let mut lab_colors: Vec<Lab> = colors.iter().map(|&c| Lab::from_color(c)).collect();
    lab_colors.sort_by(|a, b| a.l.partial_cmp(&b.l).unwrap());

    let mut interpolated = Vec::new();
    for window in lab_colors.windows(2) {
        let color1 = &window[0];
        let color2 = &window[1];
        interpolated.push(*color1);

        let distance = color1.improved_difference(*color2);

        if distance > threshold {
            let steps = (distance / threshold).ceil() as usize;
            for i in 1..steps {
                let t = i as f32 / steps as f32;
                interpolated.push(interpolate_color(color1, color2, t));
            }
        }
    }
    interpolated.push(*lab_colors.last().unwrap());

    interpolated
}

pub fn init() -> Result<AppConfig, Box<dyn std::error::Error>> {
    let matches = App::new("Image Colorizer")
        .version(VERSION)
        .author("Taylor Beeston")
        .about("Applies color schemes to images")
        .after_help("Config should be a TOML that contains a colorscheme and a Blend Factor.\n\nBlend Factor is a [0.0-1.0] float. Higher values will make the image adhere more strictly to the colorscheme. Lower values will make artifacting less visible. Colorscheme is a string that should be the name of a TOML file (minus the extension) in the same directory as the config file. For example if 'kanagawa' is used as the name of the colorscheme string, there should be a 'kanagawa.toml' file in the same directory as the config file.")
        .arg(
            Arg::with_name("Blend Factor")
                .short('b')
                .long("blend-factor")
                .value_name("FACTOR")
                .help("[0.0-1.0] Overrides the blend factor set in config")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Interpolation Threshold")
                .long("interpolation-threshold")
                .value_name("THRESHOLD")
                .help("[0.0-100.0] Overrides the interpolation threshold set in config")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Dither Amount")
                .short('d')
                .long("dither-amount")
                .value_name("AMOUNT")
                .help("[0.0-1.0] Overrides the dither amount set in config")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Spatial Averaging Radius")
                .long("spatial-averaging-radius")
                .value_name("RADIUS")
                .help("[0-100] Overrides the spatial averaging radius set in config")
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

    let colors = load_colorscheme(&config.colorscheme, &config_dir)?;
    let colors: Vec<Srgb<f32>> = colors.iter().map(|hex| hex_to_rgb(hex)).collect();

    let blend_factor = matches
        .value_of("Blend Factor")
        .unwrap_or(&config.blend_factor);

    let blend_factor: f32 = blend_factor
        .parse()
        .map_err(|e| format!("Failed to parse blend_factor: {}", e))?;

    let interpolation_threshold = matches
        .value_of("Interpolation Threshold")
        .unwrap_or(&config.interpolation_threshold);

    let interpolation_threshold: f32 = interpolation_threshold
        .parse()
        .map_err(|e| format!("Failed to parse interpolation_threshold: {}", e))?;

    let dither_amount = matches
        .value_of("Dither Amount")
        .unwrap_or(&config.dither_amount);

    let dither_amount: f32 = dither_amount
        .parse()
        .map_err(|e| format!("Failed to parse dither_amount: {}", e))?;

    let spatial_averaging_radius = matches
        .value_of("Spatial Averaging Radius")
        .unwrap_or(&config.spatial_averaging_radius);

    let spatial_averaging_radius: u32 = spatial_averaging_radius
        .parse()
        .map_err(|e| format!("Failed to parse spatial_averaging_radius: {}", e))?;

    // Interpolate colors
    let colors = interpolate_colors(&colors, interpolation_threshold);

    Ok(AppConfig {
        input_path: input_path.to_string(),
        blend_factor,
        colorscheme: config.colorscheme,
        colors,
        interpolation_threshold,
        dither_amount,
        spatial_averaging_radius,
    })
}
