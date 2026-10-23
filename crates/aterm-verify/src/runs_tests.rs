// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super::runs`].

use super::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use aterm_uds::process::EndControl;

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

fn birth(us: u64) -> Birth {
    Birth(us)
}

/// The birth the fake gives pid `pid`: one per pid, so a reused number reads another.
fn born(pid: i32) -> u64 {
    1_000 + u64::try_from(pid).expect("a pid")
}

/// A process of this uid as the fake lays it out: launched by a launcher older than launchd's
/// version below, so with `parent` 1 it is a real orphan; no terminal.
fn lin(pid: i32, uniqueid: u64, pgid: u32, parent: u32) -> Lineage {
    Lineage {
        identity: Identity {
            pid,
            birth: birth(born(pid)),
            idversion: Some(pid),
        },
        uniqueid,
        parent,
        pgid,
        launcher_version: Some(100),
        tty: None,
    }
}

/// A Linux-shaped lineage: no pid versions, and the start tick as both birth and unique id.
fn lin_linux(pid: i32, ticks: u64, pgid: u32, parent: u32) -> Lineage {
    Lineage {
        identity: Identity {
            pid,
            birth: Birth(ticks),
            idversion: None,
        },
        uniqueid: ticks,
        parent,
        pgid,
        launcher_version: None,
        tty: None,
    }
}

/// A fake kernel scripted with the processes a test lays out.
#[derive(Default)]
struct FakeKernel {
    lineages: RefCell<HashMap<i32, Lineage>>,
    groups: HashMap<u32, Vec<i32>>,
    /// Groups whose listing filled its buffer.
    partial: HashSet<u32>,
    exes: HashMap<i32, PathBuf>,
    markers: HashMap<i32, Option<Markers>>,
    held: HashMap<i32, Option<bool>>,
    ttys: HashMap<u32, Option<bool>>,
    /// The uid listing fails.
    unlisted: bool,
    /// What a process becomes just before its end: a change between the judgment and the
    /// signal, with its identity kept.
    moves: RefCell<HashMap<i32, Lineage>>,
    /// Executables that change between the judgment and the signal (an exec under the same
    /// identity, which Linux cannot see in the identity).
    exe_moves: RefCell<HashMap<i32, PathBuf>>,
    /// The executables `exe_moves` moved to, once `end` began.
    exe_moved: RefCell<HashMap<i32, PathBuf>>,
    /// Processes that outlive their SIGTERM.
    stubborn: HashSet<i32>,
    /// Processes whose signals the kernel refuses.
    refused: HashSet<i32>,
    /// Processes that exit just before their signal lands: the kernel answers `ESRCH`.
    vanish: HashSet<i32>,
    /// Processes that outlive a SIGKILL too (wedged in the kernel).
    wedged: HashSet<i32>,
    /// Live processes whose flavor-18 record does not read (their birth still does).
    unreadable: HashSet<i32>,
    /// Live processes whose separate birth read fails.
    birth_unreadable: HashSet<i32>,
    /// Injected existence-query answers; otherwise presence follows the fake process table.
    presence: HashMap<i32, ProcessPresence>,
    /// Processes that exit on their own after this many more lineage reads.
    exiting: RefCell<HashMap<i32, usize>>,
    signals: RefCell<Vec<(i32, i32)>>,
    /// Whose environment was read, in order.
    markers_read: RefCell<Vec<i32>>,
}

impl FakeKernel {
    fn put(&mut self, l: Lineage, exe: &str) {
        self.lineages.get_mut().insert(l.identity.pid, l);
        self.exes.insert(l.identity.pid, PathBuf::from(exe));
    }

    /// `pid` exits: it reads nothing more, and its children are adopted (parent 1), as the
    /// kernel reparents them.
    fn exits(&self, pid: i32) {
        let mut lineages = self.lineages.borrow_mut();
        lineages.remove(&pid);
        for l in lineages
            .values_mut()
            .filter(|l| l.parent == pid.unsigned_abs())
        {
            l.parent = 1;
        }
    }

    /// Who got a SIGTERM, in order.
    fn termed(&self) -> Vec<i32> {
        self.signals
            .borrow()
            .iter()
            .filter(|(_, s)| *s == SIGTERM)
            .map(|(p, _)| *p)
            .collect()
    }
}

/// The fake's side of the real `end_proven`: a SIGTERM ends a process unless it is stubborn, a
/// SIGKILL unless it is wedged; a refused or vanishing process takes no signal.
struct Control<'a>(&'a FakeKernel);

impl EndControl for Control<'_> {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        self.0.lineages.borrow().get(&pid).map(|l| l.identity)
    }
    fn signal(&mut self, token: &Identity, sig: i32) -> std::io::Result<()> {
        let pid = token.pid;
        if self.0.refused.contains(&pid) {
            return Err(std::io::Error::other("refused"));
        }
        if self.0.vanish.contains(&pid) {
            self.0.exits(pid);
            return Err(std::io::Error::from_raw_os_error(3));
        }
        self.0.signals.borrow_mut().push((pid, sig));
        let outlives = if sig == SIGKILL {
            self.0.wedged.contains(&pid)
        } else {
            self.0.stubborn.contains(&pid)
        };
        if !outlives {
            self.0.exits(pid);
        }
        Ok(())
    }
    fn nap(&mut self, _pause: Duration) {}
}

impl Kernel for FakeKernel {
    fn birth(&self, pid: i32) -> Option<Birth> {
        if self.birth_unreadable.contains(&pid) {
            return None;
        }
        self.lineages.borrow().get(&pid).map(|l| l.identity.birth)
    }
    fn presence(&self, pid: i32) -> ProcessPresence {
        self.presence.get(&pid).copied().unwrap_or_else(|| {
            if self.lineages.borrow().contains_key(&pid) {
                ProcessPresence::Present
            } else {
                ProcessPresence::Absent
            }
        })
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        if self.unreadable.contains(&pid) {
            return None;
        }
        let read = self.lineages.borrow().get(&pid).copied();
        if let Some(left) = self.exiting.borrow_mut().get_mut(&pid) {
            *left = left.saturating_sub(1);
            if *left == 0 {
                self.exits(pid);
            }
        }
        read
    }
    fn members(&self, pgid: u32) -> Option<GroupMembers> {
        self.groups.get(&pgid).map(|pids| GroupMembers {
            pids: pids.clone(),
            complete: !self.partial.contains(&pgid),
        })
    }
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        if !self.lineages.borrow().contains_key(&pid) {
            return None;
        }
        self.exe_moved
            .borrow()
            .get(&pid)
            .or_else(|| self.exes.get(&pid))
            .cloned()
    }
    fn markers(&self, pid: i32) -> Option<Markers> {
        self.markers_read.borrow_mut().push(pid);
        self.markers
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| Some(Markers::default()))
    }
    fn held(&self, pid: i32) -> Option<bool> {
        self.held.get(&pid).copied().unwrap_or(Some(false))
    }
    fn launchd_version(&self) -> Option<i32> {
        Some(1026)
    }
    fn tty_master_held(&self, dev: u32) -> Option<bool> {
        self.ttys.get(&dev).copied().unwrap_or(Some(false))
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        if self.unlisted {
            return None;
        }
        let mut pids: Vec<i32> = self.lineages.borrow().keys().copied().collect();
        pids.sort_unstable();
        Some(pids)
    }
    fn now_ms(&self) -> u64 {
        42
    }
    fn end(&self, token: Identity, still: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        let moved = self.moves.borrow_mut().remove(&token.pid);
        if let Some(moved) = moved {
            self.lineages.borrow_mut().insert(token.pid, moved);
        }
        let exe = self.exe_moves.borrow_mut().remove(&token.pid);
        if let Some(exe) = exe {
            self.exe_moved.borrow_mut().insert(token.pid, exe);
        }
        end_proven(token, Duration::ZERO, &mut Control(self), |id| still(id))
    }
}

fn verify_run(id: &str) -> Option<Markers> {
    Some(Markers {
        verify_run: Some(id.to_owned()),
        ..Markers::default()
    })
}

/// This process in the fake: pid 9000, born after every member below, its parent launchd.
const ME: i32 = 9000;

