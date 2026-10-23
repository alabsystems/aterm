// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Drive actual generated glue with RAM mocks, in a page: the glue's
// `initSync` meets a mock `WebAssembly` (its module bytes are empty, its
// instance's exports are JS), and each canvas a mock WebGL2 context, so no
// Wasm or GL engine is invoked. `run({ glue })` imports the glue by URL;
// the markers go to the console, and a missed case throws.
//
// The fence: for the glue's whole run (its import included) the page's way
// to the network, a real GPU, timers and the real Wasm engine is fenced, as
// node's sandbox (vm.runInNewContext with only the globals it listed) fenced
// it. A touch throws and is recorded, and any touch fails the run, whether
// the glue caught the throw or not.

// What the fence covers: [object, property] pairs, the prototypes' first
// (OffscreenCanvas is one of the globals).
const FENCED = () => [
    [HTMLCanvasElement.prototype, 'getContext'],
    [OffscreenCanvas.prototype, 'getContext'],
    [Navigator.prototype, 'gpu'],
    [Navigator.prototype, 'sendBeacon'],
    ...['fetch', 'XMLHttpRequest', 'WebSocket', 'EventSource', 'Request', 'Response', 'URL',
        'Worker', 'SharedWorker', 'WebGLRenderingContext', 'WebGL2RenderingContext',
        'OffscreenCanvas', 'setTimeout', 'setInterval', 'requestAnimationFrame',
        'queueMicrotask', 'performance'].map((name) => [globalThis, name]),
];

// Fence `name` on `target`: a read records it in `touched` and throws.
// Answers what puts the property back.
function fence(target, name, touched) {
    const own = Object.getOwnPropertyDescriptor(target, name);
    Object.defineProperty(target, name, {
        configurable: true,
        get() {
            touched.push(name);
            throw new Error(`GL_LIFETIME_FENCE: the glue reached ${name}`);
        },
    });
    return () => (own ? Object.defineProperty(target, name, own) : delete target[name]);
}

function fail(message) {
    throw new Error(message);
}
const assert = {
    equal(got, want, message) {
        if (!Object.is(got, want)) fail(`${message ?? 'equal'}: got ${String(got)}, want ${String(want)}`);
    },
    ok(value, message) {
        if (!value) fail(message ?? 'not ok');
    },
    deepEqual(got, want, message) {
        const [g, w] = [JSON.stringify(got), JSON.stringify(want)];
        if (g !== w) fail(`${message ?? 'deepEqual'}: got ${g}, want ${w}`);
    },
};

export async function run({ glue }) {
    const touched = [];
    const engine = globalThis.WebAssembly;
    const restore = FENCED().map(([target, name]) => fence(target, name, touched));
    let failure;
    try {
        await lifetimes(glue);
    } catch (e) {
        failure = e;
    } finally {
        for (const put of restore.reverse()) put();
        globalThis.WebAssembly = engine;
    }
    // A touch is named first: the glue may have caught the throw, or failed
    // on it further on.
    assert.deepEqual(touched, [], `GL_LIFETIME_FENCE: the glue reached ${touched.join(', ')}`);
    if (failure) throw failure;
    console.log('GL_LIFETIME_ALL_RETIRED');
}

