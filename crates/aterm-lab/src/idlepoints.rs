// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab idle-points`: scan harness tapes, casts, and Muse Code session journals for idle points.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aterm_agent::supervise::tape::show;
use aterm_phase::phase::Phase;
use aterm_phase::reader::{Program, ScreenReader};

use crate::halfdraw;
use crate::{Fail, LAB, Res};

/// Maximum gap in seconds to a following busy state for an idle spell to count as in-turn.
pub const TURN_GAP_S: f64 = 3.0;

/// One idle spell after a busy state ([`spells`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Spell {
    /// The cast's event that opened it, and when.
    pub event: usize,
    pub at: f64,
    /// How long it stood: to the next state that read otherwise (to the cast's end for the last).
    pub held: f64,
    /// It lay inside a turn: a busy state followed within [`TURN_GAP_S`], no draft typed in it.
    pub inside: bool,
    /// Its reader vouched for its idle reading.
    pub evidence: bool,
    pub composer: bool,
    /// The program's own window title (OSC 0/2) when the spell opened, if it set one.
    pub title: Option<String>,
    /// Seconds after the spell opened when the window title first stopped indicating activity.
    pub title_rested: Option<f64>,
    pub rows: Vec<String>,
}

/// The window title the last OSC 0 or 2 in `data` sets, if any.
fn osc_title(data: &str) -> Option<String> {
    let mut last = None;
    let mut rest = data;
    while let Some(at) = rest.find("\x1b]") {
        rest = &rest[at + 2..];
        let end = [rest.find('\x07'), rest.find("\x1b\\")]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(rest.len());
        let body = &rest[..end];
        if let Some(t) = body.strip_prefix("0;").or_else(|| body.strip_prefix("2;")) {
            last = Some(t.to_string());
        }
        rest = &rest[end..];
    }
    last
}

/// Every idle spell after a busy state of `cast` as `reader` reads it ([module docs](self)).
pub(crate) fn spells(cast: &halfdraw::Cast, reader: &dyn ScreenReader) -> Vec<Spell> {
    use aterm_core::terminal::{ClockReading, Terminal};
    struct St {
        at: f64,
        busy: bool,
        /// An idle reading: (evidence, composer).
        idle: Option<(bool, bool)>,
        rows: Vec<String>,
        typed: bool,
        title: Option<String>,
    }
    let base = halfdraw::base_instant();
    let mut t = Terminal::new(cast.rows, cast.cols);
    let mut states: Vec<St> = Vec::new();
    let mut title: Option<String> = None;
    for e in &cast.events {
        let at = base + std::time::Duration::from_secs_f64(e.at.max(0.0));
        t.process_at(
            e.data.as_bytes(),
            ClockReading {
                monotonic: at,
                wall_ms: None,
            },
        );
        if let Some(set) = osc_title(&e.data) {
            title = Some(set);
        }
        if t.frame_unfinished_at(at) {
            continue;
        }
        let rows = halfdraw::screen(&t);
        let phase = reader.phase(&rows);
        let composer = reader.composer(&rows);
        let typed = composer
            .as_ref()
            .is_some_and(|(_, text)| text.iter().any(|l| !l.trim().is_empty()));
        let idle = (phase == Phase::Idle).then(|| {
            (
                reader.phase_authoritative(&rows, &phase),
                composer.is_some(),
            )
        });
        states.push(St {
            at: e.at,
            busy: phase == Phase::Busy,
            idle,
            rows,
            typed,
            title: title.clone(),
        });
    }
    let last_at = cast.events.last().map_or(0.0, |e| e.at);
    let mut out = Vec::new();
    let mut i = 1;
    while i < states.len() {
        let Some(kind) = states[i].idle else {
            i += 1;
            continue;
        };
        if !states[i - 1].busy {
            i += 1;
            continue;
        }
        let end = (i..states.len())
            .find(|&j| states[j].idle != Some(kind))
            .unwrap_or(states.len());
        let typed = states[i..end].iter().any(|s| s.typed);
        let next = states.get(end);
        let inside = next.is_some_and(|n| n.busy && !typed && n.at - states[i].at <= TURN_GAP_S);
        let held = next.map_or(last_at, |n| n.at) - states[i].at;
        let title_rested = (i..=end.min(states.len() - 1)).find_map(|j| {
            let runs = reader.title_runs(states[j].title.as_deref()?)?;
            (!runs).then(|| states[j].at - states[i].at)
        });
        out.push(Spell {
            event: i,
            at: states[i].at,
            held,
            inside,
            evidence: kind.0,
            composer: kind.1,
            title: states[i].title.clone(),
            title_rested,
            rows: states[i].rows.clone(),
        });
        i = end.max(i + 1);
    }
    out
}

