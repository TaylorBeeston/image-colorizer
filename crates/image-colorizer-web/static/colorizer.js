export const KANAGAWA = [
  '#16161D', '#181820', '#1a1a22', '#1F1F28', '#2A2A37', '#363646', '#54546D', '#223249', '#2D4F67', '#2B3328', '#49443C', '#43242B', '#252535', '#76946A', '#C34043', '#DCA561', '#E82424', '#FF9E3B', '#6A9589', '#658594', '#C8C093', '#DCD7BA', '#727169', '#957FB8', '#b8b4d0', '#7E9CD8', '#938AA9', '#9CABCA', '#7FB4CA', '#A3D4D5', '#7AA89F', '#98BB6C', '#938056', '#C0A36E', '#E6C384', '#D27E99', '#E46876', '#FF5D62', '#FFA066', '#717C7C', '#0d0c0c', '#12120f', '#1D1C19', '#181616', '#282727', '#393836', '#625e5a', '#c5c9c5', '#87a987', '#8a9a7b', '#a292a3', '#b6927b', '#b98d7b', '#a6a69c', '#9e9b93', '#7a8382', '#8ba4b0', '#8992a7', '#c4746e', '#8ea4a2', '#737c73', '#949fb5', '#c4b28a', '#545464', '#43436c', '#dcd7ba', '#716e61', '#8a8980', '#d5cea3', '#dcd5ac', '#e5ddb0', '#f2ecbc', '#e7dba0', '#e4d794', '#a09cac', '#766b90', '#c9cbd1', '#624c83', '#c7d7e0', '#b5cbd2', '#9fb5c9', '#4d699b', '#5d57a3', '#6f894e', '#6e915f', '#b7d0ae', '#b35b79', '#cc6d00', '#e98a00', '#77713f', '#836f4a', '#de9800', '#f9d791', '#c84053', '#d7474b', '#e82424', '#d9a594', '#597b75', '#5e857a', '#4e8ca2', '#6693bf', '#5a7785', '#d7e3d8',
];

const WORKGROUP_SIZE = 16;
const PIXEL_STRIDE = 16;
const WORKING_STRIDE = 32;
const PARAMS_SIZE = 32;
const WEBGPU_START_TIMEOUT_MS = 10_000;


export class WebGpuColorizer {
  static async create() {
    if (!navigator.gpu) throw new Error(webGpuUnavailableMessage('This browser is missing the graphics support Image Colorizer needs.'));

    const firstError = { message: '' };
    const adapter = await requestAdapter({ powerPreference: 'high-performance' }, firstError)
      || await requestAdapter({ featureLevel: 'compatibility' }, firstError);

    if (!adapter) throw new Error(webGpuUnavailableMessage(firstError.message || 'This browser could not find a usable graphics device.'));

    const device = await withTimeout(adapter.requestDevice({
      requiredLimits: {
        maxBufferSize: adapter.limits.maxBufferSize,
        maxStorageBufferBindingSize: adapter.limits.maxStorageBufferBindingSize,
      },
    }), WEBGPU_START_TIMEOUT_MS, webGpuUnavailableMessage('This browser could not start the graphics renderer.'));
    const [pass1Source, spatialSource] = await Promise.all([
      fetchText('shaders/colorize_pass1.wgsl'),
      fetchText('shaders/spatial_average.wgsl'),
    ]);
    const pass1Module = device.createShaderModule({ label: 'Colorize pass 1', code: pass1Source });
    const spatialModule = device.createShaderModule({ label: 'Spatial average', code: spatialSource });

    return new WebGpuColorizer(device, {
      pass1: device.createComputePipeline({ label: 'Colorize pass 1', layout: 'auto', compute: { module: pass1Module, entryPoint: 'main' } }),
      horizontal: device.createComputePipeline({ label: 'Horizontal spatial average', layout: 'auto', compute: { module: spatialModule, entryPoint: 'horizontal' } }),
      vertical: device.createComputePipeline({ label: 'Vertical final', layout: 'auto', compute: { module: spatialModule, entryPoint: 'vertical_final' } }),
    });
  }

