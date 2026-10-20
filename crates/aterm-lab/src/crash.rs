// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `crash [tag] [--gap SECS] [--keep-tree]`: aterm crashes under a lab that
//! is up and comes back on the same state, the step of a crash the agent
//! lives through. `restore` judges a cold restore on a lab of its own;
//! `crash` is the one step, on the lab a driver brought up, and judges
//! nothing: what the next instance and its loop do is the driver's to read
//! (`journal`, `ctl TAG ls`, the harness ledger).
//!
//! It SIGKILLs the lab's OWN instance: the pid `env.sh` records
//! (`INSTANCE_PID`), and only while that pid is still the lab's
//! ([`crate::lab::names_this_lab`], the test `down` makes: alive, its command
//! line on the lab's socket or run directory), never a process found by
//! name. A recorded pid that is not the lab's (it ended, or the system has
//! given it to another process) is refused: nothing is killed and nothing
//! started, exit 1. So is a lab whose egress witness is not running
//! ([`crate::procs::need_witness`]): nothing more starts in its jail.
//!
//! Before the kill it says the Claude Code registered under the instance
//! (its pid and conversation); under the headless-restore seam
//! (`LAB_RESTORE=1`) it first waits, 30 s at most, until the crash journal
//! names that conversation, as `restore` does, and says whether it did. The
//! instance is then SIGKILLed and must end within 30 s: reaped, or a zombie
//! its reaper has not collected yet (the instance of a lab `up` made is
//! launchd's child once `up` has exited, and `kill(pid, 0)` answers for a
//! zombie). One that does not end is a failure, and nothing is relaunched.
//!
//! Its tree (what the instance had started, read before the kill, each pid
//! with its start time) is ended or kept:
//!
//! * without `--keep-tree`, one of them still alive 15 s after (the same
//!   process: its pid and its start time) is said and SIGKILLed by pid, as
//!   `restore` ends it: it would hold the conversation;
//! * with `--keep-tree`, nothing of it is touched, and what still lives 2 s
//!   after is listed. Under a PTY keeper (`keeper = { enabled = true }` in
//!   `LAB_ATERM_TOML`, `aterm keeper serve` run outside the jail on the lab's
//!   environment, and `LAB_INSTANCE_ENV='TMPDIR=/tmp'` so both find its
//!   socket: [`crate::lab::instance_env`]) the keeper holds their terminals,
//!   so the agent lives through the crash and the next instance takes its
//!   session back.
//!
//! Then the gap, `--gap SECS` (0 unless given, a day at most), with no
//! instance; then the relaunch, `restore`'s
//! ([`crate::restore::relaunch_instance`]): the socket and its token cleared
//! from outside the jail and the instance started on the same record (state,
//! socket, grid, jail, and the `LAB_INSTANCE_ENV` `up` took:
//! [`crate::lab::instance_vars`], [`crate::lab::instance_argv`]), with no
//! lifeline: the instance of a lab `up` made outlives the verb, until `down`.
//! `env.sh` records the new pid.
//!
//! Exit 0 once the new instance is up; 1 refused or failed (above, or the
//! relaunch did not come up); 2 usage. Interrupted after the kill, it says
//! that the lab is left with no instance (`down` takes it down).
//!
//! The process operations are injected ([`Ops`]): [`Live`] makes them, and
//! the tests' fake kills nothing and never sleeps. Measured with it:
//! `docs/measured/real-claude-lab-2026-10-15.md`, "A crash that leaves the
//! agent running".

use std::time::Duration;

use crate::restore::since;
use crate::run::{Run, take_only_tag};
use crate::{Fail, LAB, Res, stop};

/// How long the killed instance may take to end: a hang detector.
const END_WITHIN: Duration = Duration::from_secs(30);
/// How long a process of its tree may outlive it before it is ended by pid.
const TREE_WITHIN: Duration = Duration::from_secs(15);
/// How long `--keep-tree` waits before it lists what lives on.
const KEPT_LOOK: Duration = Duration::from_secs(2);
/// How long the crash journal may take to name the agent's conversation (the
/// headless-restore seam).
const DESK_WITHIN: Duration = Duration::from_secs(30);
/// The step of each of those waits.
const STEP: Duration = Duration::from_millis(250);
/// The longest gap, in seconds.
const GAP_MAX: u64 = 86_400;

