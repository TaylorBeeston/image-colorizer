use crate::types::AppConfig;

use anyhow::{Context, Result};
use image::{DynamicImage, GenericImageView, ImageBuffer, Rgb, RgbImage};
use indicatif::ProgressBar;
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
#[derive(Copy, Clone)]
struct Params {
    width: u32,
    height: u32,
    blend_factor: f32,
    dither_amount: f32,
    spatial_radius: u32,
}

unsafe impl bytemuck::Pod for Params {}
unsafe impl bytemuck::Zeroable for Params {}

pub struct GpuColorizer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    color_palette_buffer: wgpu::Buffer,
    pass1_pipeline: wgpu::ComputePipeline,
    horizontal_average_pipeline: wgpu::ComputePipeline,
    vertical_average_pipeline: wgpu::ComputePipeline,
    pass3_pipeline: wgpu::ComputePipeline,
    blend_factor: f32,
    dither_amount: f32,
    spatial_averaging_radius: u32,
}

impl GpuColorizer {
    pub async fn new(config: &AppConfig) -> Result<Self> {
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

        let color_palette: Vec<ColorizedPixel> = config
            .colors
            .iter()
            .map(|lab| ColorizedPixel {
                r: lab.l,
                g: lab.a,
                b: lab.b,
            })
            .collect();

        let color_palette_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Color Palette Buffer"),
            contents: bytemuck::cast_slice(&color_palette),
            usage: wgpu::BufferUsages::STORAGE,
        });

        let pass1_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Colorize Pass 1 Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/colorize_pass1.wgsl").into()),
        });

        let spatial_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Spatial Average Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/spatial_average.wgsl").into()),
        });

        let pass3_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Colorize Pass 3 Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/colorize_pass3.wgsl").into()),
        });

        let pass1_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Colorize Pass 1 Pipeline"),
            layout: None,
            module: &pass1_shader,
            entry_point: "main",
        });

        // A rectangular box average is separable, so the former CPU summed-area table pass
        // is now two GPU passes: average each row, then average those row results by column.
        let horizontal_average_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Horizontal Spatial Average Pipeline"),
                layout: None,
                module: &spatial_shader,
                entry_point: "horizontal",
            });

        let vertical_average_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Vertical Spatial Average Pipeline"),
                layout: None,
                module: &spatial_shader,
                entry_point: "vertical",
            });

        let pass3_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Colorize Pass 3 Pipeline"),
            layout: None,
            module: &pass3_shader,
            entry_point: "main",
        });

        Ok(Self {
            device,
            queue,
            color_palette_buffer,
            pass1_pipeline,
            horizontal_average_pipeline,
            vertical_average_pipeline,
            pass3_pipeline,
            blend_factor: config.blend_factor,
            dither_amount: config.dither_amount,
            spatial_averaging_radius: config.spatial_averaging_radius,
        })
    }

    pub async fn colorize(&self, img: &DynamicImage, pb: &ProgressBar) -> Result<RgbImage> {
        let (width, height) = img.dimensions();

        pb.set_length(4);

        let input_buffer = create_input_buffer(&self.device, img);
        let pass1_buffer = create_output_buffer(&self.device, width, height);
        let horizontal_average_buffer = create_output_buffer(&self.device, width, height);
        let spatial_average_buffer = create_output_buffer(&self.device, width, height);
        let output_buffer = create_output_buffer(&self.device, width, height);
        let staging_buffer = create_staging_buffer(&self.device, width, height);

        let params = Params {
            width,
            height,
            blend_factor: self.blend_factor,
            dither_amount: self.dither_amount,
            spatial_radius: self.spatial_averaging_radius,
        };

        let params_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Params Buffer"),
                contents: bytemuck::cast_slice(&[params]),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        let pass1_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Colorize Pass 1 Bind Group"),
            layout: &self.pass1_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: pass1_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.color_palette_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        });

        let horizontal_average_bind_group =
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Horizontal Spatial Average Bind Group"),
                layout: &self.horizontal_average_pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: pass1_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: horizontal_average_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: params_buffer.as_entire_binding(),
                    },
                ],
            });

        let vertical_average_bind_group =
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Vertical Spatial Average Bind Group"),
                layout: &self.vertical_average_pipeline.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: horizontal_average_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: spatial_average_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: params_buffer.as_entire_binding(),
                    },
                ],
            });

        let pass3_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Colorize Pass 3 Bind Group"),
            layout: &self.pass3_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: pass1_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: spatial_average_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: output_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        dispatch(
            &mut encoder,
            &self.pass1_pipeline,
            &pass1_bind_group,
            width,
            height,
            "Colorize Pass 1",
        );
        pb.inc(1);

        dispatch(
            &mut encoder,
            &self.horizontal_average_pipeline,
            &horizontal_average_bind_group,
            width,
            height,
            "Horizontal Spatial Average",
        );
        pb.inc(1);

        dispatch(
            &mut encoder,
            &self.vertical_average_pipeline,
            &vertical_average_bind_group,
            width,
            height,
            "Vertical Spatial Average",
        );
        pb.inc(1);

        dispatch(
            &mut encoder,
            &self.pass3_pipeline,
            &pass3_bind_group,
            width,
            height,
            "Colorize Pass 3",
        );
        encoder.copy_buffer_to_buffer(&output_buffer, 0, &staging_buffer, 0, output_buffer.size());
        pb.inc(1);

        self.queue.submit(Some(encoder.finish()));

        let result = read_output_buffer(&self.device, &staging_buffer).await?;
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
    }
}

fn dispatch(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    bind_group: &wgpu::BindGroup,
    width: u32,
    height: u32,
    label: &str,
) {
    let mut compute_pass =
        encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(label) });

    compute_pass.set_pipeline(pipeline);
    compute_pass.set_bind_group(0, bind_group, &[]);
    compute_pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
}

async fn read_output_buffer(
    device: &wgpu::Device,
    staging_buffer: &wgpu::Buffer,
) -> Result<Vec<ColorizedPixel>> {
    let buffer_slice = staging_buffer.slice(..);
    let (sender, receiver) = futures::channel::oneshot::channel();

    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("receiver should still exist");
    });

    device.poll(wgpu::Maintain::Wait);
    receiver
        .await
        .context("GPU map callback did not run")?
        .context("Failed to map output buffer")?;

    let data = buffer_slice.get_mapped_range();
    let result = bytemuck::cast_slice(&data).to_vec();

    drop(data);
    staging_buffer.unmap();

    Ok(result)
}

fn create_input_buffer(device: &wgpu::Device, img: &DynamicImage) -> wgpu::Buffer {
    create_rgb_input_buffer(device, &img.to_rgb8())
}

fn create_rgb_input_buffer(device: &wgpu::Device, img: &RgbImage) -> wgpu::Buffer {
    let input_data: Vec<ColorizedPixel> = img
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
