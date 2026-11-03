// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The reader over a fake kernel: every read injected and counted, and the fake panics on any
//! read of the environment or arguments of a process whose thread has not run.

use super::*;
use std::cell::RefCell;

/// launchd's pid version in the fake; every process here was launched by version 50.
const LAUNCHD: i32 = 1;

/// One process of this uid as the fake lays it out.
#[derive(Clone, Debug)]
struct Proc {
    lin: Lineage,
    /// Its one thread ran: only then may its environment or arguments be read.
    ran: bool,
    exe: Option<PathBuf>,
    markers: Markers,
    argv: Vec<&'static str>,
    /// Its environment and arguments read nothing (a read that failed).
    unread: bool,
}

#[derive(Default)]
struct Fake {
    procs: BTreeMap<i32, Proc>,
    /// The listener table as the scan reads it; `None` when it does not read.
    listeners: Option<HolderScan>,
    /// Each read the reader made, by kind and pid.
    reads: RefCell<Vec<(&'static str, i32)>>,
}

impl Fake {
    fn add(&mut self, pid: i32, parent: u32, pgid: u32, uniqueid: u64) -> &mut Proc {
        self.procs.insert(
            pid,
            Proc {
                lin: Lineage {
                    identity: Identity {
                        pid,
                        birth: Birth(1_792_000_000_000_000 + u64::from(pid.unsigned_abs())),
                        idversion: Some(pid * 10),
                    },
                    uniqueid,
                    parent,
                    pgid,
                    launcher_version: Some(50),
                    tty: None,
                },
                ran: true,
                exe: Some(PathBuf::from("/usr/bin/ugrep")),
                markers: Markers::default(),
                argv: Vec::new(),
                unread: false,
            },
        );
        self.procs.get_mut(&pid).expect("just added")
    }

    fn count(&self, kind: &str) -> usize {
        self.reads
            .borrow()
            .iter()
            .filter(|(k, _)| *k == kind)
            .count()
    }

    fn read_of(&self, kind: &'static str, pid: i32) -> bool {
        self.reads.borrow().contains(&(kind, pid))
    }

    fn started(&self, pid: i32, why: &str) {
        let ran = self.procs.get(&pid).is_some_and(|p| p.ran);
        assert!(ran, "read {why} of pid {pid}, whose thread has not run");
    }
}

impl Kernel for Fake {
    fn birth(&self, pid: i32) -> Option<Birth> {
        self.procs.get(&pid).map(|p| p.lin.identity.birth)
    }
    fn presence(&self, pid: i32) -> ProcessPresence {
        if self.procs.contains_key(&pid) {
            ProcessPresence::Unknown
        } else {
            ProcessPresence::Absent
        }
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        self.reads.borrow_mut().push(("lineage", pid));
        self.procs.get(&pid).map(|p| p.lin)
    }
    fn members(&self, pgid: u32) -> Option<GroupMembers> {
        Some(GroupMembers {
            pids: self
                .procs
                .iter()
                .filter(|(_, p)| p.lin.pgid == pgid)
                .map(|(pid, _)| *pid)
                .collect(),
            complete: true,
        })
    }
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        self.reads.borrow_mut().push(("exe", pid));
        self.procs.get(&pid)?.exe.clone()
    }
    fn markers(&self, pid: i32) -> Option<Markers> {
        self.started(pid, "the environment");
        self.reads.borrow_mut().push(("markers", pid));
        self.procs
            .get(&pid)
            .filter(|p| !p.unread)
            .map(|p| p.markers.clone())
    }
    fn held(&self, pid: i32) -> Option<bool> {
        self.procs.get(&pid).map(|p| !p.ran)
    }
    fn held_now(&self) -> Option<HeldNow> {
        None
    }
    fn launchd_version(&self) -> Option<i32> {
        Some(LAUNCHD)
    }
    fn tty_master_held(&self, _: u32) -> Option<bool> {
        self.reads.borrow_mut().push(("tty", 0));
        Some(false)
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        Some(self.procs.keys().copied().collect())
    }
    fn now_ms(&self) -> u64 {
        1_792_000_100_000
    }
    fn end(&self, _: Identity, _: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        Err(EndRefusal::NotProven)
    }
}