/// What `crash`'s words ask for.
#[derive(Debug, Default, PartialEq, Eq)]
struct Asked {
    /// `--gap SECS`: how long the lab stays with no instance.
    gap: u64,
    /// `--keep-tree`: nothing of the instance's tree is touched.
    keep_tree: bool,
}

/// `crash`'s words (the tag already taken); `Ok(None)` for `-h`/`--help`.
///
/// # Errors
/// An unknown word, or a `--gap` with no whole number of seconds up to a day.
fn parse_args(args: &[String]) -> Result<Option<Asked>, Fail> {
    let usage = |m: String| Fail::usage(format!("crash: {m}"));
    let mut a = Asked::default();
    let mut words = args.iter();
    while let Some(w) = words.next() {
        match w.as_str() {
            "--gap" => {
                let v = words
                    .next()
                    .ok_or_else(|| usage("--gap needs SECS".into()))?;
                a.gap =
                    v.parse().ok().filter(|&g| g <= GAP_MAX).ok_or_else(|| {
                        usage(format!("--gap {v}: whole seconds, at most {GAP_MAX}"))
                    })?;
            }
            "--keep-tree" => a.keep_tree = true,
            "-h" | "--help" => return Ok(None),
            other => {
                return Err(usage(format!(
                    "unknown argument '{other}' (crash [tag] [--gap SECS] [--keep-tree])"
                )));
            }
        }
    }
    Ok(Some(a))
}

/// A process of the instance's tree as it was read before the kill: its pid
/// and its start time, which tell it from a later process given the pid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Proc {
    pid: i32,
    start: String,
}

/// The process operations a crash makes, injected so its tests kill nothing
/// and never sleep.
pub(crate) trait Ops {
    /// The lab's egress witness runs ([`crate::procs::need_witness`]).
    ///
    /// # Errors
    /// It does not.
    fn witness(&mut self, run: &Run) -> Result<(), String>;
    /// Whether `pid` is a live process of the lab ([`crate::lab::names_this_lab`]).
    fn names_lab(&mut self, run: &Run, pid: i32) -> bool;
    /// The Claude Code registered under the instance: its pid and conversation.
    fn agent(&mut self, run: &Run) -> Option<(i64, String)>;
    /// Whether a quit layout or crash journal of the lab names conversation `conv`.
    fn desk_names(&mut self, run: &Run, conv: &str) -> bool;
    /// Every process under `pid` now.
    fn tree(&mut self, pid: i32) -> Vec<Proc>;
    /// SIGKILL `pid`.
    fn kill(&mut self, pid: i32);
    /// Whether `pid` has ended: gone, or a zombie.
    fn ended(&mut self, pid: i32) -> bool;
    /// Whether `p` lives: its pid, with the start time it was read with.
    fn lives(&mut self, p: &Proc) -> bool;
    /// Now, in milliseconds since the epoch.
    fn now_ms(&mut self) -> i64;
    /// Wait `d`.
    ///
    /// # Errors
    /// Interrupted.
    fn sleep(&mut self, d: Duration) -> Result<(), Fail>;
    /// Start the lab's instance again ([`crate::restore::relaunch_instance`]):
    /// when the launch began and when its socket came up.
    ///
    /// # Errors
    /// It did not come up.
    fn relaunch(
        &mut self,
        run: &mut Run,
        how: &str,
        armed: bool,
        t_end: i64,
    ) -> Result<(i64, i64), Fail>;
}

/// What a crash did.
#[derive(Debug, PartialEq, Eq)]
struct Crashed {
    /// The instance SIGKILLed, and when.
    killed: i32,
    t_end: i64,
    /// The instance started after it, and when its socket came up.
    up: i32,
    t_up: i64,
    /// `--keep-tree`: the processes of the old tree that lived 2 s after.
    kept: Vec<i32>,
    /// Without it: those still alive 15 s after, SIGKILLed by pid.
    ended_by_pid: Vec<i32>,
}

