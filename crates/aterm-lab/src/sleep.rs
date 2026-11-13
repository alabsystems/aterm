// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `sleep [tag] --for SECS [--hold SECS] [--at-act [--rule RULE] [--within SECS]]`: a sleep of the Mac of `SECS` seconds, emulated on a lab brought up with its clock (`LAB_CLOCK=1`, [`crate::clock`]).

use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::clock::Record;
use crate::pyjson::J;
use crate::run::{Run, take_only_tag};
use crate::{Fail, LAB, Res, stop};

/// How often the journals are looked at for the act.
const ACT_LOOK: Duration = Duration::from_millis(2);
/// How often the instance's pid is asked to still be the lab's while the act
/// is awaited; it is also checked immediately before each signal.
const PID_LOOK: Duration = Duration::from_secs(1);
/// How often the records are looked at for the step.
const RECORD_LOOK: Duration = Duration::from_millis(20);
/// How long the instance may take to record the step: a hang detector.
const RECORD_WITHIN: Duration = Duration::from_secs(60);
/// `--within`'s default: how long the act may take to come.
const WITHIN_DEFAULT: u64 = 3 * 3600;
/// The longest `--within`, `--hold` and `--for`, in seconds.
const WITHIN_MAX: u64 = 86_400;
const HOLD_MAX_MS: u64 = 86_400_000;
const FOR_MAX: i64 = 30 * 86_400;

/// What `sleep`'s words ask for.
#[derive(Debug, PartialEq, Eq)]
struct Asked {
    /// `--for SECS`: the sleep.
    secs: i64,
    /// `--hold SECS`: how long the instance stays stopped beyond the step, in ms.
    hold_ms: u64,
    /// `--at-act`: the sleep falls at the next act the harness journals.
    at_act: bool,
    /// `--rule RULE`: an act of this rule alone.
    rule: Option<String>,
    /// `--within SECS`: how long the act may take to come.
    within: u64,
}

/// `--hold`'s value: seconds, at most three places after the point, in ms.
fn parse_ms(v: &str) -> Option<u64> {
    let (whole, frac) = v.split_once('.').unwrap_or((v, ""));
    if whole.is_empty()
        || frac.len() > 3
        || !whole
            .bytes()
            .chain(frac.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let frac: u64 = format!("{frac:0<3}").parse().ok()?;
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(1000)?
        .checked_add(frac)
}

/// `sleep`'s words (the tag already taken); `Ok(None)` for `-h`/`--help`.
/// # Errors
/// An unknown word, no `--for`, a value out of its range, or `--rule` or
/// `--within` without `--at-act`.
fn parse_args(args: &[String]) -> Result<Option<Asked>, Fail> {
    let usage = |m: String| Fail::usage(format!("sleep: {m}"));
    let (mut secs, mut hold_ms, mut at_act, mut rule, mut within) = (None, 0, false, None, None);
    let mut words = args.iter();
    while let Some(w) = words.next() {
        let mut value = |what: &str| {
            words
                .next()
                .cloned()
                .ok_or_else(|| usage(format!("{w} needs {what}")))
        };
        match w.as_str() {
            "--for" => {
                let v = value("SECS")?;
                secs = Some(
                    v.bytes()
                        .all(|b| b.is_ascii_digit())
                        .then(|| v.parse::<i64>().ok())
                        .flatten()
                        .filter(|&s| s > 0 && s <= FOR_MAX)
                        .ok_or_else(|| {
                            usage(format!("--for {v}: whole seconds, 1 to {FOR_MAX}"))
                        })?,
                );
            }
            "--hold" => {
                let v = value("SECS")?;
                hold_ms = parse_ms(&v)
                    .filter(|&ms| ms <= HOLD_MAX_MS)
                    .ok_or_else(|| {
                        usage(format!(
                            "--hold {v}: seconds (to the millisecond), at most a day"
                        ))
                    })?;
            }
            "--at-act" => at_act = true,
            "--rule" => rule = Some(value("RULE")?),
            "--within" => {
                let v = value("SECS")?;
                within = Some(
                    v.bytes()
                        .all(|b| b.is_ascii_digit())
                        .then(|| v.parse::<u64>().ok())
                        .flatten()
                        .filter(|&s| s > 0 && s <= WITHIN_MAX)
                        .ok_or_else(|| {
                            usage(format!("--within {v}: whole seconds, 1 to {WITHIN_MAX}"))
                        })?,
                );
            }
            "-h" | "--help" => return Ok(None),
            other => {
                return Err(usage(format!(
                    "unknown argument '{other}' (sleep [tag] --for SECS [--hold SECS] [--at-act [--rule RULE] [--within SECS]])"
                )));
            }
        }
    }
    let secs = secs.ok_or_else(|| usage("--for SECS is needed: how long the Mac sleeps".into()))?;
    if !at_act && (rule.is_some() || within.is_some()) {
        return Err(usage("--rule and --within go with --at-act".into()));
    }
    Ok(Some(Asked {
        secs,
        hold_ms,
        at_act,
        rule,
        within: within.unwrap_or(WITHIN_DEFAULT),
    }))
}

/// The act the sleep falls at: a journal line of the harness.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Act {
    /// When the harness journaled it (ms since the epoch).
    t: i64,
    /// Its rule (`usage-resume@v1`).
    rule: String,
    /// Its text (`CONTINUED seq=… rule=… keep going`).
    line: String,
}

