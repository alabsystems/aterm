// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CORPUS'S TRANSCRIPT SLICES (plan 5B-2): every `tx …
//! slice=<file>:<a>-<b>` line of aterm-slog's `.seq` replay corpus
//! (`crates/aterm-slog/tests/sequences`, read from the directory: a file
//! added there is checked here) names lines `a..=b` of a
//! `supervise/fixtures` transcript, or of a measured build's committed
//! reduction when `<file>` is that build (`2.1.303:51-60`), and the lines
//! of a `.seq` file naming
//! one slice (in file order; a `see` or an `expect` may stand between them)
//! are exactly the steps this fold ([`StepFold`], bound at the fixture's
//! start, floor 0) makes of those lines, the fixture folded ONCE, whole, as
//! the tap reads a file (each step falls to the line its offset lies in, so
//! a slice holds only what a continuous read emits: a model restated by a
//! fresh fold is no step): the same kinds in the same order, the same closed
//! words (a tool's name and class, a subagent, an error, a wall's kind, a
//! model) and numbers, ids that map one to one across the `.seq` file (the
//! corpus names its own: `v1` for a fixture's `promptId`), and the row as
//! the real reader hands it on:
//!
//! - its STATE: the `.seq` rig's `state` at the line (`on` until a `state`
//!   line says otherwise) is the step's ([`StepState`], which the glue
//!   gives the tracker as it is, `session_log_transcript`'s `tx_state`):
//!   `partial` for every build [`MEASURED_BUILDS`] does not name;
//! - its POSITION: the line's `off=` is the step's offset, and its
//!   `dated=` the step's time, both stated, the times relative (one shift
//!   per fixture across the `.seq` file, so the gaps between rows are the
//!   ones Claude Code wrote), and no row is read (`at`, `wait`, `pass`)
//!   before it was written.
//!
//! So the transcript a `.seq` replays through the session log is one Claude
//! Code wrote, in the state and at the places the real reader applies it.
//!
//! NEGATIVE CONTROLS: a slice's lines with an event's kind, tool or number
//! changed, an event dropped or added, an id mapped two ways, a state, an
//! offset or a gap changed, a position left unstated, and a row read before
//! it was written each fail.

use std::collections::BTreeMap;

use super::*;

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../aterm-slog/tests/sequences");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/supervise/fixtures");
const MEASURED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/harness/testdata/steps");

/// The file a slice's `<file>` names: a measured build's committed
/// reduction (`harness/testdata/steps/<build>.jsonl`, ruling T8) when it is
/// a build [`MEASURED_BUILDS`] names, so the corpus replays the exact path
/// on the rows the lab read; else a `supervise/fixtures` transcript.
fn fixture_path(file: &str) -> String {
    if MEASURED_BUILDS.contains(&file) {
        format!("{MEASURED}/{file}.jsonl")
    } else {
        format!("{FIXTURES}/{file}.jsonl")
    }
}
const FP: u64 = 0x5eed_0000_0000_0005;
const ID: Option<(u64, u64)> = Some((16_777_220, 5));

/// An event as the corpus writes a row: its kind, its id, and its other
/// words and numbers (sorted).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Said {
    kind: String,
    id: Option<String>,
    words: Vec<String>,
}

/// A sliced `tx` line: its event, the rig's state at it, its stated
/// position (`dated=`, `off=`) and when the replay reads it (test ms).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    said: Said,
    state: String,
    dated: Option<i64>,
    off: Option<u64>,
    read_ms: i64,
}

/// A step of the fold: its event, state and position, and the fixture line
/// (1-based) its offset lies in.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Folded {
    said: Said,
    state: &'static str,
    at_ms: u64,
    off: u64,
    line: usize,
}

/// A step's state as the `.seq` rig names it (`state on|partial`): the
/// glue hands it to the tracker as it is (`tx_state`).
const fn state_word(s: StepState) -> &'static str {
    match s {
        StepState::On => "on",
        StepState::Partial => "partial",
    }
}

