// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The toy module under node (`verify wasm-glue`): every boundary type
// aterm-wasm-abi carries, through the glue `verify glue` wrote, held to
// the JS value wasm-bindgen gives the same Rust type, compared type-exactly
// (BigInt, undefined, typed arrays, -0, thrown strings vs Errors). Run on
// the glue wasm-bindgen 0.2.108 writes for a #[wasm_bindgen] twin of the
// toy (the check's oracle row, crates/aterm-verify/src/wasm_glue/oracle.rs),
// every check holds but the two marked STRICTER (`another class`, and the
// misuse throws).
//
//   node --expose-gc drive.mjs <dir>
//
// <dir> holds nodejs/ and web/, the glue of both flavors. The suite runs on
// the CommonJS module and on the ES module after `init`; the ES module's
// init forms (init/initSync, object and deprecated positional arguments,
// bytes, a module, a promise, a Response) each run on a fresh copy.
//
// A failed check prints one line; each section prints its count; exit 0
// only when every check held.

import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

const STEM = 'aterm_wasm_abi_toy'
const dir = process.argv[2]
if (!dir) {
  console.error('usage: node --expose-gc drive.mjs <dir with nodejs/ and web/ glue>')
  process.exit(2)
}

let failures = 0
let section = { name: '', checks: 0 }

function begin(name) {
  section = { name, checks: 0 }
}

function end() {
  console.error(`wasm-glue: ${section.name}: ${section.checks} checks`)
}

function check(name, ok, detail) {
  section.checks++
  if (!ok) {
    failures++
    console.error(`wasm-glue: FAIL ${section.name} ${name}: ${detail}`)
  }
}

function show(v) {
  if (typeof v === 'bigint') return `${v}n`
  if (typeof v === 'string') return JSON.stringify(v)
  if (ArrayBuffer.isView(v)) return `${v.constructor.name}[${Array.from(v, show).join(', ')}]`
  if (Object.is(v, -0)) return '-0'
  return String(v)
}

// Same type and value: Object.is for scalars, element by element for typed
// arrays of the same constructor.
function same(a, b) {
  if (ArrayBuffer.isView(a) || ArrayBuffer.isView(b)) {
    return (
      ArrayBuffer.isView(a) &&
      ArrayBuffer.isView(b) &&
      a.constructor === b.constructor &&
      a.length === b.length &&
      Array.from(a).every((x, i) => Object.is(x, b[i]))
    )
  }
  return Object.is(a, b)
}

function eq(name, got, want) {
  check(name, same(got, want), `got ${show(got)}, want ${show(want)}`)
}

// `f()` returns `want`; a throw is a failed check, not the driver's death.
function eqCall(name, f, want) {
  let got
  try {
    got = f()
  } catch (e) {
    check(name, false, `threw ${e?.constructor?.name}: ${e?.message ?? show(e)}`)
    return
  }
  eq(name, got, want)
}

const NOT_THROWN = Symbol('not thrown')
function thrown(f) {
  try {
    f()
    return NOT_THROWN
  } catch (e) {
    return e
  }
}

// `f` throws an Error (not a subclass) with exactly `msg`.
function throwsError(name, f, msg) {
  const e = thrown(f)
  check(
    name,
    e !== NOT_THROWN && Object.getPrototypeOf(e) === Error.prototype && e.message === msg,
    `threw ${e === NOT_THROWN ? 'nothing' : `${e?.constructor?.name}: ${e?.message ?? show(e)}`}`
  )
}

// `f` throws exactly the string `s`.
function throwsString(name, f, s) {
  const e = thrown(f)
  check(name, typeof e === 'string' && e === s, `threw ${e === NOT_THROWN ? 'nothing' : show(e)}`)
}

function captureConsole(method, f) {
  const calls = []
  const saved = console[method]
  console[method] = (...args) => calls.push(args)
  try {
    return { result: thrown(f), calls }
  } finally {
    console[method] = saved
  }
}

const P64 = 2n ** 64n - 1n

