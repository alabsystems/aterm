// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::HashMap;

fn words(w: &[&str]) -> Vec<String> {
    w.iter().map(ToString::to_string).collect()
}

/// What the fake saw, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    Kill(i32),
    Sleep(Duration),
    Relaunch {
        how: String,
        armed: bool,
        t_end: i64,
        sock: String,
        at: i64,
    },
}

/// A lab's processes on a clock of its own: nothing is signalled and
/// nothing sleeps (a sleep moves the clock).
struct Fake {
    /// `names_lab`'s answer for the recorded pid.
    ours: bool,
    /// `keeper`'s answer.
    keeper: bool,
    /// Custody can end while the existing journal wait runs.
    ours_until: Option<i64>,
    /// The process can also end during the captured tree read.
    lose_in_tree: bool,
    tree_asked: u32,
    witness: bool,
    agent: Option<(i64, String)>,
    /// The crash journal names the conversation from this time on.
    desk_from: Option<i64>,
    /// The instance ends this long after its SIGKILL (never: `None`).
    ends_after: Option<i64>,
    tree: Vec<Proc>,
    /// When a process of the tree ends by itself.
    tree_ends: HashMap<i32, i64>,
    /// Pids of the tree the system gave to another process since.
    reused: Vec<i32>,
    /// A sleep this long or longer is interrupted.
    stop_at: Option<Duration>,
    clock: i64,
    killed: HashMap<i32, i64>,
    desk_asked: u32,
    ev: Vec<Ev>,
}

const INSTANCE: i32 = 4242;
const NEXT: i32 = 5151;

fn fake() -> Fake {
    Fake {
        ours: true,
        keeper: false,
        ours_until: None,
        lose_in_tree: false,
        tree_asked: 0,
        witness: true,
        agent: Some((4250, "conv-a".into())),
        desk_from: Some(0),
        ends_after: Some(40),
        tree: [4243, 4250]
            .map(|pid| Proc {
                pid,
                start: format!("start of {pid}"),
            })
            .to_vec(),
        tree_ends: HashMap::new(),
        reused: Vec::new(),
        stop_at: None,
        clock: 1_000_000,
        killed: HashMap::new(),
        desk_asked: 0,
        ev: Vec::new(),
    }
}

fn lab() -> Run {
    Run {
        tag: "k1".into(),
        sockdir: "/tmp/at.AbCd".into(),
        sock: "/tmp/at.AbCd/a.sock".into(),
        instance_pid: INSTANCE,
        seam: "0".into(),
        ..Run::default()
    }
}

impl Ops for Fake {
    fn witness(&mut self, _run: &Run) -> Result<(), String> {
        if self.witness {
            Ok(())
        } else {
            Err("its egress witness is not running".into())
        }
    }
    fn names_lab(&mut self, run: &Run, pid: i32) -> bool {
        self.ours
            && self.ours_until.is_none_or(|until| self.clock < until)
            && pid == run.instance_pid
            && pid > 1
    }
    fn keeper(&mut self, _pid: i32) -> bool {
        self.keeper
    }
    fn agent(&mut self, _run: &Run) -> Option<(i64, String)> {
        self.agent.clone()
    }
    fn desk_names(&mut self, _run: &Run, _conv: &str) -> bool {
        self.desk_asked += 1;
        self.desk_from.is_some_and(|t| self.clock >= t)
    }
    fn tree(&mut self, pid: i32) -> Vec<Proc> {
        self.tree_asked += 1;
        if self.lose_in_tree {
            self.ours = false;
        }
        if pid == INSTANCE {
            self.tree.clone()
        } else {
            Vec::new()
        }
    }
    fn kill(&mut self, pid: i32) {
        self.killed.insert(pid, self.clock);
        self.ev.push(Ev::Kill(pid));
    }
    fn ended(&mut self, pid: i32) -> bool {
        self.killed
            .get(&pid)
            .is_some_and(|&t| self.ends_after.is_some_and(|d| self.clock >= t + d))
    }
    fn lives(&mut self, p: &Proc) -> bool {
        !self.reused.contains(&p.pid)
            && !self.killed.contains_key(&p.pid)
            && self.tree_ends.get(&p.pid).is_none_or(|&t| self.clock < t)
    }
    fn now_ms(&mut self) -> i64 {
        self.clock
    }
    fn sleep(&mut self, d: Duration) -> Result<(), Fail> {
        if self.stop_at.is_some_and(|s| d >= s) {
            return Err(Fail::from(stop::Stopped(libc::SIGINT)));
        }
        self.clock += i64::try_from(d.as_millis()).unwrap();
        self.ev.push(Ev::Sleep(d));
        Ok(())
    }
    fn relaunch(
        &mut self,
        run: &mut Run,
        how: &str,
        armed: bool,
        t_end: i64,
    ) -> Result<(i64, i64), Fail> {
        self.ev.push(Ev::Relaunch {
            how: how.into(),
            armed,
            t_end,
            sock: run.sock.clone(),
            at: self.clock,
        });
        let t_launch = self.clock;
        self.clock += 1500;
        run.instance_pid = NEXT;
        Ok((t_launch, self.clock))
    }
}

