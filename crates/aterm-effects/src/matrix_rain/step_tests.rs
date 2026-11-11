// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::RainLatches;

#[test]
fn the_execute_edge_is_a_same_session_rising_edge() {
    let mut l = RainLatches::default();
    assert!(!l.note_shell_executing(1, true), "first sight baselines");
    assert!(!l.note_shell_executing(1, true), "held is not an edge");
    assert!(!l.note_shell_executing(1, false));
    assert!(l.note_shell_executing(1, true), "the rising edge fires");
    assert!(
        !l.note_shell_executing(2, true),
        "a session switch baselines"
    );
}

#[test]
fn a_completion_is_news_once_in_its_own_session() {
    let mut l = RainLatches::default();
    assert_eq!(l.take_new_completion(1, None), None, "watching, none seen");
    assert_eq!(l.take_new_completion(1, Some((1, 2))), Some(true));
    assert_eq!(l.take_new_completion(1, Some((1, 2))), None, "once");
    assert_eq!(
        l.take_new_completion(2, Some((9, 0))),
        None,
        "a switch baselines"
    );
    l.baseline_completion(2, Some(10));
    assert_eq!(
        l.take_new_completion(2, Some((10, 0))),
        None,
        "a suspension-era completion was absorbed"
    );
    assert_eq!(l.take_new_completion(2, Some((11, 0))), Some(false));
}

#[test]
fn a_surface_without_an_engine_keeps_both_latches_observing() {
    let mut l = RainLatches::default();
    l.observe_without_engine(1, false, None);
    l.observe_without_engine(1, true, Some(4));
    assert!(
        !l.note_shell_executing(1, true),
        "the running command is not news"
    );
    assert_eq!(l.take_new_completion(1, Some((4, 1))), None, "nor its end");
    assert_eq!(l.take_new_completion(1, Some((5, 1))), Some(true));

    let mut execute_only = RainLatches::default();
    execute_only.observe_without_engine(1, false, None);
    let _ = execute_only.note_shell_executing(1, true);
    assert_eq!(
        execute_only.take_new_completion(1, Some((4, 1))),
        Some(true),
        "control: watching Execute alone replays the off-era completion"
    );
}