fn with_me(k: &mut FakeKernel, uniqueid: u64) {
    k.put(lin(ME, uniqueid, 9000, 1), "/usr/local/bin/aterm-verify");
}

/// The dead run every start-pass test reads: launcher 41983 (dead: the fake has no such
/// process), its one stage group led by pid 500 (leader unique id 300), roots `/wt-a` and its
/// scratch.
const DEAD_ID: &str = "41983-999";

fn dead_record() -> String {
    format!(
        "run v=1 boot=B id={DEAD_ID} uniqueid=200 start=1 sid=-\n\
         root path=/wt-a\n\
         root path=/tmp/atv\n\
         group pgid=500 leader_birth={} leader_uniqueid=300 stage=test%20-p%20aterm-link\n",
        born(500)
    )
}

/// A records directory holding `files` (name, text).
fn records(files: &[(&str, &str)]) -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).expect("a record");
    }
    dir
}

/// The start pass over `dir` and `k`, with its lines.
fn start_pass(dir: &Path, k: &FakeKernel) -> (usize, Vec<String>) {
    let mut lines = Vec::new();
    let ended = end_dead_runs(dir, "B", ME, k, &mut |l| lines.push(l.to_owned()));
    (ended, lines)
}

#[test]
fn a_run_id_round_trips() {
    let id = RunId {
        pid: 41983,
        birth: Birth(1_792_163_800_331_520),
    };
    assert_eq!(RunId::parse(&id.text()), Some(id));
    assert_eq!(RunId::parse("nope"), None);
}

#[test]
fn a_record_parses_its_lines_and_skips_the_unknown() {
    let text = "run v=1 boot=B id=7-100 uniqueid=50 start=1 sid=s3\n\
                root path=/w/wt-a\n\
                root path=/tmp/atv.XXXX\n\
                group pgid=200 leader_birth=120 leader_uniqueid=60 stage=test%20-p%20aterm-link\n\
                group pgid=201 leader_birth=130 leader_uniqueid=61 stage=fmt\n\
                closed pgid=201\n\
                future thing=ignored\n\
                dead_seen uniqueid=900 at=5\n";
    let rec = Record::parse(text);
    assert_eq!(
        rec.id,
        Some(RunId {
            pid: 7,
            birth: Birth(100)
        })
    );
    assert_eq!(rec.boot, "B");
    assert_eq!(rec.uniqueid, 50);
    assert_eq!(rec.sid.as_deref(), Some("s3"));
    assert_eq!(
        rec.roots,
        [PathBuf::from("/w/wt-a"), PathBuf::from("/tmp/atv.XXXX")]
    );
    assert_eq!(rec.groups.len(), 2);
    assert_eq!(rec.groups[0].stage, "test -p aterm-link");
    assert!(!rec.groups[0].closed && rec.groups[1].closed, "{rec:?}");
    assert_eq!(rec.dead_seen, Some(900));
    assert_eq!(rec.open_groups().map(|g| g.pgid).collect::<Vec<_>>(), [200]);
}

/// A header reads back what was written across non-ASCII, whitespace, `=`, `%`, and newlines.
#[test]
fn a_header_reads_back_any_path_and_session_id() {
    let roots = vec![
        PathBuf::from("/Users//Jos\u{e9}/src/wt a"),
        PathBuf::from("/tmp/no\u{a0}break\ttab=100%"),
    ];
    let id = RunId {
        pid: 7,
        birth: Birth(100),
    };
    let sid = "s 3\nroot path=/";
    let text = header_text("B", id, 50, 1, Some(sid), &roots);
    assert_eq!(
        text.lines().count(),
        3,
        "one run line and two roots: {text:?}"
    );
    let rec = Record::parse(&text);
    assert_eq!(rec.roots, roots);
    assert_eq!(rec.sid.as_deref(), Some(sid));
    assert_eq!(rec.id, Some(id));
    for s in [
        "test -p aterm-link",
        "a=b c",
        "plain",
        "tab\tkept",
        "Jos\u{e9}",
    ] {
        assert_eq!(pct_decode(&pct_encode(s)), s);
    }
}

#[test]
fn exe_word_is_the_plain_basename_only() {
    assert_eq!(
        exe_word(Path::new("/t/debug/aterm")).as_deref(),
        Some("aterm")
    );
    assert_eq!(
        exe_word(Path::new("/t/git-remote-https")).as_deref(),
        Some("git-remote-https")
    );
    assert_eq!(exe_word(Path::new("/t/weird name")), None);
    assert_eq!(exe_word(Path::new("/")), None);
}

/// A dead run's member that meets every condition is ended, while breaking any single condition leaves it named or outside the run.
#[test]
fn each_condition_of_the_dead_run_pass_names_what_it_cannot_prove() {
    /// A live run (its launcher, pid 7000, alive), claiming group 501 and id `7000-<birth>`.
    fn live_run(k: &mut FakeKernel) -> (String, String) {
        k.put(lin(7000, 1, 7000, 1), "/usr/local/bin/aterm-verify");
        let id = format!("7000-{}", born(7000));
        let rec = format!(
            "run v=1 boot=B id={id} uniqueid=1 start=1 sid=-\nroot path=/wt-b\n\
             group pgid=501 leader_birth=1 leader_uniqueid=1 stage=s\n"
        );
        (id, rec)
    }
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Want {
        Ended,
        Named,
        NotTheRuns,
    }
    type Case = (
        &'static str,
        Want,
        fn(&mut FakeKernel, &mut Lineage) -> Option<String>,
    );
    let cases: [Case; 16] = [
        ("proven", Want::Ended, |_, _| None),
        ("exe outside the roots", Want::Named, |k, _| {
            k.exes
                .insert(71, PathBuf::from("/shared/target/debug/aterm"));
            None
        }),
        ("no exe", Want::Named, |k, _| {
            k.exes.remove(&71);
            None
        }),
        ("its parent lives", Want::Named, |k, m| {
            k.put(lin(600, 1, 600, 1), "/bin/zsh");
            m.parent = 600;
            None
        }),
        ("launchd started it", Want::Named, |_, m| {
            m.launcher_version = Some(1026);
            None
        }),
        ("held at the program check", Want::Named, |k, _| {
            k.held.insert(71, Some(true));
            None
        }),
        ("the program check unreadable", Want::Named, |k, _| {
            k.held.insert(71, None);
            None
        }),
        ("in a live run's group", Want::Named, |k, m| {
            m.pgid = 501;
            k.markers.insert(71, verify_run(DEAD_ID));
            Some(live_run(k).1)
        }),
        ("carrying a live run's id", Want::Named, |k, _| {
            let (id, rec) = live_run(k);
            k.markers.insert(71, verify_run(&id));
            Some(rec)
        }),
        (
            "markers unreadable beside a live run",
            Want::Named,
            |k, _| {
                k.markers.insert(71, None);
                Some(live_run(k).1)
            },
        ),
        ("markers withheld beside a live run", Want::Named, |k, _| {
            k.markers.insert(
                71,
                Some(Markers {
                    withheld: true,
                    ..Markers::default()
                }),
            );
            Some(live_run(k).1)
        }),
        ("its tty's master is held", Want::Named, |k, m| {
            m.tty = Some(77);
            k.ttys.insert(77, Some(true));
            None
        }),
        ("its tty's holders unreadable", Want::Named, |k, m| {
            m.tty = Some(77);
            k.ttys.insert(77, None);
            None
        }),
        (
            "born before its group's leader",
            Want::NotTheRuns,
            |_, m| {
                m.uniqueid = 250;
                None
            },
        ),
        (
            "born after the dead_seen horizon",
            Want::NotTheRuns,
            |_, m| {
                m.uniqueid = 450;
                None
            },
        ),
        (
            "its pgid led again by another process",
            Want::NotTheRuns,
            |k, _| {
                let mut leader = lin(500, 900, 500, 1);
                leader.identity.birth = birth(1);
                k.put(leader, "/usr/bin/other");
                None
            },
        ),
    ];
    for (name, want, change) in cases {
        let mut k = FakeKernel::default();
        with_me(&mut k, 5_000);
        k.put(lin(500, 300, 500, 1), "/usr/local/bin/targo");
        let mut member = lin(71, 310, 500, 1);
        k.exes.insert(71, PathBuf::from("/wt-a/target/debug/aterm"));
        let live = change(&mut k, &mut member);
        k.lineages.get_mut().insert(71, member);
        let dead = format!("{}dead_seen uniqueid=400 at=1\n", dead_record());
        let mut files = vec![("41983-999.run", dead.as_str())];
        if let Some(live) = &live {
            files.push(("7000-8000.run", live.as_str()));
        }
        let dir = records(&files);
        let (ended, lines) = start_pass(dir.path(), &k);
        let got = if k.termed().contains(&71) {
            Want::Ended
        } else if lines
            .iter()
            .any(|l| l.contains("aterm \u{d7}") || l.contains("? \u{d7}"))
        {
            Want::Named
        } else {
            Want::NotTheRuns
        };
        assert_eq!(got, want, "{name}: {lines:?}");
        assert_eq!(ended, usize::from(want == Want::Ended), "{name}");
    }
}

