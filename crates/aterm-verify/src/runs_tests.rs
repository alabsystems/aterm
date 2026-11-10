// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super::runs`].

use super::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use aterm_uds::process::{
    Birth, EndControl, GroupMembers, HeldNow, Lineage, Markers, ProcessPresence, end_proven,
    process_presence,
};

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

#[cfg(unix)]
#[test]
fn a_notice_reply_survives_the_instance_closing_before_the_send_returns() {
    use std::io::{Read as _, Write as _};
    let (stream, mut instance) = aterm_uds::CtlStream::pair().unwrap();
    let must = aterm_spec::wait::MUST_HAPPEN;
    instance.set_read_timeout(Some(must)).unwrap();
    let (closed, closure) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        let mut request = [0; 6];
        instance.read_exact(&mut request).unwrap();
        assert_eq!(&request, b"notice");
        instance.write_all(b"OK recorded\n").unwrap();
        drop(instance);
        closed.send(()).unwrap();
    });
    tell_exchange(&stream, 1, Instant::now() + must, || {
        (&stream).write_all(b"notice")?;
        closure.recv_timeout(must).unwrap();
        aterm_spec::wait::until_peer_closed(&stream);
        Ok(())
    })
    .unwrap();
    peer.join().unwrap();
}

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
    /// [`Kernel::held_now`]'s answer.
    held_now: Option<HeldNow>,
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
    /// One change after run-record admission, before the cleanup pass captures its uid table.
    before_uid_table: RefCell<Option<Box<dyn FnOnce()>>>,
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
    fn held_now(&self) -> Option<HeldNow> {
        self.held_now
    }
    fn launchd_version(&self) -> Option<i32> {
        Some(1026)
    }
    fn tty_master_held(&self, dev: u32) -> Option<bool> {
        self.ttys.get(&dev).copied().unwrap_or(Some(false))
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        if let Some(change) = self.before_uid_table.borrow_mut().take() {
            change();
        }
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
    start_pass_telling(dir, k, &Inert)
}

/// The start pass over `dir` and `k`, telling `poster` what it ends, with its lines.
fn start_pass_telling(dir: &Path, k: &FakeKernel, poster: &dyn Poster) -> (usize, Vec<String>) {
    let mut lines = Vec::new();
    let ended = end_dead_runs(dir, "B", ME, k, &mut |l| lines.push(l.to_owned()), poster);
    (ended, lines)
}

/// A poster that keeps what each post told.
#[derive(Debug, Default)]
struct Recording(Mutex<Vec<Vec<String>>>);

impl Poster for Recording {
    fn post(&self, lines: &[String]) {
        self.0.lock().expect("the recording").push(lines.to_vec());
    }
}

impl Recording {
    /// Every post's lines, in order.
    fn told(&self) -> Vec<Vec<String>> {
        self.0.lock().expect("the recording").clone()
    }
}

