import init, { cpu_colorize } from './pkg/image_colorizer_web.js';

let ready;

self.onmessage = async event => {
  const { id, image, options } = event.data;

  try {
    ready ||= init();
    await ready;

    const output = cpu_colorize(
      new Uint8Array(image.data),
      image.width,
      image.height,
      options.colors.join('\n'),
      options.blendFactor,
      options.ditherAmount,
      options.spatialRadius,
      options.interpolateColors,
      options.interpolationThreshold,
      progress => self.postMessage({ type: 'progress', id, progress }),
    );

    self.postMessage({ type: 'done', id, width: image.width, height: image.height, output: output.buffer }, [output.buffer]);
  } catch (error) {
    self.postMessage({ type: 'error', id, message: error.message || String(error) });
  }
};
