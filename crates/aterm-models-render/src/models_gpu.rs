// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU glyph atlas's texture residency and the frame encode's background-buffer slice.

use aterm_spec::derive::Model;

/// GPU glyph atlas texture residency across shelf appends (`occupied_h' = occupied_h + 1`)
/// and full repacks (`epoch' = epoch + 1`).
///
/// * At `Buggy = 0`, when `occupied_h > uploaded_h` within `tex_h`, syncing the
///   atlas uploads the dirty row band `[uploaded_h .. occupied_h)` so
///   `uploaded_h' = occupied_h` without re-minting the texture (`AtlasRowsSynchronized`
///   holds at present). A dirty resident upload first waits for all outstanding
///   frame readers; fresh textures and unchanged resident textures keep them.
/// * At `Buggy = 1`, removing `map.len()` from the texture key without uploading
///   the dirty row band skips the upload when `uploaded_h > 0`, leaving
///   `uploaded_h < occupied_h` at present. The same branch omits the reader wait
///   and records an upload conflict when a dirty resident texture has readers.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn metal_atlas_residency_model() -> Model {
    aterm_spec::ty_model! {
        MetalAtlasResidency {
            const MaxEpoch = 2;
            const TexH = 3;
            const MaxReaders = 3;
            const Buggy = 0;

            var epoch = 1;
            var tex_h = 3;
            var occupied_h = 1;
            var uploaded_h = 1;
            var presented = 0;
            var readers = 0;
            var upload_conflict = 0;

            action AppendShelf when (occupied_h <= tex_h - 1) {
                occupied_h = occupied_h + 1;
                presented = 0;
            }

            action FullRepack when (epoch <= MaxEpoch - 1) {
                epoch = epoch + 1;
                occupied_h = 1;
                uploaded_h = 0;
                presented = 0;
            }

            action SubmitFrame when (presented == 1 && readers <= MaxReaders - 1) {
                readers = readers + 1;
            }

            action GpuComplete when (readers > 0) {
                readers = readers - 1;
            }

            action SyncAndPresent when (presented == 0) {
                readers = if uploaded_h > 0 && occupied_h > uploaded_h {
                    if Buggy == 1 { readers } else { 0 }
                } else {
                    readers
                };
                upload_conflict = if Buggy == 1
                    && uploaded_h > 0 && occupied_h > uploaded_h && readers > 0
                {
                    1
                } else {
                    upload_conflict
                };
                uploaded_h = if uploaded_h == 0 {
                    occupied_h
                } else {
                    if Buggy == 1 { uploaded_h } else { occupied_h }
                };
                presented = 1;
            }

            invariant AtlasRowsSynchronized:
                presented == 0 || (uploaded_h == occupied_h && uploaded_h <= tex_h);

            invariant NoAtlasUploadConflict:
                upload_conflict == 0;
        }
    }
}

/// The per-frame background-instance buffer's create/bind/slice discipline in
/// `aterm_gpu`'s frame encode.
///
/// The cell walk appends one `BgInstance` per cell with a non-default background
/// (`Append`), then the frame is encoded (`Encode`): the buffer is created, bound and
/// sliced only when it holds an instance. The defect sliced unconditionally, and a
/// zero-cell frame panicked in wgpu ("buffer slices can not be empty"); the fix gates on
/// `aterm_gpu::should_slice`. (The spec's `SliceImpliesFill`, `sliced => bgInst # 0`, said
/// `NeverSliceEmpty` again over a natural `bgInst`, so it is not repeated here.)
///
/// Mutant: `Buggy = 1` slices whatever the frame holds.
///
/// Tier-0: `aterm-models-render/tests/derived_gpu.rs`. Tier-1:
/// `aterm-gpu/tests/conformance_gpuencode.rs` drives the real `should_slice` after each
/// frame's appends, with an empty slice and a missed slice as negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn gpu_encode_model() -> Model {
    aterm_spec::ty_model! {
        GpuEncode {
            const MaxCells = 4;
            const Buggy = 0;
            var bgInst = 0;
            var encoded = 0;
            var sliced = 0;

            action Append when (encoded == 0 && bgInst <= MaxCells - 1) { bgInst = bgInst + 1; }
            action Encode when (encoded == 0) {
                encoded = 1;
                sliced = if Buggy == 1 || bgInst > 0 { 1 } else { 0 };
            }

            // The buffer is sliced and bound only when it holds an instance: the
            // precondition wgpu enforces, which the panic violated.
            invariant NeverSliceEmpty: sliced == 0 || bgInst > 0;
            bound FillsFitTheFrame: bgInst <= MaxCells;
        }
    }
}
