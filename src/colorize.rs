use crate::types::AppConfig;

use anyhow::{Context, Result};
use image::{DynamicImage, GenericImageView};
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

pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

struct FrameBuffers {
    width: u32,
    height: u32,
    input_buffer: wgpu::Buffer,
    output_buffer: wgpu::Buffer,
    staging_buffer: wgpu::Buffer,
    pass1_bind_group: wgpu::BindGroup,
    horizontal_average_bind_group: wgpu::BindGroup,
    vertical_average_bind_group: wgpu::BindGroup,
    pass3_bind_group: wgpu::BindGroup,
    params_buffer: wgpu::Buffer,
}

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
    frame_buffers: Option<FrameBuffers>,
    input_data: Vec<ColorizedPixel>,
    output_buffers: Vec<Vec<u8>>,
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
            frame_buffers: None,
            input_data: Vec::new(),
            output_buffers: Vec::new(),
        })
    }

    pub async fn colorize(
        &mut self,
        img: &DynamicImage,
        pb: &ProgressBar,
    ) -> Result<RenderedImage> {
        let (width, height) = img.dimensions();

        pb.set_length(4);

        self.ensure_frame_buffers(width, height);
        self.upload_input(img);

        let params = Params {
            width,
            height,
            blend_factor: self.blend_factor,
            dither_amount: self.dither_amount,
            spatial_radius: self.spatial_averaging_radius,
        };

        let buffers = self
            .frame_buffers
            .as_ref()
            .expect("frame buffers should exist");

        self.queue
            .write_buffer(&buffers.params_buffer, 0, bytemuck::bytes_of(&params));

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        dispatch(
            &mut encoder,
            &self.pass1_pipeline,
            &buffers.pass1_bind_group,
            width,
            height,
            "Colorize Pass 1",
        );
        pb.inc(1);

        dispatch(
            &mut encoder,
            &self.horizontal_average_pipeline,
            &buffers.horizontal_average_bind_group,
            width,
            height,
            "Horizontal Spatial Average",
        );
        pb.inc(1);

        dispatch(
            &mut encoder,
            &self.vertical_average_pipeline,
            &buffers.vertical_average_bind_group,
            width,
            height,
            "Vertical Spatial Average",
        );
        pb.inc(1);

        dispatch(
            &mut encoder,
            &self.pass3_pipeline,
            &buffers.pass3_bind_group,
            width,
            height,
            "Colorize Pass 3",
        );
        encoder.copy_buffer_to_buffer(
            &buffers.output_buffer,
            0,
            &buffers.staging_buffer,
            0,
            buffers.output_buffer.size(),
        );
        pb.inc(1);

        self.queue.submit(Some(encoder.finish()));

        let mut output_data = self.output_buffers.pop().unwrap_or_default();

        read_output_buffer(
            &self.device,
            &buffers.staging_buffer,
            width,
            height,
            &mut output_data,
        )
        .await?;

        Ok(RenderedImage {
            width,
            height,
            data: output_data,
        })
    }

    pub fn recycle_output_buffer(&mut self, mut data: Vec<u8>) {
        data.clear();
        self.output_buffers.push(data);
    }

    fn ensure_frame_buffers(&mut self, width: u32, height: u32) {
        if self
            .frame_buffers
            .as_ref()
            .is_some_and(|buffers| buffers.width == width && buffers.height == height)
        {
            return;
        }

        self.frame_buffers = Some(FrameBuffers::new(self, width, height));
    }

    fn upload_input(&mut self, img: &DynamicImage) {
        let rgb_storage;
        let rgb = if let Some(rgb) = img.as_rgb8() {
            rgb
        } else {
            rgb_storage = img.to_rgb8();
            &rgb_storage
        };

        self.input_data.clear();
        self.input_data
            .reserve(rgb.width() as usize * rgb.height() as usize);

        self.input_data.extend(rgb.pixels().map(|p| ColorizedPixel {
            r: p[0] as f32 / 255.0,
            g: p[1] as f32 / 255.0,
            b: p[2] as f32 / 255.0,
        }));

        let buffers = self
            .frame_buffers
            .as_ref()
            .expect("frame buffers should exist");

        self.queue.write_buffer(
            &buffers.input_buffer,
            0,
            bytemuck::cast_slice(&self.input_data),
        );
    }
}