  constructor(device, pipelines) {
    this.device = device;
    this.queue = device.queue;
    this.pipelines = pipelines;
    this.width = 0;
    this.height = 0;
    this.paletteKey = '';
    this.buffers = undefined;
    this.paletteBuffer = undefined;
    this.passBindGroup = undefined;
  }

  destroy() {
    this.destroyBuffers();
    this.paletteBuffer?.destroy();
  }

  async colorize(imageData, options) {
    const palette = buildPalette(options.colors, options.interpolateColors, options.interpolationThreshold);

    this.ensureBuffers(imageData.width, imageData.height);
    this.ensurePalette(palette);
    this.writeInput(imageData.data);
    this.writeParams(imageData.width, imageData.height, options);

    const { buffers } = this;
    const encoder = this.device.createCommandEncoder({ label: 'Image colorizer' });

    this.dispatch(encoder, this.pipelines.pass1, this.passBindGroup);
    this.dispatch(encoder, this.pipelines.horizontal, buffers.horizontalBindGroup);
    this.dispatch(encoder, this.pipelines.vertical, buffers.verticalBindGroup);

    encoder.copyBufferToBuffer(buffers.output, 0, buffers.read, 0, buffers.output.size);
    this.queue.submit([encoder.finish()]);

    await buffers.read.mapAsync(GPUMapMode.READ);

    try {
      const packed = new Uint32Array(buffers.read.getMappedRange().slice(0, imageData.width * imageData.height * 4));
      return packedRgbToImageData(packed, imageData.width, imageData.height);
    } finally {
      buffers.read.unmap();
    }
  }

  ensureBuffers(width, height) {
    if (this.buffers && this.width === width && this.height === height) return;

    this.destroyBuffers();

    const pixels = width * height;
    const input = this.storageBuffer(pixels * PIXEL_STRIDE, GPUBufferUsage.COPY_DST);
    const working = this.storageBuffer(pixels * WORKING_STRIDE, 0);
    const horizontal = this.storageBuffer(pixels * PIXEL_STRIDE, 0);
    const output = this.storageBuffer(pixels * 4, GPUBufferUsage.COPY_SRC);
    const read = this.device.createBuffer({
      label: 'Colorized readback',
      size: pixels * 4,
      usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
    });
    const params = this.device.createBuffer({
      label: 'Colorizer params',
      size: PARAMS_SIZE,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });

    this.width = width;
    this.height = height;
    this.buffers = { input, working, horizontal, output, read, params };
    this.passBindGroup = undefined;
    this.createSpatialBindGroups();
  }

  ensurePalette(palette) {
    const key = palette.map(([l, a, b]) => `${l.toFixed(4)},${a.toFixed(4)},${b.toFixed(4)}`).join('|');
    if (this.paletteBuffer && key === this.paletteKey) return;

    this.paletteBuffer?.destroy();
    this.paletteKey = key;

    const data = new Float32Array(palette.length * 4);
    for (let index = 0; index < palette.length; index++) data.set([...palette[index], 0], index * 4);

    this.paletteBuffer = this.device.createBuffer({
      label: 'Color palette',
      size: data.byteLength,
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
    });
    this.queue.writeBuffer(this.paletteBuffer, 0, data);
    this.createPassBindGroup();
  }

  writeInput(rgba) {
    const data = new Float32Array(this.width * this.height * 4);

    for (let source = 0, target = 0; source < rgba.length; source += 4, target += 4) {
      data[target] = rgba[source] / 255;
      data[target + 1] = rgba[source + 1] / 255;
      data[target + 2] = rgba[source + 2] / 255;
      data[target + 3] = 0;
    }

    this.queue.writeBuffer(this.buffers.input, 0, data);
  }

  writeParams(width, height, options) {
    const data = new ArrayBuffer(PARAMS_SIZE);
    const view = new DataView(data);

    view.setUint32(0, width, true);
    view.setUint32(4, height, true);
    view.setFloat32(8, options.blendFactor, true);
    view.setFloat32(12, options.ditherAmount, true);
    view.setUint32(16, options.spatialRadius, true);

    this.queue.writeBuffer(this.buffers.params, 0, data);
  }