function suite(label, m) {
  begin(`${label} scalars`)
  eq('bool true', m.echo_bool(true), true)
  eq('bool false', m.echo_bool(false), false)
  eq('bool from 2', m.echo_bool(2), true)
  eq('u8 255', m.echo_u8(255), 255)
  eq('u8 wraps 256', m.echo_u8(256), 0)
  eq('u8 wraps -1', m.echo_u8(-1), 255)
  eq('u16 max', m.echo_u16(65535), 65535)
  eq('u16 wraps', m.echo_u16(65536), 0)
  eq('u32 max is unsigned', m.echo_u32(0xffffffff), 4294967295)
  eq('u32 from -1', m.echo_u32(-1), 4294967295)
  eq('usize max', m.echo_usize(4294967295), 4294967295)
  eq('i8 min', m.echo_i8(-128), -128)
  eq('i8 wraps 128', m.echo_i8(128), -128)
  eq('i16 min', m.echo_i16(-32768), -32768)
  eq('i32 -1', m.echo_i32(-1), -1)
  eq('i32 wraps 2^31', m.echo_i32(2 ** 31), -(2 ** 31))
  eq('isize -5', m.echo_isize(-5), -5)
  eq('u64 max is a BigInt', m.echo_u64(P64), P64)
  eq('u64 zero', m.echo_u64(0n), 0n)
  eq('i64 -1', m.echo_i64(-1n), -1n)
  eq('i64 min', m.echo_i64(-(2n ** 63n)), -(2n ** 63n))
  check('u64 refuses a Number', thrown(() => m.echo_u64(5)) instanceof TypeError, 'no TypeError')
  eq('f32 rounds', m.echo_f32(0.1), Math.fround(0.1))
  eq('f64 NaN', m.echo_f64(NaN), NaN)
  eq('f64 -0', m.echo_f64(-0), -0)
  eq('f64 Infinity', m.echo_f64(Infinity), Infinity)
  eq('char BMP', m.echo_char('€'), '€')
  eq('char astral', m.echo_char('😀'), '😀')
  eq('char takes the first code point', m.echo_char('ab'), 'a')
  throwsError('char lone surrogate', () => m.echo_char('\uD800'), 'expected a valid Unicode scalar value, found 55296')
  end()

  begin(`${label} options`)
  eq('u32 undefined', m.echo_opt_u32(undefined), undefined)
  eq('u32 null', m.echo_opt_u32(null), undefined)
  eq('u32 omitted', m.echo_opt_u32(), undefined)
  eq('u32 zero is Some', m.echo_opt_u32(0), 0)
  eq('u32 max', m.echo_opt_u32(4294967295), 4294967295)
  eq('bool false is Some', m.echo_opt_bool(false), false)
  eq('bool true', m.echo_opt_bool(true), true)
  eq('bool by truthiness', m.echo_opt_bool('yes'), true)
  eq('bool none', m.echo_opt_bool(undefined), undefined)
  eq('u16', m.echo_opt_u16(7), 7)
  eq('i32 negative', m.echo_opt_i32(-7), -7)
  eq('u64 none', m.echo_opt_u64(undefined), undefined)
  eq('u64 max', m.echo_opt_u64(P64), P64)
  eq('f64 null', m.echo_opt_f64(null), undefined)
  eq('f64 NaN is Some', m.echo_opt_f64(NaN), NaN)
  eq('char none', m.echo_opt_char(undefined), undefined)
  eq('char', m.echo_opt_char('x'), 'x')
  eq('string none', m.echo_opt_string(undefined), undefined)
  eq('empty string is Some', m.echo_opt_string(''), '')
  eq('string', m.echo_opt_string('héllo'), 'héllo')
  eq('bytes none', m.echo_opt_vec_u8(null), undefined)
  eq('bytes', m.echo_opt_vec_u8(new Uint8Array([1, 2])), new Uint8Array([1, 2]))
  eq('empty bytes are Some', m.echo_opt_vec_u8(new Uint8Array(0)), new Uint8Array(0))
  end()

  begin(`${label} strings`)
  for (const s of ['', 'ascii', 'héllo — ünïcödé 😀', '\0nul', 'x'.repeat(100000) + 'é']) {
    eq(`str ${s.length}`, m.echo_str(s), s)
    eq(`string ${s.length}`, m.echo_string(s), s)
  }
  eq('utf8 length of an astral char', m.utf8_len('😀'), 4)
  eq('utf8 length of é', m.utf8_len('é'), 2)
  end()

  begin(`${label} buffers`)
  const input = new Uint8Array([0, 1, 255])
  const out = m.echo_bytes(input)
  eq('bytes', out, input)
  check('bytes are a copy', out !== input && out.buffer.byteLength === out.length, 'a view, not a copy')
  eq('bytes from an array', m.echo_bytes([1, 2, 3]), new Uint8Array([1, 2, 3]))
  eq('vec u8', m.echo_vec_u8(new Uint8Array([9])), new Uint8Array([9]))
  eq('u32s', m.echo_u32s(new Uint32Array([0, 1, 0xffffffff])), new Uint32Array([0, 1, 0xffffffff]))
  eq('empty u32s', m.echo_u32s([]), new Uint32Array(0))
  eq('i64s', m.echo_i64s(new BigInt64Array([-1n, 2n])), new BigInt64Array([-1n, 2n]))
  eq('f64s keep -0', m.echo_f64s([0.5, -0, Infinity]), new Float64Array([0.5, -0, Infinity]))
  eq('f32s', m.echo_f32s([0.1]), new Float32Array([0.1]))
  eq('u16s', m.echo_u16s([65535, 1]), new Uint16Array([65535, 1]))
  const big = m.filled(64 * 1024 * 1024, 7)
  check('a buffer that grows the memory', big.length === 64 * 1024 * 1024 && big[0] === 7 && big[big.length - 1] === 7, `length ${big.length}`)
  eq('strings after the growth', m.echo_str('après'), 'après')
  end()

  begin(`${label} shapes`)
  eq('mixed', m.mixed(1, undefined, 'c', true, 1.5), '1|None|c|Some(true)|1.5')
  eq('mixed again', m.mixed(255, 7n, '', null, 0.25), '255|Some(7)||None|0.25')
  eq('trailing omitted', m.trailing(1), 1)
  eq('trailing one', m.trailing(1, 2), 3)
  eq('trailing both', m.trailing(1, 2, 'abc'), 6)
  eq('trailing null', m.trailing(1, null, 'é'), 3)
  eq('reserved names', m.reserved(1, 2), 12)
  eq('raw names', m.raw_idents(1, 2), 12)
  eq('ok', m.checked(4, ''), 8)
  throwsString('err is the string', () => m.checked(4, 'bad input'), 'bad input')
  eq('ok some', m.checked_opt('x'), 'X')
  eq('ok none', m.checked_opt(''), undefined)
  throwsString('err of an option', () => m.checked_opt('!'), 'bang')
  eq('unit', m.unit(), undefined)
  end()

  begin(`${label} classes`)
  const { Counter, Snapshot } = m
  const c = new Counter(5n)
  check('instance', c instanceof Counter, 'not a Counter')
  eq('getter', c.value, 5n)
  throwsString('constructor err', () => new Counter(-1n), 'negative start -1')
  eq('method', c.add(2n), 7n)
  eq('note', c.note('a'), 1)
  eq('note again', c.note('ünï'), 2)
  eq('string getter', c.log, 'a;ünï')
  const desc = Object.getOwnPropertyDescriptor(Counter.prototype, 'value')
  check('getter is read-only', typeof desc?.get === 'function' && desc.set === undefined, 'not a get-only accessor')
  check('assigning a getter throws', thrown(() => { c.value = 1n }) instanceof TypeError, 'no TypeError')
  eq('renamed method', c.describe(), 'Counter(7)')
  eq('Rust name is not exported', c.describe_counter, undefined)
  const s = c.snapshot()
  check('class by value', s instanceof Snapshot, 'not a Snapshot')
  eq('its getters', s.value, 7n)
  eq('its getters 2', s.notes, 2)
  check('Some(class)', c.find('ün') instanceof Snapshot, 'not a Snapshot')
  eq('None(class)', c.find('zzz'), undefined)
  const seeded = Counter.from_seed(3)
  check('static returns an instance', seeded instanceof Counter && seeded.value === 3n, 'wrong instance')
  throwsError('no constructor: null handle', () => new Snapshot().value, 'null pointer passed to rust')
  const before = m.drops()
  const d = new Counter(1n)
  d[Symbol.dispose]()
  eq('dispose drops', m.drops(), before + 1)
  throwsError('use after dispose', () => d.value, 'null pointer passed to rust')
  const f = new Counter(1n)
  f.free()
  eq('free drops', m.drops(), before + 2)
  eq('free zeroes the handle', f.__wbg_ptr, 0)
  throwsError('use after free', () => f.add(1n), 'null pointer passed to rust')
  throwsError('free twice', () => f.free(), 'null pointer passed to rust')
  // STRICTER than wasm-bindgen, which reads the handle as the wrong type.
  throwsError('another class', () => desc.get.call(s), 'an object of another class was passed to rust')
  s.free()
  seeded.free()
  end()

  // A throw returns from the export first and waits in the return slot, so
  // no throw abandons a wasm frame or leaks the module's shadow stack: 100k
  // Errs of each shape, then ordinary calls still work. Thrown through a JS
  // import instead, each leaked ~48 bytes and the module died near 22k
  // throws; wasm-bindgen 0.2.108 returns an Err this way too, and passes.
  begin(`${label} throws`)
  const N = 100_000
  let errs = 0
  let ctorErrs = 0
  let last = null
  for (let i = 0; i < N; i++) {
    try {
      m.checked(1, 'x')
    } catch (e) {
      if (e === 'x') errs++
      else last = e
    }
  }
  for (let i = 0; i < N; i++) {
    try {
      new Counter(-1n)
    } catch (e) {
      if (e === 'negative start -1') ctorErrs++
      else last = e
    }
  }
  const lastly = last === null ? '' : `, then ${last?.constructor?.name}: ${last?.message ?? show(last)}`
  check('every Err thrown as its string', errs === N, `${errs} of ${N}${lastly}`)
  check('every constructor Err thrown', ctorErrs === N, `${ctorErrs} of ${N}${lastly}`)
  alive(m)
  end()

  // Before any trap: std still counts a panic a trap interrupted, so every
  // later panic in this instance aborts without reaching a hook.
  begin(`${label} host`)
  const panic = captureConsole('error', () => {
    m.installHook()
    m.panicWith('boom here')
  })
  check('panic traps', panic.result instanceof WebAssembly.RuntimeError, `threw ${panic.result}`)
  check('console.error once', panic.calls.length === 1, `${panic.calls.length} calls`)
  const msg = String(panic.calls[0]?.[0] ?? '')
  check('message first', msg.startsWith('panicked at ') && msg.includes('boom here\n\nStack:\n\n'), JSON.stringify(msg.slice(0, 120)))
  check('then the stack, with a wasm frame', /wasm/.test(msg.split('\n\nStack:\n\n')[1] ?? ''), 'no wasm frame')
  check('then a blank line', msg.endsWith('\n\n'), 'no trailing blank line')
  const t0 = Date.now()
  const now = m.date_now()
  const t1 = Date.now()
  check('Date.now import', t0 <= now && now <= t1, `${t0} <= ${now} <= ${t1}`)
  const q0 = performance.now()
  const pnow = m.performance_now()
  const q1 = performance.now()
  check('performance.now import', q0 <= pnow && pnow <= q1, `${q0} <= ${pnow} <= ${q1}`)
  end()

  begin(`${label} poison`)
  const p = new Counter(1n)
  const dropsBefore = m.drops()
  const boom = captureConsole('error', () => p.boom('kaboom'))
  check('a panic traps', boom.result instanceof WebAssembly.RuntimeError, `threw ${boom.result}`)
  check('through the hook', boom.calls.length === 1 && String(boom.calls[0][0]).includes('kaboom'), `${boom.calls.length} calls`)
  throwsError('then a getter throws', () => p.value, 'recursive use of an object detected which would lead to unsafe aliasing in rust')
  throwsError('then a method throws', () => p.add(1n), 'recursive use of an object detected which would lead to unsafe aliasing in rust')
  throwsError('then free throws', () => p.free(), 'attempted to take ownership of Rust value while it was borrowed')
  eq('and leaks the value', m.drops(), dropsBefore)
  throwsError('then the handle is gone', () => p.value, 'null pointer passed to rust')
  eq('another object is untouched', c.value, 7n)
  c.free()
  end()

}