impl FrameBuffers {
    fn new(colorizer: &GpuColorizer, width: u32, height: u32) -> Self {
        let input_buffer = create_storage_buffer(
            &colorizer.device,
            width,
            height,
            wgpu::BufferUsages::COPY_DST,
            "Input Buffer",
        );
        let pass1_buffer = create_storage_buffer(
            &colorizer.device,
            width,
            height,
            wgpu::BufferUsages::empty(),
            "Pass 1 Buffer",
        );
        let horizontal_average_buffer = create_storage_buffer(
            &colorizer.device,
            width,
            height,
            wgpu::BufferUsages::empty(),
            "Horizontal Average Buffer",
        );
        let spatial_average_buffer = create_storage_buffer(
            &colorizer.device,
            width,
            height,
            wgpu::BufferUsages::empty(),
            "Spatial Average Buffer",
        );
        let output_buffer = create_storage_buffer(
            &colorizer.device,
            width,
            height,
            wgpu::BufferUsages::COPY_SRC,
            "Output Buffer",
        );
        let staging_buffer = create_staging_buffer(&colorizer.device, width, height);
        let params_buffer = colorizer.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Params Buffer"),
            size: std::mem::size_of::<Params>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let pass1_bind_group = colorizer
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Colorize Pass 1 Bind Group"),
                layout: &colorizer.pass1_pipeline.get_bind_group_layout(0),
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
                        resource: colorizer.color_palette_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: params_buffer.as_entire_binding(),
                    },
                ],
            });

        let horizontal_average_bind_group =
            colorizer
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Horizontal Spatial Average Bind Group"),
                    layout: &colorizer
                        .horizontal_average_pipeline
                        .get_bind_group_layout(0),
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
            colorizer
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Vertical Spatial Average Bind Group"),
                    layout: &colorizer.vertical_average_pipeline.get_bind_group_layout(0),
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

        let pass3_bind_group = colorizer
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Colorize Pass 3 Bind Group"),
                layout: &colorizer.pass3_pipeline.get_bind_group_layout(0),
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

        Self {
            width,
            height,
            input_buffer,
            output_buffer,
            staging_buffer,
            pass1_bind_group,
            horizontal_average_bind_group,
            vertical_average_bind_group,
            pass3_bind_group,
            params_buffer,
        }
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
    width: u32,
    height: u32,
    output_data: &mut Vec<u8>,
) -> Result<()> {
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
    let result = bytemuck::cast_slice::<_, ColorizedPixel>(&data);

    output_data.clear();
    output_data.resize(width as usize * height as usize * 3, 0);

    for (pixel, output) in result.iter().zip(output_data.chunks_exact_mut(3)) {
        output[0] = (pixel.r * 255.0) as u8;
        output[1] = (pixel.g * 255.0) as u8;
        output[2] = (pixel.b * 255.0) as u8;
    }

    drop(data);
    staging_buffer.unmap();

    Ok(())
}

fn create_storage_buffer(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    usage: wgpu::BufferUsages,
    label: &str,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: frame_buffer_size(width, height),
        usage: wgpu::BufferUsages::STORAGE | usage,
        mapped_at_creation: false,
    })
}

fn create_staging_buffer(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Staging Buffer"),
        size: frame_buffer_size(width, height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn frame_buffer_size(width: u32, height: u32) -> wgpu::BufferAddress {
    (std::mem::size_of::<ColorizedPixel>() * width as usize * height as usize)
        as wgpu::BufferAddress
}