  createPassBindGroup() {
    if (!this.buffers || !this.paletteBuffer) return;

    this.passBindGroup = this.device.createBindGroup({
      label: 'Pass 1 bindings',
      layout: this.pipelines.pass1.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: { buffer: this.buffers.input } },
        { binding: 1, resource: { buffer: this.buffers.working } },
        { binding: 2, resource: { buffer: this.paletteBuffer } },
        { binding: 3, resource: { buffer: this.buffers.params } },
      ],
    });
  }

  createSpatialBindGroups() {
    this.buffers.horizontalBindGroup = this.device.createBindGroup({
      label: 'Horizontal bindings',
      layout: this.pipelines.horizontal.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: { buffer: this.buffers.working } },
        { binding: 1, resource: { buffer: this.buffers.horizontal } },
        { binding: 3, resource: { buffer: this.buffers.params } },
      ],
    });
    this.buffers.verticalBindGroup = this.device.createBindGroup({
      label: 'Vertical bindings',
      layout: this.pipelines.vertical.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: { buffer: this.buffers.working } },
        { binding: 1, resource: { buffer: this.buffers.horizontal } },
        { binding: 2, resource: { buffer: this.buffers.output } },
        { binding: 3, resource: { buffer: this.buffers.params } },
      ],
    });
  }

  dispatch(encoder, pipeline, bindGroup) {
    const pass = encoder.beginComputePass();
    pass.setPipeline(pipeline);
    pass.setBindGroup(0, bindGroup);
    pass.dispatchWorkgroups(Math.ceil(this.width / WORKGROUP_SIZE), Math.ceil(this.height / WORKGROUP_SIZE));
    pass.end();
  }

  storageBuffer(size, extraUsage) {
    return this.device.createBuffer({
      size,
      usage: GPUBufferUsage.STORAGE | extraUsage,
    });
  }

  destroyBuffers() {
    if (!this.buffers) return;

    for (const buffer of [this.buffers.input, this.buffers.working, this.buffers.horizontal, this.buffers.output, this.buffers.read, this.buffers.params]) buffer.destroy();

    this.buffers = undefined;
    this.passBindGroup = undefined;
  }
}

export function parseColorscheme(text) {
  const colors = [];

  for (const line of text.split('\n')) {
    const color = normalizeHex(line.split('//')[0].trim());
    if (color) colors.push(color);
  }

  if (!colors.length) throw new Error('Colorscheme must contain at least one hex color.');

  return colors;
}

export function normalizeHex(input) {
  if (!input) return '';

  const hex = input.startsWith('#') ? input.slice(1) : input;

  if (/^[0-9a-fA-F]{3}$/.test(hex)) return `#${hex.split('').map(char => char + char).join('').toLowerCase()}`;
  if (/^[0-9a-fA-F]{6}$/.test(hex)) return `#${hex.toLowerCase()}`;

  throw new Error(`Invalid color '${input}'. Use #rgb or #rrggbb.`);
}

export function rgbToLab([r, g, b]) {
  const [x, y, z] = rgbToXyz([r, g, b]);
  return xyzToLab([x, y, z]);
}

export function hexToRgb(hex) {
  const normalized = normalizeHex(hex).slice(1);
  return [0, 2, 4].map(offset => parseInt(normalized.slice(offset, offset + 2), 16) / 255);
}

export function labToRgb([l, a, b]) {
  const [x, y, z] = labToXyz([l, a, b]);
  return xyzToRgb([x, y, z]);
}

function buildPalette(colors, interpolateColors, threshold) {
  const labs = colors.map(color => rgbToLab(hexToRgb(color)));
  if (!interpolateColors || labs.length < 2) return labs;

  labs.sort((left, right) => left[0] - right[0]);

  const output = [];

  for (let index = 0; index < labs.length - 1; index++) {
    const current = labs[index];
    const next = labs[index + 1];
    output.push(current);

    const distance = ciede2000(current, next);
    if (distance <= threshold) continue;

    const steps = Math.ceil(distance / threshold);
    for (let step = 1; step < steps; step++) output.push(mixLab(current, next, step / steps));
  }

  output.push(labs[labs.length - 1]);
  return output;
}

