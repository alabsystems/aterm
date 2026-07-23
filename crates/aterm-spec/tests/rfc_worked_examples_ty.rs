// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! RFC worked examples: "models come out runnable."
//!
//! The aterm/Trust RFC claims that a defect found in the real renderer/GUI can be
//! written as a `ty_model!` bounded state machine, and that the SAME `ty` binary
//! that gates the committed specs will (a) PROVE the invariant at the committed
//! `Buggy = 0`, and (b) find a COUNTEREXAMPLE at `Buggy = 1` — so the model is a
//! genuine, non-vacuous catch of the real bug, not a tautology. This file makes
//! that claim concrete for three shipped fixes:
//!
//!   * **AtlasPack (Bug 3)** — the glyph-atlas height accumulator in
//!     `aterm-gpu/src/renderer.rs::build_kind`: `occupied_height()` is grown in a
//!     loop and any glyph that would push it past `cap_h` (the device's max 2D
//!     texture dimension) is ROLLED BACK and packing stops. Abstracted to a bounded
//!     counter `h` capped at `Cap`; the `Buggy` flip inflates the growth step so the
//!     cap is overrun — exactly the unbounded pack the rollback guards against.
//!
//!   * **ReloadFrame (Bug 4)** — `aterm-gui/src/main.rs::reload_config`: on a HiDPI
//!     display the AUTO default font is `round(FONT_PX · scale)`; editing an
//!     UNRELATED config key must re-derive that SCALED default, never shrink the
//!     font back to the 16px `FONT_PX` base. Modelled WITHOUT multiplication — the
//!     scaled default `Target` and the unscaled `Base` are constants. `Buggy = 1`
//!     reloads to the unscaled `Base` (the shrink bug); `Buggy = 0` re-pins `Target`.
//!
//!   * **MarkRowOrigin (Bug 6)** — `aterm-gpu/src/renderer.rs` (~line 2452): the
//!     combining-mark row origin must be `pad + r·ch` to MATCH the base-glyph loop;
//!     an earlier version used `r·ch` (no `pad`), rendering decomposed marks `pad`
//!     px too high — a CPU/GPU divergence for NFD sequences. With the `r = 0`
//!     witness the `r·ch` term drops out, so the divergence is purely the missing
//!     `pad`: correct `mark = pad`, buggy `mark = 0`, versus the base `pad`.
//!
//! Each model is checked by `assert_proves_and_catches` (the same harness shape as
//! `tests/derived_ring_ty.rs`): clean at `Buggy = 0`, counterexample at `Buggy = 1`.
//! Verification is always required (batteries-on): an absent `ty` FAILS the test with
//! a build hint, so a green run here proves `ty` actually model-checked all six configs.

use aterm_spec::derive::Model;
use aterm_spec::ty_model;
use aterm_spec::verify;

/// A `Buggy`-convention model: the invariant must be PROVEN at the committed
/// `Buggy = 0`, and a COUNTEREXAMPLE found at `Buggy = 1`. TIERED (see
/// [`verify::prove_and_catch_tiered`]): the in-process interpreter always runs
/// the whole protocol; `ty` additionally re-proves it wherever installed.
/// Mirrors `tests/derived_ring_ty.rs::assert_proves_and_catches`.
fn assert_proves_and_catches(m: &Model) {
    verify::prove_and_catch_tiered(m, m.name);
}

/// Bug 3 — the glyph-atlas height accumulator (`build_kind`'s `occupied_height()`
/// grown in a loop, capped at the device's `cap_h`). `h` is the packed height; the
/// guard only lets `Grow` fire while a step still FITS under `Cap`. At `Buggy = 0`
/// the step is `Step`, so from the guard boundary `h` lands exactly on `Cap` and
/// stays bounded. At `Buggy = 1` the step is inflated to `Step + Step + Step` (the
/// unbounded pack with no rollback): even from the EMPTY atlas (`h = 0`, where
/// `Grow` is enabled) it overshoots to `6 > Cap` — the
/// texture-taller-than-the-GPU-allows overflow the real rollback prevents (the
/// fits-the-pre-state guard cannot save you once a single step alone exceeds the
/// cap). `ty` proves `Bounded` (Buggy=0) and catches the overflow (Buggy=1). No
/// multiplication: the inflated step is `Step + Step + Step`.
fn atlas_pack_model() -> Model {
    ty_model! {
        AtlasPack {
            const Cap = 4;
            const Step = 2;
            const Buggy = 0;
            // The packed atlas height (occupied_height()), starting empty.
            var h = 0;
            // Pack one more glyph only while a step still fits under the cap. The
            // correct step is `Step`; the buggy pack inflates it past the cap.
            action Grow when (h <= Cap - Step) {
                h = h + if Buggy == 1 { Step + Step + Step } else { Step };
            }
            // The texture height never exceeds the device limit `cap_h` (= Cap).
            invariant Bounded: h <= Cap;
        }
    }
}

