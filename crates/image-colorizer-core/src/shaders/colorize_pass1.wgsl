struct Pixel {
    r: f32,
    g: f32,
    b: f32,
}

struct ColorizedPixel {
    r: f32,
    g: f32,
    b: f32,
}

struct WorkingPixel {
    r: f32,
    g: f32,
    b: f32,
    l: f32,
    a: f32,
    lab_b: f32,
}

struct Params {
    width: u32,
    height: u32,
    blend_factor: f32,
    dither_amount: f32,
    spatial_radius: u32,
}

@group(0) @binding(0) var<storage, read> input : array<Pixel>;
@group(0) @binding(1) var<storage, write> output : array<WorkingPixel>;
@group(0) @binding(2) var<storage, read> color_palette : array<ColorizedPixel>;
@group(0) @binding(3) var<uniform> params : Params;

fn clamp_color(color: vec3<f32>) -> vec3<f32> {
    return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn quantize_rgb(rgb: vec3<f32>) -> vec3<f32> {
    return floor(clamp_color(rgb) * 255.0) / 255.0;
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

fn xyz_to_rgb(xyz: vec3<f32>) -> vec3<f32> {
    let r = xyz.x * 3.2404542 + xyz.y * -1.5371385 + xyz.z * -0.4985314;
    let g = xyz.x * -0.9692660 + xyz.y * 1.8760108 + xyz.z * 0.0415560;
    let b = xyz.x * 0.0556434 + xyz.y * -0.2040259 + xyz.z * 1.0572252;

    let r1 = select(12.92 * r, 1.055 * pow(r, 1.0 / 2.4) - 0.055, r > 0.0031308);
    let g1 = select(12.92 * g, 1.055 * pow(g, 1.0 / 2.4) - 0.055, g > 0.0031308);
    let b1 = select(12.92 * b, 1.055 * pow(b, 1.0 / 2.4) - 0.055, b > 0.0031308);

    return vec3<f32>(clamp(r1, 0.0, 1.0), clamp(g1, 0.0, 1.0), clamp(b1, 0.0, 1.0));
}

fn color_palette_value(index: u32) -> vec3<f32> {
    return vec3<f32>(color_palette[index].r, color_palette[index].g, color_palette[index].b);
}

fn chroma_distance(left: vec3<f32>, right: vec3<f32>) -> f32 {
    return distance(left.yz, right.yz);
}

fn find_closest_color(lab: vec3<f32>) -> vec3<f32> {
    var closest_color = color_palette_value(0u);
    var min_distance = chroma_distance(lab, closest_color);

    for (var i = 1u; i < arrayLength(&color_palette); i = i + 1u) {
        let current_color = color_palette_value(i);
        let current_distance = chroma_distance(lab, current_color);

        if current_distance < min_distance {
            min_distance = current_distance;
            closest_color = current_color;
        }
    }

    return closest_color;
}

fn apply_dithering(color: vec3<f32>, target_color: vec3<f32>, amount: f32, global_id: vec3<u32>) -> vec3<f32> {
    let rand = fract(sin(dot(vec2<f32>(f32(global_id.x), f32(global_id.y)), vec2<f32>(12.9898, 78.233))) * 43758.5453);
    return color + (target_color - color) * amount * rand;
}

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    if global_id.x >= params.width || global_id.y >= params.height { return; }

    let index = global_id.x + global_id.y * params.width;
    let input_color = vec3<f32>(input[index].r, input[index].g, input[index].b);
    let lab_color = rgb_to_lab(input_color);
    let closest_color = find_closest_color(lab_color);
    let final_lab = vec3<f32>(lab_color.x, closest_color.y, closest_color.z);
    let dithered_lab = apply_dithering(final_lab, lab_color, params.dither_amount, global_id);
    let final_rgb = lab_to_rgb(dithered_lab);
    let quantized_rgb = quantize_rgb(mix(input_color, final_rgb, params.blend_factor));
    let quantized_lab = rgb_to_lab(quantized_rgb);

    output[index] = WorkingPixel(
        quantized_rgb.r,
        quantized_rgb.g,
        quantized_rgb.b,
        quantized_lab.r,
        quantized_lab.g,
        quantized_lab.b,
    );
}
