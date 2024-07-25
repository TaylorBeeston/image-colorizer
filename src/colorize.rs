use crate::{types::AppConfig, utils::compute_integral_image};

use anyhow::{Context, Result};
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb, RgbImage};
use indicatif::ProgressBar;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Pixel {
    r: f32,
    g: f32,
    b: f32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorizedPixel {
    r: f32,
    g: f32,
    b: f32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    blend_factor: f32,
    dither_amount: f32,
    spatial_radius: u32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct ScanParams {
    width: u32,
    height: u32,
    is_horizontal: u32,
}

pub async fn colorize(
    img: &DynamicImage,
    config: &AppConfig,
    pb: &ProgressBar,
) -> Result<RgbImage> {
    let (width, height) = img.dimensions();

    pb.set_length((width * height + 2).into());

    // Initialize wgpu
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .context("Failed to find an appropriate adapter")?;

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
        .context("Failed to create device")?;

    let buffer_size = (std::mem::size_of::<ColorizedPixel>() * width as usize * height as usize)
        as wgpu::BufferAddress;

    let input_buffer = create_input_buffer(&device, img);
    let output_buffer1 = create_output_buffer(&device, width, height);
    let staging_buffer = create_staging_buffer(&device, width, height);

    let color_palette: Vec<[f32; 3]> = config
        .colors
        .iter()
        .map(|lab| [lab.l as f32, lab.a as f32, lab.b as f32])
        .collect();
    let color_palette_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Color Palette Buffer"),
        contents: bytemuck::cast_slice(&color_palette),
        usage: wgpu::BufferUsages::STORAGE,
    });

    let params = Params {
        width,
        height,
        blend_factor: config.blend_factor,
        dither_amount: config.dither_amount,
        spatial_radius: config.spatial_averaging_radius,
    };

    let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Params Buffer"),
        contents: bytemuck::cast_slice(&[params]),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    // Load and compile the shaders
    let shader1 = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Colorize Shader 1"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/colorize_pass1.wgsl").into()),
    });

    // Create compute pipelines
    let compute_pipeline1 = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Compute Pipeline 1"),
        layout: None,
        module: &shader1,
        entry_point: "main",
    });

    // Create bind groups
    let bind_group1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Bind Group 1"),
        layout: &compute_pipeline1.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer1.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: color_palette_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    // First GPU pass
    {
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut compute_pass =
                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None });
            compute_pass.set_pipeline(&compute_pipeline1);
            compute_pass.set_bind_group(0, &bind_group1, &[]);
            compute_pass.dispatch_workgroups((width + 15) / 16, (height + 15) / 16, 1);
        }
        encoder.copy_buffer_to_buffer(&output_buffer1, 0, &staging_buffer, 0, buffer_size);
        queue.submit(Some(encoder.finish()));
    }

    pb.inc(1);

    // Read back the result of the first pass
    let buffer_slice = staging_buffer.slice(..);
    let (sender, receiver) = futures::channel::oneshot::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |v| sender.send(v).unwrap());
    device.poll(wgpu::Maintain::Wait);

    if let Ok(()) = receiver.await? {
        let result = read_buffer(&buffer_slice);
        staging_buffer.unmap();

        process_result(&device, &queue, result, width, height, params_buffer, &pb).await
    } else {
        Err(anyhow::anyhow!("Failed to run compute on GPU!"))
    }
}

fn read_buffer(buffer_slice: &wgpu::BufferSlice) -> Vec<Pixel> {
    let data = buffer_slice.get_mapped_range();
    bytemuck::cast_slice(&data).to_vec()
}