async function fetchText(url) {
  const response = await fetch(url, { cache: 'no-store' });
  if (!response.ok) throw new Error(`Failed to load ${url}: ${response.status}`);

  return response.text();
}

function packedRgbToImageData(packed, width, height) {
  const rgba = new Uint8ClampedArray(width * height * 4);

  for (let index = 0; index < packed.length; index++) {
    const color = packed[index];
    const offset = index * 4;
    rgba[offset] = color & 255;
    rgba[offset + 1] = (color >> 8) & 255;
    rgba[offset + 2] = (color >> 16) & 255;
    rgba[offset + 3] = 255;
  }

  return new ImageData(rgba, width, height);
}

function rgbToXyz([r, g, b]) {
  const linear = [r, g, b].map(channel => channel > 0.04045 ? Math.pow((channel + 0.055) / 1.055, 2.4) : channel / 12.92);

  return [
    linear[0] * 0.4124564 + linear[1] * 0.3575761 + linear[2] * 0.1804375,
    linear[0] * 0.2126729 + linear[1] * 0.7151522 + linear[2] * 0.0721750,
    linear[0] * 0.0193339 + linear[1] * 0.1191920 + linear[2] * 0.9503041,
  ];
}

function xyzToLab([x, y, z]) {
  const epsilon = 0.008856;
  const kappa = 903.3;
  const xr = x / 0.950489;
  const yr = y;
  const zr = z / 1.088840;
  const fx = xr > epsilon ? Math.cbrt(xr) : (kappa * xr + 16) / 116;
  const fy = yr > epsilon ? Math.cbrt(yr) : (kappa * yr + 16) / 116;
  const fz = zr > epsilon ? Math.cbrt(zr) : (kappa * zr + 16) / 116;

  return [116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz)];
}

function labToXyz([l, a, b]) {
  const fy = (l + 16) / 116;
  const fx = a / 500 + fy;
  const fz = fy - b / 200;
  const epsilon = 0.008856;
  const kappa = 903.3;
  const fx3 = fx * fx * fx;
  const fz3 = fz * fz * fz;
  const xr = fx3 > epsilon ? fx3 : (116 * fx - 16) / kappa;
  const yr = l > kappa * epsilon ? fy * fy * fy : l / kappa;
  const zr = fz3 > epsilon ? fz3 : (116 * fz - 16) / kappa;

  return [xr * 0.950489, yr, zr * 1.088840];
}

function xyzToRgb([x, y, z]) {
  const linear = [
    x * 3.2404542 + y * -1.5371385 + z * -0.4985314,
    x * -0.9692660 + y * 1.8760108 + z * 0.0415560,
    x * 0.0556434 + y * -0.2040259 + z * 1.0572252,
  ];

  return linear.map(channel => clamp01(channel > 0.0031308 ? 1.055 * Math.pow(channel, 1 / 2.4) - 0.055 : 12.92 * channel));
}

function mixLab(left, right, amount) {
  return [
    left[0] + (right[0] - left[0]) * amount,
    left[1] + (right[1] - left[1]) * amount,
    left[2] + (right[2] - left[2]) * amount,
  ];
}

