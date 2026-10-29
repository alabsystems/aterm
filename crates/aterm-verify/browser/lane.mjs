// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The browser lane's page half (`verify browser`, crates/aterm-verify/src/
// browser/): boot one web module in a real browser page, render the frames
// the Rust side hands in, and hand every frame's pixels back. The Rust side
// owns the content, the comparison and the verdict; this file only drives
// the module the way a site page does.
//
//   cpu.html     aterm-wasm, the CPU module, through the ES module glue
//                `verify glue` writes: construct, then per frame process_str
//                + render(), the RGBA put on the page's 2D canvas as the site
//                puts it, and rgba() handed back.
//   gpu.html     aterm-gpu-web, aterm-gpu's GL arm on WebGL2 through the
//                module's `aterm_gl` imports, through the ES module glue
//                `verify glue` writes: construct, `await init(canvas)`, then
//                per frame process_str + render(), a real WebGL2 present onto
//                the page's canvas, and that canvas read back in the same
//                task. init is synchronous (wasm-platform WP.5), awaited here
//                as a host written for its old Promise awaits it; the page
//                reports whether the GPU was ready when the call returned, and
//                whether an init that throws inside an awaited call rejects
//                its caller.
//   worker.html  the gpu page's run in a dedicated module worker
//                (worker.mjs): the page transfers its canvas
//                (transferControlToOffscreen) and the worker calls
//                init_offscreen(canvas) on it, presents and reads back as the
//                gpu page does, and draws the differential on an
//                OffscreenCanvas it makes itself.
//
// The GPU frames are the presented canvas. After the last one the page also
// reads the same frame back through render_offscreen() + rgba(), the
// harness readback, which reads the offscreen with a synchronous
// glReadPixels (wgpu's WebGL2 path answered a blank frame here: it waited on
// a buffer map, which WebGL2 cannot block on).
//
// The gpu page then draws `spec.diff` (a wider grid of attributes, colours,
// box drawing and a sixel image) on a second canvas, one line per frame: the
// frames the recorded-frame differential holds to the wgpu-WebGL module's
// recording (crates/aterm-verify/browser/wgpu-webgl/).
//
// Last, the gpu page (not the worker) runs `spec.glow`: the cursor glow
// typed under a prompt, on a dark and a light page, with bloom on and off,
// once with its beam runs laid as ramps drawn through cell.glsl's fx stream
// and once with every row the quad it stands for, through the module's
// dev-build lane seams; on the pages `spec.glow.art` names, every frame
// also draws fx art of its own, the stream its ramps then ride beside. It answers each frame's digest, the ramps it laid
// and its last frame whole, for the Rust side to hold the ramps to the
// quads byte for byte.
//
// Every presented frame's readback is timed apart from render(), one entry
// per frame, parity then differential: gl.finish() on the canvas's context
// (`finish_ms`), the copy of the WebGL2 canvas onto a 2D canvas (`copy_ms`),
// getImageData (`read_ms`) and the base64 the answer carries (`encode_ms`).
// Chromium answers finish() without waiting for the GPU, so there the wait
// for the draws render() queued falls in getImageData, where the copy's read
// of the frame is waited for.
//
// `window.atermLane(kind, spec)` answers { module, adapter, user_agent,
// frames: [{ width, height, rgba }], timings }, rgba base64 RGBA8, and the
// gpu and worker pages add readback: frame, diff: [frames], init: {
// ready_on_return, probe: { rejected, thrown, error } } and where: { scope,
// canvas, diff_canvas } (the global scope and canvas types the frames were
// drawn in), and the gpu page glow: [{ page, bloom, ramps, digests, laid:
// [[ramps, lit rows]], last: frame }]; a failure rejects naming the step
// that failed. The Rust side
// calls it over the DevTools protocol. A browser it does not drive that way
// opens `<kind>.html?post=1&next=<kind>,...` instead: the page fetches
// spec.json, runs its lane, POSTs { answer } or { error } to
// /answer/<kind>, and opens the next page. Either way, while it works it
// POSTs ./beat (beat()), which the Rust side's hang detector counts from.

const ms = (t0) => Math.round((performance.now() - t0) * 1000) / 1000

