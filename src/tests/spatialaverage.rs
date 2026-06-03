use std::iter;

use wgpu::util::DeviceExt;

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
        .map(|i| WorkingPixel {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            l: i as f32,
            a: (i % width) as f32 * 2.0,
            lab_b: (i / width) as f32 * -3.0,
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

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Spatial Average Bind Group"),
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
                binding: 3,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Staging Buffer"),
        size: horizontal_buffer.size(),
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
        compute_pass.set_bind_group(0, &bind_group, &[]);
        compute_pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
    }

    encoder.copy_buffer_to_buffer(
        &horizontal_buffer,
        0,
        &staging_buffer,
        0,
        staging_buffer.size(),
    );
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
        let expected = cpu_horizontal_average(&input, width, height, radius);

        assert_eq!(output.len(), expected.len());

        for (i, (actual, expected)) in output.iter().zip(expected).enumerate() {
            assert!(
                (actual.r - expected.r).abs() < f32::EPSILON,
                "{}: actual {:?}, expected {:?}",
                i,
                actual,
                expected
            );
            assert!(
                (actual.g - expected.g).abs() < f32::EPSILON,
                "{}: actual {:?}, expected {:?}",
                i,
                actual,
                expected
            );
            assert!(
                (actual.b - expected.b).abs() < f32::EPSILON,
                "{}: actual {:?}, expected {:?}",
                i,
                actual,
                expected
            );
        }
    }

    staging_buffer.unmap();
}

fn cpu_horizontal_average(
    input: &[WorkingPixel],
    width: u32,
    height: u32,
    radius: u32,
) -> Vec<ColorizedPixel> {
    (0..height)
        .flat_map(|y| {
            (0..width).map(move |x| {
                let x1 = x.saturating_sub(radius);
                let x2 = (x + radius).min(width - 1);
                let mut sum = (0.0, 0.0, 0.0);
                let mut count = 0.0;

                for sample_x in x1..=x2 {
                    let sample = input[(y * width + sample_x) as usize];

                    sum.0 += sample.l;
                    sum.1 += sample.a;
                    sum.2 += sample.lab_b;
                    count += 1.0;
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
