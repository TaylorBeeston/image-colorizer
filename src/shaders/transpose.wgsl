struct ColorizedPixel {
  r: f32, g: f32, b: f32,
}

struct Params {
  width: u32, height: u32, is_horizontal: u32,
}

@group(0) @binding(0) var<storage, read> input : array<ColorizedPixel>;
@group(0) @binding(1) var<storage, read_write> output : array<ColorizedPixel>;
@group(0) @binding(2) var<uniform> params : Params;

@compute @workgroup_size(16, 16)fn transpose(@builtin(global_invocation_id) global_id: vec3<u32>) {
    let x = global_id.x;
    let y = global_id.y;

    if x < params.width && y < params.height {
        let input_index = y * params.width + x;
        let output_index = x * params.height + y;
        output[output_index] = input[input_index];
    }
}