/// Whether this runs in a document (a page) rather than a worker.
const inDocument = typeof document !== 'undefined'

/// Base64 of `bytes`, in chunks `String.fromCharCode` takes.
function base64(bytes) {
  let s = ''
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000))
  }
  return btoa(s)
}

/// `v` appended to the list `timings[key]`.
function push(timings, key, v) {
  ;(timings[key] = timings[key] || []).push(v)
}

/// When this page or worker last beat.
let beaten = -Infinity

/// Tell the lane this page is still at work: a POST to ./beat, at most once a
/// second, from each step and each frame the lane waits for. The Rust side
/// calls a page hung only after a minute with no answer and no beat
/// (serve.rs `hang_at`), so a page that is only slow (SwiftShader's readbacks
/// on a busy machine) is waited for, and one that stopped is not. A beat that
/// fails is dropped: the minute still runs.
function beat() {
  const now = performance.now()
  if (now - beaten < 1000) return
  beaten = now
  fetch('./beat', { method: 'POST', body: '' }).catch(() => {})
}

/// One animation frame: the compositor runs between two of the lane's
/// frames, as it does between a page's. A worker without
/// requestAnimationFrame yields one task instead. The wait is counted apart
/// (`frame_wait_ms`), so `total_ms` less it is the page's own work.
async function waitFrame(timings) {
  beat()
  const t0 = performance.now()
  const raf = globalThis.requestAnimationFrame
  await new Promise((resolve) => (raf ? raf(() => resolve()) : setTimeout(resolve, 0)))
  timings.frame_wait_ms = Math.round(((timings.frame_wait_ms || 0) + ms(t0)) * 1000) / 1000
}

/// A 2D canvas of `width` x `height`: a document canvas on a page, an
/// OffscreenCanvas in a worker.
function canvas2d(width, height) {
  const c = inDocument ? document.createElement('canvas') : new OffscreenCanvas(width, height)
  c.width = width
  c.height = height
  return c
}

/// What `canvas` holds now. Called in the task of the present that drew it:
/// a WebGL canvas without preserveDrawingBuffer is cleared once the
/// compositor takes the frame. finish(), the copy and the read are timed
/// apart: getContext answers the context the module made on the canvas.
function readCanvas(canvas, timings) {
  let t0 = performance.now()
  canvas.getContext('webgl2').finish()
  push(timings, 'finish_ms', ms(t0))
  t0 = performance.now()
  const copy = canvas2d(canvas.width, canvas.height)
  const ctx = copy.getContext('2d', { willReadFrequently: true })
  ctx.drawImage(canvas, 0, 0)
  push(timings, 'copy_ms', ms(t0))
  t0 = performance.now()
  const rgba = ctx.getImageData(0, 0, copy.width, copy.height).data
  push(timings, 'read_ms', ms(t0))
  return rgba
}

/// The frame `canvas` holds now, read back and encoded for the answer.
function presented(canvas, timings) {
  const rgba = readCanvas(canvas, timings)
  const t0 = performance.now()
  const frame = { width: canvas.width, height: canvas.height, rgba: base64(rgba) }
  push(timings, 'encode_ms', ms(t0))
  return frame
}

/// `f()`, its error renamed for the step that failed.
async function step(name, f) {
  beat()
  try {
    return await f()
  } catch (e) {
    throw new Error(`${name}: ${e && e.message ? e.message : String(e)}`)
  }
}

/// The page's font, fetched beside it.
async function fetchFont(timings) {
  const t0 = performance.now()
  const font = await step('fetch the font', async () =>
    new Uint8Array(await (await fetch('./font.ttf')).arrayBuffer()))
  timings.font_fetch_ms = ms(t0)
  return font
}