impl Fake {
    fn kills(&self) -> Vec<i32> {
        self.ev
            .iter()
            .filter_map(|e| match e {
                Ev::Kill(p) => Some(*p),
                _ => None,
            })
            .collect()
    }
    fn relaunches(&self) -> Vec<Ev> {
        self.ev
            .iter()
            .filter(|e| matches!(e, Ev::Relaunch { .. }))
            .cloned()
            .collect()
    }
    /// The time slept after the first kill.
    fn slept_after_kill(&self) -> Duration {
        self.ev
            .iter()
            .skip_while(|e| !matches!(e, Ev::Kill(_)))
            .filter_map(|e| match e {
                Ev::Sleep(d) => Some(*d),
                _ => None,
            })
            .sum()
    }
}

#[test]
fn crash_s_words_are_read() {
    let asked = |w: &[&str]| parse_args(&words(w)).unwrap();
    assert_eq!(asked(&[]), Some(Asked::default()));
    assert_eq!(
        asked(&["--gap", "180", "--keep-tree"]),
        Some(Asked {
            gap: 180,
            keep_tree: true
        })
    );
    assert_eq!(
        asked(&["--keep-tree", "--gap", "0"]),
        Some(Asked {
            gap: 0,
            keep_tree: true
        })
    );
    assert_eq!(asked(&["--gap", "86400"]).map(|a| a.gap), Some(GAP_MAX));
    assert_eq!(asked(&["--gap", "5", "--help"]), None);
    for bad in [
        &["--gap"][..],
        &["--gap", "x"],
        &["--gap", "-5"],
        &["--gap", "1.5"],
        &["--gap", "86401"],
        &["--gap=5"],
        &["--keep"],
        &["k1"],
    ] {
        let e = parse_args(&words(bad)).unwrap_err();
        assert_eq!(e.code, 2, "{bad:?}: usage");
    }
}

/// The pid the crash signals is the one `env.sh` records, and only that
/// one when the tree is kept: the agent and its shell live on.
#[test]
fn the_crash_kills_the_recorded_instance_and_nothing_of_a_kept_tree() {
    let mut f = fake();
    let mut run = lab();
    let keep = Asked {
        gap: 0,
        keep_tree: true,
    };
    let did = crash(&mut run, &keep, &mut f).unwrap();
    assert_eq!(f.kills(), vec![INSTANCE]);
    assert_eq!(did.killed, INSTANCE);
    assert_eq!(did.kept, vec![4243, 4250]);
    assert!(did.ended_by_pid.is_empty());
    assert_eq!(f.relaunches().len(), 1);
    assert_eq!((did.up, run.instance_pid), (NEXT, NEXT));
}

