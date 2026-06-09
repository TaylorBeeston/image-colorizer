use crate::types::ColorizerConfig;

use anyhow::{bail, Context, Result};
use image::{DynamicImage, GenericImageView};
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
struct WorkingPixel {
    r: f32,
    g: f32,
    b: f32,
    l: f32,
    a: f32,
    lab_b: f32,
}

unsafe impl bytemuck::Pod for WorkingPixel {}
unsafe impl bytemuck::Zeroable for WorkingPixel {}

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

/// A completed colorization stage.
///
/// This is useful for CLI or UI progress reporting without coupling the library
/// to a specific progress-bar implementation.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ColorizeStage {
    PaletteMatch,
    HorizontalSpatialAverage,
    VerticalSpatialAverageAndFinalColor,
}

impl ColorizeStage {
    pub const COUNT: u64 = 3;
}

/// RGB8 image bytes produced by [`GpuColorizer`].
///
/// `data` is tightly packed RGB: `width * height * 3` bytes.
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
    horizontal_bind_group: wgpu::BindGroup,
    vertical_final_bind_group: wgpu::BindGroup,
    params_buffer: wgpu::Buffer,
}

/// Reusable WebGPU image colorizer.
///
/// Creating a colorizer initializes the GPU device, queue, shaders, pipelines,
/// palette buffer, and reusable scratch buffers. Reuse one value for a batch
/// instead of constructing one per image.
pub struct GpuColorizer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    color_palette_buffer: wgpu::Buffer,
    pass1_pipeline: wgpu::ComputePipeline,
    horizontal_average_pipeline: wgpu::ComputePipeline,
    vertical_final_pipeline: wgpu::ComputePipeline,
    blend_factor: f32,
    dither_amount: f32,
    spatial_averaging_radius: u32,
    frame_buffers: Option<FrameBuffers>,
    input_data: Vec<ColorizedPixel>,
    output_buffers: Vec<Vec<u8>>,
}

impl GpuColorizer {
    pub async fn new(config: &ColorizerConfig) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .context("Failed to find an appropriate adapter")?;

        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: None,
                    features: wgpu::Features::empty(),
                    limits,
                },
                None,
            )
            .await
            .context("Failed to create device")?;

        let color_palette_buffer = create_color_palette_buffer(&device, config);

        let pass1_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Colorize Pass 1 Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/colorize_pass1.wgsl").into()),
        });

        let spatial_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Spatial Average Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/spatial_average.wgsl").into()),
        });

        let pass1_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Colorize Pass 1 Pipeline"),
            layout: None,
            module: &pass1_shader,
            entry_point: "main",
        });

        let horizontal_average_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Horizontal Spatial Average Pipeline"),
                layout: None,
                module: &spatial_shader,
                entry_point: "horizontal",
            });

        let vertical_final_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Vertical Spatial Average and Final Color Pipeline"),
                layout: None,
                module: &spatial_shader,
                entry_point: "vertical_final",
            });

        Ok(Self {
            device,
            queue,
            color_palette_buffer,
            pass1_pipeline,
            horizontal_average_pipeline,
            vertical_final_pipeline,
            blend_factor: config.blend_factor,
            dither_amount: config.dither_amount,
            spatial_averaging_radius: config.spatial_averaging_radius,
            frame_buffers: None,
            input_data: Vec::new(),
            output_buffers: Vec::new(),
        })
    }

    /// Update colorization parameters and palette without recreating the GPU device or pipelines.
    ///
    /// Existing image-sized scratch buffers are recreated on the next render because pass-one bind
    /// groups reference the palette buffer.
    pub fn update_config(&mut self, config: &ColorizerConfig) {
        self.color_palette_buffer = create_color_palette_buffer(&self.device, config);
        self.blend_factor = config.blend_factor;
        self.dither_amount = config.dither_amount;
        self.spatial_averaging_radius = config.spatial_averaging_radius;
        self.frame_buffers = None;
    }

    /// Update scalar parameters without recreating GPU buffers or bind groups.
    pub fn update_parameters(
        &mut self,
        blend_factor: f32,
        dither_amount: f32,
        spatial_averaging_radius: u32,
    ) {
        self.blend_factor = blend_factor;
        self.dither_amount = dither_amount;
        self.spatial_averaging_radius = spatial_averaging_radius;
    }

    pub fn max_colorizable_pixels(&self) -> u64 {
        let limits = self.device.limits();
        let storage_limit = limits
            .max_buffer_size
            .min(limits.max_storage_buffer_binding_size as wgpu::BufferAddress);

        storage_limit / std::mem::size_of::<WorkingPixel>() as u64
    }

    /// Colorize an image.
    pub async fn colorize(&mut self, img: &DynamicImage) -> Result<RenderedImage> {
        self.colorize_with_progress(img, |_| {}).await
    }

    /// Colorize an image and report completed GPU stages.
    pub async fn colorize_with_progress(
        &mut self,
        img: &DynamicImage,
        mut progress: impl FnMut(ColorizeStage),
    ) -> Result<RenderedImage> {
        let (width, height) = img.dimensions();

        self.ensure_frame_buffers(width, height)?;
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
        progress(ColorizeStage::PaletteMatch);

        dispatch(
            &mut encoder,
            &self.horizontal_average_pipeline,
            &buffers.horizontal_bind_group,
            width,
            height,
            "Horizontal Spatial Average",
        );
        progress(ColorizeStage::HorizontalSpatialAverage);

        dispatch(
            &mut encoder,
            &self.vertical_final_pipeline,
            &buffers.vertical_final_bind_group,
            width,
            height,
            "Vertical Spatial Average and Final Color",
        );
        encoder.copy_buffer_to_buffer(
            &buffers.output_buffer,
            0,
            &buffers.staging_buffer,
            0,
            buffers.output_buffer.size(),
        );
        progress(ColorizeStage::VerticalSpatialAverageAndFinalColor);

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

    fn ensure_frame_buffers(&mut self, width: u32, height: u32) -> Result<()> {
        if self
            .frame_buffers
            .as_ref()
            .is_some_and(|buffers| buffers.width == width && buffers.height == height)
        {
            return Ok(());
        }

        self.frame_buffers = Some(FrameBuffers::new(self, width, height)?);

        Ok(())
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
    fn new(colorizer: &GpuColorizer, width: u32, height: u32) -> Result<Self> {
        validate_frame_buffers(&colorizer.device, width, height)?;
        let input_buffer = create_storage_buffer::<ColorizedPixel>(
            &colorizer.device,
            width,
            height,
            wgpu::BufferUsages::COPY_DST,
            "Input Buffer",
        );
        let pass1_buffer = create_storage_buffer::<WorkingPixel>(
            &colorizer.device,
            width,
            height,
            wgpu::BufferUsages::empty(),
            "Pass 1 Buffer",
        );
        let horizontal_average_buffer = create_storage_buffer::<ColorizedPixel>(
            &colorizer.device,
            width,
            height,
            wgpu::BufferUsages::empty(),
            "Horizontal Average Buffer",
        );
        let output_buffer = create_storage_buffer::<u32>(
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

        let horizontal_bind_group =
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
                            binding: 3,
                            resource: params_buffer.as_entire_binding(),
                        },
                    ],
                });

        let vertical_final_bind_group =
            colorizer
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Vertical Spatial Average and Final Color Bind Group"),
                    layout: &colorizer.vertical_final_pipeline.get_bind_group_layout(0),
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
                            resource: output_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: params_buffer.as_entire_binding(),
                        },
                    ],
                });

        Ok(Self {
            width,
            height,
            input_buffer,
            output_buffer,
            staging_buffer,
            pass1_bind_group,
            horizontal_bind_group,
            vertical_final_bind_group,
            params_buffer,
        })
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
    let result = bytemuck::cast_slice::<_, u32>(&data);

    output_data.clear();
    output_data.resize(width as usize * height as usize * 3, 0);

    for (pixel, output) in result.iter().zip(output_data.chunks_exact_mut(3)) {
        output[0] = (pixel & 0xff) as u8;
        output[1] = ((pixel >> 8) & 0xff) as u8;
        output[2] = ((pixel >> 16) & 0xff) as u8;
    }

    drop(data);
    staging_buffer.unmap();

    Ok(())
}