#[test]
fn a_run_id_round_trips() {
    let id = RunId {
        pid: 41983,
        birth: Birth(1_792_163_800_331_520),
    };
    assert_eq!(RunId::parse(&id.text()), Some(id));
    assert_eq!(RunId::parse("nope"), None);
    assert_eq!(RunId::parse("+41983-100"), None);
    assert_eq!(RunId::parse("41983-+100"), None);
    assert_eq!(RunId::parse("-41983-100"), None);
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
    let cases: [Case; 17] = [
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
        ("its tty's master closed", Want::Ended, |k, m| {
            m.tty = Some(77);
            k.ttys.insert(77, Some(false));
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
    let report = sweep_group(
        &k,
        500,
        "test -p aterm-link",
        &[PathBuf::from("/wt-a")],
        &Inert,
    );
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

#[test]
fn an_unrecorded_dead_seen_horizon_never_authorizes_cleanup() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(71, 400, 500, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(DEAD_ID));
    let text = dead_record();
    let dir = records(&[("41983-999.run", &text)]);
    let path = dir.path().join("41983-999.run");
    let saved = dir.path().join("41983-999.preserved");
    let (changed_path, saved_path) = (path.clone(), saved.clone());
    *k.before_uid_table.borrow_mut() = Some(Box::new(move || {
        std::fs::rename(&changed_path, &saved_path).unwrap();
        std::fs::create_dir(&changed_path).unwrap();
    }));

    let (ended, lines) = start_pass(dir.path(), &k);
    let signals = k.signals.borrow().clone();
    let markers = k.markers_read.borrow().clone();
    let saved_text = std::fs::read_to_string(&saved).unwrap();
    assert!(path.is_dir(), "the failed record path is retained");
    assert_eq!(saved_text, text, "the admitted evidence is retained intact");
    assert_eq!(ended, 0, "{lines:?}");
    assert!(
        signals.is_empty(),
        "unrecorded horizon signaled {signals:?}"
    );
    assert!(
        markers.is_empty(),
        "cleanup began without a recorded horizon"
    );
}

#[test]
fn a_concurrent_first_dead_seen_horizon_limits_the_stale_cleanup_reader() {
    let mut k = FakeKernel::default();
    with_me(&mut k, 5_000);
    k.put(lin(71, 400, 500, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(71, verify_run(DEAD_ID));
    // An older member remains held, so even a successful cleanup pass retains the record.
    k.put(lin(72, 320, 500, 1), "/wt-a/target/debug/aterm");
    k.markers.insert(72, verify_run(DEAD_ID));
    k.held.insert(72, Some(true));
    let dir = records(&[("41983-999.run", &dead_record())]);
    let path = dir.path().join("41983-999.run");
    let winner_path = path.clone();
    *k.before_uid_table.borrow_mut() = Some(Box::new(move || {
        append(&winner_path, "dead_seen uniqueid=350 at=3").unwrap();
        assert_eq!(
            Record::parse(&std::fs::read_to_string(&winner_path).unwrap()).dead_seen,
            Some(350),
            "the first observer recorded its horizon before the stale reader continues"
        );
    }));

    let (ended, lines) = start_pass(dir.path(), &k);
    let signals = k.signals.borrow().clone();
    let text = std::fs::read_to_string(&path).unwrap();
    let admitted = read_records(dir.path()).unwrap();
    assert_eq!(
        ended, 0,
        "the later table cannot widen the first horizon: {lines:?}"
    );
    assert!(
        signals.is_empty(),
        "the earlier horizon excludes pid 71: {signals:?}"
    );
    assert_eq!(admitted.len(), 1, "the held member keeps its record");
    assert_eq!(admitted[0].1.dead_seen, Some(350));
    assert_eq!(admitted[0].1.dead_seen_ms, Some(3));
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("dead_seen "))
            .count(),
        1
    );
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

    #[cfg(unix)]
    {
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "untouched\n").expect("victim");
        let link = dir.path().join("link.run");
        std::os::unix::fs::symlink(&victim, &link).expect("symlink");
        assert!(append(&link, "dead_seen uniqueid=9 at=9").is_err());
        assert!(open_regular_write(&link, false).is_err());
        assert_eq!(
            std::fs::read_to_string(&victim).expect("victim"),
            "untouched\n"
        );
    }
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
         targo \u{d7}1; 1 not observed running, program-check cause unconfirmed; not judged"
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
         1 not observed running, program-check cause unconfirmed; not judged"
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
    let report = sweep_group(&k, 500, "test -p aterm-link", &roots, &Inert);
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
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
    assert!(report.closed(), "{report:?}");
    let mut k = FakeKernel::default();
    k.groups.insert(500, vec![500]);
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
    assert!(report.closed() && report.line().is_none(), "{report:?}");
}

/// A group the sweep cannot prove empty stays open across missing or partial listings, refused signals, and survivors that left the group.
#[test]
fn a_group_the_sweep_cannot_prove_empty_stays_open() {
    let roots = vec![PathBuf::from("/wt-a")];
    let k = FakeKernel::default();
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
    assert!(!report.closed() && report.line().is_none(), "{report:?}");

    let mut k = FakeKernel::default();
    k.groups.insert(500, vec![500]);
    k.partial.insert(500);
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
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
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
    assert_eq!(report.named, ["aterm"], "{report:?}");
    assert!(!report.closed());

    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/wt-a/target/debug/aterm");
    k.moves.borrow_mut().insert(71, lin(71, 300, 71, 500));
    k.groups.insert(500, vec![500, 71]);
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
    assert_eq!(
        (report.named.len(), k.termed()),
        (1, Vec::new()),
        "{report:?}"
    );
}

/// Run roots include the worktree first and every target or build directory cargo reports.
#[test]
fn the_roots_are_where_the_build_put_its_executables() {
    let json_path = |p: &Path| p.display().to_string().replace('\\', "\\\\");
    let wt_a = std::path::absolute("/wt-a").expect("absolute");
    let shared_target = std::path::absolute("/shared/target").expect("absolute");
    let cache = std::path::absolute("/cache/cargo-build.noindex/wt-a/.cargo-build").expect("abs");
    let metadata = format!(
        "{{\"packages\":[],\"workspace_root\":\"{}\",\
         \"target_directory\":\"{}\",\"build_directory\":\"{}\",\
         \"version\":1}}",
        json_path(&wt_a),
        json_path(&shared_target),
        json_path(&cache)
    );
    let built = parse_build_dirs(&format!("targo: a lane banner\n{metadata}\n"));
    assert_eq!(built, [shared_target.clone(), cache.clone()]);
    let roots = run_roots(&wt_a, Path::new("/tmp/atv.scratch"), &shared_target, &built);
    assert_eq!(roots[0], canonical(&wt_a), "the worktree first: {roots:?}");

    let mut k = FakeKernel::default();
    let agent_exe = cache.join("debug/deps/aterm_agent-336f0d9f2ce88bd5");
    k.put(lin(71, 300, 500, 500), &agent_exe.display().to_string());
    k.put(
        lin(72, 301, 500, 500),
        &shared_target.join("debug/aterm").display().to_string(),
    );
    k.put(lin(73, 302, 500, 500), "/usr/bin/ssh-agent");
    k.groups.insert(500, vec![500, 71, 72, 73]);
    let label = agent_exe.display().to_string();
    let report = sweep_group(&k, 500, &label, &roots, &Inert);
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
            &wt_a,
            Path::new("/tmp/s"),
            &wt_a.join("target"),
            &[PathBuf::from(
                "/Library/Caches/aterm/cargo-build.noindex/wt-a",
            )],
        ),
        ..Record::default()
    };
    let want_head = format!("({})", canonical(&wt_a).display());
    assert!(
        dead_run_line(&rec, &Left::default()).contains(&want_head),
        "{rec:?}"
    );

    let w_target = std::path::absolute("/w/target").expect("absolute");
    assert_eq!(
        parse_build_dirs(&format!(
            "{{\"target_directory\":\"{}\"}}",
            json_path(&w_target)
        )),
        [w_target]
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

/// The first start has no records to sweep and does not create the records directory.
#[test]
fn a_records_directory_not_created_yet_is_an_empty_start_pass() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let absent = dir.path().join("verify-runs");
    let k = FakeKernel::default();
    let (ended, lines) = start_pass(&absent, &k);
    assert_eq!(ended, 0);
    assert!(lines.is_empty(), "{lines:?}");
    assert!(k.signals.borrow().is_empty());
    assert!(k.markers_read.borrow().is_empty());
    assert!(!absent.exists());
}

/// Other directory-opening failures still leave the competing claims unknown.
#[test]
fn a_non_directory_does_not_admit_an_empty_records_snapshot() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("verify-runs");
    let bytes = dead_record();
    std::fs::write(&path, &bytes).expect("non-directory fixture");
    assert!(read_records(&path).is_err());
    let k = FakeKernel::default();
    let (ended, _) = start_pass(&path, &k);
    assert_eq!(ended, 0);
    assert!(k.signals.borrow().is_empty());
    assert!(k.markers_read.borrow().is_empty());
    assert_eq!(std::fs::read_to_string(&path).expect("kept fixture"), bytes);
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
        swept: Mutex::new(Vec::new()),
        made: Mutex::new(BTreeMap::new()),
        said: Mutex::new(Vec::new()),
        may_forget: AtomicBool::new(false),
        poster: Box::new(Inert),
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
        true,
    );
    runs.settle_group(
        501,
        &GroupReport {
            stage: "fmt".into(),
            listed: Some(true),
            ..GroupReport::default()
        },
        true,
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
         1 not observed running, program-check cause unconfirmed; not judged",
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
        swept: Mutex::new(Vec::new()),
        made: Mutex::new(BTreeMap::new()),
        said: Mutex::new(Vec::new()),
        may_forget: AtomicBool::new(false),
        poster: Box::new(Inert),
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
            true,
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
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
    assert_eq!(
        (report.named.len(), k.termed()),
        (1, Vec::new()),
        "{report:?}"
    );
    assert!(!report.closed(), "{report:?}");

    k.birth_unreadable.insert(71);
    for presence in [ProcessPresence::Present, ProcessPresence::Unknown] {
        k.presence.insert(71, presence);
        let report = sweep_group(&k, 500, "s", &roots, &Inert);
        assert_eq!(
            (report.named.len(), k.termed()),
            (1, Vec::new()),
            "{report:?}"
        );
        assert!(!report.closed(), "{presence:?}: {report:?}");

        k.unreadable.remove(&71);
        k.refused.insert(71);
        let report = sweep_group(&k, 500, "s", &roots, &Inert);
        assert_eq!(report.named, ["aterm"], "{presence:?}: {report:?}");
        assert!(report.ended.is_empty() && !report.closed(), "{report:?}");
        k.unreadable.insert(71);
    }

    k.lineages.borrow_mut().remove(&71);
    k.presence.remove(&71);
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
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
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
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
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
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
    let report = sweep_group(&k, 500, "s", &roots, &Inert);
    assert!(
        report.ended.is_empty() && report.named.is_empty() && report.closed(),
        "{report:?}"
    );
}