async function cpu(spec, font, timings) {
  let t0 = performance.now()
  const mod = await step('import the CPU glue', () => import('./cpu/aterm_wasm.js'))
  await step('instantiate the CPU module', () => mod.default())
  timings.module_init_ms = ms(t0)
  t0 = performance.now()
  const term = await step('construct AtermTerminal', () =>
    new mod.AtermTerminal(spec.rows, spec.cols, font, spec.px, ...spec.theme))
  timings.construct_ms = ms(t0)
  const canvas = document.getElementById('screen')
  const ctx = canvas.getContext('2d')
  const frames = []
  timings.frame_ms = []
  for (const chunk of spec.frames) {
    t0 = performance.now()
    term.process_str(chunk)
    term.render()
    const rgba = term.rgba()
    const [width, height] = [term.width, term.height]
    timings.frame_ms.push(ms(t0))
    canvas.width = width
    canvas.height = height
    ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0)
    frames.push({ width, height, rgba: base64(rgba) })
    await waitFrame(timings)
  }
  term.free()
  return { module: 'aterm-wasm', adapter: 'cpu', frames, timings }
}

/// Where the gpu page draws: the document's canvases through `init`.
const documentCanvases = {
  init: 'init',
  screen: () => document.getElementById('screen'),
  second: () => document.body.appendChild(document.createElement('canvas')),
  scratch: () => document.createElement('canvas'),
  bind: (term, canvas) => term.init(canvas),
}

/// Where a worker draws: the canvas the page transferred to it, and an
/// OffscreenCanvas of its own for the differential, through init_offscreen.
export function workerCanvases(screen) {
  return {
    init: 'init_offscreen',
    screen: () => screen,
    second: () => new OffscreenCanvas(1, 1),
    scratch: () => new OffscreenCanvas(1, 1),
    bind: (term, canvas) => term.init_offscreen(canvas),
  }
}

/// The GPU module's run on `on` (documentCanvases, or workerCanvases in a
/// worker): the parity frames, the harness readback and the differential.
export async function gpu(spec, font, timings, on = documentCanvases) {
  let t0 = performance.now()
  const mod = await step('import the GPU glue', () => import('./gpu/aterm_gpu_web.js'))
  await step('instantiate the GPU module', () => mod.default())
  timings.module_init_ms = ms(t0)
  t0 = performance.now()
  const term = await step('construct AtermGpuTerminal', () =>
    new mod.AtermGpuTerminal(spec.rows, spec.cols, font, spec.px, ...spec.theme))
  timings.construct_ms = ms(t0)
  const canvas = on.screen()
  // A canvas already holding a 2D context has no WebGL2 to give, so init
  // throws on it. A host awaits init inside an async function: that throw
  // must reject the function, not escape it.
  t0 = performance.now()
  const taken = on.scratch()
  taken.getContext('2d')
  const host = async () => {
    await on.bind(term, taken)
  }
  const probe = { rejected: false, thrown: '', error: '' }
  await host().catch((e) => {
    Object.assign(probe, { rejected: true, thrown: typeof e, error: String(e) })
  })
  timings.probe_ms = ms(t0)
  let readyOnReturn = false
  t0 = performance.now()
  await step(`${on.init}(canvas)`, () => {
    const returned = on.bind(term, canvas)
    // Read before anything awaits: true only if init finished in the call.
    readyOnReturn = term.gpu_ready
    return returned
  })
  timings.gpu_init_ms = ms(t0)
  if (!term.gpu_ready) throw new Error(`${on.init}(canvas) resolved but gpu_ready is false`)
  const adapter = term.adapter_info
  const frames = []
  timings.frame_ms = []
  for (const chunk of spec.frames) {
    t0 = performance.now()
    term.process_str(chunk)
    try {
      term.render()
    } catch (e) {
      throw new Error(`render(): ${e && e.message ? e.message : String(e)}`)
    }
    timings.frame_ms.push(ms(t0))
    // Same task as the present: nothing between render() and the copy yields.
    frames.push(presented(canvas, timings))
    await waitFrame(timings)
  }
  t0 = performance.now()
  await step('render_offscreen()', () => term.render_offscreen())
  const readback = { width: term.width, height: term.height, rgba: base64(term.rgba()) }
  timings.readback_ms = ms(t0)
  t0 = performance.now()
  term.free()
  timings.free_ms = ms(t0)
  const second = on.second()
  const diff = await gpuDiff(mod, spec, font, timings, on, second)
  const init = { ready_on_return: readyOnReturn, probe }
  const where = {
    scope: globalThis.constructor.name,
    canvas: canvas.constructor.name,
    diff_canvas: second.constructor.name,
  }
  const answer = { module: 'aterm-gpu-web', adapter, frames, readback, diff, timings, init, where }
  // The glow scene runs on the page alone: the worker's run is held to the
  // page's frames, and the glow's to its own oracle.
  if (on === documentCanvases && spec.glow) {
    answer.glow = await gpuGlow(mod, spec, font, timings)
  }
  return answer
}

