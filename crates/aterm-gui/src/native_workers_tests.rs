// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::task_record::FROM_CTL;
use std::time::Instant;

fn record(state: TaskState, why: Option<&'static str>, exit: Option<i32>) -> TaskRecord {
    let now = Instant::now();
    let at = 1_700_000_000_000;
    TaskRecord {
        rev: 1,
        state,
        from: FROM_CTL.to_owned(),
        manager_node: None,
        told: Default::default(),
        why,
        checked: None,
        left: None,
        exit,
        finished: false,
        unfinished_exit: false,
        result_bytes: 0,
        result_path: None,
        attention: false,
        asks: Default::default(),
        since_unix_ms: at,
        since: now,
        started_unix_ms: at,
        started: now,
        entered: 1,
        agent: None,
        program: None,
        trusted_cwd: None,
        fact: None,
        program_block: None,
        fact_rev: 0,
        carried_entered: None,
    }
}

fn row(session: u64, group: &str, state: RowState) -> WorkerRow {
    WorkerRow {
        session,
        group: group.to_owned(),
        under: if group == ATERM_GROUP {
            RowGroup::Aterm
        } else {
            RowGroup::Manager(format!("s-{group}"))
        },
        state,
        title: format!("w{session}"),
        agent: Some("claude".to_owned()),
        elapsed: "12m".to_owned(),
        last_words: Some("reading the tree".to_owned()),
        why: None,
        has_result: false,
        stoppable: !state.concluded(),
        finished: false,
    }
}

#[test]
fn the_row_state_is_one_function_of_the_task_state() {
    let of = |state, why, exit| row_state(Some(&record(state, why, exit)), false);
    assert_eq!(of(TaskState::Working, None, None), RowState::Working);
    assert_eq!(of(TaskState::Stalled, None, None), RowState::Stalled);
    assert_eq!(
        of(TaskState::Done, Some("landed"), None),
        RowState::Finished
    );
    assert_eq!(
        of(TaskState::Failed, Some("could-not-finish"), None),
        RowState::Failed
    );
    for (why, exit) in [
        (None, Some(0)),
        (None, Some(1)),
        (None, None),
        (Some("exit"), Some(0)),
        (Some("exit"), Some(101)),
        (Some("exit"), None),
        (Some("shell"), Some(0)),
    ] {
        assert_eq!(
            of(TaskState::Exited, why, exit),
            RowState::Failed,
            "an exit before its task finished, whatever its code: {why:?} {exit:?}"
        );
    }
    assert_eq!(row_state(None, false), RowState::Idle);
    assert_eq!(row_state(None, true), RowState::NeedsYou);
    let need = |state| row_state(Some(&record(state, None, None)), true);
    assert_eq!(need(TaskState::Working), RowState::NeedsYou);
    assert_eq!(need(TaskState::Stalled), RowState::NeedsYou);
    assert_eq!(need(TaskState::Done), RowState::Finished, "over is over");
}

/// What `mark_task_exited` and `mark_ended` write: `exited why=exit`, a landing's result
/// carried over, a code only where the tab's blocks showed one.
fn exit_after_landing(exit: Option<i32>) -> TaskRecord {
    let mut landed = record(TaskState::Exited, Some("exit"), exit);
    landed.finished = true;
    landed.result_bytes = 2_048;
    landed.result_path = Some("/state/tasks/s.md".into());
    landed
}

#[test]
fn an_exit_after_a_landing_stays_finished_with_its_result_to_read() {
    for exit in [None, Some(0), Some(1)] {
        let exited = exit_after_landing(exit);
        assert_eq!(
            row_state(Some(&exited), false),
            RowState::Finished,
            "{exit:?}"
        );
        assert!(result_to_read(&exited), "{exit:?}");
    }
    assert_eq!(why_words(&exit_after_landing(None)), None);
    assert_eq!(why_words(&exit_after_landing(Some(0))), None);
    assert_eq!(
        why_words(&exit_after_landing(Some(1))).as_deref(),
        Some("exit 1")
    );
    assert!(!result_to_read(&record(
        TaskState::Exited,
        Some("exit"),
        Some(1)
    )));
    let mut running = exit_after_landing(None);
    running.state = TaskState::Working;
    assert!(!result_to_read(&running), "a reopened task's is stale");
}

#[test]
fn a_failed_exit_with_no_code_says_it_exited() {
    let gone = record(TaskState::Exited, Some("exit"), None);
    assert_eq!(
        why_words(&gone).as_deref(),
        Some("its agent exited before landing")
    );
    let said = record(TaskState::Failed, Some("could-not-finish"), None);
    assert_eq!(
        why_words(&said).as_deref(),
        Some("it said it could not finish")
    );
}