/// The dead run's stage leader itself, orphaned when the run's launcher died, is judged like any member and ended.
#[test]
fn a_dead_runs_stage_leader_is_ended() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(500, 300, 500, 1), "/wt-a/target/debug/deps/aterm_x-1");
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!(k.termed(), [500], "{lines:?}");
    assert_eq!(ended, 1);
    assert!(lines[0].contains("ended aterm_x-1"), "{lines:?}");
}

/// A pass judges a parent before its children, the table oldest first: a dead run's stage
/// leader, alive and orphaned, with a child in its group, is ended first, so the child, which
/// that end orphaned, is ended too and the record goes. NEGATIVE CONTROL: judge in listing
/// order (the child's lower pid first, as a newest-first listing or a wrapped pid gives), and
/// the child is named while its parent lives, and outlives the pass.
#[test]
fn a_pass_judges_a_parent_before_its_children() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(500, 300, 500, 1), "/wt-a/target/debug/aterm");
    k.put(lin(499, 310, 500, 500), "/wt-a/target/debug/aterm");
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!(k.termed(), [500, 499], "{lines:?}");
    assert_eq!(ended, 2);
    assert!(
        !dir.path().join("41983-999.run").exists(),
        "nothing of it left: {lines:?}"
    );
}

/// A process that has exited and waits to be reaped (a zombie) holds nothing: a survivor its
/// SIGTERM leaves a zombie (its adopter has not reaped it yet, or an init that reaps nothing
/// never will) is counted ended, and a member already a zombie when it is read is gone, so the
/// group is proven empty. NEGATIVE CONTROL: count only an absent pid as gone in
/// `may_match_birth`, and both are named as left.
#[test]
fn a_zombie_holds_nothing_and_its_end_counts_ended() {
    let mut k = FakeKernel::default();
    k.put(lin(501, 310, 500, 1), "/wt-a/target/debug/aterm");
    k.groups.insert(500, vec![500, 501, 502]);
    k.presence.insert(501, ProcessPresence::Exited);
    k.presence.insert(502, ProcessPresence::Exited);
    let report = sweep_group(&k, 500, "test -p aterm-link", &[PathBuf::from("/wt-a")]);
    assert_eq!(k.termed(), [501]);
    assert_eq!(report.ended, ["aterm"], "{report:?}");
    assert!(report.named.is_empty() && report.closed(), "{report:?}");
}

/// Records are kept per pid namespace where there are namespaces: a record's pids read only in
/// the namespace that wrote them, and namespaces share a boot. NEGATIVE CONTROL: ignore the
/// namespace, and a container's verify and the host's share one directory.
#[test]
fn records_are_kept_per_pid_namespace() {
    let rendezvous = Path::new("/run/user/501/aterm");
    let host = records_dir(rendezvous, Some("4026531836"));
    let container = records_dir(rendezvous, Some("4026532201"));
    assert_ne!(host, container);
    assert!(host.starts_with(rendezvous.join("verify-runs")), "{host:?}");
    assert_eq!(
        records_dir(rendezvous, None),
        rendezvous.join("verify-runs"),
        "no namespaces"
    );
}

/// A member that left every recorded group (`setsid`) but carries the dead run's id and was born in its window is found and ended.
#[test]
fn a_dead_runs_member_that_left_its_groups_is_found_by_its_id() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(71, 310, 71, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(DEAD_ID));
    k.put(lin(72, 311, 72, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(72, verify_run("55-9"));
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!(k.termed(), [71], "{lines:?}");
    assert_eq!(ended, 1);
    assert!(
        !dir.path().join("41983-999.run").exists(),
        "nothing left named"
    );
}

/// The window's upper bound is a high-water read after the launcher was found dead, recorded in `dead_seen`.
#[test]
fn the_dead_runs_window_closes_at_a_high_water_read_after_its_death() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 350);
    k.put(lin(71, 400, 500, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(DEAD_ID));
    k.put(lin(80, 420, 80, 1), "/usr/bin/ssh-agent");
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (_, lines) = start_pass(dir.path(), &k);
    assert_eq!(k.termed(), [71], "{lines:?}");
}

/// A dead run's record is kept with `dead_seen` appended once while any member remains named or unjudged, and removed once nothing is left.
#[test]
fn a_dead_runs_record_stays_while_it_names_a_member() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(71, 310, 500, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(DEAD_ID));
    k.held.insert(71, Some(true));
    let other_boot = "run v=1 boot=A id=5-1 uniqueid=1 start=1 sid=-\n";
    let dir = records(&[
        ("41983-999.run", &dead_record()),
        ("5-1.run", other_boot),
        ("6-1.run", ""),
    ]);
    let path = dir.path().join("41983-999.run");
    let before: Vec<_> = ["41983-999.run", "5-1.run", "6-1.run"]
        .into_iter()
        .map(|name| {
            let path = dir.path().join(name);
            let bytes = std::fs::read(&path).expect("seed record");
            (path, bytes)
        })
        .collect();
    let (ended, _) = start_pass(dir.path(), &k);
    assert_eq!(ended, 0);
    assert!(k.signals.borrow().is_empty());
    assert!(k.markers_read.borrow().is_empty());
    for (path, bytes) in before {
        assert_eq!(
            std::fs::read(path).expect("unknown snapshot retained"),
            bytes
        );
    }
    std::fs::remove_file(dir.path().join("6-1.run")).expect("retire the incomplete fixture");
    let (_, lines) = start_pass(dir.path(), &k);
    assert!(lines[0].contains("not judged"), "{lines:?}");
    let text = std::fs::read_to_string(&path).expect("the record is kept");
    assert_eq!(
        Record::parse(&text).dead_seen,
        Some(5_000),
        "dead_seen at the high-water: {text}"
    );
    assert!(!dir.path().join("5-1.run").exists(), "another boot's");

    let (_, lines) = start_pass(dir.path(), &k);
    assert!(lines[0].contains("not judged"), "{lines:?}");
    let text = std::fs::read_to_string(&path).expect("the record is kept");
    assert_eq!(
        text.matches("dead_seen").count(),
        1,
        "appended once: {text}"
    );

    k.held.insert(71, Some(false));
    let (ended, _) = start_pass(dir.path(), &k);
    assert_eq!(ended, 1);
    assert!(!path.exists(), "nothing of it left named");
}

/// The pass never ends this process or its ancestors, and a failed uid listing ends nothing and keeps every record.
#[test]
fn the_start_pass_spares_what_it_runs_under_and_keeps_records_it_cannot_judge() {
    let mut k = FakeKernel::default();
    k.put(
        lin(ME, 5_000, ME as u32, 500),
        "/usr/local/bin/aterm-verify",
    );
    k.put(lin(500, 300, 500, 1), "/wt-a/target/debug/deps/aterm_x-1");
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (0, Vec::new()), "{lines:?}");

    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(71, 310, 500, 1), "/wt-a/target/debug/aterm");
    k.unlisted = true;
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (0, Vec::new()), "{lines:?}");
    assert!(lines[0].contains("could not list"), "{lines:?}");
    assert!(dir.path().join("41983-999.run").exists());
}

