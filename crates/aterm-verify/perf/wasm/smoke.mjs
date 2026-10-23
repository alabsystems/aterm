// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The wasm host-boundary smoke (`verify wasm-smoke`): drive the DEBUG
// builds of the two web modules in a page of the pinned chrome-headless-shell
// through the exports only a debug build carries (each crate's src/host.rs, both through aterm-wasm-abi's
// `aterm_host` imports), and check what crosses the host boundary in each
// direction.
//
//   clocks  aterm_time's two browser clocks, which the module links in with
//           aterm_time::wasm_clock_host!: Instant never goes backwards over a
//           thousand reads, and its span across a 20 ms wait lies within the
//           performance.now() reads that bracket it; SystemTime is within
//           1 ms of the Date.now() reads that bracket it.
//   panic   a constructor installs aterm_wasm_abi::panic_hook; a panic then
//           reaches console.error ONCE with the panic message, then
//           "\n\nStack:\n\n", a JS stack that names a wasm frame, then "\n\n"
//           (console_error_panic_hook 0.1.7's format), and the call throws.
//
// One line per check on the console; `run({ cpu, gpu, font })` (the two
// modules' ES module glue and the font, by URL) answers { failures }, 0 only
// when every check held.

let font
const THEME = [0x00e0e0e0, 0x00101010, 0x00e0e0e0, 0x00404060]

let failures = 0
function check(name, ok, detail) {
  console.error(`wasm-smoke: ${ok ? 'ok  ' : 'FAIL'} ${name}${ok ? '' : `: ${detail}`}`)
  if (!ok) failures++
}

function clockSmoke(label, mod) {
  if (typeof mod.atermSmokeClocks !== 'function') {
    check(`${label} clocks`, false, 'no atermSmokeClocks export: is this a debug build?')
    return
  }
  const reads = []
  for (let i = 0; i < 1000; i++) reads.push(mod.atermSmokeClocks()[0])
  const back = reads.findIndex((v, i) => i > 0 && v < reads[i - 1])
  check(`${label} Instant is monotonic`, back < 0, `read ${back}: ${reads[back - 1]} then ${reads[back]}`)

  // Bracketed, not timed: the Instant span between two reads lies inside the
  // JS span around them and outside the JS span between them, whatever the
  // scheduler does. 0.01 ms covers the float conversions.
  const pA = performance.now()
  const e0 = mod.atermSmokeClocks()[0]
  const pB = performance.now()
  while (performance.now() - pB < 20) {
    /* wait on the JS clock */
  }
  const pC = performance.now()
  const e1 = mod.atermSmokeClocks()[0]
  const pD = performance.now()
  const span = e1 - e0
  check(
    `${label} Instant tracks performance.now()`,
    span >= pC - pB - 0.01 && span <= pD - pA + 0.01,
    `Instant ${span} ms, performance.now() between ${pC - pB} and ${pD - pA} ms`
  )

  const before = Date.now()
  const wall = mod.atermSmokeClocks()[1]
  const after = Date.now()
  check(`${label} SystemTime is within 1 ms of Date.now()`, wall >= before - 1 && wall <= after + 1, `Date.now() ${before}..${after}, SystemTime ${wall}`)
}

function panicSmoke(label, mod, Terminal) {
  if (typeof mod.atermSmokePanic !== 'function') {
    check(`${label} panic`, false, 'no atermSmokePanic export: is this a debug build?')
    return
  }
  // The hook is installed by the constructor, as in the page.
  new Terminal(24, 80, font, 16.0, ...THEME)
  const logged = []
  const original = console.error
  console.error = (...args) => logged.push(args)
  let threw = null
  const text = `aterm smoke panic ${label} 1234`
  try {
    mod.atermSmokePanic(text)
  } catch (e) {
    threw = e
  } finally {
    console.error = original
  }
  check(`${label} panic throws`, threw !== null, 'the panicking export returned')
  check(`${label} panic logs once`, logged.length === 1, `console.error called ${logged.length} times`)
  const msg = logged.length === 1 && logged[0].length === 1 ? logged[0][0] : null
  check(`${label} panic logs one string`, typeof msg === 'string', JSON.stringify(logged).slice(0, 200))
  if (typeof msg !== 'string') return
  const sep = '\n\nStack:\n\n'
  const index = msg.indexOf(sep)
  const head = msg.slice(0, index)
  const rest = msg.slice(index + sep.length)
  check(`${label} panic message`, /^panicked at .+:\d+:\d+:\n/.test(head) && head.endsWith(`\n${text}`), JSON.stringify(head))
  check(`${label} panic stack header`, msg.includes(sep), JSON.stringify(msg.slice(0, 300)))
  check(`${label} panic stack has a wasm frame`, /wasm:\/\/|\.wasm/.test(rest), JSON.stringify(rest.slice(0, 300)))
  check(`${label} panic trailer`, msg.endsWith('\n\n') && !rest.startsWith('\n'), JSON.stringify(msg.slice(-40)))
}

export async function run(urls) {
  const r = await fetch(urls.font)
  if (!r.ok) throw new Error(`${urls.font}: HTTP ${r.status}`)
  font = new Uint8Array(await r.arrayBuffer())
  const cpu = await import(urls.cpu)
  const gpu = await import(urls.gpu)
  // The default init, as a page loads each module.
  await cpu.default()
  await gpu.default()
  // Clocks first: a panic traps, and a module is not called again after one.
  clockSmoke('cpu', cpu)
  clockSmoke('gpu', gpu)
  panicSmoke('cpu', cpu, cpu.AtermTerminal)
  panicSmoke('gpu', gpu, gpu.AtermGpuTerminal)
  console.error(`wasm-smoke: ${failures === 0 ? 'PASS' : `FAIL (${failures})`}`)
  return { failures }
}