/// A journal line read as an act (`"kind":"continued"`), of `rule` when one is named.
fn act_of(line: &str, rule: Option<&str>) -> Option<Act> {
    let j = J::parse(line).ok()?;
    if j.get("kind")?.as_str()? != "continued" {
        return None;
    }
    let act = Act {
        t: j.get("t").and_then(J::as_i64).unwrap_or(0),
        rule: j.get("phase").and_then(J::as_str).unwrap_or("").to_string(),
        line: j.get("line").and_then(J::as_str).unwrap_or("").to_string(),
    };
    rule.is_none_or(|r| act.rule == r).then_some(act)
}

/// A signal the verb sends the instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sig {
    /// `SIGSTOP`.
    Stop,
    /// `SIGCONT`.
    Cont,
}

/// The process operations a sleep makes, injected so its tests stop nothing and never sleep.
pub(crate) trait Ops {
    /// Whether `pid` is a live process of the lab ([`crate::lab::names_this_lab`]).
    fn names_lab(&mut self, run: &Run, pid: i32) -> bool;
    /// The clock records of the lab's processes ([`crate::clock::records`]).
    fn records(&mut self, run: &Run) -> Vec<Record>;
    /// Whether `pid` lives.
    fn alive(&mut self, pid: i32) -> bool;
    /// The offset now, in whole seconds.
    /// # Errors
    /// It cannot be read.
    fn read_offset(&mut self, run: &Run) -> Result<i64, String>;
    /// Set the offset (by a rename).
    /// # Errors
    /// The write failed.
    fn write_offset(&mut self, run: &Run, secs: i64) -> Result<(), String>;
    /// The lines the instance's harness journaled since the last call: the first call returns none and marks where the journals end now.
    fn journal_lines(&mut self, run: &Run) -> Vec<String>;
    /// Send `sig` to `pid`.
    /// # Errors
    /// The kernel refused it.
    fn signal(&mut self, pid: i32, sig: Sig) -> Result<(), String>;
    /// Now, in milliseconds since the epoch.
    fn now_ms(&mut self) -> i64;
    /// Wait `d`.
    /// # Errors
    /// Interrupted.
    fn sleep(&mut self, d: Duration) -> Result<(), Fail>;
}

