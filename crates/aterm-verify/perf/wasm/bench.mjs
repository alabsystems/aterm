// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// WASM-BENCH scenarios (E0): drive the REAL exports of the two shipped
// renderer modules (the CPU module's glue from `verify glue`, the GPU
// module's from wasm-bindgen) under node and report medians as one flat JSON
// line on stdout (the harness convention the perf lanes parse). Human
// notes go to stderr. Deterministic corpora, median-of-N + warmup — the same
// non-flake discipline as the native harnesses.
//
// What each scenario fences (all BIGGER-IS-BETTER):
//   wasm_cpu_ingest_mixed_mbps    Terminal::process over the mixed-VT workload
//                                 (the perf_harness corpus shape) — wasm ingest.
//   wasm_cpu_ingest_replog_mbps   plain repetitive-log flood (cat-flood shape).
//   wasm_cpu_ingest_str_ascii_mbps
//                                 Terminal::process_str over the same log flood
//                                 as a JS string: the call a web host's PTY pump
//                                 makes, so the string boundary (encode into wasm
//                                 memory, take it as a str) is timed with the
//                                 engine. MB are UTF-8 bytes.
//   wasm_cpu_ingest_str_nonascii_mbps
//                                 process_str over a log flood whose lines carry
//                                 CJK, accented Latin, box drawing and emoji: the
//                                 non-ASCII half of that boundary.
//   wasm_cpu_scroll_present_fps   scroll_lines(3) + render() per frame over deep
//                                 history — today's FullRepaint + full RGBA
//                                 expansion path (the E3/E5/E7 target).
//   wasm_cpu_typing_present_fps   1 char + render() per frame — the dirty-row
//                                 damage path (steady-state present).
//   wasm_cpu_search_build_klps    first search() after a content change — the
//                                 all-or-nothing index rebuild in wasm.
//   wasm_cpu_search_query_qps     repeat search() on the cached index.
//   wasm_cpu_search_lines_per_mib indexed lines per MiB of wasm-heap GROWTH
//                                 across the first build (RSS proxy: the linear
//                                 memory never shrinks, so growth ≈ peak added).
//   wasm_cpu_restore_hz           serialize() -> fresh engine -> full replay.
//   wasm_cpu_uniform_flood_present_fps
//                                 bottom-pinned flood of byte-identical lines +
//                                 render() per line: the anchor moves, the offsets
//                                 stay equal, every frame gate-hits — the E3
//                                 zero-band present (pinned functionally by
//                                 aterm-render tests/anchor_reuse.rs
//                                 uniform_bottom_flood_gate_hits_on_equal_offsets).
//   wasm_cpu_scrolled_stream_present_fps
//                                 output streams while the viewport is scrolled
//                                 into history: each line re-pins the viewport
//                                 (display offset grows, content identical) —
//                                 the E5 anchor-equal repin shape.
//   wasm_cpu_styled_scrub_present_fps
//                                 scroll_lines(3) + render() over a heavily
//                                 SGR'd history (a colour/attribute change every
//                                 4 cells) — the E6a attr run-cursor path.
//   wasm_gpu_ingest_mixed_mbps    the GPU module's engine ingest.
//   wasm_gpu_frame_build_fps      render() WITHOUT WebGL init: pump + cell_frame
//                                 + effects + stamp + spill run, then the GPU
//                                 access errors — i.e. the GPU path's per-frame
//                                 wasm-side cost. GL submission needs a browser
//                                 (on-glass half: aterm-ctl metrics / arena).
//   wasm_gpu_scroll_frame_build_fps  same, while scrolled through history.

import { readFileSync, statSync } from 'node:fs'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)
const outDir = process.argv[2]
if (!outDir) {
  console.error('usage: node bench.mjs <dir with the cpu/ and gpu/ CommonJS glue>')
  process.exit(2)
}

const N = 5
const WARMUP = 1
const ROWS = 24
const COLS = 80
const PX = 16.0
const THEME = [0x00e0e0e0, 0x00101010, 0x00e0e0e0, 0x00404060]

// A monospace face for the ctor (the host injects font bytes on the web; here
// the host is this harness). First readable candidate wins.
const FONT_CANDIDATES = [
  '/System/Library/Fonts/Monaco.ttf',
  '/System/Library/Fonts/Supplemental/Courier New.ttf',
  '/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf',
  '/usr/share/fonts/TTF/DejaVuSansMono.ttf'
]
function loadFont() {
  for (const p of FONT_CANDIDATES) {
    try {
      return { path: p, bytes: readFileSync(p) }
    } catch {
      /* next candidate */
    }
  }
  console.error('wasm-bench: no monospace font found among candidates', FONT_CANDIDATES)
  process.exit(1)
}

