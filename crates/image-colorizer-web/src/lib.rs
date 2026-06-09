//! Static client-only WebGPU app assets and CPU fallback.
//!
//! This crate is a workspace boundary for the hostable browser app. The files in
//! `static/` are served directly by static hosts such as Netlify and intentionally
//! do not replace the CLI's native-GPU `image-colorizer serve` UI.

use js_sys::Function;
use wasm_bindgen::prelude::*;

/// Relative path to the static site assets from the repository root.
pub const STATIC_DIR: &str = "crates/image-colorizer-web/static";

#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn cpu_colorize(
    rgba: &[u8],
    width: u32,
    height: u32,
    colorscheme: &str,
    blend_factor: f32,
    dither_amount: f32,
    spatial_radius: u32,
    interpolate_colors: bool,
    interpolation_threshold: f32,
    progress: &Function,
) -> Result<Vec<u8>, JsValue> {
    let pixel_count = width as usize * height as usize;

    if rgba.len() != pixel_count * 4 {
        return Err(JsValue::from_str(
            "Image data size does not match dimensions.",
        ));
    }

    let palette = build_palette(colorscheme, interpolate_colors, interpolation_threshold)?;
    report_progress(progress, 0.03);

    let mut working = Vec::with_capacity(pixel_count);

    for (index, pixel) in rgba.chunks_exact(4).enumerate() {
        let input = [
            f32::from(pixel[0]) / 255.0,
            f32::from(pixel[1]) / 255.0,
            f32::from(pixel[2]) / 255.0,
        ];
        let lab = rgb_to_lab(input);
        let closest = closest_color(lab, &palette);
        let final_lab = [lab[0], closest[1], closest[2]];
        let x = (index % width as usize) as u32;
        let y = (index / width as usize) as u32;
        let dithered_lab = apply_dither(final_lab, lab, dither_amount, x, y);
        let final_rgb = lab_to_rgb(dithered_lab);
        let quantized_rgb = quantize_rgb(mix_rgb(input, final_rgb, blend_factor));
        let quantized_lab = rgb_to_lab(quantized_rgb);

        working.push(WorkingPixel {
            rgb: quantized_rgb,
            lab: quantized_lab,
        });

        if index % width as usize == 0 {
            let y = index / width as usize;

            if y & 7 == 0 {
                report_progress(progress, 0.05 + 0.45 * y as f64 / height as f64);
            }
        }
    }

    let mut horizontal = vec![[0.0; 3]; pixel_count];
    let radius = spatial_radius as i32;
    let width_i32 = width as i32;
    let height_i32 = height as i32;

    report_progress(progress, 0.50);

    for y in 0..height_i32 {
        for x in 0..width_i32 {
            let x1 = (x - radius).max(0);
            let x2 = (x + radius).min(width_i32 - 1);
            let mut sum = [0.0; 3];

            for sample_x in x1..=x2 {
                let sample = working[(sample_x + y * width_i32) as usize].lab;

                sum[0] += sample[0];
                sum[1] += sample[1];
                sum[2] += sample[2];
            }

            let count = (x2 - x1 + 1) as f32;
            horizontal[(x + y * width_i32) as usize] =
                [sum[0] / count, sum[1] / count, sum[2] / count];
        }

        if y & 7 == 0 {
            report_progress(progress, 0.50 + 0.20 * f64::from(y) / f64::from(height));
        }
    }

    let mut output = vec![0; pixel_count * 4];
    report_progress(progress, 0.70);

    for y in 0..height_i32 {
        for x in 0..width_i32 {
            let y1 = (y - radius).max(0);
            let y2 = (y + radius).min(height_i32 - 1);
            let mut sum = [0.0; 3];

            for sample_y in y1..=y2 {
                let sample = horizontal[(x + sample_y * width_i32) as usize];

                sum[0] += sample[0];
                sum[1] += sample[1];
                sum[2] += sample[2];
            }

            let index = (x + y * width_i32) as usize;
            let count = (y2 - y1 + 1) as f32;
            let average_lab = [sum[0] / count, sum[1] / count, sum[2] / count];
            let input = working[index];
            let transferred_rgb = lab_to_rgb([input.lab[0], average_lab[1], average_lab[2]]);
            let final_rgb = mix_rgb(input.rgb, transferred_rgb, blend_factor).map(clamp01);
            let out = &mut output[index * 4..index * 4 + 4];

            out[0] = (final_rgb[0] * 255.0) as u8;
            out[1] = (final_rgb[1] * 255.0) as u8;
            out[2] = (final_rgb[2] * 255.0) as u8;
            out[3] = 255;
        }

        if y & 7 == 0 {
            report_progress(progress, 0.70 + 0.29 * f64::from(y) / f64::from(height));
        }
    }

    report_progress(progress, 1.0);

    Ok(output)
}

