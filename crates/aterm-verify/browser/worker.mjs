// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The browser lane's worker (worker.html starts it; lane.mjs says what the
// lane does): the gpu page's run in a dedicated module worker, on the canvas
// the page transferred (transferControlToOffscreen) through aterm-gpu-web's
// init_offscreen(canvas). It answers what the gpu page answers, or { error }.

import { gpu, workerCanvases } from './lane.mjs'

const ms = (t0) => Math.round((performance.now() - t0) * 1000) / 1000

self.onmessage = async (e) => {
  const { spec, font, canvas } = e.data
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