/// The program check is judged before reading markers so a held program's environment is never read.
#[test]
fn a_held_programs_environment_is_never_read() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(500, 300, 500, 1), "/usr/local/bin/targo");
    k.put(lin(71, 310, 500, 1), "/wt-a/target/debug/aterm");
    k.held.insert(71, Some(true));
    k.put(lin(90, 320, 90, 1), "/wt-a/target/debug/aterm");
    k.held.insert(90, Some(true));
    k.put(lin(91, 321, 91, 1), "/bin/zsh");
    k.held.insert(91, Some(true));
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!(ended, 0, "{lines:?}");
    let read = k.markers_read.borrow().clone();
    assert!(
        !read.contains(&71) && !read.contains(&90) && !read.contains(&91),
        "a held program's environment was read: {read:?}"
    );
    assert_eq!(
        lines[0],
        "the verify run of pid 41983 (/wt-a) left 2 processes: named aterm \u{d7}1, \
         targo \u{d7}1; 1 held at macOS's program check, not judged"
    );
    assert!(
        dir.path().join("41983-999.run").exists(),
        "kept for a later pass"
    );

    k.lineages.get_mut().remove(&71);
    let (_, lines) = start_pass(dir.path(), &k);
    assert_eq!(
        lines[0],
        "the verify run of pid 41983 (/wt-a) left 1 processes: named targo \u{d7}1; \
         1 held at macOS's program check, not judged"
    );
    assert!(
        dir.path().join("41983-999.run").exists(),
        "kept while not judged"
    );
}

/// The live kernel reads a process whose one thread never ran as held whatever the program
/// check's words call it, so the pass above never reads its environment or signals it: a
/// start-suspended child (one thread, handle `0`, young and stopped, which the words call
/// started) reads held, and this process, which runs, does not.
#[cfg(target_os = "macos")]
#[test]
fn the_live_kernel_holds_every_process_whose_thread_never_ran() {
    let child =
        aterm_uds::deadline::Held::start(aterm_uds::spawnfd::command("/bin/sleep").arg("30"))
            .expect("a start-suspended sleep");
    let pid = i32::try_from(child.id()).expect("pid");
    assert_eq!(
        aterm_uds::process::at_program_check(pid),
        Some(false),
        "the words do not hold it"
    );
    assert_eq!(LiveKernel.held(pid), Some(true), "verify leaves it");
    let me = i32::try_from(std::process::id()).expect("pid");
    assert_eq!(LiveKernel.held(me), Some(false), "this process runs");
    drop(child);
}

/// A member proven at the listing that is adopted by a live parent before its signal is not signalled and is named.
#[test]
fn a_member_that_stops_being_proven_before_its_signal_is_left() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(71, 310, 500, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(DEAD_ID));
    k.put(lin(600, 1, 600, 1), "/bin/zsh");
    k.moves.borrow_mut().insert(71, lin(71, 310, 500, 600));
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (0, Vec::new()), "{lines:?}");
    assert!(lines[0].contains("named aterm"), "{lines:?}");
}

/// The stage sweep ends unheld survivors under the roots, names the rest, and records a group `closed` only when listed in full with nothing named.
#[test]
fn the_stage_sweep_ends_under_roots_and_names_the_rest() {
    let roots = vec![PathBuf::from("/wt-a")];
    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/wt-a/target/debug/aterm");
    k.put(lin(72, 301, 500, 500), "/usr/bin/ssh-agent");
    k.put(lin(74, 303, 500, 500), "/wt-a/target/debug/aterm");
    k.held.insert(74, Some(true));
    k.put(lin(75, 304, 500, 500), "/wt-a/target/debug/aterm");
    k.held.insert(75, None);
    k.put(lin(76, 305, 500, 500), "/wt-a/target/debug/aterm");
    k.stubborn.insert(76);
    k.groups.insert(500, vec![500, 71, 72, 73, 74, 75, 76]);
    let report = sweep_group(&k, 500, "test -p aterm-link", &roots);
    assert_eq!(report.ended, ["aterm", "aterm"], "{report:?}");
    assert_eq!(report.named, ["ssh-agent", "aterm", "aterm"], "{report:?}");
    assert_eq!(k.termed(), [71, 76]);
    assert!(
        k.signals.borrow().contains(&(76, SIGKILL)),
        "the stubborn one is killed"
    );
    assert!(!report.closed(), "something is named");
    assert!(report.line().is_some_and(|l| l.contains("3 other named")));

    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/wt-a/target/debug/aterm");
    k.groups.insert(500, vec![500, 71]);
    let report = sweep_group(&k, 500, "s", &roots);
    assert!(report.closed(), "{report:?}");
    let mut k = FakeKernel::default();
    k.groups.insert(500, vec![500]);
    let report = sweep_group(&k, 500, "s", &roots);
    assert!(report.closed() && report.line().is_none(), "{report:?}");
}

/// A group the sweep cannot prove empty stays open across missing or partial listings, refused signals, and survivors that left the group.
#[test]
fn a_group_the_sweep_cannot_prove_empty_stays_open() {
    let roots = vec![PathBuf::from("/wt-a")];
    let k = FakeKernel::default();
    let report = sweep_group(&k, 500, "s", &roots);
    assert!(!report.closed() && report.line().is_none(), "{report:?}");

    let mut k = FakeKernel::default();
    k.groups.insert(500, vec![500]);
    k.partial.insert(500);
    let report = sweep_group(&k, 500, "s", &roots);
    assert!(!report.closed(), "{report:?}");
    assert_eq!(
        report.line().as_deref(),
        Some(
            "stage s: its group's listing was partial, so it may have left processes not named here"
        ),
        "{report:?}"
    );

    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/wt-a/target/debug/aterm");
    k.refused.insert(71);
    k.groups.insert(500, vec![500, 71]);
    let report = sweep_group(&k, 500, "s", &roots);
    assert_eq!(report.named, ["aterm"], "{report:?}");
    assert!(!report.closed());

    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/wt-a/target/debug/aterm");
    k.moves.borrow_mut().insert(71, lin(71, 300, 71, 500));
    k.groups.insert(500, vec![500, 71]);
    let report = sweep_group(&k, 500, "s", &roots);
    assert_eq!(
        (report.named.len(), k.termed()),
        (1, Vec::new()),
        "{report:?}"
    );
}

/// Run roots include the worktree first and every target or build directory cargo reports.
#[test]
fn the_roots_are_where_the_build_put_its_executables() {
    let cache = "/cache/cargo-build.noindex/wt-a/.cargo-build";
    let metadata = format!(
        "{{\"packages\":[],\"workspace_root\":\"/wt-a\",\
         \"target_directory\":\"/shared/target\",\"build_directory\":\"{cache}\",\
         \"version\":1}}"
    );
    let built = parse_build_dirs(&format!("targo: a lane banner\n{metadata}\n"));
    assert_eq!(
        built,
        [PathBuf::from("/shared/target"), PathBuf::from(cache)]
    );
    let roots = run_roots(
        Path::new("/wt-a"),
        Path::new("/tmp/atv.scratch"),
        Path::new("/shared/target"),
        &built,
    );
    assert_eq!(
        roots[0],
        PathBuf::from("/wt-a"),
        "the worktree first: {roots:?}"
    );

    let mut k = FakeKernel::default();
    k.put(
        lin(71, 300, 500, 500),
        &format!("{cache}/debug/deps/aterm_agent-336f0d9f2ce88bd5"),
    );
    k.put(lin(72, 301, 500, 500), "/shared/target/debug/aterm");
    k.put(lin(73, 302, 500, 500), "/usr/bin/ssh-agent");
    k.groups.insert(500, vec![500, 71, 72, 73]);
    let label = format!("{cache}/debug/deps/aterm_agent-336f0d9f2ce88bd5");
    let report = sweep_group(&k, 500, &label, &roots);
    assert_eq!(
        report.ended,
        ["aterm_agent-336f0d9f2ce88bd5", "aterm"],
        "what runs from the build's own directories is ended: {report:?} over {roots:?}"
    );
    assert_eq!(report.named, ["ssh-agent"]);
    assert_eq!(k.termed(), [71, 72]);

    let rec = Record {
        id: Some(RunId {
            pid: 7,
            birth: Birth(1),
        }),
        roots: run_roots(
            Path::new("/wt-a"),
            Path::new("/tmp/s"),
            Path::new("/wt-a/target"),
            &[PathBuf::from(
                "/Library/Caches/aterm/cargo-build.noindex/wt-a",
            )],
        ),
        ..Record::default()
    };
    assert!(
        dead_run_line(&rec, &Left::default()).contains("(/wt-a)"),
        "{rec:?}"
    );

    assert_eq!(
        parse_build_dirs("{\"target_directory\":\"/w/target\"}"),
        [PathBuf::from("/w/target")]
    );
    assert!(parse_build_dirs("warning: not json").is_empty());
}