impl LeftKernel for Fake {
    fn listeners(&self) -> Option<HolderScan> {
        self.reads.borrow_mut().push(("listeners", 0));
        self.listeners.clone()
    }
    fn sandboxed(&self, _: i32) -> Option<bool> {
        Some(false)
    }
    fn coalition(&self, _: i32) -> Option<u64> {
        None
    }
    fn argv(&self, pid: i32) -> Option<Vec<OsString>> {
        self.started(pid, "the arguments");
        self.reads.borrow_mut().push(("argv", pid));
        let p = self.procs.get(&pid).filter(|p| !p.unread)?;
        Some(p.argv.iter().map(OsString::from).collect())
    }
    fn writer(&self, _: i32) -> bool {
        false
    }
}

/// The sweep's rows of the fake's processes.
fn rows(k: &Fake) -> Vec<Row> {
    k.procs
        .iter()
        .map(|(pid, p)| Row {
            pid: pid.unsigned_abs(),
            ppid: p.lin.parent,
            pgid: p.lin.pgid,
            start: Some(u64::from(
                p.lin.identity.idversion.unwrap_or(0).unsigned_abs(),
            )),
            cpu_ns: Some(2_000_000),
            birth_ms: None,
        })
        .collect()
}

/// A fake with this process (pid 100, under pid 90) in it, and a listener table that reads.
fn world() -> Fake {
    let mut k = Fake {
        listeners: Some(HolderScan::default()),
        ..Fake::default()
    };
    k.add(90, 1, 90, 10);
    k.add(100, 90, 90, 20);
    k
}

fn read(reader: &mut Reader, k: &Fake, held: &[(u32, Held)]) -> Reading {
    read_at(reader, k, held, 1_792_000_100_000)
}

fn read_at(reader: &mut Reader, k: &Fake, held: &[(u32, Held)], now_ms: u64) -> Reading {
    let rows = rows(k);
    let known = Known {
        self_pid: 100,
        now_ms,
        boot: Some("b"),
        records: &[],
        held,
        waiting: &[],
        shells: &[],
    };
    reader.read(k, &rows, &known)
}

/// `pid`'s facts in `reading`.
fn facts_of(reading: &Reading, pid: u32) -> &Facts {
    reading.facts.iter().find(|f| f.pid == pid).expect("listed")
}

fn naming(sid: &str) -> Markers {
    Markers {
        parent_session: Some(sid.into()),
        ..Markers::default()
    }
}

/// A held orphan's environment and arguments are never read, nor are a program's the jobs view
/// names as held whose thread list reads it ran (a development build's stand-in): only a
/// group-mate that ran is asked. NEGATIVE CONTROL: read the markers of every new candidate
/// whatever its thread list says, and the fake panics.
#[test]
fn the_reader_never_reads_a_held_programs_environment() {
    let mut k = world();
    let held = k.add(2606, 1, 2600, 1500);
    held.ran = false;
    held.exe = Some(PathBuf::from(
        "/Applications/aterm.app/Contents/MacOS/aterm",
    ));
    k.add(2600, 1, 2600, 1400).markers = naming("s-live");
    k.add(2700, 1, 2700, 1600).exe = Some(PathBuf::from("/usr/local/bin/aterm"));
    let stand_in = (
        2700,
        Held {
            check_ms: 1_000,
            tab: None,
        },
    );
    let reading = read(&mut Reader::new(), &k, &[stand_in]);
    let held = reading
        .facts
        .iter()
        .find(|f| f.pid == 2606)
        .expect("listed");
    assert_eq!(held.thread_ran, Some(false));
    assert_eq!(held.marks, None);
    assert_eq!(
        held.mate_marks.as_ref().and_then(|m| m.session.as_deref()),
        Some("s-live"),
        "its group-mate that ran speaks for it"
    );
    let stood = reading
        .facts
        .iter()
        .find(|f| f.pid == 2700)
        .expect("listed");
    assert_eq!(stood.thread_ran, Some(false));
    assert!(!k.read_of("markers", 2606) && !k.read_of("argv", 2606));
    assert!(!k.read_of("markers", 2700) && !k.read_of("argv", 2700));
}