/// `Runs::open` queries cargo metadata once with the run's child environment and records each directory cargo names as a root after the worktree.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_runs_record_roots_are_the_dirs_its_build_names() {
    let json_path = |p: &Path| p.display().to_string().replace('\\', "\\\\");
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let wt_a = std::path::absolute("/wt-a").expect("absolute");
    let shared_target = std::path::absolute("/shared/target").expect("absolute");
    let cache = std::path::absolute("/cache/cargo-build.noindex/wt-a/.cargo-build").expect("abs");
    let mut ctx = Ctx::new(
        wt_a.clone(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/atv-s"),
    );
    ctx.main_build_dir = Some(format!("build.build-dir=\"{}\"", json_path(&cache)));
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
            case_observations: None,
            stopped_at: None,
            hung: None,
            ok: true,
            output: format!(
                "targo: a lane banner\n{{\"target_directory\":\"{}\",\
                 \"build_directory\":\"{}\",\"version\":1}}\n",
                json_path(&shared_target),
                json_path(&cache)
            ),
            code: Some(0),
            spawn_error: None,
            usage: None,
        }
    })
    .expect("a record");
    assert_eq!(asked, 1);
    let rec = Record::parse(&std::fs::read_to_string(&runs.path).expect("the record"));
    assert_eq!(rec.roots[0], canonical(&wt_a), "{rec:?}");
    for root in [&cache, &shared_target] {
        assert!(
            rec.roots.contains(&canonical(root)),
            "{} is a root: {rec:?}",
            root.display()
        );
    }
}

/// A stage's label keeps whole arguments within [`crate::exec::LABEL_MAX`] bytes: a short argv,
/// and one exactly that long, are their label unchanged; one byte more leaves its last argument
/// out (` …+1 arg`, one argument named singular), as does an argument after an exact fit; a
/// `test` with 100 `-p` pairs is labelled by its first arguments and a count of the rest; and a
/// program longer than the bound is kept whole. NEGATIVE CONTROL: label a stage by its whole
/// argv joined, and the 100 pairs' label is 1823 bytes.
#[test]
fn a_stage_label_keeps_whole_arguments_within_its_bound() {
    use crate::exec::{LABEL_MAX, stage_label};
    let words = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(
        stage_label(&words("test -p aterm-link")),
        "test -p aterm-link"
    );
    let exact = format!("targo {}", "x".repeat(LABEL_MAX - "targo ".len()));
    assert_eq!(stage_label(&words(&exact)), exact);
    let over = format!("targo {}", "x".repeat(LABEL_MAX + 1 - "targo ".len()));
    assert_eq!(stage_label(&words(&over)), "targo …+1 arg", "one byte over");
    assert_eq!(
        stage_label(&words(&format!("{exact} y"))),
        format!("{exact} …+1 arg"),
        "an argument after an exact fit"
    );

    let mut argv = words("targo --unverified test");
    for n in 0..100 {
        argv.extend(["-p".to_owned(), format!("aterm-crate-{n:02}")]);
    }
    let label = stage_label(&argv);
    let tail = " …+185 args";
    assert!(
        label.len() <= LABEL_MAX + tail.len(),
        "{} bytes: {label}",
        label.len()
    );
    let kept = label.strip_suffix(tail).expect("a count of the rest");
    assert_eq!(argv.len() - words(kept).len(), 185, "{label}");
    assert!(argv.join(" ").starts_with(kept), "{label}");

    let program = format!("/{}", "w".repeat(300));
    assert_eq!(
        stage_label(&[program.clone(), "test".to_owned()]),
        format!("{program} …+1 arg"),
        "a program longer than the bound"
    );
}

/// A stage's `group` line stays a few hundred bytes whatever label its group is made with: a
/// label whose encoding passes `STAGE_FIELD_MAX` (a 300-byte program, then spaces, `=` and
/// non-ASCII, each encoded to three bytes) is cut at a whole character and ends `…`, and the
/// record still reads back whole, cleanup's check of each field included; a label that fits
/// reads back as it was. NEGATIVE CONTROL: write the label pct-encoded whole, and its line is
/// 2.2 KB.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_stage_groups_line_is_bounded_whatever_its_label() {
    use crate::exec::StageGroupEnd as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let ctx = Ctx::new(
        PathBuf::from("/wt-a"),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/atv-s"),
    );
    let runs = Runs::open(&ctx, dir.path(), |_, _| crate::exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: String::new(),
        code: None,
        spawn_error: None,
        usage: None,
    })
    .expect("a record");
    let fits = "targo --unverified test -p aterm-uds -p aterm-verify …+3 args";
    let long = format!("/{} a=b {}", "w".repeat(300), "\u{e9} ".repeat(200));
    runs.group_made(std::process::id(), fits);
    runs.group_made(std::process::id(), &long);
    let text = std::fs::read_to_string(&runs.path).expect("the record");
    let groups: Vec<&str> = text.lines().filter(|l| l.starts_with("group ")).collect();
    assert_eq!(groups.len(), 2, "{text}");
    for group in &groups {
        let (_, field) = group.split_once(" stage=").expect("a stage field");
        assert!(
            field.len() <= STAGE_FIELD_MAX,
            "{} bytes: {group}",
            field.len()
        );
        assert!(group.len() < 600, "{} bytes: {group}", group.len());
    }
    let read = read_records(dir.path()).expect("the record reads back whole");
    assert_eq!(read.len(), 1, "{text}");
    assert_eq!(read[0].1.groups[0].stage, fits, "{text}");
    let cut = &read[0].1.groups[1].stage;
    let kept = cut.strip_suffix('…').expect("a cut label ends …");
    assert!(
        long.starts_with(kept) && kept.len() > 300,
        "{} bytes kept: {cut}",
        kept.len()
    );
}

/// What group custody names a stage's group by is its label ([`crate::exec::stage_label`]),
/// not its whole argv.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[derive(Debug, Default)]
struct LabelProbe(Mutex<Vec<String>>);

#[cfg(any(target_vendor = "apple", target_os = "linux"))]
impl crate::exec::StageGroupEnd for LabelProbe {
    fn group_made(&self, _pgid: u32, label: &str) {
        self.0
            .lock()
            .expect("the probe's labels")
            .push(label.to_owned());
    }
    fn group_ended(&self, _pgid: u32, _label: &str) {}
    fn group_reaped(&self, _pgid: u32, _label: &str) {}
}

