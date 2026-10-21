// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU glyph atlas's texture residency and the frame encode's background-buffer slice.

use aterm_spec::derive::Model;

/// GPU atlas texture residency: the glyph atlas across shelf appends
/// (`occupied_h' = occupied_h + 1`) and full repacks (`epoch' = epoch + 1`), the
/// fx atlas across region appends (`fx_regions' = fx_regions + 1`) and
/// compactions (`fx_gen' = fx_gen + 1`), and the fx palette each frame stages.
///
/// * At `Buggy = 0`, when `occupied_h > uploaded_h` within `tex_h`, syncing the
///   atlas uploads the dirty row band `[uploaded_h .. occupied_h)` so
///   `uploaded_h' = occupied_h` without re-minting the texture (`AtlasRowsSynchronized`
///   holds at present). A dirty resident upload first waits for all outstanding
///   frame readers; fresh textures and unchanged resident textures keep them.
///   The fx atlas is the same law per region: an append into a texture earlier
///   frames read first waits for every reader, and a compaction is a fresh
///   texture (frames in flight keep the old one) that needs none.
/// * The frame-slot ring: `MaxReaders` slots, `busy0..2` set while the frame
///   that took the slot is in flight (frames complete in any order:
///   `GpuComplete0..2`), `cur` the slot the next frame takes. A frame
///   (`SubmitFrame`) waits until `cur` is free, writes its palette into that
///   slot's own stream, and goes in flight there, so a palette write never
///   lands under a reader (`PaletteRingExclusive`). A settle (`SyncAndPresent`'s
///   reader wait) frees every slot.
/// * At `Buggy = 1`, removing `map.len()` from the texture key without uploading
///   the dirty row band skips the upload when `uploaded_h > 0`, leaving
///   `uploaded_h < occupied_h` at present. The same branch omits the reader wait
///   and records an upload conflict when a dirty resident texture has readers.
/// * At `Buggy = 2`, an fx region append into a resident texture skips the
///   reader wait: a region of a texture a frame in flight samples is rewritten
///   under it (`region_race`), which `AtlasRowsSynchronized`'s reader clause
///   refuses.
/// * At `Buggy = 3`, the palette lives in one texture every frame shares
///   (slot 0's): a frame's palette write lands under the frame in flight
///   there (`pal_conflict`).
/// * At `Buggy = 4`, a frame skips the slot wait and stages into a slot whose
///   last frame is still in flight (`pal_conflict`).
///
/// Tier-1: `aterm-gpu`'s `atlas_upload_sync_tests` drive the real glyph-atlas
/// upload and the real fx sync and palette staging against this model, each
/// with a negative control.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn metal_atlas_residency_model() -> Model {
    aterm_spec::ty_model! {
        MetalAtlasResidency {
            const MaxEpoch = 2;
            const TexH = 3;
            const MaxReaders = 3;
            const MaxFxRegions = 2;
            const Buggy = 0;

            var epoch = 1;
            var tex_h = 3;
            var occupied_h = 1;
            var uploaded_h = 1;
            var presented = 0;
            var readers = 0;
            var upload_conflict = 0;
            var fx_gen = 1;
            var fx_regions = 1;
            var fx_uploaded = 1;
            var region_race = 0;
            var pal_conflict = 0;
            var cur = 0;
            var busy0 = 0;
            var busy1 = 0;
            var busy2 = 0;

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

            action FxAppend when (fx_regions <= MaxFxRegions - 1) {
                fx_regions = fx_regions + 1;
                presented = 0;
            }

            action FxCompact when (fx_gen <= MaxEpoch - 1) {
                fx_gen = fx_gen + 1;
                fx_regions = 1;
                fx_uploaded = 0;
                presented = 0;
            }

            // A frame takes slot `cur` once its last frame completed, writes
            // its palette there (`Buggy = 3`: into slot 0's, shared) and goes
            // in flight in it.
            action SubmitFrame when (presented == 1 && readers <= MaxReaders - 1
                && (Buggy == 4
                    || (cur == 0 && busy0 == 0)
                    || (cur == 1 && busy1 == 0)
                    || (cur == 2 && busy2 == 0)))
            {
                pal_conflict = if Buggy == 3 {
                    if busy0 == 1 { 1 } else { pal_conflict }
                } else {
                    if (cur == 0 && busy0 == 1)
                        || (cur == 1 && busy1 == 1)
                        || (cur == 2 && busy2 == 1)
                    {
                        1
                    } else {
                        pal_conflict
                    }
                };
                busy0 = if cur == 0 { 1 } else { busy0 };
                busy1 = if cur == 1 { 1 } else { busy1 };
                busy2 = if cur == 2 { 1 } else { busy2 };
                readers = readers + 1;
                cur = if cur == MaxReaders - 1 { 0 } else { cur + 1 };
            }

            action GpuComplete0 when (busy0 == 1) {
                busy0 = 0;
                readers = readers - 1;
            }

            action GpuComplete1 when (busy1 == 1) {
                busy1 = 0;
                readers = readers - 1;
            }

            action GpuComplete2 when (busy2 == 1) {
                busy2 = 0;
                readers = readers - 1;
            }

            action SyncAndPresent when (presented == 0) {
                // Every update reads the PRE-state: the fx append (at staging)
                // sees the readers the frame found. A settle frees every slot.
                readers = if uploaded_h > 0 && occupied_h > uploaded_h {
                    if Buggy == 1 { readers } else { 0 }
                } else {
                    if fx_uploaded > 0 && fx_regions > fx_uploaded {
                        if Buggy == 2 { readers } else { 0 }
                    } else {
                        readers
                    }
                };
                busy0 = if uploaded_h > 0 && occupied_h > uploaded_h {
                    if Buggy == 1 { busy0 } else { 0 }
                } else {
                    if fx_uploaded > 0 && fx_regions > fx_uploaded {
                        if Buggy == 2 { busy0 } else { 0 }
                    } else {
                        busy0
                    }
                };
                busy1 = if uploaded_h > 0 && occupied_h > uploaded_h {
                    if Buggy == 1 { busy1 } else { 0 }
                } else {
                    if fx_uploaded > 0 && fx_regions > fx_uploaded {
                        if Buggy == 2 { busy1 } else { 0 }
                    } else {
                        busy1
                    }
                };
                busy2 = if uploaded_h > 0 && occupied_h > uploaded_h {
                    if Buggy == 1 { busy2 } else { 0 }
                } else {
                    if fx_uploaded > 0 && fx_regions > fx_uploaded {
                        if Buggy == 2 { busy2 } else { 0 }
                    } else {
                        busy2
                    }
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
                region_race = if Buggy == 2
                    && fx_uploaded > 0 && fx_regions > fx_uploaded && readers > 0
                {
                    1
                } else {
                    region_race
                };
                fx_uploaded = fx_regions;
                presented = 1;
            }

            invariant AtlasRowsSynchronized:
                presented == 0
                    || (uploaded_h == occupied_h && uploaded_h <= tex_h
                        && fx_uploaded == fx_regions && region_race == 0);

            invariant NoAtlasUploadConflict:
                upload_conflict == 0;

            invariant PaletteRingExclusive:
                pal_conflict == 0;
        }
    }
}