async fn process_result(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    result: Vec<Pixel>,
    width: u32,
    height: u32,
    params_buffer: wgpu::Buffer,
    pb: &ProgressBar,
) -> Result<RgbImage> {
    let buffer_size = (std::mem::size_of::<ColorizedPixel>() * width as usize * height as usize)
        as wgpu::BufferAddress;
    let shader2 = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Colorize Shader 3"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/colorize_pass3.wgsl").into()),
    });
    let compute_pipeline2 = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Compute Pipeline 2"),
        layout: None,
        module: &shader2,
        entry_point: "main",
    });
    let output_buffer2 = create_output_buffer(&device, width, height);
    let staging_buffer = create_staging_buffer(&device, width, height);
    // Convert to image for CPU processing
    let mut img = ImageBuffer::new(width, height);
    for (i, pixel) in result.iter().enumerate() {
        let x = i as u32 % width;
        let y = i as u32 / width;
        img.put_pixel(
            x,
            y,
            Rgb([
                (pixel.r * 255.0) as u8,
                (pixel.g * 255.0) as u8,
                (pixel.b * 255.0) as u8,
            ]),
        );
    }

    let input_buffer = create_input_buffer(&device, &img.clone().into());

    // Perform CPU-based spatial averaging
    let spatially_averaged = compute_integral_image(&img, pb);

    let input_data: Vec<ColorizedPixel> = spatially_averaged
        .iter()
        .flatten()
        .map(|&p| ColorizedPixel {
            r: p.0 as f32,
            g: p.1 as f32,
            b: p.2 as f32,
        })
        .collect();

    // Create a new buffer with the spatially averaged result
    let spatially_averaged_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Spatially Averaged Buffer"),
        contents: bytemuck::cast_slice(&input_data),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });

    let bind_group2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Bind Group 2"),
        layout: &compute_pipeline2.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: spatially_averaged_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: output_buffer2.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    // Second GPU pass
    {
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut compute_pass =
                encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None });
            compute_pass.set_pipeline(&compute_pipeline2);
            compute_pass.set_bind_group(0, &bind_group2, &[]);
            compute_pass.dispatch_workgroups((width + 15) / 16, (height + 15) / 16, 1);
        }
        encoder.copy_buffer_to_buffer(&output_buffer2, 0, &staging_buffer, 0, buffer_size);
        queue.submit(Some(encoder.finish()));
    }

    // Read back the final result
    let buffer_slice = staging_buffer.slice(..);
    let (sender, receiver) = futures::channel::oneshot::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |v| sender.send(v).unwrap());
    device.poll(wgpu::Maintain::Wait);

    if let Ok(()) = receiver.await? {
        let result = read_buffer(&buffer_slice);
        staging_buffer.unmap();

        let mut output_image = ImageBuffer::new(width, height);
        for (i, pixel) in result.iter().enumerate() {
            let x = i as u32 % width;
            let y = i as u32 / width;

            output_image.put_pixel(
                x,
                y,
                Rgb([
                    (pixel.r * 255.0) as u8,
                    (pixel.g * 255.0) as u8,
                    (pixel.b * 255.0) as u8,
                ]),
            );
        }

        pb.finish_with_message("Processing complete!");

        Ok(output_image)
    } else {
        Err(anyhow::anyhow!("Failed to run compute on GPU!"))
    }
}

fn create_input_buffer(device: &wgpu::Device, img: &DynamicImage) -> wgpu::Buffer {
    let input_data: Vec<ColorizedPixel> = img
        .to_rgb8()
        .pixels()
        .map(|p| ColorizedPixel {
            r: p[0] as f32 / 255.0,
            g: p[1] as f32 / 255.0,
            b: p[2] as f32 / 255.0,
        })
        .collect();
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Input Buffer"),
        contents: bytemuck::cast_slice(&input_data),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    })
}

fn create_output_buffer(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Buffer {
    let buffer_size = (std::mem::size_of::<ColorizedPixel>() * width as usize * height as usize)
        as wgpu::BufferAddress;
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Output Buffer"),
        size: buffer_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

fn create_staging_buffer(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Buffer {
    let buffer_size = (std::mem::size_of::<ColorizedPixel>() * width as usize * height as usize)
        as wgpu::BufferAddress;
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Staging Buffer"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