/// What a sleep did.
#[derive(Debug, PartialEq, Eq)]
struct Slept {
    /// The act it fell at, and when it was read.
    act: Option<(Act, i64)>,
    /// When the instance was stopped and continued.
    t_stop: i64,
    t_cont: i64,
    /// The offset before and after, in seconds.
    old: i64,
    new: i64,
    /// The live processes whose record says the new offset, and those whose
    /// record does not yet (pid, executable's name).
    took: Vec<(i32, String)>,
    not_yet: Vec<(i32, String)>,
}

/// Look at `pred` every `step` until it holds or `within` has passed (a hang
/// detector): whether it held.
fn poll<O: Ops>(
    ops: &mut O,
    within: Duration,
    step: Duration,
    mut pred: impl FnMut(&mut O) -> bool,
) -> Result<bool, Fail> {
    for _ in 0..within.as_millis() / step.as_millis() {
        if pred(ops) {
            return Ok(true);
        }
        ops.sleep(step)?;
    }
    Ok(pred(ops))
}

/// The next act the harness journals from now, read within `a.within`.
fn await_act(run: &Run, a: &Asked, ops: &mut impl Ops) -> Result<(Act, i64), Fail> {
    let pid = run.instance_pid;
    let _ = ops.journal_lines(run);
    let (mut got, mut gone, mut looks) = (None, false, 0u64);
    let rule = a.rule.as_deref();
    let every = PID_LOOK.as_millis() / ACT_LOOK.as_millis();
    poll(ops, Duration::from_secs(a.within), ACT_LOOK, |o| {
        got = o.journal_lines(run).iter().find_map(|l| act_of(l, rule));
        looks += 1;
        if got.is_none() && u128::from(looks) % every == 0 {
            gone = !o.names_lab(run, pid);
        }
        got.is_some() || gone
    })?;
    if gone {
        return Err(Fail::die(format!(
            "sleep: instance {pid} is no process of the lab any more: nothing stopped"
        )));
    }
    let act = got.ok_or_else(|| {
        Fail::die(format!(
            "sleep: no act{} journaled within {} s: nothing stopped",
            rule.map(|r| format!(" of {r}")).unwrap_or_default(),
            a.within
        ))
    })?;
    let t = ops.now_ms();
    Ok((act, t))
}

/// Re-read the recorded instance's lab membership at the point of use.
fn require_instance(run: &Run, pid: i32, ops: &mut impl Ops) -> Result<(), Fail> {
    if ops.names_lab(run, pid) {
        return Ok(());
    }
    Err(Fail::die(format!(
        "sleep: lab '{}' records instance pid {pid}, which is no process of the lab now: nothing stopped",
        run.tag
    )))
}