/// A platform binary's environment reads withheld; a group-mate's that ran names its session.
/// NEGATIVE CONTROL: read only its own markers, and no session ties it.
#[test]
fn a_platform_binary_borrows_a_group_mates_markers() {
    let mut k = world();
    k.add(2606, 1, 2600, 1500).markers = Markers {
        withheld: true,
        ..Markers::default()
    };
    k.add(2600, 1, 2600, 1400).markers = naming("s-live");
    let reading = read(&mut Reader::new(), &k, &[]);
    let f = reading
        .facts
        .iter()
        .find(|f| f.pid == 2606)
        .expect("listed");
    assert!(f.marks.as_ref().is_some_and(|m| m.withheld));
    assert_eq!(
        f.mate_marks.as_ref().and_then(|m| m.session.as_deref()),
        Some("s-live")
    );
    let mate = reading
        .facts
        .iter()
        .find(|f| f.pid == 2600)
        .expect("listed");
    assert_eq!(mate.mate_marks, None, "its own environment spoke");
}

/// Seventy orphans: one reading reads 64 of them and says it was bounded; the next reads the
/// other six and reads none of the 64 again. NEGATIVE CONTROL: lift the bound, and the first
/// reading reads all seventy.
#[test]
fn at_most_64_new_candidates_are_read_a_reading_and_the_view_says_bounded() {
    let mut k = world();
    for n in 0..70 {
        k.add(
            3000 + n,
            1,
            3000 + n.unsigned_abs(),
            2000 + u64::from(n.unsigned_abs()),
        );
    }
    let mut reader = Reader::new();
    let first = read(&mut reader, &k, &[]);
    assert_eq!(first.facts.len(), NEW_READS);
    assert!(first.bounded);
    assert_eq!(k.count("exe"), NEW_READS);
    let second = read(&mut reader, &k, &[]);
    assert_eq!(second.facts.len(), 70);
    assert!(!second.bounded);
    assert_eq!(k.count("exe"), 70, "the 64 kept were not read again");
}

/// A candidate's reads are kept while its identity holds: the next reading reads its lineage
/// and nothing it kept. NEGATIVE CONTROL: key the kept reads by pid alone, and an exec (a new
/// pid version) is not read again.
#[test]
fn a_cached_candidate_is_not_read_again_until_its_identity_moves() {
    let mut k = world();
    k.add(2606, 1, 2600, 1500).markers = naming("s-live");
    let mut reader = Reader::new();
    read(&mut reader, &k, &[]);
    read(&mut reader, &k, &[]);
    assert_eq!((k.count("exe"), k.count("markers")), (1, 1));
    if let Some(p) = k.procs.get_mut(&2606) {
        p.lin.identity.idversion = Some(26_061);
    }
    read(&mut reader, &k, &[]);
    assert_eq!((k.count("exe"), k.count("markers")), (2, 2));
}

/// An exec keeps the pid and moves its version (Apple): the new image's path and environment
/// are the row's, never the old one's. NEGATIVE CONTROL: key the kept reads by pid alone, and
/// the row keeps the shell it exec'd from.
#[test]
fn an_exec_moves_the_cache_key() {
    let mut k = world();
    let p = k.add(2606, 1, 2600, 1500);
    p.exe = Some(PathBuf::from("/bin/zsh"));
    p.markers = naming("s-old");
    let mut reader = Reader::new();
    let before = read(&mut reader, &k, &[]);
    assert_eq!(
        before
            .facts
            .iter()
            .find(|f| f.pid == 2606)
            .and_then(|f| f.exe.clone()),
        Some("/bin/zsh".into())
    );
    if let Some(p) = k.procs.get_mut(&2606) {
        p.lin.identity.idversion = Some(26_061);
        p.exe = Some(PathBuf::from("/usr/bin/ugrep"));
        p.markers = naming("s-new");
    }
    let after = read(&mut reader, &k, &[]);
    let f = after.facts.iter().find(|f| f.pid == 2606).expect("listed");
    assert_eq!(f.exe.as_deref(), Some("/usr/bin/ugrep"));
    assert_eq!(
        f.marks.as_ref().and_then(|m| m.session.as_deref()),
        Some("s-new")
    );
    assert_eq!(
        f.token.as_deref(),
        Some("2606@1792000000002606:26061"),
        "the token names the image now running"
    );
}

