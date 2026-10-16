// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! RFC worked examples: a defect found in the real renderer or GUI, written as a `ty_model!`
//! bounded state machine, is proven at `Buggy = 0` and caught at `Buggy = 1`, so the model is a
//! non-vacuous catch of the real bug. Four shipped fixes: the atlas height cap
//! (`aterm-gpu/src/renderer.rs::build_kind`), the HiDPI font reload
//! (`aterm-gui/src/app_config.rs::reload_config`), the combining-mark row origin
//! (`aterm-gpu/src/renderer.rs`) and cmap fidelity. Tiered: the interpreter always discharges
//! every config, and Trust `ty` re-checks them where installed (the two must agree).

use aterm_spec::derive::Model;
use aterm_spec::ty_model;
use aterm_spec::verify;

/// Proven at `Buggy = 0`, caught at `Buggy = 1`. `ty_model!` emits no function-valued vars, so
/// the `_scalar` form fits here, where `derived_ring_ty.rs` needs the `_tiered` one.
fn assert_proves_and_catches(m: &Model) {
    verify::prove_and_catch_scalar(m, m.name);
}

/// The glyph-atlas height accumulator (`build_kind` grows `occupied_height()` and rolls back the
/// glyph that would pass the device's `cap_h`). `Grow` fires only while a step fits under `Cap`;
/// `Buggy = 1` triples the step, so even from the empty atlas one step overshoots to `6 > Cap`.
fn atlas_pack_model() -> Model {
    ty_model! {
        AtlasPack {
            const Cap = 4;
            const Step = 2;
            const Buggy = 0;
            var h = 0;
            action Grow when (h <= Cap - Step) {
                h = h + if Buggy == 1 { Step + Step + Step } else { Step };
            }
            invariant Bounded: h <= Cap;
        }
    }
}

/// `reload_config` on a HiDPI display: the auto default font is `round(FONT_PX · scale)`, and
/// reloading after an unrelated config edit must re-derive that scaled default, never shrink to
/// the unscaled `FONT_PX`. `Target` and `Base` are constants, so no multiplication; `Buggy = 1`
/// reloads to `Base`.
fn reload_frame_model() -> Model {
    ty_model! {
        ReloadFrame {
            const Target = 32;
            const Base = 16;
            const Buggy = 0;
            var font = 32;
            action Reload {
                font = if Buggy == 1 { Base } else { Target };
            }
            invariant FontIsScaledDefault: font == Target;
        }
    }
}

/// Combining-mark row origin: a mark's origin is `pad + r·ch`, the same as its base glyph's.
/// With the `r = 0` witness both are `Pad`; `Buggy = 1` drops the pad from the mark, which then
/// renders `pad` px too high (a CPU/GPU divergence for NFD sequences).
fn mark_row_origin_model() -> Model {
    ty_model! {
        MarkRowOrigin {
            const Pad = 1;
            const Buggy = 0;
            var base = 0;
            var mark = 0;
            action Step {
                base = Pad;
                mark = if Buggy == 1 { 0 } else { Pad };
            }
            invariant Aligned: base == mark;
        }
    }
}

/// Cmap fidelity: the renderer rasterizes the font's Unicode-cmap glyph for a cell's scalar,
/// never the legacy `(1,0)` Mac Roman subtable's (which fontdue prefers on Apple `.ttc` faces,
/// mapping `·` to `∑` and `é` to `È`). `Buggy = 1` substitutes the Mac Roman glyph.
fn glyph_fidelity_model() -> Model {
    ty_model! {
        GlyphFidelity {
            const Unicode = 1;
            const MacRoman = 2;
            const Buggy = 0;
            var gid = 1;
            action Resolve {
                gid = if Buggy == 1 { MacRoman } else { Unicode };
            }
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
