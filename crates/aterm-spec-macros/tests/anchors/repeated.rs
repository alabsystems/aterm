// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::refines;

struct Replica;
impl Replica {
    #[refines(machine = "Left", action = "Restart", project = "test::project")]
    #[refines(machine = "Right", action = "Restart", project = "test::project")]
    #[refines(machine = "Scope-A", action = "Same", project = "test::project")]
    #[refines(machine = "Scope_A", action = "Same", project = "test::project")]
    fn restart(&self) -> u8 {
        7
    }
}

#[test]
fn stacked_model_anchors_keep_distinct_registry_and_execution_identities() {
    use aterm_spec::xref::{entered_anchor_ids, refinements, reset_entered_anchors};
    assert!(reset_entered_anchors());
    assert_eq!(Replica.restart(), 7);
    for machine in ["Left", "Right", "Scope-A", "Scope_A"] {
        let anchors: Vec<_> = refinements()
            .filter(|anchor| anchor.machine == machine)
            .collect();
        assert_eq!(anchors.len(), 1);
        assert!(entered_anchor_ids().contains(anchors[0].entry_id));
    }
}