fn report_progress(progress: &Function, value: f64) {
    let _ = progress.call1(&JsValue::NULL, &JsValue::from_f64(value));
}

#[derive(Clone, Copy)]
struct WorkingPixel {
    rgb: [f32; 3],
    lab: [f32; 3],
}

fn build_palette(text: &str, interpolate: bool, threshold: f32) -> Result<Vec<[f32; 3]>, JsValue> {
    let mut colors = Vec::new();

    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();

        if line.is_empty() {
            continue;
        }

        colors.push(rgb_to_lab(hex_to_rgb(line)?));
    }

    if colors.is_empty() {
        return Err(JsValue::from_str(
            "Colorscheme must contain at least one color.",
        ));
    }

    if !interpolate || colors.len() < 2 {
        return Ok(colors);
    }

    colors.sort_by(|left, right| left[0].total_cmp(&right[0]));

    let mut output = Vec::with_capacity(colors.len());

    for pair in colors.windows(2) {
        let current = pair[0];
        let next = pair[1];

        output.push(current);

        let distance = ciede2000(current, next);

        if distance <= threshold {
            continue;
        }

        let steps = (distance / threshold).ceil() as u32;

        for step in 1..steps {
            output.push(mix_lab(current, next, step as f32 / steps as f32));
        }
    }

    output.push(*colors.last().expect("palette has at least one color"));

    Ok(output)
}

fn hex_to_rgb(hex: &str) -> Result<[f32; 3], JsValue> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    let expanded;
    let hex = if hex.len() == 3 {
        expanded = hex.chars().flat_map(|ch| [ch, ch]).collect::<String>();
        expanded.as_str()
    } else {
        hex
    };

    if hex.len() != 6 || !hex.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(JsValue::from_str("Colors must be hex values like #7e9cd8."));
    }

    let r = u8::from_str_radix(&hex[0..2], 16).map_err(|_| JsValue::from_str("Invalid color."))?;
    let g = u8::from_str_radix(&hex[2..4], 16).map_err(|_| JsValue::from_str("Invalid color."))?;
    let b = u8::from_str_radix(&hex[4..6], 16).map_err(|_| JsValue::from_str("Invalid color."))?;

    Ok([
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    ])
}

fn closest_color(color: [f32; 3], palette: &[[f32; 3]]) -> [f32; 3] {
    let mut closest = palette[0];
    let mut min_distance = chroma_distance(color, closest);

    for &candidate in &palette[1..] {
        let current_distance = chroma_distance(color, candidate);

        if current_distance < min_distance {
            min_distance = current_distance;
            closest = candidate;
        }
    }

    closest
}

fn apply_dither(color: [f32; 3], target: [f32; 3], amount: f32, x: u32, y: u32) -> [f32; 3] {
    let rand = ((x as f32 * 12.9898 + y as f32 * 78.233).sin() * 43758.547).fract();

    [
        color[0] + (target[0] - color[0]) * amount * rand,
        color[1] + (target[1] - color[1]) * amount * rand,
        color[2] + (target[2] - color[2]) * amount * rand,
    ]
}

fn quantize_rgb(rgb: [f32; 3]) -> [f32; 3] {
    [
        (clamp01(rgb[0]) * 255.0).floor() / 255.0,
        (clamp01(rgb[1]) * 255.0).floor() / 255.0,
        (clamp01(rgb[2]) * 255.0).floor() / 255.0,
    ]
}

