// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ForeignFacts, Probe, ProcessReading, fmt_uptime, foreign_note_text, probe_cause};
use aterm_uds::process::ProcessBirth;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

#[test]
fn uptime_names_its_two_largest_units() {
    assert_eq!(fmt_uptime(Duration::from_secs(42)), "42s");
    assert_eq!(fmt_uptime(Duration::from_secs(5 * 60 + 9)), "5m");
    assert_eq!(fmt_uptime(Duration::from_secs(3 * 3600 + 12 * 60)), "3h12m");
    assert_eq!(
        fmt_uptime(Duration::from_secs(7 * 86_400 + 3600 + 5)),
        "7d1h"
    );
}

/// An orphan, as `instances` names it: orphaned, on a
/// deleted binary, seven days up, with the `kill` that ends it. The control
/// is an instance whose launcher still runs: its uptime only, no `kill`.
#[test]
fn an_orphan_is_named_with_its_remedy_and_a_live_one_is_not() {
    let orphan = ForeignFacts::Writer {
        orphaned: true,
        binary_deleted: true,
        up: Some(Duration::from_secs(7 * 86_400 + 3600)),
    };
    let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &orphan)]).expect("a note");
    assert!(
        note.contains(
            "pid 42199 /tmp/aq/a.sock [its parent is launchd (its launcher exited, or \
             launchd started it); running a deleted binary; up 7d1h]"
        ),
        "{note}"
    );
    assert!(note.contains("`kill 42199` ends it"), "{note}");

    let live = ForeignFacts::Writer {
        orphaned: false,
        binary_deleted: false,
        up: Some(Duration::from_secs(90)),
    };
    let note = foreign_note_text(&[(7, "/tmp/b.sock", &live)]).expect("a note");
    assert!(
        note.starts_with(
            "NOTE: 1 private instance only whoever started it can drive (pid 7 \
             /tmp/b.sock [up 1m])"
        ),
        "{note}"
    );
    assert!(!note.contains("not dialed"), "{note}");
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

/// PID REUSE: a process born AFTER the entry was last written cannot have
/// written it, however much it looks like the orphan — launchd's child, the
/// pid the entry names. It is called stale and offered no `kill`. CONTROL:
/// the same reading born before the write is the writer, and is offered one.
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
    assert!(note.contains("`kill 501` ends it"), "{note}");
    assert!(note.contains("up 7d1h"), "{note}");
}

/// The identity's edges: a birth inside the write slack (an mtime's
/// filesystem granularity) is still the writer; one past it is not; and
/// with no entry time nothing is proven, so nothing is said or offered.
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

/// Only aterm is described: an executable named anything but `aterm` or
/// `aterm-*` gets no facts and no `kill`, even as launchd's child born
/// before the entry. The dev-only `aterm-gui` target counts as aterm.
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

/// A `foreign` row whose pid is no longer the entry's writer names the
/// ENTRY, never an instance being there; a row with no such verdict keeps
/// the private-instance framing.
#[test]
fn a_foreign_row_whose_writer_is_gone_names_the_entry_not_an_instance() {
    let token = PathBuf::from("/x/aterm-9.token");
    for gone in [ForeignFacts::Stale, ForeignFacts::NotAterm("Safari".into())] {
        let row = probe_cause(
            9,
            &Probe::Foreign {
                token: token.clone(),
                facts: gone,
            },
        );
        assert!(row.contains("the entry a private instance left"), "{row}");
        assert!(!row.contains("a private instance whose token"), "{row}");
    }
    let live = probe_cause(
        9,
        &Probe::Foreign {
            token,
            facts: ForeignFacts::Unknown,
        },
    );
    assert!(live.contains("a private instance whose token"), "{live}");
}

/// "running a deleted binary" is said only on evidence: no path for the SAME
/// live process, or a path that is `Ok(false)`. A path read that failed
/// because the process ended (its birth vanished or changed by the last
/// read) says nothing at all, and a `try_exists` refused (`EPERM`, TCC) is
/// unknown, never deleted.
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
/// born it is stale. Only macOS keeps the wall-clock birth this reads.
#[cfg(target_vendor = "apple")]
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
