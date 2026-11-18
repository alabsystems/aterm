// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row(pid: u32, ppid: u32, start: u64) -> ProcessReading {
    ProcessReading {
        pid,
        ppid,
        start: Some(start),
        footprint: Some(u64::from(pid)),
        ..Default::default()
    }
}

fn census() -> Vec<ProcessReading> {
    vec![
        row(4, 1, 100),
        row(8, 4, 110),
        row(12, 8, 120),
        row(16, 999, 130),
        row(20, 16, 140),
        row(28, 20, 145),
        row(24, 999, 150),
        row(40, 1, 90),
        row(44, 999, 160),
        row(48, 1, 50),
        row(52, 999, 99),
        row(60, 999, 170),
    ]
}

fn console(row: &ProcessReading) -> Result<(u64, Option<(u32, u64)>), ()> {
    Ok((
        row.start.unwrap(),
        match row.pid {
            4 => Some((40, 90)),
            16 => Some((12, 120)),
            24 => Some((28, 145)),
            44 => Some((48, 50)),
            60 => None,
            other => panic!("unexpected console query {other}"),
        },
    ))
}

#[test]
fn retained_console_generations_adopt_nested_orphans_once_without_native_lookup() {
    let procs = census();
    let mut reads = Vec::new();
    let facts = collect_console_facts(&procs, &[4, 4], |row| {
        reads.push(row.pid);
        console(row)
    });
    assert_eq!(reads, [4, 16, 24, 44, 60]);
    assert!(!facts.incomplete);
    let tree = facts.tree(&procs, 4);
    assert!(!tree.incomplete);
    let mut pids: Vec<_> = tree.members.iter().map(|row| row.pid).collect();
    pids.sort_unstable();
    assert_eq!(pids, [4, 8, 12, 16, 20, 24, 28]);
    assert_eq!(
        facts
            .rows
            .iter()
            .find(|fact| fact.process.0 == 60)
            .unwrap()
            .host,
        None,
        "a positive detached observation is retained, unlike an unread console"
    );
}

#[test]
fn changed_process_or_host_generation_and_unread_console_never_become_complete() {
    let procs = census();
    for bad in 0..4 {
        let facts = collect_console_facts(&procs, &[4], |row| {
            if row.pid == 16 {
                return match bad {
                    0 => Ok((row.start.unwrap() + 1, Some((12, 120)))),
                    1 => Ok((row.start.unwrap(), Some((12, 121)))),
                    2 => Ok((row.start.unwrap(), Some((9999, 120)))),
                    _ => Err(()),
                };
            }
            console(row)
        });
        let tree = facts.tree(&procs, 4);
        assert!(facts.incomplete && tree.incomplete, "case {bad}");
        assert!(!tree.members.iter().any(|row| row.pid == 16), "case {bad}");
    }
    let facts = collect_console_facts(&procs, &[4], console);
    let mut reused = procs.clone();
    reused[0].start = Some(101);
    let tree = facts.tree(&reused, 4);
    assert!(tree.incomplete);
    assert!(
        tree.members.is_empty(),
        "a later root at the same PID is not this receipt's root"
    );
    let mut reused = procs;
    reused.iter_mut().find(|row| row.pid == 12).unwrap().start = Some(121);
    let tree = facts.tree(&reused, 4);
    assert!(tree.incomplete);
    assert!(!tree.members.iter().any(|row| row.pid == 16));
}

#[test]
fn console_queries_share_one_bound_and_no_orphans_need_no_native_read() {
    let plain = [row(4, 1, 100), row(8, 4, 110)];
    let facts = collect_console_facts(&plain, &[4], |_| panic!("no orphan needs a console read"));
    assert!(!facts.incomplete);
    assert_eq!(facts.tree(&plain, 4).members.len(), 2);
    let mut procs = vec![row(4, 1, 100)];
    procs.extend((0..MAX_CONSOLE_READS as u32 + 1).map(|i| row(8 + i, 999_999, 200)));
    let mut reads = 0;
    let facts = collect_console_facts(&procs, &[4], |row| {
        reads += 1;
        Ok((row.start.unwrap(), None))
    });
    assert_eq!(reads, MAX_CONSOLE_READS);
    assert_eq!(facts.rows.len(), MAX_CONSOLE_READS);
    assert!(facts.incomplete);
    assert!(facts.tree(&procs, 4).incomplete);
}

#[test]
fn separate_orphaned_roots_are_each_observed_once_even_without_other_candidates() {
    let procs = [
        row(4, 999, 100),
        row(8, 999, 110),
        row(12, 1, 50),
        row(16, 1, 60),
    ];
    let mut reads = Vec::new();
    let facts = collect_console_facts(&procs, &[4, 8], |row| {
        reads.push(row.pid);
        Ok((
            row.start.unwrap(),
            Some(if row.pid == 4 { (12, 50) } else { (16, 60) }),
        ))
    });
    assert_eq!(reads, [4, 8]);
    assert!(!facts.incomplete);
    for root in [4, 8] {
        let tree = facts.tree(&procs, root);
        assert!(!tree.incomplete);
        assert_eq!(
            tree.members.iter().map(|row| row.pid).collect::<Vec<_>>(),
            [root]
        );
    }
}