/// The build-directory question is asked with the main lane's build-dir `--config`.
#[test]
fn the_build_dirs_are_asked_as_the_main_lane_asks() {
    let mut ctx = Ctx::new(
        PathBuf::from("/repo"),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    );
    let value = "build.build-dir=\"/cache/cargo-build.noindex/repo/.cargo-build\"".to_owned();
    ctx.main_build_dir = Some(value.clone());
    let ask = metadata_ask(&ctx);
    let lane = crate::stages::main_targo(&ctx, vec!["test".to_owned()]);
    assert_eq!(ask.program, lane.program);
    assert_eq!(
        ask.args[..2],
        [std::ffi::OsString::from("--config"), value.into()],
        "{ask:?}"
    );
    assert_eq!(ask.args[..2], lane.args[..2], "the main lane's own config");
    assert!(ask.args.iter().any(|a| a == "metadata"), "{ask:?}");
}

/// A root yet to be created under a symlinked prefix resolves to its kernel path so executables later placed there match.
#[cfg(unix)]
#[test]
fn a_root_made_after_the_run_started_still_matches() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let real = std::fs::canonicalize(dir.path())
        .expect("canonical")
        .join("real");
    std::fs::create_dir(&real).expect("real");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let target = link.join("ct");
    let roots = run_roots(Path::new("/wt-a"), Path::new("/tmp/s"), &target, &[]);
    assert!(roots.contains(&real.join("ct")), "{roots:?}");
    let exe = real.join("ct/debug/aterm");
    std::fs::create_dir_all(exe.parent().expect("a dir")).expect("the target");
    std::fs::write(&exe, "").expect("the broker");
    assert!(under_any(&exe, &roots), "{exe:?} under {roots:?}");
    assert!(
        under_any(&exe, &[target]),
        "a raw root reads as its kernel path"
    );
}

/// A member claimed by a live run's group or id is never ended by another run's start.
#[test]
fn a_member_a_live_run_claims_is_spared() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(7000, 1, 7000, 1), "/usr/local/bin/aterm-verify");
    let live = format!(
        "run v=1 boot=B id=7000-{} uniqueid=1 start=1 sid=-\nroot path=/wt-a\n\
         group pgid=500 leader_birth=1 leader_uniqueid=1 stage=s\n",
        born(7000)
    );
    k.put(lin(71, 310, 500, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(DEAD_ID));
    k.put(lin(7100, 0, 7100, 1), "/usr/local/bin/aterm-verify");
    let header_only = "run v=1 boot=B id=7100-8100 uniqueid=0 start=0 sid=-\n";
    let dir = records(&[
        ("41983-999.run", &dead_record()),
        ("7000-8000.run", &live),
        ("7100-8100.run", header_only),
    ]);
    let live_path = dir.path().join("7000-8000.run");
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (0, Vec::new()), "{lines:?}");
    assert!(live_path.exists(), "a live run's record");

    let retained = || {
        ["41983-999.run", "7000-8000.run", "7100-8100.run"]
            .into_iter()
            .map(|name| {
                let path = dir.path().join(name);
                let bytes = std::fs::read(&path).expect("record before refused pass");
                (path, bytes)
            })
            .collect::<Vec<_>>()
    };
    let unknown_keeps_claims = |kernel: &FakeKernel| {
        let before = retained();
        kernel.markers_read.borrow_mut().clear();
        let (ended, _) = start_pass(dir.path(), kernel);
        assert_eq!(ended, 0);
        assert!(kernel.signals.borrow().is_empty());
        assert!(kernel.markers_read.borrow().is_empty());
        for (path, bytes) in before {
            assert_eq!(
                std::fs::read(path).expect("unadmitted record is retained"),
                bytes
            );
        }
    };
    let mut invalid_utf8 = live.as_bytes().to_vec();
    invalid_utf8.extend_from_slice(b"\xff\n");
    let header = live.lines().next().expect("live header");
    for bytes in [
        Vec::new(),
        invalid_utf8,
        b"run v=1 boot=B id=7000-8".to_vec(),
        header.as_bytes().to_vec(),
        format!("{header}\nroot path=/wt-a\ngroup pgid=50").into_bytes(),
        format!("{header}\ngroup pgid=500 leader_birth=1\n").into_bytes(),
        live.replacen("id=7000-8000", "id=7000-8", 1).into_bytes(),
        live.replacen("uniqueid=1", "uniqueid=broken", 1)
            .into_bytes(),
        live.replacen(" uniqueid=1", "", 1).into_bytes(),
        live.replacen(" start=1", " uniqueid=1 start=1", 1)
            .into_bytes(),
        format!("{live}{header}\n").into_bytes(),
        live.replacen("v=1", "v=2", 1).into_bytes(),
        live.replace("pgid=500", "pgid=broken").into_bytes(),
        live.replace("pgid=500", "pgid=500 pgid=500").into_bytes(),
        live.replace("leader_birth=1", "leader_birth=broken")
            .into_bytes(),
        live.replace("path=/wt-a", "path=/wt%G0").into_bytes(),
        live.replace("sid=-", "sid=%FF").into_bytes(),
        live.replace("stage=s", "stage=%GG").into_bytes(),
        live.replace("stage=s", "stage=%FF").into_bytes(),
        format!("{live}root\n").into_bytes(),
        format!("{live}closed pgid=broken\n").into_bytes(),
        format!("{live}closed pgid=500 pgid=500\n").into_bytes(),
        format!("{live}dead_seen uniqueid=broken at=1\n").into_bytes(),
        format!("{live}dead_seen at=1 uniqueid=900\n").into_bytes(),
        format!("{live}end at=broken\n").into_bytes(),
    ] {
        std::fs::write(&live_path, bytes).expect("publish incomplete or unreadable fixture");
        unknown_keeps_claims(&k);
    }
    std::fs::write(&live_path, &live).expect("restore the complete live claim");
    let mismatched = dir.path().join("7000-8001.run");
    std::fs::rename(&live_path, &mismatched).expect("mismatched record name");
    let before = std::fs::read(&mismatched).expect("mismatched bytes");
    let dead_before = std::fs::read(dir.path().join("41983-999.run")).expect("dead record");
    let other_before = std::fs::read(dir.path().join("7100-8100.run")).expect("other live record");
    k.markers_read.borrow_mut().clear();
    let (ended, _) = start_pass(dir.path(), &k);
    assert_eq!(ended, 0);
    assert!(k.signals.borrow().is_empty());
    assert!(k.markers_read.borrow().is_empty());
    assert_eq!(std::fs::read(&mismatched).expect("kept mismatch"), before);
    assert_eq!(
        std::fs::read(dir.path().join("41983-999.run")).expect("kept dead record"),
        dead_before
    );
    assert_eq!(
        std::fs::read(dir.path().join("7100-8100.run")).expect("kept other live record"),
        other_before
    );
    std::fs::rename(&mismatched, &live_path).expect("restore canonical name");

    let non_file = dir.path().join("7200-8200.run");
    std::fs::create_dir(&non_file).expect("non-regular candidate");
    unknown_keeps_claims(&k);
    assert!(non_file.is_dir());
    std::fs::remove_dir(&non_file).expect("retire private non-file fixture");
    let escaped = live
        .replace("sid=-", "sid=session%20one%25%3D")
        .replace("stage=s", "stage=work%20with%25%3D")
        .replace(
            "root path=/wt-a\n",
            "root path=/wt-a\nroot path=/with%20space%25%3D\n",
        );
    std::fs::write(&live_path, escaped).expect("complete escaped live claim");
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (0, Vec::new()), "{lines:?}");

    k.birth_unreadable.insert(7000);
    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        k.presence.insert(7000, presence);
        let before = std::fs::read(&live_path).expect("live record");
        let (ended, lines) = start_pass(dir.path(), &k);
        assert_eq!(
            (ended, k.termed()),
            (0, Vec::new()),
            "{presence:?}: {lines:?}"
        );
        assert_eq!(
            std::fs::read(&live_path).expect("claims retained"),
            before,
            "{presence:?}: an unreadable launcher is not declared dead"
        );
    }

    k.lineages.borrow_mut().remove(&7000);
    k.presence.remove(&7000);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (1, vec![71]), "{lines:?}");
}