/// The report of `spells` from the cast named `name`, the first `show` of each kind shown.
#[must_use]
pub fn spell_report(name: &str, spells: &[Spell], show: usize) -> String {
    let mut out = String::new();
    let inside: Vec<&Spell> = spells.iter().filter(|s| s.inside).collect();
    let longest = |of: &[&Spell]| {
        of.iter()
            .map(|s| s.held)
            .fold(None::<f64>, |m, h| Some(m.map_or(h, |m| m.max(h))))
            .map_or_else(|| "-".to_string(), |h| format!("{h:.3} s"))
    };
    let pick = |evidence: bool, composer: Option<bool>| -> Vec<&Spell> {
        inside
            .iter()
            .copied()
            .filter(|s| s.evidence == evidence && composer.is_none_or(|c| s.composer == c))
            .collect()
    };
    let (bare_c, bare_n, vouched) = (
        pick(false, Some(true)),
        pick(false, Some(false)),
        pick(true, None),
    );
    let _ = writeln!(
        out,
        "{name}: idle spells inside a turn: {} no evidence with a composer (longest {}), {} no \
         evidence without one (longest {}), {} with evidence (longest {})",
        bare_c.len(),
        longest(&bare_c),
        bare_n.len(),
        longest(&bare_n),
        vouched.len(),
        longest(&vouched),
    );
    if spells.iter().any(|s| s.title.is_some()) {
        let inside_running = vouched
            .iter()
            .filter(|s| s.title_rested.is_none() && s.title.is_some())
            .count();
        let ends: Vec<&Spell> = spells.iter().filter(|s| !s.inside && s.evidence).collect();
        let rested: Vec<f64> = ends.iter().filter_map(|s| s.title_rested).collect();
        let worst = rested
            .iter()
            .copied()
            .fold(None::<f64>, |m, h| Some(m.map_or(h, |m| m.max(h))));
        let _ = writeln!(
            out,
            "  title: {inside_running} of {} spells with evidence inside a turn ran under a running \
             title throughout; {} of {} turn ends saw the title rest (latest {} after the end drew)",
            vouched.len(),
            rested.len(),
            ends.len(),
            worst.map_or_else(|| "-".to_string(), |w| format!("{w:.3} s")),
        );
    }
    for s in spells
        .iter()
        .filter(|s| !s.inside && s.evidence && s.title.is_some() && s.title_rested.is_none())
        .take(show)
    {
        let _ = writeln!(
            out,
            "  turn end at event {} t={:.3} held {:.3} s: the title never rested ({})",
            s.event,
            s.at,
            s.held,
            s.title.as_deref().unwrap_or("-"),
        );
    }
    for evidence in [false, true] {
        for s in inside.iter().filter(|s| s.evidence == evidence).take(show) {
            let _ = writeln!(
                out,
                "  spell at event {} t={:.3} held {:.3} s evidence={} composer={} title={}",
                s.event,
                s.at,
                s.held,
                if s.evidence { "yes" } else { "no" },
                if s.composer { "yes" } else { "no" },
                s.title.as_deref().unwrap_or("-"),
            );
            let last: Vec<&String> = s.rows.iter().filter(|r| !r.trim().is_empty()).collect();
            for r in &last[last.len().saturating_sub(SHOWN_ROWS)..] {
                let _ = writeln!(out, "    │{r}");
            }
        }
    }
    out
}

/// How many of a shown screen's last non-blank rows are printed.
const SHOWN_ROWS: usize = 8;

/// One point read again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub seq: u64,
    pub phase: String,
    pub evidence: bool,
    /// The journal's word for the point named no last line (`… -`).
    pub nameless: bool,
    pub journal: Option<String>,
    pub outcome: String,
    pub rows: Vec<String>,
}

