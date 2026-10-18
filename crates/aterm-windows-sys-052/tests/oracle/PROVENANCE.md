<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# The WB.1 oracle: this crate against the registry windows-sys 0.52.0

This directory is its own workspace (no patch table, the `libc-oracle`
layout): `conformance/` compares this crate (`under-test/` names it) with
the registry windows-sys 0.52.0, the crate it replaces for ring 0.17.14 on
aarch64 Windows (docs/AUDIT-third-party-2026-10-15.md, windows-bindings step
1). It was run on 2026-10-15, on both Windows triples aterm builds,
`x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`, under rustup's stable
1.96 (Trust ships no Windows std; nothing was linked or run). `targo
--unverified run -p aterm-win32-oracle -- oracle-052` drives it (a STOCK
EXCEPTION: it needs rustup's stable and a Windows std), `tests/recorded.rs`
holds the recorded answers on every host, and the Windows cells of `targo
--unverified verify cells` keep compiling ring against this crate.

## What it compared, and the result

- **Conformance** (`conformance/src/lib.rs`, const-asserts decided by `cargo
  check`): alias identity both ways, the size and alignment of `BOOL` and
  `PROCESSOR_FEATURE_ID`, the type and value of
  `PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE`, and `IsProcessorFeaturePresent`
  coerced to a function pointer both ways (ABI `"system"`, unsafety, argument
  and return types). PASS on both triples.
- **Import** (the objects of an rlib, read with `llvm-nm`): a call through
  each side references `__imp_IsProcessorFeaturePresent`, resolved from
  `kernel32.dll` (this crate's rlib bundles its raw-dylib import members;
  upstream's `windows.0.52.0.lib` maps it there too). Equal on both triples.
- **Negative controls**, each a copy with one planted mismatch, all caught on
  both triples: `BOOL` as `u32` (same size, so only the identity check sees
  it), `PROCESSOR_FEATURE_ID` as `u64`, the constant as 31, ABI `"C"`, an
  `i32` argument and a `u8` return (conformance); `link_name` set to
  `IsProcessorFeaturePresentA`, and `kernelbase.dll` (import).

Verdict: `windows-sys 0.52 oracle: PASS`.