// --- deterministic corpora (byte-identical every run: no RNG, no clock) -----
function mixedWorkload(bytes) {
  // The perf_harness frame: plain text + SGR + CSI + clears, scrolling constantly.
  const frame = [
    'the quick brown fox jumps over the lazy dog 0123456789\r\n',
    '\x1b[1;38;5;202mERROR\x1b[0m \x1b[4;48;5;19mwarning\x1b[0m normal text here\r\n',
    '\x1b[2K\x1b[1Gredrawn line after erase-in-line and cursor-to-col-1\r\n',
    '\x1b[7minverse\x1b[27m \x1b[3mitalic\x1b[23m \x1b[9mstrike\x1b[29m mixed sgr\r\n',
    '\x1b[10;5Hpositioned via CUP \x1b[K then erased to end of line\r\n',
    'plain throughput line with a tab\tand more ascii content padding 42\r\n',
    '\x1b[38;2;120;200;255mtruecolor\x1b[0m run plus \x1b[48;2;10;10;10mbg\x1b[0m\r\n',
    '\x1b[H\x1b[2Jfull clear then home, forcing a screen reset every frame\r\n'
  ].join('')
  const unit = new TextEncoder().encode(frame)
  const out = new Uint8Array(bytes + unit.length)
  for (let off = 0; off < bytes; off += unit.length) out.set(unit, off)
  return out.subarray(0, Math.floor(bytes / unit.length) * unit.length)
}

function replogLine(i) {
  const templates = [
    'INFO  svc-api    request completed method=GET path=/api/v1/items status=200',
    'INFO  svc-api    request completed method=POST path=/api/v1/items status=201',
    'DEBUG svc-cache  entry refreshed key=items shard=4 hit_rate=0.97',
    'INFO  svc-worker job dequeued queue=default attempts=1',
    'WARN  svc-api    slow request method=GET path=/api/v1/search status=200',
    'ERROR svc-db     connection reset pool=primary retrying',
    'INFO  svc-worker job finished queue=default result=ok',
    'DEBUG svc-gc     sweep complete freed_kb=128 live_objects=40213'
  ]
  const mm = String(Math.floor(i / 60) % 60).padStart(2, '0')
  const ss = String(i % 60).padStart(2, '0')
  const ms = String(i % 1000).padStart(3, '0')
  return `2026-07-22T12:${mm}:${ss}.${ms}Z ${templates[i % templates.length]} latency_ms=${i % 250}\r\n`
}
function replogWorkload(bytes) {
  const parts = []
  let total = 0
  for (let i = 0; total < bytes; i++) {
    const line = replogLine(i)
    parts.push(line)
    total += line.length
  }
  return new TextEncoder().encode(parts.join(''))
}

/** The log flood as a JS string of at least `bytes` UTF-8 bytes; `nonAscii`
 * gives every line a non-ASCII field (2-, 3- and 4-byte UTF-8). */
function logText(bytes, nonAscii) {
  const fields = [
    'ビルド完了 経過時間=1.2s',
    'façade naïve résumé Ünïcödé',
    '├── src/main.rs ✓ 🚀',
    '请求完成 延迟=42ms ⚠',
    'Ελληνικά кириллица ✔ 😀'
  ]
  const parts = []
  let total = 0
  for (let i = 0; total < bytes; i++) {
    let line = replogLine(i)
    if (nonAscii) line = `${line.slice(0, -2)} ${fields[i % fields.length]}\r\n`
    parts.push(line)
    total += utf8Length(line)
  }
  return parts.join('')
}
function utf8Length(text) {
  return new TextEncoder().encode(text).length
}

const GLYPHS = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/-='
function rotatingLines(n) {
  const parts = []
  for (let line = 0; line < n; line++) {
    let body = ''
    for (let c = 0; c < 40; c++) body += GLYPHS[(line + c) % GLYPHS.length]
    parts.push(`${line} ${body}\r\n`)
  }
  return parts.join('')
}