#[test]
fn a_conclusion_says_why_and_a_running_task_says_nothing() {
    let why = |state, why, exit| why_words(&record(state, why, exit));
    assert_eq!(why(TaskState::Working, None, None), None);
    assert_eq!(
        why(TaskState::Exited, None, Some(3)).as_deref(),
        Some("its agent exited before landing, exit 3")
    );
    assert_eq!(
        why(TaskState::Exited, Some("shell"), Some(1)).as_deref(),
        Some("never started, exit 1")
    );
    assert_eq!(
        why(TaskState::Stalled, Some("startup"), None).as_deref(),
        Some("never started")
    );
}

#[test]
fn the_summary_counts_only_the_states_with_a_row_in_its_order() {
    let rows = [
        row(1, "a", RowState::Finished),
        row(2, "a", RowState::Working),
        row(3, "a", RowState::Failed),
        row(4, "a", RowState::Working),
        row(5, "a", RowState::Finished),
        row(6, "a", RowState::Working),
    ];
    assert_eq!(summary_words(&rows), "3 working, 1 failed, 2 finished");
    assert_eq!(summary_words(&[]), "No workers are running.");
    assert_eq!(
        summary_words(&[row(1, "a", RowState::NeedsYou)]),
        "1 needs you"
    );
}

#[test]
fn running_rows_sit_above_concluded_ones_and_aterm_goes_last() {
    let rows = [
        row(1, ATERM_GROUP, RowState::Working),
        row(2, "lead", RowState::Finished),
        row(3, "lead", RowState::Working),
        row(4, "lead", RowState::Failed),
        row(5, "lead", RowState::Stalled),
    ];
    let groups = grouped(&rows);
    assert_eq!(groups[0].0, "lead");
    assert_eq!(groups[1].0, ATERM_GROUP);
    let order: Vec<u64> = groups[0].1.iter().map(|row| row.session).collect();
    assert_eq!(order, [3, 5, 2, 4], "running first, tab order within each");
}

/// GROUPS ARE MANAGERS, NOT LABELS: two managers with one title keep two groups, each heading
/// naming its manager, and a manager titled `aterm` is not the harness's own. NEGATIVE CONTROL:
/// one manager's rows share one plain heading.
#[test]
fn workers_of_two_managers_with_one_label_keep_two_groups() {
    let of = |session, manager: Option<&str>, label: &str| WorkerRow {
        under: manager.map_or(RowGroup::Aterm, |sid| RowGroup::Manager(sid.to_owned())),
        ..row(session, label, RowState::Working)
    };
    let rows = [
        of(1, Some("s-a1000000000000000001"), "Claude Code"),
        of(2, Some("s-a1000000000000000002"), "Claude Code"),
        of(3, Some("s-a1000000000000000001"), "Claude Code"),
        of(4, Some("s-c3000000000000000003"), ATERM_GROUP),
        of(5, None, ATERM_GROUP),
        of(6, Some("s-d4000000000000000004"), "lead"),
    ];
    let groups = grouped(&rows);
    let sessions: Vec<Vec<u64>> = groups
        .iter()
        .map(|(_, members)| members.iter().map(|row| row.session).collect())
        .collect();
    assert_eq!(sessions, [vec![1, 3], vec![2], vec![4], vec![6], vec![5]]);
    let headings: Vec<&str> = groups.iter().map(|(heading, _)| heading.as_str()).collect();
    assert_ne!(headings[0], headings[1], "{headings:?}");
    assert!(headings[0].starts_with("Claude Code ("), "{headings:?}");
    assert_ne!(headings[2], ATERM_GROUP, "{headings:?}");
    assert_eq!(headings[3], "lead");
    assert_eq!(headings[4], ATERM_GROUP);
}