/// What is no candidate is not read at all: a live process under a live parent, this process
/// and its ancestors, and what launchd started.
#[test]
fn only_candidates_are_read() {
    let mut k = world();
    k.add(2000, 90, 2000, 1000);
    let launchd = k.add(2100, 1, 2100, 1100);
    launchd.lin.launcher_version = Some(LAUNCHD);
    let reading = read(&mut Reader::new(), &k, &[]);
    assert!(reading.facts.is_empty(), "{:?}", reading.facts);
    assert_eq!(k.count("exe"), 0);
    assert_eq!(k.count("markers"), 0);
    assert_eq!(reading.horizon, 1100, "the highest unique id of the table");
}

/// A dead verify run's stage member: born into its open group after the leader, running from
/// its roots, orphaned. It is read with the record's tie and the record's own verdict.
#[test]
fn a_dead_runs_member_is_read_with_its_run_tie() {
    let mut k = world();
    k.add(4000, 1, 4000, 3000).exe = Some(PathBuf::from("/wt/target/debug/aterm-link"));
    k.add(4001, 4000, 4000, 3001).exe = Some(PathBuf::from("/wt/target/debug/aterm-link"));
    let leader_birth = k.procs[&4000].lin.identity.birth.0;
    let rec = Record::parse(&format!(
        "run v=1 boot=b id=77-1792000000000077 uniqueid=2900 start=1 sid=-\n\
         root path=/wt\n\
         group pgid=4000 leader_birth={leader_birth} leader_uniqueid=3000 stage=test\n\
         dead_seen uniqueid=3001 at=1792000100000\n"
    ));
    let records = vec![(PathBuf::from("/r/77-1792000000000077.run"), rec)];
    let rows = rows(&k);
    let known = Known {
        self_pid: 100,
        now_ms: 1_792_000_100_000,
        boot: Some("b"),
        records: &records,
        held: &[],
        waiting: &[],
        shells: &[],
    };
    let reading = Reader::new().read(&k, &rows, &known);
    assert_eq!(reading.runs.len(), 1);
    assert!(reading.runs[0].dead, "its launcher, pid 77, is gone");
    let member = reading
        .facts
        .iter()
        .find(|f| f.pid == 4001)
        .expect("listed");
    assert_eq!(
        member.runs,
        vec![RunTie {
            id: "77-1792000000000077".into(),
            group: true,
            run: false,
            tree: true,
            dead: true,
            auto: false,
        }],
        "its parent, the stage leader, lives: not orphaned, so not the next run's to end"
    );
    let leader = reading
        .facts
        .iter()
        .find(|f| f.pid == 4000)
        .expect("listed");
    assert!(
        leader.runs[0].auto,
        "orphaned, under the roots, its thread ran: A1–A7 hold"
    );
}

#[test]
fn an_unrecorded_dead_run_has_no_automatic_cleanup_authority() {
    let mut k = world();
    k.add(4000, 1, 4000, 3000).exe = Some(PathBuf::from("/wt/target/debug/aterm-link"));
    let leader_birth = k.procs[&4000].lin.identity.birth.0;
    let record = Record::parse(&format!(
        "run v=1 boot=b id=77-1792000000000077 uniqueid=2900 start=1 sid=-\n\
         root path=/wt\n\
         group pgid=4000 leader_birth={leader_birth} leader_uniqueid=3000 stage=test\n"
    ));
    let mut records = vec![(PathBuf::from("/r/77-1792000000000077.run"), record)];
    for recorded in [false, true] {
        if recorded {
            records[0].1.dead_seen = Some(3000);
            records[0].1.dead_seen_ms = Some(1_792_000_100_000);
        }
        let known = Known {
            self_pid: 100,
            now_ms: 1_792_000_100_000,
            boot: Some("b"),
            records: &records,
            held: &[],
            waiting: &[],
            shells: &[],
        };
        let reading = Reader::new().read(&k, &rows(&k), &known);
        assert_eq!(facts_of(&reading, 4000).runs[0].auto, recorded);
        let fresh = read_one(&k, 4000, &known).unwrap();
        assert_eq!(fresh.runs[0].auto, recorded);
        assert!(fresh.runs[0].tree, "the diagnostic tie remains available");
    }
}

