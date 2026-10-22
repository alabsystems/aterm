// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ForeignFacts, ProcessReading, foreign_note_text};
use aterm_uds::process::ProcessBirth;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

/// An orphan is named with its facts and the `kill` that ends it; an instance whose launcher
/// still runs gets its uptime only.
#[test]
fn an_orphan_is_named_with_its_remedy_and_a_live_one_is_not() {
    let orphan = ForeignFacts::Writer {
        orphaned: true,
        binary_deleted: true,
        up: Some(Duration::from_secs(7 * 86_400 + 3600)),
    };
    let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &orphan)]).expect("a note");
    assert!(note.contains("`kill 42199`"), "{note}");

    let live = ForeignFacts::Writer {
        orphaned: false,
        binary_deleted: false,
        up: Some(Duration::from_secs(90)),
    };
    let note = foreign_note_text(&[(7, "/tmp/b.sock", &live)]).expect("a note");
    assert!(!note.contains("kill"), "{note}");
    assert_eq!(foreign_note_text(&[]), None);
    assert_eq!(ForeignFacts::default().describe(), None);
}

/// The entry was last written at this second; births are placed around it.
const WRITTEN: u64 = 1_790_000_000;

fn at(seconds: u64) -> ProcessBirth {
    ProcessBirth {
        seconds,
        microseconds: 0,
    }
}

/// A reading of an orphaned aterm on its installed binary, born at `birth`
/// and unchanged through the reads: the case a `kill` is offered for.
fn orphan_reading(birth: ProcessBirth) -> ProcessReading {
    ProcessReading {
        birth,
        slack: crate::graph_hygiene::WRITE_SLACK,
        parent: Some(1),
        exe: Some((
            PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm"),
            Ok(true),
        )),
        birth_after: Some(birth),
    }
}

fn judge(r: ProcessReading) -> ForeignFacts {
    let written = UNIX_EPOCH + Duration::from_secs(WRITTEN);
    ForeignFacts::judge(Some(written), written + Duration::from_secs(3600), r)
}

/// A process born after the entry was last written cannot have written it, however much it
/// looks like the orphan: it is stale and offered no `kill`. Control: born before the write,
/// it is the writer and is offered one.
#[test]
fn a_pid_younger_than_its_entry_is_stale_and_never_offered_a_kill() {
    let stranger = judge(orphan_reading(at(WRITTEN + 600)));
    assert_eq!(stranger, ForeignFacts::Stale);
    // A stale entry is no instance: the note does not count it.
    assert_eq!(
        foreign_note_text(&[(501, "/tmp/aq/a.sock", &stranger)]),
        None
    );

    let writer = judge(orphan_reading(at(WRITTEN - 7 * 86_400)));
    assert!(writer.orphaned(), "{writer:?}");
    let note = foreign_note_text(&[(501, "/tmp/aq/a.sock", &writer)]).expect("a note");
    assert!(note.contains("`kill 501`"), "{note}");
}

/// A birth inside the write slack is still the writer, one past it is not, and with no entry
/// time nothing is proven.
#[test]
fn identity_needs_an_entry_time_and_tolerates_only_the_write_slack() {
    assert!(judge(orphan_reading(at(WRITTEN + 1))).orphaned());
    assert_eq!(judge(orphan_reading(at(WRITTEN + 3))), ForeignFacts::Stale);
    let now = UNIX_EPOCH + Duration::from_secs(WRITTEN);
    assert_eq!(
        ForeignFacts::judge(None, now, orphan_reading(at(WRITTEN - 60))),
        ForeignFacts::Unknown
    );
}

/// Only `aterm` or `aterm-*` executables (the dev-only `aterm-gui` included) get facts or a
/// `kill`.
#[test]
fn a_process_that_is_not_aterm_gets_no_facts_and_no_kill() {
    for name in ["Safari", "loginwindow", "atermx", "aterm_ctl-0123abcd"] {
        let mut r = orphan_reading(at(WRITTEN - 60));
        r.exe = Some((PathBuf::from(format!("/somewhere/{name}")), Ok(true)));
        let facts = judge(r);
        assert_eq!(facts, ForeignFacts::NotAterm(name.to_string()));
        assert!(!facts.orphaned());
        assert_eq!(foreign_note_text(&[(88, "/tmp/c.sock", &facts)]), None);
    }
    let mut r = orphan_reading(at(WRITTEN - 60));
    r.exe = Some((PathBuf::from("/repo/target/debug/aterm-gui"), Ok(true)));
    assert!(judge(r).orphaned(), "an aterm-* binary is aterm");
}

/// "running a deleted binary" needs evidence: no path for the same live process, or a path
/// that is `Ok(false)`. A process that ended or changed hands mid-read says nothing, and a
/// refused `try_exists` (`EPERM`, TCC) is unknown, never deleted.
#[test]
fn a_deleted_binary_is_claimed_only_on_evidence() {
    let deleted = |facts: &ForeignFacts| {
        matches!(
            facts,
            ForeignFacts::Writer {
                binary_deleted: true,
                ..
            }
        )
    };
    let born = at(WRITTEN - 60);
    let installed = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");

    let mut no_path = orphan_reading(born);
    no_path.exe = None;
    assert!(deleted(&judge(no_path)), "no path, same process: deleted");

    let mut exited = orphan_reading(born);
    exited.exe = None;
    exited.birth_after = None;
    assert_eq!(judge(exited), ForeignFacts::Unknown, "it ended mid-read");

    let mut replaced = orphan_reading(born);
    replaced.exe = None;
    replaced.birth_after = Some(at(WRITTEN + 5));
    assert_eq!(judge(replaced), ForeignFacts::Unknown, "pid changed hands");

    let mut gone = orphan_reading(born);
    gone.exe = Some((installed.clone(), Ok(false)));
    assert!(deleted(&judge(gone)), "the named path is gone: deleted");

    let mut refused = orphan_reading(born);
    refused.exe = Some((
        installed.clone(),
        Err(io::Error::from(io::ErrorKind::PermissionDenied)),
    ));
    let facts = judge(refused);
    assert!(
        matches!(facts, ForeignFacts::Writer { .. }) && !deleted(&facts),
        "a refused stat is unknown, never deleted: {facts:?}"
    );
    assert!(
        !deleted(&judge(orphan_reading(born))),
        "present: not deleted"
    );
}

/// Live: this test process, judged against an entry written NOW, is its
/// writer (and, a test binary, not aterm); against one written before it was
/// born it is stale. macOS keeps the wall-clock birth this reads; Linux's
/// boot-clock start is placed on the wall clock.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn this_process_is_judged_by_its_real_birth() {
    let me = std::process::id();
    let now = std::time::SystemTime::now();
    assert!(
        matches!(ForeignFacts::read(me, Some(now)), ForeignFacts::NotAterm(_)),
        "{:?}",
        ForeignFacts::read(me, Some(now))
    );
    assert_eq!(
        ForeignFacts::read(me, Some(UNIX_EPOCH + Duration::from_secs(1_000))),
        ForeignFacts::Stale
    );
    assert_eq!(ForeignFacts::read(me, None), ForeignFacts::Unknown);
}
