/* tslint:disable */
/* eslint-disable */
/**
* @param {Uint8Array} rgba
* @param {number} width
* @param {number} height
* @param {string} colorscheme
* @param {number} blend_factor
* @param {number} dither_amount
* @param {number} spatial_radius
* @param {boolean} interpolate_colors
* @param {number} interpolation_threshold
* @returns {Uint8Array}
*/
export function cpu_colorize(rgba: Uint8Array, width: number, height: number, colorscheme: string, blend_factor: number, dither_amount: number, spatial_radius: number, interpolate_colors: boolean, interpolation_threshold: number): Uint8Array;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  readonly cpu_colorize: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => void;
  readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
  readonly __wbindgen_malloc: (a: number, b: number) => number;
  readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
  readonly __wbindgen_free: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;
/**
* Instantiates the given `module`, which can either be bytes or
* a precompiled `WebAssembly.Module`.
*
* @param {SyncInitInput} module
*
* @returns {InitOutput}
*/
export function initSync(module: SyncInitInput): InitOutput;

/**
* If `module_or_path` is {RequestInfo} or {URL}, makes a request and
* for everything else, calls `WebAssembly.instantiate` directly.
*
* @param {InitInput | Promise<InitInput>} module_or_path
*
* @returns {Promise<InitOutput>}
*/
export default function __wbg_init (module_or_path?: InitInput | Promise<InitInput>): Promise<InitOutput>;