/// Re-evaluates turn-end decisions in `decisions` using `reader` (or auto-identified reader).
#[must_use]
pub fn points(decisions: &[show::Decision], reader: Option<&dyn ScreenReader>) -> Vec<Point> {
    decisions
        .iter()
        .filter(|d| d.door == "decide.turn_end")
        .filter_map(|d| {
            let rows = d.screen.clone()?;
            let reader = reader.unwrap_or_else(|| aterm_phase::identify(None, &rows));
            let phase = reader.phase(&rows);
            let evidence = reader.phase_authoritative(&rows, &phase);
            let nameless = d
                .journal
                .as_deref()
                .is_some_and(|j| j.starts_with("EVENT idle ") && j.trim_end().ends_with(" -"));
            Some(Point {
                seq: d.seq,
                phase: phase.name().to_string(),
                evidence: evidence || phase != Phase::Idle,
                nameless,
                journal: d.journal.clone(),
                outcome: d.outcome.clone(),
                rows,
            })
        })
        .collect()
}

/// The report of `points` from the tape named `name`, the first `show` with no evidence shown.
#[must_use]
pub fn report(name: &str, points: &[Point], show: usize) -> String {
    let mut out = String::new();
    let idle = points.iter().filter(|p| p.phase == "idle").count();
    let bare = points.iter().filter(|p| !p.evidence).count();
    let nameless = points.iter().filter(|p| p.nameless).count();
    let _ = writeln!(
        out,
        "{name}: {} points, {idle} idle, {bare} with no evidence, {nameless} naming no last line",
        points.len()
    );
    for p in points.iter().filter(|p| !p.evidence).take(show) {
        let _ = writeln!(
            out,
            "  NO EVIDENCE seq={} outcome={} journal={}",
            p.seq,
            p.outcome,
            p.journal.as_deref().unwrap_or("-")
        );
        let last: Vec<&String> = p.rows.iter().filter(|r| !r.trim().is_empty()).collect();
        for r in &last[last.len().saturating_sub(SHOWN_ROWS)..] {
            let _ = writeln!(out, "    │{r}");
        }
    }
    out
}

fn tapes_under(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(rd) = std::fs::read_dir(path) else {
            return;
        };
        let mut kids: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        kids.sort();
        for k in kids {
            tapes_under(&k, out);
        }
    } else if path.extension().is_some_and(|e| e == "atape") {
        out.push(path.to_path_buf());
    }
}

/// The verb.
pub fn cmd_idle_points(args: &[String]) -> Res {
    let usage = || {
        Fail::usage(format!(
            "usage: {LAB} idle-points scan <tape|dir>... --program claude|agy|muse|gemini|auto [--show N]\n\
             \x20      {LAB} idle-points cast <cast>... --program claude|agy|muse|gemini [--show N]\n\
             \x20      {LAB} idle-points gate <lab logs dir>..."
        ))
    };
    let cast = match args.first().map(String::as_str) {
        Some("scan") => false,
        Some("cast") => true,
        Some("gate") => {
            let dirs = &args[1..];
            if dirs.is_empty() || dirs.iter().any(|d| d.starts_with('-')) {
                return Err(usage());
            }
            let mut inside = 0;
            for d in dirs {
                let (text, n) = gate_dir(Path::new(d))?;
                print!("{text}");
                inside += n;
            }
            return Ok(i32::from(inside > 0));
        }
        _ => return Err(usage()),
    };
    let (mut paths, mut program, mut show) = (Vec::new(), None, 0usize);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--program" => {
                let name = it.next().ok_or_else(usage)?;
                program = Some(
                    (name != "auto")
                        .then(|| {
                            Program::ALL
                                .into_iter()
                                .find(|p| p.name() == name)
                                .ok_or_else(|| {
                                    Fail::usage(format!("idle-points: no program '{name}'"))
                                })
                        })
                        .transpose()?,
                );
            }
            "--show" => show = it.next().and_then(|n| n.parse().ok()).ok_or_else(usage)?,
            f if !f.starts_with('-') => paths.push(PathBuf::from(f)),
            _ => return Err(usage()),
        }
    }
    let Some(program) = program else {
        return Err(usage());
    };
    if paths.is_empty() {
        return Err(usage());
    }
    if cast {
        let reader = program
            .ok_or_else(|| Fail::usage("idle-points cast: name the program".to_string()))?
            .reader();
        for p in &paths {
            let text = std::fs::read_to_string(p)
                .map_err(|e| Fail::die(format!("{}: {e}", p.display())))?;
            let c = halfdraw::parse_cast(&text)
                .map_err(|e| Fail::die(format!("{}: {e}", p.display())))?;
            print!(
                "{}",
                spell_report(&p.display().to_string(), &spells(&c, reader), show)
            );
        }
        return Ok(0);
    }
    let mut tapes = Vec::new();
    for p in &paths {
        tapes_under(p, &mut tapes);
    }
    let reader = program.map(Program::reader);
    let (mut total, mut bare, mut nameless) = (0usize, 0usize, 0usize);
    for t in &tapes {
        let bytes = std::fs::read(t).map_err(|e| Fail::die(format!("{}: {e}", t.display())))?;
        let tape = match show::open(&bytes, true) {
            Ok(tape) => tape,
            Err(e) => {
                println!("{}: unreadable: {e}", t.display());
                continue;
            }
        };
        let pts = points(&show::decisions(&tape), reader);
        total += pts.len();
        bare += pts.iter().filter(|p| !p.evidence).count();
        nameless += pts.iter().filter(|p| p.nameless).count();
        print!("{}", report(&t.display().to_string(), &pts, show));
    }
    println!(
        "{} tapes: {total} points, {bare} with no evidence, {nameless} naming no last line",
        tapes.len()
    );
    Ok(0)
}

