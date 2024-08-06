use crate::colors::KANAGAWA;
use crate::constants::VERSION;
use crate::types::AppConfig;
use crate::utils::{hex_to_rgb, interpolate_color};

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{App, Arg};
use config::builder::DefaultState;
use config::{ConfigBuilder, ConfigError, File};
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use palette::{color_difference::ImprovedCiede2000, FromColor, Lab, Srgb};
use reqwest;
use serde_derive::Deserialize;
use toml;

#[derive(Debug)]
pub enum AppError {
    Io(std::io::Error),
    Image(image::ImageError),
    Config(ConfigError),
    Toml(toml::de::Error),
    DownloadError(String),
    Other(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            AppError::Io(err) => write!(f, "I/O error: {}", err),
            AppError::Image(err) => write!(f, "Image error: {}", err),
            AppError::Config(err) => write!(f, "Config error: {}", err),
            AppError::Toml(err) => write!(f, "TOML error: {}", err),
            AppError::DownloadError(err) => write!(f, "Download error: {}", err),
            AppError::Other(err) => write!(f, "Error: {}", err),
        }
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> AppError {
        AppError::Io(err)
    }
}

impl From<image::ImageError> for AppError {
    fn from(err: image::ImageError) -> AppError {
        AppError::Image(err)
    }
}

impl From<ConfigError> for AppError {
    fn from(err: ConfigError) -> AppError {
        AppError::Config(err)
    }
}

impl From<toml::de::Error> for AppError {
    fn from(err: toml::de::Error) -> AppError {
        AppError::Toml(err)
    }
}

impl From<String> for AppError {
    fn from(err: String) -> AppError {
        AppError::Other(err)
    }
}

impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        AppError::DownloadError(err.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct SerializedAppConfig {
    blend_factor: String,
    colorscheme: String,
    interpolate_colors: bool,
    interpolation_threshold: String,
    dither_amount: String,
    spatial_averaging_radius: String,
}

#[derive(Debug)]
pub struct ConfigInfo {
    config: SerializedAppConfig,
    config_dir: PathBuf,
}

fn load_config(config_path: Option<&str>) -> Result<ConfigInfo, AppError> {
    let mut builder = ConfigBuilder::default();

    builder = builder
        .set_default("blend_factor", "0.9")?
        .set_default("colorscheme", "kanagawa")?
        .set_default("interpolate_colors", true)?
        .set_default("interpolation_threshold", "2.5")?
        .set_default("dither_amount", "0.1")?
        .set_default("spatial_averaging_radius", "10")?;

    let default_config_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from(""))
        .join(".config/image-colorizer");
    let default_config_path = default_config_dir.join("config.toml");

    let (config_path, config_dir) = if let Some(path) = config_path {
        (
            PathBuf::from(path),
            PathBuf::from(path).parent().unwrap().to_path_buf(),
        )
    } else if default_config_path.exists() {
        (default_config_path, default_config_dir)
    } else {
        (PathBuf::new(), default_config_dir)
    };

    if config_path.exists() {
        builder = ConfigBuilder::<DefaultState>::add_source(
            builder,
            File::from(config_path).required(false),
        );
    }

    let config = builder.build()?;

    Ok(ConfigInfo {
        config: config.try_deserialize()?,
        config_dir,
    })
}

async fn load_colorscheme(name: &str, config_dir: &Path) -> Result<Vec<String>, AppError> {
    let colorscheme_path = config_dir.join(format!("{}.txt", name));

    if colorscheme_path.exists() {
        // Load from local file
        let colorscheme_str = fs::read_to_string(&colorscheme_path)?;
        parse_and_validate_colorscheme(&colorscheme_str, name)
    } else if name == "kanagawa" {
        // Built-in colorscheme
        Ok(KANAGAWA.iter().map(|&s| s.to_string()).collect())
    } else {
        // Show warning
        eprintln!(
            "Warning: Colorscheme '{}' not found locally. Attempting to download from GitHub...",
            name
        );

        // Attempt to download from GitHub
        match download_colorscheme_from_github(name).await {
            Ok(colorscheme_str) => {
                let colorscheme = parse_and_validate_colorscheme(&colorscheme_str, name)?;

                // Save the downloaded scheme
                if let Err(e) = save_colorscheme(&colorscheme_path, &colorscheme_str) {
                    eprintln!("Warning: Failed to save downloaded colorscheme: {}", e);
                }

                Ok(colorscheme)
            }
            Err(e) => Err(e), // Propagate the error without additional wrapping
        }
    }
}

fn parse_and_validate_colorscheme(content: &str, name: &str) -> Result<Vec<String>, AppError> {
    let colorscheme = parse_colorscheme(content);
    if colorscheme.is_empty() {
        Err(AppError::Other(format!("Colorscheme '{}' is empty", name)))
    } else {
        Ok(colorscheme)
    }
}

async fn download_colorscheme_from_github(name: &str) -> Result<String, AppError> {
    let url = format!(
        "https://raw.githubusercontent.com/TaylorBeeston/image-colorizer/main/colorschemes/{}.txt",
        name.to_lowercase()
    );

    let client = reqwest::Client::new();
    let res = client.get(&url).send().await?;

    // Check if the request was successful
    if !res.status().is_success() {
        return Err(AppError::DownloadError(format!(
            "Failed to download colorscheme '{}'. HTTP status: {}",
            name,
            res.status()
        )));
    }

    let total_size = res.content_length().unwrap_or(0);

    let pb = ProgressBar::new(total_size);
    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})").unwrap()
        .progress_chars("#>-"));

    let mut content = String::new();
    let mut stream = res.bytes_stream();

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| AppError::DownloadError(e.to_string()))?;
        content.push_str(&String::from_utf8_lossy(&chunk));
        pb.inc(chunk.len() as u64);
    }

    pb.finish_with_message("Download complete");

    Ok(content)
}