/// A real stage whose argv is 200 arguments long makes its group under its label, cut at a
/// whole argument, which is what the run record's `group` line carries. NEGATIVE CONTROL: name
/// the group by its argv joined, and the label is its whole 2617 bytes.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_real_stage_makes_its_group_under_its_label() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let probe = LabelProbe::default();
    let cmd = crate::exec::Cmd::new("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .args((0..200).map(|n| format!("argument-{n:03}")))
        .capture(crate::exec::Capture::Silent);
    let env = crate::exec::ExecEnv {
        events: None,
        cwd: dir.path(),
        path: std::ffi::OsStr::new("/usr/bin:/bin"),
        scratch: dir.path(),
        child_ceiling: Some(HANG_DETECTOR),
        remove_env: &[],
        add_env: &[],
        stage_groups: Some(&probe),
        host: &crate::exec::LIVE_HOST,
        hang_samples: None,
    };
    let run = crate::exec::run(&cmd, env);
    assert!(run.ok, "the stage: {}", run.output);
    let labels = probe.0.into_inner().expect("the probe's labels");
    assert_eq!(
        labels,
        [crate::exec::stage_label(&cmd.argv())],
        "{labels:?}"
    );
    assert!(
        labels[0].ends_with(" args") && labels[0].len() < 200,
        "{} bytes: {}",
        labels[0].len(),
        labels[0]
    );
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

/// A Linux member on a terminal is named, never ended, while its master reads held (Linux reads
/// every PTY a live process still has held) or unknown (a terminal that is no PTY's), and is
/// ended where it reads closed. NEGATIVE CONTROL: read an unknown master as free
/// (`!= Some(true)` for `== Some(false)`), and the unknown one is ended; ask only whether it has
/// a terminal (`lin.tty.is_none()`), and the closed one is named.
#[test]
fn a_linux_member_with_a_terminal_is_named_while_its_master_is_not_read_closed() {
    for (master, ends) in [(Some(true), false), (None, false), (Some(false), true)] {
        let mut k = FakeKernel::default();
        with_me(&mut k, 5_000);
        let mut member = lin_linux(71, 310, 71, 1);
        member.tty = Some(34817);
        k.put(member, "/wt-a/target/debug/aterm");
        k.markers.insert(71, verify_run(LINUX_DEAD_ID));
        k.ttys.insert(34817, master);
        let dir = records(&[("41983-200.run", &linux_dead_record())]);
        let (ended, lines) = start_pass(dir.path(), &k);
        if ends {
            assert_eq!((ended, k.termed()), (1, vec![71]), "{master:?}: {lines:?}");
        } else {
            assert_eq!(
                (ended, k.termed()),
                (0, Vec::new()),
                "{master:?}: {lines:?}"
            );
            assert!(lines[0].contains("named aterm"), "{master:?}: {lines:?}");
        }
    }
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
        let report = sweep_group(&LiveKernel, pgid, label, &self.roots, &Inert);
        *self.report.lock().expect("the probe's report") = Some(report);
    }
    fn group_reaped(&self, pgid: u32, _label: &str) {
        let mut report = self.report.lock().expect("the probe's report");
        *report = report
            .take()
            .map(|r| r.reaped(|| crate::exec::group::empty(pgid)));
    }
}

