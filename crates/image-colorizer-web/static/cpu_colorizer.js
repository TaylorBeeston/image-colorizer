export class CpuColorizer {
  static async create() {
    const worker = new Worker(new URL('./cpu_worker.js', import.meta.url), { type: 'module' });

    return new CpuColorizer(worker);
  }

  constructor(worker) {
    this.worker = worker;
    this.mode = 'cpu';
    this.nextId = 0;
    this.pending = new Map();
    this.onProgress = undefined;

    this.worker.addEventListener('message', event => this.handleMessage(event.data));
  }

  async colorize(imageData, options) {
    const id = this.nextId++;
    const data = new Uint8Array(imageData.data);

    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.worker.postMessage({
        id,
        image: {
          width: imageData.width,
          height: imageData.height,
          data: data.buffer,
        },
        options,
      }, [data.buffer]);
    });
  }

  handleMessage(message) {
    const pending = this.pending.get(message.id);
    if (!pending) return;

    if (message.type === 'progress') {
      this.onProgress?.(message.progress);
      return;
    }

    this.pending.delete(message.id);

    if (message.type === 'done') {
      pending.resolve(new ImageData(new Uint8ClampedArray(message.output), message.width, message.height));
    } else {
      pending.reject(new Error(message.message));
    }
  }
}
