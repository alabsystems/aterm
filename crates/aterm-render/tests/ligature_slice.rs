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

/// Tier-1 bind of `aterm_models_render::ligature_gate_model()` to the shipping
/// `classify_shape`: for every `(n_in, n_out, admit)` in `0..=8 × 0..=8 × {off, on}` the
/// model's `Classify` step, replayed with the real verdict as `accept'`, is one
/// `LigatureGate.Next` admits at `Buggy = 0`, and `ConservativeAccept` holds on every real
/// post-state. `Pick` is checked over its `0..=4` range.
///
/// Negative control: the real verdict with the flag check dropped (the model's defect)
/// yields a step the committed model rejects and the `Buggy = 1` model admits.
#[test]
fn classify_shape_conforms_to_ligature_gate_model() {
    use aterm_models_render::ligature_gate_model;
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