// Ordinary calls of every kind still work.
function alive(m) {
  eqCall('then a string still crosses', () => m.echo_string('still alive'), 'still alive')
  eqCall('then every shape still crosses', () => m.mixed(1, 2n, 'c', false, 0.5), '1|Some(2)|c|Some(false)|0.5')
  eqCall('then Ok still returns', () => m.checked(21, ''), 42)
  eqCall('then a class still works', () => {
    const c = new m.Counter(2n)
    const v = c.add(1n)
    c.free()
    return v
  }, 3n)
}

// Misuse (a null handle) throws an Error the same way, from the return
// slot: 100k of them leak nothing either. STRICTER than wasm-bindgen, whose
// misuse throws go through an import and leak (a wasm-bindgen 0.2.108 twin
// of the toy is dead after these 100k), so this runs last on each instance.
function misuse(label, m) {
  begin(`${label} misuse throws`)
  const N = 100_000
  let misused = 0
  let last = null
  for (let i = 0; i < N; i++) {
    try {
      new m.Snapshot().value
    } catch (e) {
      if (Object.getPrototypeOf(e) === Error.prototype && e.message === 'null pointer passed to rust') misused++
      else last = e
    }
  }
  const lastly = last === null ? '' : `, then ${last?.constructor?.name}: ${last?.message ?? show(last)}`
  check('every misuse thrown as an Error', misused === N, `${misused} of ${N}${lastly}`)
  alive(m)
  end()
}

