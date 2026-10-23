<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# The wasm-glue toy

`src/` is a small crate built on `aterm-wasm-abi`'s `#[export]`, its module is
`aterm_wasm_abi_toy.wasm`, `golden/` holds the `.js` and `.d.ts` glue the
generator writes for it (an ES module, wasm-bindgen's `--target web` shape),
and `drive.mjs` loads that glue in a page of the pinned chrome-headless-shell
and checks it. `targo --unverified verify wasm-glue` holds the generator: it
regenerates the glue, compares it with `golden/` and runs `drive.mjs`, then
runs the same driver on wasm-bindgen 0.2.108's glue for a `#[wasm_bindgen]`
twin of the toy (the oracle row).

Until 2026-10-22 the generator also wrote a CommonJS `nodejs` flavor and the
driver ran under node; both are gone (docs/DESIGN-init-system-2026-10-20.md,
S10). The record below is of the one-time runs, which covered both flavors.

## The one-time oracle against wasm-bindgen

On 2026-10-16 (WP.6, docs/AUDIT-third-party-2026-10-15.md, wasm-platform step
6), `drive.mjs`'s expectations were run once against the code this layer
replaced: a `#[wasm_bindgen]` twin of the toy, built on rustup's stable
toolchain and bound with the wasm-bindgen 0.2.108 CLI in both flavors. Every
check held except the two `drive.mjs` marks STRICTER (`another class`, and
the misuse throws), and `another class` failed in both flavors, which showed
the glue under test was wasm-bindgen's. A stand-in CLI that handed the oracle
aterm's own glue was refused.

The same oracle later compared the `.d.ts` (GLUE-NAMES, 2026-10-16): every
declaration with a parameter list in `golden/` (functions, methods, statics
and constructors, parameter names and types included) against wasm-bindgen
0.2.108's glue for the twin, as sets, in both flavors. Against goldens that
still named parameters `arg$0`, `arg$1`, … it failed (94 declarations
differed); against the goldens committed with the Rust names it passed (59
nodejs and 61 web declarations identical), and a copy with one parameter
renamed failed, as it must. Class member order and the raw `InitOutput` rows
(the ABI's own) were not compared.

The script that ran both (`oracle.sh`, a stock toolchain and perl) is
deleted: it was a one-time comparison, and the generator is now held by
`verify wasm-glue`, not by wasm-bindgen.