/// A real stage under verify's group custody: a shell that backgrounds this test binary as the
/// survivor stand-in and exits once the survivor wrote its pid. At the stage's end the leader is
/// exited and unreaped, the group is listed off the kernel, and the survivor, running from the
/// roots, is ended through `end_proven` and the kernel's own reads, and the group reads closed
/// where a listing can be whole (on a `/proc` mounted with `hidepid` only the kernel's answer
/// once the leader is reaped can close it, and the survivor just ended may still be in the group
/// then, a zombie its new parent has not reaped); under roots it does not run from, it is named
/// and left, and leaves on its own. Under a binary translator
/// the kernel names the translator as the survivor's executable, and the survivor still reads
/// as the program the kernel handed the translator, so it is ended there too; under one the
/// kernel hands no program it maps executable (`translator_holds_its_program`), the survivor
/// reads as the translator, under no root, and the test says it is not run. NEGATIVE CONTROL:
/// make `sweep_group`'s held conjunct `== Some(true)`, and the survivor is named, not ended.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_stage_survivor_running_from_the_roots_is_ended_at_its_stage_end() {
    if let Some(translator) = aterm_uds::process::translator()
        && !aterm_uds::process::translator_holds_its_program()
    {
        println!(
            "NOT RUN under {translator}: runs::tests::\
             a_stage_survivor_running_from_the_roots_is_ended_at_its_stage_end: the kernel hands \
             this translator no program it maps executable, so the survivor reads as the \
             translator, under no root, and is named"
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
            events: None,
            cwd: dir,
            path: std::ffi::OsStr::new("/usr/bin:/bin"),
            scratch: dir,
            child_ceiling: Some(HANG_DETECTOR),
            remove_env: &[],
            add_env: &[],
            stage_groups: Some(&probe),
            host: &crate::exec::LIVE_HOST,
            hang_samples: None,
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
    assert!(
        report.closed() || !aterm_uds::process::group_listing_can_be_whole(),
        "closed where a listing can be whole: {report:?}"
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

/// A group listed only in part whose sweep named nothing is closed once its leader is reaped and
/// the kernel finds no process in the group; it stays open, its partial line said, when the
/// kernel finds one or does not answer; a group with a survivor named stays open whatever the
/// kernel would say, which is not asked; and a group never listed stays as it was. NEGATIVE
/// CONTROL: let `GroupReport::reaped` return the report unchanged, and the empty group stays
/// open with its partial line.
#[test]
fn a_partly_listed_group_the_kernel_finds_empty_once_reaped_is_closed() {
    let roots = vec![PathBuf::from("/wt-a")];
    let partial = || {
        let mut k = FakeKernel::default();
        k.groups.insert(500, vec![500]);
        k.partial.insert(500);
        sweep_group(&k, 500, "s", &roots, &Inert)
    };
    let closed = partial().reaped(|| Some(true));
    assert!(closed.closed() && closed.line().is_none(), "{closed:?}");
    for answer in [Some(false), None] {
        let open = partial().reaped(|| answer);
        assert!(!open.closed(), "{answer:?}: {open:?}");
        assert_eq!(
            open.line().as_deref(),
            Some(
                "stage s: its group's listing was partial, so it may have left processes not \
                 named here"
            ),
            "{answer:?}"
        );
    }

    let mut k = FakeKernel::default();
    k.put(lin(72, 301, 500, 500), "/usr/bin/ssh-agent");
    k.groups.insert(500, vec![500, 72]);
    k.partial.insert(500);
    let asked = std::cell::Cell::new(false);
    let named = sweep_group(&k, 500, "s", &roots, &Inert).reaped(|| {
        asked.set(true);
        Some(true)
    });
    assert!(
        !named.closed() && named.named == ["ssh-agent"] && !asked.get(),
        "{named:?}"
    );
    let never = sweep_group(&FakeKernel::default(), 500, "s", &roots, &Inert).reaped(|| Some(true));
    assert!(!never.closed() && never.listed.is_none(), "{never:?}");
}

/// The run's record of its stage groups, live: a real stage that leaves nothing is written
/// `closed` once its leader is reaped, with no line for the receipt, on every mount of `/proc`
/// (under `hidepid` its listing is partial, and the kernel's answer about the reaped group closes
/// it); a group listed in part that still has a member (this test's own) keeps its partial line
/// and is not closed; and a group whose leader's reap never came is said as its sweep found it
/// at the run's end. NEGATIVE CONTROL: answer `None` in `crate::exec::group::empty`, and under
/// `hidepid` the real stage's group stays open with its partial line.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_real_stage_that_leaves_nothing_is_recorded_closed() {
    use crate::exec::StageGroupEnd as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let ctx = Ctx::new(
        PathBuf::from("/wt-a"),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/atv-s"),
    );
    let runs = Runs::open(&ctx, dir.path(), |_, _| crate::exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: String::new(),
        code: None,
        spawn_error: None,
        usage: None,
    })
    .expect("a record");
    let cmd = crate::exec::Cmd::new("/bin/sh")
        .args(["-c", "exit 0"])
        .capture(crate::exec::Capture::Silent);
    let env = crate::exec::ExecEnv {
        events: None,
        cwd: dir.path(),
        path: std::ffi::OsStr::new("/usr/bin:/bin"),
        scratch: dir.path(),
        child_ceiling: Some(HANG_DETECTOR),
        remove_env: &[],
        add_env: &[],
        stage_groups: Some(&runs),
        host: &crate::exec::LIVE_HOST,
        hang_samples: None,
    };
    let run = crate::exec::run(&cmd, env);
    assert!(run.ok, "the stage: {}", run.output);
    let rec = Record::parse(&std::fs::read_to_string(&runs.path).expect("the record"));
    assert!(
        rec.groups.len() == 1 && rec.groups[0].closed,
        "the stage's group is closed: {rec:?}"
    );
    assert!(runs.said.lock().expect("said").is_empty());

    // SAFETY: `getpgid(0)` reads this process's group; it signals nothing.
    let own = u32::try_from(unsafe { libc::getpgid(0) }).expect("this process's group");
    let partial = |stage: &str| GroupReport {
        stage: stage.to_owned(),
        listed: Some(false),
        ..GroupReport::default()
    };
    runs.swept
        .lock()
        .expect("swept")
        .extend([(own, 0, partial("led")), (u32::MAX, 0, partial("unreaped"))]);
    runs.group_reaped(own, "led");
    let said = runs.end_of_run(&FakeKernel::default());
    let text = std::fs::read_to_string(&runs.path).expect("the record");
    assert_eq!(
        text.lines().filter(|l| l.starts_with("closed ")).count(),
        1,
        "only the stage's group is closed: {text}"
    );
    for stage in ["led", "unreaped"] {
        assert!(
            said.iter()
                .any(|l| l.starts_with(&format!("stage {stage}: its group's listing was partial"))),
            "{stage}: {said:?}"
        );
    }
}

/// A `closed` line, live, through the run's own hooks, on every host: a group listed only in
/// part whose sweep named nothing is written `closed` once its leader is reaped and the kernel
/// finds no process in it (each leader here a child of this test's, in a session of its own,
/// exited and reaped), with no line for the receipt; and a `closed` line never lands on a group
/// made under the same id after the sweep: one the sweep listed whole is written at once, while
/// its leader is unreaped, and one listed in part is written after the reap only while no newer
/// `group` line names the id, so the newer group stays open. NEGATIVE CONTROLS: drop
/// `GroupReport::reaped` from `Runs::group_reaped`, or answer `None` in
/// `crate::exec::group::empty`, and the partly listed group stays open, its partial line said;
/// keep every report for the reap, or write its line whatever the record says since, and the
/// newer group reads closed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_closed_line_lands_on_the_group_its_sweep_proved_empty() {
    use crate::exec::StageGroupEnd as _;
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let ctx = Ctx::new(
        PathBuf::from("/wt-a"),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/atv-s"),
    );
    let runs = Runs::open(&ctx, dir.path(), |_, _| crate::exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: String::new(),
        code: None,
        spawn_error: None,
        usage: None,
    })
    .expect("a record");
    let reaped_leader = || -> u32 {
        let mut leader = aterm_uds::spawnfd::command("/bin/sh");
        leader
            .args(["-c", "exit 0"])
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        let mut leader = leader.spawn().expect("a leader");
        leader.wait().expect("the leader, reaped");
        leader.id()
    };
    // What the sweep keeps of a group it could list only in part (a `/proc` mounted with
    // `hidepid`), as `group_ended` keeps it.
    let swept_in_part = |pgid: u32, stage: &str| {
        let lines = runs
            .made
            .lock()
            .expect("made")
            .get(&pgid)
            .copied()
            .unwrap_or(0);
        let report = GroupReport {
            stage: stage.to_owned(),
            listed: Some(false),
            ..GroupReport::default()
        };
        runs.swept
            .lock()
            .expect("swept")
            .push((pgid, lines, report));
    };

    let alone = reaped_leader();
    runs.group_made(alone, "alone");
    swept_in_part(alone, "alone");
    runs.group_reaped(alone, "alone");

    let reused = reaped_leader();
    runs.group_made(reused, "first");
    swept_in_part(reused, "first");
    runs.group_made(reused, "second");
    runs.group_reaped(reused, "first");

    let whole = reaped_leader();
    runs.group_made(whole, "whole");
    runs.group_ended(whole, "whole");
    runs.group_made(whole, "after");
    runs.group_reaped(whole, "whole");

    let rec = Record::parse(&std::fs::read_to_string(&runs.path).expect("the record"));
    let closed = |stage: &str| {
        rec.groups
            .iter()
            .find(|g| g.stage == stage)
            .unwrap_or_else(|| panic!("no group {stage}: {rec:?}"))
            .closed
    };
    assert!(closed("alone"), "{rec:?}");
    assert!(!closed("first") && !closed("second"), "{rec:?}");
    assert!(!closed("after"), "{rec:?}");
    // Under `hidepid` the whole group's own listing is partial too, and the newer line keeps it
    // open.
    assert_eq!(
        closed("whole"),
        aterm_uds::process::group_listing_can_be_whole(),
        "{rec:?}"
    );
    let said = runs.said.lock().expect("said").clone();
    assert!(
        !said.iter().any(|l| l.starts_with("stage alone:")),
        "{said:?}"
    );
}

