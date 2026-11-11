// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// A synthetic anchor. `entry_id` is spelled exactly as the `#[refines]` macro
/// spells it, because `fn_of` splits on that shape.
fn synth(action: &'static str, method: &'static str, entry_id: &'static str) -> RefinementAnchor {
    RefinementAnchor {
        machine: "Ring",
        action,
        rust_method: method,
        location: "xref.rs:0:0",
        project: "",
        entry_id,
    }
}

fn window(action: &'static str, entered: &[&'static str]) -> StepRecord {
    StepRecord {
        action,
        entered: entered.iter().copied().collect(),
    }
}

fn audit_of(anchors: &[RefinementAnchor], steps: &[StepRecord]) -> StepAudit {
    let linked: Vec<&'static RefinementAnchor> =
        anchors.iter().map(|a| &*Box::leak(Box::new(*a))).collect();
    audit_steps_of("Ring", &linked, steps)
}

/// The within-window move `stray` catches: an anchor moved onto a function whose entry
/// pattern is wider than the action it now claims.
#[test]
fn a_move_inside_one_window_is_caught_when_the_new_site_is_wider() {
    let moved = vec![
        synth("Push", "chatty", "Ring::Push @ chatty"),
        synth("Pop", "batch", "Ring::Pop @ batch"),
        synth("Evict", "evict", "Ring::Evict @ evict"),
    ];
    let steps = vec![
        window("Push", &["Ring::Push @ chatty", "Ring::Pop @ batch"]),
        window("Pop", &["Ring::Push @ chatty", "Ring::Pop @ batch"]),
        window("Evict", &["Ring::Evict @ evict"]),
    ];
    let caught = audit_of(&moved, &steps);
    assert!(
        caught.unwitnessed.is_empty() && caught.blind_steps.is_empty(),
        "entry-in-its-own-window is satisfied — that check alone would pass this"
    );
    assert_eq!(
        caught.stray.get("chatty"),
        Some(&["Pop"].into_iter().collect::<BTreeSet<_>>()),
        "…and the contrapositive is what fails it: {:?}",
        caught.stray
    );
    assert!(!caught.is_complete());
}

#[test]
fn a_window_that_misses_its_own_action_is_named_even_when_a_sibling_window_saw_it() {
    let anchors = vec![synth("Evict", "evict", "Ring::Evict @ evict")];
    let steps = vec![
        window("Evict", &["Ring::Evict @ evict"]),
        window("Evict", &["Other::X @ chatty"]),
    ];
    let audit = audit_of(&anchors, &steps);
    assert!(
        audit.unwitnessed.is_empty(),
        "the OR across an action's windows still passes it"
    );
    assert_eq!(
        audit.blind_steps,
        vec!["Evict"],
        "…and the AND names the other one"
    );
    assert_eq!(audit.witnessed_steps, 1);
    assert!(!audit.is_complete(), "which the gate must fail on");
}

#[test]
fn a_second_anchor_site_that_never_runs_is_named_rather_than_carried() {
    let anchors = vec![
        synth("Evict", "evict", "Ring::Evict @ evict"),
        synth("Evict", "batch", "Ring::Evict @ batch"),
    ];
    let steps = vec![window("Evict", &["Ring::Evict @ evict"])];
    let audit = audit_of(&anchors, &steps);
    assert!(
        audit.unwitnessed.is_empty(),
        "`witnessed` ORs across SITES, so one live site discharges the action"
    );
    assert_eq!(
        audit.dark_sites,
        vec!["Ring::Evict @ batch"],
        "…and the site that never ran is reported instead of vanishing"
    );
    // Overlapping `bound` and `waived` entries never double-count toward `ratio()`.
    let overlap = MachineCoverage {
        machine: "Ring".into(),
        total_actions: 2,
        bound: BTreeSet::from(["A".to_string()]),
        waived: BTreeSet::from(["A".to_string()]),
        uncovered: BTreeSet::from(["B".to_string()]),
        active: true,
    };
    assert_eq!(overlap.ratio(), 0.5);
}

/// A worker's window reaches its owner only by take and merge: nothing crosses threads
/// otherwise, a taken window is closed and empty, and a closed window neither hands its record
/// over nor takes a merge.
#[test]
fn a_workers_window_reaches_its_owner_only_by_take_and_merge() {
    assert!(reset_entered_anchors(), "the owner's window opens");
    let taken = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                assert!(reset_entered_anchors(), "an earlier window opens");
                note_entered("Ring::Pop @ an earlier window");
                disarm_entered_anchors();
                assert_eq!(
                    take_entered_anchors(),
                    None,
                    "a closed window hands nothing over"
                );
                assert_eq!(entered_anchor_count(), Some(1), "…and keeps its record");
                assert!(reset_entered_anchors(), "the worker's window opens");
                note_entered("Ring::Push @ worker");
                let taken = take_entered_anchors();
                assert_eq!(window_is_armed(), Some(false), "taking closes it");
                assert_eq!(entered_anchor_count(), Some(0), "…and empties it");
                note_entered("Ring::Pop @ worker after the take");
                assert_eq!(entered_anchor_count(), Some(0), "closed: nothing recorded");
                taken
            })
            .join()
            .unwrap()
    })
    .expect("the worker's record is taken");
    assert_eq!(taken, BTreeSet::from(["Ring::Push @ worker"]));
    assert_eq!(
        entered_anchor_count(),
        Some(0),
        "the worker's entry never reached the owner by itself"
    );
    note_entered("Ring::Push @ owner");
    assert!(
        merge_entered_anchors(taken),
        "the owner's open window takes it"
    );
    assert_eq!(
        entered_anchor_ids(),
        BTreeSet::from(["Ring::Push @ owner", "Ring::Push @ worker"])
    );
    disarm_entered_anchors();
    assert!(
        !merge_entered_anchors(BTreeSet::from(["Ring::Pop @ late"])),
        "a closed window takes no merge"
    );
    assert_eq!(entered_anchor_count(), Some(2), "and records nothing");
}