/// One run of a Muse Code session ([`muse_runs`]), its times in epoch ms.
#[derive(Debug, Clone, PartialEq)]
pub struct MuseRun {
    pub started: f64,
    /// The prompt it started on (empty: a queued input drained into it).
    pub prompt: String,
    /// Its last reply committed: drawn whole on the screen.
    pub committed: Option<f64>,
    pub terminal: Option<f64>,
    /// The end-of-turn gate Muse Code says it held between the reply and the run's end.
    pub eot_gate_ms: Option<u64>,
}

/// Every record of a Muse Code `session.jsonl` line, those a retained frame holds unwrapped.
fn muse_records(line: &str) -> Vec<aterm_json::Value> {
    let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
        return Vec::new();
    };
    match v.get("children").and_then(aterm_json::Value::as_array) {
        Some(children) if v.get("retained_frame").is_some() => children
            .iter()
            .filter_map(|c| c.get("record_json")?.as_str())
            .filter_map(|r| aterm_json::from_str(r).ok())
            .collect(),
        _ => vec![v],
    }
}

/// A Muse Code session journal's first record's time (epoch ms) and its runs, in order.
#[must_use]
pub fn muse_runs(text: &str) -> (Option<f64>, Vec<MuseRun>) {
    let ms = |r: &aterm_json::Value| {
        r.get("recorded_at")
            .and_then(aterm_json::Value::as_u64)
            .map(|us| us as f64 / 1000.0)
    };
    let (mut first, mut runs) = (None::<f64>, Vec::<MuseRun>::new());
    for line in text.lines().filter(|l| l.starts_with('{')) {
        for r in muse_records(line) {
            let at = ms(&r);
            if first.is_none() {
                first = at;
            }
            let (Some(at), Some("run")) = (at, r.pointer("/payload/kind").and_then(|k| k.as_str()))
            else {
                continue;
            };
            let Some(event) = r.pointer("/payload/event") else {
                continue;
            };
            match event.get("kind").and_then(aterm_json::Value::as_str) {
                Some("started") => runs.push(MuseRun {
                    started: at,
                    prompt: event
                        .get("prompt")
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    committed: None,
                    terminal: None,
                    eot_gate_ms: None,
                }),
                Some("assistant_message_committed") => {
                    if let Some(run) = runs.last_mut() {
                        run.committed = Some(at);
                    }
                }
                Some("terminal") => {
                    if let Some(run) = runs.last_mut() {
                        run.terminal = Some(at);
                        run.eot_gate_ms =
                            event.get("eot_gate_ms").and_then(aterm_json::Value::as_u64);
                    }
                }
                _ => {}
            }
        }
    }
    (first, runs)
}

/// A harness idle point of a tab's journal: when it was journaled (epoch ms), its seq and line.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalPoint {
    pub t: f64,
    pub seq: u64,
    pub summary: String,
}

/// A harness journal's first line's time and its idle points (`EVENT idle`).
#[must_use]
pub fn journal_points(text: &str) -> (Option<f64>, Vec<JournalPoint>) {
    let (mut first, mut out) = (None, Vec::new());
    for line in text.lines().filter(|l| l.starts_with('{')) {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let Some(t) = v.get("t").and_then(aterm_json::Value::as_f64) else {
            continue;
        };
        first.get_or_insert(t);
        let field = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if field("kind") == Some("event") && field("phase") == Some("idle") {
            out.push(JournalPoint {
                t,
                seq: v
                    .get("seq")
                    .and_then(aterm_json::Value::as_u64)
                    .unwrap_or(0),
                summary: field("summary").unwrap_or("-").to_string(),
            });
        }
    }
    (first, out)
}

