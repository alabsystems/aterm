// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `nudge-trial`: repaint-nudge trials on Claude Code ([`crate::relay`]).

use std::fs;
use std::io::Write as _;

use crate::LAB;
use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::J;
use crate::run::{Run, is_run};
use crate::scn::{Ctx, LabKw, ScnErr, results_dir};
use crate::screen::{field, kv};
use crate::stop;
use crate::util::{now, scratch_text};
use crate::{Fail, Res};

/// Any output between the halves of a flap ends the engine's undo (which would hand the rows back).
const MODE_SET: &[u8] = b"\x1b[?1004h";
/// Rows that may differ between two readings of one screen (a spinner frame, a transient hint).
const SLACK: usize = 1;
/// Seconds between two flaps of a lab whose nudge acts: 3 tries per session-hour, a 60 s cooldown.
const SPACING: f64 = 62.0;
/// Seconds a trial waits after the flap before reading the end state.
const OBSERVE: f64 = 8.0;
/// The most a trial waits for a nudge that is still running.
const OBSERVE_CAP: f64 = 30.0;

/// The trial could not mean anything; the message says why.
#[derive(Debug)]
struct CouldNotRun(String);

type R<T> = Result<T, Out>;

#[derive(Debug)]
enum Out {
    CouldNot(CouldNotRun),
    Stopped(stop::Stopped),
}

impl From<stop::Stopped> for Out {
    fn from(s: stop::Stopped) -> Self {
        Out::Stopped(s)
    }
}

fn could_not(msg: impl Into<String>) -> Out {
    Out::CouldNot(CouldNotRun(msg.into()))
}

/// The rows (0-based) at which two screens differ.
#[must_use]
pub fn differing(a: &[String], b: &[String]) -> Vec<usize> {
    let n = a.len().max(b.len());
    (0..n)
        .filter(|&i| a.get(i).map_or("", |s| s.trim_end()) != b.get(i).map_or("", |s| s.trim_end()))
        .collect()
}

/// Row numbers as ranges: `[0, 1, 2, 45, 46]` → `0-2,45-46`.
#[must_use]
pub fn spans(rows: &[usize]) -> String {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &r in rows {
        match out.last_mut() {
            Some(last) if last.1 + 1 == r => last.1 = r,
            _ => out.push((r, r)),
        }
    }
    out.iter()
        .map(|(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}-{b}")
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// One epoch row of `resizes`.
#[derive(Clone, Debug, Default)]
pub struct Epoch {
    /// Its id.
    pub id: i64,
    /// Its `key=value` fields.
    pub fields: Vec<(String, String)>,
    /// `shift=` as a number.
    pub shift: i64,
    /// `pushed=` as a number.
    pub pushed: i64,
}

impl Epoch {
    fn get(&self, k: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    fn word(&self, k: &str) -> String {
        self.get(k).unwrap_or("None").to_string()
    }

    fn to_j(&self) -> J {
        let mut o = J::Obj(
            self.fields
                .iter()
                .map(|(k, v)| (k.clone(), J::s(v.as_str())))
                .collect(),
        );
        o.set("id", J::Int(self.id));
        o.set("shift", J::Int(self.shift));
        o.set("pushed", J::Int(self.pushed));
        o
    }
}

/// `resizes 200`: the header's fields (stderr) and the epochs, newest last (stdout).
#[must_use]
pub fn parse_resizes(stdout: &str, stderr: &str) -> (Vec<(String, String)>, Vec<Epoch>) {
    let head = stderr
        .lines()
        .find(|l| l.contains("render="))
        .map(kv)
        .unwrap_or_default();
    let epochs = stdout
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("epoch ")?;
            let (id, rest) = rest.split_once(' ').unwrap_or((rest, ""));
            let fields = kv(rest);
            let num = |k: &str| {
                fields
                    .iter()
                    .find(|(n, _)| n == k)
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0)
            };
            Some(Epoch {
                id: id.parse().ok()?,
                shift: num("shift"),
                pushed: num("pushed"),
                fields,
            })
        })
        .collect();
    (head, epochs)
}

fn hget(h: &[(String, String)], k: &str) -> Option<String> {
    h.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
}

struct Lab<'a> {
    door: Door<'a>,
    run: &'a Run,
}