/// Selects the dead run's launcher stand-in: set, this test binary is a verify run's launcher,
/// and its value is the scratch directory whose `records` it writes its run's record into.
const LAUNCHER_ENV: &str = "ATERM_VERIFY_LAUNCHER_CHILD";

/// A verify run's launcher, as far as a later run's start pass reads one: it writes its run's
/// record (its id, boot and unique id read off the kernel, this test binary as its one root, no
/// stage group), starts two [`a_stage_survivor_stand_in`]s from this binary, `marked` carrying
/// the run's id in `ATERM_VERIFY_RUN` as a stage child does and `unmarked` with none, and waits
/// to be killed, or for the hang detector. Without [`LAUNCHER_ENV`] it returns at once.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_dead_runs_launcher_stand_in() {
    let Some(dir) = std::env::var_os(LAUNCHER_ENV).map(PathBuf::from) else {
        return;
    };
    let me = i32::try_from(std::process::id()).expect("a pid");
    let id = RunId {
        pid: me,
        birth: read_birth(me).expect("this process reads its birth"),
    };
    let uniqueid = read_lineage(me).expect("this process's lineage").uniqueid;
    let exe = std::env::current_exe().expect("this test binary");
    let header = header_text(
        &boot_id().expect("a boot id"),
        id,
        uniqueid,
        aterm_time::unix_millis().unwrap_or(0),
        None,
        &[canonical(&exe)],
    );
    std::fs::write(
        dir.join("records").join(format!("{}.run", id.text())),
        header,
    )
    .expect("the run's record");
    let survivor = |name: &str, marked: bool| {
        let at = dir.join(name);
        std::fs::create_dir(&at).expect("the survivor's directory");
        let mut cmd = aterm_uds::spawnfd::command(&exe);
        cmd.args([
            "--exact",
            "runs::tests::a_stage_survivor_stand_in",
            "--nocapture",
        ])
        .env(SURVIVOR_ENV, &at)
        .env_remove(LAUNCHER_ENV)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
        if marked {
            cmd.env(VERIFY_RUN_ENV, id.text());
        } else {
            cmd.env_remove(VERIFY_RUN_ENV);
        }
        cmd.spawn().expect("a survivor")
    };
    let _survivors = [survivor("marked", true), survivor("unmarked", false)];
    std::thread::sleep(HANG_DETECTOR);
}

/// The live kernel with its listing of this user's processes narrowed to `only`: every read and
/// every signal is the live kernel's.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Listing {
    only: Vec<i32>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Kernel for Listing {
    fn birth(&self, pid: i32) -> Option<Birth> {
        LiveKernel.birth(pid)
    }
    fn presence(&self, pid: i32) -> ProcessPresence {
        LiveKernel.presence(pid)
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        LiveKernel.lineage(pid)
    }
    fn members(&self, pgid: u32) -> Option<GroupMembers> {
        LiveKernel.members(pgid)
    }
    fn exe(&self, pid: i32) -> Option<PathBuf> {
        LiveKernel.exe(pid)
    }
    fn markers(&self, pid: i32) -> Option<Markers> {
        LiveKernel.markers(pid)
    }
    fn held(&self, pid: i32) -> Option<bool> {
        LiveKernel.held(pid)
    }
    fn held_now(&self) -> Option<HeldNow> {
        LiveKernel.held_now()
    }
    fn launchd_version(&self) -> Option<i32> {
        LiveKernel.launchd_version()
    }
    fn tty_master_held(&self, dev: u32) -> Option<bool> {
        LiveKernel.tty_master_held(dev)
    }
    fn uid_pids(&self) -> Option<Vec<i32>> {
        let live = LiveKernel.uid_pids()?;
        Some(live.into_iter().filter(|p| self.only.contains(p)).collect())
    }
    fn now_ms(&self) -> u64 {
        LiveKernel.now_ms()
    }
    fn end(&self, token: Identity, still: &dyn Fn(&Identity) -> bool) -> Result<Ended, EndRefusal> {
        LiveKernel.end(token, still)
    }
}

