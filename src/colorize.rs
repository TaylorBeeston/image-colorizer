use crate::types::AppConfig;
use crate::utils::{
    apply_dithering, compute_integral_image, create_progress_bar, fast_spatial_color_average,
    lab_to_image_rgb, update_progress,
};

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use image::{DynamicImage, GenericImageView, ImageBuffer, Pixel, Rgb, RgbImage};
use indicatif::ProgressBar;
use palette::color_difference::ImprovedCiede2000;
use palette::{IntoColor, Lab, Srgb};
use rayon::prelude::*;

pub fn colorize(img: &DynamicImage, config: &AppConfig) -> RgbImage {
    let (width, height) = img.dimensions();
    let total_pixels = (width * height) as u64;
    let total_steps = total_pixels * 3; // Three passes over the image

    let progress = Arc::new(AtomicU64::new(0));
    let progress_bar = create_progress_bar(total_steps, "Colorizing image".to_string());

    progress_bar.set_message("Pass 1: Applying Color Mapping and Dithering");

    // First pass: Color mapping and dithering
    let first_pass_output =
        apply_color_mapping_and_dithering(img, config, &progress, &progress_bar);

    progress_bar.set_message("Pass 2: Calculating Integral Image");

    // Second pass: Compute integral image
    let integral_image = compute_integral_image(&first_pass_output, &progress, &progress_bar);

    progress_bar.set_message("Pass 3: Applying Spatial Averaging and Luminance Transfer");

    // Third pass: Spatial averaging and luminance transfer
    let final_output = apply_spatial_averaging_and_luminance_transfer(
        img,
        config,
        &integral_image,
        &progress,
        &progress_bar,
    );

    progress_bar.finish_with_message("Image colorization complete");
    final_output
}

fn apply_color_mapping_and_dithering(
    img: &DynamicImage,
    config: &AppConfig,
    progress: &Arc<AtomicU64>,
    progress_bar: &ProgressBar,
) -> RgbImage {
    let (width, height) = img.dimensions();
    let output: Arc<Mutex<RgbImage>> = Arc::new(Mutex::new(ImageBuffer::new(width, height)));
    let color_map: Arc<Mutex<HashMap<[u8; 3], Lab>>> = Arc::new(Mutex::new(HashMap::new()));

    (0..height).into_par_iter().for_each(|y| {
        for x in 0..width {
            let pixel = img.get_pixel(x, y);
            let colorized_lab =
                memoized_find_closest_color(&color_map, pixel.to_rgb(), &config.colors);
            let dithered_color =
                apply_dithering(colorized_lab, colorized_lab, config.dither_amount);

            let new_pixel = lab_to_image_rgb(dithered_color);
            output.lock().unwrap().put_pixel(x, y, new_pixel);

            update_progress(progress, progress_bar);
        }
    });

    Arc::try_unwrap(output).unwrap().into_inner().unwrap()
}

fn apply_spatial_averaging_and_luminance_transfer(
    original_img: &DynamicImage,
    config: &AppConfig,
    integral_image: &[Vec<(f64, f64, f64)>],
    progress: &Arc<AtomicU64>,
    progress_bar: &ProgressBar,
) -> RgbImage {
    let (width, height) = original_img.dimensions();
    let final_output: Arc<Mutex<RgbImage>> = Arc::new(Mutex::new(ImageBuffer::new(width, height)));

    (0..height).into_par_iter().for_each(|y| {
        for x in 0..width {
            let original_lab = get_lab_color(original_img, x, y);
            let averaged_lab = fast_spatial_color_average(
                x,
                y,
                width,
                height,
                config.spatial_averaging_radius,
                integral_image,
            );

            let final_lab = Lab::new(original_lab.l, averaged_lab.a, averaged_lab.b);
            let final_rgb = lab_to_image_rgb(final_lab);
            let blended_rgb = blend_colors(
                final_rgb,
                original_img.get_pixel(x, y).to_rgb(),
                config.blend_factor,
            );

            final_output.lock().unwrap().put_pixel(x, y, blended_rgb);

            update_progress(progress, progress_bar);
        }
    });

    Arc::try_unwrap(final_output).unwrap().into_inner().unwrap()
}

fn get_lab_color(img: &DynamicImage, x: u32, y: u32) -> Lab {
    let pixel = img.get_pixel(x, y);
    let rgb = Srgb::new(
        pixel[0] as f32 / 255.0,
        pixel[1] as f32 / 255.0,
        pixel[2] as f32 / 255.0,
    );
    rgb.into_color()
}

fn memoized_find_closest_color(
    color_map: &Arc<Mutex<HashMap<[u8; 3], Lab>>>,
    pixel: Rgb<u8>,
    colors: &[Lab],
) -> Lab {
    let key = [pixel[0], pixel[1], pixel[2]];

    if let Some(&lab) = color_map.lock().unwrap().get(&key) {
        return lab;
    }

    let original_rgb = Srgb::new(
        pixel[0] as f32 / 255.0,
        pixel[1] as f32 / 255.0,
        pixel[2] as f32 / 255.0,
    );
    let original_lab: Lab = original_rgb.into_color();
    let closest_color = find_closest_color(&original_lab, colors);
    let colorized_lab = Lab::new(original_lab.l, closest_color.a, closest_color.b);

    color_map.lock().unwrap().insert(key, colorized_lab);

    colorized_lab
}

fn find_closest_color<'a>(original: &Lab, colors: &'a [Lab]) -> &'a Lab {
    colors
        .iter()
        .min_by(|&&a, &&b| {
            original
                .improved_difference(a)
                .partial_cmp(&original.improved_difference(b))
                .unwrap()
        })
        .unwrap()
}

fn blend_colors(color1: Rgb<u8>, color2: Rgb<u8>, blend_factor: f32) -> Rgb<u8> {
    Rgb([
        ((color1[0] as f32 * blend_factor + color2[0] as f32 * (1.0 - blend_factor)) as u8)
            .clamp(0, 255),
        ((color1[1] as f32 * blend_factor + color2[1] as f32 * (1.0 - blend_factor)) as u8)
            .clamp(0, 255),
        ((color1[2] as f32 * blend_factor + color2[2] as f32 * (1.0 - blend_factor)) as u8)
            .clamp(0, 255),
    ])
}