// --- measurement discipline -------------------------------------------------
function median(samples) {
  const v = [...samples].sort((a, b) => a - b)
  const n = v.length
  return n % 2 === 1 ? v[(n - 1) / 2] : (v[n / 2 - 1] + v[n / 2]) / 2
}
/** Median work-rate of `iter` (a fn returning {work, secs}) over N runs. */
function rate(iter) {
  for (let i = 0; i < WARMUP; i++) iter()
  const samples = []
  for (let i = 0; i < N; i++) {
    const { work, secs } = iter()
    samples.push(secs > 0 ? work / secs : Infinity)
  }
  return median(samples)
}
function timeIt(f) {
  const t0 = process.hrtime.bigint()
  f()
  return Number(process.hrtime.bigint() - t0) / 1e9
}

const font = loadFont()
console.error(`wasm-bench: node ${process.version}, font ${font.path}`)

const cpu = require(`${outDir}/cpu/aterm_wasm.js`)
const gpu = require(`${outDir}/gpu/aterm_gpu_web.js`)
const cpuWasmBytes = statSync(`${outDir}/cpu/aterm_wasm_bg.wasm`).size
const gpuWasmBytes = statSync(`${outDir}/gpu/aterm_gpu_web_bg.wasm`).size

function newCpuTerm(rows = ROWS, cols = COLS) {
  return new cpu.AtermTerminal(rows, cols, font.bytes, PX, ...THEME)
}
function newGpuTerm(rows = ROWS, cols = COLS) {
  return new gpu.AtermGpuTerminal(rows, cols, font.bytes, PX, ...THEME)
}
/** Process RSS after a GC pass (the wasm lane passes --expose-gc). The nodejs-target
 * glue keeps the wasm instance private, so linear-memory growth is observed as
 * an RSS delta — a PROXY (JS garbage pollutes it), reported but never gated;
 * the native search harness owns the precise counting-allocator floor. */
function rssBytes() {
  globalThis.gc?.()
  return process.memoryUsage().rss
}

const report = {}

// --- CPU ingest -------------------------------------------------------------
const MIX = mixedWorkload(8 << 20)
const REPLOG = replogWorkload(8 << 20)
report.wasm_cpu_ingest_mixed_mbps = rate(() => {
  const t = newCpuTerm()
  const secs = timeIt(() => t.process(MIX))
  t.free()
  return { work: MIX.length / 1e6, secs }
})
report.wasm_cpu_ingest_replog_mbps = rate(() => {
  const t = newCpuTerm()
  const secs = timeIt(() => t.process(REPLOG))
  t.free()
  return { work: REPLOG.length / 1e6, secs }
})
const LOG_ASCII = logText(8 << 20, false)
const LOG_NONASCII = logText(8 << 20, true)
for (const [key, text] of [
  ['wasm_cpu_ingest_str_ascii_mbps', LOG_ASCII],
  ['wasm_cpu_ingest_str_nonascii_mbps', LOG_NONASCII]
]) {
  const mb = utf8Length(text) / 1e6
  report[key] = rate(() => {
    const t = newCpuTerm()
    const secs = timeIt(() => t.process_str(text))
    t.free()
    return { work: mb, secs }
  })
}
console.error(
  `wasm-bench: cpu ingest mixed ${report.wasm_cpu_ingest_mixed_mbps.toFixed(1)} MB/s | ` +
    `replog ${report.wasm_cpu_ingest_replog_mbps.toFixed(1)} MB/s | ` +
    `str ascii ${report.wasm_cpu_ingest_str_ascii_mbps.toFixed(1)} MB/s | ` +
    `str non-ascii ${report.wasm_cpu_ingest_str_nonascii_mbps.toFixed(1)} MB/s`
)

// --- CPU scroll+present and typing+present ----------------------------------
{
  const t = newCpuTerm()
  t.set_scrollback_limit(15000)
  t.process(new TextEncoder().encode(rotatingLines(12000)))
  t.render() // settle the first full paint outside the timers
  const SCROLL_FRAMES = 200
  report.wasm_cpu_scroll_present_fps = rate(() => {
    t.scroll_to_bottom()
    const secs = timeIt(() => {
      for (let i = 0; i < SCROLL_FRAMES; i++) {
        t.scroll_lines(3)
        t.render()
      }
    })
    return { work: SCROLL_FRAMES, secs }
  })
  const TYPE_FRAMES = 200
  t.scroll_to_bottom()
  report.wasm_cpu_typing_present_fps = rate(() => {
    const secs = timeIt(() => {
      for (let i = 0; i < TYPE_FRAMES; i++) {
        t.process_str('x')
        t.render()
      }
    })
    return { work: TYPE_FRAMES, secs }
  })
  report.wasm_cpu_present_px = t.width * t.height
  t.free()
  console.error(
    `wasm-bench: cpu scroll+present ${report.wasm_cpu_scroll_present_fps.toFixed(1)} fps | ` +
      `typing+present ${report.wasm_cpu_typing_present_fps.toFixed(1)} fps ` +
      `(${report.wasm_cpu_present_px} px frame)`
  )
}