// FinalizationRegistry frees what JS drops. Collection is the engine's
// call, so this waits for it: a one-minute hang detector, not a budget.
async function finalizers(label, m) {
  begin(`${label} finalizers`)
  if (typeof globalThis.gc !== 'function') {
    check('gc', false, 'run node with --expose-gc')
    end()
    return
  }
  const before = m.drops()
  ;(() => {
    for (let i = 0; i < 16; i++) new m.Counter(1n).snapshot()
  })()
  const deadline = Date.now() + 60_000
  while (m.drops() < before + 32 && Date.now() < deadline) {
    globalThis.gc()
    await new Promise((r) => setTimeout(r, 10))
  }
  check('dropped objects are freed', m.drops() >= before + 32, `${m.drops() - before} of 32 freed`)
  end()
}

const require = createRequire(import.meta.url)
const cjs = require(join(dir, 'nodejs', `${STEM}.js`))
suite('nodejs', cjs)
await finalizers('nodejs', cjs)
misuse('nodejs', cjs)

const webUrl = pathToFileURL(join(dir, 'web', `${STEM}.js`)).href
const bytes = readFileSync(join(dir, 'web', `${STEM}_bg.wasm`))
let fresh = 0
const load = () => import(`${webUrl}?copy=${fresh++}`)