/// Where a point fell against a session's runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Placed {
    /// Before any run started (the program's launch).
    BeforeRuns,
    /// Inside run `run`, before its reply was committed.
    MidReply { run: usize },
    /// Inside run `run`, after its reply was committed and before its end: the gate.
    InGate { run: usize, after_commit: f64 },
    /// After run `run` ended, `after` s past its `terminal`.
    After { run: usize, after: f64 },
}

/// Where `t` (epoch ms) falls against `runs`.
#[must_use]
pub fn place(t: f64, runs: &[MuseRun]) -> Placed {
    let Some(run) = runs.iter().rposition(|r| r.started <= t) else {
        return Placed::BeforeRuns;
    };
    let r = &runs[run];
    match (r.committed, r.terminal) {
        (_, Some(end)) if t >= end => Placed::After {
            run,
            after: (t - end) / 1000.0,
        },
        (Some(c), _) if t >= c => Placed::InGate {
            run,
            after_commit: (t - c) / 1000.0,
        },
        _ => Placed::MidReply { run },
    }
}

/// The report of one tab's points against its session's runs, and how many fell inside a run.
#[must_use]
pub fn gate_report(name: &str, points: &[JournalPoint], runs: &[MuseRun]) -> (String, usize) {
    let mut out = String::new();
    let replies = runs.iter().filter(|r| r.committed.is_some()).count();
    let gates: Vec<u64> = runs
        .iter()
        .filter_map(|r| r.eot_gate_ms)
        .filter(|&g| g > 0)
        .collect();
    let placed: Vec<Placed> = points.iter().map(|p| place(p.t, runs)).collect();
    let afters: Vec<f64> = placed
        .iter()
        .filter_map(|p| match p {
            Placed::After { after, .. } => Some(*after),
            _ => None,
        })
        .collect();
    let in_gate = placed
        .iter()
        .filter(|p| matches!(p, Placed::InGate { .. }))
        .count();
    let mid = placed
        .iter()
        .filter(|p| matches!(p, Placed::MidReply { .. }))
        .count();
    let span = |v: &[f64]| {
        let lo = v.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if v.is_empty() {
            "-".to_string()
        } else {
            format!("+{lo:.3} to +{hi:.3} s")
        }
    };
    let _ = writeln!(
        out,
        "{name}: {replies} replies ({} with an end-of-turn gate, {} ms), {} points: {} after a \
         run's end ({} past its terminal), {in_gate} IN THE GATE, {mid} MID-REPLY, {} before any run",
        gates.len(),
        match (gates.iter().min(), gates.iter().max()) {
            (Some(lo), Some(hi)) => format!("{lo} to {hi}"),
            _ => "-".to_string(),
        },
        points.len(),
        afters.len(),
        span(&afters),
        placed
            .iter()
            .filter(|p| matches!(p, Placed::BeforeRuns))
            .count(),
    );
    for (p, at) in points.iter().zip(&placed) {
        let (run, what) = match at {
            Placed::InGate { run, after_commit } => (
                *run,
                format!("IN THE GATE {after_commit:.3} s after the reply was committed"),
            ),
            Placed::MidReply { run } => (*run, "MID-REPLY".to_string()),
            _ => continue,
        };
        let r = &runs[run];
        let _ = writeln!(
            out,
            "  seq={} {what}; run {} ({:?}) ended {} s after the reply, gate {} ms: {}",
            p.seq,
            run + 1,
            r.prompt,
            match (r.committed, r.terminal) {
                (Some(c), Some(e)) => format!("{:.3}", (e - c) / 1000.0),
                _ => "-".to_string(),
            },
            r.eot_gate_ms
                .map_or_else(|| "-".to_string(), |g| g.to_string()),
            p.summary,
        );
    }
    (out, in_gate + mid)
}

/// Every file named `name` under `dir`, none under a `subagent` folder, sorted.
fn files_named(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n != "subagent") {
                files_named(&p, name, out);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            out.push(p);
        }
    }
}

/// (`subject=land-d`; `-` and none are no name).
fn tape_subject(drive: &Path, sid: &str) -> Option<String> {
    let mut tapes = Vec::new();
    for e in std::fs::read_dir(drive.join("tapes").join(sid))
        .ok()?
        .flatten()
    {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "atape") {
            tapes.push(p);
        }
    }
    tapes.sort();
    tapes.iter().find_map(|p| {
        let tape = show::open(&std::fs::read(p).ok()?, true).ok()?;
        subject_named(&show::skim(&tape))
    })
}

