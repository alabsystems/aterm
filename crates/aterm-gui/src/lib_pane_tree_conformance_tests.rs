// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_models_gui::pane_tree_model;
use std::collections::BTreeMap;

fn project(tree: &pane::PaneTree) -> [i64; 2] {
    let leaf_count = tree.len() as i64;
    let sessions = tree.sessions();
    let focused = sessions
        .iter()
        .position(|&s| s == tree.focus())
        .expect("focused session is always a live leaf") as i64;
    [leaf_count, focused]
}

fn validate_transition(action_name: &str, prev: [i64; 2], next: [i64; 2]) -> (bool, String) {
    let m = pane_tree_model();
    let st = |s: [i64; 2]| -> BTreeMap<&'static str, i64> {
        [("leaf_count", s[0]), ("focused", s[1])]
            .into_iter()
            .collect()
    };
    aterm_spec::verify::validate_transition_tiered(
        &m,
        &[("Cap", 1_000_000)],
        &st(prev),
        &st(next),
        Some(action_name),
        "PaneTree split/close conformance",
    )
}

#[test]
fn the_real_pane_tree_conforms() {
    crate::conformance_support::on_conformance_stack("pane_tree", run_conformance);
}

fn run_conformance() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-panetree-conf-{}-{}",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    let t0 = pane::PaneTree::new(0);
    let init = pane_tree_model().init_state();
    let model_init = [init["leaf_count"], init["focused"]];
    assert_eq!(
        project(&t0),
        model_init,
        "fresh PaneTree projection must equal pane_tree_model().init_state() — got {:?}, model {:?}",
        project(&t0),
        model_init
    );
    assert_eq!(model_init, [1, 0], "sanity: model init is one focused leaf");

    let mut t = pane::PaneTree::new(0);

    let prev = project(&t);
    assert!(t.split_focused(pane::SplitDir::Vertical, 1));
    let next = project(&t);
    let (ok, out) = validate_transition("Split", prev, next);
    assert!(
        ok,
        "real Split {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );

    let prev = next;
    assert!(t.split_focused(pane::SplitDir::Horizontal, 2));
    let next = project(&t);
    let (ok, out) = validate_transition("Split", prev, next);
    assert!(
        ok,
        "real Split {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );

    assert!(t.set_focus(0));
    let prev = project(&t);
    assert!(matches!(
        t.close_pane(0),
        pane::CloseOutcome::Collapsed { .. }
    ));
    let next = project(&t);
    let (ok, out) = validate_transition("Close", prev, next);
    assert!(
        ok,
        "real Close (collapse) {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );

    let prev = project(&t);
    assert!(matches!(
        t.close_focused(),
        pane::CloseOutcome::Collapsed { .. }
    ));
    let next = project(&t);
    let (ok, out) = validate_transition("Close", prev, next);
    assert!(
        ok,
        "real Close (focused last) {prev:?} -> {next:?} must conform\n--- ty ---\n{out}"
    );

    let (bad_ok, _bad) = validate_transition("Close", [2, 1], [1, 1]);
    assert!(
        !bad_ok,
        "corrupted Close [2,1] -> [1,1] (dangling focus) MUST be ty-REJECTED — \
         the conformance would be vacuous otherwise"
    );

    let mut sole = pane::PaneTree::new(0);
    assert!(matches!(
        sole.close_pane(0),
        pane::CloseOutcome::LastPane { .. }
    ));
    assert_eq!(project(&sole), [1, 0], "LastPane must not shrink the tree");

    let (noop_ok, _noop) = validate_transition("Close", [2, 1], [2, 1]);
    assert!(
        !noop_ok,
        "a Collapsed Close that keeps its leaf [2,1] -> [2,1] MUST be ty-REJECTED"
    );

    let _ = std::fs::remove_dir_all(&dir);
    crate::logging::stderr_line!(
        "pane_tree Tier-1 conformance: real split/close transitions conform AND the \
         dangling-focus and kept-leaf negative controls were rejected."
    );
}
