struct ColorizedPixel {
    r: f32,
    g: f32,
    b: f32,
}

struct Params {
    width: u32,
    height: u32,
    blend_factor: f32,
    dither_amount: f32,
    spatial_radius: u32,
}

@group(0) @binding(0) var<storage, read> input : array<ColorizedPixel>;
@group(0) @binding(1) var<storage, write> output : array<ColorizedPixel>;
@group(0) @binding(2) var<uniform> params : Params;

fn quantize_rgb(rgb: vec3<f32>) -> vec3<f32> {
    return floor(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)) * 255.0) / 255.0;
}

fn rgb_to_lab(rgb: vec3<f32>) -> vec3<f32> {
    let xyz = rgb_to_xyz(rgb);
    return xyz_to_lab(xyz);
}

fn rgb_to_xyz(rgb: vec3<f32>) -> vec3<f32> {
    let r = select(rgb.r / 12.92, pow((rgb.r + 0.055) / 1.055, 2.4), rgb.r > 0.04045);
    let g = select(rgb.g / 12.92, pow((rgb.g + 0.055) / 1.055, 2.4), rgb.g > 0.04045);
    let b = select(rgb.b / 12.92, pow((rgb.b + 0.055) / 1.055, 2.4), rgb.b > 0.04045);

    return vec3<f32>(
        r * 0.4124564 + g * 0.3575761 + b * 0.1804375,
        r * 0.2126729 + g * 0.7151522 + b * 0.0721750,
        r * 0.0193339 + g * 0.1191920 + b * 0.9503041,
    );
}

fn xyz_to_lab(xyz: vec3<f32>) -> vec3<f32> {
    let epsilon = 0.008856;
    let kappa = 903.3;

    let xr = xyz.x / 0.950489;
    let yr = xyz.y;
    let zr = xyz.z / 1.088840;

    let fx = select((kappa * xr + 16.0) / 116.0, pow(xr, 1.0 / 3.0), xr > epsilon);
    let fy = select((kappa * yr + 16.0) / 116.0, pow(yr, 1.0 / 3.0), yr > epsilon);
    let fz = select((kappa * zr + 16.0) / 116.0, pow(zr, 1.0 / 3.0), zr > epsilon);

    return vec3<f32>(116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz));
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
        let sample_index = u32(sample_x) + y * params.width;
        let sample = input[sample_index];

        sum = sum + rgb_to_lab(quantize_rgb(vec3<f32>(sample.r, sample.g, sample.b)));
    }

    let average = sum / f32(x2 - x1 + 1);
    let index = x + y * params.width;

    output[index] = ColorizedPixel(average.r, average.g, average.b);
}

@compute @workgroup_size(16, 16, 1)
fn vertical(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;

    if x >= params.width || y >= params.height { return; }

    let radius = i32(params.spatial_radius);
    let y1 = max(i32(y) - radius, 0);
    let y2 = min(i32(y) + radius, i32(params.height) - 1);

    var sum = vec3<f32>(0.0);

    for (var sample_y = y1; sample_y <= y2; sample_y = sample_y + 1) {
        let sample_index = x + u32(sample_y) * params.width;
        let sample = input[sample_index];

        sum = sum + vec3<f32>(sample.r, sample.g, sample.b);
    }

    let average = sum / f32(y2 - y1 + 1);
    let index = x + y * params.width;

    output[index] = ColorizedPixel(average.r, average.g, average.b);
}