// --- CPU search build/query/heap --------------------------------------------
{
  const SEARCH_LINES = 20000
  const t = newCpuTerm()
  t.set_scrollback_limit(25000)
  t.process(new TextEncoder().encode(rotatingLines(SEARCH_LINES)))
  const before = rssBytes()
  const firstMatches = t.search('jkl', true, false)
  const growth = rssBytes() - before
  report.wasm_cpu_search_matches = firstMatches.length / 3
  if (growth > 0) {
    report.wasm_cpu_search_rss_bytes_per_line = growth / SEARCH_LINES
  }
  report.wasm_cpu_search_build_klps = rate(() => {
    t.process_str('x') // content change -> next search pays the full rebuild
    const secs = timeIt(() => t.search('jkl', true, false))
    return { work: SEARCH_LINES / 1000, secs }
  })
  const QUERY_BATCH = 20
  report.wasm_cpu_search_query_qps = rate(() => {
    const secs = timeIt(() => {
      for (let i = 0; i < QUERY_BATCH; i++) t.search('jkl', true, false)
    })
    return { work: QUERY_BATCH, secs }
  })
  t.free()
  console.error(
    `wasm-bench: cpu search build ${report.wasm_cpu_search_build_klps.toFixed(1)} klines/s | ` +
      `query ${report.wasm_cpu_search_query_qps.toFixed(0)} q/s | rss-proxy ` +
      (report.wasm_cpu_search_rss_bytes_per_line != null
        ? `${report.wasm_cpu_search_rss_bytes_per_line.toFixed(0)} B/line`
        : 'unavailable')
  )
}

// --- CPU restore (serialize -> fresh engine -> replay) ----------------------
{
  const source = newCpuTerm()
  source.set_scrollback_limit(10000)
  source.process(new TextEncoder().encode(rotatingLines(5000)))
  const payload = source.serialize(undefined)
  source.free()
  report.wasm_cpu_restore_payload_bytes = payload.length
  report.wasm_cpu_restore_hz = rate(() => {
    const t = newCpuTerm()
    t.set_scrollback_limit(10000)
    const secs = timeIt(() => t.process_str(payload))
    t.free()
    return { work: 1, secs }
  })
  console.error(
    `wasm-bench: cpu restore ${report.wasm_cpu_restore_hz.toFixed(1)} restores/s ` +
      `(${(payload.length / 1024).toFixed(0)} KiB payload)`
  )
}