fn save_colorscheme(path: &Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}

fn parse_colorscheme(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.split("//").next().unwrap_or("").trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

fn interpolate_colors(mut colors: Vec<Lab>, threshold: f32) -> Vec<Lab> {
    colors.sort_by(|a, b| a.l.partial_cmp(&b.l).unwrap());

    let mut interpolated = Vec::new();
    for window in colors.windows(2) {
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
    interpolated.push(*colors.last().unwrap());

    interpolated
}

pub async fn init() -> Result<Arc<AppConfig>, AppError> {
    let matches = App::new("Image Colorizer")
        .version(VERSION)
        .author("Taylor Beeston")
        .about("Applies color schemes to images")
        .after_help("Config should be a TOML that contains a colorscheme and a Blend Factor.\n\nBlend Factor is a [0.0-1.0] float. Higher values will make the image adhere more strictly to the colorscheme. Lower values will make artifacting less visible. Colorscheme is a string that should be the name of a colorscheme txt file (minus the extension) in the same directory as the config file. For example if 'kanagawa' is used as the name of the colorscheme string, there should be a 'kanagawa.txt' file in the same directory as the config file.\n\nColorscheme files are simple files with one hex code per line and may optionally have comments using double slashes, e.g.\n\n// Grayscale\n#fff\n#000")
        .arg(
            Arg::with_name("Blend Factor")
                .short('b')
                .long("blend-factor")
                .value_name("FACTOR")
                .help("[0.0-1.0] (Default: 0.9) Overrides the blend factor set in config")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Interpolation Threshold")
                .long("interpolation-threshold")
                .value_name("THRESHOLD")
                .help("[0.0-100.0] (Default: 2.5) Overrides the interpolation threshold set in config")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Interpolate Colors")
                .short('i')
                .long("interpolate-colors")
                .takes_value(false)
                .help("(Default: true) Sets whether or not to interpolate colors in the colorscheme for less artifacting")
        )
        .arg(
            Arg::with_name("Dither Amount")
                .short('d')
                .long("dither-amount")
                .value_name("AMOUNT")
                .help("[0.0-1.0] (Default: 0.1) Overrides the dither amount set in config")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Spatial Averaging Radius")
                .long("spatial-averaging-radius")
                .value_name("RADIUS")
                .help("[0-100] (Default: 10) Overrides the spatial averaging radius set in config")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Colorscheme")
                .short('s')
                .long("colorscheme")
                .value_name("SCHEME")
                .help("(Default: kanagawa) Sets the colorscheme to use")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Config")
                .short('c')
                .long("config")
                .value_name("/path/to/config.toml")
                .help("(Default: ~/.config/image-colorizer/config.toml) Sets a custom config file")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Output")
                .short('o')
                .long("output")
                .value_name("OUTPUT_DIR")
                .help("Sets the output directory")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("Image Paths")
                .help("Paths to the images you'd like to colorize")
                .required(true)
                .multiple(true)
                .index(1),
        )
        .get_matches();

    let ConfigInfo { config, config_dir } = load_config(matches.value_of("config"))?;

    let input_paths: Vec<&str> = matches.values_of("Image Paths").unwrap().collect();
    let output_dir = matches.value_of("output").map(PathBuf::from);

    let input_output_pairs =
        generate_input_output_pairs(&input_paths, output_dir, &config.colorscheme)?;

    let colorscheme = matches
        .value_of("Colorscheme")
        .unwrap_or(&config.colorscheme);

    let blend_factor = matches
        .value_of("Blend Factor")
        .unwrap_or(&config.blend_factor);

    let blend_factor: f32 = blend_factor
        .parse()
        .map_err(|e| format!("Failed to parse blend_factor: {}", e))?;

    let should_interpolate_colors = if matches.contains_id("Interpolate Colors") {
        matches.get_flag("Interpolate Colors")
    } else {
        config.interpolate_colors
    };

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

    let colors = load_colorscheme(colorscheme, &config_dir).await?;
    let colors: Vec<Lab> = colors
        .iter()
        .map(|hex| Lab::from_color(hex_to_rgb(hex).unwrap()))
        .collect();

    // Interpolate colors
    let colors = if should_interpolate_colors {
        interpolate_colors(colors, interpolation_threshold)
    } else {
        colors
    };

    Ok(Arc::new(AppConfig {
        input_output_pairs,
        blend_factor,
        colors,
        dither_amount,
        spatial_averaging_radius,
    }))
}

fn generate_input_output_pairs(
    input_paths: &[&str],
    output_dir: Option<PathBuf>,
    colorscheme: &str,
) -> Result<Vec<(String, String)>, AppError> {
    let mut pairs = Vec::new();

    for input_path in input_paths {
        let input_path = Path::new(input_path);
        let file_stem = input_path.file_stem().unwrap().to_str().unwrap();
        let extension = input_path.extension().unwrap_or_default().to_str().unwrap();

        let output_path = if let Some(ref dir) = output_dir {
            dir.join(format!("{}_{}.{}", file_stem, colorscheme, extension))
        } else {
            input_path.with_file_name(format!("{}_{}.{}", file_stem, colorscheme, extension))
        };

        pairs.push((
            input_path.to_str().unwrap().to_string(),
            output_path.to_str().unwrap().to_string(),
        ));
    }

    Ok(pairs)
}
