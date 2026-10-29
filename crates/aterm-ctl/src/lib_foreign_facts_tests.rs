// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    DirOutcome, ForeignFacts, Parent, Probe, ProcessReading, discovery_report, foreign_note_text,
};
use crate::census::Image;
use aterm_uds::process::{Identity, Lineage, ProcessBirth};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

#[test]
fn an_orphan_is_named_with_its_remedy_and_a_live_one_is_not() {
    let orphan = ForeignFacts::Writer {
        parent: Parent::Adopted,
        binary_deleted: true,
        up: Some(Duration::from_secs(7 * 86_400 + 3600)),
    };
    let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &orphan)]).expect("a note");
    assert!(note.contains("`kill 42199`"), "{note}");
    assert!(note.contains("up 7d 1h"), "{note}");

    let live = ForeignFacts::Writer {
        parent: Parent::Other,
        binary_deleted: false,
        up: Some(Duration::from_secs(90)),
    };
    let note = foreign_note_text(&[(7, "/tmp/b.sock", &live)]).expect("a note");
    assert!(!note.contains("kill"), "{note}");
    assert!(note.contains("up 1m 30s"), "{note}");
    assert_eq!(foreign_note_text(&[]), None);
    assert_eq!(ForeignFacts::default().describe(), None);
}

const WRITTEN: u64 = 1_790_000_000;

fn at(seconds: u64) -> ProcessBirth {
    ProcessBirth {
        seconds,
        microseconds: 0,
    }
}

const LAUNCHD: i32 = 4_100;

fn lineage(birth: ProcessBirth, launcher_version: i32) -> Lineage {
    Lineage {
        launcher_version: Some(launcher_version),
        ..unversioned(birth)
    }
}

/// A lineage that keeps no launcher version, as Linux's.
fn unversioned(birth: ProcessBirth) -> Lineage {
    Lineage {
        identity: Identity {
            pid: 42199,
            birth: birth.into(),
            idversion: Some(77_000),
        },
        uniqueid: 9_000,
        parent: 1,
        pgid: 42199,
        launcher_version: None,
        tty: None,
    }
}

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

#[test]
fn a_pid_younger_than_its_entry_is_stale_and_never_offered_a_kill() {
    let stranger = judge(orphan_reading(at(WRITTEN + 600)));
    assert_eq!(stranger, ForeignFacts::Stale);
    assert_eq!(
        foreign_note_text(&[(501, "/tmp/aq/a.sock", &stranger)]),
        None
    );

    let writer = judge(orphan_reading(at(WRITTEN - 7 * 86_400)));
    assert!(writer.orphaned(), "{writer:?}");
    let note = foreign_note_text(&[(501, "/tmp/aq/a.sock", &writer)]).expect("a note");
    assert!(note.contains("`kill 501`"), "{note}");
}

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

fn parent_of(r: ProcessReading) -> Parent {
    match judge(r) {
        ForeignFacts::Writer { parent, .. } => parent,
        other => panic!("not a writer: {other:?}"),
    }
}

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

/// A lineage that keeps no launcher version (Linux's) says nothing of who launched the process,
/// whether launchd's own version reads or not: init's child is never judged adopted from it.
#[test]
fn a_lineage_without_a_launcher_version_proves_nothing() {
    let born = at(WRITTEN - 60);
    for launchd in [Some(LAUNCHD), None] {
        let mut r = orphan_reading(born);
        r.lineage = Some(unversioned(born));
        r.launchd = launchd;
        let facts = judge(r);
        assert!(
            matches!(
                facts,
                ForeignFacts::Writer {
                    parent: Parent::Unjudged,
                    ..
                }
            ),
            "launchd {launchd:?}: {facts:?}"
        );
        let note = foreign_note_text(&[(42199, "/tmp/aq/a.sock", &facts)]).expect("a note");
        assert!(!note.contains("kill"), "{note}");
    }
}

