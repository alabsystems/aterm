// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GPU glyph atlas's texture residency and the frame encode's background-buffer slice.

use aterm_spec::derive::Model;

/// GPU atlas texture residency.
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
            var fx_rewrite = 0;
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
                fx_rewrite = 0;
                presented = 0;
            }

            action FxReuse when (fx_regions > 0 && fx_rewrite == 0) {
                fx_rewrite = 1;
                presented = 0;
            }

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
                readers = if uploaded_h > 0 && occupied_h > uploaded_h {
                    if Buggy == 1 { readers } else { 0 }
                } else {
                    if fx_uploaded > 0 && fx_regions > fx_uploaded {
                        if Buggy == 2 { readers } else { 0 }
                    } else {
                        if fx_uploaded > 0 && fx_rewrite == 1 {
                            if Buggy == 5 { readers } else { 0 }
                        } else {
                            readers
                        }
                    }
                };
                busy0 = if uploaded_h > 0 && occupied_h > uploaded_h {
                    if Buggy == 1 { busy0 } else { 0 }
                } else {
                    if fx_uploaded > 0 && fx_regions > fx_uploaded {
                        if Buggy == 2 { busy0 } else { 0 }
                    } else {
                        if fx_uploaded > 0 && fx_rewrite == 1 {
                            if Buggy == 5 { busy0 } else { 0 }
                        } else {
                            busy0
                        }
                    }
                };
                busy1 = if uploaded_h > 0 && occupied_h > uploaded_h {
                    if Buggy == 1 { busy1 } else { 0 }
                } else {
                    if fx_uploaded > 0 && fx_regions > fx_uploaded {
                        if Buggy == 2 { busy1 } else { 0 }
                    } else {
                        if fx_uploaded > 0 && fx_rewrite == 1 {
                            if Buggy == 5 { busy1 } else { 0 }
                        } else {
                            busy1
                        }
                    }
                };
                busy2 = if uploaded_h > 0 && occupied_h > uploaded_h {
                    if Buggy == 1 { busy2 } else { 0 }
                } else {
                    if fx_uploaded > 0 && fx_regions > fx_uploaded {
                        if Buggy == 2 { busy2 } else { 0 }
                    } else {
                        if fx_uploaded > 0 && fx_rewrite == 1 {
                            if Buggy == 5 { busy2 } else { 0 }
                        } else {
                            busy2
                        }
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
                region_race = if (Buggy == 2
                    && fx_uploaded > 0 && fx_regions > fx_uploaded && readers > 0)
                    || (Buggy == 5 && fx_uploaded > 0 && fx_regions == fx_uploaded
                        && fx_rewrite == 1 && readers > 0
                        && (uploaded_h == 0 || occupied_h <= uploaded_h))
                {
                    1
                } else {
                    region_race
                };
                fx_uploaded = fx_regions;
                fx_rewrite = 0;
                presented = 1;
            }

            invariant AtlasRowsSynchronized:
                presented == 0
                    || (uploaded_h == occupied_h && uploaded_h <= tex_h
                        && fx_uploaded == fx_regions && fx_rewrite == 0
                        && region_race == 0);

            invariant NoAtlasUploadConflict:
                upload_conflict == 0;

            invariant PaletteRingExclusive:
                pal_conflict == 0;
        }
    }
}

/// The armed tray card's two-slot ring (`aterm_gpu::tray_card_ring`).
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

/// The per-frame background-instance buffer's create/bind/slice discipline in `aterm_gpu`'s frame encode.
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

            invariant NeverSliceEmpty: sliced == 0 || bgInst > 0;
            bound FillsFitTheFrame: bgInst <= MaxCells;
        }
    }
}