/// The run-end sweep ends only this run's proven orphans.
#[test]
fn the_run_end_sweep_ends_this_runs_own_orphans() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("41983-1.run");
    std::fs::write(
        &path,
        "run v=1 boot=B id=41983-1 uniqueid=100 start=1 sid=-\n\
         group pgid=500 leader_birth=1 leader_uniqueid=1 stage=test%20-p%20aterm-link\n\
         group pgid=501 leader_birth=1 leader_uniqueid=1 stage=fmt\n",
    )
    .expect("seed");
    let runs = Runs {
        log: Mutex::new(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open"),
        ),
        path: path.clone(),
        boot: "B".to_owned(),
        id: RunId {
            pid: 41983,
            birth: Birth(1),
        },
        uniqueid: 100,
        roots: vec![PathBuf::from("/wt-a")],
        said: Mutex::new(Vec::new()),
        may_forget: AtomicBool::new(false),
    };
    let mut k = FakeKernel::default();
    let mut verify = lin(41983, 100, 41983, 1);
    verify.identity.birth = birth(1);
    k.put(verify, "/usr/local/bin/aterm-verify");
    let ours = || verify_run("41983-1");
    k.put(lin(71, 300, 71, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, ours());
    k.put(lin(72, 301, 72, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(72, verify_run("55-9"));
    k.put(lin(73, 302, 73, 1), "/usr/bin/ssh-agent");
    k.markers.insert(73, ours());
    k.put(lin(600, 1, 600, 1), "/bin/zsh");
    k.put(lin(74, 303, 74, 600), "/wt-a/target/debug/aterm");
    k.markers.insert(74, ours());
    k.put(lin(75, 304, 75, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(75, ours());
    k.held.insert(75, Some(true));
    let mut on_tty = lin(76, 305, 76, 1);
    on_tty.tty = Some(77);
    k.put(on_tty, "/wt-a/target/debug/aterm");
    k.markers.insert(76, ours());
    k.ttys.insert(77, Some(true));
    k.put(lin(77, 50, 77, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(77, ours());

    runs.settle_group(
        500,
        &GroupReport {
            stage: "test -p aterm-link".into(),
            named: vec!["ssh-agent".into()],
            listed: Some(true),
            ..GroupReport::default()
        },
    );
    runs.settle_group(
        501,
        &GroupReport {
            stage: "fmt".into(),
            listed: Some(true),
            ..GroupReport::default()
        },
    );
    let unknown = dir.path().join("7000-8000.run");
    let partial: &[u8] = b"run v=1 boot=B id=7000-8";
    std::fs::write(&unknown, partial).expect("partial competing record");
    let before = std::fs::read(&path).expect("own record before unknown sweep");
    let _ = runs.end_of_run(&k);
    runs.finish(&k);
    assert!(k.signals.borrow().is_empty());
    assert!(k.markers_read.borrow().is_empty());
    assert_eq!(
        std::fs::read(&path).expect("unknown keeps own record"),
        before
    );
    assert_eq!(
        std::fs::read(&unknown)
            .expect("unknown record kept")
            .as_slice(),
        partial
    );
    std::fs::remove_file(&unknown).expect("retire private partial record");

    let lines = runs.end_of_run(&k);
    assert_eq!(k.termed(), [71], "{lines:?}");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with("stage test -p aterm-link left 1"),
        "{lines:?}"
    );
    assert_eq!(
        lines[1],
        "the run left 4 processes: ended aterm \u{d7}1; named aterm \u{d7}2, ssh-agent \u{d7}1; \
         1 held at macOS's program check, not judged",
        "{lines:?}"
    );
    let before = std::fs::read(&path).expect("own record before leftover finish");
    runs.finish(&k);
    assert_eq!(
        std::fs::read(&path).expect("named and unjudged members keep own record"),
        before
    );

    for pid in [73, 74, 75, 76] {
        k.lineages.borrow_mut().remove(&pid);
    }
    let _ = runs.end_of_run(&k);
    std::fs::write(&unknown, b"\xff\n").expect("new uncertainty after a clear sweep");
    let before = std::fs::read(&path).expect("own record after clear sweep");
    let signals_before = k.signals.borrow().clone();
    k.markers_read.borrow_mut().clear();
    let _ = runs.end_of_run(&k);
    runs.finish(&k);
    assert_eq!(*k.signals.borrow(), signals_before);
    assert!(k.markers_read.borrow().is_empty());
    assert_eq!(
        std::fs::read(&path).expect("unknown clears earlier forget permission"),
        before
    );
    assert_eq!(
        std::fs::read(&unknown)
            .expect("unknown bytes kept")
            .as_slice(),
        &b"\xff\n"[..]
    );
    std::fs::remove_file(&unknown).expect("retire private unreadable record");
    let _ = runs.end_of_run(&k);
    runs.finish(&k);
    assert!(
        !path.exists(),
        "a fresh clear sweep permits the orderly end"
    );
}

/// Only a group proven empty is written `closed` in the run's own record.
#[test]
fn only_a_group_proven_empty_is_recorded_closed() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("x.run");
    let mut seed = "run v=1 boot=B id=1-1 uniqueid=1 start=1 sid=-\n".to_owned();
    for pgid in 500..504 {
        seed.push_str(&format!(
            "group pgid={pgid} leader_birth=1 leader_uniqueid=1 stage=s\n"
        ));
    }
    std::fs::write(&path, seed).expect("seed");
    let runs = Runs {
        log: Mutex::new(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open"),
        ),
        path: path.clone(),
        boot: "B".to_owned(),
        id: RunId {
            pid: 1,
            birth: Birth(1),
        },
        uniqueid: 1,
        roots: Vec::new(),
        said: Mutex::new(Vec::new()),
        may_forget: AtomicBool::new(false),
    };
    for (pgid, listed, named) in [
        (500, Some(true), vec![]),
        (501, None, vec![]),
        (502, Some(false), vec![]),
        (503, Some(true), vec!["aterm".to_owned()]),
    ] {
        runs.settle_group(
            pgid,
            &GroupReport {
                stage: "s".into(),
                named,
                listed,
                ..GroupReport::default()
            },
        );
    }
    let rec = Record::parse(&std::fs::read_to_string(&path).expect("read"));
    let closed: Vec<u32> = rec
        .groups
        .iter()
        .filter(|g| g.closed)
        .map(|g| g.pgid)
        .collect();
    assert_eq!(closed, [500], "{rec:?}");
}

/// A dead run's group is a witness only while its leader still reads with the recorded birth, whereas a member carrying the run's id is still the run's.
#[test]
fn a_group_whose_leader_is_gone_proves_no_member() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(71, 310, 500, 1), "/wt-a/target/debug/aterm");
    k.put(lin(72, 311, 500, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(72, verify_run(DEAD_ID));
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (1, vec![72]), "{lines:?}");

    let unread = format!(
        "run v=1 boot=B id={DEAD_ID} uniqueid=200 start=1 sid=-\nroot path=/wt-a\n\
         group pgid=500 leader_birth=0 leader_uniqueid=0 stage=s\n"
    );
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(71, 5, 500, 1), "/wt-a/target/debug/aterm");
    let dir = records(&[("41983-999.run", &unread)]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (0, Vec::new()), "{lines:?}");
}

/// A `closed` line closes only the group recorded before it.
#[test]
fn a_closed_line_closes_only_the_group_before_it() {
    let text = "run v=1 boot=B id=7-100 uniqueid=50 start=1 sid=-\n\
                group pgid=500 leader_birth=120 leader_uniqueid=60 stage=a\n\
                closed pgid=500\n\
                group pgid=500 leader_birth=900 leader_uniqueid=800 stage=b\n";
    let rec = Record::parse(text);
    let open: Vec<_> = rec
        .open_groups()
        .map(|g| (g.pgid, g.stage.clone()))
        .collect();
    assert_eq!(open, [(500, "b".to_owned())], "{rec:?}");
}

/// A live run's launcher is spared even when its environment inherited an outer dead run's id.
#[test]
fn a_live_runs_launcher_is_never_a_dead_runs_member() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(
        lin(7000, 310, 7000, 1),
        "/wt-a/target/debug/deps/aterm_verify-1",
    );
    k.markers.insert(7000, verify_run(DEAD_ID));
    let inner = format!(
        "run v=1 boot=B id=7000-{} uniqueid=310 start=1 sid=-\nroot path=/wt-a\n",
        born(7000)
    );
    let dir = records(&[("41983-999.run", &dead_record()), ("7000-8000.run", &inner)]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (0, Vec::new()), "{lines:?}");
}

/// A live run claims only the groups it has not closed.
#[test]
fn a_group_a_live_run_closed_claims_nothing() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(7000, 1, 7000, 1), "/usr/local/bin/aterm-verify");
    let live = format!(
        "run v=1 boot=B id=7000-{} uniqueid=1 start=1 sid=-\nroot path=/wt-b\n\
         group pgid=600 leader_birth=1 leader_uniqueid=1 stage=s\nclosed pgid=600\n",
        born(7000)
    );
    k.put(lin(71, 310, 600, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(DEAD_ID));
    let dir = records(&[("41983-999.run", &dead_record()), ("7000-8000.run", &live)]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (1, vec![71]), "{lines:?}");
}

