// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Ligature slicing: the shaping gate [`aterm_render::ligature_shaping::classify_shape`]
//! admits exactly the two grid-mappable forms (1:1, or N:1 with `N>=2` behind the flag)
//! and, with the flag off, is `accept iff n_out == n_in`; the `LigatureGate` ty model's own
//! `Classify` rule judges every real verdict. The tile arithmetic the N:1 path rests on is
//! the lattice `extract_cell_slice_reassembles_wide_raster` in `ligature_shaping.rs` (ty
//! has no multiplication).

use aterm_render::ligature_shaping::{ShapeVerdict, classify_shape};

// The conservative gate.

/// Over every `(n_in, n_out)` in `0..=8` under both flag settings `classify_shape` matches
/// its specification, admits only the two grid-mappable forms, and with the flag off equals
/// `accept iff n_out == n_in`. Every verdict is reached.
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

                // Flag off: identical to the plain count-equality accept.
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
                // A Collapsed verdict is always a flagged N:1 (N>=2).
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

    // Negative control: a 3:1 collapse is rejected without the flag (the shipping default)
    // and admitted with it.
    assert_eq!(classify_shape(3, 1, false), ShapeVerdict::Reject);
    assert_eq!(classify_shape(3, 1, true), ShapeVerdict::Collapsed);
}

/// One shape decision as `LigatureGate`'s variables: `accept` is the real verdict's
/// admission (`Reject` is the only refusal); `phase` is the model's Pick/Classify phase.
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

/// Tier-1 bind of `aterm_spec::derive::ligature_gate_model()` to the shipping
/// `classify_shape`: for every `(n_in, n_out, admit)` in `0..=8 × 0..=8 × {off, on}` the
/// model's `Classify` step, replayed with the real verdict as `accept'`, is one
/// `LigatureGate.Next` admits at `Buggy = 0`, and `ConservativeAccept` holds on every real
/// post-state. `Pick` is checked over its `0..=4` range.
///
/// Negative control: the real verdict with the flag check dropped (the model's defect)
/// yields a step the committed model rejects and the `Buggy = 1` model admits.
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

    // Negative control: a 3:1 collapse with the flag off, which a flag-blind gate accepts.
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

// The slice child-key encoding is disjoint from the plain wide-glyph key.

/// A plain `mono_gid` key has zero slice bits, a `mono_gid_slice` key encodes `slice + 1`
/// there over the same gid, so the rasterizer's `ch_or_id >> LIG_SLICE_SHIFT` branch (0 =
/// whole glyph, else slice) is unambiguous and distinct (gid, slice) pairs are distinct keys.
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
            // Never the whole-glyph key: whole rasterizes the wide glyph, slice tiles it.
            assert_ne!(key.ch_or_id, whole.ch_or_id);
            assert!(
                seen.insert(key.ch_or_id),
                "distinct (gid={gid}, slice={slice}) must map to a distinct key"
            );
        }
    }
}
