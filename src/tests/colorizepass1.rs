use std::iter;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct Pixel {
    r: f32,
    g: f32,
    b: f32,
}

unsafe impl bytemuck::Pod for Pixel {}
unsafe impl bytemuck::Zeroable for Pixel {}

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
async fn test_colorize_pass1_shader_preserves_input_when_blend_is_zero() {
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
    let height = 4;

    let input_data = (0..width * height)
        .map(|i| Pixel {
            r: i as f32 / 32.0,
            g: 0.25,
            b: 1.0 - i as f32 / 32.0,
        })
        .collect::<Vec<_>>();

    let color_palette = [
        ColorizedPixel {
            r: 53.240_79,
            g: 80.092_46,
            b: 67.203_19,
        },
        ColorizedPixel {
            r: 87.734_726,
            g: -86.182_72,
            b: 83.179_32,
        },
        ColorizedPixel {
            r: 32.297_012,
            g: 79.187_52,
            b: -107.860_16,
        },
    ];

    let input_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Input Buffer"),
        contents: bytemuck::cast_slice(&input_data),
        usage: wgpu::BufferUsages::STORAGE,
    });

    let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Output Buffer"),
        size: (input_data.len() * std::mem::size_of::<ColorizedPixel>()) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });

    let color_palette_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Color Palette Buffer"),
        contents: bytemuck::cast_slice(&color_palette),
        usage: wgpu::BufferUsages::STORAGE,
    });

    let params = Params {
        width,
        height,
        blend_factor: 0.0,
        dither_amount: 0.0,
        spatial_radius: 1,
    };

    let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Params Buffer"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("Colorize Pass 1 Shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/colorize_pass1.wgsl").into()),
    });

    let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("Colorize Pass 1 Pipeline"),
        layout: None,
        module: &shader_module,
        entry_point: "main",
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Colorize Pass 1 Bind Group"),
        layout: &compute_pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer.as_entire_binding(),
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

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Command Encoder"),
    });

    {
        let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Compute Pass"),
        });

        compute_pass.set_pipeline(&compute_pipeline);
        compute_pass.set_bind_group(0, &bind_group, &[]);
        compute_pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
    }

    queue.submit(iter::once(encoder.finish()));

    let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Staging Buffer"),
        size: output_buffer.size(),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Command Encoder for Copy"),
    });

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

        assert_eq!(output.len(), input_data.len());

        for (input, output) in input_data.iter().zip(output) {
            assert!((input.r - output.r).abs() < f32::EPSILON);
            assert!((input.g - output.g).abs() < f32::EPSILON);
            assert!((input.b - output.b).abs() < f32::EPSILON);
        }
    }

    staging_buffer.unmap();
}