fn create_color_palette_buffer(device: &wgpu::Device, config: &ColorizerConfig) -> wgpu::Buffer {
    let color_palette: Vec<ColorizedPixel> = config
        .colors
        .iter()
        .map(|lab| ColorizedPixel {
            r: lab.l,
            g: lab.a,
            b: lab.b,
        })
        .collect();

    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Color Palette Buffer"),
        contents: bytemuck::cast_slice(&color_palette),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

fn validate_frame_buffers(device: &wgpu::Device, width: u32, height: u32) -> Result<()> {
    let limits = device.limits();
    let max_buffer_size = limits.max_buffer_size;
    let max_storage_binding_size = limits.max_storage_buffer_binding_size as wgpu::BufferAddress;
    let storage_limit = max_buffer_size.min(max_storage_binding_size);
    let largest_storage_buffer = frame_buffer_size::<WorkingPixel>(width, height);
    let staging_buffer = frame_buffer_size::<u32>(width, height);

    if largest_storage_buffer > storage_limit {
        bail!(
            "Image dimensions {}x{} need a {} byte GPU storage binding, but this device only allows {} bytes. Resize the image smaller or use a GPU with a larger storage-buffer binding limit.",
            width,
            height,
            largest_storage_buffer,
            storage_limit
        );
    }

    if staging_buffer > max_buffer_size {
        bail!(
            "Image dimensions {}x{} need a {} byte GPU readback buffer, but this device only allows {} bytes. Resize the image smaller or use a GPU with a larger buffer limit.",
            width,
            height,
            staging_buffer,
            max_buffer_size
        );
    }

    Ok(())
}

fn create_storage_buffer<T>(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    usage: wgpu::BufferUsages,
    label: &str,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: frame_buffer_size::<T>(width, height),
        usage: wgpu::BufferUsages::STORAGE | usage,
        mapped_at_creation: false,
    })
}

fn create_staging_buffer(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Staging Buffer"),
        size: frame_buffer_size::<u32>(width, height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn frame_buffer_size<T>(width: u32, height: u32) -> wgpu::BufferAddress {
    (std::mem::size_of::<T>() * width as usize * height as usize) as wgpu::BufferAddress
}