/// A `tx` line's event, the corpus's defaults filled in (a tool is `Bash`
/// of class `shell`; a turn end 4200 ms, 900 tokens in, 120 out).
fn said_of_line(toks: &[&str]) -> Result<Said, String> {
    let (mut kv, mut bare) = (BTreeMap::new(), Vec::new());
    for t in toks {
        match t.split_once('=') {
            Some((k, _)) if ["dated", "off", "slice"].contains(&k) => {}
            Some((k, v)) => {
                kv.insert(k, v);
            }
            None => bare.push(*t),
        }
    }
    let get = |k: &str, d: &str| format!("{k}={}", kv.get(k).copied().unwrap_or(d));
    let (kind, id, mut words) = match bare.as_slice() {
        ["prompt", v] => ("prompt", Some(*v), vec![]),
        ["interrupt"] => ("interrupt", None, vec![]),
        ["tool", c, rest @ ..] => {
            let mut w = vec![get("tool", "Bash"), get("cls", "shell")];
            w.extend(rest.iter().map(|s| (*s).to_string()));
            ("tool", Some(*c), w)
        }
        ["result", c, rest @ ..] => (
            "result",
            Some(*c),
            rest.iter().map(|s| (*s).to_string()).collect(),
        ),
        ["end"] => (
            "end",
            None,
            vec![get("vdur", "4200"), get("in", "900"), get("out", "120")],
        ),
        ["endwall", k] => ("endwall", None, vec![(*k).to_string()]),
        ["compact"] => ("compact", None, vec![]),
        ["model"] => (
            "model",
            None,
            ["model", "effort", "ver"]
                .iter()
                .filter_map(|k| kv.get(k).map(|v| format!("{k}={v}")))
                .collect(),
        ),
        other => {
            return Err(format!(
                "`tx {other:?}` is no row a transcript fixture writes"
            ));
        }
    };
    words.sort();
    Ok(Said {
        kind: kind.to_string(),
        id: id.map(str::to_string),
        words,
    })
}

/// A fold's event as the corpus would write it (`None`: the bind).
fn said_of_event(e: &StepEvent) -> Option<Said> {
    let (kind, id, mut words): (&str, Option<String>, Vec<String>) = match e {
        StepEvent::Bind { .. } => return None,
        StepEvent::Prompt { vref } => ("prompt", Some(vref.as_str().to_string()), vec![]),
        StepEvent::Interrupt => ("interrupt", None, vec![]),
        StepEvent::ToolUse {
            id,
            tool,
            cls,
            subagent,
        } => {
            let mut w = vec![format!("tool={}", tool.as_str()), format!("cls={cls}")];
            if *subagent {
                w.push("subagent".to_string());
            }
            ("tool", Some(id.as_str().to_string()), w)
        }
        StepEvent::ToolResult { id, error } => (
            "result",
            Some(id.as_str().to_string()),
            if *error {
                vec!["error".to_string()]
            } else {
                vec![]
            },
        ),
        StepEvent::TurnEnd {
            vdur_ms,
            tok_in,
            tok_out,
        } => (
            "end",
            None,
            vec![
                format!("vdur={vdur_ms}"),
                format!("in={tok_in}"),
                format!("out={tok_out}"),
            ],
        ),
        StepEvent::EndWall { kind } => ("endwall", None, vec![kind.name().to_string()]),
        StepEvent::Compact => ("compact", None, vec![]),
        StepEvent::Model { model, effort, ver } => {
            let mut w = vec![format!("model={}", model.as_str())];
            w.extend(effort.iter().map(|e| format!("effort={}", e.as_str())));
            w.extend(ver.iter().map(|v| format!("ver={}", v.as_str())));
            ("model", None, w)
        }
        StepEvent::Off { why } => ("off", None, vec![(*why).to_string()]),
    };
    words.sort();
    Some(Said {
        kind: kind.to_string(),
        id,
        words,
    })
}

/// The fixture `file` folded once, whole, from a bind at its start: every
/// step it makes (a held prompt's included), each with the line its offset
/// lies in.
fn fold_whole(file: &str) -> Result<Vec<Folded>, String> {
    let path = fixture_path(file);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let mut fold = StepFold::new(0, None);
    fold.read_at(FP, ID, 0, false);
    let (mut end, mut ends) = (0u64, Vec::new());
    for line in text.lines() {
        end += line.len() as u64 + 1;
        ends.push(end);
        fold.push(line.as_bytes(), end);
    }
    let mut steps = fold.take().steps;
    steps.extend(fold.take().steps);
    steps
        .iter()
        .filter_map(|s| said_of_event(&s.ev).map(|said| (s, said)))
        .map(|(s, said)| {
            // Line i (1-based) holds the offsets past line i-1's end, up to
            // its own.
            let line = ends.partition_point(|&e| e < s.pos.off) + 1;
            if line > ends.len() || s.pos.off == 0 {
                return Err(format!("{file}: a step at {} lies in no line", s.pos.off));
            }
            Ok(Folded {
                said,
                state: state_word(s.state),
                at_ms: s.pos.at_ms,
                off: s.pos.off,
                line,
            })
        })
        .collect()
}