/// The armed tray card's two-slot ring (`aterm_gpu::tray_card_ring`): a change rewrites the slot
/// not shown with a CPU `replaceRegion:`, outside the queue's hazard tracking, so it first settles
/// the in-flight presents that may sample that slot, and only those.
///
/// Presents are numbered `1..=seq` (`aterm_gpu`'s count from zero, plus one; `0` is none) and the
/// queue completes them in order, `1..=done`. Each present samples the slot shown when it is
/// encoded. `last0`/`last1` are what the code RECORDS as each slot's last reader; `real0`/`real1`
/// are the presents that REALLY last sampled it. A rewrite of slot `k` is `Settle<k>` (through
/// the recorded reader, `present_may_read`: every committed present numbered no later), then `Show<k>` or
/// `Bake<k>`; `writing` says which slot's rewrite is under way and `before` where `done` stood
/// when its settle began.
///
/// * `Draw`: the card unchanged; its slot is recorded, and sampled, by the next present.
/// * `Show0`/`Show1`: the rewritten slot is shown to the next present (a ring of one slot mints
///   the second, never read: `last1 = 0`).
/// * `Bake0`/`Bake1`: a readback's tray bake rewrote it; the bake's command buffer is waited
///   inline, so it leaves no reader in flight and records none.
/// * `Bare`: a present that samples neither slot (another window's).
/// * `Complete`: the GPU finishes the oldest in-flight present.
/// * `Remint`: a size change mints a fresh ring of one slot for the next present.
/// * A present refused after its Submit B is committed (the drawable's present refused) keeps the
///   number its slot recorded and is waited inline: a `Draw` (or `Show<k>`), then a `Complete` for
///   it and every present before it.
///
/// Invariants, against the REAL readers, while a rewrite is under way:
/// `RewritesNoSlotUnderAReader` (no present that samples the slot is still in flight) and
/// `WaitsForNoPresentPastTheSlotsReader` (the settle waited for no present later than the slot's
/// last real reader: one that samples the other slot, which the second slot exists not to wait
/// for).
///
/// Mutants: `Buggy = 1` settles every present from the slot's reader on (the rule before
/// 969c8abb08, `await_presents_since`), waiting for the in-flight present that shows the other
/// slot; `Buggy = 2` settles nothing; `Buggy = 3` has a bake record the next present as the slot's
/// reader, so a later rewrite waits for that present though it samples the other slot; `Buggy = 4`
/// has a refused present hand its number back (`Refuse`): the slot it sampled records a number the
/// next present takes too, so a later rewrite of that slot waits for that present, which samples
/// the other one.
///
/// Tier-0: `aterm-models-render/tests/derived_gpu.rs`. Tier-1:
/// `aterm-gpu/tests/conformance_tray_card_ring.rs` drives the real `TrayCardRing` and
/// `present_may_read` against a simulated in-order queue, with the old rule, the old bake and the
/// old refused present's number as negative controls.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn metal_tray_card_ring_model() -> Model {
    aterm_spec::ty_model! {
        MetalTrayCardRing {
            const MaxSeq = 4;
            const Buggy = 0;

            var seq = 0;
            var done = 0;
            var shown = 0;
            var last0 = 0;
            var last1 = 0;
            var real0 = 0;
            var real1 = 0;
            var writing = 0;
            var before = 0;

            action Draw when (writing == 0 && seq <= MaxSeq - 1) {
                last0 = if shown == 0 { seq + 1 } else { last0 };
                last1 = if shown == 1 { seq + 1 } else { last1 };
                real0 = if shown == 0 { seq + 1 } else { real0 };
                real1 = if shown == 1 { seq + 1 } else { real1 };
                seq = seq + 1;
            }

            action Settle1 when (writing == 0 && shown == 0 && seq <= MaxSeq - 1) {
                before = done;
                done = if Buggy == 2 {
                    done
                } else if Buggy == 1 {
                    if last1 > 0 && seq > done { seq } else { done }
                } else if last1 > seq {
                    seq
                } else if last1 > done {
                    last1
                } else {
                    done
                };
                writing = 1;
            }

            action Settle0 when (writing == 0 && shown == 1 && seq <= MaxSeq - 1) {
                before = done;
                done = if Buggy == 2 {
                    done
                } else if Buggy == 1 {
                    if last0 > 0 && seq > done { seq } else { done }
                } else if last0 > seq {
                    seq
                } else if last0 > done {
                    last0
                } else {
                    done
                };
                writing = 2;
            }

            action Show1 when (writing == 1 && seq <= MaxSeq - 1) {
                shown = 1;
                last1 = seq + 1;
                real1 = seq + 1;
                seq = seq + 1;
                writing = 0;
                before = 0;
            }

            action Show0 when (writing == 2 && seq <= MaxSeq - 1) {
                shown = 0;
                last0 = seq + 1;
                real0 = seq + 1;
                seq = seq + 1;
                writing = 0;
                before = 0;
            }

            action Bake1 when (writing == 1 && seq <= MaxSeq - 1) {
                shown = 1;
                last1 = if Buggy == 3 { seq + 1 } else { 0 };
                writing = 0;
                before = 0;
            }

            action Bake0 when (writing == 2 && seq <= MaxSeq - 1) {
                shown = 0;
                last0 = if Buggy == 3 { seq + 1 } else { 0 };
                writing = 0;
                before = 0;
            }

            action Bare when (writing == 0 && seq <= MaxSeq - 1) {
                seq = seq + 1;
            }

            action Refuse when (Buggy == 4 && writing == 0 && seq <= MaxSeq - 1) {
                last0 = if shown == 0 { seq + 1 } else { last0 };
                last1 = if shown == 1 { seq + 1 } else { last1 };
                done = seq;
            }

            action Complete when (writing == 0 && done <= seq - 1) {
                done = done + 1;
            }

            action Remint when (writing == 0 && seq <= MaxSeq - 1) {
                shown = 0;
                last0 = seq + 1;
                last1 = 0;
                real0 = seq + 1;
                real1 = 0;
                seq = seq + 1;
            }

            invariant RewritesNoSlotUnderAReader:
                writing == 0
                    || (writing == 1 && real1 <= done)
                    || (writing == 2 && real0 <= done);
            invariant WaitsForNoPresentPastTheSlotsReader:
                writing == 0
                    || done <= before
                    || (writing == 1 && done <= real1)
                    || (writing == 2 && done <= real0);
            bound PresentsAndRecordsFitTheBox:
                seq <= MaxSeq && done <= MaxSeq && shown <= 1
                && last0 <= MaxSeq && last1 <= MaxSeq && real0 <= MaxSeq && real1 <= MaxSeq
                && writing <= 2 && before <= MaxSeq;
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