async function lifetimes(glue) {
    const memory = { buffer: new ArrayBuffer(65536) };
    const live = new Map();
    const freed = [];
    let next = 1024;
    let imports;
    let active;
    const VERSION = 0x1F02;
    const RENDERER = 0x1F01;
    const utf8 = new TextEncoder();
    function canvas(label) {
        const gl = {
            label,
            uniform: undefined,
            getExtension: () => null,
            getParameter: (key) => key === VERSION ? `WebGL 2 ${label} Ω` : `renderer ${label}`,
            createProgram: () => ({ kind: 'program', context: label }),
            deleteProgram: (program) => { assert.equal(program.context, label); },
            getUniformLocation: (program, name) => ({ kind: 'location', program, name }),
            uniform1i: (location) => { gl.uniform = location; },
        };
        const value = { getContext: (kind) => { assert.equal(kind, 'webgl2'); return gl; } };
        gl.canvas = value;
        return { value, gl };
    }
    globalThis.WebAssembly = {
        Module: class { constructor(bytes) { assert.equal(bytes.length, 0); } },
        Instance: class {
            constructor(module, bindings) {
                imports = bindings;
                this.exports = {
                    memory,
                    aterm_ret_slot: () => 0,
                    aterm_alloc: (len, alignment) => {
                        assert.equal(alignment, 1);
                        const ptr = next;
                        next += len;
                        assert.ok(next < memory.buffer.byteLength);
                        live.set(ptr, { len, alignment });
                        return ptr;
                    },
                    aterm_free: (ptr, len, alignment) => {
                        assert.deepEqual(live.get(ptr), { len, alignment }, 'exact owned allocation');
                        live.delete(ptr);
                        freed.push({ ptr, len, alignment });
                    },
                    create_context: (slot, zero) => {
                        assert.equal(zero, 0);
                        try { return imports.aterm_gl.context_create(slot); }
                        finally { imports.aterm_host.object_drop(slot); }
                    },
                };
            }
        },
    };
    // The mock stays in place until run's end: the glue meets no real engine.
    const box = await import(glue);
    // Opaque mock module bytes, never actual Wasm.
    box.initSync({ module: new Uint8Array() });
    const gl = imports.aterm_gl;
    function enter(id) { assert.equal(gl.make_current(id), 1); active = id; }
    function textAt(ptr) {
        const mem = new Uint8Array(memory.buffer);
        let end = ptr;
        while (end < mem.length && mem[end] !== 0) end++;
        assert.ok(end < mem.length, 'actual generated string is terminated');
        return new TextDecoder().decode(mem.subarray(ptr, end));
    }
    function cName(text, ptr) {
        const bytes = utf8.encode(text);
        new Uint8Array(memory.buffer).set(bytes, ptr);
        new Uint8Array(memory.buffer)[ptr + bytes.length] = 0;
        return ptr;
    }
    function location(program, ptr, name) {
        const slot = gl.glGetUniformLocation(program, cName(name, ptr));
        assert.ok(slot > 0);
        return slot;
    }
    function observed(glMock, slot) { gl.glUniform1i(slot, 0); return glMock.uniform; }

    assert.equal(box.create_context(undefined), 0, 'non-canvas refuses');
    const a = canvas('a');
    const aid = box.create_context(a.value);
    assert.ok(aid > 0);
    enter(aid);
    const av = gl.glGetString(VERSION);
    const ar = gl.glGetString(RENDERER);
    assert.equal(textAt(av), 'WebGL 2 a Ω');
    assert.equal(textAt(ar), 'renderer a');
    assert.equal(gl.glGetString(VERSION), av, 'same-context cache positive');
    assert.equal(live.size, 2);
    const a1 = gl.glCreateProgram();
    const a2 = gl.glCreateProgram();
    const a1x = location(a1, 128, 'a1x');
    const a1y = location(a1, 160, 'a1y');
    const a2x = location(a2, 192, 'a2x');
    assert.equal(observed(a.gl, a1x).program.context, 'a');
    assert.equal(observed(a.gl, a2x).name, 'a2x');

    const b = canvas('b');
    const bid = box.create_context(b.value);
    assert.ok(bid > 0 && bid !== aid);
    enter(bid);
    const bv = gl.glGetString(VERSION);
    assert.equal(textAt(bv), 'WebGL 2 b Ω');
    assert.equal(live.size, 3);
    const bp = gl.glCreateProgram();
    const bx = location(bp, 224, 'bx');
    assert.equal(observed(b.gl, bx).program.context, 'b');
    console.log('GL_LIFETIME_ADMITTED_CONTROLS_REACHED');

    enter(aid);
    gl.glDeleteProgram(a1);
    const a1Retired = observed(a.gl, a1x) === null && observed(a.gl, a1y) === null;
    assert.equal(observed(a.gl, a2x).name, 'a2x', 'other program remains live');
    assert.equal(live.size, 3, 'program deletion never frees context strings');
    // Retire a non-current context: current A and all A allocations/locations stay live.
    gl.context_drop(bid);
    const bRetired = observed(a.gl, bx) === null;
    assert.equal(gl.glGetString(VERSION), av, 'current context remains A');
    assert.equal(observed(a.gl, a2x).name, 'a2x', 'other context remains live');
    assert.ok(live.has(av) && live.has(ar));
    const bStringsRetired = !live.has(bv) && freed.length === 1
        && freed[0].ptr === bv && freed[0].len === utf8.encode('WebGL 2 b Ω').length + 1
        && freed[0].alignment === 1;
    const freedBeforeDuplicate = freed.length;
    gl.context_drop(bid);
    assert.equal(freed.length, freedBeforeDuplicate, 'duplicate context drop is inert');
    assert.equal(gl.make_current(bid), 0, 'retired context is unavailable');
    enter(aid);
    gl.context_drop(aid);
    const aStringsRetired = !live.has(av) && !live.has(ar) && live.size === 0;
    const c = canvas('c');
    const cid = box.create_context(c.value);
    enter(cid);
    const a2Retired = observed(c.gl, a2x) === null;
    gl.context_drop(cid);
    console.log('GL_LIFETIME_RETIREMENT_CASES_REACHED');
    const missed = [];
    for (const [name, pass] of [
        ['deleted program locations', a1Retired],
        ['retired context locations', bRetired && a2Retired],
        ['only retired context strings', bStringsRetired],
        ['all retired context strings', aStringsRetired],
    ]) if (!pass) missed.push(name);
    assert.deepEqual(missed, [], `GL_LIFETIME_RETIREMENT_MISSING: ${missed.join(', ')}`);
}