/// A dead run's launcher stand-in, `SIGKILL`ed and reaped when dropped unless the test reaped it,
/// so a test that fails first leaves none behind (its survivors end as the test's scratch goes).
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Launcher(aterm_uds::spawnfd::Child);

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Drop for Launcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A verify run really SIGKILLed: its launcher ([`a_dead_runs_launcher_stand_in`], in a session
/// of its own, so no terminal's master holds its children) wrote its record and started two
/// survivors from its root, then got `SIGKILL`, and launchd or init adopted them. The next run's
/// start pass on the live kernel reads the launcher dead and ends the survivor that carries the
/// run's id (W-run, C-tree, orphaned, its thread ran), through `end_proven`; it leaves the one
/// that carries no id running, unsignalled, as no member of the run's; and with nothing named or
/// unjudged it removes the record. The pass lists only the two survivors ([`Listing`]): the dead
/// run's root is this test binary, and a copy another test of this binary starts in the run's
/// window, its first thread not yet run as the pass reads it, is held at macOS's program check,
/// so it would be counted unjudged and the record kept. NEGATIVE CONTROLS: make
/// `Judge::judge`'s `w_run` false, and the marked survivor is left too: nothing is ended; list
/// every process of this user while another test's copy of this binary is held, and the pass
/// says `1 not observed running, program-check cause unconfirmed; not judged`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_sigkilled_runs_marked_orphan_is_ended_by_the_next_runs_start_pass() {
    if let Some(translator) = aterm_uds::process::translator()
        && !aterm_uds::process::translator_holds_its_program()
    {
        println!(
            "NOT RUN under {translator}: runs::tests::\
             a_sigkilled_runs_marked_orphan_is_ended_by_the_next_runs_start_pass: the kernel \
             hands this translator no program it maps executable, so the survivor reads as the \
             translator, under no root, and is not the run's"
        );
        return;
    }
    let until = |what: &str, done: &mut dyn FnMut() -> bool| {
        let deadline = std::time::Instant::now() + HANG_DETECTOR;
        while !done() {
            assert!(
                std::time::Instant::now() < deadline,
                "{what}: past the hang detector"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let exe = std::env::current_exe().expect("this test binary");
    let word = exe_word(&exe).expect("a test binary's word");
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let records = scratch.path().join("records");
    std::fs::create_dir(&records).expect("the records directory");
    let mut launcher = aterm_uds::spawnfd::command(&exe);
    launcher
        .args([
            "--exact",
            "runs::tests::a_dead_runs_launcher_stand_in",
            "--nocapture",
        ])
        .env(LAUNCHER_ENV, scratch.path())
        .setsid(true)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    let mut launcher = Launcher(launcher.spawn().expect("the launcher"));
    let launcher_pid = launcher.0.id();
    let survivor = |name: &str| -> (i32, Birth) {
        let file = scratch.path().join(name).join("pid");
        until(&format!("the {name} survivor's pid"), &mut || {
            file.is_file()
        });
        let written = std::fs::read_to_string(&file).expect("the survivor's pid");
        written
            .split_once(' ')
            .and_then(|(pid, birth)| Some((pid.parse().ok()?, Birth(birth.trim().parse().ok()?))))
            .unwrap_or_else(|| panic!("`<pid> <birth>`: {written:?}"))
    };
    // Each survivor wrote its pid, so both ran, and the launcher had written its record first.
    let (marked, unmarked) = (survivor("marked"), survivor("unmarked"));
    launcher.0.kill().expect("SIGKILL to the launcher");
    launcher.0.wait().expect("the launcher, reaped");
    for (pid, _) in [marked, unmarked] {
        until(&format!("pid {pid}'s adoption"), &mut || {
            read_lineage(pid).is_some_and(|l| l.parent != launcher_pid)
        });
    }
    let root = canonical(&exe);
    let mut lines = Vec::new();
    let ended = end_dead_runs(
        &records,
        &boot_id().expect("a boot id"),
        i32::try_from(std::process::id()).expect("a pid"),
        &Listing {
            only: vec![marked.0, unmarked.0],
        },
        &mut |l| lines.push(l.to_owned()),
        &Inert,
    );
    let left = |(pid, birth): (i32, Birth)| match read_birth(pid) {
        Some(now) => now != birth,
        None => process_presence(pid) != ProcessPresence::Present,
    };
    assert!(
        !left(unmarked),
        "the unmarked survivor was ended: {lines:?}"
    );
    assert_eq!(ended, 1, "{lines:?}");
    assert_eq!(
        lines,
        [format!(
            "the verify run of pid {launcher_pid} ({}) left 1 processes: ended {word} \u{d7}1",
            root.display()
        )]
    );
    assert!(left(marked), "the marked survivor outlived its end");
    assert!(
        std::fs::read_dir(&records)
            .expect("the records directory")
            .next()
            .is_none(),
        "the dead run's record is removed"
    );
    std::fs::write(scratch.path().join("unmarked").join("stop"), "").expect("stop");
    until("the unmarked survivor's own end", &mut || left(unmarked));
}

/// THE RUN-START LINE COUNTS WHAT MACOS'S PROGRAM CHECK HOLDS: this user's programs the one
/// shared reader counts held ([`Kernel::held_now`], doctor's too), the oldest by its fork age,
/// with the responsible app's listing read only then. Nothing held, or no listing of this user's
/// processes, is no line and no listing read; the line reads no process itself.
#[test]
fn the_run_start_line_counts_held_programs_and_reads_the_listing_only_then() {
    fn listing<'a>(
        asked: &'a std::cell::Cell<u32>,
        app: Option<&'static str>,
        word: &'static str,
    ) -> impl FnOnce() -> (Option<String>, &'static str) + 'a {
        move || {
            asked.set(asked.get() + 1);
            (app.map(str::to_owned), word)
        }
    }
    let asked = std::cell::Cell::new(0);
    let listed = |app, word| listing(&asked, app, word);
    let mut k = FakeKernel::default();
    with_me(&mut k, 1);
    for none in [None, Some(HeldNow::default())] {
        k.held_now = none;
        assert_eq!(program_check_line(&k, listed(None, "no")), None, "{none:?}");
    }
    assert_eq!(asked.get(), 0, "nothing held: no listing read");

    k.held_now = Some(HeldNow {
        count: 2,
        oldest_ms: 35_940_500,
    });
    assert_eq!(
        program_check_line(&k, listed(None, "no")).as_deref(),
        Some(
            "macOS's program check: 2 programs waiting, the oldest started 9 h 59 m ago (aterm \
             listed: no)"
        )
    );
    assert_eq!(asked.get(), 1);
    assert_eq!(
        program_check_line(&k, listed(Some("Terminal"), "unknown")).as_deref(),
        Some(
            "macOS's program check: 2 programs waiting, the oldest started 9 h 59 m ago \
             (Terminal listed: unknown)"
        ),
        "the responsible app takes aterm's place"
    );
    k.held_now = Some(HeldNow {
        count: 1,
        oldest_ms: 20_000,
    });
    assert_eq!(
        program_check_line(&k, listed(None, "yes")).as_deref(),
        Some(
            "macOS's program check: 1 program waiting, the oldest started 20 s ago (aterm listed: yes)"
        )
    );
}

/// A dead run's start pass lays out: its stage leader 500, orphaned and running its build
/// (W-group, C-tree), and pid 71, out of every group but carrying the run's id (W-run, C-tree),
/// which outlives its `SIGTERM`.
fn two_dead_members(k: &mut FakeKernel) {
    with_me(k, 5_000);
    k.put(lin(500, 300, 500, 1), "/wt-a/target/debug/aterm");
    k.put(lin(71, 310, 71, 1), "/wt-a/target/debug/deps/aterm_x-1");
    k.markers.insert(71, verify_run(DEAD_ID));
    k.stubborn.insert(71);
}

/// What a dead run's start pass ends unasked is told once, after the pass: one line per process
/// with the witness that made it the run's (its stage group, or its run id) and C-tree, its
/// signals and that it is gone. The receipt's lines and the count are what the pass makes with
/// nobody told. NEGATIVE CONTROL: drop the post from `end_dead_runs`, and nothing is told.
#[test]
fn a_dead_runs_ended_member_posts_one_auto_end_record_with_its_proof() {
    let mut quiet = FakeKernel::default();
    two_dead_members(&mut quiet);
    let dir = records(&[("41983-999.run", &dead_record())]);
    let unheard = start_pass(dir.path(), &quiet);

    let mut k = FakeKernel::default();
    two_dead_members(&mut k);
    let dir = records(&[("41983-999.run", &dead_record())]);
    let poster = Recording::default();
    let heard = start_pass_telling(dir.path(), &k, &poster);
    assert_eq!(k.termed(), [500, 71], "{heard:?}");
    assert_eq!(heard, unheard, "telling changes no verdict and no line");
    assert_eq!(heard.0, 2);
    assert_eq!(
        poster.told(),
        [vec![
            "verify ended aterm (pid 500), left by the verify run of pid 41983 (/wt-a): proof \
             group,tree; SIGTERM; gone"
                .to_owned(),
            "verify ended aterm_x-1 (pid 71), left by the verify run of pid 41983 (/wt-a): \
             proof run,tree; SIGTERM, then SIGKILL; gone"
                .to_owned(),
        ]]
    );
}

/// A stage's sweep tells each member it ended, by its stage group, and nothing of the members
/// it named.
#[test]
fn a_stage_sweep_tells_each_member_it_ended() {
    let roots = vec![PathBuf::from("/wt-a")];
    let mut k = FakeKernel::default();
    k.put(lin(71, 300, 500, 500), "/wt-a/target/debug/aterm");
    k.put(lin(72, 301, 500, 500), "/usr/bin/ssh-agent");
    k.groups.insert(500, vec![500, 71, 72]);
    let poster = Recording::default();
    let report = sweep_group(&k, 500, "test -p aterm-link", &roots, &poster);
    assert_eq!(report.ended, ["aterm"], "{report:?}");
    assert_eq!(report.named, ["ssh-agent"], "{report:?}");
    assert_eq!(
        poster.told(),
        [vec![
            "verify ended aterm (pid 71), left in stage `test -p aterm-link`'s group: proof \
             group,tree; SIGTERM; gone"
                .to_owned()
        ]]
    );
}

/// With no instance to tell (no session, or no instance hosting it, or one whose socket answers
/// nothing), the pass ends what it ends, says what it says, and returns.
#[test]
fn a_run_with_no_instance_posts_nothing_and_its_verdict_stands() {
    let mut quiet = FakeKernel::default();
    two_dead_members(&mut quiet);
    let dir = records(&[("41983-999.run", &dead_record())]);
    let unheard = start_pass(dir.path(), &quiet);

    let rendezvous = aterm_tempfile::tempdir().expect("tempdir");
    let graph = rendezvous.path().join("graph");
    std::fs::create_dir(&graph).expect("graph");
    let sock = rendezvous.path().join("gone.sock");
    std::fs::write(graph.join("s-dead"), format!("sock {}\n", sock.display())).expect("entry");
    for poster in [
        ToInstance::default(),
        ToInstance {
            sid: Some("s-0b0d1e5".to_owned()),
            dir: Some(rendezvous.path().to_path_buf()),
        },
        ToInstance {
            sid: Some("s-dead".to_owned()),
            dir: Some(rendezvous.path().to_path_buf()),
        },
    ] {
        let mut k = FakeKernel::default();
        two_dead_members(&mut k);
        let dir = records(&[("41983-999.run", &dead_record())]);
        assert_eq!(
            start_pass_telling(dir.path(), &k, &poster),
            unheard,
            "{poster:?}"
        );
        assert!(!dir.path().join("41983-999.run").exists(), "{poster:?}");
    }
}

/// The instance hosting the run's session, as its graph entry names it, is told each line as
/// one `appnotice proc` after the instance token's `AUTH`, on one connection.
#[cfg(unix)]
#[test]
fn the_hosting_instance_is_told_each_line_as_an_appnotice_proc() {
    use std::io::{BufRead as _, Write as _};
    let rendezvous = aterm_tempfile::tempdir().expect("tempdir");
    let graph = rendezvous.path().join("graph");
    std::fs::create_dir(&graph).expect("graph");
    let sock = rendezvous.path().join("fake.sock");
    std::fs::write(graph.join("s-5e55"), format!("sock {}\n", sock.display())).expect("entry");
    let token = rendezvous
        .path()
        .join(aterm_types::control_socket::token_name_for_sock(
            "fake.sock",
        ));
    std::fs::write(&token, "cafef00d\n").expect("token");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    let instance = std::thread::spawn(move || {
        let must = aterm_spec::wait::MUST_HAPPEN;
        let (conn, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(Instant::now() + must),
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("the post's connection");
        conn.set_read_timeout(Some(must)).expect("a hang detector");
        let mut reader = std::io::BufReader::new(conn.try_clone().expect("clone"));
        let mut heard = Vec::new();
        for _ in 0..3 {
            let mut line = String::new();
            reader.read_line(&mut line).expect("a request line");
            if line.starts_with("appnotice ") {
                (&conn).write_all(b"OK recorded\n").expect("reply");
            }
            heard.push(line);
        }
        heard
    });
    ToInstance {
        sid: Some("s-5e55".to_owned()),
        dir: Some(rendezvous.path().to_path_buf()),
    }
    .post(&["verify ended aterm (pid 71)".to_owned(), "a\nb".to_owned()]);
    assert_eq!(
        instance.join().expect("the instance"),
        [
            "AUTH cafef00d\n",
            "appnotice proc verify ended aterm (pid 71)\n",
            "appnotice proc a b\n",
        ]
    );
}

/// A post that never ends (a connect the kernel blocks, an instance that stopped mid-reply) holds
/// its own thread, never the pass that posts: `within` returns once its bound is up. NEGATIVE
/// CONTROL: run the post on the pass's own thread, and the pass never returns (the one-minute
/// hang detector trips).
#[test]
fn a_post_that_never_ends_holds_no_pass() {
    let (release, blocked) = std::sync::mpsc::channel::<()>();
    let (returned, back) = std::sync::mpsc::channel();
    let pass = std::thread::spawn(move || {
        within(Duration::from_millis(10), move || {
            let _ = blocked.recv();
        });
        let _ = returned.send(());
    });
    assert!(
        back.recv_timeout(aterm_spec::wait::MUST_HAPPEN).is_ok(),
        "the pass waited on a post that never ends"
    );
    drop(release);
    pass.join().expect("the pass");
}

/// Each line is cut within the bytes an `appnotice` carries at a character boundary, so a line
/// of multibyte characters (a run's root named in CJK or emoji) is told cut, never dropped by the
/// instance for being over its 512 bytes; control characters read as spaces. NEGATIVE CONTROL:
/// cut at 400 characters, and 400 two-byte characters are 800 bytes.
#[test]
fn a_notice_line_is_cut_by_bytes_at_a_character_boundary() {
    let wide = notice_text(&"é".repeat(400));
    assert!(wide.len() <= NOTICE_TEXT_BYTES, "{} bytes", wide.len());
    assert_eq!(wide, "é".repeat(NOTICE_TEXT_BYTES / 2));
    let emoji = notice_text(&format!("a{}", "🦀".repeat(200)));
    assert!(emoji.len() <= NOTICE_TEXT_BYTES, "{} bytes", emoji.len());
    assert!(emoji.starts_with("a🦀"), "{emoji}");
    assert_eq!(notice_text(&"x".repeat(500)).len(), NOTICE_TEXT_BYTES);
    assert_eq!(notice_text("a\nb\tc"), "a b c");
}

/// A run's record tells nobody until verify's own entry hands it a poster
/// ([`Runs::telling`]), so no test's sweep reaches an instance.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn the_default_poster_is_inert() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let ctx = Ctx::new(
        PathBuf::from("/wt-a"),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        PathBuf::from("/tmp/atv-s"),
    );
    let runs = Runs::open(&ctx, dir.path(), |_, _| crate::exec::Run {
        case_observations: None,
        stopped_at: None,
        hung: None,
        ok: false,
        output: String::new(),
        code: None,
        spawn_error: None,
        usage: None,
    })
    .expect("a record");
    assert_eq!(format!("{:?}", runs.poster), format!("{Inert:?}"));
    let told = Recording::default();
    let runs = runs.telling(Box::new(told));
    assert!(format!("{:?}", runs.poster).starts_with("Recording"));
}
