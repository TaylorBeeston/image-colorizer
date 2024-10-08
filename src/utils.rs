use image::RgbImage;
use indicatif::ProgressBar;
use palette::{IntoColor, Lab, Srgb};

pub fn hex_to_rgb(input: &str) -> Result<Srgb<f32>, String> {
    let cleaned = input.trim_start_matches('#');

    match cleaned.len() {
        3 => {
            // Three-character hex code
            let r = u8::from_str_radix(&cleaned[0..1].repeat(2), 16).map_err(|e| e.to_string())?
                as f32
                / 255.0;
            let g = u8::from_str_radix(&cleaned[1..2].repeat(2), 16).map_err(|e| e.to_string())?
                as f32
                / 255.0;
            let b = u8::from_str_radix(&cleaned[2..3].repeat(2), 16).map_err(|e| e.to_string())?
                as f32
                / 255.0;
            Ok(Srgb::new(r, g, b))
        }
        6 => {
            // Six-character hex code
            let r =
                u8::from_str_radix(&cleaned[0..2], 16).map_err(|e| e.to_string())? as f32 / 255.0;
            let g =
                u8::from_str_radix(&cleaned[2..4], 16).map_err(|e| e.to_string())? as f32 / 255.0;
            let b =
                u8::from_str_radix(&cleaned[4..6], 16).map_err(|e| e.to_string())? as f32 / 255.0;
            Ok(Srgb::new(r, g, b))
        }
        _ => Err(format!(
            "Invalid input: '{}'. Expected a 3 or 6-digit hex code.",
            input
        )),
    }
}

pub fn interpolate_color(color1: &Lab, color2: &Lab, t: f32) -> Lab {
    Lab::new(
        color1.l + (color2.l - color1.l) * t,
        color1.a + (color2.a - color1.a) * t,
        color1.b + (color2.b - color1.b) * t,
    )
}

pub fn compute_integral_image(
    image: &RgbImage,
    progress_bar: &ProgressBar,
) -> Vec<Vec<(f64, f64, f64)>> {
    let (width, height) = image.dimensions();
    let mut integral = vec![vec![(0.0, 0.0, 0.0); width as usize + 1]; height as usize + 1];

    for y in 1..=height as usize {
        for x in 1..=width as usize {
            let pixel = image.get_pixel(x as u32 - 1, y as u32 - 1);
            let lab: Lab = Srgb::new(
                pixel[0] as f32 / 255.0,
                pixel[1] as f32 / 255.0,
                pixel[2] as f32 / 255.0,
            )
            .into_color();

            integral[y][x] = (
                integral[y - 1][x].0 + integral[y][x - 1].0 - integral[y - 1][x - 1].0
                    + lab.l as f64,
                integral[y - 1][x].1 + integral[y][x - 1].1 - integral[y - 1][x - 1].1
                    + lab.a as f64,
                integral[y - 1][x].2 + integral[y][x - 1].2 - integral[y - 1][x - 1].2
                    + lab.b as f64,
            );

            if (y * width as usize + x) % 100 == 0 {
                progress_bar.inc(100);
            }
        }
    }

    integral
}
