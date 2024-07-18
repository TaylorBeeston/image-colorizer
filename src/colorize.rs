use crate::types::AppConfig;

use anyhow::{Context, Result};
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb, RgbImage};
use indicatif::ProgressBar;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Pixel {
    r: f32,
    g: f32,
    b: f32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
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

pub async fn colorize(img: &DynamicImage, config: &AppConfig, pb: ProgressBar) -> Result<RgbImage> {
    let (width, height) = img.dimensions();

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

    // Create input buffer
    let input_data: Vec<Pixel> = img
        .to_rgb8()
        .pixels()
        .map(|p| Pixel {
            r: p[0] as f32 / 255.0,
            g: p[1] as f32 / 255.0,
            b: p[2] as f32 / 255.0,
        })
        .collect();
    let input_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Input Buffer"),
        contents: bytemuck::cast_slice(&input_data),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    });

    // Create output buffers for each pass
    let buffer_size = (std::mem::size_of::<ColorizedPixel>() * width as usize * height as usize)
        as wgpu::BufferAddress;
    let output_buffer1 = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Output Buffer 1"),
        size: buffer_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let output_buffer2 = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Output Buffer 2"),
        size: buffer_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let output_buffer3 = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Output Buffer 3"),
        size: buffer_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let output_buffer4 = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Output Buffer 4"),
        size: buffer_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    // Create staging buffer for reading results
    let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Staging Buffer"),
        size: buffer_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    // Create color palette buffer
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

    // Create params buffers
    let params = Params {
        width,
        height,
        blend_factor: config.blend_factor,
        dither_amount: config.dither_amount,
        spatial_radius: config.spatial_averaging_radius,
    };
    let horizontal_params = ScanParams {
        width,
        height,
        is_horizontal: 1, // 1 for horizontal
    };
    let vertical_params = ScanParams {
        width: height,    // Swapped for vertical pass
        height: width,    // Swapped for vertical pass
        is_horizontal: 0, // 0 for vertical
    };
    let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Params Buffer"),
        contents: bytemuck::cast_slice(&[params]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let horizontal_params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Horizontal Params Buffer"),
        contents: bytemuck::cast_slice(&[horizontal_params]),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let vertical_params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Vertical Params Buffer"),
        contents: bytemuck::cast_slice(&[vertical_params]),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    // Load and compile the shaders
    let shader1 = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Colorize Shader 1"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/colorize_pass1.wgsl").into()),
    });
    let shader3 = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Colorize Shader 3"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/colorize_pass3.wgsl").into()),
    });
    let scan_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Scan Shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/scan.wgsl").into()),
    });
    let transpose_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Transpose Shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/transpose.wgsl").into()),
    });

    // Create compute pipelines
    let compute_pipeline1 = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Compute Pipeline 1"),
        layout: None,
        module: &shader1,
        entry_point: "main",
    });
    let compute_pipeline3 = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Compute Pipeline 3"),
        layout: None,
        module: &shader3,
        entry_point: "main",
    });
    let scan_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Scan Pipeline"),
        layout: None,
        module: &scan_shader,
        entry_point: "scan",
    });
    let transpose_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Transpose Pipeline"),
        layout: None,
        module: &transpose_shader,
        entry_point: "transpose",
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

    let bind_group3 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Bind Group 3"),
        layout: &compute_pipeline3.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input_buffer.as_entire_binding(), // Changed from output_buffer1
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer3.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: output_buffer4.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let horizontal_scan_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Horizontal Scan Bind Group"),
        layout: &scan_pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer2.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: horizontal_params_buffer.as_entire_binding(),
            },
        ],
    });

    let vertical_scan_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Vertical Scan Bind Group"),
        layout: &scan_pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: output_buffer3.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer2.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: vertical_params_buffer.as_entire_binding(),
            },
        ],
    });

    let transpose_bind_group1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Transpose Bind Group 1"),
        layout: &transpose_pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: output_buffer2.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer3.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: horizontal_params_buffer.as_entire_binding(),
            },
        ],
    });

    let transpose_bind_group2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Transpose Bind Group 2"),
        layout: &transpose_pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: output_buffer2.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer3.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: vertical_params_buffer.as_entire_binding(),
            },
        ],
    });

    // Create command encoder
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Compute Encoder"),
    });

    {
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Compute Pass 1"),
        });
        compute_pass.set_pipeline(&compute_pipeline1);
        compute_pass.set_bind_group(0, &bind_group1, &[]);
        compute_pass.dispatch_workgroups((width + 15) / 16, (height + 15) / 16, 1);
    }

    {
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Horizontal Scan Pass"),
        });
        compute_pass.set_pipeline(&scan_pipeline);
        compute_pass.set_bind_group(0, &horizontal_scan_bind_group, &[]);
        compute_pass.dispatch_workgroups((width + 255) / 256, height, 1);
    }

    {
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("First Transpose Pass"),
        });
        compute_pass.set_pipeline(&transpose_pipeline);
        compute_pass.set_bind_group(0, &transpose_bind_group1, &[]);
        compute_pass.dispatch_workgroups((width + 15) / 16, (height + 15) / 16, 1);
    }

    {
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Vertical Scan Pass"),
        });
        compute_pass.set_pipeline(&scan_pipeline);
        compute_pass.set_bind_group(0, &vertical_scan_bind_group, &[]);
        compute_pass.dispatch_workgroups((height + 255) / 256, width, 1);
    }

    {
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Second Transpose Pass"),
        });
        compute_pass.set_pipeline(&transpose_pipeline);
        compute_pass.set_bind_group(0, &transpose_bind_group2, &[]);
        compute_pass.dispatch_workgroups((height + 15) / 16, (width + 15) / 16, 1);
    }

    {
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Compute Pass 3"),
        });
        compute_pass.set_pipeline(&compute_pipeline3);
        compute_pass.set_bind_group(0, &bind_group3, &[]);
        compute_pass.dispatch_workgroups((width + 15) / 16, (height + 15) / 16, 1);
    }

    // Copy the final output to the staging buffer
    encoder.copy_buffer_to_buffer(&output_buffer4, 0, &staging_buffer, 0, buffer_size);

    // Submit command encoder
    queue.submit(Some(encoder.finish()));

    // Read back the result
    let buffer_slice = staging_buffer.slice(..);
    let (sender, receiver) = futures::channel::oneshot::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |v| sender.send(v).unwrap());
    device.poll(wgpu::Maintain::Wait);

    if let Ok(()) = receiver.await.context("Failed to receive compute result")? {
        let output_image = {
            let data = buffer_slice.get_mapped_range();
            let result: Vec<ColorizedPixel> = bytemuck::cast_slice(&data).to_vec();
            drop(data);
            staging_buffer.unmap();

            // Convert result back to image
            let mut output_image = ImageBuffer::new(width, height);
            for (i, pixel) in result.iter().enumerate() {
                let x = i as u32 % width;
                let y = i as u32 / width;
                let rgb = Rgb([
                    (pixel.r * 255.0) as u8,
                    (pixel.g * 255.0) as u8,
                    (pixel.b * 255.0) as u8,
                ]);
                output_image.put_pixel(x, y, rgb);
            }
            output_image
        };

        pb.finish_with_message("GPU processing complete");
        Ok(output_image)
    } else {
        Err(anyhow::anyhow!("Failed to run compute on GPU!"))
    }
}
