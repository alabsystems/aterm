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

fn birth(us: u64) -> ProcessBirth {
    ProcessBirth {
        seconds: us / 1_000_000,
        microseconds: us % 1_000_000,
    }
}

/// The birth the fake gives pid `pid`: one per pid, so a reused number reads another.
fn born(pid: i32) -> u64 {
    1_000 + u64::try_from(pid).expect("a pid")
}

/// version below, so with `parent` 1 it is a real orphan; no terminal.
fn lin(pid: i32, uniqueid: u64, pgid: u32, parent: u32) -> Lineage {
    Lineage {
        identity: Identity {
            pid,
            birth: birth(born(pid)),
            idversion: pid,
        },
        uniqueid,
        parent,
        pgid,
        launcher_version: 100,
        tty: None,
    }
}

/// every guard would have it: markers readable and empty, not held, a tty master free.
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
    /// signal, with its identity kept.
    moves: RefCell<HashMap<i32, Lineage>>,
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

/// SIGKILL always does.
struct Control<'a>(&'a FakeKernel);

impl EndControl for Control<'_> {
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        self.0.lineages.borrow().get(&pid).map(|l| l.identity)
    }
    fn signal(&mut self, pid: i32, sig: i32) -> std::io::Result<()> {
        if self.0.refused.contains(&pid) {
            return Err(std::io::Error::other("refused"));
        }
        if self.0.vanish.contains(&pid) {
            self.0.lineages.borrow_mut().remove(&pid);
            return Err(std::io::Error::from_raw_os_error(3));
        }
        self.0.signals.borrow_mut().push((pid, sig));
        let outlives = if sig == SIGKILL {
            self.0.wedged.contains(&pid)
        } else {
            self.0.stubborn.contains(&pid)
        };
        if !outlives {
            self.0.lineages.borrow_mut().remove(&pid);
        }
        Ok(())
    }
    fn nap(&mut self, _pause: Duration) {}
}

impl Kernel for FakeKernel {
    fn birth(&self, pid: i32) -> Option<ProcessBirth> {
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
                self.lineages.borrow_mut().remove(&pid);
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
        if self.lineages.borrow().contains_key(&pid) {
            self.exes.get(&pid).cloned()
        } else {
            None
        }
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
        birth_us: 1_792_163_800_331_520,
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
            birth_us: 100
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

/// newlines only) reads the roots back mangled and the sid's newline as a `root` line.
#[test]
fn a_header_reads_back_any_path_and_session_id() {
    let roots = vec![
        PathBuf::from("/Users//Jos\u{e9}/src/wt a"),
        PathBuf::from("/tmp/no\u{a0}break\ttab=100%"),
    ];
    let id = RunId {
        pid: 7,
        birth_us: 100,
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

/// the leader check) and its case's member is ended.
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
            m.launcher_version = 1026;
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

/// lives on while the pass says the run left nothing.
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

/// and the first is never seen, so the record is removed with it still running.
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

/// process's own unique id, and the member is skipped silently.
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

/// one being written is deleted as another boot's.
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

/// parent is ended.
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

/// held processes' environments are read.
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

/// drop the judgment from `end_proven`) and it is SIGTERMed.
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

/// ended or named and the unlisted group is closed.
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

/// and these groups are closed.
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

/// build dir is a root only inside the worktree) names both, so `ended` is empty.
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
            birth_us: 1,
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

/// `targo` (as `cells.rs` asks for member names) and the `--config` is gone.
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

/// not exist yet) leaves the root under the link, and the exe is not under it.
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

/// or it carries the live run's id) is never ended by another run's start.
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

    // Positive absence releases the old run's claim, allowing the dead run's proven member.
    k.lineages.borrow_mut().remove(&7000);
    k.presence.remove(&7000);
    let (ended, lines) = start_pass(dir.path(), &k);
    assert_eq!((ended, k.termed()), (1, vec![71]), "{lines:?}");
}

/// NEGATIVE CONTROL: dropping any one guard ends a named pid, so `termed` is no longer `[71]`.
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
            birth_us: 1,
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
            birth_us: 1,
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

/// dead passes) ends the stranger in both records.
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

/// NEGATIVE CONTROL: close by the number alone, and the later group is never read.
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

/// CONTROL: claim only live runs' groups and ids, and the inner verify is SIGTERMed.
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

/// named.
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

/// and the group is closed with it alive.
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

        // The same refusal applies after an attempted end, not only before any judgment.
        k.unreadable.remove(&71);
        k.refused.insert(71);
        let report = sweep_group(&k, 500, "s", &roots);
        assert_eq!(report.named, ["aterm"], "{presence:?}: {report:?}");
        assert!(report.ended.is_empty() && !report.closed(), "{report:?}");
        k.unreadable.insert(71);
    }

    // A positive absence answer permits closure, unlike an unreadable identity.
    k.lineages.borrow_mut().remove(&71);
    k.presence.remove(&71);
    let report = sweep_group(&k, 500, "s", &roots);
    assert!(report.closed() && report.named.is_empty(), "{report:?}");
}

/// count every member not ended as named, and a `?` is named and the group stays open.
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

/// recorded closed with the survivor in the table.
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

/// signalled.
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

/// ask with another environment, and the environment assertion fails.
#[cfg(target_vendor = "apple")]
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
