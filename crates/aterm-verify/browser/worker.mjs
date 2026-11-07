// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The browser lane's worker (worker.html starts it; lane.mjs says what the
// lane does): the gpu page's run in a dedicated module worker, on the canvas
// the page transferred (transferControlToOffscreen) through aterm-gpu-web's
// init_offscreen(canvas). It answers what the gpu page answers, or { error },
// or { unsupported } where the browser gives a worker no WebGL2 at all.

import { gpu, workerCanvases } from './lane.mjs'

const ms = (t0) => Math.round((performance.now() - t0) * 1000) / 1000

/// Whether this browser gives a worker a WebGL2 context: asked before aterm
/// draws, on an OffscreenCanvas of the worker's own, and let go at once. WPE
/// makes a worker's WebGL only in its GPU process, which needs a GBM display
/// (a DRM render node), and answers null without one.
function workerWebGL2() {
  try {
    const gl = new OffscreenCanvas(1, 1).getContext('webgl2')
    if (!gl) return false
    const lose = gl.getExtension('WEBGL_lose_context')
    if (lose) lose.loseContext()
    return true
  } catch {
    return false
  }
}

self.onmessage = async (e) => {
  const { spec, font, canvas } = e.data
  if (!workerWebGL2()) {
    self.postMessage({
      unsupported:
        'this browser gives a worker no WebGL2 context: an OffscreenCanvas of the worker\'s own answered null before aterm drew',
    })
    return
  }
  const timings = {}
  const t0 = performance.now()
  try {
    const answer = await gpu(spec, font, timings, workerCanvases(canvas))
    timings.worker_total_ms = ms(t0)
    self.postMessage({ answer })
  } catch (err) {
    self.postMessage({ error: err && err.message ? err.message : String(err) })
  }
}