fn mix_rgb(left: [f32; 3], right: [f32; 3], amount: f32) -> [f32; 3] {
    [
        left[0] + (right[0] - left[0]) * amount,
        left[1] + (right[1] - left[1]) * amount,
        left[2] + (right[2] - left[2]) * amount,
    ]
}

fn mix_lab(left: [f32; 3], right: [f32; 3], amount: f32) -> [f32; 3] {
    [
        left[0] + (right[0] - left[0]) * amount,
        left[1] + (right[1] - left[1]) * amount,
        left[2] + (right[2] - left[2]) * amount,
    ]
}

fn rgb_to_lab(rgb: [f32; 3]) -> [f32; 3] {
    xyz_to_lab(rgb_to_xyz(rgb))
}

fn rgb_to_xyz(rgb: [f32; 3]) -> [f32; 3] {
    let r = srgb_to_linear(rgb[0]);
    let g = srgb_to_linear(rgb[1]);
    let b = srgb_to_linear(rgb[2]);

    [
        r * 0.4124564 + g * 0.3575761 + b * 0.1804375,
        r * 0.2126729 + g * 0.7151522 + b * 0.0721750,
        r * 0.0193339 + g * 0.119_192 + b * 0.9503041,
    ]
}

fn xyz_to_lab(xyz: [f32; 3]) -> [f32; 3] {
    let xr = xyz[0] / 0.950489;
    let yr = xyz[1];
    let zr = xyz[2] / 1.088_84;
    let fx = lab_f(xr);
    let fy = lab_f(yr);
    let fz = lab_f(zr);

    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_rgb(lab: [f32; 3]) -> [f32; 3] {
    xyz_to_rgb(lab_to_xyz(lab))
}

fn lab_to_xyz(lab: [f32; 3]) -> [f32; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = lab[1] / 500.0 + fy;
    let fz = fy - lab[2] / 200.0;
    let epsilon = 0.008856;
    let kappa = 903.3;
    let fx3 = fx * fx * fx;
    let fz3 = fz * fz * fz;
    let xr = if fx3 > epsilon {
        fx3
    } else {
        (116.0 * fx - 16.0) / kappa
    };
    let yr = if lab[0] > kappa * epsilon {
        fy * fy * fy
    } else {
        lab[0] / kappa
    };
    let zr = if fz3 > epsilon {
        fz3
    } else {
        (116.0 * fz - 16.0) / kappa
    };

    [xr * 0.950489, yr, zr * 1.088_84]
}

fn xyz_to_rgb(xyz: [f32; 3]) -> [f32; 3] {
    let r = xyz[0] * 3.2404542 + xyz[1] * -1.5371385 + xyz[2] * -0.4985314;
    let g = xyz[0] * -0.969_266 + xyz[1] * 1.8760108 + xyz[2] * 0.0415560;
    let b = xyz[0] * 0.0556434 + xyz[1] * -0.2040259 + xyz[2] * 1.0572252;

    [linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b)].map(clamp01)
}

fn srgb_to_linear(channel: f32) -> f32 {
    if channel > 0.04045 {
        ((channel + 0.055) / 1.055).powf(2.4)
    } else {
        channel / 12.92
    }
}

fn linear_to_srgb(channel: f32) -> f32 {
    if channel > 0.0031308 {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    } else {
        12.92 * channel
    }
}
fn lab_f(value: f32) -> f32 {
    if value > 0.008856 {
        value.cbrt()
    } else {
        (903.3 * value + 16.0) / 116.0
    }
}

fn chroma_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    let a = left[1] - right[1];
    let b = left[2] - right[2];

    (a * a + b * b).sqrt()
}

