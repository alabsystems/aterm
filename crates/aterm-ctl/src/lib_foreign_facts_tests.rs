// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ForeignFacts, Parent, ProcessReading, foreign_note_text};
use crate::census::Image;
use aterm_uds::process::{Identity, Lineage, ProcessBirth};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

/// An orphan is named with its facts and the `kill` that ends it; an instance whose launcher
/// still runs gets its uptime only.
#[test]
fn an_orphan_is_named_with_its_remedy_and_a_live_one_is_not() {
    let orphan = ForeignFacts::Writer {
        parent: Parent::Adopted,
        binary_deleted: true,
        up: Some(Duration::from_secs(7 * 86_400 + 3600)),
    };
    let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &orphan)]).expect("a note");
    assert!(note.contains("`kill 42199`"), "{note}");

    let live = ForeignFacts::Writer {
        parent: Parent::Other,
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

/// launchd's pid version in these readings.
const LAUNCHD: i32 = 4_100;

/// The lineage of pid 42199 born at `birth`, adopted by launchd, its launcher's pid version
/// `launcher_version`.
fn lineage(birth: ProcessBirth, launcher_version: i32) -> Lineage {
    Lineage {
        identity: Identity {
            pid: 42199,
            birth: birth.into(),
            idversion: Some(77_000),
        },
        uniqueid: 9_000,
        parent: 1,
        pgid: 42199,
        launcher_version: Some(launcher_version),
        tty: None,
    }
}

/// A reading of an orphaned aterm on its installed binary, born at `birth`, adopted by launchd
/// from a launcher that exited, and unchanged through the reads: the case a `kill` is offered for.
fn orphan_reading(birth: ProcessBirth) -> ProcessReading {
    ProcessReading {
        birth,
        slack: crate::graph_hygiene::WRITE_SLACK,
        parent: Some(1),
        lineage: Some(lineage(birth, 31)),
        launchd: Some(LAUNCHD),
        exe: Some(image("/Applications/aterm.app/Contents/MacOS/aterm", false)),
        birth_after: Some(birth),
    }
}

/// The image at `path`, which the kernel marks deleted or not.
fn image(path: &str, deleted: bool) -> Image {
    Image {
        path: PathBuf::from(path),
        deleted,
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
        r.exe = Some(image(&format!("/somewhere/{name}"), false));
        let facts = judge(r);
        assert_eq!(facts, ForeignFacts::NotAterm(name.to_string()));
        assert!(!facts.orphaned());
        assert_eq!(foreign_note_text(&[(88, "/tmp/c.sock", &facts)]), None);
    }
    let mut r = orphan_reading(at(WRITTEN - 60));
    r.exe = Some(image("/repo/target/debug/aterm-gui", false));
    assert!(judge(r).orphaned(), "an aterm-* binary is aterm");
}

/// "running a deleted binary" is the kernel's word: an image it marks deleted. An image the
/// kernel names no path for proves nothing, not even that the process is aterm, and a process
/// that ended or changed hands mid-read says nothing.
#[test]
fn a_deleted_binary_is_claimed_only_on_the_kernel_s_word() {
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

    let mut no_path = orphan_reading(born);
    no_path.exe = None;
    let facts = judge(no_path);
    assert_eq!(facts, ForeignFacts::Unknown, "no path proves nothing");
    assert!(!facts.orphaned());
    let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &facts)]).expect("a note");
    assert!(!note.contains("kill"), "{note}");

    let mut exited = orphan_reading(born);
    exited.birth_after = None;
    assert_eq!(judge(exited), ForeignFacts::Unknown, "it ended mid-read");

    let mut replaced = orphan_reading(born);
    replaced.birth_after = Some(at(WRITTEN + 5));
    assert_eq!(judge(replaced), ForeignFacts::Unknown, "pid changed hands");

    let mut gone = orphan_reading(born);
    gone.exe = Some(image("/w/wt/target/debug/aterm", true));
    assert!(deleted(&judge(gone)), "the kernel marks it deleted");
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

/// The parent `judge` read of `r`.
fn parent_of(r: ProcessReading) -> Parent {
    match judge(r) {
        ForeignFacts::Writer { parent, .. } => parent,
        other => panic!("not a writer: {other:?}"),
    }
}

/// A process launchd started itself (its launcher's pid version is launchd's) is NEVER an
/// orphan: it is named as launchd's, and no `kill` is offered for it.
#[test]
fn a_launchd_started_instance_is_never_offered_a_kill() {
    let born = at(WRITTEN - 60);
    let mut r = orphan_reading(born);
    r.lineage = Some(lineage(born, LAUNCHD));
    let facts = judge(r);
    let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &facts)]).expect("a note");
    assert!(!note.contains("kill"), "{note}");
    assert!(!facts.orphaned(), "{facts:?}");
    assert!(
        matches!(
            facts,
            ForeignFacts::Writer {
                parent: Parent::Launchd,
                ..
            }
        ),
        "{facts:?}"
    );
    let said = facts.describe().expect("words");
    assert!(said.contains("launchd started it"), "{said}");
}

