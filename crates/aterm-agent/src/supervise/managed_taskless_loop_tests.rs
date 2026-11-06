// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A worker's task is its launch's own prompt, so the conversation's record a host reads for
//! whether anyone asked it anything ([`IdleHost::taskless`], Claude Code's transcript) can only
//! lag that task: a session whose task record stands is never rested on the record's word, even at
//! a first point that comes before the record holds the prompt.

use super::*;

/// A host whose conversation record holds no prompt yet: what Claude Code 2.1.318's transcript
/// held 0.4 s after a worker's launch on the lab's stub, its mode rows and file-history snapshots
/// written and the task's row not, though the stub's instant reply had already ended the turn on
/// screen.
#[derive(Debug, Default)]
struct UnwrittenRecord {
    asked: std::sync::atomic::AtomicUsize,
}

impl IdleHost for UnwrittenRecord {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn taskless(&self) -> bool {
        self.asked.fetch_add(1, Ordering::SeqCst);
        true
    }
}

/// The journal's lines and the host's `taskless` asks of a Land loop over one instant reply, the
/// session's task record `task`, under a host whose record holds no prompt.
fn first_point(tag: &str, task: Option<(u64, String)>) -> (Vec<String>, usize) {
    let (dir, journal) = journal_file(tag);
    let tasks = dir.join("tasks");
    let host = Arc::new(UnwrittenRecord::default());
    let mut m = Mock::new(vec![ended_quick(LAB_REPLY)]);
    m.autopilot = Some("land");
    m.task = task;
    m.vanish_after = Some(3);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
        journal: Some(journal.clone()),
        tasks: Some(tasks),
        ..hosted(60)
    };
    let _ = watch_lines(&mut m, &opts);
    let lines = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    (lines, host.asked.load(Ordering::SeqCst))
}

/// A MANAGED WORKER WHOSE FIRST REPLY CAME AT ONCE IS CONTINUED AT THAT POINT: its task record
/// stands (`state=working`, its manager waits), so the host's record, which holds no prompt yet,
/// is not asked whether the conversation was asked anything, and Land types its instruction at
/// the first point. NEGATIVE CONTROL: with no task record, the same record's word rests the point
/// as nobody's (`NeverTypeIntoATasklessSession`).
#[test]
fn a_managed_workers_first_point_is_continued_before_its_record_holds_the_task() {
    let (lines, asked) = first_point("managed-first-point", working_task());
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with(&format!("CONTINUED seq=101 rule={RULE_LAND} "))),
        "the managed worker's first point is continued: {lines:#?}"
    );
    assert_eq!(asked, 0, "the record is not asked of a managed worker");
    let (lines, asked) = first_point("unmanaged-first-point", None);
    assert!(
        lines
            .iter()
            .any(|l| l == "HOLD seq=101 point nothing why=policy"),
        "the control: an untasked session rests on its record's word: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED ")),
        "the control: nothing is typed into it: {lines:#?}"
    );
    assert!(asked > 0, "the control: the record is asked");
}
