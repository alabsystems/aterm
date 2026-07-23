// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for Settings' bounded semantic-page cursor.
//!
//! Every shipping reducer path calls `settings_page_scroll_transition`; this test
//! projects that genuine seam onto the derived Previous/Next/Absolute and signed
//! line-scroll actions, including limit shrink and stale-offset recovery.

use aterm_gui::{SettingsPageScrollCommand, settings_page_scroll_transition};
use aterm_spec::derive::{Model, settings_page_scroll_model};
use aterm_spec::interp::State;

const MODEL_MAX: usize = 3;

fn project_before(model: &Model, limit: usize, cursor: usize, target: usize) -> State {
    let mut state = model.init_state();
    state.insert("limit", i64::try_from(limit).expect("bounded limit"));
    state.insert("cursor", i64::try_from(cursor).expect("bounded cursor"));
    state.insert("target", i64::try_from(target).expect("bounded target"));
    state
}

fn assert_command_transition(
    model: &Model,
    limit: usize,
    cursor: usize,
    target: usize,
    command: SettingsPageScrollCommand,
    action: &str,
) {
    let before = project_before(model, limit, cursor, target);
    let mut after = before.clone();
    after.insert(
        "cursor",
        i64::try_from(settings_page_scroll_transition(cursor, limit, command))
            .expect("bounded cursor result"),
    );
    assert_eq!(
        model.successors(action, &before).as_slice(),
        std::slice::from_ref(&after),
        "shipping {command:?} diverged from {action} at cursor={cursor}, limit={limit}, target={target}",
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &after),
            "{} failed after shipping {command:?}: {after:?}",
            invariant.name,
        );
    }
}

#[test]
fn shipping_page_scroll_commands_conform_over_complete_bounded_lattice() {
    let model = settings_page_scroll_model();
    let mut cases = 0usize;

    for limit in 0..=MODEL_MAX {
        for cursor in 0..=limit {
            for (command, action) in [
                (SettingsPageScrollCommand::Previous, "PreviousPage"),
                (SettingsPageScrollCommand::Next, "NextPage"),
                (SettingsPageScrollCommand::Lines(-2), "ScrollBackward"),
                (SettingsPageScrollCommand::Lines(2), "ScrollForward"),
            ] {
                assert_command_transition(&model, limit, cursor, 0, command, action);
                cases += 1;
            }
            for target in 0..=MODEL_MAX + 1 {
                assert_command_transition(
                    &model,
                    limit,
                    cursor,
                    target,
                    SettingsPageScrollCommand::Absolute(target),
                    "Absolute",
                );
                cases += 1;
            }
        }
    }

    assert_eq!(cases, 90);
}

#[test]
fn shipping_absolute_clamp_conforms_to_modeled_limit_shrink() {
    let model = settings_page_scroll_model();
    let mut cases = 0usize;

    for old_limit in 1..=MODEL_MAX {
        let new_limit = old_limit - 1;
        for cursor in 0..=old_limit {
            let before = project_before(&model, old_limit, cursor, 0);
            let mut after = before.clone();
            after.insert("limit", i64::try_from(new_limit).expect("bounded limit"));
            after.insert(
                "cursor",
                i64::try_from(settings_page_scroll_transition(
                    cursor,
                    new_limit,
                    SettingsPageScrollCommand::Absolute(cursor),
                ))
                .expect("bounded cursor"),
            );
            assert_eq!(
                model.successors("ShrinkLimit", &before).as_slice(),
                std::slice::from_ref(&after),
                "shipping clamp diverged while shrinking {old_limit} -> {new_limit} at {cursor}",
            );
            cases += 1;
        }
    }

    assert_eq!(cases, 9);
}

fn independent_transition(
    current: usize,
    limit: usize,
    command: SettingsPageScrollCommand,
) -> usize {
    let clamped = current.min(limit);
    match command {
        SettingsPageScrollCommand::Previous => clamped.saturating_sub(1),
        SettingsPageScrollCommand::Next => {
            if clamped < limit {
                clamped + 1
            } else {
                limit
            }
        }
        SettingsPageScrollCommand::Absolute(target) => target.min(limit),
        SettingsPageScrollCommand::Lines(lines) if lines < 0 => {
            clamped.saturating_sub(lines.unsigned_abs() as usize)
        }
        SettingsPageScrollCommand::Lines(lines) => {
            clamped.saturating_add(lines as usize).min(limit)
        }
    }
}

#[test]
fn shipping_helper_clamps_stale_offsets_and_arbitrary_line_deltas() {
    let currents = [0, 1, 2, 3, 4, 5, usize::MAX];
    let targets = [0, 1, 2, 3, 4, 5, usize::MAX];
    let lines = [i32::MIN, -5, -2, -1, 0, 1, 2, 5, i32::MAX];
    let mut cases = 0usize;

    for limit in 0..=MODEL_MAX {
        for current in currents {
            for command in [
                SettingsPageScrollCommand::Previous,
                SettingsPageScrollCommand::Next,
            ] {
                let actual = settings_page_scroll_transition(current, limit, command);
                assert_eq!(actual, independent_transition(current, limit, command));
                assert!(actual <= limit);
                cases += 1;
            }
            for target in targets {
                let command = SettingsPageScrollCommand::Absolute(target);
                let actual = settings_page_scroll_transition(current, limit, command);
                assert_eq!(actual, independent_transition(current, limit, command));
                assert!(actual <= limit);
                cases += 1;
            }
            for delta in lines {
                let command = SettingsPageScrollCommand::Lines(delta);
                let actual = settings_page_scroll_transition(current, limit, command);
                assert_eq!(actual, independent_transition(current, limit, command));
                assert!(actual <= limit);
                cases += 1;
            }
        }
    }

    assert_eq!(cases, 504);
}

#[test]
fn unclamped_next_negative_control_is_rejected() {
    let model = settings_page_scroll_model();
    let mut rejected = 0usize;

    for limit in 0..=MODEL_MAX {
        let before = project_before(&model, limit, limit, 0);
        let mut forged = before.clone();
        forged.insert(
            "cursor",
            i64::try_from(limit + 1).expect("bounded mutant cursor"),
        );
        assert_ne!(
            model.successors("NextPage", &before).as_slice(),
            std::slice::from_ref(&forged),
        );
        assert!(!model.check_invariant("CursorBounded", &forged));
        rejected += 1;
    }

    assert_eq!(rejected, 4);
}