fn ciede2000(left: [f32; 3], right: [f32; 3]) -> f32 {
    let [l1, a1, b1] = left;
    let [l2, a2, b2] = right;
    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let c_mean = (c1 + c2) / 2.0;
    let c_mean7 = c_mean.powi(7);
    let g = 0.5 * (1.0 - (c_mean7 / (c_mean7 + 25_f32.powi(7))).sqrt());
    let a1_prime = (1.0 + g) * a1;
    let a2_prime = (1.0 + g) * a2;
    let c1_prime = a1_prime.hypot(b1);
    let c2_prime = a2_prime.hypot(b2);
    let h1_prime = hue_degrees(b1, a1_prime);
    let h2_prime = hue_degrees(b2, a2_prime);
    let delta_l_prime = l2 - l1;
    let delta_c_prime = c2_prime - c1_prime;
    let delta_h_prime = 2.0
        * (c1_prime * c2_prime).sqrt()
        * (deg_to_rad(delta_hue(h1_prime, h2_prime, c1_prime, c2_prime) / 2.0)).sin();
    let l_mean_prime = (l1 + l2) / 2.0;
    let c_mean_prime = (c1_prime + c2_prime) / 2.0;
    let h_mean_prime = mean_hue(h1_prime, h2_prime, c1_prime, c2_prime);
    let t = 1.0 - 0.17 * deg_to_rad(h_mean_prime - 30.0).cos()
        + 0.24 * deg_to_rad(2.0 * h_mean_prime).cos()
        + 0.32 * deg_to_rad(3.0 * h_mean_prime + 6.0).cos()
        - 0.20 * deg_to_rad(4.0 * h_mean_prime - 63.0).cos();
    let delta_theta = 30.0 * (-((h_mean_prime - 275.0) / 25.0).powi(2)).exp();
    let c_mean_prime7 = c_mean_prime.powi(7);
    let rc = 2.0 * (c_mean_prime7 / (c_mean_prime7 + 25_f32.powi(7))).sqrt();
    let sl = 1.0
        + (0.015 * (l_mean_prime - 50.0).powi(2)) / (20.0 + (l_mean_prime - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * c_mean_prime;
    let sh = 1.0 + 0.015 * c_mean_prime * t;
    let rt = -deg_to_rad(2.0 * delta_theta).sin() * rc;
    let l_term = delta_l_prime / sl;
    let c_term = delta_c_prime / sc;
    let h_term = delta_h_prime / sh;

    (l_term * l_term + c_term * c_term + h_term * h_term + rt * c_term * h_term).sqrt()
}

fn hue_degrees(y: f32, x: f32) -> f32 {
    if x == 0.0 && y == 0.0 {
        0.0
    } else {
        let angle = y.atan2(x).to_degrees();

        if angle >= 0.0 {
            angle
        } else {
            angle + 360.0
        }
    }
}

fn delta_hue(h1: f32, h2: f32, c1: f32, c2: f32) -> f32 {
    if c1 * c2 == 0.0 {
        return 0.0;
    }

    let difference = h2 - h1;

    if difference.abs() <= 180.0 {
        difference
    } else if difference > 180.0 {
        difference - 360.0
    } else {
        difference + 360.0
    }
}

fn mean_hue(h1: f32, h2: f32, c1: f32, c2: f32) -> f32 {
    if c1 * c2 == 0.0 {
        h1 + h2
    } else if (h1 - h2).abs() <= 180.0 {
        (h1 + h2) / 2.0
    } else if h1 + h2 < 360.0 {
        (h1 + h2 + 360.0) / 2.0
    } else {
        (h1 + h2 - 360.0) / 2.0
    }
}

fn deg_to_rad(degrees: f32) -> f32 {
    degrees.to_radians()
}

fn clamp01(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{chroma_distance, closest_color};

    #[test]
    fn closest_color_ignores_lightness() {
        let source = [15.0, 42.0, -36.0];
        let dark_gray = [14.0, 0.0, 0.0];
        let bright_chroma_match = [82.0, 42.0, -36.0];

        assert_eq!(
            closest_color(source, &[dark_gray, bright_chroma_match]),
            bright_chroma_match
        );
    }

    #[test]
    fn chroma_distance_ignores_lightness() {
        assert_eq!(chroma_distance([12.0, 3.0, 4.0], [98.0, 3.0, 4.0]), 0.0);
    }
}