/// The sleep of lab `run`.
/// # Errors
/// Refused (above), interrupted, the offset could not be stepped, or the
/// instance did not take the step.
fn sleep(run: &Run, a: &Asked, ops: &mut impl Ops) -> Result<Slept, Fail> {
    let (tag, pid) = (&run.tag, run.instance_pid);
    if run.clock != "1" {
        return Err(Fail::die(format!(
            "sleep: lab '{tag}' was brought up without its clock (LAB_CLOCK=1 {LAB} up {tag}): nothing stopped"
        )));
    }
    require_instance(run, pid, ops)?;
    if !ops.records(run).iter().any(|r| r.pid == pid) {
        return Err(Fail::die(format!(
            "sleep: instance {pid} wrote no clock record: the clock is not loaded in it: nothing stopped"
        )));
    }
    let old = ops
        .read_offset(run)
        .map_err(|e| Fail::die(format!("sleep: {e}: nothing stopped")))?;
    let new = old
        .checked_add(a.secs)
        .ok_or_else(|| Fail::die("sleep: the offset step is out of range: nothing stopped"))?;
    let act = if a.at_act {
        Some(await_act(run, a, ops)?)
    } else {
        None
    };
    let t_stop = ops.now_ms();
    require_instance(run, pid, ops)?;
    ops.signal(pid, Sig::Stop)
        .map_err(|e| Fail::die(format!("sleep: instance {pid} could not be stopped: {e}")))?;
    let held = step_and_hold(run, a, ops, new);
    let t_cont = ops.now_ms();
    let continued = if ops.names_lab(run, pid) {
        ops.signal(pid, Sig::Cont)
    } else {
        Err("the pid is no longer confirmed as this lab's".to_string())
    };
    if let Err(e) = continued {
        // Ended in the hold (a crash a driver made: `crash --keep-tree`, whose next
        // instance starts at the stepped offset): nothing is left to continue.
        if !ops.alive(pid) {
            return Err(Fail::die(format!(
                "sleep: instance {pid} ended while it was stopped ({} ms after the stop): nothing to continue; the offset is {}",
                t_cont - t_stop,
                ops.read_offset(run)
                    .map_or_else(|e| e, |s| format!("{s} s"))
            )));
        }
        return Err(Fail::die(format!(
            "sleep: instance {pid} could not be continued ({e}): its stopped state is unconfirmed"
        )));
    }
    if let Err(f) = held {
        eprintln!(
            "aterm-lab: sleep: instance {pid} continued at {t_cont} ms after {} ms stopped; the offset is {} s",
            t_cont - t_stop,
            ops.read_offset(run).map_or_else(|e| e, |s| s.to_string())
        );
        return Err(f);
    }
    if let Some((act, read)) = &act {
        println!(
            "sleep: the act at {} ms, read at {read} ms: {}",
            act.t, act.line
        );
    }
    println!(
        "sleep: instance {pid} stopped at {t_stop} ms; offset {old} s -> {new} s; continued at {t_cont} ms ({} ms stopped)",
        t_cont - t_stop
    );
    let stepped = poll(ops, RECORD_WITHIN, RECORD_LOOK, |o| {
        o.records(run).iter().any(|r| r.pid == pid && r.at(new))
    })?;
    let (mut took, mut not_yet) = (Vec::new(), Vec::new());
    for r in ops.records(run) {
        if ops.alive(r.pid) {
            let row = (r.pid, r.name().to_string());
            if r.at(new) {
                took.push(row);
            } else {
                not_yet.push(row);
            }
        }
    }
    let list = |rows: &[(i32, String)]| {
        rows.iter()
            .map(|(p, n)| format!("{p} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!("sleep: the step taken by: {}", list(&took));
    if !not_yet.is_empty() {
        println!("sleep: not yet taken by: {}", list(&not_yet));
    }
    if !stepped {
        return Err(Fail::die(format!(
            "sleep: instance {pid} did not record the step within 60 s of its continue"
        )));
    }
    Ok(Slept {
        act,
        t_stop,
        t_cont,
        old,
        new,
        took,
        not_yet,
    })
}

/// [`sleep`] while the instance is stopped: the step, then the hold.
fn step_and_hold(run: &Run, a: &Asked, ops: &mut impl Ops, new: i64) -> Result<(), Fail> {
    ops.write_offset(run, new)
        .map_err(|e| Fail::die(format!("sleep: the offset was not stepped: {e}")))?;
    if a.hold_ms > 0 {
        ops.sleep(Duration::from_millis(a.hold_ms))?;
    }
    Ok(())
}

/// The new lines of a set of journals: where each was read to (`None` until
/// the first look, which reads nothing and marks where each ends).
#[derive(Default)]
struct Tail {
    read_to: Option<HashMap<PathBuf, u64>>,
}

impl Tail {
    /// The whole lines `journals` (each with its length now) gained since the
    /// last look; `read(path, from)` reads one from a place.
    fn lines(
        &mut self,
        journals: &[(PathBuf, u64)],
        mut read: impl FnMut(&Path, u64) -> Option<Vec<u8>>,
    ) -> Vec<String> {
        let Some(read_to) = self.read_to.as_mut() else {
            self.read_to = Some(journals.iter().cloned().collect());
            return Vec::new();
        };
        let mut lines = Vec::new();
        for (p, len) in journals {
            let from = read_to.entry(p.clone()).or_insert(0);
            if *len <= *from {
                continue;
            }
            let Some(buf) = read(p, *from) else {
                continue;
            };
            let Some(end) = buf.iter().rposition(|&b| b == b'\n') else {
                continue;
            };
            *from += end as u64 + 1;
            lines.extend(
                String::from_utf8_lossy(&buf[..end])
                    .lines()
                    .map(str::to_string),
            );
        }
        lines
    }
}

/// The process operations of a real sleep.
#[derive(Default)]
struct Live {
    /// The instance's journals read so far.
    tail: Tail,
}

impl Live {
    /// Every journal of the instance's harness, and its length.
    fn journals(run: &Run) -> Vec<(PathBuf, u64)> {
        let dir = run.state().join("drive");
        let Ok(Some(entries)) = crate::sys::scratch_dir(&dir) else {
            return Vec::new();
        };
        entries
            .into_iter()
            .filter(|(n, k)| {
                *k == crate::sys::EntryKind::File && n.to_string_lossy().ends_with(".journal.jsonl")
            })
            .filter_map(|(n, _)| {
                let p = dir.join(n);
                let (_, meta) = crate::sys::scratch_file_with_meta(&p).ok()??;
                Some((p, meta.len()))
            })
            .collect()
    }
}

impl Ops for Live {
    fn names_lab(&mut self, run: &Run, pid: i32) -> bool {
        crate::lab::names_this_lab(run, pid)
    }

    fn records(&mut self, run: &Run) -> Vec<Record> {
        crate::clock::records(run)
    }

    fn alive(&mut self, pid: i32) -> bool {
        crate::procs::alive(pid)
    }

    fn read_offset(&mut self, run: &Run) -> Result<i64, String> {
        crate::clock::read_offset(run)
    }

    fn write_offset(&mut self, run: &Run, secs: i64) -> Result<(), String> {
        crate::clock::write_offset(run, secs)
    }

    fn journal_lines(&mut self, run: &Run) -> Vec<String> {
        self.tail.lines(&Self::journals(run), |p, from| {
            let (mut f, meta) = crate::sys::scratch_file_with_meta(p).ok()??;
            let len = meta.len();
            let cap = usize::try_from(
                len.saturating_sub(from)
                    .min(64 * 1024 * 1024)
                    .saturating_add(1),
            )
            .unwrap_or(0);
            let mut buf = Vec::with_capacity(cap);
            f.seek(SeekFrom::Start(from)).ok()?;
            f.take(64 * 1024 * 1024).read_to_end(&mut buf).ok()?;
            Some(buf)
        })
    }

    fn signal(&mut self, pid: i32, sig: Sig) -> Result<(), String> {
        let s = match sig {
            Sig::Stop => libc::SIGSTOP,
            Sig::Cont => libc::SIGCONT,
        };
        crate::sys::kill(pid, s).map_err(|e| e.to_string())
    }

    fn now_ms(&mut self) -> i64 {
        crate::util::now_ms()
    }

    fn sleep(&mut self, d: Duration) -> Result<(), Fail> {
        Ok(stop::sleep(d)?)
    }
}

/// `sleep [tag] --for SECS [--hold SECS] [--at-act [--rule RULE] [--within SECS]]`.
pub fn cmd_sleep(args: &[String]) -> Res {
    let mut words = args.to_vec();
    let tag = take_only_tag(&mut words)?;
    let Some(asked) = parse_args(&words)? else {
        print!("{}", crate::USAGE);
        return Ok(0);
    };
    let run = Run::load(&tag)?;
    stop::install();
    sleep(&run, &asked, &mut Live::default())?;
    Ok(0)
}

#[cfg(test)]
#[path = "sleep_tests.rs"]
mod tests;
