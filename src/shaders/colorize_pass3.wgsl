struct ColorizedPixel {
  r: f32, g: f32, b: f32,
}

struct Params {
  width: u32,
          height: u32,
                   blend_factor: f32,
                                  dither_amount: f32,
                                                  spatial_radius: u32,
}

@group(0) @binding(0) var<storage, read> input : array<ColorizedPixel>;
@group(0) @binding(1) var<storage, read> sat : array<ColorizedPixel>;
@group(0) @binding(2) var<storage, write> output : array<ColorizedPixel>;
@group(0) @binding(3) var<uniform> params : Params;

fn clamp_color(color: vec3<f32>) -> vec3<f32> {
    return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn fast_spatial_color_average(x: u32, y: u32) -> vec3<f32> {
    let radius = i32(params.spatial_radius);
    let x1 = max(i32(x) - radius, 0);
    let y1 = max(i32(y) - radius, 0);
    let x2 = min(i32(x) + radius, i32(params.width - 1u));
    let y2 = min(i32(y) + radius, i32(params.height - 1u));

    let area = f32((x2 - x1 + 1) * (y2 - y1 + 1));

    let top_left = get_sat_value(u32(x1), u32(y1));
    let top_right = get_sat_value(u32(x2 + 1), u32(y1));
    let bottom_left = get_sat_value(u32(x1), u32(y2 + 1));
    let bottom_right = get_sat_value(u32(x2 + 1), u32(y2 + 1));

    let sum = vec3<f32>(bottom_right.r - top_right.r - bottom_left.r + top_left.r,
        bottom_right.g - top_right.g - bottom_left.g + top_left.g,
        bottom_right.b - top_right.b - bottom_left.b + top_left.b);

    return vec3<f32>(f32(sum.r / area), f32(sum.g / area), f32(sum.b / area));
}

fn get_sat_value(x: u32, y: u32) -> vec3<f32> {
    let index = y * (params.width + 1u) + x;
    return vec3<f32>(sat[index].r, sat[index].g, sat[index].b);
}

fn rgb_to_lab(rgb: vec3<f32>) -> vec3<f32> {
    let xyz = rgb_to_xyz(rgb);
    return xyz_to_lab(xyz);
}

fn rgb_to_xyz(rgb: vec3<f32>) -> vec3<f32> {
    let r = select(rgb.r / 12.92, pow((rgb.r + 0.055) / 1.055, 2.4), rgb.r > 0.04045);
    let g = select(rgb.g / 12.92, pow((rgb.g + 0.055) / 1.055, 2.4), rgb.g > 0.04045);
    let b = select(rgb.b / 12.92, pow((rgb.b + 0.055) / 1.055, 2.4), rgb.b > 0.04045);

    return vec3<f32>(r * 0.4124564 + g * 0.3575761 + b * 0.1804375,
        r * 0.2126729 + g * 0.7151522 + b * 0.0721750,
        r * 0.0193339 + g * 0.1191920 + b * 0.9503041);
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

    return vec3<f32>(clamp(r1, 0.0, 1.0), clamp(g1, 0.0, 1.0),
        clamp(b1, 0.0, 1.0));
}

@compute @workgroup_size(16, 16, 1)fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;
    let index = x + y * params.width;

    if x >= params.width || y >= params.height { return; }

    let input_color = vec3<f32>(f32(input[index].r), f32(input[index].g), f32(input[index].b));
    let avg_lab = fast_spatial_color_average(x, y);

    let input_lab = rgb_to_lab(input_color);

    let luminance_transferred_lab = vec3<f32>(input_lab.r, avg_lab.g, avg_lab.b);
    let luminance_transferred_rgb = lab_to_rgb(luminance_transferred_lab);

    let final_color = mix(input_color, luminance_transferred_rgb, f32(params.blend_factor));

    let clamped_color = clamp_color(final_color);
  // let clamped_color = clamp_color(get_sat_value(x, y));

    output[index] = ColorizedPixel(f32(clamped_color.r), f32(clamped_color.g),
        f32(clamped_color.b));
}