/// The recorded-frame differential's terminal: a wider grid on a canvas of
/// its own, one line of attributes, colours, box drawing or an image per
/// frame, each presented and read back as the parity frames are. These are
/// the frames held to the wgpu-WebGL module's recording.
async function gpuDiff(mod, spec, font, timings, on, canvas) {
  let t0 = performance.now()
  const term = await step('construct the differential AtermGpuTerminal', () =>
    new mod.AtermGpuTerminal(spec.diff.rows, spec.diff.cols, font, spec.px, ...spec.theme))
  await step(`${on.init}(differential canvas)`, () => on.bind(term, canvas))
  timings.diff_setup_ms = ms(t0)
  const frames = []
  timings.diff_frame_ms = []
  for (const chunk of spec.diff.frames) {
    t0 = performance.now()
    term.process_str(chunk)
    try {
      term.render()
    } catch (e) {
      throw new Error(`differential render(): ${e && e.message ? e.message : String(e)}`)
    }
    timings.diff_frame_ms.push(ms(t0))
    frames.push(presented(canvas, timings))
    await waitFrame(timings)
  }
  t0 = performance.now()
  term.free()
  timings.free_ms = Math.round((timings.free_ms + ms(t0)) * 1000) / 1000
  return frames
}

/// Two independent 32-bit FNV-1a digests of `bytes`, as one hex string: a
/// glow frame's identity, so the answer carries every frame's without its
/// pixels.
function digest(bytes) {
  let a = 0x811c9dc5
  let b = 0x01000193
  for (let i = 0; i < bytes.length; i++) {
    a = Math.imul(a ^ bytes[i], 0x01000193)
    b = Math.imul(b ^ bytes[i], 0x1000193b) + i
  }
  const hex = (v) => (v >>> 0).toString(16).padStart(8, '0')
  return hex(a) + hex(b)
}

/// The glow scene's runs (`spec.glow`): in each glow style on each page
/// theme, with bloom (and its SDR crown) on and off, a terminal laying its
/// cursor glow's beam runs as ramps drawn through cell.glsl's fx stream (the
/// bloom and the crown reading the additive stream's as an item list) and one
/// laying every row as the quad it stands for (the oracle), each typing the
/// same keys at the same injected clock. Frame 0 is the prompt before any key
/// (no glow); each later frame types one key, or none in the run-out. Every
/// frame's digest comes back, with the glow ramps and lit rows it laid and
/// the additive stream's ramps, and the last frame whole.
///
/// The module's first glow terminal lays its first key's under-ink bed
/// apart from every later one's whatever its route (on Firefox, 516 quads
/// against the 518 every later run lays, a 400 ms pause before its first
/// frame changing neither), so a discarded run first puts every compared
/// run in the same state.
async function gpuGlow(mod, spec, font, timings) {
  const g = spec.glow
  const runs = []
  const t0 = performance.now()
  await glowRun(mod, spec, font, timings, g.styles[0], 'warm-up', g.themes.dark, true, false)
  for (const style of g.styles) {
    for (const [page, theme] of Object.entries(g.themes)) {
      for (const bloom of [true, false]) {
        for (const ramps of [true, false]) {
          runs.push(await glowRun(mod, spec, font, timings, style, page, theme, bloom, ramps))
        }
      }
    }
  }
  timings.glow_ms = ms(t0)
  return runs
}