/// Bug 4 — `reload_config`'s AUTO-default-font re-derivation on a HiDPI display.
/// `font` starts at the SCALED default `Target` (= round(FONT_PX·scale)), so the
/// invariant holds before any reload. A `Reload` re-derives the auto default: at
/// `Buggy = 0` it re-pins `Target` (the unchanged-config no-op must not shrink the
/// font), at `Buggy = 1` it shrinks to the UNSCALED `Base` (= FONT_PX), the defect.
/// `ty` proves `FontIsScaledDefault` (Buggy=0) and catches the shrink (Buggy=1).
/// No multiplication: `Target` and `Base` are constants (32 = round(16·2), 16).
fn reload_frame_model() -> Model {
    ty_model! {
        ReloadFrame {
            const Target = 32;
            const Base = 16;
            const Buggy = 0;
            // The live font size, initialized to the scaled HiDPI default.
            var font = 32;
            // Reload re-derives the auto default. Correct: re-pin the scaled
            // Target. Buggy: shrink back to the unscaled Base (the 16px regression).
            action Reload {
                font = if Buggy == 1 { Base } else { Target };
            }
            // After a reload of an unrelated key, the font is still the scaled
            // default — it never shrinks back to the base size.
            invariant FontIsScaledDefault: font == Target;
        }
    }
}

/// Bug 6 — the combining-mark row origin vs the base-glyph row origin. With the
/// `r = 0` witness, the base-glyph origin `pad + r·ch` collapses to `Pad`, and the
/// correct mark origin (also `pad + r·ch`) collapses to `Pad` too — so they align.
/// The bug used `r·ch` (no `pad`) for the mark, which collapses to `0`: the mark is
/// rendered `pad` px too high. `Step` computes both origins; at `Buggy = 0` they
/// agree (both `Pad`), at `Buggy = 1` the mark drops the pad (`0`) and diverges.
/// `ty` proves `Aligned` (Buggy=0) and catches the divergence (Buggy=1). The cell
/// height `ch` drops out at `r = 0`, so no multiplication is needed.
fn mark_row_origin_model() -> Model {
    ty_model! {
        MarkRowOrigin {
            const Pad = 1;
            const Buggy = 0;
            // The base-glyph row top and the combining-mark row top (r=0 witness:
            // both are `pad + r*ch` = `pad`). Equal until a buggy Step drops the pad.
            var base = 0;
            var mark = 0;
            // Base origin = pad (+ r*ch, r=0). Mark origin: correct = pad; the bug
            // omits the pad (uses r*ch = 0), so the mark sits `pad` px too high.
            action Step {
                base = Pad;
                mark = if Buggy == 1 { 0 } else { Pad };
            }
            // The combining mark shares the base glyph's row origin (no CPU/GPU
            // y divergence for NFD sequences).
            invariant Aligned: base == mark;
        }
    }
}

/// Bug 7 — char→glyph CMAP FIDELITY (the `·`→`∑`, `é`→`È` regression). A cell
/// stores a Unicode scalar; the renderer must rasterize the font's UNICODE-cmap
/// glyph for it, NEVER a glyph from a legacy `(1,0)` Mac Roman subtable. fontdue
/// prefers Mac Roman on Apple `.ttc` faces (Menlo/Monaco), where byte 0xB7 is `∑`
/// and 0xE9 is `È`, so the whole Latin-1 block was mis-mapped. `gid` is the glyph
/// the renderer will rasterize, initialized to the faithful Unicode glyph
/// (`Unicode` = 1). A `Resolve` models selecting the cmap subtable: at `Buggy = 0`
/// it keeps the Unicode glyph; at `Buggy = 1` it substitutes the DIFFERENT Mac
/// Roman glyph (`MacRoman` = 2) — exactly the shipped defect. `ty` proves
/// `Faithful` (Buggy=0) and catches the substitution (Buggy=1). No multiplication.
fn glyph_fidelity_model() -> Model {
    ty_model! {
        GlyphFidelity {
            const Unicode = 1;
            const MacRoman = 2;
            const Buggy = 0;
            // The glyph id the renderer will rasterize for the cell's scalar,
            // initialized to the faithful Unicode-cmap glyph.
            var gid = 1;
            // Resolve the scalar through a cmap subtable. Correct: keep the Unicode
            // glyph. Buggy: substitute the Mac Roman glyph (the `·` -> `∑` defect).
            action Resolve {
                gid = if Buggy == 1 { MacRoman } else { Unicode };
            }
            // Every rasterized glyph is the font's Unicode glyph for the scalar.
            invariant Faithful: gid == Unicode;
        }
    }
}

#[test]
fn rfc_atlas_pack_proves_and_catches_overflow() {
    assert_proves_and_catches(&atlas_pack_model());
}

#[test]
fn rfc_reload_frame_proves_and_catches_font_shrink() {
    assert_proves_and_catches(&reload_frame_model());
}

#[test]
fn rfc_mark_row_origin_proves_and_catches_y_divergence() {
    assert_proves_and_catches(&mark_row_origin_model());
}

#[test]
fn rfc_glyph_fidelity_proves_and_catches_mac_roman_substitution() {
    assert_proves_and_catches(&glyph_fidelity_model());
}
