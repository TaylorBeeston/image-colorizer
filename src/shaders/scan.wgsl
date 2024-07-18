struct ColorizedPixel {
  r: f32, g: f32, b: f32,
}

struct AtomicColorizedPixel {
  r: atomic<u32>, g: atomic<u32>, b: atomic<u32>,
}

struct Params {
  width: u32, height: u32, is_horizontal: u32,
}

@group(0) @binding(0) var<storage, read> input : array<ColorizedPixel>;
@group(0) @binding(1) var<storage, read_write> output
    : array<AtomicColorizedPixel>;
@group(0) @binding(2) var<uniform> params : Params;

var<workgroup> shared_data : array<vec3<f32>, 256>;

fn float_to_uint(f: f32) -> u32 { return bitcast<u32>(f); }

fn uint_to_float(u: u32) -> f32 { return bitcast<f32>(u); }

@compute @workgroup_size(256, 1)fn scan(@builtin(global_invocation_id) global_id: vec3<u32>, @builtin(local_invocation_id) local_id: vec3<u32>, @builtin(workgroup_id) group_id: vec3<u32>) {
    let stride = select(params.height, params.width, params.is_horizontal == 1u);
    let other = select(params.width, params.height, params.is_horizontal == 1u);
    let row = global_id.y;

    if row >= other { return; }

  // Load data into shared memory
    let global_offset = group_id.x * 256u;
    if global_offset + local_id.x < stride {
        let index = select((global_offset + local_id.x) * other + row,
            row * stride + (global_offset + local_id.x),
            params.is_horizontal == 1u);
        shared_data[local_id.x] = vec3<f32>(input[index].r, input[index].g, input[index].b);
    } else {
        shared_data[local_id.x] = vec3<f32>(0.0);
    }

    workgroupBarrier();

  // Perform parallel prefix sum within the workgroup
    for (var step = 1u; step < 256u; step *= 2u) {
        if local_id
      .x >= step { shared_data[local_id.x] += shared_data[local_id.x - step]; }
        workgroupBarrier();
    }

  // Write results back to global memory
    if global_offset + local_id.x < stride {
        let index = select((global_offset + local_id.x) * other + row,
            row * stride + (global_offset + local_id.x),
            params.is_horizontal == 1u);
        atomicStore(&output[index].r, float_to_uint(shared_data[local_id.x].r));
        atomicStore(&output[index].g, float_to_uint(shared_data[local_id.x].g));
        atomicStore(&output[index].b, float_to_uint(shared_data[local_id.x].b));
    }

  // If this is not the last workgroup, add the total sum to the first element
  // of the next workgroup
    if group_id
    .x < (stride - 1u) / 256u && local_id.x == 255u {
        let next_group_index = select((group_id.x + 1u) * 256u * other + row,
            row * stride + (group_id.x + 1u) * 256u,
            params.is_horizontal == 1u);
        atomicAdd(&output[next_group_index].r, float_to_uint(shared_data[255].r));
        atomicAdd(&output[next_group_index].g, float_to_uint(shared_data[255].g));
        atomicAdd(&output[next_group_index].b, float_to_uint(shared_data[255].b));
    }
}