/// A keeper is read from its command line wherever the lab is, and a line
/// that could not be read is taken for one.
#[test]
fn a_keeper_is_known_by_its_command_line() {
    for line in [
        "/private/tmp/.ubd/at.lab-501/x/runs/t/bin/aterm-lab keeper -- /b/aterm --control-sock /tmp/at.a/a.sock",
        "/private/tmp/lab state/runs/t/bin/aterm-lab keeper -- /b/aterm --no-reroute",
        "",
    ] {
        assert!(may_be_keeper(line), "{line:?}");
    }
    assert!(!may_be_keeper(
        "/w/target/debug/aterm --no-reroute --control-sock /tmp/at.a/a.sock -d /tmp/at.a/proj"
    ));
}

/// A WINDOWED LAB WHOSE RECORDED PID MAY BE ITS KEEPER IS NEVER CRASHED:
/// the keeper's SIGKILL leaves the aterm running. One whose recorded pid is
/// its aterm is crashed and relaunched, as a headless one is, and a
/// headless lab is crashed whatever `keeper` says.
#[test]
fn a_windowed_lab_is_crashed_only_where_its_pid_is_its_aterm() {
    let keep = Asked::default();
    let windowed = || Run {
        window: "1".into(),
        ..lab()
    };
    let mut f = Fake {
        keeper: true,
        ..fake()
    };
    let e = crash(&mut windowed(), &keep, &mut f).unwrap_err();
    assert_eq!(e.code, 2);
    assert!(e.msg.unwrap_or_default().contains("in a window"));
    assert!(f.ev.is_empty(), "{:?}", f.ev);
    for (window, keeper) in [("1", false), ("0", true)] {
        let mut f = Fake { keeper, ..fake() };
        let mut run = Run {
            window: window.into(),
            ..lab()
        };
        assert!(crash(&mut run, &keep, &mut f).is_ok(), "window {window}");
        assert_eq!(f.kills().first(), Some(&INSTANCE));
        assert_eq!(f.relaunches().len(), 1);
    }
}

/// A RECORDED PID THAT IS NOT THE LAB'S IS NEVER SIGNALLED: the instance
/// ended and its pid went to another process, or `env.sh` names none;
/// nor is anything killed or started when the lab's witness is gone.
#[test]
fn a_recorded_pid_that_is_not_the_lab_s_is_never_signalled() {
    let keep = Asked::default();
    let mut f = Fake {
        ours: false,
        ..fake()
    };
    let e = crash(&mut lab(), &keep, &mut f).unwrap_err();
    assert_eq!(e.code, 1);
    assert!(f.ev.is_empty(), "{:?}", f.ev);
    let mut f = fake();
    let mut none = Run {
        instance_pid: 0,
        ..lab()
    };
    assert!(crash(&mut none, &keep, &mut f).is_err());
    assert!(f.ev.is_empty(), "{:?}", f.ev);
    let mut f = Fake {
        witness: false,
        ..fake()
    };
    assert_eq!(crash(&mut lab(), &keep, &mut f).unwrap_err().code, 1);
    assert!(f.ev.is_empty(), "{:?}", f.ev);
    let mut f = Fake {
        ours_until: Some(1_001_000),
        desk_from: Some(1_002_000),
        ..fake()
    };
    let mut run = Run {
        seam: "1".into(),
        ..lab()
    };
    let e = crash(&mut run, &keep, &mut f).unwrap_err();
    assert_eq!(e.code, 1);
    assert!(f.desk_asked > 1, "the real crash path waited first");
    assert_eq!(f.tree_asked, 0);
    assert!(f.kills().is_empty());
    assert!(f.relaunches().is_empty());

    let mut f = Fake {
        lose_in_tree: true,
        ..fake()
    };
    let e = crash(&mut lab(), &keep, &mut f).unwrap_err();
    assert_eq!(e.code, 1);
    assert_eq!(f.tree_asked, 1);
    assert!(f.kills().is_empty());
    assert!(f.relaunches().is_empty());

    let mut f = fake();
    assert!(crash(&mut lab(), &keep, &mut f).is_ok());
    assert_eq!(f.kills()[0], INSTANCE);
}