/// Facts read for `end` are read again, none kept and none bounded.
#[test]
fn read_one_reads_every_fact_again() {
    let mut k = world();
    k.add(2606, 1, 2600, 1500).markers = naming("s-live");
    let known = Known {
        self_pid: 100,
        now_ms: 1_792_000_100_000,
        boot: Some("b"),
        records: &[],
        held: &[],
        waiting: &[],
        shells: &[],
    };
    let f = read_one(&k, 2606, &known).expect("read");
    assert_eq!(f.token.as_deref(), Some("2606@1792000000002606:26060"));
    assert!(read_one(&k, 2606, &known).is_some());
    assert_eq!(k.count("markers"), 2);
    assert_eq!(read_one(&k, 100, &known), None, "this process");
    assert_eq!(read_one(&k, 9999, &known), None, "gone");
}

/// The listener table is one scan of every process's table: a reading reads it at most once a
/// minute, and a reading for `end` reads it anew.
#[test]
fn the_listener_table_is_read_at_most_once_a_minute() {
    let mut k = world();
    k.add(2606, 1, 2600, 1500);
    let mut reader = Reader::new();
    let at = |now_ms: u64| Known {
        self_pid: 100,
        now_ms,
        boot: Some("b"),
        records: &[],
        held: &[],
        waiting: &[],
        shells: &[],
    };
    let rows = rows(&k);
    reader.read(&k, &rows, &at(1_000));
    reader.read(&k, &rows, &at(2_000));
    reader.read(&k, &rows, &at(1_000 + SHARED_READS_MS - 1));
    assert_eq!(k.count("listeners"), 1);
    reader.read(&k, &rows, &at(1_000 + SHARED_READS_MS));
    assert_eq!(k.count("listeners"), 2);
    let _ = read_one(&k, 2606, &at(1_000 + SHARED_READS_MS));
    assert_eq!(k.count("listeners"), 3);
}

/// Where the kernel keeps no pid version (Linux), an exec keeps the pid and the start: the
/// executable, read again each reading, says the image moved, and the new image's path and
/// environment are the row's. NEGATIVE CONTROL: keep the reads by identity alone, and the row
/// keeps the shell it exec'd from.
#[test]
fn an_exec_with_no_pid_version_reads_the_new_image() {
    let mut k = world();
    let p = k.add(2606, 1, 2600, 1500);
    p.lin.identity.idversion = None;
    p.lin.launcher_version = None;
    p.exe = Some(PathBuf::from("/bin/sh"));
    p.markers = naming("s-old");
    let mut reader = Reader::new();
    let before = read(&mut reader, &k, &[]);
    assert_eq!(facts_of(&before, 2606).exe.as_deref(), Some("/bin/sh"));
    let again = read(&mut reader, &k, &[]);
    assert_eq!(
        (k.count("markers"), facts_of(&again, 2606).exe.as_deref()),
        (1, Some("/bin/sh")),
        "the same image is not read again"
    );
    if let Some(p) = k.procs.get_mut(&2606) {
        p.exe = Some(PathBuf::from("/usr/bin/ssh-agent"));
        p.markers = naming("s-new");
    }
    let after = read(&mut reader, &k, &[]);
    let f = facts_of(&after, 2606);
    assert_eq!(f.exe.as_deref(), Some("/usr/bin/ssh-agent"));
    assert_eq!(
        f.marks.as_ref().and_then(|m| m.session.as_deref()),
        Some("s-new")
    );
}

/// A read that answered nothing is no answer kept: an environment or an aterm image's verb that
/// did not read is asked again once a minute has passed since, and not before. NEGATIVE
/// CONTROL: keep a failed read as read, and the row never gets its session or its verb.
#[test]
fn a_read_that_answered_nothing_is_asked_again_a_minute_on() {
    let mut k = world();
    let p = k.add(2606, 1, 2600, 1500);
    p.markers = naming("s-live");
    p.unread = true;
    let w = k.add(2700, 1, 2700, 1600);
    w.exe = Some(PathBuf::from("/usr/local/bin/aterm"));
    w.argv = vec!["aterm", crate::tether::VERB];
    w.unread = true;
    let t0 = 1_792_000_100_000;
    let mut reader = Reader::new();
    let first = read_at(&mut reader, &k, &[], t0);
    assert_eq!(facts_of(&first, 2606).marks, None);
    assert_eq!(facts_of(&first, 2700).by_design, None, "an unread verb");
    for p in k.procs.values_mut() {
        p.unread = false;
    }
    let soon = read_at(&mut reader, &k, &[], t0 + 1_000);
    assert_eq!(
        facts_of(&soon, 2606).marks,
        None,
        "asked again only a minute on"
    );
    assert_eq!(
        k.count("markers"),
        2,
        "one read each, none again within the minute"
    );
    let later = read_at(&mut reader, &k, &[], t0 + SHARED_READS_MS);
    assert_eq!(
        facts_of(&later, 2606)
            .marks
            .as_ref()
            .and_then(|m| m.session.as_deref()),
        Some("s-live")
    );
    assert_eq!(facts_of(&later, 2700).by_design, Some(true));
}

