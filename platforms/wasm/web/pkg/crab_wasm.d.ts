/* tslint:disable */
/* eslint-disable */

/**
 * A wasm-visible handle to a running emulator.
 */
export class Gb {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Current 160x144 framebuffer as 2-bit shades (0..=3) per pixel.
     */
    framebuffer(): Uint8Array;
    /**
     * Create an emulator from a raw ROM byte slice.
     */
    constructor(rom: Uint8Array);
    /**
     * Press (or release) a button by its GB bitmask (see gb_core::joypad).
     */
    set_button(button: number, pressed: boolean): void;
    /**
     * Advance the emulator by one frame (70224 cycles).
     */
    step_frame(): void;
    /**
     * Drain audio produced since the last call as interleaved stereo f32
     * samples in `-1.0..=1.0` (sample rate 8192 Hz).
     */
    take_audio(): Float32Array;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_gb_free: (a: number, b: number) => void;
    readonly gb_framebuffer: (a: number) => [number, number];
    readonly gb_new: (a: number, b: number) => [number, number, number];
    readonly gb_set_button: (a: number, b: number, c: number) => void;
    readonly gb_step_frame: (a: number) => void;
    readonly gb_take_audio: (a: number) => [number, number];
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