impl Lab<'_> {
    fn screen(&self) -> Vec<String> {
        self.door
            .out(&["text"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Two readings 0.7 s apart that agree (nothing animates).
    fn stable_screen(&self) -> R<Vec<String>> {
        let mut prev: Option<Vec<String>> = None;
        for _ in 0..20 {
            let rows = self.screen();
            if prev.as_ref() == Some(&rows) {
                return Ok(rows);
            }
            prev = Some(rows);
            stop::sleep(std::time::Duration::from_millis(700))?;
        }
        Ok(prev.unwrap_or_default())
    }

    fn resizes(&self) -> (Vec<(String, String)>, Vec<Epoch>) {
        let r = self.door.ctl(&["resizes", "200"]).unwrap_or_default();
        parse_resizes(&r.stdout, &r.stderr)
    }

    fn aterm_log(&self) -> Vec<String> {
        scratch_text(&self.run.state().join("logs/aterm.log"))
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn wait_render_ok(&self, secs: f64) -> R<bool> {
        let end = now() + secs;
        while now() < end {
            if hget(&self.resizes().0, "render").as_deref() == Some("ok") {
                return Ok(true);
            }
            stop::sleep(std::time::Duration::from_millis(300))?;
        }
        Ok(false)
    }

    /// `status render=`, re-read while it answers `-` (aterm never waits for the terminal lock).
    fn status_render(&self) -> R<String> {
        let mut word = "-".to_string();
        for _ in 0..6 {
            word = field(&self.door.out(&["status"]), "render").unwrap_or_else(|| "-".into());
            if word != "-" {
                break;
            }
            stop::sleep(std::time::Duration::from_millis(300))?;
        }
        Ok(word)
    }

    fn resize(&self, rows: i64, cols: i64) {
        let _ = self
            .door
            .ctl(&["resize", &rows.to_string(), &cols.to_string()]);
    }
}

/// Parse the `[render] repaint_nudge` value from an `aterm.toml`.
#[must_use]
pub fn configured_nudge(toml: &str) -> Option<String> {
    toml.lines().find_map(|l| {
        let rest = l
            .trim_start()
            .strip_prefix("repaint_nudge")?
            .trim_start()
            .strip_prefix('=')?
            .trim_start();
        let rest = rest.strip_prefix('"')?;
        rest.find('"').map(|i| rest[..i].trim().to_lowercase())
    })
}

/// The lab's PTY relay, found in the instance's process tree by its path.
struct Relay {
    instance: i32,
    exe: String,
    log: std::path::PathBuf,
}

impl Relay {
    fn new(run: &Run) -> Relay {
        Relay {
            instance: run.instance_pid,
            exe: run.run.join("relay/node").to_string_lossy().into_owned(),
            log: run.w.join("relay.log"),
        }
    }

    /// Find the relay process under the instance by command line (`procs::descendants`).
    fn pid(&self) -> R<i32> {
        let found: Vec<i32> = crate::procs::descendants(self.instance)
            .into_iter()
            .filter(|&pid| {
                let cmd = crate::procs::command_line(pid);
                cmd == self.exe || cmd.starts_with(&format!("{} ", self.exe))
            })
            .collect();
        match found.as_slice() {
            [one] => Ok(*one),
            _ => Err(could_not(format!(
                "no single relay of this lab is running ({}: {})",
                self.exe,
                if found.is_empty() {
                    "none".into()
                } else {
                    format!("{found:?}")
                }
            ))),
        }
    }

    fn signal(&self, sig: i32) -> R<()> {
        let pid = self.pid()?;
        let _ = crate::sys::kill(pid, sig);
        Ok(())
    }

    fn lines(&self) -> Vec<String> {
        scratch_text(&self.log)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn mark(&self) -> usize {
        self.lines().len()
    }

    /// SIGUSR1, then until the relay's own log says it holds.
    fn hold(&self) -> R<()> {
        let n = self.mark();
        self.signal(libc::SIGUSR1)?;
        let end = now() + 5.0;
        while now() < end {
            if self
                .lines()
                .get(n..)
                .unwrap_or_default()
                .iter()
                .any(|l| l.ends_with(" HOLD"))
            {
                return Ok(());
            }
            stop::sleep(std::time::Duration::from_millis(20))?;
        }
        Err(could_not("the relay did not confirm the hold"))
    }

    /// Output of the program in the tab: the bytes go into the tab's tty, from the slave side.
    fn write_tty(&self, data: &[u8]) -> R<()> {
        let pid = self.pid()?;
        let tty = crate::procs::tty_of(pid);
        let ok = tty
            .strip_prefix("ttys")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if !ok {
            return Err(could_not(format!("the relay has no tty ('{tty}')")));
        }
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(format!("/dev/{tty}"))
            .map_err(|e| could_not(format!("/dev/{tty}: {e}")))?;
        f.write_all(data)
            .map_err(|e| could_not(format!("/dev/{tty}: {e}")))
    }

    /// The sizes the relay passed on to Claude Code after line `since` of its log.
    fn told(&self, since: usize) -> Vec<String> {
        self.lines()
            .get(since..)
            .unwrap_or_default()
            .iter()
            .filter(|l| l.contains(" told "))
            .filter_map(|l| l.split_whitespace().last().map(str::to_string))
            .collect()
    }
}

/// One trial's readings.
#[derive(Clone, Debug, Default)]
struct Trial {
    k: u32,
    via: String,
    hold: bool,
    rows: i64,
    cols: i64,
    before: Vec<String>,
    error: Option<String>,
    t_flap: f64,
    shot: Vec<String>,
    series: Vec<(f64, (String, String))>,
    after: Vec<String>,
    head: Vec<(String, String)>,
    status_render: String,
    nudged: i64,
    epoch: Option<Epoch>,
    log: Vec<String>,
    frame_rows: Vec<(f64, usize, usize)>,
    told: Vec<String>,
    fresh: Vec<String>,
    shot_rows: usize,
    end_rows: Vec<usize>,
    reference_rows: Vec<usize>,
    verdict: String,
    bad: Vec<String>,
}

impl Trial {
    fn to_j(&self) -> J {
        let rows = |v: &[String]| J::Arr(v.iter().map(|r| J::s(r.as_str())).collect());
        let ints = |v: &[usize]| {
            J::Arr(
                v.iter()
                    .map(|&i| J::Int(i64::try_from(i).unwrap_or(0)))
                    .collect(),
            )
        };
        let mut o = J::obj([
            ("k", J::Int(i64::from(self.k))),
            ("via", J::s(self.via.as_str())),
            ("hold", J::Bool(self.hold)),
            ("rows", J::Int(self.rows)),
            ("cols", J::Int(self.cols)),
            ("before", rows(&self.before)),
        ]);
        if let Some(e) = &self.error {
            o.set("error", J::s(e.as_str()));
        }
        let series = J::Arr(
            self.series
                .iter()
                .map(|(t, (r, n))| {
                    J::Arr(vec![
                        J::Float(*t),
                        J::Arr(vec![J::s(r.as_str()), J::s(n.as_str())]),
                    ])
                })
                .collect(),
        );
        for (k, v) in [
            ("t_flap", J::Float(self.t_flap)),
            ("shot", rows(&self.shot)),
            ("series", series),
            ("after", rows(&self.after)),
            (
                "head",
                J::Obj(
                    self.head
                        .iter()
                        .map(|(k, v)| (k.clone(), J::s(v.as_str())))
                        .collect(),
                ),
            ),
            ("status_render", J::s(self.status_render.as_str())),
            ("nudged", J::Int(self.nudged)),
            ("epoch", self.epoch.as_ref().map_or(J::Null, Epoch::to_j)),
            ("log", rows(&self.log)),
            (
                "frame_rows",
                J::Arr(
                    self.frame_rows
                        .iter()
                        .map(|(t, h, d)| {
                            J::Arr(vec![
                                J::Float(*t),
                                J::Int(i64::try_from(*h).unwrap_or(0)),
                                J::Int(i64::try_from(*d).unwrap_or(0)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("told", rows(&self.told)),
            ("fresh", rows(&self.fresh)),
            (
                "shot_rows",
                J::Int(i64::try_from(self.shot_rows).unwrap_or(0)),
            ),
            ("end_rows", ints(&self.end_rows)),
            ("reference_rows", ints(&self.reference_rows)),
            ("verdict", J::s(self.verdict.as_str())),
            ("bad", rows(&self.bad)),
        ] {
            o.set(k, v);
        }
        o
    }
}

#[derive(Debug)]
struct NoFrame(String);

/// HOLD; shrink one row; output reaches the tab; grow back; RELEASE.
fn flap(
    lab: &Lab<'_>,
    relay: &Relay,
    rows: i64,
    cols: i64,
    hold: bool,
    via: &str,
) -> R<Option<NoFrame>> {
    let inner = (|| -> R<Option<NoFrame>> {
        if hold {
            relay.hold()?;
        }
        lab.resize(rows - 1, cols);
        if via == "key" {
            let shrunk = lab.screen();
            let _ = lab.door.ctl(&["send", "x"]);
            let end = now() + 5.0;
            while lab.screen() == shrunk {
                if now() > end {
                    return Ok(Some(NoFrame("Claude Code drew nothing for the key".into())));
                }
                stop::check()?;
            }
        } else {
            relay.write_tty(MODE_SET)?;
            stop::sleep(std::time::Duration::from_millis(100))?;
        }
        Ok(None)
    })();
    lab.resize(rows, cols);
    if hold {
        let _ = relay.signal(libc::SIGUSR2);
    }
    inner
}

/// Force one displacement and read everything; the judgement is [`judge`]'s.
struct How<'a> {
    rows: i64,
    cols: i64,
    hold: bool,
    via: &'a str,
}

fn trial(ctx: &mut Ctx, lab: &Lab<'_>, relay: &Relay, k: u32, how: &How<'_>) -> R<Trial> {
    let How {
        rows,
        cols,
        hold,
        via,
    } = *how;
    let mut m = Trial {
        k,
        via: via.into(),
        hold,
        rows,
        cols,
        ..Trial::default()
    };
    if !lab.wait_render_ok(15.0)? {
        return Err(could_not(format!(
            "render is not ok before trial {k} (resizes: {})",
            hget(&lab.resizes().0, "render").unwrap_or_else(|| "None".into())
        )));
    }
    m.before = lab.stable_screen()?;
    let (h0, e0) = lab.resizes();
    let (log0, relay0, frames0) = (lab.aterm_log().len(), relay.mark(), ctx.n_frames());
    let epoch0 = e0.iter().map(|e| e.id).max().unwrap_or(0);
    ctx.mark(&format!("trial {k}: flap"), None);
    if let Some(NoFrame(e)) = flap(lab, relay, rows, cols, hold, via)? {
        m.error = Some(e);
    }
    m.t_flap = now();
    ctx.mark(&format!("trial {k}: flap done"), None);
    m.shot = lab.screen();
    loop {
        let (h, _) = lab.resizes();
        let rel = now() - m.t_flap;
        let word = (
            hget(&h, "render").unwrap_or_else(|| "None".into()),
            hget(&h, "nudge").unwrap_or_else(|| "None".into()),
        );
        if m.series.last().map(|s| &s.1) != Some(&word) {
            m.series.push(((rel * 10.0).round() / 10.0, word));
        }
        let nudge = hget(&h, "nudge");
        if (rel >= OBSERVE && (nudge.as_deref() == Some("idle") || nudge.is_none()))
            || rel >= OBSERVE_CAP
        {
            break;
        }
        stop::sleep(std::time::Duration::from_millis(300))?;
    }
    let frames = ctx
        .rec
        .as_ref()
        .map(|r| r.frames_from(frames0))
        .unwrap_or_default();
    if via == "key" {
        let _ = lab.door.ctl(&["key", "backspace"]);
        stop::sleep(std::time::Duration::from_millis(700))?;
    }
    m.after = lab.screen();
    let (h1, e1) = lab.resizes();
    m.status_render = lab.status_render()?;
    let nud = |h: &[(String, String)]| {
        hget(h, "nudged")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0)
    };
    m.nudged = nud(&h1) - nud(&h0);
    m.head = h1;
    m.epoch = e1.into_iter().rev().find(|e| e.id > epoch0);
    m.log = lab
        .aterm_log()
        .get(log0..)
        .unwrap_or_default()
        .iter()
        .filter(|l| l.contains("nudge") || l.contains("render epoch"))
        .cloned()
        .collect();
    m.frame_rows = frames
        .iter()
        .map(|f| {
            (
                ((f.wall - m.t_flap) * 100.0).round() / 100.0,
                f.rows.len(),
                differing(&f.rows, &m.before).len(),
            )
        })
        .collect();
    m.told = relay.told(relay0);
    ctx.mark(&format!("trial {k}: real resize pair"), None);
    for r in [rows - 1, rows] {
        lab.resize(r, cols);
        lab.wait_render_ok(8.0)?;
    }
    m.fresh = lab.stable_screen()?;
    Ok(m)
}

/// PASS, FAIL or INCONCLUSIVE for one trial against the mode expected.
fn judge(expect: &str, m: &mut Trial) -> (String, Vec<String>) {
    let rows_h = usize::try_from(m.rows).unwrap_or(0);
    m.shot_rows = std::iter::once(differing(&m.shot, &m.before).len())
        .chain(
            m.frame_rows
                .iter()
                .filter(|(_, h, _)| *h == rows_h)
                .map(|(_, _, n)| *n),
        )
        .max()
        .unwrap_or(0);
    m.end_rows = differing(&m.after, &m.fresh);
    m.reference_rows = differing(&m.fresh, &m.before);
    let inconclusive = |why: String| ("INCONCLUSIVE".to_string(), vec![why]);
    if let Some(e) = &m.error {
        return inconclusive(e.clone());
    }
    if m.reference_rows.len() > SLACK {
        return inconclusive(format!(
            "a real resize pair left {} rows unlike the screen before the flap: no reference to judge by",
            m.reference_rows.len()
        ));
    }
    let Some(e) = m.epoch.clone() else {
        return inconclusive("aterm opened no epoch: the flap left no rows owed (the engine's undo, or Claude Code repainted)".into());
    };
    if e.get("class") != Some("agent") {
        return inconclusive(format!(
            "aterm does not class the tab as Claude Code (epoch class={})",
            e.word("class")
        ));
    }
    if e.shift == 0 && e.pushed == 0 && e.get("cut").unwrap_or("0") == "0" {
        return inconclusive("the epoch owes nothing (shift=0 pushed=0 cut=0)".into());
    }
    if e.get("verdict") != Some("desync-risk") && e.get("by") != Some("nudge") {
        let why = match e.get("by") {
            Some("app") => {
                "something other than the nudge closed the epoch (a resize or a repaint Claude Code answered)"
            }
            Some("undo") => "the engine's undo handed the rows back",
            _ => "the epoch never reached desync-risk",
        };
        return inconclusive(format!(
            "{why} (epoch verdict={} by={})",
            e.word("verdict"),
            e.word("by")
        ));
    }
    if m.shot_rows <= SLACK {
        return inconclusive(format!(
            "no screen after the flap differed from the baseline (at most {} rows)",
            m.shot_rows
        ));
    }
    if m.status_render == "-" {
        return inconclusive(
            "`status` could not read the render state (render=- six times in two seconds): aterm's terminal lock was busy".into(),
        );
    }
    let count = |n: &str| m.log.iter().filter(|l| l.contains(n)).count();
    let (holds, would, healed) = (
        count(" hold rows="),
        count("would-nudge"),
        count("healed by=nudge"),
    );
    let head_render = hget(&m.head, "render").unwrap_or_else(|| "None".into());
    let mut bad = Vec::new();
    let mut need = |ok: bool, what: String| {
        if !ok {
            bad.push(what);
        }
    };
    if expect == "on" {
        need(
            e.get("verdict") == Some("healed") && e.get("by") == Some("nudge"),
            format!(
                "epoch not `healed by=nudge` (verdict={} by={})",
                e.word("verdict"),
                e.word("by")
            ),
        );
        need(
            head_render == "ok" && m.status_render == "ok",
            format!(
                "render is not ok (resizes {head_render}, status {})",
                m.status_render
            ),
        );
        need(
            m.nudged == 1 && holds == 1 && healed == 1,
            format!(
                "not exactly one nudge (nudged +{}, {holds} holds, {healed} healed lines)",
                m.nudged
            ),
        );
        let (short, truth) = (
            format!("{}x{}", m.rows - 1, m.cols),
            format!("{}x{}", m.rows, m.cols),
        );
        need(
            m.told == [short.clone(), truth.clone()],
            format!(
                "Claude Code was told {}, not {short} then {truth} (a row shorter, then the true size)",
                if m.told.is_empty() {
                    "nothing".into()
                } else {
                    m.told.join(", ")
                }
            ),
        );
        need(
            m.end_rows.len() <= SLACK,
            format!(
                "{} rows still differ from a fresh repaint",
                m.end_rows.len()
            ),
        );
    } else {
        need(
            e.get("verdict") == Some("desync-risk") && e.get("by") == Some("-"),
            format!(
                "epoch not left at desync-risk (verdict={} by={})",
                e.word("verdict"),
                e.word("by")
            ),
        );
        need(
            head_render == "desync-risk" && m.status_render == "desync-risk",
            format!(
                "render is not desync-risk (resizes {head_render}, status {})",
                m.status_render
            ),
        );
        need(
            m.nudged == 0 && holds == 0 && healed == 0,
            format!(
                "the nudge acted (nudged +{}, {holds} holds, {healed} healed lines)",
                m.nudged
            ),
        );
        need(
            m.end_rows.len() > SLACK,
            format!(
                "the screen was repaired ({} rows differ from a fresh repaint)",
                m.end_rows.len()
            ),
        );
        need(
            m.told.is_empty(),
            format!("Claude Code was told a size ({})", m.told.join(", ")),
        );
        let want = usize::from(expect == "log");
        need(
            would == want,
            format!("{would} would-nudge lines, expected {want}"),
        );
    }
    (
        if bad.is_empty() { "PASS" } else { "FAIL" }.to_string(),
        bad,
    )
}

fn report(m: &Trial) {
    let e = m.epoch.clone().unwrap_or_default();
    let ew = |k: &str| {
        if m.epoch.is_some() {
            e.get(k).unwrap_or("-").to_string()
        } else {
            "-".into()
        }
    };
    println!(
        "trial {}  via {}{}",
        m.k,
        m.via,
        if m.hold {
            ""
        } else {
            "  (NO HOLD: the control)"
        }
    );
    println!(
        "  flap        up to {} rows differ from the baseline after it; epoch {} shift={} pushed={} class={}",
        m.shot_rows,
        if m.epoch.is_some() {
            e.id.to_string()
        } else {
            "-".into()
        },
        if m.epoch.is_some() {
            e.shift.to_string()
        } else {
            "-".into()
        },
        if m.epoch.is_some() {
            e.pushed.to_string()
        } else {
            "-".into()
        },
        ew("class")
    );
    let series: Vec<String> = m
        .series
        .iter()
        .map(|(t, (r, n))| format!("{r}/{n}@{t:.1}"))
        .collect();
    println!(
        "  aterm       render/nudge, seconds after the flap: {}",
        if series.is_empty() {
            "-".into()
        } else {
            series.join(" -> ")
        }
    );
    let lines: Vec<String> = m
        .log
        .iter()
        .filter(|l| l.contains("app_nudge"))
        .map(|l| nudge_log_line(l))
        .collect();
    println!(
        "  nudge log   {}",
        if lines.is_empty() {
            "(none)".into()
        } else {
            lines.join("; ")
        }
    );
    let none = |k: &str| {
        if m.epoch.is_some() {
            e.get(k).unwrap_or("None").to_string()
        } else {
            "None".into()
        }
    };
    println!(
        "  end         resizes render={} nudge={} nudged=+{}, status render={}, epoch verdict={} by={} tried={}; {} rows differ from a fresh repaint{}; Claude Code was told {}",
        hget(&m.head, "render").unwrap_or_else(|| "None".into()),
        hget(&m.head, "nudge").unwrap_or_else(|| "None".into()),
        m.nudged,
        m.status_render,
        none("verdict"),
        none("by"),
        none("tried"),
        m.end_rows.len(),
        if m.end_rows.is_empty() {
            String::new()
        } else {
            format!(" (rows {})", spans(&m.end_rows))
        },
        if m.told.is_empty() {
            "nothing".into()
        } else {
            m.told.join(", ")
        }
    );
    println!("  {}", m.verdict);
    for why in &m.bad {
        println!("    - {why}");
    }
}

/// An aterm.log nudge line, its prefix and session/reason/epoch fields cut.
#[must_use]
pub fn nudge_log_line(l: &str) -> String {
    let after = l
        .find("aterm_gui::app_nudge: ")
        .map_or(l, |i| &l[i + "aterm_gui::app_nudge: ".len()..]);
    let after = after.strip_prefix("repaint nudge ").unwrap_or(after);
    after
        .split(' ')
        .filter(|w| {
            !["session=", "reason=", "epoch="].iter().any(|p| {
                w.starts_with(p)
                    && w[p.len()..]
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_')
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn next_run_index() -> u32 {
    1 + fs::read_dir(results_dir())
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    let num = n.strip_prefix("nudge-trial-")?;
                    (num.len() == 2).then(|| num.parse::<u32>().ok())?
                })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// The trial options.
#[derive(Debug)]
struct Opts {
    trials: u32,
    expect: Option<String>,
    via: String,
    no_hold: bool,
}

/// Run the trials on the lab `tag`; the exit status.
fn drive(tag: &str, o: &Opts) -> Result<i32, stop::Stopped> {
    let mut ctx = Ctx::new(
        "nudge-trial",
        next_run_index(),
        tag,
        "",
        LabKw::default(),
        true,
        true,
    );
    let mut results: Vec<Trial> = Vec::new();
    let outcome = (|| -> Result<i32, Out> {
        ctx.start().map_err(|e| match e {
            ScnErr::Stopped(s) => Out::Stopped(s),
            ScnErr::Failed(m) => could_not(m),
        })?;
        let run = ctx.run.clone();
        let lab = Lab {
            door: Door::new(&run).map_err(could_not)?,
            run: &run,
        };
        let word = configured_nudge(&scratch_text(&run.w.join("cfg/aterm/aterm.toml")));
        let expect = o
            .expect
            .clone()
            .or_else(|| word.clone())
            .unwrap_or_else(|| "on".into());
        if !["on", "log", "off"].contains(&expect.as_str()) {
            println!(
                "nudge-trial: the lab's [render] repaint_nudge is {}, which names no expectation: pass --expect on|log|off",
                crate::util::py_repr(word.as_deref().unwrap_or(""))
            );
            return Ok(3);
        }
        let (rows, cols): (i64, i64) = (
            run.rows.parse().unwrap_or(50),
            run.cols.parse().unwrap_or(144),
        );
        let program = field(&lab.door.out(&["ls"]), "program").unwrap_or_else(|| "?".into());
        let program = program.trim_start_matches('-');
        if ["zsh", "bash", "sh"].contains(&program) {
            let l = Launch {
                relay: true,
                envs: vec!["CLAUDE_CODE_NO_FLICKER=1".into()],
                ..Launch::default()
            };
            ctx.launch(120.0, l).map_err(|e| match e {
                ScnErr::Stopped(s) => Out::Stopped(s),
                ScnErr::Failed(m) => could_not(m),
            })?;
        }
        let relay = Relay::new(&run);
        relay.pid()?;
        ctx.put("expect", expect.as_str());
        ctx.put("config", word.clone().map_or(J::Null, J::Str));
        ctx.put("via", o.via.as_str());
        ctx.put("hold", !o.no_hold);
        ctx.put("claude", run.claude_ver.as_str());
        let end = now() + 10.0;
        while now() < end
            && field(&lab.door.out(&["status"]), "agent").unwrap_or_else(|| "-".into()) == "-"
        {
            stop::sleep(std::time::Duration::from_millis(500))?;
        }
        let agent = field(&lab.door.out(&["status"]), "agent").unwrap_or_else(|| "-".into());
        if agent == "-" || !lab.wait_render_ok(30.0)? {
            println!(
                "nudge-trial: COULD NOT RUN: aterm does not class the tab as Claude Code (status agent={agent}) or its render is not ok"
            );
            return Ok(3);
        }
        if field(&lab.door.out(&["modes"]).replace('\n', " "), "alt_screen").as_deref()
            != Some("true")
        {
            println!(
                "nudge-trial: COULD NOT RUN: Claude Code is not on the alternate screen, the only one the nudge acts on (`{LAB} claude TAG --relay` starts it on the fullscreen renderer)"
            );
            return Ok(3);
        }
        let acts = !matches!(word.as_deref(), Some("off" | "log"));
        let gap = if acts { SPACING } else { 4.0 };
        let nudged0: i64 = hget(&lab.resizes().0, "nudged")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if acts && nudged0 + i64::from(o.trials) > 3 {
            println!(
                "nudge-trial: COULD NOT RUN: this lab's nudge has made {nudged0} attempt(s) and may make 3 per hour: use a fresh lab"
            );
            return Ok(3);
        }
        println!(
            "nudge-trial: Claude Code {} behind the PTY relay in {cols}x{rows}; config {}; expect {expect}; {} trial(s) {gap:.0} s apart; {}",
            run.claude_ver,
            word.as_ref().map_or_else(
                || "no [render] key (the default)".to_string(),
                |w| format!("repaint_nudge = {}", crate::util::py_repr(w))
            ),
            o.trials,
            if o.no_hold {
                "NO HOLD (the control: nothing should displace)"
            } else {
                "the relay HOLDS the size changes"
            }
        );
        let mut t_last: Option<f64> = None;
        for k in 1..=o.trials {
            if let Some(t) = t_last {
                stop::until(t + gap)?;
            }
            let how = How {
                rows,
                cols,
                hold: !o.no_hold,
                via: &o.via,
            };
            let mut m = trial(&mut ctx, &lab, &relay, k, &how)?;
            t_last = Some(m.t_flap);
            let (verdict, bad) = judge(&expect, &mut m);
            m.verdict = verdict;
            m.bad = bad;
            report(&m);
            results.push(m);
            ctx.put("trials", J::Arr(results.iter().map(Trial::to_j).collect()));
        }
        let names: Vec<&str> = results.iter().map(|m| m.verdict.as_str()).collect();
        let total = if names.contains(&"FAIL") {
            "FAIL"
        } else if names.contains(&"INCONCLUSIVE") {
            "INCONCLUSIVE"
        } else {
            "PASS"
        };
        let counts: Vec<String> = ["PASS", "FAIL", "INCONCLUSIVE"]
            .iter()
            .filter(|v| names.contains(v))
            .map(|v| format!("{} {v}", names.iter().filter(|n| *n == v).count()))
            .collect();
        println!(
            "nudge-trial: expect {expect}: {total}  ({})",
            counts.join(", ")
        );
        ctx.put("verdict", total);
        if total == "INCONCLUSIVE" {
            println!(
                "  a trial that never held the displacement (or whose epoch something other than the nudge closed) says nothing about the nudge: this run proves neither that it works nor that it does not"
            );
        }
        Ok(match total {
            "PASS" => 0,
            "FAIL" => 1,
            _ => 3,
        })
    })();
    ctx.save_timeline();
    ctx.finish();
    ctx.save_summary();
    if let Some(d) = &ctx.dir {
        println!("results: {}", d.display());
    }
    match outcome {
        Ok(c) => Ok(c),
        Err(Out::Stopped(s)) => Err(s),
        Err(Out::CouldNot(CouldNotRun(why))) => {
            let failed = results.iter().filter(|m| m.verdict == "FAIL").count();
            println!("nudge-trial: COULD NOT RUN: {why}");
            if failed > 0 {
                println!("nudge-trial: {failed} trial(s) FAILED before it stopped (above): exit 1");
                return Ok(1);
            }
            Ok(3)
        }
    }
}

/// Run `nudge-trial` with the given CLI arguments.
pub fn cmd_nudge_trial(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("nudge-trial: {m}"));
    let (mut tag, mut mode) = (None::<String>, None::<String>);
    let mut o = Opts {
        trials: 3,
        expect: None,
        via: "mode".into(),
        no_hold: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |choices: &[&str], msg: &str| {
            it.next()
                .filter(|v| choices.contains(&v.as_str()))
                .cloned()
                .ok_or_else(|| usage(msg))
        };
        match a.as_str() {
            "--mode" => mode = Some(val(&["on", "log", "off"], "--mode is on, log or off")?),
            "--expect" => {
                o.expect = Some(val(&["on", "log", "off"], "--expect is on, log or off")?)
            }
            "--trials" => {
                o.trials = val(
                    &["1", "2", "3"],
                    "--trials is 1, 2 or 3 (the nudge tries three times per session-hour)",
                )?
                .parse()
                .unwrap_or(3);
            }
            "--via" => o.via = val(&["mode", "key"], "--via is mode or key")?,
            "--no-hold" => o.no_hold = true,
            "-h" | "--help" => {
                print!("{}", crate::USAGE);
                return Ok(0);
            }
            s if s.starts_with('-') => {
                return Err(usage(&format!(
                    "unknown option '{s}' (--mode, --trials, --expect, --via, --no-hold)"
                )));
            }
            s => tag = Some(s.to_string()),
        }
    }
    let tag = tag.unwrap_or_else(|| format!("nudge-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    stop::install();
    let mut ours = false;
    if is_run(&tag) {
        if mode.is_some() {
            return Err(usage(&format!(
                "lab '{tag}' is up with its own config: --mode sets the config of a lab this starts (--expect says what to expect of this one)"
            )));
        }
    } else {
        let mut u = UpOpts::from_env();
        u.trust = true;
        u.harness_keys = "continue = false".into();
        u.lifeline = true;
        if let Some(m) = &mode {
            u.aterm_toml = format!("[render]\nrepaint_nudge = \"{m}\"");
        }
        ours = true;
        if let Err(f) = crate::lab::up(&tag, &u) {
            if let Some(m) = &f.msg {
                eprintln!("aterm-lab: {m}");
            }
            eprintln!("aterm-lab: nudge-trial: COULD NOT RUN: the lab did not come up");
            if f.stopped.is_some() {
                return Err(f);
            }
            return Ok(3);
        }
    }
    let rc = drive(&tag, &o);
    if ours {
        match crate::lab::down(&tag, false) {
            Ok(keep) => println!("lab '{tag}' is down; logs kept in {}", keep.display()),
            Err(f) => eprintln!("aterm-lab: {}", f.msg.unwrap_or_default()),
        }
    }
    Ok(rc?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    #[test]
    fn screens_differ_by_the_rows_whose_text_differs() {
        assert_eq!(differing(&v("a\nb\nc"), &v("a\nB\nc\nd")), vec![1, 3]);
        assert_eq!(differing(&v("a  \nb"), &v("a\nb")), Vec::<usize>::new());
        assert_eq!(spans(&[0, 1, 2, 45, 46, 50]), "0-2,45-46,50");
    }

    #[test]
    fn resizes_reads_its_header_and_epochs() {
        let (h, e) = parse_resizes(
            "epoch 1 shift=-1 pushed=0 class=agent verdict=healed by=nudge tried=1\n",
            "aterm-ctl: OK 1 total=1 runs=1 flaps=1 risks=0 render=ok unledgered=0 lost=0 nudge=idle nudged=1\n",
        );
        assert_eq!(hget(&h, "nudged").as_deref(), Some("1"));
        assert_eq!(e.len(), 1);
        assert_eq!(
            (e[0].id, e[0].shift, e[0].get("by")),
            (1, -1, Some("nudge"))
        );
    }

    #[test]
    fn the_configured_nudge_is_the_render_tables_word() {
        assert_eq!(
            configured_nudge("[render]\nrepaint_nudge = \"Off\"\n").as_deref(),
            Some("off")
        );
        assert_eq!(configured_nudge("[update]\nenabled = false\n"), None);
    }

    fn healed_trial() -> Trial {
        let before = v("a\nb\nc");
        Trial {
            k: 1,
            via: "mode".into(),
            hold: true,
            rows: 3,
            cols: 10,
            shot: v("x\ny\nc"),
            after: before.clone(),
            fresh: before.clone(),
            before,
            head: vec![("render".into(), "ok".into())],
            status_render: "ok".into(),
            nudged: 1,
            epoch: Some(
                parse_resizes(
                    "epoch 2 shift=-1 pushed=0 class=agent verdict=healed by=nudge tried=1",
                    "",
                )
                .1
                .remove(0),
            ),
            log: vec![
                "repaint nudge hold rows=2 cols=10".into(),
                "healed by=nudge answer_ms=50".into(),
            ],
            told: vec!["2x10".into(), "3x10".into()],
            ..Trial::default()
        }
    }

    #[test]
    fn a_healed_trial_passes_on_and_fails_off() {
        let mut m = healed_trial();
        assert_eq!(judge("on", &mut m), ("PASS".to_string(), Vec::new()));
        let (v, bad) = judge("off", &mut healed_trial());
        assert_eq!(v, "FAIL");
        assert!(bad.iter().any(|b| b.starts_with("the nudge acted")));
    }

    #[test]
    fn a_trial_that_never_displaced_is_inconclusive() {
        let mut m = healed_trial();
        m.shot = m.before.clone();
        assert_eq!(judge("on", &mut m).0, "INCONCLUSIVE");
        let mut m = healed_trial();
        m.epoch = None;
        assert_eq!(judge("on", &mut m).0, "INCONCLUSIVE");
    }

    #[test]
    fn a_nudge_log_line_is_shown_without_its_prefix_and_ids() {
        assert_eq!(
            nudge_log_line(
                "2026-10-15 INFO aterm_gui::app_nudge: repaint nudge hold rows=49 cols=144 session=3 epoch=1"
            ),
            "hold rows=49 cols=144"
        );
    }
}