/// A FLEET DRIVE'S WORKERS SIT UNDER THE MAC THAT ASKED: one group per requesting Mac, its
/// shells and workers together, apart from a local manager's, the harness's own still last.
/// NEGATIVE CONTROLS: two Macs with one name keep two groups, each heading naming its node; a
/// manager titled as a Mac's group is not that Mac's.
#[test]
fn a_drives_workers_group_by_their_mac_and_a_managers_by_their_manager() {
    const N1: &str = "87be5228c0ffee0087be5228c0ffee00";
    const N2: &str = "c0ffee00c0ffee00c0ffee00c0ffee00";
    let of = |session, under: RowGroup, label: &str| WorkerRow {
        under,
        ..row(session, label, RowState::Working)
    };
    let from = |node: &str| RowGroup::From(node.to_owned());
    let rows = [
        of(1, from(N1), &from_label("m114", N1)),
        of(2, RowGroup::Aterm, ATERM_GROUP),
        of(3, from(N2), &from_label("m72", N2)),
        of(4, from(N1), &from_label("m114", N1)),
        of(
            5,
            RowGroup::Manager("s-a1000000000000000001".to_owned()),
            "lead",
        ),
    ];
    let groups = grouped(&rows);
    let listed: Vec<(&str, Vec<u64>)> = groups
        .iter()
        .map(|(heading, members)| {
            (
                heading.as_str(),
                members.iter().map(|row| row.session).collect(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("From m114", vec![1, 4]),
            ("From m72", vec![3]),
            ("lead", vec![5]),
            (ATERM_GROUP, vec![2]),
        ]
    );
    assert_eq!(
        from_label("", N2),
        "From c0ffee00",
        "no name: the node's head"
    );
    // NEGATIVE CONTROL: two Macs named alike, and a manager titled as one of them.
    let rows = [
        of(1, from(N1), "From m114"),
        of(2, from(N2), "From m114"),
        of(
            3,
            RowGroup::Manager("s-a1000000000000000001".to_owned()),
            "From m114",
        ),
    ];
    let headings: Vec<String> = grouped(&rows)
        .into_iter()
        .map(|(heading, _)| heading)
        .collect();
    assert_eq!(
        headings,
        [
            "From m114 (87be5228)".to_owned(),
            "From m114 (c0ffee00)".to_owned(),
            "From m114 (s-a100)".to_owned(),
        ]
    );
}

#[test]
fn a_press_asks_the_host_by_session() {
    assert_eq!(action_of("workers/open/7"), Some(WorkersAction::Open(7)));
    assert_eq!(action_of("workers/stop/7"), Some(WorkersAction::Stop(7)));
    assert_eq!(
        action_of("workers/result/7"),
        Some(WorkersAction::ReadResult(7))
    );
    assert_eq!(action_of("workers/keep/7"), Some(WorkersAction::Keep(7)));
    assert_eq!(
        action_of("workers/dismiss/7"),
        Some(WorkersAction::Dismiss(7))
    );
    assert_eq!(
        action_of("workers/clear-finished"),
        Some(WorkersAction::ClearFinished)
    );
    assert_eq!(action_of("workers/move/7"), None, "Move to Tab is gone");
    assert_eq!(action_of("workers/open/x"), None);
    assert_eq!(action_of("mail/open/7"), None);
}

#[test]
fn one_row_offers_what_its_state_allows() {
    let mut finished = row(1, "a", RowState::Finished);
    finished.has_result = true;
    let labels = |row: &WorkerRow| -> Vec<String> {
        row_actions(row)
            .0
            .iter()
            .map(|node| {
                node.key
                    .as_str()
                    .rsplit_once('/')
                    .map_or("", |p| p.0)
                    .to_owned()
            })
            .collect()
    };
    assert_eq!(labels(&finished), ["workers/result", "workers/keep"]);
    assert_eq!(
        labels(&row(2, "a", RowState::Working)),
        ["workers/stop", "workers/keep"]
    );
    // A worker whose tab aterm closed offers what it left, never a tab to keep.
    let closed = WorkerRow {
        finished: true,
        ..finished.clone()
    };
    assert_eq!(labels(&closed), ["workers/result", "workers/dismiss"]);
    let no_result = WorkerRow {
        has_result: false,
        stoppable: true,
        ..closed
    };
    assert_eq!(labels(&no_result), ["workers/dismiss"], "nothing to stop");
}

/// FINISHED ROWS SIT UNDER FINISHED, LAST, IN THE ORDER THEIR TABS CLOSED, WHOEVER MANAGED
/// THEM: a running group keeps its heading. CONTROL: with none finished there is no such group,
/// and the page can be closed only when every row is a Finished one.
#[test]
fn finished_rows_sit_last_under_their_own_heading() {
    let done = |session, group: &str| WorkerRow {
        finished: true,
        ..row(session, group, RowState::Finished)
    };
    let rows = [
        done(7, "lead"),
        row(1, "lead", RowState::Working),
        done(3, ATERM_GROUP),
    ];
    let groups = grouped(&rows);
    let headings: Vec<&str> = groups.iter().map(|(heading, _)| heading.as_str()).collect();
    assert_eq!(headings, ["lead", FINISHED_GROUP]);
    let finished: Vec<u64> = groups[1].1.iter().map(|row| row.session).collect();
    assert_eq!(finished, [7, 3]);
    assert!(
        grouped(&rows[1..2])
            .iter()
            .all(|(heading, _)| heading != FINISHED_GROUP)
    );
    let app = |rows: Vec<WorkerRow>| WorkersApp {
        rows,
        actions: VecDeque::new(),
    };
    let view = WorkersViewState::default();
    assert!(!app(rows.to_vec()).presentation(&view).closable);
    assert!(
        app(vec![done(7, "lead"), done(3, ATERM_GROUP)])
            .presentation(&view)
            .closable
    );
    assert!(app(Vec::new()).presentation(&view).closable);
}

#[test]
fn the_chip_names_the_worker_it_shows() {
    assert_eq!(chip_title_of("bench"), "Workers \u{25b8} bench");
}