/// Look at `pred` every [`STEP`] until it holds or `within` has passed (a
/// hang detector): whether it held.
fn poll<O: Ops>(
    ops: &mut O,
    within: Duration,
    mut pred: impl FnMut(&mut O) -> bool,
) -> Result<bool, Fail> {
    for _ in 0..within.as_millis() / STEP.as_millis() {
        if pred(ops) {
            return Ok(true);
        }
        ops.sleep(STEP)?;
    }
    Ok(pred(ops))
}

/// Re-read the recorded instance's current lab membership before using its pid.
/// This observation is not an atomic process handle: the final check and signal still race.
fn require_instance(run: &Run, pid: i32, ops: &mut impl Ops) -> Result<(), Fail> {
    if ops.names_lab(run, pid) {
        return Ok(());
    }
    Err(Fail::die(format!(
        "crash: lab '{}' records instance pid {pid}, which is no process of the lab now (it ended, \
         or the pid is another process's): nothing killed, nothing started",
        run.tag
    )))
}

/// The crash of lab `run`'s instance, the gap and the relaunch.
///
/// # Errors
/// Refused (the recorded pid is no process of the lab, its witness is not
/// running), the instance did not end, interrupted, or the relaunch did not
/// come up.
fn crash(run: &mut Run, a: &Asked, ops: &mut impl Ops) -> Result<Crashed, Fail> {
    let old = run.instance_pid;
    require_instance(run, old, ops)?;
    ops.witness(run)
        .map_err(|e| Fail::die(format!("crash: {e}: nothing killed")))?;
    match ops.agent(run) {
        Some((pid, conv)) => {
            println!("crash: Claude Code pid {pid}, conversation {conv}");
            if run.seam == "1" {
                let t0 = ops.now_ms();
                let named = poll(ops, DESK_WITHIN, |o| o.desk_names(run, &conv))?;
                if named {
                    let t = ops.now_ms();
                    println!("crash: the crash journal names it within {}", since(t0, t));
                } else {
                    println!("crash: NO crash journal named it within 30 s");
                }
            }
        }
        None => println!("crash: no Claude Code registered under the instance"),
    }
    // The journal wait may have outlived the recorded instance. Do not inspect
    // another process's tree on the strength of the entry check.
    require_instance(run, old, ops)?;
    let tree = ops.tree(old);
    let t_end = ops.now_ms();
    // The tree read may itself take time; re-read custody closest to the signal.
    require_instance(run, old, ops)?;
    ops.kill(old);
    after_kill(run, a, ops, old, &tree, t_end).inspect_err(|f| {
        if f.stopped.is_some() {
            eprintln!(
                "aterm-lab: crash: interrupted after instance {old} was SIGKILLed: lab '{}' has no instance ({LAB} down {0})",
                run.tag
            );
        }
    })
}

/// [`crash`] once the instance `old` (its tree `tree`) was SIGKILLed at `t_end`.
fn after_kill(
    run: &mut Run,
    a: &Asked,
    ops: &mut impl Ops,
    old: i32,
    tree: &[Proc],
    t_end: i64,
) -> Result<Crashed, Fail> {
    if !poll(ops, END_WITHIN, |o| o.ended(old))? {
        return Err(Fail::die(format!(
            "crash: instance {old} did not end within 30 s of its SIGKILL: nothing relaunched"
        )));
    }
    println!("crash: instance {old} SIGKILLed at {t_end} ms");
    let (mut kept, mut ended_by_pid) = (Vec::new(), Vec::new());
    if a.keep_tree {
        ops.sleep(KEPT_LOOK)?;
        kept = tree
            .iter()
            .filter(|p| ops.lives(p))
            .map(|p| p.pid)
            .collect();
        let list: Vec<String> = kept.iter().map(ToString::to_string).collect();
        println!(
            "crash: its tree still alive 2 s after: [{}]",
            list.join(" ")
        );
    } else {
        let mut left: Vec<Proc> = Vec::new();
        poll(ops, TREE_WITHIN, |o| {
            left = tree.iter().filter(|p| o.lives(p)).cloned().collect();
            left.is_empty()
        })?;
        for p in &left {
            eprintln!(
                "aterm-lab: crash: {} still alive 15 s after the instance ended (ended by pid)",
                p.pid
            );
            ops.kill(p.pid);
            ended_by_pid.push(p.pid);
        }
    }
    if a.gap > 0 {
        println!("crash: the gap, {} s, no instance", a.gap);
        ops.sleep(Duration::from_secs(a.gap))?;
    }
    // restore's relaunch, unarmed: the instance of a lab `up` made outlives this verb
    let (_, t_up) = ops.relaunch(run, "crash", false, t_end)?;
    println!(
        "crash: instance {old} SIGKILLed at {t_end} ms; instance {} up at {t_up} ms, {} after",
        run.instance_pid,
        since(t_end, t_up)
    );
    Ok(Crashed {
        killed: old,
        t_end,
        up: run.instance_pid,
        t_up,
        kept,
        ended_by_pid,
    })
}