/// The first `subject=<name>` a tape's lines name, `-` and an empty one skipped.
pub fn subject_named(text: &str) -> Option<String> {
    text.split_whitespace()
        .filter_map(|w| w.strip_prefix("subject="))
        .find(|n| !n.is_empty() && *n != "-")
        .map(str::to_string)
}

/// Returns the most frequently referenced `proj/<name>` project folder in a session journal.
pub fn session_project(text: &str) -> Option<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (i, _) in text.match_indices("proj/") {
        let name: String = text[i + "proj/".len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .collect();
        if name.is_empty() {
            continue;
        }
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, k)) => *k += 1,
            None => counts.push((name, 1)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, k)| *k)
        .map(|(name, _)| name)
}

/// Pairs harness tabs and Muse Code sessions by unique shared name, then by start order.
pub fn pair(
    tabs: &[(f64, Option<String>)],
    sessions: &[(f64, Option<String>)],
) -> Vec<(usize, usize, bool)> {
    let unique = |list: &[(f64, Option<String>)], name: &str| {
        list.iter()
            .filter(|(_, n)| n.as_deref() == Some(name))
            .count()
            == 1
    };
    let mut by_tab: Vec<Option<(usize, bool)>> = vec![None; tabs.len()];
    let mut taken = vec![false; sessions.len()];
    for (i, (_, name)) in tabs.iter().enumerate() {
        let Some(name) = name.as_deref() else {
            continue;
        };
        if !unique(tabs, name) || !unique(sessions, name) {
            continue;
        }
        if let Some(j) = sessions
            .iter()
            .position(|(_, n)| n.as_deref() == Some(name))
        {
            by_tab[i] = Some((j, true));
            taken[j] = true;
        }
    }
    let mut rest = (0..sessions.len()).filter(|j| !taken[*j]);
    for slot in &mut by_tab {
        if slot.is_none() {
            *slot = rest.next().map(|j| (j, false));
        }
    }
    by_tab
        .into_iter()
        .enumerate()
        .filter_map(|(i, s)| s.map(|(j, named)| (i, j, named)))
        .collect()
}

