export class CpuColorizer {
  static async create() {
    const module = await import('./pkg/image_colorizer_web.js');

    await module.default();

    return new CpuColorizer(module.cpu_colorize);
  }

  constructor(colorize) {
    this.colorizeWasm = colorize;
    this.mode = 'cpu';
  }

  async colorize(imageData, options) {
    await new Promise(resolve => setTimeout(resolve, 0));

    const output = this.colorizeWasm(
      imageData.data,
      imageData.width,
      imageData.height,
      options.colors.join('\n'),
      options.blendFactor,
      options.ditherAmount,
      options.spatialRadius,
      options.interpolateColors,
      options.interpolationThreshold,
    );

    return new ImageData(new Uint8ClampedArray(output), imageData.width, imageData.height);
  }
}