/// The process operations of a real crash.
struct Live;

impl Ops for Live {
    fn witness(&mut self, run: &Run) -> Result<(), String> {
        crate::procs::need_witness(run)
    }

    fn names_lab(&mut self, run: &Run, pid: i32) -> bool {
        crate::lab::names_this_lab(run, pid)
    }

    fn agent(&mut self, run: &Run) -> Option<(i64, String)> {
        crate::restore::the_agent(run)
    }

    fn desk_names(&mut self, run: &Run, conv: &str) -> bool {
        crate::restore::desk_naming(run, conv).is_some()
    }

    fn tree(&mut self, pid: i32) -> Vec<Proc> {
        crate::procs::descendants(pid)
            .into_iter()
            .map(|pid| Proc {
                pid,
                start: crate::procs::start_time(pid),
            })
            .collect()
    }

    fn kill(&mut self, pid: i32) {
        let _ = crate::sys::kill(pid, libc::SIGKILL);
    }

    fn ended(&mut self, pid: i32) -> bool {
        !crate::procs::alive(pid) || crate::procs::state_of(pid).is_some_and(|s| s.starts_with('Z'))
    }

    fn lives(&mut self, p: &Proc) -> bool {
        crate::procs::alive(p.pid)
            && !p.start.is_empty()
            && crate::procs::start_time(p.pid) == p.start
    }

    fn now_ms(&mut self) -> i64 {
        crate::util::now_ms()
    }

    fn sleep(&mut self, d: Duration) -> Result<(), Fail> {
        Ok(stop::sleep(d)?)
    }

    fn relaunch(
        &mut self,
        run: &mut Run,
        how: &str,
        armed: bool,
        t_end: i64,
    ) -> Result<(i64, i64), Fail> {
        crate::restore::relaunch_instance(run, how, armed, t_end)
    }
}

/// `crash [tag] [--gap SECS] [--keep-tree]`.
pub fn cmd_crash(args: &[String]) -> Res {
    let mut words = args.to_vec();
    let tag = take_only_tag(&mut words)?;
    let Some(asked) = parse_args(&words)? else {
        print!("{}", crate::USAGE);
        return Ok(0);
    };
    let mut run = Run::load(&tag)?;
    stop::install();
    crash(&mut run, &asked, &mut Live)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
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
        // The recorded instance can end while the existing journal wait runs.
        // Refuse before reading a tree under the now-unowned numeric pid.
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

        // The tree read is another operation between the wait and the kill.
        // Check custody again after it, rather than signalling its stale pid.
        let mut f = Fake {
            lose_in_tree: true,
            ..fake()
        };
        let e = crash(&mut lab(), &keep, &mut f).unwrap_err();
        assert_eq!(e.code, 1);
        assert_eq!(f.tree_asked, 1);
        assert!(f.kills().is_empty());
        assert!(f.relaunches().is_empty());

        // CONTROL: the same lab with its pid its own is crashed
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
            // the kill's stamp is when the instance was SIGKILLed
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
        // CONTROL: a tree that ends with the instance is not waited on 15 s
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
        // a journal that never names it is said, and the crash goes on
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
}