/// A member listed in the group whose lineage does not read while its birth still does is named and keeps its group open.
#[test]
fn a_member_the_sweep_cannot_read_keeps_its_group_open() {
    let roots = vec![PathBuf::from("/wt-a")];
    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/wt-a/target/debug/aterm");
    k.unreadable.insert(71);
    k.groups.insert(500, vec![500, 71]);
    let report = sweep_group(&k, 500, "s", &roots);
    assert_eq!(
        (report.named.len(), k.termed()),
        (1, Vec::new()),
        "{report:?}"
    );
    assert!(!report.closed(), "{report:?}");

    k.birth_unreadable.insert(71);
    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        k.presence.insert(71, presence);
        let report = sweep_group(&k, 500, "s", &roots);
        assert_eq!(
            (report.named.len(), k.termed()),
            (1, Vec::new()),
            "{report:?}"
        );
        assert!(!report.closed(), "{presence:?}: {report:?}");

        k.unreadable.remove(&71);
        k.refused.insert(71);
        let report = sweep_group(&k, 500, "s", &roots);
        assert_eq!(report.named, ["aterm"], "{presence:?}: {report:?}");
        assert!(report.ended.is_empty() && !report.closed(), "{report:?}");
        k.unreadable.insert(71);
    }

    k.lineages.borrow_mut().remove(&71);
    k.presence.remove(&71);
    let report = sweep_group(&k, 500, "s", &roots);
    assert!(report.closed() && report.named.is_empty(), "{report:?}");
}

