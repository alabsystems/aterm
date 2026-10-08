// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for M4 (ligature slicing): the shaping GATE stays
//! conservative when the Cascadia N:1 branch is added. The tile arithmetic the
//! N:1 path rests on is the L0 lattice `extract_cell_slice_reassembles_wide_raster`
//! (in `ligature_shaping.rs`; ty has no multiplication, the M4 rounding/scaling
//! waiver).
//!
//! These bind the SHIPPING pure function
//! [`aterm_render::ligature_shaping::classify_shape`] to M4 PROVE bullet 3:
//!
//! 3. **Conservative gate** — `classify_shape` admits EXACTLY the two
//!    grid-mappable forms (1:1, or N:1 with `N>=2` behind the flag) and rejects
//!    everything else; with the flag off it is byte-identical to the legacy
//!    `accept iff n_out == n_in`. This is the SAME invariant the `gate_*` kani
//!    proofs carry, and `classify_shape_conforms_to_ligature_gate_model` is the
//!    Tier-1 bind to the `LigatureGate` derived ty model: the model's own
//!    `Classify` rule judges every real verdict.

use aterm_render::ligature_shaping::{ShapeVerdict, classify_shape};

// ---------------------------------------------------------------------------
// PROVE (3): conservative gate — Tier-1 binding of the shipping classifier.
// ---------------------------------------------------------------------------

/// The complete small-count lattice: `classify_shape` matches its specification for
/// every `(n_in, n_out)` in `0..=8` under BOTH flag settings, admits ONLY the two
/// grid-mappable forms, and — with the flag off — is byte-identical to the legacy
/// `accept iff n_out == n_in`. Non-vacuity: each verdict is reached; a negative
/// control shows the flag is what gates the collapse.
#[test]
fn classify_shape_lattice() {
    let (mut saw_one, mut saw_collapse, mut saw_reject) = (false, false, false);
    for n_in in 0..=8usize {
        for n_out in 0..=8usize {
            for &admit in &[false, true] {
                let v = classify_shape(n_in, n_out, admit);
                let one_to_one = n_in >= 1 && n_out == n_in;
                let collapsed = admit && n_out == 1 && n_in >= 2;
                // Spec: the two forms are mutually exclusive, 1:1 wins ties.
                let expect = if one_to_one {
                    ShapeVerdict::OneToOne
                } else if collapsed {
                    ShapeVerdict::Collapsed
                } else {
                    ShapeVerdict::Reject
                };
                assert_eq!(
                    v, expect,
                    "classify_shape({n_in}, {n_out}, {admit}) verdict"
                );

                // Flag OFF => byte-identical to the legacy count-equality accept.
                if !admit {
                    let legacy_accept = n_in >= 1 && n_out == n_in;
                    assert_eq!(
                        matches!(v, ShapeVerdict::OneToOne),
                        legacy_accept,
                        "admit-off gate must equal the legacy accept"
                    );
                    assert!(
                        !matches!(v, ShapeVerdict::Collapsed),
                        "no collapse admitted without the flag"
                    );
                }
                // A Collapsed verdict is ALWAYS a genuine N:1 (N>=2) with the flag.
                if matches!(v, ShapeVerdict::Collapsed) {
                    assert!(admit && n_out == 1 && n_in >= 2, "collapsed => flagged N:1");
                }
                match v {
                    ShapeVerdict::OneToOne => saw_one = true,
                    ShapeVerdict::Collapsed => saw_collapse = true,
                    ShapeVerdict::Reject => saw_reject = true,
                }
            }
        }
    }
    assert!(
        saw_one && saw_collapse && saw_reject,
        "non-vacuity: every verdict reached"
    );

    // NEGATIVE CONTROL — the exact case the flag guards: a 3:1 collapse is REJECTED
    // without the flag (the shipping default, byte-identical to pre-M4) and only
    // becomes admissible when the flag is set.
    assert_eq!(classify_shape(3, 1, false), ShapeVerdict::Reject);
    assert_eq!(classify_shape(3, 1, true), ShapeVerdict::Collapsed);
}

/// Project one shape decision onto `LigatureGate`'s variables. `accept` is the
/// REAL verdict's admission (`Reject` is the only refusal); `phase` is the model's
/// Pick/Classify alternation.
fn gate_state(phase: i64, n_in: usize, n_out: usize, admit: bool, accept: bool) -> GateState {
    [
        ("phase", phase),
        ("n_in", i64::try_from(n_in).expect("small lattice count")),
        ("n_out", i64::try_from(n_out).expect("small lattice count")),
        ("admit", i64::from(admit)),
        ("accept", i64::from(accept)),
    ]
    .into_iter()
    .collect()
}

type GateState = std::collections::BTreeMap<&'static str, i64>;