/// The gap passes after the instance ended and before the relaunch: the
/// lab stands with no instance for exactly `--gap`.
#[test]
fn the_gap_passes_with_no_instance_before_the_relaunch() {
    let run_with = |gap: u64| {
        let mut f = fake();
        let did = crash(
            &mut lab(),
            &Asked {
                gap,
                keep_tree: true,
            },
            &mut f,
        )
        .unwrap();
        (f, did)
    };
    let (f0, d0) = run_with(0);
    let (f180, d180) = run_with(180);
    assert_eq!(
        f180.slept_after_kill() - f0.slept_after_kill(),
        Duration::from_secs(180)
    );
    let n = f180.ev.len();
    assert_eq!(f180.ev[n - 2], Ev::Sleep(Duration::from_secs(180)));
    assert!(matches!(f180.ev[n - 1], Ev::Relaunch { .. }));
    assert!(
        !f0.ev.contains(&Ev::Sleep(Duration::from_secs(180))),
        "no gap asked, none slept"
    );
    let Ev::Relaunch { at, .. } = f180.ev[n - 1] else {
        unreachable!()
    };
    assert!(
        at >= d180.t_end + 180_000,
        "relaunched {at}, killed {}",
        d180.t_end
    );
    assert_eq!(d180.t_up - d0.t_up, 180_000);
}

/// The relaunch is restore's, on the same record: `crash`, unarmed (the
/// instance of a lab `up` made outlives the verb), timed from the kill.
#[test]
fn the_relaunch_is_restore_s_on_the_same_record_unarmed() {
    for keep_tree in [false, true] {
        let mut f = fake();
        let mut run = lab();
        let did = crash(&mut run, &Asked { gap: 3, keep_tree }, &mut f).unwrap();
        assert_eq!(
            f.relaunches(),
            vec![Ev::Relaunch {
                how: "crash".into(),
                armed: false,
                t_end: did.t_end,
                sock: "/tmp/at.AbCd/a.sock".into(),
                at: f
                    .relaunches()
                    .iter()
                    .find_map(|e| match e {
                        Ev::Relaunch { at, .. } => Some(*at),
                        _ => None,
                    })
                    .unwrap(),
            }]
        );
        assert_eq!(did.t_end, 1_000_000);
    }
}

/// Without `--keep-tree` what outlives the instance 15 s is ended by pid,
/// as restore ends it, but never a process that ended by itself, nor one
/// that has the pid of the tree's since; with it, nothing of the tree.
#[test]
fn what_outlives_the_instance_is_ended_by_pid_unless_the_tree_is_kept() {
    let tree = [4243, 4250, 4260]
        .map(|pid| Proc {
            pid,
            start: format!("start of {pid}"),
        })
        .to_vec();
    let with = || Fake {
        tree: tree.clone(),
        tree_ends: HashMap::from([(4243, 1_001_000)]),
        reused: vec![4260],
        ..fake()
    };
    let mut f = with();
    let did = crash(&mut lab(), &Asked::default(), &mut f).unwrap();
    assert_eq!(f.kills(), vec![INSTANCE, 4250]);
    assert_eq!(did.ended_by_pid, vec![4250]);
    let killed_at = f.killed[&4250];
    assert!(
        killed_at >= f.killed[&INSTANCE] + 15_000,
        "ended {} ms after the instance",
        killed_at - f.killed[&INSTANCE]
    );
    let mut f = with();
    let did = crash(
        &mut lab(),
        &Asked {
            gap: 0,
            keep_tree: true,
        },
        &mut f,
    )
    .unwrap();
    assert_eq!(f.kills(), vec![INSTANCE]);
    assert_eq!(did.kept, vec![4250]);
    let mut f = Fake {
        tree_ends: HashMap::from([(4243, 0), (4250, 0)]),
        ..fake()
    };
    crash(&mut lab(), &Asked::default(), &mut f).unwrap();
    assert_eq!(f.kills(), vec![INSTANCE]);
    assert!(f.slept_after_kill() < Duration::from_secs(1));
}