/// A member that exits on its own while being read is treated as gone rather than named.
#[test]
fn a_member_that_exits_on_its_own_mid_read_is_gone() {
    let roots = vec![PathBuf::from("/wt-a")];
    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/usr/bin/ssh-agent");
    k.put(lin(72, 301, 500, 500), "/wt-a/target/debug/aterm");
    k.exiting.get_mut().extend([(71, 1), (72, 1)]);
    k.groups.insert(500, vec![500, 71, 72]);
    let report = sweep_group(&k, 500, "s", &roots);
    assert!(
        report.named.is_empty() && report.ended.is_empty() && report.closed(),
        "{report:?}"
    );

    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(71, 310, 71, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(DEAD_ID));
    k.exiting.get_mut().insert(71, 2);
    let dir = records(&[("41983-999.run", &dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!(ended, 0, "{lines:?}");
    assert!(!lines[0].contains("named"), "{lines:?}");
    assert!(!dir.path().join("41983-999.run").exists(), "{lines:?}");
}

/// A survivor that outlives its SIGKILL is named and keeps its group open.
#[test]
fn a_survivor_still_there_after_its_sigkill_is_named() {
    let roots = vec![PathBuf::from("/wt-a")];
    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/wt-a/target/debug/aterm");
    k.stubborn.insert(71);
    k.wedged.insert(71);
    k.groups.insert(500, vec![500, 71]);
    let report = sweep_group(&k, 500, "s", &roots);
    assert!(k.signals.borrow().contains(&(71, SIGKILL)), "{report:?}");
    assert_eq!(report.named, ["aterm"], "{report:?}");
    assert!(report.ended.is_empty() && !report.closed(), "{report:?}");
}

/// A survivor that exits just before its SIGTERM lands (`ESRCH`) is neither ended nor named, and its group closes.
#[test]
fn a_survivor_gone_as_its_signal_lands_is_gone() {
    let roots = vec![PathBuf::from("/wt-a")];
    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/wt-a/target/debug/aterm");
    k.vanish.insert(71);
    k.groups.insert(500, vec![500, 71]);
    let report = sweep_group(&k, 500, "s", &roots);
    assert!(
        report.ended.is_empty() && report.named.is_empty() && report.closed(),
        "{report:?}"
    );
}

/// `Runs::open` queries cargo metadata once with the run's child environment and records each directory cargo names as a root after the worktree.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_runs_record_roots_are_the_dirs_its_build_names() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let cache = "/cache/cargo-build.noindex/wt-a/.cargo-build";
    let mut ctx = Ctx::new(
        PathBuf::from("/wt-a"),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/atv-s"),
    );
    ctx.main_build_dir = Some(format!("build.build-dir=\"{cache}\""));
    let want_cmd = metadata_ask(&ctx);
    let want_env = ctx.child_env_add.clone();
    let mut asked = 0;
    let runs = Runs::open(&ctx, dir.path(), |cmd, env| {
        asked += 1;
        assert_eq!(cmd, &want_cmd, "the main lane's question");
        assert_eq!(
            env.add_env,
            want_env.as_slice(),
            "the run's child environment"
        );
        crate::exec::Run {
            ok: true,
            output: format!(
                "targo: a lane banner\n{{\"target_directory\":\"/shared/target\",\
                 \"build_directory\":\"{cache}\",\"version\":1}}\n"
            ),
            code: Some(0),
            spawn_error: None,
        }
    })
    .expect("a record");
    assert_eq!(asked, 1);
    let rec = Record::parse(&std::fs::read_to_string(&runs.path).expect("the record"));
    assert_eq!(rec.roots[0], canonical(Path::new("/wt-a")), "{rec:?}");
    for root in [cache, "/shared/target"] {
        assert!(
            rec.roots.contains(&canonical(Path::new(root))),
            "{root} is a root: {rec:?}"
        );
    }
}

/// A dead run's record as Linux writes it: every birth and unique id is a start tick. The run
/// started at tick 200, its stage group's leader at 300, and the first reader that found it dead
/// saw tick 400.
const LINUX_DEAD_ID: &str = "41983-200";

fn linux_dead_record() -> String {
    format!(
        "run v=1 boot=B id={LINUX_DEAD_ID} uniqueid=200 start=1 sid=-\n\
         root path=/wt-a\n\
         group pgid=500 leader_birth=300 leader_uniqueid=300 stage=test%20-p%20aterm-link\n\
         dead_seen uniqueid=400 at=1\n"
    )
}

/// W-group and W-run by start ticks, and Linux's orphan rule. A member born in the leader's
/// own tick is inside the group's window and is ended; one that left the group carrying the
/// run's id, adopted by a subreaper born before the run, is ended; one whose parent was born in
/// the run's own tick (the run's, never an adopter) is named; one born the tick after
/// `dead_seen` is not the run's. NEGATIVE CONTROLS: `<=` for `<` in `Lineage::orphaned`, and the
/// member whose parent shares the run's tick is ended; widen the windows past `dead_seen`, and
/// the late member is ended.
#[test]
fn a_linux_dead_run_ends_its_members_by_start_ticks() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin_linux(500, 300, 500, 1), "/usr/local/bin/targo");
    k.put(lin_linux(501, 300, 500, 1), "/wt-a/target/debug/aterm");
    k.put(lin_linux(7000, 5, 7000, 1), "/usr/lib/systemd/systemd");
    k.put(lin_linux(502, 310, 502, 7000), "/wt-a/target/debug/aterm");
    k.markers.insert(502, verify_run(LINUX_DEAD_ID));
    k.put(lin_linux(504, 200, 504, 1), "/bin/sh");
    k.put(lin_linux(503, 320, 500, 504), "/wt-a/target/debug/aterm");
    k.put(lin_linux(505, 401, 500, 1), "/wt-a/target/debug/aterm");
    let dir = records(&[("41983-200.run", &linux_dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!(k.termed(), [501, 502], "{lines:?}");
    assert_eq!(ended, 2);
    assert!(
        lines[0].contains("named aterm \u{d7}1, targo \u{d7}1"),
        "the member whose parent the run started is named: {lines:?}"
    );
    assert!(
        k.lineages.borrow().contains_key(&505),
        "born after dead_seen: not the run's"
    );
}

/// Linux reads no terminal's holders (A7), so a member with a controlling terminal is named,
/// never ended. NEGATIVE CONTROL: read an unread terminal scan as free (`!= Some(true)` for
/// `== Some(false)`), and it is ended.
#[test]
fn a_linux_member_with_a_terminal_is_named() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    let mut member = lin_linux(71, 310, 71, 1);
    member.tty = Some(34817);
    k.put(member, "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(LINUX_DEAD_ID));
    k.ttys.insert(34817, None);
    let dir = records(&[("41983-200.run", &linux_dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (0, Vec::new()), "{lines:?}");
    assert!(lines[0].contains("named aterm"), "{lines:?}");
}

/// With no pid version, an exec under the same pid and start is the same identity, so only the
/// re-judgment before the signal sees that it left the roots: it is named, never signalled.
/// NEGATIVE CONTROL: let the judgment before each signal always hold, and it is SIGTERMed.
#[test]
fn a_versionless_member_that_execd_out_of_the_roots_is_not_signalled() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin_linux(71, 310, 71, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(LINUX_DEAD_ID));
    k.exe_moves
        .borrow_mut()
        .insert(71, PathBuf::from("/usr/bin/sleep"));
    let dir = records(&[("41983-200.run", &linux_dead_record())]);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert!(k.termed().is_empty(), "{lines:?}");
    assert_eq!(ended, 0);
    assert!(lines[0].contains("named aterm"), "{lines:?}");
}

/// Selects the stage survivor's stand-in: set, this test binary is the survivor, and its value
/// is the scratch directory it writes `pid` to and waits for `stop` in.
const SURVIVOR_ENV: &str = "ATERM_VERIFY_SURVIVOR_CHILD";

/// A wait for something that must happen.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// The stage survivor: past `main`, so it can never be held at a program check, it writes
/// `<pid> <birth>` and waits for `stop`, or for its directory to go (a failed test's scratch is
/// removed as it unwinds), or the hang detector. Without [`SURVIVOR_ENV`] it returns at once.
#[test]
fn a_stage_survivor_stand_in() {
    let Some(dir) = std::env::var_os(SURVIVOR_ENV) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let me = i32::try_from(std::process::id()).expect("a pid");
    let birth = read_birth(me).expect("this process reads its birth");
    let written = dir.join("pid.tmp");
    std::fs::write(&written, format!("{me} {}\n", birth.0)).expect("the pid file");
    std::fs::rename(&written, dir.join("pid")).expect("the pid file, whole");
    let deadline = std::time::Instant::now() + HANG_DETECTOR;
    while dir.is_dir() && !dir.join("stop").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A stage records its group's sweep, as the run's record does, under roots of its own.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug)]
struct SweepProbe {
    roots: Vec<PathBuf>,
    report: Mutex<Option<GroupReport>>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl crate::exec::StageGroupEnd for SweepProbe {
    fn group_made(&self, _pgid: u32, _label: &str) {}
    fn group_ended(&self, pgid: u32, label: &str) {
        let report = sweep_group(&LiveKernel, pgid, label, &self.roots);
        *self.report.lock().expect("the probe's report") = Some(report);
    }
}

/// A real stage under verify's group custody: a shell that backgrounds this test binary as the
/// survivor stand-in and exits once the survivor wrote its pid. At the stage's end the leader is
/// exited and unreaped, the group is listed off the kernel, and the survivor, running from the
/// roots, is ended through `end_proven` and the kernel's own reads, and the group reads closed
/// where a listing can be whole (on a `/proc` mounted with `hidepid` it never is); under roots
/// it does not run from, it is named and left, and leaves on its own. Under a binary translator
/// the kernel names the translator as the survivor's executable, so it runs from no root and the
/// test says it is not run. NEGATIVE CONTROL: make `sweep_group`'s held conjunct
/// `== Some(true)`, and the survivor is named, not ended.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_stage_survivor_running_from_the_roots_is_ended_at_its_stage_end() {
    if let Some(translator) = aterm_uds::process::translator() {
        println!(
            "NOT RUN under {translator}: runs::tests::\
             a_stage_survivor_running_from_the_roots_is_ended_at_its_stage_end: the kernel names \
             the translator as the survivor's executable, so it runs from no root and is named"
        );
        return;
    }
    let exe = std::env::current_exe().expect("this test binary");
    let word = exe_word(&exe).expect("a test binary's word");
    let stage = |dir: &Path, roots: Vec<PathBuf>| -> (GroupReport, i32, Birth) {
        let probe = SweepProbe {
            roots,
            report: Mutex::new(None),
        };
        let cmd = crate::exec::Cmd::new("/bin/sh")
            .arg("-c")
            .arg(
                "\"$0\" --exact runs::tests::a_stage_survivor_stand_in --nocapture & \
                 while [ ! -s \"$1/pid\" ]; do sleep 0.05; done",
            )
            .arg(&exe)
            .arg(dir)
            .env(SURVIVOR_ENV, dir)
            .capture(crate::exec::Capture::Silent);
        let env = crate::exec::ExecEnv {
            cwd: dir,
            path: std::ffi::OsStr::new("/usr/bin:/bin"),
            scratch: dir,
            child_ceiling: Some(HANG_DETECTOR),
            remove_env: &[],
            add_env: &[],
            stage_groups: Some(&probe),
        };
        let run = crate::exec::run(&cmd, env);
        assert!(run.ok, "the stage: {}", run.output);
        let written = std::fs::read_to_string(dir.join("pid")).expect("the survivor's pid");
        let (pid, birth) = written
            .split_once(' ')
            .and_then(|(pid, birth)| Some((pid.parse().ok()?, Birth(birth.trim().parse().ok()?))))
            .unwrap_or_else(|| panic!("`<pid> <birth>`: {written:?}"));
        let report = probe
            .report
            .lock()
            .expect("the probe's report")
            .take()
            .expect("the stage's group was swept at its end");
        (report, pid, birth)
    };
    let gone = |pid: i32, birth: Birth| {
        let deadline = std::time::Instant::now() + HANG_DETECTOR;
        loop {
            let left = match read_birth(pid) {
                Some(now) => now != birth,
                None => process_presence(pid) != ProcessPresence::Present,
            };
            if left {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pid {pid} outlived its hang detector"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    let dir = aterm_tempfile::tempdir().expect("scratch");
    let roots = vec![canonical(exe.parent().expect("the binary's directory"))];
    let (report, pid, birth) = stage(dir.path(), roots);
    std::fs::write(dir.path().join("stop"), "").expect("stop");
    assert_eq!(report.ended, std::slice::from_ref(&word), "{report:?}");
    assert_eq!(
        report.closed(),
        aterm_uds::process::group_listing_can_be_whole(),
        "closed exactly where a listing can be whole: {report:?}"
    );
    gone(pid, birth);

    let dir = aterm_tempfile::tempdir().expect("scratch");
    let elsewhere = aterm_tempfile::tempdir().expect("roots that hold no executable");
    let (report, pid, birth) = stage(dir.path(), vec![canonical(elsewhere.path())]);
    std::fs::write(dir.path().join("stop"), "").expect("stop");
    assert_eq!(report.named, [word], "{report:?}");
    assert!(report.ended.is_empty() && !report.closed(), "{report:?}");
    gone(pid, birth);
}
