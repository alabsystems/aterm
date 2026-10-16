/* @ts-self-types="./aterm_wasm_abi_toy.d.ts" */

/**
 * A counter with a log: the class with every member kind.
 */
export class Counter {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(Counter.prototype);
        obj.__wbg_ptr = ptr;
        CounterFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        CounterFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__aterm_free_counter(ptr, 0);
        checkThrown();
    }
    /**
     * A counter at `start`; a negative start throws.
     * @param {bigint} start
     */
    constructor(start) {
        const ret$ = wasm.counter_new(start, 0);
        checkThrown();
        this.__wbg_ptr = ret$ >>> 0;
        CounterFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Add `by`; the new value.
     * @param {bigint} by
     * @returns {bigint}
     */
    add(by) {
        const ret$ = wasm.counter_add(this.__wbg_ptr, by, 0);
        checkThrown();
        return ret$;
    }
    /**
     * Panic mid-change: the value is bumped, then the module traps.
     * @param {string} msg
     * @returns {number}
     */
    boom(msg) {
        const ptr$0 = passStringToWasm0(msg, wasm.aterm_alloc, wasm.aterm_realloc);
        const len$0 = WASM_VECTOR_LEN;
        const ret$ = wasm.counter_boom(this.__wbg_ptr, ptr$0, len$0);
        checkThrown();
        return ret$ >>> 0;
    }
    /**
     * A renamed method.
     * @returns {string}
     */
    describe() {
        const ret$ = wasm.counter_describe_counter(this.__wbg_ptr);
        checkThrown();
        const len$ = retSlot();
        try {
            return getStringFromWasm0(ret$, len$);
        } finally {
            wasm.aterm_free(ret$, len$, 1);
        }
    }
    /**
     * A snapshot when a note contains `needle`.
     * @param {string} needle
     * @returns {Snapshot | undefined}
     */
    find(needle) {
        const ptr$0 = passStringToWasm0(needle, wasm.aterm_alloc, wasm.aterm_realloc);
        const len$0 = WASM_VECTOR_LEN;
        const ret$ = wasm.counter_find(this.__wbg_ptr, ptr$0, len$0);
        checkThrown();
        return ret$ === 0 ? undefined : Snapshot.__wrap(ret$);
    }
    /**
     * A counter at `seed`, through a static method.
     * @param {number} seed
     * @returns {Counter}
     */
    static from_seed(seed) {
        const ret$ = wasm.counter_from_seed(seed, 0);
        checkThrown();
        return Counter.__wrap(ret$);
    }
    /**
     * The notes, joined by `;`.
     * @returns {string}
     */
    get log() {
        const ret$ = wasm.counter_log(this.__wbg_ptr);
        checkThrown();
        const len$ = retSlot();
        try {
            return getStringFromWasm0(ret$, len$);
        } finally {
            wasm.aterm_free(ret$, len$, 1);
        }
    }
    /**
     * Append a note; how many there are.
     * @param {string} text
     * @returns {number}
     */
    note(text) {
        const ptr$0 = passStringToWasm0(text, wasm.aterm_alloc, wasm.aterm_realloc);
        const len$0 = WASM_VECTOR_LEN;
        const ret$ = wasm.counter_note(this.__wbg_ptr, ptr$0, len$0);
        checkThrown();
        return ret$ >>> 0;
    }
    /**
     * A snapshot, returned by value.
     * @returns {Snapshot}
     */
    snapshot() {
        const ret$ = wasm.counter_snapshot(this.__wbg_ptr);
        checkThrown();
        return Snapshot.__wrap(ret$);
    }
    /**
     * The value.
     * @returns {bigint}
     */
    get value() {
        const ret$ = wasm.counter_value(this.__wbg_ptr);
        checkThrown();
        return ret$;
    }
}
if (Symbol.dispose) Counter.prototype[Symbol.dispose] = Counter.prototype.free;

/**
 * A counter's state at one moment.
 */
export class Snapshot {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(Snapshot.prototype);
        obj.__wbg_ptr = ptr;
        SnapshotFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        SnapshotFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__aterm_free_snapshot(ptr, 0);
        checkThrown();
    }
    /**
     * The note count then.
     * @returns {number}
     */
    get notes() {
        const ret$ = wasm.snapshot_notes(this.__wbg_ptr);
        checkThrown();
        return ret$ >>> 0;
    }
    /**
     * The value then.
     * @returns {bigint}
     */
    get value() {
        const ret$ = wasm.snapshot_value(this.__wbg_ptr);
        checkThrown();
        return ret$;
    }
}
if (Symbol.dispose) Snapshot.prototype[Symbol.dispose] = Snapshot.prototype.free;