/// Tier-1 bind of `aterm_spec::derive::ligature_gate_model()` to the SHIPPING
/// `classify_shape`. For every `(n_in, n_out, admit)` in `0..=8 × 0..=8 × {off,
/// on}`, the model's `Classify` step is replayed with `accept'` taken from the real
/// verdict, and the step must be one `LigatureGate.Next` admits at the committed
/// `Buggy = 0` — so the model's OWN accept rule, not a restatement of it, judges
/// the shipping gate. The `Pick` half is checked over the model's `0..=4` pick
/// range (every real shape there is a state the model can reach), and
/// `ConservativeAccept` is evaluated on every real post-state.
///
/// NEGATIVE CONTROL: the model's defect is "admit a collapse without the flag".
/// A classifier with exactly that defect — the real verdict with the flag check
/// dropped — must produce a step the committed model REJECTS and the `Buggy = 1`
/// model ADMITS, so a pass here is never vacuous: this bind distinguishes the
/// shipping gate from the one the model was written to catch.
#[test]
fn classify_shape_conforms_to_ligature_gate_model() {
    use aterm_spec::derive::ligature_gate_model;
    use aterm_spec::{interp, verify};

    let m = ligature_gate_model();
    let init = m.init_state();
    let mut classified = 0usize;
    for n_in in 0..=8usize {
        for n_out in 0..=8usize {
            for admit in [false, true] {
                let picked = gate_state(1, n_in, n_out, admit, false);
                if n_in <= 4 && n_out <= 4 {
                    assert!(
                        m.successors("Pick", &init).contains(&picked),
                        "Pick must reach the real shape ({n_in}, {n_out}, {admit})"
                    );
                }
                let real = classify_shape(n_in, n_out, admit) != ShapeVerdict::Reject;
                let decided = gate_state(0, n_in, n_out, admit, real);
                let (ok, why) = verify::validate_transition_tiered(
                    &m,
                    &[],
                    &picked,
                    &decided,
                    Some("Classify"),
                    "ligature gate conformance",
                );
                assert!(
                    ok,
                    "classify_shape({n_in}, {n_out}, {admit}) accept={real} is not the \
                     model's Classify step\n{why}"
                );
                assert!(
                    m.check_invariant("ConservativeAccept", &decided),
                    "real verdict for ({n_in}, {n_out}, {admit}) breaks ConservativeAccept"
                );
                classified += 1;
            }
        }
    }
    assert_eq!(classified, 9 * 9 * 2, "the whole lattice was classified");

    // NEGATIVE CONTROL — the modelled defect on a real shape: a 3:1 collapse with
    // the flag OFF. The shipping gate refuses it; a flag-blind gate would accept.
    assert_eq!(classify_shape(3, 1, false), ShapeVerdict::Reject);
    let picked = gate_state(1, 3, 1, false, false);
    let flag_blind = gate_state(0, 3, 1, false, true);
    let (committed_ok, _) = verify::validate_transition_tiered(
        &m,
        &[],
        &picked,
        &flag_blind,
        Some("Classify"),
        "ligature gate negative control",
    );
    assert!(
        !committed_ok,
        "the committed model must REJECT an unflagged collapse being accepted"
    );
    let buggy = interp::with_buggy(&m, 1);
    assert!(
        buggy.successors("Classify", &picked).contains(&flag_blind),
        "the Buggy = 1 model must ADMIT the flag-blind accept — the defect this bind catches"
    );
    assert!(
        !m.check_invariant("ConservativeAccept", &flag_blind),
        "the flag-blind accept is exactly what ConservativeAccept forbids"
    );
}

// ---------------------------------------------------------------------------
// PROVE (4): the slice CHILD-KEY encoding is a strict superset of the plain
// wide-glyph key — a whole-glyph `mono_gid` key is slice-free, and a slice key
// round-trips (gid, slice) distinctly — so the rasterizer decodes the two apart.
// ---------------------------------------------------------------------------

/// A plain `mono_gid` key has ZERO in the slice bits (a whole-glyph key), while a
/// `mono_gid_slice` key encodes `slice + 1` there over the SAME gid — so the two
/// never collide and the rasterizer's `ch_or_id >> LIG_SLICE_SHIFT` branch (0 =
/// whole glyph, else slice) is unambiguous. Distinct (gid, slice) pairs map to
/// distinct keys.
#[test]
fn slice_key_encoding_round_trips_and_is_disjoint_from_whole_glyph() {
    use aterm_render::{GlyphKey, LIG_SLICE_SHIFT, StyleBits};
    use std::collections::HashSet;
    let px_q = GlyphKey::quantize_px(18.0);
    let mut seen = HashSet::new();
    for gid in [0u16, 1, 42, 700, u16::MAX] {
        // The whole-glyph key is slice-free: high bits 0, low 16 bits == gid.
        let whole = GlyphKey::mono_gid(gid, StyleBits::REGULAR, px_q);
        assert_eq!(
            whole.ch_or_id >> LIG_SLICE_SHIFT,
            0,
            "a whole-glyph mono_gid key must have empty slice bits"
        );
        assert_eq!(whole.ch_or_id as u16, gid, "low bits carry the gid");
        assert!(seen.insert(whole.ch_or_id), "whole-glyph keys are distinct");
        for slice in 0u16..=8 {
            let key = GlyphKey::mono_gid_slice(gid, slice, StyleBits::REGULAR, px_q);
            // Slice bits decode back to `slice`; low bits still carry the gid.
            assert_eq!((key.ch_or_id >> LIG_SLICE_SHIFT) - 1, slice as u32);
            assert_eq!(key.ch_or_id as u16, gid);
            // Never equal to the whole-glyph key (the raster path treats them
            // differently: whole = rasterize the wide glyph, slice = tile it).
            assert_ne!(key.ch_or_id, whole.ch_or_id);
            assert!(
                seen.insert(key.ch_or_id),
                "distinct (gid={gid}, slice={slice}) must map to a distinct key"
            );
        }
    }
}
