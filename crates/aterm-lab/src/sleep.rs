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
                    v.parse::<i64>()
                        .ok()
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
                    v.parse::<u64>()
                        .ok()
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
                let len = crate::sys::scratch_file(&p).ok()??.metadata().ok()?.len();
                Some((p, len))
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
            let mut f = crate::sys::scratch_file(p).ok()??;
            let mut buf = Vec::new();
            f.seek(SeekFrom::Start(from)).ok()?;
            f.read_to_end(&mut buf).ok()?;
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
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(ToString::to_string).collect()
    }

    const INSTANCE: i32 = 4242;
    const CLAUDE: i32 = 4250;

    /// What the fake saw, in order.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Ev {
        Signal(i32, Sig),
        Offset(i64),
        Hold(Duration),
    }

    /// A lab's processes on a clock of their own: nothing is signalled and
    /// nothing sleeps (a sleep moves the clock).
    struct Fake {
        ours: bool,
        /// The instance's pid stops being the lab's at this time.
        ours_until: Option<i64>,
        clock: i64,
        offset: i64,
        /// The offset write fails.
        write_fails: bool,
        /// A sleep this long or longer is interrupted.
        stop_at: Option<Duration>,
        /// The journal: when each line is written.
        journal: Vec<(i64, String)>,
        /// How far the journal was read (`None`: never looked at).
        journal_read: Option<usize>,
        /// Each process's record: the offset it took, from this time on.
        took: Vec<Proc>,
        /// The instance takes a new offset this long after its continue (never: `None`).
        instance_takes_after: Option<i64>,
        /// The instance ends at this time (a crash a driver made in the hold).
        ends_at: Option<i64>,
        stopped: bool,
        ev: Vec<Ev>,
    }

    /// A process of the fake lab: its pid, executable, and each offset it
    /// took with the time it took it.
    type Proc = (i32, &'static str, Vec<(i64, i64)>);

    fn fake() -> Fake {
        Fake {
            ours: true,
            ours_until: None,
            clock: 1_000_000,
            offset: 0,
            write_fails: false,
            stop_at: None,
            journal: Vec::new(),
            journal_read: None,
            took: vec![
                (INSTANCE, "/b/aterm", vec![(0, 0)]),
                (CLAUDE, "/r/clock/claude", vec![(0, 0)]),
            ],
            instance_takes_after: Some(40),
            ends_at: None,
            stopped: false,
            ev: Vec::new(),
        }
    }

    fn lab() -> Run {
        Run {
            tag: "z1".into(),
            instance_pid: INSTANCE,
            clock: "1".into(),
            ..Run::default()
        }
    }

    fn line(t: i64, kind: &str, rule: &str) -> (i64, String) {
        (
            t,
            format!(
                r#"{{"t":{t},"sid":"s-1","kind":"{kind}","phase":"{rule}","seq":7,"line":"{} seq=7 rule={rule} keep going"}}"#,
                kind.to_uppercase()
            ),
        )
    }

    impl Ops for Fake {
        fn names_lab(&mut self, run: &Run, pid: i32) -> bool {
            self.ours
                && pid == run.instance_pid
                && self.alive(pid)
                && self.ours_until.is_none_or(|t| self.clock < t)
        }
        fn records(&mut self, _run: &Run) -> Vec<Record> {
            let now = self.clock;
            self.took
                .iter()
                .map(|(pid, exe, steps)| Record {
                    pid: *pid,
                    ppid: 1,
                    exe: (*exe).into(),
                    offset_ns: steps
                        .iter()
                        .rfind(|(t, _)| *t <= now)
                        .map_or(0, |(_, s)| i128::from(*s) * 1_000_000_000),
                })
                .collect()
        }
        fn alive(&mut self, pid: i32) -> bool {
            pid != INSTANCE || self.ends_at.is_none_or(|t| self.clock < t)
        }
        fn read_offset(&mut self, _run: &Run) -> Result<i64, String> {
            Ok(self.offset)
        }
        fn write_offset(&mut self, _run: &Run, secs: i64) -> Result<(), String> {
            if self.write_fails {
                return Err("a full disk".into());
            }
            self.offset = secs;
            self.ev.push(Ev::Offset(secs));
            let now = self.clock;
            for (pid, _, steps) in &mut self.took {
                if *pid == CLAUDE {
                    steps.push((now + 5, secs));
                }
            }
            Ok(())
        }
        fn journal_lines(&mut self, _run: &Run) -> Vec<String> {
            let now = self.clock;
            let upto = self.journal.iter().filter(|(t, _)| *t <= now).count();
            let from = self.journal_read.replace(upto);
            from.map_or_else(Vec::new, |from| {
                self.journal[from..upto]
                    .iter()
                    .map(|(_, l)| l.clone())
                    .collect()
            })
        }
        fn signal(&mut self, pid: i32, sig: Sig) -> Result<(), String> {
            self.ev.push(Ev::Signal(pid, sig));
            if !self.alive(pid) {
                return Err("No such process (os error 3)".into());
            }
            match sig {
                Sig::Stop => self.stopped = true,
                Sig::Cont => {
                    self.stopped = false;
                    let (now, off) = (self.clock, self.offset);
                    if let Some(after) = self.instance_takes_after {
                        for (p, _, steps) in &mut self.took {
                            if *p == INSTANCE {
                                steps.push((now + after, off));
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        fn now_ms(&mut self) -> i64 {
            self.clock
        }
        fn sleep(&mut self, d: Duration) -> Result<(), Fail> {
            if self.stop_at.is_some_and(|s| d >= s) {
                return Err(Fail::from(stop::Stopped(libc::SIGINT)));
            }
            self.clock += i64::try_from(d.as_millis()).unwrap();
            if self.stopped {
                self.ev.push(Ev::Hold(d));
            }
            Ok(())
        }
    }

    fn asked(w: &[&str]) -> Asked {
        parse_args(&words(w)).unwrap().unwrap()
    }

    /// THE SLEEP: the instance alone is stopped, the offset stepped while it
    /// is, held, and continued; nothing else is signalled.
    #[test]
    fn the_offset_steps_between_a_stop_and_a_continue_of_the_instance_alone() {
        let mut f = fake();
        let s = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap();
        assert_eq!(
            f.ev,
            [
                Ev::Signal(INSTANCE, Sig::Stop),
                Ev::Offset(1800),
                Ev::Hold(Duration::from_secs(3)),
                Ev::Signal(INSTANCE, Sig::Cont),
            ]
        );
        assert_eq!((s.old, s.new, s.t_cont - s.t_stop), (0, 1800, 3000));
        assert_eq!(
            s.took,
            [
                (INSTANCE, "aterm".to_string()),
                (CLAUDE, "claude".to_string())
            ]
        );
        assert!(s.not_yet.is_empty() && s.act.is_none());
        let s = sleep(&lab(), &asked(&["--for", "60"]), &mut f).unwrap();
        assert_eq!((s.old, s.new), (1800, 1860));
    }

    /// AT AN ACT: the instance is stopped the moment the harness journals a
    /// `CONTINUED` line written after the verb began (of the rule named),
    /// never at one written before, nor at another kind or rule.
    #[test]
    fn at_an_act_it_stops_at_the_first_act_journaled_after_it_began() {
        let mut f = fake();
        let t0 = f.clock;
        f.journal = vec![
            line(t0 - 10, "continued", "usage-resume@v1"),
            line(t0 + 1000, "waiting", "usage-resume@v1"),
            line(t0 + 2000, "continued", "keep-going@v1"),
            line(t0 + 1_800_000, "continued", "usage-resume@v1"),
        ];
        let a = asked(&[
            "--for",
            "1800",
            "--hold",
            "3",
            "--at-act",
            "--rule",
            "usage-resume@v1",
        ]);
        let s = sleep(&lab(), &a, &mut f).unwrap();
        let (act, read) = s.act.clone().unwrap();
        assert_eq!(
            (act.t, act.rule.as_str()),
            (t0 + 1_800_000, "usage-resume@v1")
        );
        assert_eq!(act.line, "CONTINUED seq=7 rule=usage-resume@v1 keep going");
        assert!(read - act.t <= 2, "read {} ms after it", read - act.t);
        assert_eq!(s.t_stop, read);
        assert_eq!(f.ev[0], Ev::Signal(INSTANCE, Sig::Stop));
        let mut f = fake();
        let t0 = f.clock;
        f.journal = vec![
            line(t0 - 10, "continued", "usage-resume@v1"),
            line(t0 + 2000, "continued", "keep-going@v1"),
        ];
        let s = sleep(&lab(), &asked(&["--for", "5", "--at-act"]), &mut f).unwrap();
        assert_eq!(s.act.unwrap().0.rule, "keep-going@v1");
    }

    /// AN INTERRUPT IN THE HOLD, OR A STEP THAT COULD NOT BE WRITTEN, STILL
    /// CONTINUES THE CONFIRMED INSTANCE.
    #[test]
    fn an_interrupt_or_a_failed_step_still_continues_the_instance() {
        let mut f = fake();
        f.stop_at = Some(Duration::from_secs(3));
        let e = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap_err();
        assert_eq!(e.stopped, Some(libc::SIGINT));
        assert_eq!(
            f.ev,
            [
                Ev::Signal(INSTANCE, Sig::Stop),
                Ev::Offset(1800),
                Ev::Signal(INSTANCE, Sig::Cont),
            ]
        );
        let mut f = fake();
        f.write_fails = true;
        let e = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap_err();
        assert!(e.msg.unwrap().contains("not stepped"));
        assert_eq!(
            f.ev,
            [
                Ev::Signal(INSTANCE, Sig::Stop),
                Ev::Signal(INSTANCE, Sig::Cont)
            ]
        );
        assert_eq!(f.offset, 0);

        let mut f = fake();
        f.ours_until = Some(f.clock + 1000);
        let e = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap_err();
        assert!(e.msg.unwrap().contains("no longer confirmed"));
        assert_eq!(
            f.ev,
            [
                Ev::Signal(INSTANCE, Sig::Stop),
                Ev::Offset(1800),
                Ev::Hold(Duration::from_secs(3)),
            ]
        );
        assert!(f.alive(INSTANCE));
    }

    /// AN INSTANCE THAT ENDS WHILE IT IS STOPPED (a crash a driver makes in
    /// the hold: `crash --keep-tree`, the next instance up at the stepped
    /// offset) is said to have ended, never to be left stopped: there is
    /// nothing to continue.
    #[test]
    fn an_instance_that_ends_in_the_hold_is_said_gone_not_left_stopped() {
        let mut f = fake();
        f.ends_at = Some(f.clock + 1000);
        let e = sleep(&lab(), &asked(&["--for", "1800", "--hold", "3"]), &mut f).unwrap_err();
        let msg = e.msg.unwrap();
        assert_eq!(e.code, 1);
        assert!(msg.contains("ended while it was stopped"), "{msg}");
        assert!(!msg.contains("left stopped"), "{msg}");
        assert_eq!(
            f.ev,
            [
                Ev::Signal(INSTANCE, Sig::Stop),
                Ev::Offset(1800),
                Ev::Hold(Duration::from_secs(3)),
            ]
        );
        assert_eq!(f.offset, 1800);
    }

    /// THE STEP IS AWAITED IN THE INSTANCE'S OWN RECORD: one that never
    /// records the new offset fails the verb (the instance continued).
    #[test]
    fn the_step_is_awaited_in_the_instance_record() {
        let mut f = fake();
        f.instance_takes_after = Some(59_000);
        assert!(sleep(&lab(), &asked(&["--for", "10"]), &mut f).is_ok());
        let mut f = fake();
        f.instance_takes_after = None;
        let e = sleep(&lab(), &asked(&["--for", "10"]), &mut f).unwrap_err();
        assert!(e.msg.unwrap().contains("did not record the step"));
        assert_eq!(f.ev.last(), Some(&Ev::Signal(INSTANCE, Sig::Cont)));
    }

    /// REFUSED, NOTHING STOPPED AND THE OFFSET AS IT WAS: a lab up without
    /// its clock, a pid that is no longer the lab's, an instance with no
    /// clock record, an act that never came.
    #[test]
    fn a_refusal_stops_nothing_and_steps_nothing() {
        let a = asked(&["--for", "1800"]);
        let mut no_clock = lab();
        no_clock.clock = "0".into();
        let mut f = fake();
        let e = sleep(&no_clock, &a, &mut f).unwrap_err();
        assert!(e.msg.unwrap().contains("without its clock"));
        let mut f = fake();
        f.ours = false;
        assert!(sleep(&lab(), &a, &mut f).is_err());
        assert!(f.ev.is_empty());
        let mut f = fake();
        f.took.retain(|(p, _, _)| *p != INSTANCE);
        let e = sleep(&lab(), &a, &mut f).unwrap_err();
        assert!(e.msg.unwrap().contains("no clock record"));
        assert!(f.ev.is_empty());
        let mut f = fake();
        let t0 = f.clock;
        f.journal = vec![line(t0 - 10, "continued", "usage-resume@v1")];
        let e = sleep(
            &lab(),
            &asked(&["--for", "1800", "--at-act", "--within", "5"]),
            &mut f,
        )
        .unwrap_err();
        assert!(e.msg.unwrap().contains("no act journaled within 5 s"));
        assert!(f.ev.is_empty() && f.offset == 0);
        let mut f = fake();
        let t0 = f.clock;
        f.journal = vec![line(t0 + 600_000, "continued", "usage-resume@v1")];
        f.ours_until = Some(t0 + 60_000);
        let e = sleep(&lab(), &asked(&["--for", "1800", "--at-act"]), &mut f).unwrap_err();
        assert!(e.msg.unwrap().contains("no process of the lab any more"));
        assert!(f.ev.is_empty() && f.offset == 0);
        assert!(f.clock - t0 <= 61_000, "refused {} ms on", f.clock - t0);

        let mut f = fake();
        let t0 = f.clock;
        f.journal = vec![line(t0 + 2, "continued", "usage-resume@v1")];
        f.ours_until = Some(t0 + 2);
        let e = sleep(&lab(), &asked(&["--for", "1800", "--at-act"]), &mut f).unwrap_err();
        assert!(e.msg.unwrap().contains("no process of the lab now"));
        assert!(f.ev.is_empty() && f.offset == 0);
        assert_eq!(f.clock, t0 + 2);

        let mut f = fake();
        f.offset = i64::MAX;
        let e = sleep(&lab(), &asked(&["--for", "1"]), &mut f).unwrap_err();
        assert!(e.msg.unwrap().contains("offset step is out of range"));
        assert!(f.ev.is_empty());
        assert_eq!(f.offset, i64::MAX);

        let mut f = fake();
        f.offset = -1;
        let got = sleep(&lab(), &asked(&["--for", "1"]), &mut f).unwrap();
        assert_eq!((got.old, got.new, f.offset), (-1, 0, 0));
        assert_eq!(f.ev.first(), Some(&Ev::Signal(INSTANCE, Sig::Stop)));
        assert_eq!(f.ev.last(), Some(&Ev::Signal(INSTANCE, Sig::Cont)));
    }

    /// THE JOURNALS' NEW LINES ONLY: the first look reads nothing (what was
    /// written before the verb began is never an act of it), a later one
    /// what was appended since, a journal born since from its start, and a
    /// line still being written only once it ends.
    #[test]
    fn the_journals_are_read_from_where_they_ended_at_the_first_look() {
        let mut files: HashMap<PathBuf, Vec<u8>> = HashMap::new();
        let a = PathBuf::from("/d/s-1.journal.jsonl");
        let b = PathBuf::from("/d/s-2.journal.jsonl");
        files.insert(a.clone(), b"old 1\nold 2\n".to_vec());
        let mut tail = Tail::default();
        let mut look = |files: &HashMap<PathBuf, Vec<u8>>| {
            let lens: Vec<(PathBuf, u64)> = files
                .iter()
                .map(|(p, b)| (p.clone(), b.len() as u64))
                .collect();
            tail.lines(&lens, |p, from| {
                Some(files[p][usize::try_from(from).unwrap()..].to_vec())
            })
        };
        assert!(look(&files).is_empty());
        assert!(look(&files).is_empty());
        files.get_mut(&a).unwrap().extend(b"new 1\nnew 2 half");
        files.insert(b.clone(), b"born 1\n".to_vec());
        let mut got = look(&files);
        got.sort();
        assert_eq!(got, ["born 1", "new 1"]);
        files.get_mut(&a).unwrap().extend(b" done\n");
        assert_eq!(look(&files), ["new 2 half done"]);
        assert!(look(&files).is_empty());
    }

    #[test]
    fn its_words_parse() {
        assert_eq!(
            asked(&[
                "--for", "1800", "--hold", "0.25", "--at-act", "--rule", "r@v1", "--within", "60"
            ]),
            Asked {
                secs: 1800,
                hold_ms: 250,
                at_act: true,
                rule: Some("r@v1".into()),
                within: 60,
            }
        );
        let plain = asked(&["--for", "1"]);
        assert_eq!(
            (plain.hold_ms, plain.within, plain.at_act),
            (0, WITHIN_DEFAULT, false)
        );
        for bad in [
            &[][..],
            &["--for", "0"],
            &["--for", "-5"],
            &["--for", "2592001"],
            &["--for", "10", "--hold", "1.2345"],
            &["--for", "10", "--hold", ".5"],
            &["--for", "10", "--rule", "r@v1"],
            &["--for", "10", "--within", "5"],
            &["--for", "10", "--bogus"],
            &["--for"],
        ] {
            assert_eq!(parse_args(&words(bad)).unwrap_err().code, 2, "{bad:?}");
        }
        assert!(parse_args(&words(&["--help"])).unwrap().is_none());
        assert_eq!(parse_ms("3"), Some(3000));
        assert_eq!(parse_ms("0.2"), Some(200));
    }
}