begin('web init')
{
  const m = await load()
  const warned = []
  const savedWarn = console.warn
  console.warn = (...a) => warned.push(a.join(' '))
  const out = await m.default({ module_or_path: bytes })
  check('init returns the exports', out.memory instanceof WebAssembly.Memory, 'no memory')
  check('init twice is the same', (await m.default({ module_or_path: bytes })) === out, 'a second instance')
  check('initSync after init is the same', m.initSync({ module: bytes }) === out, 'a second instance')
  check('no warning for the object form', warned.length === 0, warned.join('; '))
  console.warn = savedWarn
  end()
  suite('web', m)
  await finalizers('web', m)
  misuse('web', m)
}
begin('web init forms')
const forms = [
  ['init(bytes)', (m) => m.default(bytes), 'using deprecated parameters for the initialization function; pass a single object instead'],
  ['init({ promise })', (m) => m.default({ module_or_path: Promise.resolve(bytes) }), null],
  ['init({ Response })', (m) => m.default({ module_or_path: new Response(bytes, { headers: { 'Content-Type': 'application/wasm' } }) }), null],
  ['init(Response, wrong type)', (m) => m.default({ module_or_path: new Response(bytes, { headers: { 'Content-Type': 'text/plain' } }) }), '`WebAssembly.instantiateStreaming` failed'],
  ['initSync({ module })', (m) => m.initSync({ module: new WebAssembly.Module(bytes) }), null],
  ['initSync(bytes)', (m) => m.initSync(bytes), 'using deprecated parameters for `initSync()`; pass a single object instead'],
]
for (const [name, init, warning] of forms) {
  const m = await load()
  const warned = []
  const savedWarn = console.warn
  console.warn = (...a) => warned.push(a.map(String).join(' '))
  let out
  try {
    out = await init(m)
  } catch (e) {
    out = e
  } finally {
    console.warn = savedWarn
  }
  check(`${name} instantiates`, out?.memory instanceof WebAssembly.Memory, out instanceof Error ? out.message : typeof out)
  check(
    `${name} warns as wasm-bindgen does`,
    warning === null ? warned.length === 0 : warned.length === 1 && warned[0].includes(warning),
    JSON.stringify(warned)
  )
  check(`${name} runs`, m.echo_str('ok') === 'ok' && m.echo_u64(P64) === P64, 'calls fail')
}
end()

console.error(`wasm-glue: ${failures === 0 ? 'every check held' : `${failures} checks FAILED`}`)
// Ended by itself, never `process.exit()`, which can hang once a module ran
// (crates/aterm-verify/perf/wasm/replay.mjs, where it was measured).
process.exitCode = failures === 0 ? 0 : 1
