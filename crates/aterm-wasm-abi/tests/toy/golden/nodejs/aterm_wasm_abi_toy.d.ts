/* tslint:disable */
/* eslint-disable */

/**
 * A counter with a log: the class with every member kind.
 */
export class Counter {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * A counter at `start`; a negative start throws.
     */
    constructor(start: bigint);
    /**
     * Add `by`; the new value.
     */
    add(by: bigint): bigint;
    /**
     * Panic mid-change: the value is bumped, then the module traps.
     */
    boom(msg: string): number;
    /**
     * A renamed method.
     */
    describe(): string;
    /**
     * A snapshot when a note contains `needle`.
     */
    find(needle: string): Snapshot | undefined;
    /**
     * A counter at `seed`, through a static method.
     */
    static from_seed(seed: number): Counter;
    /**
     * The notes, joined by `;`.
     */
    readonly log: string;
    /**
     * Append a note; how many there are.
     */
    note(text: string): number;
    /**
     * A snapshot, returned by value.
     */
    snapshot(): Snapshot;
    /**
     * The value.
     */
    readonly value: bigint;
}

/**
 * A counter's state at one moment.
 */
export class Snapshot {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * The note count then.
     */
    readonly notes: number;
    /**
     * The value then.
     */
    readonly value: bigint;
}

/**
 * `Ok(n * 2)`, or `Err(msg)` thrown as the string.
 */
export function checked(n: number, msg: string): number;

/**
 * `Ok(Some(text))` for a non-empty text, `Ok(None)` for an empty one, an
 * error for `"!"`.
 */
export function checked_opt(text: string): string | undefined;

/**
 * `Date.now()` through the `aterm_host` import.
 */
export function date_now(): number;

/**
 * Values dropped so far.
 */
export function drops(): number;

/**
 * Returns its argument.
 */
export function echo_bool(v: boolean): boolean;

/**
 * A borrowed byte slice, returned as a vector.
 */
export function echo_bytes(v: Uint8Array): Uint8Array;

/**
 * Returns its argument.
 */
export function echo_char(v: string): string;

/**
 * Returns its argument.
 */
export function echo_f32(v: number): number;

/**
 * `&[f32]` in, `Vec<f32>` out.
 */
export function echo_f32s(v: Float32Array): Float32Array;

/**
 * Returns its argument.
 */
export function echo_f64(v: number): number;

/**
 * `&[f64]` in, `Vec<f64>` out.
 */
export function echo_f64s(v: Float64Array): Float64Array;

/**
 * Returns its argument.
 */
export function echo_i16(v: number): number;

/**
 * Returns its argument.
 */
export function echo_i32(v: number): number;

/**
 * Returns its argument.
 */
export function echo_i64(v: bigint): bigint;

/**
 * `&[i64]` in, `Vec<i64>` out (a `BigInt64Array`).
 */
export function echo_i64s(v: BigInt64Array): BigInt64Array;

/**
 * Returns its argument.
 */
export function echo_i8(v: number): number;

/**
 * Returns its argument.
 */
export function echo_isize(v: number): number;

/**
 * Returns its argument.
 */
export function echo_opt_bool(v?: boolean | null): boolean | undefined;

/**
 * Returns its argument.
 */
export function echo_opt_char(v?: string | null): string | undefined;

/**
 * Returns its argument.
 */
export function echo_opt_f64(v?: number | null): number | undefined;

/**
 * Returns its argument.
 */
export function echo_opt_i32(v?: number | null): number | undefined;

/**
 * Returns its argument.
 */
export function echo_opt_string(v?: string | null): string | undefined;

/**
 * Returns its argument.
 */
export function echo_opt_u16(v?: number | null): number | undefined;

/**
 * Returns its argument.
 */
export function echo_opt_u32(v?: number | null): number | undefined;

/**
 * Returns its argument.
 */
export function echo_opt_u64(v?: bigint | null): bigint | undefined;

/**
 * Returns its argument.
 */
export function echo_opt_vec_u8(v?: Uint8Array | null): Uint8Array | undefined;

/**
 * A borrowed string, returned as a new one.
 */
export function echo_str(v: string): string;

/**
 * Returns its argument.
 */
export function echo_string(v: string): string;

/**
 * Returns its argument.
 */
export function echo_u16(v: number): number;

/**
 * `&[u16]` in, `Vec<u16>` out.
 */
export function echo_u16s(v: Uint16Array): Uint16Array;

/**
 * Returns its argument.
 */
export function echo_u32(v: number): number;

/**
 * `&[u32]` in, `Vec<u32>` out.
 */
export function echo_u32s(v: Uint32Array): Uint32Array;

/**
 * Returns its argument.
 */
export function echo_u64(v: bigint): bigint;

/**
 * Returns its argument.
 */
export function echo_u8(v: number): number;

/**
 * Returns its argument.
 */
export function echo_usize(v: number): number;

/**
 * Returns its argument.
 */
export function echo_vec_u8(v: Uint8Array): Uint8Array;

/**
 * `n` bytes of `fill`: enough to grow the memory under the glue's views.
 */
export function filled(n: number, fill: number): Uint8Array;

/**
 * Install the panic hook on the host console, as a module's constructor
 * does.
 */
export function installHook(): void;

/**
 * Every parameter shape at once, with a non-trailing optional: `a|b|c|d|e`.
 */
export function mixed(a: number, b: bigint | null | undefined, c: string, d: boolean | null | undefined, e: number): string;

/**
 * Panic with `msg`: a trap, after the hook (if installed) has logged it.
 */
export function panicWith(msg: string): void;

/**
 * `performance.now()` through the `aterm_host` import.
 */
export function performance_now(): number;

/**
 * Rust keywords as parameters, written raw: the descriptor records them
 * without the `r#`.
 */
export function raw_idents(type: number, _in: number): number;

/**
 * Parameters JS cannot name as written.
 */
export function reserved(_function: number, _this: number): number;

/**
 * Trailing optionals, which JS may leave out: `a + b + len(c)`.
 */
export function trailing(a: number, b?: number | null, c?: string | null): number;

/**
 * Nothing in, nothing out.
 */
export function unit(): void;

/**
 * The UTF-8 length of `text`, which crossed as UTF-16 in JS.
 */
export function utf8_len(text: string): number;
