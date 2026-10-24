// Copyright (c) 2014 Alex Crichton
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from wasm-bindgen-cli-support 0.2.108 src/js/mod.rs and modified by the aterm project.
// See ../../LICENSE-MIT for the upstream license; Apache-2.0 is the workspace's.

//! Fixed JavaScript and TypeScript glue templates assembled by `js.rs`.

/// The text after the `use strict`-free header line.
pub(crate) const DTS_HEADER: &str = "/* tslint:disable */\n/* eslint-disable */\n";

/// `_assertChar`.
pub(crate) const ASSERT_CHAR: &str = "\
function _assertChar(c) {
    if (typeof(c) === 'number' && (c >= 0x110000 || (c >= 0xD800 && c < 0xE000))) throw new Error(`expected a valid Unicode scalar value, found ${c}`);
}
";

/// `isLikeNone`.
pub(crate) const IS_LIKE_NONE: &str = "\
function isLikeNone(x) {
    return x === undefined || x === null;
}
";

/// `getDataViewMemory0`.
pub(crate) const DATA_VIEW: &str = "\
let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}
";

/// A cached typed view of the memory; `{T}` is the typed array.
pub(crate) const VIEW: &str = "\
let cached{T}Memory0 = null;
function get{T}Memory0() {
    if (cached{T}Memory0 === null || cached{T}Memory0.byteLength === 0) {
        cached{T}Memory0 = new {T}(wasm.memory.buffer);
    }
    return cached{T}Memory0;
}
";

/// `getArray{S}FromWasm0`: a view of `len` elements; `{T}` the typed array, `{N}` its element size.
pub(crate) const GET_ARRAY: &str = "\
function getArray{S}FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return get{T}Memory0().subarray(ptr / {N}, ptr / {N} + len);
}
";

/// `passArray{S}ToWasm0`: copy an array-like into a new allocation.
pub(crate) const PASS_ARRAY: &str = "\
function passArray{S}ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * {N}, {N}) >>> 0;
    get{T}Memory0().set(arg, ptr / {N});
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}
";

/// `getStringFromWasm0` and its decoder ([`DECODE_TEXT_WEB`]).
pub(crate) const GET_STRING: &str = "\
function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
{DECODE}";

/// `decodeText` for a page.
pub(crate) const DECODE_TEXT_WEB: &str = "\
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
";

/// `passStringToWasm0` and its encoder.
pub(crate) const PASS_STRING: &str = "\
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
";

/// The buffer length the last `pass*ToWasm0` wrote.
pub(crate) const VECTOR_LEN: &str = "\
let WASM_VECTOR_LEN = 0;
";

/// The return slot (`crates/aterm-wasm-abi/src/mem.rs`, `Slot`) and the check after every call.
pub(crate) const RET_SLOT: &str = "\
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
    wasm.{FREE}(ptr, len, 1);
    throw kind === {ERROR} ? new Error(msg) : msg;
}
";

/// The statement after every call into the module.
pub(crate) const CHECK_THROWN: &str = "checkThrown();";

/// The slot's value word: a returned length or a presence flag.
pub(crate) const RET_VALUE: &str = "\
function retSlot() {
    return getUint32ArrayMemory0()[RET_SLOT >>> 2];
}
";

/// A constructor's statements after its call: adopt the handle `ret$`.
pub(crate) const CONSTRUCTOR_TAIL: &str = "\
this.__wbg_ptr = ret$ >>> 0;
{C}Finalization.register(this, this.__wbg_ptr, this);
return this;";

/// After a class: `using` frees it.
pub(crate) const DISPOSE_ALIAS: &str =
    "if (Symbol.dispose) {C}.prototype[Symbol.dispose] = {C}.prototype.free;\n";

/// `aterm_host.console_error(ptr, len)`.
pub(crate) const CONSOLE_ERROR_SHIM: &str = "\
function(arg0, arg1) {
            console.error(getStringFromWasm0(arg0, arg1));
        }";

/// `aterm_host.error_stack(out)`.
pub(crate) const ERROR_STACK_SHIM: &str = "\
function(arg0) {
            const ret = new Error().stack ?? '';
            const ptr = {PASS};
            const len = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr, true);
        }";

/// The object table.
pub(crate) const HEAP: &str = "\
const heap = [undefined];
const heapFree = [];
function addHeapObject(obj) {
    const idx = heapFree.length > 0 ? heapFree.pop() : heap.length;
    heap[idx] = obj;
    return idx;
}
function getObject(idx) {
    return heap[idx];
}
function dropObject(idx) {
    if (idx === 0 || idx >= heap.length) return;
    heap[idx] = undefined;
    heapFree.push(idx);
}
";

/// `aterm_host.object_drop(index)`.
pub(crate) const OBJECT_DROP_SHIM: &str = "\
function(arg0) {
            dropObject(arg0);
        }";

/// `aterm_host.performance_now()`.
pub(crate) const PERFORMANCE_NOW_SHIM: &str = "\
function() {
            return performance.now();
        }";

/// `aterm_host.date_now()`.
pub(crate) const DATE_NOW_SHIM: &str = "\
function() {
            return Date.now();
        }";

/// `aterm_host.random_fill(ptr, len)`: unavailable entropy is a refused fill.
pub(crate) const RANDOM_FILL_SHIM: &str = "\
function(ptr, len) {
            try {
                ptr = ptr >>> 0;
                len = len >>> 0;
                const memory = wasm.memory.buffer;
                if (len > 65536 || ptr + len > memory.byteLength) return 0;
                const crypto = globalThis.crypto;
                if (!crypto || typeof crypto.getRandomValues !== 'function') return 0;
                crypto.getRandomValues(new Uint8Array(memory, ptr, len));
                return 1;
            } catch (_) {
                return 0;
            }
        }";

/// A class's handle plumbing; `{C}` the class, `{FREE}` its free export.
pub(crate) const CLASS_HEAD: &str = "    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create({C}.prototype);
        obj.__wbg_ptr = ptr;
        {C}Finalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        {C}Finalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.{FREE}(ptr, 0);
        checkThrown();
    }
";

/// A class's FinalizationRegistry, inert where the host has none.
pub(crate) const FINALIZATION: &str = "\
const {C}Finalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.{FREE}(ptr >>> 0, 1));
";

/// The `--target web` loader.
pub(crate) const WEB_LOADER: &str = "\
let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
{RESET}{INIT}    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn(\"`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\\n\", e);

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
        module_or_path = new URL({FILE}, import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
";

/// The `--target web` declarations after the module's own.
pub(crate) const WEB_DTS_INIT: &str = "\
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
";

/// `InitInput`, before `InitOutput`.
pub(crate) const WEB_DTS_INPUT: &str =
    "export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;\n";
