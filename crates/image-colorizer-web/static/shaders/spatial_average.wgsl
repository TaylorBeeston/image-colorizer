struct WorkingPixel {
    rgb_l: vec4<f32>,
    ab: vec4<f32>,
}


struct Params {
    width: u32,
    height: u32,
    blend_factor: f32,
    dither_amount: f32,
    spatial_radius: u32,
}

@group(0) @binding(0) var<storage, read> working_input : array<WorkingPixel>;
@group(0) @binding(1) var<storage, read_write> horizontal_average : array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> final_output : array<u32>;
@group(0) @binding(3) var<uniform> params : Params;

fn clamp_color(color: vec3<f32>) -> vec3<f32> {
    return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn pack_rgb(color: vec3<f32>) -> u32 {
    let bytes = vec3<u32>(clamp_color(color) * 255.0);

    return bytes.r | (bytes.g << 8u) | (bytes.b << 16u);
}

fn lab_to_rgb(lab: vec3<f32>) -> vec3<f32> {
    let xyz = lab_to_xyz(lab);
    return xyz_to_rgb(xyz);
}

fn lab_to_xyz(lab: vec3<f32>) -> vec3<f32> {
    let fy = (lab.x + 16.0) / 116.0;
    let fx = lab.y / 500.0 + fy;
    let fz = fy - lab.z / 200.0;

    let epsilon = 0.008856;
    let kappa = 903.3;

    let fx3 = fx * fx * fx;
    let fz3 = fz * fz * fz;

    let xr = select((116.0 * fx - 16.0) / kappa, fx3, fx3 > epsilon);
    let yr = select(lab.x / kappa, fy * fy * fy, lab.x > kappa * epsilon);
    let zr = select((116.0 * fz - 16.0) / kappa, fz3, fz3 > epsilon);

    return vec3<f32>(xr * 0.950489, yr, zr * 1.088840);
}

fn linear_to_srgb(channel: f32) -> f32 {
    if channel > 0.0031308 {
        return 1.055 * pow(channel, 1.0 / 2.4) - 0.055;
    }

    return 12.92 * channel;
}

fn xyz_to_rgb(xyz: vec3<f32>) -> vec3<f32> {
    let r = xyz.x * 3.2404542 + xyz.y * -1.5371385 + xyz.z * -0.4985314;
    let g = xyz.x * -0.9692660 + xyz.y * 1.8760108 + xyz.z * 0.0415560;
    let b = xyz.x * 0.0556434 + xyz.y * -0.2040259 + xyz.z * 1.0572252;

    return clamp(
        vec3<f32>(linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(b)),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}

@compute @workgroup_size(16, 16, 1)
fn horizontal(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;

    if x >= params.width || y >= params.height { return; }

    let radius = i32(params.spatial_radius);
    let x1 = max(i32(x) - radius, 0);
    let x2 = min(i32(x) + radius, i32(params.width) - 1);

    var sum = vec3<f32>(0.0);

    for (var sample_x = x1; sample_x <= x2; sample_x = sample_x + 1) {
        let sample = working_input[u32(sample_x) + y * params.width];

        sum = sum + vec3<f32>(sample.rgb_l.w, sample.ab.x, sample.ab.y);
    }

    let average = sum / f32(x2 - x1 + 1);
    let index = x + y * params.width;

    horizontal_average[index] = vec4<f32>(average, 0.0);
}

@compute @workgroup_size(16, 16, 1)
fn vertical_final(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;

    if x >= params.width || y >= params.height { return; }

    let radius = i32(params.spatial_radius);
    let y1 = max(i32(y) - radius, 0);
    let y2 = min(i32(y) + radius, i32(params.height) - 1);

    var sum = vec3<f32>(0.0);

    for (var sample_y = y1; sample_y <= y2; sample_y = sample_y + 1) {
        let sample = horizontal_average[x + u32(sample_y) * params.width];

        sum = sum + sample.rgb;
    }

    let average_lab = sum / f32(y2 - y1 + 1);
    let index = x + y * params.width;
    let input = working_input[index];
    let input_color = input.rgb_l.rgb;
    let luminance_transferred_lab = vec3<f32>(input.rgb_l.w, average_lab.g, average_lab.b);
    let luminance_transferred_rgb = lab_to_rgb(luminance_transferred_lab);
    let final_color = clamp_color(mix(input_color, luminance_transferred_rgb, params.blend_factor));

    final_output[index] = pack_rgb(final_color);
}
