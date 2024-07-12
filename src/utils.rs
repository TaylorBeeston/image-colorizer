use image::{Rgb, RgbImage};
use palette::{IntoColor, Lab, Srgb};
use rand::Rng;

pub fn hex_to_rgb(hex: &str) -> Srgb<f32> {
    let hex = hex.trim_start_matches('#');
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap() as f32 / 255.0;
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap() as f32 / 255.0;
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap() as f32 / 255.0;
    Srgb::new(r, g, b)
}

pub fn lab_to_image_rgb(lab: Lab) -> Rgb<u8> {
    let rgb: Srgb = lab.into_color();
    Rgb([
        (rgb.red.clamp(0.0, 1.0) * 255.0) as u8,
        (rgb.green.clamp(0.0, 1.0) * 255.0) as u8,
        (rgb.blue.clamp(0.0, 1.0) * 255.0) as u8,
    ])
}

pub fn interpolate_color(color1: &Lab, color2: &Lab, t: f32) -> Lab {
    Lab::new(
        color1.l + (color2.l - color1.l) * t,
        color1.a + (color2.a - color1.a) * t,
        color1.b + (color2.b - color1.b) * t,
    )
}

pub fn apply_dithering(color: Lab, target: Lab, amount: f32) -> Lab {
    let mut rng = rand::thread_rng();
    Lab::new(
        color.l + (target.l - color.l) * amount * rng.gen::<f32>(),
        color.a + (target.a - color.a) * amount * rng.gen::<f32>(),
        color.b + (target.b - color.b) * amount * rng.gen::<f32>(),
    )
}

pub fn compute_integral_image(image: &RgbImage) -> Vec<Vec<(f64, f64, f64)>> {
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
        }
    }

    integral
}

pub fn fast_spatial_color_average(
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    radius: u32,
    integral_image: &[Vec<(f64, f64, f64)>],
) -> Lab {
    let x1 = (x as i32 - radius as i32).max(0) as usize;
    let y1 = (y as i32 - radius as i32).max(0) as usize;
    let x2 = (x + radius).min(width - 1) as usize + 1;
    let y2 = (y + radius).min(height - 1) as usize + 1;

    let area = (x2 - x1) * (y2 - y1);

    let sum = (
        integral_image[y2][x2].0 - integral_image[y1][x2].0 - integral_image[y2][x1].0
            + integral_image[y1][x1].0,
        integral_image[y2][x2].1 - integral_image[y1][x2].1 - integral_image[y2][x1].1
            + integral_image[y1][x1].1,
        integral_image[y2][x2].2 - integral_image[y1][x2].2 - integral_image[y2][x1].2
            + integral_image[y1][x1].2,
    );

    Lab::new(
        (sum.0 / area as f64) as f32,
        (sum.1 / area as f64) as f32,
        (sum.2 / area as f64) as f32,
    )
}