/// Lines `a..=b` (1-based) of the fixture `file`, folded with the whole
/// file ([`fold_whole`]): the steps that fall to those lines.
fn fold(file: &str, a: usize, b: usize) -> Result<Vec<Folded>, String> {
    let whole = fold_whole(file)?;
    let path = fixture_path(file);
    let n = std::fs::read_to_string(&path)
        .map_err(|e| format!("{path}: {e}"))?
        .lines()
        .count();
    if b > n || a == 0 || a > b {
        return Err(format!("{file} has {n} lines, not {a}-{b}"));
    }
    Ok(whole
        .into_iter()
        .filter(|f| (a..=b).contains(&f.line))
        .collect())
}

/// The lines of one `.seq` file that name one slice, in file order (other
/// lines, a `see` or an `expect`, may stand between them).
#[derive(Clone, Debug)]
struct Run {
    file: String,
    line: usize,
    slice: (String, usize, usize),
    said: Vec<Row>,
}

/// A `key=<n>` token of `toks`, parsed (`Ok(None)`: not there).
fn opt_num<T: std::str::FromStr>(toks: &[&str], key: &str) -> Result<Option<T>, String> {
    toks.iter()
        .find_map(|t| t.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
        .map(|v| v.parse().map_err(|_| format!("`{key}={v}` is no number")))
        .transpose()
}

/// The slice runs of one `.seq` file's text, in the order each slice is
/// first named, each row with the rig's state and the replay's time at it.
fn runs(name: &str, text: &str) -> Result<Vec<Run>, String> {
    let mut out: Vec<Run> = Vec::new();
    let mut named: BTreeMap<String, usize> = BTreeMap::new();
    let (mut state, mut now) = ("on".to_string(), 0i64);
    for (i, raw) in text.lines().enumerate() {
        let at = |e: String| format!("{name}:{}: {e}", i + 1);
        let mut l = raw.trim();
        if let Some(rest) = l.strip_prefix("at ") {
            let (ms, r) = rest.split_once(' ').unwrap_or((rest, ""));
            now = now.max(ms.parse().map_err(|_| at(format!("`at {ms}`")))?);
            l = r.trim();
        }
        let toks: Vec<&str> = l.split_whitespace().collect();
        match toks.as_slice() {
            ["state", w] => state = (*w).to_string(),
            ["wait" | "pass", ms] => now += ms.parse::<i64>().map_err(|_| at(format!("`{l}`")))?,
            ["adopt", rest @ ..] => now += opt_num::<i64>(rest, "after").map_err(at)?.unwrap_or(0),
            _ => {}
        }
        let slice = (toks.first() == Some(&"tx"))
            .then(|| toks.iter().find_map(|t| t.strip_prefix("slice=")))
            .flatten();
        let Some(slice) = slice else { continue };
        let said = Row {
            said: said_of_line(&toks[1..]).map_err(at)?,
            state: state.clone(),
            dated: opt_num(&toks, "dated").map_err(at)?,
            off: opt_num(&toks, "off").map_err(at)?,
            read_ms: now,
        };
        if let Some(&k) = named.get(slice) {
            out[k].said.push(said);
            continue;
        }
        let (file, range) = slice
            .rsplit_once(':')
            .ok_or_else(|| at(format!("`slice={slice}` names no lines")))?;
        let (a, b) = range
            .split_once('-')
            .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            .ok_or_else(|| at(format!("`slice={slice}` is no range")))?;
        named.insert(slice.to_string(), out.len());
        out.push(Run {
            file: name.to_string(),
            line: i + 1,
            slice: (file.to_string(), a, b),
            said: vec![said],
        });
    }
    Ok(out)
}

/// Each run of `runs` folds to what it says, its ids one to one across the
/// runs (one `.seq` file's), each row in the step's state, at its offset,
/// dated at its time (one shift per fixture across the runs) and read no
/// earlier.
fn check(runs: &[Run]) -> Result<(), String> {
    let (mut fwd, mut back): (BTreeMap<String, String>, BTreeMap<String, String>) =
        Default::default();
    let mut shift: BTreeMap<String, i64> = BTreeMap::new();
    for r in runs {
        let (f, a, b) = &r.slice;
        let got = fold(f, *a, *b)?;
        let at = format!("{}:{} (slice {f}:{a}-{b})", r.file, r.line);
        if got.len() != r.said.len() {
            return Err(format!(
                "{at}: the slice folds to {got:?}, the run says {:?}",
                r.said
            ));
        }
        for (g, row) in got.iter().zip(&r.said) {
            let s = &row.said;
            let g_said = &g.said;
            if (&g_said.kind, &g_said.words) != (&s.kind, &s.words)
                || g_said.id.is_some() != s.id.is_some()
            {
                return Err(format!(
                    "{at}: the slice folds to {g:?}, the line says {s:?}"
                ));
            }
            if let (Some(gi), Some(si)) = (&g_said.id, &s.id) {
                let one = fwd.entry(si.clone()).or_insert_with(|| gi.clone()) == gi;
                let other = back.entry(gi.clone()).or_insert_with(|| si.clone()) == si;
                if !one || !other {
                    return Err(format!("{at}: id {si} and the fixture's {gi} map two ways"));
                }
            }
            if row.state != g.state {
                return Err(format!(
                    "{at}: `{} {}` is replayed at state {}, the real reader hands it on {}",
                    s.kind, g.line, row.state, g.state
                ));
            }
            let (Some(off), Some(dated)) = (row.off, row.dated) else {
                return Err(format!(
                    "{at}: `{}` states no off= and dated= (the step's are off={} at {} ms)",
                    s.kind, g.off, g.at_ms
                ));
            };
            if off != g.off {
                return Err(format!(
                    "{at}: `{}` says off={off}, the step lies at off={}",
                    s.kind, g.off
                ));
            }
            let real = i64::try_from(g.at_ms).map_err(|_| format!("{at}: a time past i64"))?;
            let base = *shift.entry(f.clone()).or_insert(real - dated);
            if real - base != dated {
                return Err(format!(
                    "{at}: `{}` says dated={dated}, the step's time is dated={} (the file's                      shift {base})",
                    s.kind,
                    real - base
                ));
            }
            if row.read_ms < dated {
                return Err(format!(
                    "{at}: `{}` is read at {} ms, before it was written at {dated}",
                    s.kind, row.read_ms
                ));
            }
        }
    }
    Ok(())
}

/// Every `.seq` file of the corpus, by name.
fn corpus() -> Vec<(String, String)> {
    let mut all: Vec<(String, String)> = std::fs::read_dir(CORPUS)
        .expect("the corpus")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "seq"))
        .map(|p| {
            let name = p
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned();
            (name, std::fs::read_to_string(&p).expect("a corpus file"))
        })
        .collect();
    all.sort();
    all
}

