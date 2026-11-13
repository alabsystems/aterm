<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# The wasm-glue toy

`src/` is a small crate built on `aterm-wasm-abi`'s `#[export]`, its module is
`aterm_wasm_abi_toy.wasm`, `golden/` holds the `.js` and `.d.ts` glue the
generator writes for it (an ES module, wasm-bindgen's `--target web` shape),
and `drive.mjs` loads that glue in a page of the pinned chrome-headless-shell
and checks it. `targo --unverified verify wasm-glue` holds the generator: it
regenerates the glue, compares it with `golden/` and runs `drive.mjs`, with a
planted conversion bug the driver must catch.