/// One glow run ([`gpuGlow`]) on a canvas of its own, freed after.
async function glowRun(mod, spec, font, timings, style, page, theme, bloom, ramps) {
  const g = spec.glow
  const canvas = document.body.appendChild(document.createElement('canvas'))
  const term = await step(`construct the glow AtermGpuTerminal (${style}, ${page})`, () =>
    new mod.AtermGpuTerminal(g.rows, g.cols, font, spec.px, ...theme))
  await step(`init(glow canvas, ${style}, ${page})`, () => term.init(canvas))
  await step('set_glow_ramps_for_lane', () => term.set_glow_ramps_for_lane(ramps))
  await step('set_bloom_for_lane', () => term.set_bloom_for_lane(bloom))
  await step('set_fx_art_for_lane', () => term.set_fx_art_for_lane(g.art.includes(page)))
  term.set_cursor_glow(true, style, undefined, undefined, g.duration_ms, g.length, g.intensity, g.radius, true)
  const digests = []
  const laid = []
  let last = null
  for (let k = 0; k < g.frames; k++) {
    if (k === 0) {
      term.process_str(g.prompt)
    } else if (k <= g.keys.length) {
      const ch = g.keys[k - 1]
      term.note_typed_char(ch)
      term.process_str(ch)
    }
    if (k > 0) term.advance_effects(g.dt_ms)
    try {
      term.render()
    } catch (e) {
      throw new Error(`glow render() (${style}, ${page}, frame ${k}): ${e && e.message ? e.message : String(e)}`)
    }
    const rgba = readCanvas(canvas, timings)
    digests.push(digest(rgba))
    laid.push(Array.from(term.glow_ramps_for_lane()))
    if (k === g.frames - 1) {
      last = { width: canvas.width, height: canvas.height, rgba: base64(rgba) }
    }
    await waitFrame(timings)
  }
  term.free()
  const lose = canvas.getContext('webgl2').getExtension('WEBGL_lose_context')
  if (lose) lose.loseContext()
  canvas.remove()
  return { style, page, bloom, ramps, digests, laid, last }
}

/// The gpu page's run in a dedicated module worker (worker.mjs): the page's
/// canvas transferred to it. The worker's own timings come back in its
/// answer; the page adds what starting it and the round trip cost.
async function worker(spec, font, timings) {
  let t0 = performance.now()
  const offscreen = await step('transferControlToOffscreen()', () =>
    document.getElementById('screen').transferControlToOffscreen())
  const w = new Worker('./worker.mjs', { type: 'module' })
  const answer = await new Promise((resolve, reject) => {
    w.onmessage = (e) =>
      e.data && e.data.answer ? resolve(e.data.answer) : reject(new Error(`the worker: ${e.data && e.data.error}`))
    w.onerror = (e) => reject(new Error(`the worker: ${e.message || 'failed to load'}`))
    w.postMessage({ spec, font, canvas: offscreen }, [offscreen])
  })
  w.terminate()
  timings.worker_round_trip_ms = ms(t0)
  answer.timings = { ...answer.timings, page: timings }
  return answer
}

const lanes = { cpu, gpu, worker }

/// Run the lane `kind` with `spec`: the answer, with its timings' total and
/// the browser it ran in.
async function atermLane(kind, spec) {
  const timings = {}
  const t0 = performance.now()
  const run = lanes[kind]
  if (!run) throw new Error(`no lane ${kind}`)
  const font = await fetchFont(timings)
  const answer = await run(spec, font, timings)
  answer.timings.total_ms = ms(t0)
  answer.user_agent = navigator.userAgent
  return answer
}

/// The post-back run (`?post=1`): fetch the spec, run this page's lane, POST
/// { answer } or { error } to /answer/<kind>, then open the next page.
async function postBack(kind, params) {
  let body
  try {
    const spec = await step('fetch spec.json', async () => (await fetch('./spec.json')).json())
    body = { answer: await atermLane(kind, spec) }
  } catch (e) {
    body = { error: e && e.message ? e.message : String(e) }
  }
  await fetch(`./answer/${kind}`, { method: 'POST', body: JSON.stringify(body) })
  const [next, ...rest] = (params.get('next') || '').split(',').filter((k) => k)
  if (next) {
    location.replace(`./${next}.html?post=1${rest.length ? `&next=${rest.join(',')}` : ''}`)
  }
}

if (inDocument) {
  window.atermLane = atermLane
  const params = new URLSearchParams(location.search)
  if (params.has('post')) {
    const kind = location.pathname.split('/').pop().replace(/\.html$/, '')
    addEventListener('load', () => postBack(kind, params))
  }
}