/// Every slice the corpus names folds to the events its lines state.
#[test]
fn every_corpus_slice_folds_to_its_lines() {
    let (mut n, mut events) = (0, 0);
    for (name, text) in corpus() {
        let rs = runs(&name, &text).unwrap_or_else(|e| panic!("{e}"));
        check(&rs).unwrap_or_else(|e| panic!("{e}"));
        n += rs.len();
        events += rs.iter().map(|r| r.said.len()).sum::<usize>();
    }
    assert!(
        n >= 4 && events >= 8,
        "the corpus names {n} slices of {events} events"
    );
}

/// NEGATIVE CONTROLS: a slice's lines changed in each way fail, each for
/// its own reason.
#[test]
fn a_slice_with_the_wrong_event_fails() {
    let files: Vec<Vec<Run>> = corpus()
        .iter()
        .map(|(n, t)| runs(n, t).expect("runs"))
        .filter(|rs| !rs.is_empty())
        .collect();
    let all: Vec<&Run> = files.iter().flatten().collect();
    let run = *all
        .iter()
        .find(|r| r.said.iter().any(|s| s.said.kind == "tool"))
        .expect("a sliced tool call");
    check(std::slice::from_ref(run)).expect("the shipping lines");
    let i = run
        .said
        .iter()
        .position(|s| s.said.kind == "tool")
        .expect("a tool");
    let fails = |f: &dyn Fn(&mut Vec<Row>), why: &str, what: &str| {
        let mut r = run.clone();
        f(&mut r.said);
        let e = check(&[r]).expect_err(what);
        assert!(e.contains(why), "{what}: {e}");
    };
    fails(
        &|s| s[i].said.kind = "result".into(),
        "folds to",
        "a kind changed",
    );
    fails(
        &|s| s[i].said.words = vec!["cls=edit".into(), "tool=Edit".into()],
        "folds to",
        "a tool renamed",
    );
    fails(
        &|s| {
            s.remove(i);
        },
        "the run says",
        "an event dropped",
    );
    fails(&|s| s.push(s[i].clone()), "the run says", "an event added");
    // The row as the real reader hands it on: its state, its place, its time.
    fails(&|s| s[i].state = "on".into(), "at state on", "replayed On");
    fails(
        &|s| s[i].off = s[i].off.map(|o| o + 1),
        "the step lies at",
        "an offset moved",
    );
    fails(
        &|s| s[i].off = None,
        "states no off=",
        "an offset left unstated",
    );
    fails(
        &|s| s[i].dated = None,
        "states no off=",
        "a time left unstated",
    );
    // The last row's gap from the first (which fixes the file's shift), the
    // row read no earlier than it now says.
    let last = run.said.len() - 1;
    assert!(last > 0, "a slice of more than one row");
    fails(
        &|s| {
            s[last].dated = s[last].dated.map(|d| d + 7);
            s[last].read_ms += 7;
        },
        "the step's time is",
        "a gap changed",
    );
    fails(
        &|s| s[i].read_ms = s[i].dated.expect("dated") - 1,
        "before it was written",
        "a row read before it was written",
    );
    let ended = *all
        .iter()
        .find(|r| r.said.iter().any(|s| s.said.kind == "end"))
        .expect("a sliced turn end");
    check(std::slice::from_ref(ended)).expect("the shipping lines");
    let mut late = ended.clone();
    for s in late.said.iter_mut().filter(|s| s.said.kind == "end") {
        s.said.words = vec!["in=1".into(), "out=1".into(), "vdur=1".into()];
    }
    let e = check(&[late]).expect_err("a number changed");
    assert!(e.contains("folds to"), "a number changed: {e}");
    // One corpus id for two prompts the fixture tells apart.
    let rs = files
        .iter()
        .find(|rs| {
            rs.iter()
                .filter(|r| r.said.iter().any(|s| s.said.kind == "prompt"))
                .count()
                >= 2
        })
        .expect("a corpus file whose prompts come from two slices");
    check(rs).expect("the shipping lines");
    let mut twice: Vec<Run> = rs
        .iter()
        .filter(|r| r.said.iter().any(|s| s.said.kind == "prompt"))
        .cloned()
        .collect();
    let first = twice[0]
        .said
        .iter()
        .find(|s| s.said.kind == "prompt")
        .and_then(|s| s.said.id.clone())
        .expect("a prompt id");
    for s in twice[1].said.iter_mut().filter(|s| s.said.kind == "prompt") {
        s.said.id = Some(first.clone());
    }
    let e = check(&twice).expect_err("an id mapped two ways");
    assert!(e.contains("map two ways"), "an id mapped two ways: {e}");
}

/// A fixture folded whole hands on no restatement a fresh fold per slice
/// would make: the limit fixture's third turn (lines 10-12) runs on the
/// model its first named, so a continuous read emits no `model` there, and
/// every step is Partial (no build is measured).
#[test]
fn a_whole_fold_restates_nothing_and_is_partial() {
    const LIMIT: &str = "claude-2.1.299-transcript-session-limit-cancelled-then-person-turn";
    let whole = fold_whole(LIMIT).expect("the fixture");
    let third = fold(LIMIT, 10, 12).expect("the third turn");
    assert!(
        third.iter().any(|f| f.said.kind == "prompt"),
        "the third turn folds to its prompt: {third:?}"
    );
    assert!(
        third.iter().all(|f| f.said.kind != "model"),
        "a continuous read restates no model: {third:?}"
    );
    assert_eq!(
        whole.iter().filter(|f| f.said.kind == "model").count(),
        1,
        "one model named once: {whole:?}"
    );
    assert!(
        whole.iter().all(|f| f.state == "partial"),
        "MEASURED_BUILDS names no 2.1.299: {whole:?}"
    );
}
