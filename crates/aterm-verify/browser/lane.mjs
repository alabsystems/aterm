// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The browser lane's page half (`verify browser`, crates/aterm-verify/src/
// browser/): boot one web module in a real Chromium page, render the frames
// the Rust side hands in, and hand every frame's pixels back over the
// DevTools protocol. The Rust side owns the content, the comparison and the
// verdict; this file only drives the module the way a site page does.
//
//   cpu.html  aterm-wasm, the CPU module, through the glue `verify glue
//             --flavor web` writes: construct, then per frame process_str +
//             render(), the RGBA put on the page's 2D canvas as the site puts
//             it, and rgba() handed back.
//   gpu.html  aterm-gpu-web, aterm-gpu's GL arm on WebGL2 through the
//             module's `aterm_gl` imports, through the glue `verify glue
//             --flavor web` writes: construct, `await init(canvas)`, then per
//             frame process_str + render(), a real WebGL2 present onto the
//             page's canvas, and that canvas read back in the same task.
//             init is synchronous (wasm-platform WP.5), awaited here as a
//             host written for its old Promise awaits it; the page reports
//             whether the GPU was ready when the call returned, and whether
//             an init that throws inside an awaited call rejects its caller.
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
// `window.atermLane(kind, spec)` answers { module, adapter, frames: [{ width,
// height, rgba }], timings }, rgba base64 RGBA8, and the gpu page adds
// readback: frame, diff: [frames] and init: { ready_on_return, probe: {
// rejected, thrown, error } }; a failure rejects naming the step that failed.

const ms = (t0) => Math.round((performance.now() - t0) * 1000) / 1000

/// Base64 of `bytes`, in chunks `String.fromCharCode` takes.
function base64(bytes) {
  let s = ''
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000))
  }
  return btoa(s)
}

/// One animation frame: the compositor runs between two of the lane's
/// frames, as it does between a page's. The wait is counted apart
/// (`frame_wait_ms`), so `total_ms` less it is the page's own work.
async function waitFrame(timings) {
  const t0 = performance.now()
  await new Promise((resolve) => requestAnimationFrame(() => resolve()))
  timings.frame_wait_ms = Math.round(((timings.frame_wait_ms || 0) + ms(t0)) * 1000) / 1000
}

/// What `canvas` holds now. Called in the task of the present that drew it:
/// a WebGL canvas without preserveDrawingBuffer is cleared once the
/// compositor takes the frame.
function readCanvas(canvas) {
  const copy = document.createElement('canvas')
  copy.width = canvas.width
  copy.height = canvas.height
  const ctx = copy.getContext('2d', { willReadFrequently: true })
  ctx.drawImage(canvas, 0, 0)
  return ctx.getImageData(0, 0, copy.width, copy.height).data
}

/// `f()`, its error renamed for the step that failed.
async function step(name, f) {
  try {
    return await f()
  } catch (e) {
    throw new Error(`${name}: ${e && e.message ? e.message : String(e)}`)
  }
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

async function gpu(spec, font, timings) {
  let t0 = performance.now()
  const mod = await step('import the GPU glue', () => import('./gpu/aterm_gpu_web.js'))
  await step('instantiate the GPU module', () => mod.default())
  timings.module_init_ms = ms(t0)
  t0 = performance.now()
  const term = await step('construct AtermGpuTerminal', () =>
    new mod.AtermGpuTerminal(spec.rows, spec.cols, font, spec.px, ...spec.theme))
  timings.construct_ms = ms(t0)
  const canvas = document.getElementById('screen')
  // A canvas already holding a 2D context has no WebGL2 to give, so init
  // throws on it. A host awaits init inside an async function: that throw
  // must reject the function, not escape it.
  const taken = document.createElement('canvas')
  taken.getContext('2d')
  const host = async () => {
    await term.init(taken)
  }
  const probe = { rejected: false, thrown: '', error: '' }
  await host().catch((e) => {
    Object.assign(probe, { rejected: true, thrown: typeof e, error: String(e) })
  })
  let readyOnReturn = false
  t0 = performance.now()
  await step('init(canvas)', () => {
    const returned = term.init(canvas)
    // Read before anything awaits: true only if init finished in the call.
    readyOnReturn = term.gpu_ready
    return returned
  })
  timings.gpu_init_ms = ms(t0)
  if (!term.gpu_ready) throw new Error('init(canvas) resolved but gpu_ready is false')
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
    const rgba = readCanvas(canvas)
    frames.push({ width: canvas.width, height: canvas.height, rgba: base64(rgba) })
    await waitFrame(timings)
  }
  t0 = performance.now()
  await step('render_offscreen()', () => term.render_offscreen())
  const readback = { width: term.width, height: term.height, rgba: base64(term.rgba()) }
  timings.readback_ms = ms(t0)
  term.free()
  const diff = await gpuDiff(mod, spec, font, timings)
  const init = { ready_on_return: readyOnReturn, probe }
  return { module: 'aterm-gpu-web', adapter, frames, readback, diff, timings, init }
}

/// The recorded-frame differential's terminal: a wider grid on a canvas of
/// its own, one line of attributes, colours, box drawing or an image per
/// frame, each presented and read back as the parity frames are. These are
/// the frames held to the wgpu-WebGL module's recording.
async function gpuDiff(mod, spec, font, timings) {
  const canvas = document.createElement('canvas')
  document.body.appendChild(canvas)
  const term = await step('construct the differential AtermGpuTerminal', () =>
    new mod.AtermGpuTerminal(spec.diff.rows, spec.diff.cols, font, spec.px, ...spec.theme))
  await step('init(differential canvas)', () => term.init(canvas))
  const frames = []
  timings.diff_frame_ms = []
  for (const chunk of spec.diff.frames) {
    const t0 = performance.now()
    term.process_str(chunk)
    try {
      term.render()
    } catch (e) {
      throw new Error(`differential render(): ${e && e.message ? e.message : String(e)}`)
    }
    timings.diff_frame_ms.push(ms(t0))
    const rgba = readCanvas(canvas)
    frames.push({ width: canvas.width, height: canvas.height, rgba: base64(rgba) })
    await waitFrame(timings)
  }
  term.free()
  return frames
}

window.atermLane = async (kind, spec) => {
  const timings = {}
  const t0 = performance.now()
  const font = await step('fetch the font', async () =>
    new Uint8Array(await (await fetch('./font.ttf')).arrayBuffer()))
  timings.font_fetch_ms = ms(t0)
  const answer = kind === 'gpu' ? await gpu(spec, font, timings) : await cpu(spec, font, timings)
  answer.timings.total_ms = ms(t0)
  return answer
}