function ciede2000(left, right) {
  const [l1, a1, b1] = left;
  const [l2, a2, b2] = right;
  const c1 = Math.hypot(a1, b1);
  const c2 = Math.hypot(a2, b2);
  const cMean = (c1 + c2) / 2;
  const cMean7 = Math.pow(cMean, 7);
  const g = 0.5 * (1 - Math.sqrt(cMean7 / (cMean7 + Math.pow(25, 7))));
  const a1Prime = (1 + g) * a1;
  const a2Prime = (1 + g) * a2;
  const c1Prime = Math.hypot(a1Prime, b1);
  const c2Prime = Math.hypot(a2Prime, b2);
  const h1Prime = hueDegrees(b1, a1Prime);
  const h2Prime = hueDegrees(b2, a2Prime);
  const deltaLPrime = l2 - l1;
  const deltaCPrime = c2Prime - c1Prime;
  const deltaHPrime = 2 * Math.sqrt(c1Prime * c2Prime) * Math.sin(degToRad(deltaHue(h1Prime, h2Prime, c1Prime, c2Prime) / 2));
  const lMeanPrime = (l1 + l2) / 2;
  const cMeanPrime = (c1Prime + c2Prime) / 2;
  const hMeanPrime = meanHue(h1Prime, h2Prime, c1Prime, c2Prime);
  const t = 1 - 0.17 * Math.cos(degToRad(hMeanPrime - 30)) + 0.24 * Math.cos(degToRad(2 * hMeanPrime)) + 0.32 * Math.cos(degToRad(3 * hMeanPrime + 6)) - 0.20 * Math.cos(degToRad(4 * hMeanPrime - 63));
  const deltaTheta = 30 * Math.exp(-Math.pow((hMeanPrime - 275) / 25, 2));
  const cMeanPrime7 = Math.pow(cMeanPrime, 7);
  const rc = 2 * Math.sqrt(cMeanPrime7 / (cMeanPrime7 + Math.pow(25, 7)));
  const sl = 1 + (0.015 * Math.pow(lMeanPrime - 50, 2)) / Math.sqrt(20 + Math.pow(lMeanPrime - 50, 2));
  const sc = 1 + 0.045 * cMeanPrime;
  const sh = 1 + 0.015 * cMeanPrime * t;
  const rt = -Math.sin(degToRad(2 * deltaTheta)) * rc;
  const lTerm = deltaLPrime / sl;
  const cTerm = deltaCPrime / sc;
  const hTerm = deltaHPrime / sh;

  return Math.sqrt(lTerm * lTerm + cTerm * cTerm + hTerm * hTerm + rt * cTerm * hTerm);
}

function hueDegrees(y, x) {
  if (x === 0 && y === 0) return 0;

  const angle = radToDeg(Math.atan2(y, x));
  return angle >= 0 ? angle : angle + 360;
}

function deltaHue(h1, h2, c1, c2) {
  if (c1 * c2 === 0) return 0;

  const difference = h2 - h1;
  if (Math.abs(difference) <= 180) return difference;
  return difference > 180 ? difference - 360 : difference + 360;
}

function meanHue(h1, h2, c1, c2) {
  if (c1 * c2 === 0) return h1 + h2;
  if (Math.abs(h1 - h2) <= 180) return (h1 + h2) / 2;
  return h1 + h2 < 360 ? (h1 + h2 + 360) / 2 : (h1 + h2 - 360) / 2;
}

function degToRad(degrees) {
  return degrees * Math.PI / 180;
}

function radToDeg(radians) {
  return radians * 180 / Math.PI;
}

function clamp01(value) {
  return Math.max(0, Math.min(1, value));
}


async function requestAdapter(options, firstError) {
  try {
    const adapter = await withTimeout(
      navigator.gpu.requestAdapter(options),
      WEBGPU_START_TIMEOUT_MS,
      'This browser could not start the graphics renderer.',
    );

    if (adapter) return adapter;
    firstError.message ||= 'This browser could not find a usable graphics device.';
  } catch (error) {
    firstError.message ||= error.message || String(error);
  }

  return undefined;
}

function withTimeout(promise, ms, message) {
  return Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error(message)), ms)),
  ]);
}

function webGpuUnavailableMessage(reason) {
  const local = location.hostname === 'localhost' || location.hostname === '127.0.0.1' || location.protocol === 'https:';
  const secureContext = window.isSecureContext;
  const transport = secureContext && local ? '' : ' Open this page from https://, localhost, or 127.0.0.1.';

  return `${reason}${transport} Try current Chrome or Edge with hardware acceleration enabled, or use 'image-colorizer serve'.`;
}