use std::iter;

use palette::{IntoColor, Lab, Srgb};
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct ColorizedPixel {
    r: f32,
    g: f32,
    b: f32,
}

unsafe impl bytemuck::Pod for ColorizedPixel {}
unsafe impl bytemuck::Zeroable for ColorizedPixel {}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct Params {
    width: u32,
    height: u32,
    blend_factor: f32,
    dither_amount: f32,
    spatial_radius: u32,
}

unsafe impl bytemuck::Pod for Params {}
unsafe impl bytemuck::Zeroable for Params {}

#[tokio::test]
async fn test_spatial_average_shader_matches_cpu_reference() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
    let Some(adapter) = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
    else {
        eprintln!("Skipping GPU shader test: no WebGPU adapter available");
        return;
    };

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: None,
                features: wgpu::Features::empty(),
                limits: wgpu::Limits::default(),
            },
            None,
        )
        .await
        .expect("failed to create GPU device");

    let width = 4;
    let height = 3;
    let radius = 1;
    let input = (0..width * height)
        .map(|i| ColorizedPixel {
            r: (i % width) as f32 / (width - 1) as f32,
            g: (i / width) as f32 / (height - 1) as f32,
            b: i as f32 / (width * height - 1) as f32,
        })
        .collect::<Vec<_>>();

    let input_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Input Buffer"),
        contents: bytemuck::cast_slice(&input),
        usage: wgpu::BufferUsages::STORAGE,
    });

    let horizontal_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Horizontal Average Buffer"),
        size: (input.len() * std::mem::size_of::<ColorizedPixel>()) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });

    let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Output Buffer"),
        size: horizontal_buffer.size(),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    let params = Params {
        width,
        height,
        blend_factor: 0.0,
        dither_amount: 0.0,
        spatial_radius: radius,
    };

    let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Params Buffer"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Spatial Average Shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/spatial_average.wgsl").into()),
    });

    let horizontal_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Horizontal Average Pipeline"),
        layout: None,
        module: &shader_module,
        entry_point: "horizontal",
    });

    let vertical_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Vertical Average Pipeline"),
        layout: None,
        module: &shader_module,
        entry_point: "vertical",
    });

    let horizontal_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Horizontal Average Bind Group"),
        layout: &horizontal_pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: horizontal_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let vertical_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Vertical Average Bind Group"),
        layout: &vertical_pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: horizontal_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Staging Buffer"),
        size: output_buffer.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Command Encoder"),
    });

    {
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Horizontal Average Pass"),
        });

        compute_pass.set_pipeline(&horizontal_pipeline);
        compute_pass.set_bind_group(0, &horizontal_bind_group, &[]);
        compute_pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
    }

    {
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Vertical Average Pass"),
        });

        compute_pass.set_pipeline(&vertical_pipeline);
        compute_pass.set_bind_group(0, &vertical_bind_group, &[]);
        compute_pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
    }

    encoder.copy_buffer_to_buffer(&output_buffer, 0, &staging_buffer, 0, staging_buffer.size());
    queue.submit(iter::once(encoder.finish()));

    let buffer_slice = staging_buffer.slice(..);
    let (sender, receiver) = futures::channel::oneshot::channel();

    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("receiver should still exist");
    });

    device.poll(wgpu::Maintain::Wait);
    receiver
        .await
        .expect("GPU map callback should run")
        .expect("staging buffer should map");

    {
        let data = buffer_slice.get_mapped_range();
        let output = bytemuck::cast_slice::<_, ColorizedPixel>(&data);
        let expected = cpu_spatial_average(&input, width, height, radius);
        let epsilon = 0.01;

        assert_eq!(output.len(), expected.len());

        for (i, (actual, expected)) in output.iter().zip(expected).enumerate() {
            assert!(
                (actual.r - expected.r).abs() < epsilon,
                "{}: actual {:?}, expected {:?}",
                i,
                actual,
                expected
            );
            assert!(
                (actual.g - expected.g).abs() < epsilon,
                "{}: actual {:?}, expected {:?}",
                i,
                actual,
                expected
            );
            assert!(
                (actual.b - expected.b).abs() < epsilon,
                "{}: actual {:?}, expected {:?}",
                i,
                actual,
                expected
            );
        }
    }

    staging_buffer.unmap();
}

fn cpu_spatial_average(
    input: &[ColorizedPixel],
    width: u32,
    height: u32,
    radius: u32,
) -> Vec<ColorizedPixel> {
    (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                let x1 = x.saturating_sub(radius);
                let y1 = y.saturating_sub(radius);
                let x2 = (x + radius).min(width - 1);
                let y2 = (y + radius).min(height - 1);
                let mut sum = (0.0, 0.0, 0.0);
                let mut count = 0.0;

                for sample_y in y1..=y2 {
                    for sample_x in x1..=x2 {
                        let sample = input[(sample_y * width + sample_x) as usize];
                        let lab: Lab = Srgb::new(
                            quantize_channel(sample.r),
                            quantize_channel(sample.g),
                            quantize_channel(sample.b),
                        )
                        .into_color();

                        sum.0 += lab.l;
                        sum.1 += lab.a;
                        sum.2 += lab.b;
                        count += 1.0;
                    }
                }

                ColorizedPixel {
                    r: sum.0 / count,
                    g: sum.1 / count,
                    b: sum.2 / count,
                }
            })
        })
        .collect()
}

fn quantize_channel(value: f32) -> f32 {
    (value.clamp(0.0, 1.0) * 255.0).floor() / 255.0
}
