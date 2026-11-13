// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `crash [tag] [--gap SECS] [--keep-tree]`, and `relaunch [tag]`: the instance started again
//! on its own state after it ended by itself.

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
                a.gap = v
                    .bytes()
                    .all(|b| b.is_ascii_digit())
                    .then(|| v.parse().ok())
                    .flatten()
                    .filter(|&g| g <= GAP_MAX)
                    .ok_or_else(|| usage(format!("--gap {v}: whole seconds, at most {GAP_MAX}")))?;
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

/// A process of the instance's tree as it was read before the kill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Proc {
    pid: i32,
    start: String,
}

/// The process operations a crash makes, injected so its tests kill nothing and never sleep.
pub(crate) trait Ops {
    /// The lab's egress witness runs ([`crate::procs::need_witness`]).
    /// # Errors
    /// It does not.
    fn witness(&mut self, run: &Run) -> Result<(), String>;
    /// Whether `pid` is a live process of the lab ([`crate::lab::names_this_lab`]).
    fn names_lab(&mut self, run: &Run, pid: i32) -> bool;
    /// Whether `pid` may be a window's keeper (`aterm-lab keeper`, which holds its aterm's
    /// lifeline): its command line says so, or could not be read.
    fn keeper(&mut self, pid: i32) -> bool;
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
/// # Errors
/// Refused (the recorded pid is no process of the lab, its witness is not
/// running), the instance did not end, interrupted, or the relaunch did not
/// come up.
fn crash(run: &mut Run, a: &Asked, ops: &mut impl Ops) -> Result<Crashed, Fail> {
    // A WINDOWED LAB WHOSE RECORDED PID IS ITS KEEPER (an armed lab's, which
    // holds its window's lifeline) is not crashed: the keeper's SIGKILL would
    // leave the window's aterm running beside the relaunch. One whose recorded
    // pid is its aterm (`up --window`) is, and its relaunch opens a window.
    let old = run.instance_pid;
    require_instance(run, old, ops)?;
    if crate::lab::windowed(run) && ops.keeper(old) {
        return Err(Fail::usage(format!(
            "crash: lab '{}' runs its instance in a window whose recorded pid {old} may be its keeper's: \
             nothing killed (crash a headless lab, or one brought up with `up --window`)",
            run.tag
        )));
    }
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
    require_instance(run, old, ops)?;
    let tree = ops.tree(old);
    let t_end = ops.now_ms();
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

/// Whether a process whose `ps` command line is `line` may be a window's keeper
/// (`<run>/bin/aterm-lab keeper -- <aterm> …`, the lab's path spaces and all): it
/// names the keeper, or it could not be read.
fn may_be_keeper(line: &str) -> bool {
    line.is_empty() || line.contains("aterm-lab keeper -- ")
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

    fn keeper(&mut self, pid: i32) -> bool {
        may_be_keeper(&crate::procs::command_line(pid))
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
        !crate::procs::alive(pid) || crate::procs::zombie(pid)
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

/// `relaunch [tag]` (fix unit Q1): the lab's instance, which ended by itself (`ctl invoke Quit`,
/// or anything else that ended it), started again on its own state and socket, the way a crash's
/// relaunch is ([`crate::restore::relaunch_instance`], which waits for an `up --fabric` lab's
/// bridge too): a clean quit's next launch, so a reader of the lab's bus sees what a quit and
/// its relaunch leave there. Refused while the recorded instance is still a process of the lab
/// (nothing is ended here: a quit is the lab's own `ctl invoke Quit`, a crash is `crash`'s), or
/// while its egress witness is not running. Returns when the relaunched socket came up.
/// # Errors
/// Refused as above, interrupted, or the relaunch did not come up.
fn relaunch(run: &mut Run, ops: &mut impl Ops) -> Result<i64, Fail> {
    let old = run.instance_pid;
    if ops.names_lab(run, old) {
        return Err(Fail::usage(format!(
            "relaunch: lab '{0}' instance {old} still runs: end it first ({LAB} ctl {0} invoke \
             Quit, or {LAB} crash {0}): nothing started",
            run.tag
        )));
    }
    ops.witness(run)
        .map_err(|e| Fail::die(format!("relaunch: {e}: nothing started")))?;
    let t_end = ops.now_ms();
    let (_, t_up) = ops.relaunch(run, "end", false, t_end)?;
    println!(
        "relaunch: instance {old} had ended; instance {} up at {t_up} ms",
        run.instance_pid
    );
    Ok(t_up)
}

/// `relaunch [tag]`: it takes the tag alone.
pub fn cmd_relaunch(args: &[String]) -> Res {
    let mut words = args.to_vec();
    let tag = take_only_tag(&mut words)?;
    match words.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            print!("{}", crate::USAGE);
            return Ok(0);
        }
        Some(other) => {
            return Err(Fail::usage(format!(
                "relaunch: unknown argument '{other}' (relaunch [tag])"
            )));
        }
    }
    let mut run = Run::load(&tag)?;
    stop::install();
    relaunch(&mut run, &mut Live)?;
    Ok(0)
}

#[cfg(test)]
#[path = "crash_tests.rs"]
mod tests;