/// A held program's group-mates are asked for their markers only once their thread ran: eight
/// held mates listed first take none of the eight asks, and the mate that ran speaks. NEGATIVE
/// CONTROL: take the eight before leaving out the held ones, and no mate speaks.
#[test]
fn a_mate_that_ran_speaks_past_eight_held_ones() {
    let mut k = world();
    k.add(2606, 1, 2600, 1500).ran = false;
    for mate in 2591..=2598 {
        k.add(mate, 2606, 2600, 1501 + u64::from(mate.unsigned_abs()))
            .ran = false;
    }
    k.add(2599, 2606, 2600, 1499).markers = naming("s-live");
    let reading = read(&mut Reader::new(), &k, &[]);
    assert_eq!(
        facts_of(&reading, 2606)
            .mate_marks
            .as_ref()
            .and_then(|m| m.session.as_deref()),
        Some("s-live")
    );
}

/// The run's own verdict is judged on what the reading read: a dead run's member is asked for
/// its environment once and its terminal's master once a minute, however many readings judge it.
/// NEGATIVE CONTROL: judge it on the kernel's own reads, and each reading reads both again.
#[test]
fn a_dead_runs_verdict_reads_nothing_of_its_own() {
    let mut k = world();
    let leader = k.add(4000, 1, 4000, 3000);
    leader.exe = Some(PathBuf::from("/wt/target/debug/aterm-link"));
    leader.lin.tty = Some(0x1000_0005);
    let leader_birth = k.procs[&4000].lin.identity.birth.0;
    let rec = Record::parse(&format!(
        "run v=1 boot=b id=77-1792000000000077 uniqueid=2900 start=1 sid=-\n\
         root path=/wt\n\
         group pgid=4000 leader_birth={leader_birth} leader_uniqueid=3000 stage=test\n\
         dead_seen uniqueid=3000 at=1792000100000\n"
    ));
    let records = vec![(PathBuf::from("/r/77-1792000000000077.run"), rec)];
    let rows = rows(&k);
    let mut reader = Reader::new();
    for n in 0..3 {
        let known = Known {
            self_pid: 100,
            now_ms: 1_792_000_100_000 + n,
            boot: Some("b"),
            records: &records,
            held: &[],
            waiting: &[],
            shells: &[],
        };
        let reading = reader.read(&k, &rows, &known);
        assert!(facts_of(&reading, 4000).runs[0].auto, "A1–A7 hold");
    }
    assert_eq!((k.count("markers"), k.count("tty")), (1, 1));
}

/// A listener table that did not read, or that could not match a process's descriptors, rules
/// no listener out: the row's listener is unknown. NEGATIVE CONTROL: read an unmatched process
/// as holding none, and its listener reads `Some(false)`.
#[test]
fn an_unread_listener_table_rules_no_listener_out() {
    let mut k = world();
    k.add(2606, 1, 2600, 1500);
    k.add(2607, 1, 2607, 1501);
    let listener = |k: &Fake, pid: u32| facts_of(&read(&mut Reader::new(), k, &[]), pid).listener;
    assert_eq!(listener(&k, 2606), Some(false));
    k.listeners = Some(HolderScan {
        holders: vec![2606],
        unreadable: vec![2607],
        ..HolderScan::default()
    });
    assert_eq!(listener(&k, 2606), Some(true));
    assert_eq!(
        listener(&k, 2607),
        None,
        "its descriptors could not be matched"
    );
    k.listeners = None;
    assert_eq!(listener(&k, 2606), None, "the table did not read");
}