// --- CPU present shapes: uniform flood, scrolled stream, styled scrub ------
// Each lane asserts its SHAPE before it reports a number: a lane whose frames
// stopped gate-hitting (or stopped re-pinning) would otherwise time a different
// path under the same name.
function styledLines(n) {
  const parts = []
  for (let line = 0; line < n; line++) {
    let body = ''
    for (let k = 0; k < 20; k++) {
      const attr = [1, 3, 4, 7][(line + k) % 4]
      body += `\x1b[${attr};38;5;${(line * 7 + k * 13) % 256}m`
      body += GLYPHS.slice((line + k) % 60, ((line + k) % 60) + 4)
    }
    parts.push(`${body}\x1b[0m\r\n`)
  }
  return parts.join('')
}
{
  const FRAMES = 200
  const LINE = new TextEncoder().encode('flood line: the same bytes every time 0123456789\r\n')
  const t = newCpuTerm()
  t.set_scrollback_limit(15000)
  for (let i = 0; i < 2 * ROWS; i++) t.process(LINE)
  t.render()
  let bands = 0
  report.wasm_cpu_uniform_flood_present_fps = rate(() => {
    bands = 0
    const secs = timeIt(() => {
      for (let i = 0; i < FRAMES; i++) {
        t.process(LINE)
        t.render()
        bands += t.present_band_count()
      }
    })
    return { work: FRAMES, secs }
  })
  if (bands !== 0) {
    console.error(`wasm-bench: uniform flood presented ${bands} band(s) in its last run; the shape is zero-band`)
    process.exit(1)
  }
  t.free()
}
{
  const FRAMES = 200
  const t = newCpuTerm()
  t.set_scrollback_limit(15000)
  t.process(new TextEncoder().encode(rotatingLines(12000)))
  t.scroll_lines(200)
  t.render()
  let repinned = true
  let line = 0
  report.wasm_cpu_scrolled_stream_present_fps = rate(() => {
    const secs = timeIt(() => {
      for (let i = 0; i < FRAMES; i++) {
        const before = t.display_offset
        t.process_str(`streamed ${line++} while the reader is scrolled back\r\n`)
        t.render()
        repinned &&= t.display_offset > before
      }
    })
    return { work: FRAMES, secs }
  })
  if (!repinned) {
    console.error('wasm-bench: the scrolled stream stopped re-pinning the viewport; the shape is a repin')
    process.exit(1)
  }
  t.free()
}
{
  const FRAMES = 200
  const t = newCpuTerm()
  t.set_scrollback_limit(15000)
  t.process(new TextEncoder().encode(styledLines(12000)))
  t.render()
  report.wasm_cpu_styled_scrub_present_fps = rate(() => {
    t.scroll_to_bottom()
    const secs = timeIt(() => {
      for (let i = 0; i < FRAMES; i++) {
        t.scroll_lines(3)
        t.render()
      }
    })
    return { work: FRAMES, secs }
  })
  t.free()
}
console.error(
  `wasm-bench: cpu uniform-flood ${report.wasm_cpu_uniform_flood_present_fps.toFixed(1)} fps | ` +
    `scrolled-stream ${report.wasm_cpu_scrolled_stream_present_fps.toFixed(1)} fps | ` +
    `styled-scrub ${report.wasm_cpu_styled_scrub_present_fps.toFixed(1)} fps`
)

// --- GPU module: ingest + per-frame wasm-side build cost --------------------
report.wasm_gpu_ingest_mixed_mbps = rate(() => {
  const t = newGpuTerm()
  const secs = timeIt(() => t.process(MIX))
  t.free()
  return { work: MIX.length / 1e6, secs }
})
{
  const t = newGpuTerm()
  t.set_scrollback_limit(15000)
  t.process(new TextEncoder().encode(rotatingLines(12000)))
  // render() without init(): the full wasm-side frame build runs (pump +
  // cell_frame_into + effects + stamp + spill), then the GPU access errors.
  // Assert the seam once so a future behavior change cannot silently turn
  // this scenario into a no-op (or into a real GL call).
  let seam = ''
  try {
    t.render()
  } catch (e) {
    seam = String(e)
  }
  if (!seam.includes('before init()')) {
    console.error(`wasm-bench: unexpected gpu render() seam: ${JSON.stringify(seam)}`)
    process.exit(1)
  }
  const FRAMES = 200
  const oneChar = new TextEncoder().encode('x')
  report.wasm_gpu_frame_build_fps = rate(() => {
    const secs = timeIt(() => {
      for (let i = 0; i < FRAMES; i++) {
        t.process(oneChar) // the GPU module has no process_str export
        try {
          t.render()
        } catch {
          /* expected: no GL in node */
        }
      }
    })
    return { work: FRAMES, secs }
  })
  report.wasm_gpu_scroll_frame_build_fps = rate(() => {
    t.scroll_to_bottom()
    const secs = timeIt(() => {
      for (let i = 0; i < FRAMES; i++) {
        t.scroll_lines(3)
        try {
          t.render()
        } catch {
          /* expected: no GL in node */
        }
      }
    })
    return { work: FRAMES, secs }
  })
  t.free()
  console.error(
    `wasm-bench: gpu ingest ${report.wasm_gpu_ingest_mixed_mbps.toFixed(1)} MB/s | ` +
      `frame-build ${report.wasm_gpu_frame_build_fps.toFixed(1)} fps | ` +
      `scroll frame-build ${report.wasm_gpu_scroll_frame_build_fps.toFixed(1)} fps`
  )
}

// --- meta + the single JSON report line -------------------------------------
report.wasm_cpu_module_bytes = cpuWasmBytes
report.wasm_gpu_module_bytes = gpuWasmBytes
report.n = N
report.warmup = WARMUP
const ordered = Object.fromEntries(
  Object.entries(report).map(([k, v]) => [k, typeof v === 'number' ? Number(v.toFixed(3)) : v])
)
console.log(JSON.stringify(ordered))
