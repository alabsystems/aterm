<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# The wgpu-WebGL recording

These are the frames `aterm-gpu-web` presented on a WebGL2 canvas while it
still drew through wgpu 29's WebGL2 backend (glow over web-sys, wasm-bindgen
glue), before wasm-platform WP.8 (docs/AUDIT-third-party-2026-10-15.md)
replaced that backend with aterm-gpu's own GL arm driven through
aterm-wasm-abi imports. `verify browser` holds every later module to them:
the recorded-frame differential (`crates/aterm-verify/src/browser/recorded.rs`).

- `recording.json` names the module, the browser and the WebGL2 adapter, and
  each frame's size and the SHA-256 of its RGBA8 bytes.
- `parity-<k>.png` are the four frames of cpu_gpu_parity.rs's grid the lane
  also holds to the CPU module; `diff-<k>.png` the seven frames of the lane's
  wider differential grid (weights and styles, every underline shape, indexed
  and true colours, box drawing, blocks and braille, a hyperlink, a sixel
  image, a cursor shape), one line more per frame.

Recorded on 2026-10-16 on mac-arm (Apple M4 Pro, macOS), in the pinned
chrome-headless-shell 155.0.8059.84 with WebGL2 on ANGLE's Metal backend, by
`targo --unverified verify browser` on devendor/lgpu at the commit that added
this directory, whose module was still the wgpu one. Every run writes its own
frames in this layout, then to `target/verify/cells/browser/gpu-frames/` and
since the lane runs more than one ANGLE backend to
`target/verify/cells/browser/<backend>/gpu-frames/`; this directory is a copy
of one such run. Two more runs on the same machine matched it byte for byte.

On the recording's adapter the differential is byte for byte; on another it
holds each frame within the CPU parity tolerance, and says so. The lane's
SwiftShader session is such a run: on 2026-10-17 every frame came within 1 of
this recording (`docs/measured/browser-swiftshader-2026-10-17.md`). The recording
is never re-made: the module that drew it is gone, so a new recording would
hold the GL arm to itself.