/// `aterm ctl`'s hint, when private instances are what it found, offers `kill` for exactly the
/// writers launchd adopted: never one launchd started, an unjudged one, or a stale entry, and no
/// hint at all when every entry's writer is gone.
#[test]
fn a_foreign_hint_offers_kill_for_exactly_the_adopted_writers() {
    let born = at(WRITTEN - 60);
    let writer = |lineage: Option<Lineage>| {
        let mut r = orphan_reading(born);
        r.lineage = lineage;
        judge(r)
    };
    let adopted = writer(Some(lineage(born, 31)));
    let started = writer(Some(lineage(born, LAUNCHD)));
    let unjudged = writer(None);
    let foreign = |pid: u32, facts: &ForeignFacts| {
        (
            pid,
            format!("/tmp/aq/aterm-{pid}.sock"),
            Probe::Foreign {
                token: PathBuf::from(format!("/tmp/aq/aterm-{pid}.token")),
                facts: facts.clone(),
            },
        )
    };
    let hint = |probes: &[(u32, String, Probe)]| {
        let found = DirOutcome::Found {
            path: PathBuf::from("/tmp/aq"),
            n: probes.len(),
        };
        let (report, _) = discovery_report(found, probes);
        report
            .lines()
            .find_map(|l| l.trim_start().strip_prefix("hint: ").map(str::to_string))
    };
    let said = hint(&[
        foreign(42199, &adopted),
        foreign(501, &started),
        foreign(502, &unjudged),
        foreign(503, &ForeignFacts::Stale),
    ])
    .expect("a hint");
    assert!(said.contains("`kill 42199` ends it"), "{said}");
    for other in ["501", "502", "503"] {
        assert!(!said.contains(other), "{said}");
    }
    let said = hint(&[foreign(501, &started), foreign(502, &unjudged)]).expect("a hint");
    assert!(
        said.starts_with("only whoever started a `foreign` instance can drive it"),
        "{said}"
    );
    assert!(!said.contains("kill"), "{said}");
    assert_eq!(
        hint(&[
            foreign(503, &ForeignFacts::Stale),
            foreign(504, &ForeignFacts::NotAterm("Safari".to_string())),
        ]),
        None,
        "every writer gone: no hint"
    );
}

/// Live (macOS): a process whose launcher exited reads as adopted through both readers, the
/// reads `ForeignFacts` judges and the census's kernel, so each one reads the lineage of that
/// process and launchd's version.
#[cfg(target_vendor = "apple")]
#[test]
fn a_process_whose_launcher_exited_reads_as_adopted_through_both_readers() {
    use crate::census::{Kernel as _, LiveKernel};
    use aterm_uds::process::{
        END_GRACE, EndControlLive, end_proven, read_identity, read_lineage, read_process_path,
    };
    use std::io::BufRead as _;
    let mut launcher = aterm_uds::spawnfd::command("/bin/sh")
        .args([
            "-c",
            "/bin/sleep 60 </dev/null >/dev/null 2>&1 & echo $!; read _ || :",
        ])
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("spawn the launcher");
    let mut line = String::new();
    std::io::BufReader::new(launcher.stdout.take().expect("its stdout"))
        .read_line(&mut line)
        .expect("the launcher prints its child's pid");
    let pid: u32 = line.trim().parse().expect("a pid");
    let p = i32::try_from(pid).expect("a pid");
    let started = std::time::Instant::now();
    while read_process_path(p).as_deref() != Some(std::path::Path::new("/bin/sleep")) {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "pid {pid} never ran /bin/sleep"
        );
        std::thread::yield_now();
    }
    let shell = read_identity(i32::try_from(launcher.id()).expect("a pid"))
        .expect("the launcher reads while it waits on its stdin");
    let sleep = read_lineage(p).expect("the sleep reads");
    assert_eq!(
        sleep.launched_by(&shell),
        Some(true),
        "the pid the launcher printed is its own child: {sleep:?} {shell:?}"
    );
    /// Ends the proven sleep through the end protocol, its identity (read once it ran
    /// `/bin/sleep`: the pid version moves at the exec) re-read before each signal; a test
    /// that fails first ends it on the way out.
    struct Ends(Option<aterm_uds::process::Identity>);
    impl Ends {
        fn end(
            &mut self,
        ) -> Option<Result<aterm_uds::process::Ended, aterm_uds::process::EndRefusal>> {
            let token = self.0.take()?;
            Some(end_proven(token, END_GRACE, &mut EndControlLive, |_| true))
        }
    }
    impl Drop for Ends {
        fn drop(&mut self) {
            let _ = self.end();
        }
    }
    let mut ends = Ends(Some(sleep.identity));
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
    let ended = ends.end();
    assert!(
        matches!(
            ended,
            Some(Ok(aterm_uds::process::Ended { gone: true, .. }))
        ),
        "the proven sleep is ended: {ended:?}"
    );
}