/// `gate` over one lab's kept logs (or a live run's directory).
fn gate_dir(dir: &Path) -> Result<(String, usize), Fail> {
    let (drive, sessions) = if dir.join("drive").is_dir() {
        (dir.join("drive"), dir.join("muse-sessions"))
    } else {
        (
            dir.join("scratch/state/drive"),
            dir.join("scratch/home/.local/share/muse/sessions"),
        )
    };
    let read = |p: &Path| {
        std::fs::read_to_string(p).map_err(|e| Fail::die(format!("{}: {e}", p.display())))
    };
    let mut tabs = Vec::new();
    let mut rd: Vec<PathBuf> = std::fs::read_dir(&drive)
        .map_err(|e| Fail::die(format!("{}: {e}", drive.display())))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("s-") && n.ends_with(".journal.jsonl"))
        })
        .collect();
    rd.sort();
    for p in rd {
        let (first, points) = journal_points(&read(&p)?);
        let sid = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".journal.jsonl"))
            .unwrap_or_default()
            .to_string();
        let subject = tape_subject(&drive, &sid);
        tabs.push((first.unwrap_or(f64::MAX), p, points, subject));
    }
    let mut files = Vec::new();
    files_named(&sessions, "session.jsonl", &mut files);
    let mut sess = Vec::new();
    for p in files {
        let text = read(&p)?;
        let (first, runs) = muse_runs(&text);
        sess.push((first.unwrap_or(f64::MAX), p, runs, session_project(&text)));
    }
    tabs.sort_by(|a, b| a.0.total_cmp(&b.0));
    sess.sort_by(|a, b| a.0.total_cmp(&b.0));
    let pairs = pair(
        &tabs.iter().map(|t| (t.0, t.3.clone())).collect::<Vec<_>>(),
        &sess.iter().map(|s| (s.0, s.3.clone())).collect::<Vec<_>>(),
    );
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}: {} tab(s), {} Muse Code session(s), {} paired by name, the rest in the order they started",
        dir.display(),
        tabs.len(),
        sess.len(),
        pairs.iter().filter(|p| p.2).count()
    );
    let (mut inside, mut replies, mut points) = (0, 0, 0);
    for (i, j, named) in pairs {
        let (t0, tab, pts, subject) = &tabs[i];
        let (s0, session, runs, _) = &sess[j];
        let name = format!(
            "{} ~ {} ({}; started {:+.3} s apart)",
            tab.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            session
                .parent()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str())
                .unwrap_or("?"),
            if named {
                format!("by name: {}", subject.as_deref().unwrap_or("?"))
            } else {
                "by order".to_string()
            },
            (t0 - s0) / 1000.0
        );
        let (text, n) = gate_report(&name, pts, runs);
        out.push_str(&text);
        inside += n;
        replies += runs.iter().filter(|r| r.committed.is_some()).count();
        points += pts.len();
    }
    let _ = writeln!(
        out,
        "{}: {replies} replies, {points} points, {inside} inside a run",
        dir.display()
    );
    Ok((out, inside))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(seq: u64, rows: &[&str], journal: &str) -> show::Decision {
        show::Decision {
            door: "decide.turn_end".to_string(),
            item: 0,
            rule: None,
            outcome: "Nothing".to_string(),
            seq,
            screen_record: Some(1),
            screen: Some(rows.iter().map(|r| (*r).to_string()).collect()),
            act: None,
            reply: None,
            journal: Some(journal.to_string()),
        }
    }

    /// assertion.
    #[test]
    fn a_bare_status_row_is_a_point_with_no_evidence() {
        use aterm_phase::muse::fixtures as u;
        use aterm_phase::prompt::fixtures::screen;
        let bare = screen(u::BUSY_FIRST_FRAME_CYCLED);
        let ended = screen(u::END_OF_TURN_1_4_10);
        let bare_refs: Vec<&str> = bare.iter().map(String::as_str).collect();
        let ended_refs: Vec<&str> = ended.iter().map(String::as_str).collect();
        let pts = points(
            &[
                decision(7, &bare_refs, "EVENT idle seq=7 -"),
                decision(9, &ended_refs, "EVENT idle seq=9 Nothing left."),
            ],
            Some(Program::Muse.reader()),
        );
        assert_eq!(pts.len(), 2);
        assert!(!pts[0].evidence, "{:?}", pts[0]);
        assert!(pts[0].nameless);
        assert!(pts[1].evidence, "{:?}", pts[1]);
        assert!(!pts[1].nameless);
        let text = report("t", &pts, 1);
        assert!(
            text.contains("t: 2 points, 2 idle, 1 with no evidence, 1 naming no last line"),
            "{text}"
        );
        assert!(text.contains("NO EVIDENCE seq=7"), "{text}");
        assert!(!text.contains("seq=9 outcome"), "{text}");
    }

    /// shell's 633 marks, a hyperlink's 8) are no title.
    #[test]
    fn the_title_is_the_last_osc_0_or_2() {
        assert_eq!(osc_title("\x1b]0;⠙ proj\x07"), Some("⠙ proj".to_string()));
        assert_eq!(
            osc_title("a\x1b]2;one\x1b\\b\x1b]0;proj\x07c"),
            Some("proj".to_string())
        );
        assert_eq!(osc_title("\x1b]633;P;Cwd=/x\x07\x1b]8;;http://x\x07"), None);
        assert_eq!(osc_title("plain"), None);
    }

    const HM4_JOURNAL: &str = include_str!("fixtures/muse-1.4.10-R6427.1-lab-hm4-t2.journal.jsonl");
    const HM4_SESSION: &str = include_str!("fixtures/muse-1.4.10-R6427.1-lab-hm4-t2.session.jsonl");

    /// pairs them. MUTANT: names ignored.
    #[test]
    fn a_tab_and_its_muse_session_are_paired_by_the_name_both_say() {
        assert_eq!(
            subject_named(
                "#41 status → 0 OK schema=1 sid=7 subject=land-d subject_source=pin observed=true"
            )
            .as_deref(),
            Some("land-d")
        );
        assert_eq!(
            subject_named("0 OK schema=1 sid=7 subject=- subject_source=cwd"),
            None
        );
        assert_eq!(
            session_project(
                r#"{"cwd":"/private/tmp/at.xdoA/proj/land-i"} {"path":"/private/tmp/at.xdoA/proj/land-i/README.md"} {"x":"proj/"}"#
            )
            .as_deref(),
            Some("land-i")
        );
        let named = |n: &str| Some(n.to_string());
        let tabs = [
            (1.0, named("cruise-n")),
            (2.0, named("land-a")),
            (4.0, named("instant-land")),
        ];
        let sessions = [
            (1.5, named("cruise-n")),
            (2.5, named("land-a")),
            (3.0, named("off")),
            (4.5, named("instant-land")),
        ];
        assert_eq!(
            pair(&tabs, &sessions),
            vec![(0, 0, true), (1, 1, true), (2, 3, true)]
        );
        let tabs = [(1.0, None), (2.0, named("proj")), (3.0, named("proj"))];
        let sessions = [
            (1.5, named("proj")),
            (2.5, named("proj")),
            (3.5, named("proj")),
        ];
        assert_eq!(
            pair(&tabs, &sessions),
            vec![(0, 0, false), (1, 1, false), (2, 2, false)]
        );
    }

    #[test]
    fn a_point_in_muses_end_of_turn_gate_is_told_from_the_journals() {
        let (first, runs) = muse_runs(HM4_SESSION);
        assert_eq!(first, Some(1_792_687_536_409.791));
        assert_eq!(runs.len(), 11);
        assert!(
            runs.iter()
                .all(|r| r.committed.is_some() && r.terminal.is_some())
        );
        assert_eq!(runs[1].prompt, "", "round 2 drained a queued input");
        assert_eq!(runs[5].eot_gate_ms, Some(1349));
        let (t0, points) = journal_points(HM4_JOURNAL);
        assert_eq!(t0, Some(1_792_687_537_352.0));
        assert_eq!(points.len(), 11);
        let (text, inside) = gate_report("t2", &points, &runs);
        assert_eq!(inside, 2, "{text}");
        assert!(
            text.contains("t2: 11 replies (10 with an end-of-turn gate, 1171 to 1625 ms), 11 points: 8 after a run's end"),
            "{text}"
        );
        assert!(
            text.contains("2 IN THE GATE, 0 MID-REPLY, 1 before any run"),
            "{text}"
        );
        assert!(
            text.contains("seq=15031 IN THE GATE 0.119 s after"),
            "{text}"
        );
        assert!(
            text.contains("seq=76024 IN THE GATE 1.082 s after"),
            "{text}"
        );
        let later: Vec<JournalPoint> = points
            .iter()
            .map(|p| JournalPoint {
                t: p.t + 1700.0,
                ..p.clone()
            })
            .collect();
        let (text, inside) = gate_report("t2", &later, &runs);
        assert_eq!(inside, 0, "{text}");
        assert!(text.contains("10 after a run's end"), "{text}");
    }

    const EOT_GATE: &str = include_str!("fixtures/muse-1.4.10-R6427.1-lab-eot-gate.cast");

    /// title as running never rests; one that reads none as running rests at once (0 s).
    #[test]
    fn a_muse_turn_end_is_told_by_its_resting_title() {
        let cast = halfdraw::parse_cast(EOT_GATE).expect("the cast");
        let muse = Program::Muse.reader();
        let all = spells(&cast, muse);
        let end = all
            .iter()
            .rfind(|s| s.evidence && !s.inside)
            .expect("the turn's end");
        let title = end.title.as_deref().expect("a title");
        assert!(muse.title_runs(title) == Some(true), "{title:?}");
        let rested = end.title_rested.expect("the title rested");
        assert!((0.5..1.0).contains(&rested), "{rested}");
        assert!(
            end.rows
                .iter()
                .any(|r| r.contains("Hi! I am the local lab stub")),
            "{:?}",
            end.rows
        );
        let text = spell_report("c", &all, 1);
        assert!(
            text.contains("1 of 1 turn ends saw the title rest (latest 0.6"),
            "{text}"
        );
        let mut untitled = halfdraw::parse_cast(EOT_GATE).expect("the cast");
        for e in &mut untitled.events {
            while let Some(at) = e.data.find("\x1b]0;") {
                let close = e.data[at..]
                    .find('\x07')
                    .map_or(e.data.len(), |c| at + c + 1);
                e.data.replace_range(at..close, "");
            }
        }
        let bare = spells(&untitled, muse);
        assert!(
            bare.iter()
                .all(|s| s.title.is_none() && s.title_rested.is_none())
        );
        assert!(!spell_report("c", &bare, 1).contains("title:"));
        let claude = spells(&cast, Program::Claude.reader());
        assert!(claude.iter().all(|s| s.title_rested.is_none()));
    }
}