#[test]
fn an_instance_that_does_not_end_is_not_relaunched() {
    let mut f = Fake {
        ends_after: None,
        ..fake()
    };
    let e = crash(&mut lab(), &Asked::default(), &mut f).unwrap_err();
    assert_eq!(e.code, 1);
    assert_eq!(f.kills(), vec![INSTANCE]);
    assert!(f.relaunches().is_empty());
    assert_eq!(f.slept_after_kill(), END_WITHIN);
}

/// Under the headless-restore seam the crash waits until the crash
/// journal names the agent's conversation, as restore does; without it
/// the journal is inert and is not waited on.
#[test]
fn under_the_seam_the_journal_names_the_conversation_before_the_kill() {
    let mut f = Fake {
        desk_from: Some(1_002_000),
        ..fake()
    };
    let mut seam = Run {
        seam: "1".into(),
        ..lab()
    };
    let did = crash(&mut seam, &Asked::default(), &mut f).unwrap();
    assert_eq!(did.t_end, 1_002_000);
    let mut f = Fake {
        desk_from: None,
        ..fake()
    };
    let did = crash(&mut lab(), &Asked::default(), &mut f).unwrap();
    assert_eq!((did.t_end, f.desk_asked), (1_000_000, 0));
    let mut f = Fake {
        desk_from: None,
        ..fake()
    };
    let mut seam = Run {
        seam: "1".into(),
        ..lab()
    };
    let did = crash(&mut seam, &Asked::default(), &mut f).unwrap();
    assert_eq!(did.t_end, 1_000_000 + 30_000);
}

/// RELAUNCH STARTS AN ENDED INSTANCE AGAIN, AND NEVER ONE THAT RUNS (fix unit Q1). Once the
/// recorded instance is no process of the lab (a quit ended it), `relaunch` starts it once on
/// the same socket, killing and sleeping nothing; while it runs, or with no egress witness,
/// nothing is started. NEGATIVE CONTROL, run: a relaunch that does not read the recorded pid
/// first starts a second instance beside the one that runs.
#[test]
fn relaunch_starts_an_ended_instance_once_and_never_one_that_runs() {
    let mut ended = Fake {
        ours: false,
        ..fake()
    };
    let mut run = lab();
    let t_up = relaunch(&mut run, &mut ended).unwrap();
    assert_eq!(
        ended.relaunches(),
        vec![Ev::Relaunch {
            how: "end".into(),
            armed: false,
            t_end: 1_000_000,
            sock: "/tmp/at.AbCd/a.sock".into(),
            at: 1_000_000,
        }]
    );
    assert_eq!((t_up, run.instance_pid), (1_001_500, NEXT));
    assert!(ended.kills().is_empty() && ended.slept_after_kill().is_zero());
    let mut runs = fake();
    let e = relaunch(&mut lab(), &mut runs).unwrap_err();
    assert_eq!(e.code, 2, "a running instance is refused as a usage error");
    assert!(runs.relaunches().is_empty() && runs.kills().is_empty());
    let mut blind = Fake {
        ours: false,
        witness: false,
        ..fake()
    };
    assert!(relaunch(&mut lab(), &mut blind).is_err());
    assert!(
        blind.relaunches().is_empty(),
        "no egress witness, no relaunch"
    );
}

#[test]
fn an_interrupted_gap_relaunches_nothing() {
    let mut f = Fake {
        stop_at: Some(Duration::from_secs(60)),
        ..fake()
    };
    let e = crash(
        &mut lab(),
        &Asked {
            gap: 600,
            keep_tree: true,
        },
        &mut f,
    )
    .unwrap_err();
    assert_eq!(e.stopped, Some(libc::SIGINT));
    assert_eq!(f.kills(), vec![INSTANCE]);
    assert!(f.relaunches().is_empty());
}