/**
 * `Ok(n * 2)`, or `Err(msg)` thrown as the string.
 * @param {number} n
 * @param {string} msg
 * @returns {number}
 */
export function checked(n, msg) {
    const ptr$1 = passStringToWasm0(msg, wasm.aterm_alloc, wasm.aterm_realloc);
    const len$1 = WASM_VECTOR_LEN;
    const ret$ = wasm.checked(n, 0, ptr$1, len$1);
    checkThrown();
    return ret$ >>> 0;
}

/**
 * `Ok(Some(text))` for a non-empty text, `Ok(None)` for an empty one, an
 * error for `"!"`.
 * @param {string} text
 * @returns {string | undefined}
 */
export function checked_opt(text) {
    const ptr$0 = passStringToWasm0(text, wasm.aterm_alloc, wasm.aterm_realloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.checked_opt(ptr$0, len$0);
    checkThrown();
    if (ret$ === 0) {
        return undefined;
    }
    const len$ = retSlot();
    try {
        return getStringFromWasm0(ret$, len$);
    } finally {
        wasm.aterm_free(ret$, len$, 1);
    }
}

/**
 * `Date.now()` through the `aterm_host` import.
 * @returns {number}
 */
export function date_now() {
    const ret$ = wasm.date_now();
    checkThrown();
    return ret$;
}

/**
 * Values dropped so far.
 * @returns {number}
 */
export function drops() {
    const ret$ = wasm.drops();
    checkThrown();
    return ret$ >>> 0;
}

/**
 * Returns its argument.
 * @param {boolean} v
 * @returns {boolean}
 */
export function echo_bool(v) {
    const ret$ = wasm.echo_bool(v, 0);
    checkThrown();
    return ret$ !== 0;
}

/**
 * A borrowed byte slice, returned as a vector.
 * @param {Uint8Array} v
 * @returns {Uint8Array}
 */
export function echo_bytes(v) {
    const ptr$0 = passArrayU8ToWasm0(v, wasm.aterm_alloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_bytes(ptr$0, len$0);
    checkThrown();
    const len$ = retSlot();
    const v$ = getArrayU8FromWasm0(ret$, len$).slice();
    wasm.aterm_free(ret$, len$ * 1, 1);
    return v$;
}

/**
 * Returns its argument.
 * @param {string} v
 * @returns {string}
 */
export function echo_char(v) {
    const char$0 = v.codePointAt(0);
    _assertChar(char$0);
    const ret$ = wasm.echo_char(char$0, 0);
    checkThrown();
    return String.fromCodePoint(ret$);
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_f32(v) {
    const ret$ = wasm.echo_f32(v, 0);
    checkThrown();
    return ret$;
}

/**
 * `&[f32]` in, `Vec<f32>` out.
 * @param {Float32Array} v
 * @returns {Float32Array}
 */
export function echo_f32s(v) {
    const ptr$0 = passArrayF32ToWasm0(v, wasm.aterm_alloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_f32s(ptr$0, len$0);
    checkThrown();
    const len$ = retSlot();
    const v$ = getArrayF32FromWasm0(ret$, len$).slice();
    wasm.aterm_free(ret$, len$ * 4, 4);
    return v$;
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_f64(v) {
    const ret$ = wasm.echo_f64(v, 0);
    checkThrown();
    return ret$;
}

/**
 * `&[f64]` in, `Vec<f64>` out.
 * @param {Float64Array} v
 * @returns {Float64Array}
 */
export function echo_f64s(v) {
    const ptr$0 = passArrayF64ToWasm0(v, wasm.aterm_alloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_f64s(ptr$0, len$0);
    checkThrown();
    const len$ = retSlot();
    const v$ = getArrayF64FromWasm0(ret$, len$).slice();
    wasm.aterm_free(ret$, len$ * 8, 8);
    return v$;
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_i16(v) {
    const ret$ = wasm.echo_i16(v, 0);
    checkThrown();
    return ret$;
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_i32(v) {
    const ret$ = wasm.echo_i32(v, 0);
    checkThrown();
    return ret$;
}

/**
 * Returns its argument.
 * @param {bigint} v
 * @returns {bigint}
 */
export function echo_i64(v) {
    const ret$ = wasm.echo_i64(v, 0);
    checkThrown();
    return ret$;
}

/**
 * `&[i64]` in, `Vec<i64>` out (a `BigInt64Array`).
 * @param {BigInt64Array} v
 * @returns {BigInt64Array}
 */
export function echo_i64s(v) {
    const ptr$0 = passArrayI64ToWasm0(v, wasm.aterm_alloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_i64s(ptr$0, len$0);
    checkThrown();
    const len$ = retSlot();
    const v$ = getArrayI64FromWasm0(ret$, len$).slice();
    wasm.aterm_free(ret$, len$ * 8, 8);
    return v$;
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_i8(v) {
    const ret$ = wasm.echo_i8(v, 0);
    checkThrown();
    return ret$;
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_isize(v) {
    const ret$ = wasm.echo_isize(v, 0);
    checkThrown();
    return ret$;
}

/**
 * Returns its argument.
 * @param {boolean | null} [v]
 * @returns {boolean | undefined}
 */
export function echo_opt_bool(v) {
    const ret$ = wasm.echo_opt_bool(isLikeNone(v) ? 0 : 1, isLikeNone(v) ? 0 : v ? 1 : 0);
    checkThrown();
    return retSlot() === 0 ? undefined : ret$ !== 0;
}

/**
 * Returns its argument.
 * @param {string | null} [v]
 * @returns {string | undefined}
 */
export function echo_opt_char(v) {
    const char$0 = isLikeNone(v) ? 0xFFFFFF : v.codePointAt(0);
    if (char$0 !== 0xFFFFFF) { _assertChar(char$0); }
    const ret$ = wasm.echo_opt_char(char$0 === 0xFFFFFF ? 0 : 1, char$0 === 0xFFFFFF ? 0 : char$0);
    checkThrown();
    return retSlot() === 0 ? undefined : String.fromCodePoint(ret$);
}

/**
 * Returns its argument.
 * @param {number | null} [v]
 * @returns {number | undefined}
 */
export function echo_opt_f64(v) {
    const ret$ = wasm.echo_opt_f64(isLikeNone(v) ? 0 : 1, isLikeNone(v) ? 0 : v);
    checkThrown();
    return retSlot() === 0 ? undefined : ret$;
}

/**
 * Returns its argument.
 * @param {number | null} [v]
 * @returns {number | undefined}
 */
export function echo_opt_i32(v) {
    const ret$ = wasm.echo_opt_i32(isLikeNone(v) ? 0 : 1, isLikeNone(v) ? 0 : v);
    checkThrown();
    return retSlot() === 0 ? undefined : ret$;
}

/**
 * Returns its argument.
 * @param {string | null} [v]
 * @returns {string | undefined}
 */
export function echo_opt_string(v) {
    const ptr$0 = isLikeNone(v) ? 0 : passStringToWasm0(v, wasm.aterm_alloc, wasm.aterm_realloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_opt_string(ptr$0, len$0);
    checkThrown();
    if (ret$ === 0) {
        return undefined;
    }
    const len$ = retSlot();
    try {
        return getStringFromWasm0(ret$, len$);
    } finally {
        wasm.aterm_free(ret$, len$, 1);
    }
}

/**
 * Returns its argument.
 * @param {number | null} [v]
 * @returns {number | undefined}
 */
export function echo_opt_u16(v) {
    const ret$ = wasm.echo_opt_u16(isLikeNone(v) ? 0 : 1, isLikeNone(v) ? 0 : v);
    checkThrown();
    return retSlot() === 0 ? undefined : ret$;
}

/**
 * Returns its argument.
 * @param {number | null} [v]
 * @returns {number | undefined}
 */
export function echo_opt_u32(v) {
    const ret$ = wasm.echo_opt_u32(isLikeNone(v) ? 0 : 1, isLikeNone(v) ? 0 : v);
    checkThrown();
    return retSlot() === 0 ? undefined : ret$ >>> 0;
}

/**
 * Returns its argument.
 * @param {bigint | null} [v]
 * @returns {bigint | undefined}
 */
export function echo_opt_u64(v) {
    const ret$ = wasm.echo_opt_u64(isLikeNone(v) ? 0 : 1, isLikeNone(v) ? BigInt(0) : v);
    checkThrown();
    return retSlot() === 0 ? undefined : BigInt.asUintN(64, ret$);
}

/**
 * Returns its argument.
 * @param {Uint8Array | null} [v]
 * @returns {Uint8Array | undefined}
 */
export function echo_opt_vec_u8(v) {
    const ptr$0 = isLikeNone(v) ? 0 : passArrayU8ToWasm0(v, wasm.aterm_alloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_opt_vec_u8(ptr$0, len$0);
    checkThrown();
    if (ret$ === 0) {
        return undefined;
    }
    const len$ = retSlot();
    const v$ = getArrayU8FromWasm0(ret$, len$).slice();
    wasm.aterm_free(ret$, len$ * 1, 1);
    return v$;
}

/**
 * A borrowed string, returned as a new one.
 * @param {string} v
 * @returns {string}
 */
export function echo_str(v) {
    const ptr$0 = passStringToWasm0(v, wasm.aterm_alloc, wasm.aterm_realloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_str(ptr$0, len$0);
    checkThrown();
    const len$ = retSlot();
    try {
        return getStringFromWasm0(ret$, len$);
    } finally {
        wasm.aterm_free(ret$, len$, 1);
    }
}

/**
 * Returns its argument.
 * @param {string} v
 * @returns {string}
 */
export function echo_string(v) {
    const ptr$0 = passStringToWasm0(v, wasm.aterm_alloc, wasm.aterm_realloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_string(ptr$0, len$0);
    checkThrown();
    const len$ = retSlot();
    try {
        return getStringFromWasm0(ret$, len$);
    } finally {
        wasm.aterm_free(ret$, len$, 1);
    }
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_u16(v) {
    const ret$ = wasm.echo_u16(v, 0);
    checkThrown();
    return ret$;
}

/**
 * `&[u16]` in, `Vec<u16>` out.
 * @param {Uint16Array} v
 * @returns {Uint16Array}
 */
export function echo_u16s(v) {
    const ptr$0 = passArrayU16ToWasm0(v, wasm.aterm_alloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_u16s(ptr$0, len$0);
    checkThrown();
    const len$ = retSlot();
    const v$ = getArrayU16FromWasm0(ret$, len$).slice();
    wasm.aterm_free(ret$, len$ * 2, 2);
    return v$;
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_u32(v) {
    const ret$ = wasm.echo_u32(v, 0);
    checkThrown();
    return ret$ >>> 0;
}

/**
 * `&[u32]` in, `Vec<u32>` out.
 * @param {Uint32Array} v
 * @returns {Uint32Array}
 */
export function echo_u32s(v) {
    const ptr$0 = passArrayU32ToWasm0(v, wasm.aterm_alloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_u32s(ptr$0, len$0);
    checkThrown();
    const len$ = retSlot();
    const v$ = getArrayU32FromWasm0(ret$, len$).slice();
    wasm.aterm_free(ret$, len$ * 4, 4);
    return v$;
}

/**
 * Returns its argument.
 * @param {bigint} v
 * @returns {bigint}
 */
export function echo_u64(v) {
    const ret$ = wasm.echo_u64(v, 0);
    checkThrown();
    return BigInt.asUintN(64, ret$);
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_u8(v) {
    const ret$ = wasm.echo_u8(v, 0);
    checkThrown();
    return ret$;
}

/**
 * Returns its argument.
 * @param {number} v
 * @returns {number}
 */
export function echo_usize(v) {
    const ret$ = wasm.echo_usize(v, 0);
    checkThrown();
    return ret$ >>> 0;
}

/**
 * Returns its argument.
 * @param {Uint8Array} v
 * @returns {Uint8Array}
 */
export function echo_vec_u8(v) {
    const ptr$0 = passArrayU8ToWasm0(v, wasm.aterm_alloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.echo_vec_u8(ptr$0, len$0);
    checkThrown();
    const len$ = retSlot();
    const v$ = getArrayU8FromWasm0(ret$, len$).slice();
    wasm.aterm_free(ret$, len$ * 1, 1);
    return v$;
}

/**
 * `n` bytes of `fill`: enough to grow the memory under the glue's views.
 * @param {number} n
 * @param {number} fill
 * @returns {Uint8Array}
 */
export function filled(n, fill) {
    const ret$ = wasm.filled(n, 0, fill, 0);
    checkThrown();
    const len$ = retSlot();
    const v$ = getArrayU8FromWasm0(ret$, len$).slice();
    wasm.aterm_free(ret$, len$ * 1, 1);
    return v$;
}

/**
 * Install the panic hook on the host console, as a module's constructor
 * does.
 */
export function installHook() {
    wasm.installHook();
    checkThrown();
}

/**
 * Every parameter shape at once, with a non-trailing optional: `a|b|c|d|e`.
 * @param {number} a
 * @param {bigint | null | undefined} b
 * @param {string} c
 * @param {boolean | null | undefined} d
 * @param {number} e
 * @returns {string}
 */
export function mixed(a, b, c, d, e) {
    const ptr$2 = passStringToWasm0(c, wasm.aterm_alloc, wasm.aterm_realloc);
    const len$2 = WASM_VECTOR_LEN;
    const ret$ = wasm.mixed(a, 0, isLikeNone(b) ? 0 : 1, isLikeNone(b) ? BigInt(0) : b, ptr$2, len$2, isLikeNone(d) ? 0 : 1, isLikeNone(d) ? 0 : d ? 1 : 0, e, 0);
    checkThrown();
    const len$ = retSlot();
    try {
        return getStringFromWasm0(ret$, len$);
    } finally {
        wasm.aterm_free(ret$, len$, 1);
    }
}

/**
 * Panic with `msg`: a trap, after the hook (if installed) has logged it.
 * @param {string} msg
 */
export function panicWith(msg) {
    const ptr$0 = passStringToWasm0(msg, wasm.aterm_alloc, wasm.aterm_realloc);
    const len$0 = WASM_VECTOR_LEN;
    wasm.panicWith(ptr$0, len$0);
    checkThrown();
}

/**
 * `performance.now()` through the `aterm_host` import.
 * @returns {number}
 */
export function performance_now() {
    const ret$ = wasm.performance_now();
    checkThrown();
    return ret$;
}

/**
 * Rust keywords as parameters, written raw: the descriptor records them
 * without the `r#`.
 * @param {number} type
 * @param {number} _in
 * @returns {number}
 */
export function raw_idents(type, _in) {
    const ret$ = wasm.raw_idents(type, 0, _in, 0);
    checkThrown();
    return ret$ >>> 0;
}

/**
 * Parameters JS cannot name as written.
 * @param {number} _function
 * @param {number} _this
 * @returns {number}
 */
export function reserved(_function, _this) {
    const ret$ = wasm.reserved(_function, 0, _this, 0);
    checkThrown();
    return ret$ >>> 0;
}

/**
 * Trailing optionals, which JS may leave out: `a + b + len(c)`.
 * @param {number} a
 * @param {number | null} [b]
 * @param {string | null} [c]
 * @returns {number}
 */
export function trailing(a, b, c) {
    const ptr$2 = isLikeNone(c) ? 0 : passStringToWasm0(c, wasm.aterm_alloc, wasm.aterm_realloc);
    const len$2 = WASM_VECTOR_LEN;
    const ret$ = wasm.trailing(a, 0, isLikeNone(b) ? 0 : 1, isLikeNone(b) ? 0 : b, ptr$2, len$2);
    checkThrown();
    return ret$ >>> 0;
}

/**
 * Nothing in, nothing out.
 */
export function unit() {
    wasm.unit();
    checkThrown();
}

/**
 * The UTF-8 length of `text`, which crossed as UTF-16 in JS.
 * @param {string} text
 * @returns {number}
 */
export function utf8_len(text) {
    const ptr$0 = passStringToWasm0(text, wasm.aterm_alloc, wasm.aterm_realloc);
    const len$0 = WASM_VECTOR_LEN;
    const ret$ = wasm.utf8_len(ptr$0, len$0);
    checkThrown();
    return ret$ >>> 0;
}

function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        console_error: function(arg0, arg1) {
            console.error(getStringFromWasm0(arg0, arg1));
        },
        date_now: function() {
            return Date.now();
        },
        error_stack: function(arg0) {
            const ret = new Error().stack ?? '';
            const ptr = passStringToWasm0(ret, wasm.aterm_alloc, wasm.aterm_realloc);
            const len = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr, true);
        },
        performance_now: function() {
            return performance.now();
        },
    };
    return {
        __proto__: null,
        "aterm_host": import0,
    };
}

const CounterFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__aterm_free_counter(ptr >>> 0, 1));
const SnapshotFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__aterm_free_snapshot(ptr >>> 0, 1));

function _assertChar(c) {
    if (typeof(c) === 'number' && (c >= 0x110000 || (c >= 0xD800 && c < 0xE000))) throw new Error(`expected a valid Unicode scalar value, found ${c}`);
}

function getArrayF32FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getFloat32ArrayMemory0().subarray(ptr / 4, ptr / 4 + len);
}

function getArrayF64FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getFloat64ArrayMemory0().subarray(ptr / 8, ptr / 8 + len);
}

function getArrayI64FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getBigInt64ArrayMemory0().subarray(ptr / 8, ptr / 8 + len);
}

function getArrayU16FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint16ArrayMemory0().subarray(ptr / 2, ptr / 2 + len);
}

function getArrayU32FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint32ArrayMemory0().subarray(ptr / 4, ptr / 4 + len);
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

let cachedBigInt64ArrayMemory0 = null;
function getBigInt64ArrayMemory0() {
    if (cachedBigInt64ArrayMemory0 === null || cachedBigInt64ArrayMemory0.byteLength === 0) {
        cachedBigInt64ArrayMemory0 = new BigInt64Array(wasm.memory.buffer);
    }
    return cachedBigInt64ArrayMemory0;
}

let cachedFloat32ArrayMemory0 = null;
function getFloat32ArrayMemory0() {
    if (cachedFloat32ArrayMemory0 === null || cachedFloat32ArrayMemory0.byteLength === 0) {
        cachedFloat32ArrayMemory0 = new Float32Array(wasm.memory.buffer);
    }
    return cachedFloat32ArrayMemory0;
}

let cachedFloat64ArrayMemory0 = null;
function getFloat64ArrayMemory0() {
    if (cachedFloat64ArrayMemory0 === null || cachedFloat64ArrayMemory0.byteLength === 0) {
        cachedFloat64ArrayMemory0 = new Float64Array(wasm.memory.buffer);
    }
    return cachedFloat64ArrayMemory0;
}

let cachedUint16ArrayMemory0 = null;
function getUint16ArrayMemory0() {
    if (cachedUint16ArrayMemory0 === null || cachedUint16ArrayMemory0.byteLength === 0) {
        cachedUint16ArrayMemory0 = new Uint16Array(wasm.memory.buffer);
    }
    return cachedUint16ArrayMemory0;
}

let cachedUint32ArrayMemory0 = null;
function getUint32ArrayMemory0() {
    if (cachedUint32ArrayMemory0 === null || cachedUint32ArrayMemory0.byteLength === 0) {
        cachedUint32ArrayMemory0 = new Uint32Array(wasm.memory.buffer);
    }
    return cachedUint32ArrayMemory0;
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function passArrayF32ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 4, 4) >>> 0;
    getFloat32ArrayMemory0().set(arg, ptr / 4);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayF64ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 8, 8) >>> 0;
    getFloat64ArrayMemory0().set(arg, ptr / 8);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayI64ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 8, 8) >>> 0;
    getBigInt64ArrayMemory0().set(arg, ptr / 8);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayU16ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 2, 2) >>> 0;
    getUint16ArrayMemory0().set(arg, ptr / 2);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayU32ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 4, 4) >>> 0;
    getUint32ArrayMemory0().set(arg, ptr / 4);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayU8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let RET_SLOT = 0;
function checkThrown() {
    const slot = getUint32ArrayMemory0();
    const at = RET_SLOT >>> 2;
    const kind = slot[at + 1];
    if (kind === 0) return;
    slot[at + 1] = 0;
    const ptr = slot[at + 2];
    const len = slot[at + 3];
    const msg = getStringFromWasm0(ptr, len);
    wasm.aterm_free(ptr, len, 1);
    throw kind === 1 ? new Error(msg) : msg;
}

function retSlot() {
    return getUint32ArrayMemory0()[RET_SLOT >>> 2];
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedBigInt64ArrayMemory0 = null;
    cachedFloat32ArrayMemory0 = null;
    cachedFloat64ArrayMemory0 = null;
    cachedUint16ArrayMemory0 = null;
    cachedUint32ArrayMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    RET_SLOT = wasm.aterm_ret_slot() >>> 0;
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL("aterm_wasm_abi_toy_bg.wasm", import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
