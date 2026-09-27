// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0: the chord and type-ahead cannot steal the new line's walk.

use aterm_spec::{derive::rainbow_composer_newline_gate_model, verify};

#[test]
fn composer_gate_waits_for_home_and_catches_the_early_spend() {
    let model = rainbow_composer_newline_gate_model();
    assert!(
        aterm_spec::xref::model_registry()
            .iter()
            .any(|candidate| candidate.name == model.name)
    );
    verify::prove_and_catch_scalar(&model, model.name);

    let start = model.init_state();
    let plain = model.successors("PlainModifiedKey", &start)[0].clone();
    assert!(!model.action_enabled("ObserveHome", &plain));
    assert!(!model.action_enabled("FirstEcho", &plain));

    let pending = model.successors("ComposerKey", &start)[0].clone();
    let cancelled = model.successors("CancelBeforeHome", &pending)[0].clone();
    assert!(!model.action_enabled("ObserveHome", &cancelled));
    assert!(!model.action_enabled("FirstEcho", &cancelled));

    let chord = model.successors("ComposerKey", &start)[0].clone();
    let stamped = model.successors("ChordStamp", &chord)[0].clone();
    let raced = model.successors("TypeAheadBeforeHome", &stamped)[0].clone();
    assert_eq!(raced.get("spent"), Some(&0));
    assert!(!model.action_enabled("FirstEcho", &raced));
    let homed = model.successors("ObserveHome", &raced)[0].clone();
    let echoed = model.successors("FirstEcho", &homed)[0].clone();
    assert_eq!(echoed.get("spent"), Some(&1));

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let chord = buggy.successors("ComposerKey", &buggy.init_state())[0].clone();
    let stolen = buggy.successors("ChordStamp", &chord)[0].clone();
    assert!(!buggy.check_invariant("NoGateTheftBeforeHome", &stolen));
    let plain = buggy.successors("PlainModifiedKey", &buggy.init_state())[0].clone();
    assert!(!buggy.check_invariant("PlainKeyCannotArmComposerGate", &plain));
    let pending = buggy.successors("ComposerKey", &buggy.init_state())[0].clone();
    let stranded = buggy.successors("CancelBeforeHome", &pending)[0].clone();
    assert!(!buggy.check_invariant("CancelledGateCannotStrandTyping", &stranded));
}
