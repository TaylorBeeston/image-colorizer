use image::{DynamicImage, ImageBuffer, Rgb};
use indicatif::ProgressBar;
use palette::Lab;

use crate::colorize::GpuColorizer;
use crate::types::AppConfig;

#[tokio::test]
async fn test_colorize_pipeline_preserves_input_when_blend_is_zero() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
    if instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .is_none()
    {
        eprintln!("Skipping GPU pipeline test: no WebGPU adapter available");
        return;
    }

    let input = ImageBuffer::from_fn(2, 2, |x, y| match (x, y) {
        (0, 0) => Rgb([0, 0, 0]),
        (1, 0) => Rgb([255, 0, 0]),
        (0, 1) => Rgb([0, 255, 0]),
        _ => Rgb([255, 255, 255]),
    });

    let config = AppConfig {
        input_output_pairs: Vec::new(),
        blend_factor: 0.0,
        colors: vec![Lab::new(0.0, 0.0, 0.0), Lab::new(100.0, 0.0, 0.0)],
        dither_amount: 0.0,
        spatial_averaging_radius: 1,
    };

    let colorizer = GpuColorizer::new(&config)
        .await
        .expect("GPU colorizer should initialize");

    let output = colorizer
        .colorize(
            &DynamicImage::ImageRgb8(input.clone()),
            &ProgressBar::hidden(),
        )
        .await
        .expect("GPU colorize pipeline should complete");

    assert_eq!(output, input);

    let second_input = ImageBuffer::from_fn(3, 1, |x, _| match x {
        0 => Rgb([16, 32, 64]),
        1 => Rgb([128, 96, 48]),
        _ => Rgb([240, 224, 208]),
    });

    let second_output = colorizer
        .colorize(
            &DynamicImage::ImageRgb8(second_input.clone()),
            &ProgressBar::hidden(),
        )
        .await
        .expect("reused GPU colorizer should complete");

    assert_eq!(second_output, second_input);
}