/// An instance whose launcher exited (another launcher version) is named as adopted, with the
/// `kill` that ends it.
#[test]
fn an_adopted_instance_is_named_with_its_kill() {
    let facts = judge(orphan_reading(at(WRITTEN - 60)));
    let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &facts)]).expect("a note");
    assert!(note.contains("`kill 42199`"), "{note}");
    assert!(facts.orphaned(), "{facts:?}");
    assert!(
        matches!(
            facts,
            ForeignFacts::Writer {
                parent: Parent::Adopted,
                ..
            }
        ),
        "{facts:?}"
    );
    let said = facts.describe().expect("words");
    assert!(said.contains("its launcher exited"), "{said}");
}

/// With flavors 17 and 18 unread (no lineage, or launchd's version unread), a child of launchd
/// keeps the words that say both cases and is offered no `kill`: it may be one launchd started.
#[test]
fn unread_flavors_keep_todays_words_and_offer_no_kill() {
    let born = at(WRITTEN - 60);
    let mut unread = orphan_reading(born);
    unread.lineage = None;
    let facts = judge(unread);
    let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &facts)]).expect("a note");
    assert!(!note.contains("kill"), "{note}");
    assert!(!facts.orphaned(), "{facts:?}");
    assert_eq!(
        facts,
        ForeignFacts::Writer {
            parent: Parent::Unjudged,
            binary_deleted: false,
            up: Some(Duration::from_secs(3660)),
        }
    );
    let said = facts.describe().expect("words");
    let today = if cfg!(target_vendor = "apple") {
        "its parent is launchd (its launcher exited, or launchd started it)"
    } else {
        "its parent is init (its launcher exited)"
    };
    assert!(said.contains(today), "{said}");

    let mut no_launchd = orphan_reading(born);
    no_launchd.launchd = None;
    assert_eq!(parent_of(no_launchd), Parent::Unjudged);
    let mut not_launchds = orphan_reading(born);
    not_launchds.parent = Some(812);
    assert_eq!(parent_of(not_launchds), Parent::Other);
}

/// A lineage read of another process given the same pid (another birth), or one that no longer
/// names launchd as the parent, proves nothing about this one.
#[test]
fn a_lineage_of_another_birth_proves_nothing() {
    let born = at(WRITTEN - 60);
    let mut other = orphan_reading(born);
    other.lineage = Some(lineage(at(WRITTEN - 30), 31));
    assert_eq!(parent_of(other), Parent::Unjudged);
    let mut reparented = orphan_reading(born);
    reparented.lineage = Some(Lineage {
        parent: 812,
        ..lineage(born, 31)
    });
    assert_eq!(parent_of(reparented), Parent::Unjudged);
    let mut other = orphan_reading(born);
    other.lineage = Some(lineage(at(WRITTEN - 30), 31));
    let facts = judge(other);
    assert!(!facts.orphaned(), "{facts:?}");
    let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &facts)]).expect("a note");
    assert!(!note.contains("kill"), "{note}");
}

/// Live (macOS): a process whose launcher exited reads as adopted through both readers, the
/// reads `ForeignFacts` judges and the census's kernel, so each one reads the lineage of that
/// process and launchd's version.
#[cfg(target_vendor = "apple")]
#[test]
fn a_process_whose_launcher_exited_reads_as_adopted_through_both_readers() {
    use crate::census::{Kernel as _, LiveKernel};
    use std::io::BufRead as _;
    let mut launcher = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "/bin/sleep 60 </dev/null >/dev/null 2>&1 & echo $!"])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn the launcher");
    let mut line = String::new();
    std::io::BufReader::new(launcher.stdout.take().expect("its stdout"))
        .read_line(&mut line)
        .expect("the launcher prints its child's pid");
    let pid: u32 = line.trim().parse().expect("a pid");
    // The sleep is this test's: its pid came from the shell this test started.
    struct Ends(u32);
    impl Drop for Ends {
        fn drop(&mut self) {
            unsafe extern "C" {
                fn kill(pid: i32, sig: i32) -> i32;
            }
            const SIGKILL: i32 = 9;
            if let Ok(pid) = i32::try_from(self.0) {
                // SAFETY: SIGKILL to the one pid this test started.
                unsafe { kill(pid, SIGKILL) };
            }
        }
    }
    let _ends = Ends(pid);
    assert!(launcher.wait().expect("reap the launcher").success());

    let reading = ProcessReading::read(pid).expect("the sleep reads");
    assert_eq!(reading.parent, Some(1), "{reading:?}");
    let lineage = reading.lineage.expect("its lineage reads");
    assert_eq!(
        u32::try_from(lineage.identity.pid).ok(),
        Some(pid),
        "{lineage:?}"
    );
    assert_eq!(
        Parent::of(
            reading.parent,
            reading.lineage.as_ref(),
            Some(reading.birth),
            reading.launchd
        ),
        Parent::Adopted,
        "{reading:?}"
    );

    let row = LiveKernel
        .listed()
        .expect("the table reads")
        .into_iter()
        .find(|r| r.pid == pid)
        .expect("the sleep is listed");
    let lineage = LiveKernel.lineage(pid);
    assert_eq!(
        Parent::of(
            row.ppid,
            lineage.as_ref(),
            row.birth,
            LiveKernel.launchd_version()
        ),
        Parent::Adopted,
        "{row:?} {lineage:?}"
    );
}
