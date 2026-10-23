// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `.seq` replay corpus parser, runner, and mutant coverage harness.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

use aterm_models_session::{transcript_join_model, turn_tracker_model};
use aterm_slog::{
    ActWord, Actor, Close, Conf, Cursor, Fields, GapWhy, Key, Kind, Limits, Log, Lvl, MAX_BYTES,
    Mark, Node, Op, Open, Out, Rec, RunOut, ShortId, Src, Tier, TurnTracker, TxEvent, TxPos,
    TxState, Verdict, WallJudge, WallKind, encode,
};
use aterm_spec::derive::{InvariantKind, Model};
use aterm_spec::interp::{State, admits, with_buggy};

use crate::conformance_transcript::project_parts;
use crate::conformance_turn::{self, Rig, Step, handoff};
use crate::seq_twins::{self, Took};
use crate::support::{self, WALL0, agent_session, at, time_order_holds};

/// Path to the `.seq` replay corpus directory.
pub const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/sequences");

/// The transcript rig's file (`transcript_replays`' path and identity).
const FP: u64 = 0xc1a0_de00_0000_0001;
const DEV: u64 = 16_777_220;
const INO: u64 = 4_242;
/// Simulated Claude Code process start timestamp (`WALL0 - 60_000`).
pub const PROCESS_START: u64 = WALL0 - 60_000;
/// A row's offset past the last, by default.
const ROW_BYTES: u64 = 200;

/// Replay rig selector for a `.seq` file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RigKind {
    /// Screen-only turn-model conformance rig.
    Screen,
    /// Transcript-bound replay rig.
    Transcript,
}

/// Parsed `.seq` replay specification.
#[derive(Clone, Debug)]
pub struct Seq {
    pub name: String,
    pub rig: RigKind,
    pub program: String,
    pub build: String,
    /// Optional ring capacity override (`ring=`).
    pub ring: Option<usize>,
    pub lines: Vec<Line>,
}

/// Single timestamped step or expectation line in a `.seq` file.
#[derive(Clone, Debug)]
pub struct Line {
    pub no: usize,
    pub text: String,
    pub at: Option<u64>,
    pub ev: Ev,
}

/// Replay step or expectation event.
#[derive(Clone, Debug)]
pub enum Ev {
    /// Advance the clock to `at <ms>` with no action.
    Nothing,
    See {
        v: Verdict,
        detail: Option<ShortId>,
    },
    Wait(u64),
    Pass(u64),
    Input(Actor),
    Interrupted,
    Ctl,
    /// Control input with explicit `cause=<id>`.
    CtlCaused(ShortId),
    /// Delivered mail event (`mail <kind> off=<n>`).
    Mail {
        kind: String,
        off: u64,
    },
    /// Human input mark.
    Human,
    /// Manual mail mark on the open turn (`turn-mail`).
    TurnMail,
    /// Program self-reported state (`program <state> [ask=<w>]`).
    Program {
        state: String,
        ask: Option<String>,
    },
    /// Close the active agent run (`close-run <out>`).
    CloseRun(RunOut),
    Handoff(bool),
    Half,
    Elapse,
    Wake,
    VendorTurn,
    JudgeRefutes,
    /// Admitted and accepted control/person input (`Admit`).
    Admit(Actor, Option<ActWord>),
    Accept,
    Person,
    State(TxState),
    File(u64),
    Bind(u64),
    Tx(Tx),
    /// Seamless-update tracker carry adoption after `after` ms.
    Adopt {
        keep_tx: bool,
        after: u64,
    },
    Expect(Expect),
}

/// Simulated transcript row.
#[derive(Clone, Debug)]
pub struct Tx {
    pub row: Row,
    /// Relative timestamp in milliseconds.
    pub dated: Option<i64>,
    pub off: Option<u64>,
}

/// Owned transcript row event.
#[derive(Clone, Debug)]
pub enum Row {
    Prompt(ShortId),
    Interrupt,
    Tool {
        id: ShortId,
        tool: String,
        cls: String,
        subagent: bool,
    },
    Result {
        id: ShortId,
        error: bool,
    },
    End {
        vdur_ms: u64,
        tok_in: u64,
        tok_out: u64,
    },
    Compact,
    Model {
        model: Option<String>,
        effort: Option<String>,
        ver: Option<String>,
    },
    Wall(WallKind, WallJudge),
    EndWall(WallKind),
    Lost(u64),
    Overflow(u64),
    Flush,
}

impl Row {
    /// Borrowed [`TxEvent`] view of this row.
    pub fn event(&self) -> TxEvent<'_> {
        match self {
            Row::Prompt(vref) => TxEvent::Prompt { vref: *vref },
            Row::Interrupt => TxEvent::Interrupt,
            Row::Tool {
                id,
                tool,
                cls,
                subagent,
            } => TxEvent::ToolUse {
                id: *id,
                tool,
                cls,
                subagent: *subagent,
            },
            Row::Result { id, error } => TxEvent::ToolResult {
                id: *id,
                error: *error,
            },
            Row::End {
                vdur_ms,
                tok_in,
                tok_out,
            } => TxEvent::TurnEnd {
                vdur_ms: *vdur_ms,
                tok_in: *tok_in,
                tok_out: *tok_out,
            },
            Row::Compact => TxEvent::Compact,
            Row::Model { model, effort, ver } => TxEvent::Model {
                model: model.as_deref(),
                effort: effort.as_deref(),
                ver: ver.as_deref(),
            },
            Row::Wall(kind, judge) => TxEvent::Wall {
                kind: *kind,
                judge: *judge,
            },
            Row::EndWall(kind) => TxEvent::EndWall { kind: *kind },
            Row::Lost(n) => TxEvent::Lost { n: *n },
            Row::Overflow(n) => TxEvent::Overflow { n: *n },
            Row::Flush => TxEvent::Flush,
        }
    }
}

/// Assertion checked during `.seq` replay.
#[derive(Clone, Debug)]
pub enum Expect {
    Rec {
        op: Op,
        node: String,
        lvl: Lvl,
        kind: Kind,
        toks: Vec<String>,
    },
    /// Expected open/closed turn state (`turn=open|none|<node>`).
    Turn(Option<Option<String>>),
    Need(Option<String>),
    Clause(Option<String>),
    Wake(Option<u64>),
    Lost(u64),
    Tx(Vec<(String, String)>),
    /// Expected transcript link offset (`link=@<off>|none`).
    Link(Option<u64>),
}

fn num(s: &str) -> Result<u64, String> {
    s.parse().map_err(|_| format!("`{s}` is not a number"))
}

fn id(s: &str) -> Result<ShortId, String> {
    ShortId::new(s).ok_or_else(|| format!("`{s}` is not an id"))
}

/// The `k=v` options of `toks`, and the bare words.
fn split_opts<'a>(toks: &[&'a str]) -> (BTreeMap<&'a str, &'a str>, Vec<&'a str>) {
    let mut opts = BTreeMap::new();
    let mut bare = Vec::new();
    for t in toks {
        match t.split_once('=') {
            Some((k, v)) => {
                opts.insert(k, v);
            }
            None => bare.push(*t),
        }
    }
    (opts, bare)
}

fn wall_kind(s: &str) -> Result<WallKind, String> {
    WallKind::from_name(s).ok_or_else(|| format!("`{s}` is no wall kind"))
}

fn tx_row(toks: &[&str]) -> Result<Tx, String> {
    let (opts, bare) = split_opts(toks);
    let known = |allowed: &[&str]| -> Result<(), String> {
        match opts
            .keys()
            .find(|k| !allowed.contains(k) && !["dated", "off", "slice"].contains(k))
        {
            Some(k) => Err(format!("`{k}=` is no option of this row")),
            None => Ok(()),
        }
    };
    let opt_num = |k: &str, d: u64| opts.get(k).map_or(Ok(d), |v| num(v));
    let row = match bare.as_slice() {
        ["prompt", v] => Row::Prompt(id(v)?),
        ["interrupt"] => Row::Interrupt,
        ["tool", i, rest @ ..] => {
            known(&["tool", "cls"])?;
            Row::Tool {
                id: id(i)?,
                tool: opts.get("tool").unwrap_or(&"Bash").to_string(),
                cls: opts.get("cls").unwrap_or(&"shell").to_string(),
                subagent: match rest {
                    [] => false,
                    ["subagent"] => true,
                    _ => return Err(format!("`{rest:?}` after a tool call")),
                },
            }
        }
        ["result", i, rest @ ..] => Row::Result {
            id: id(i)?,
            error: match rest {
                [] => false,
                ["error"] => true,
                _ => return Err(format!("`{rest:?}` after a result")),
            },
        },
        ["end"] => {
            known(&["vdur", "in", "out"])?;
            Row::End {
                vdur_ms: opt_num("vdur", 4200)?,
                tok_in: opt_num("in", 900)?,
                tok_out: opt_num("out", 120)?,
            }
        }
        ["compact"] => Row::Compact,
        ["model"] => {
            known(&["model", "effort", "ver"])?;
            let o = |k: &str| opts.get(k).map(|v| (*v).to_string());
            Row::Model {
                model: o("model"),
                effort: o("effort"),
                ver: o("ver"),
            }
        }
        ["wall", k, judge] => Row::Wall(
            wall_kind(k)?,
            match *judge {
                "confirm" => WallJudge::Confirm,
                "refute" => WallJudge::Refute,
                j => return Err(format!("`{j}` is no judge's answer")),
            },
        ),
        ["endwall", k] => Row::EndWall(wall_kind(k)?),
        ["lost", n] => Row::Lost(num(n)?),
        ["overflow", n] => Row::Overflow(num(n)?),
        ["flush"] => Row::Flush,
        other => return Err(format!("`tx {other:?}` is no transcript row")),
    };
    if !matches!(row, Row::Tool { .. } | Row::End { .. } | Row::Model { .. }) {
        known(&[])?;
    }
    let dated = opts
        .get("dated")
        .map(|d| d.parse::<i64>().map_err(|_| format!("`dated={d}`")))
        .transpose()?;
    let off = opts.get("off").map(|o| num(o)).transpose()?;
    if let Some(s) = opts.get("slice") {
        slice_of(s)?;
    }
    Ok(Tx { row, dated, off })
}

/// Parse a `slice=<file>:<a>-<b>` specification.
pub fn slice_of(s: &str) -> Result<(String, usize, usize), String> {
    let (file, range) = s
        .rsplit_once(':')
        .ok_or_else(|| format!("`slice={s}` names no lines"))?;
    let (a, b) = range
        .split_once('-')
        .ok_or_else(|| format!("`slice={s}` is no range"))?;
    let (a, b) = (num(a)? as usize, num(b)? as usize);
    if file.is_empty() || a == 0 || b < a {
        return Err(format!("`slice={s}` is no range of a file"));
    }
    Ok((file.to_string(), a, b))
}

fn expect(rest: &str) -> Result<Expect, String> {
    if let Some(words) = rest.strip_prefix("clause=") {
        return Ok(Expect::Clause((words != "none").then(|| words.to_string())));
    }
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let none_or = |v: &str| (v != "none").then(|| v.to_string());
    Ok(match toks.as_slice() {
        [one] if one.starts_with("turn=") => {
            let v = &one["turn=".len()..];
            Expect::Turn(match v {
                "none" => None,
                "open" => Some(None),
                n => Some(Some(n.to_string())),
            })
        }
        [one] if one.starts_with("need=") => Expect::Need(none_or(&one["need=".len()..])),
        [one] if one.starts_with("wake=") => {
            let v = &one["wake=".len()..];
            Expect::Wake(if v == "none" { None } else { Some(num(v)?) })
        }
        [one] if one.starts_with("lost=") => Expect::Lost(num(&one["lost=".len()..])?),
        [one] if one.starts_with("link=") => Expect::Link(match &one["link=".len()..] {
            "none" => None,
            v => Some(num(v
                .strip_prefix('@')
                .ok_or_else(|| format!("`link={v}`: @<off> or none"))?)?),
        }),
        ["tx", kv @ ..] => {
            let mut out = Vec::new();
            for t in kv {
                let (k, v) = t
                    .split_once('=')
                    .ok_or_else(|| format!("`{t}` in `expect tx`"))?;
                if ![
                    "mapped", "dropped", "ended", "refuted", "desync", "vref", "through",
                ]
                .contains(&k)
                {
                    return Err(format!("`expect tx {k}=` names nothing the join holds"));
                }
                out.push((k.to_string(), v.to_string()));
            }
            Expect::Tx(out)
        }
        [op, node, kind, toks @ ..] => {
            let op = Op::from_word(op).ok_or_else(|| format!("`{op}` is no op"))?;
            let lvl = Node::parse(node)
                .ok_or_else(|| format!("`{node}` is no node"))?
                .lvl;
            let k = Kind::from_word(kind);
            if k == Kind::Unknown && *kind != "unknown" {
                return Err(format!("`{kind}` is no kind"));
            }
            let toks = toks
                .iter()
                .map(|t| match t.strip_prefix('@') {
                    Some(ms) => num(ms).map(|ms| format!("t={}", WALL0 + ms)),
                    None if t.contains('=') => Ok((*t).to_string()),
                    None => Err(format!("`{t}` is no k=v token")),
                })
                .collect::<Result<_, _>>()?;
            Expect::Rec {
                op,
                node: (*node).to_string(),
                lvl,
                kind: k,
                toks,
            }
        }
        _ => return Err(format!("`expect {rest}` checks nothing")),
    })
}

fn step(rig: RigKind, toks: &[&str]) -> Result<Ev, String> {
    let screen = rig == RigKind::Screen;
    let only = |ok: bool, ev: Ev| {
        if ok {
            Ok(ev)
        } else {
            Err(format!("`{}` is no step of this rig", toks.join(" ")))
        }
    };
    match toks {
        ["see", word, rest @ ..] => {
            let (opts, bare) = split_opts(rest);
            if let Some(k) = opts.keys().find(|k| **k != "fixture") {
                return Err(format!("`{k}=` is no option of `see`"));
            }
            let detail = match bare.as_slice() {
                [] => None,
                [d] => Some(id(d)?),
                _ => return Err(format!("`see {word}` takes one detail")),
            };
            let v = Verdict::from_agent_word(word, detail.as_ref().map(ShortId::as_str));
            if v == Verdict::Unknown && *word != "unknown" {
                return Err(format!("`{word}` is no agent word"));
            }
            Ok(Ev::See { v, detail })
        }
        ["wait", ms] => Ok(Ev::Wait(num(ms)?)),
        ["pass", ms] => Ok(Ev::Pass(num(ms)?)),
        ["input", by] => Ok(Ev::Input(Actor::from_word(by))),
        ["interrupted"] => Ok(Ev::Interrupted),
        ["ctl"] => Ok(Ev::Ctl),
        ["ctl", cause] => match cause.strip_prefix("cause=") {
            Some(c) => Ok(Ev::CtlCaused(id(c)?)),
            None => Err(format!("`ctl {cause}`: cause=<id>")),
        },
        ["mail", kind, rest @ ..] => {
            let (opts, bare) = split_opts(rest);
            if let Some(k) = opts.keys().find(|k| **k != "off") {
                return Err(format!("`{k}=` is no option of `mail`"));
            }
            if !bare.is_empty() {
                return Err(format!("`{bare:?}` after `mail {kind}`"));
            }
            let off = num(opts.get("off").ok_or("`mail` names its off=")?)?;
            mail_mark(kind, off).map_err(|e| format!("`mail {kind}`: {e:?}"))?;
            Ok(Ev::Mail {
                kind: (*kind).to_string(),
                off,
            })
        }
        ["human"] => Ok(Ev::Human),
        ["turn-mail"] => Ok(Ev::TurnMail),
        ["program", state, rest @ ..] => {
            let (opts, bare) = split_opts(rest);
            if let Some(k) = opts.keys().find(|k| **k != "ask") {
                return Err(format!("`{k}=` is no option of `program`"));
            }
            if !bare.is_empty() {
                return Err(format!("`{bare:?}` after `program {state}`"));
            }
            let ask = opts.get("ask").map(|a| (*a).to_string());
            program_mark(state, ask.as_deref(), Node::SESSION)
                .map_err(|e| format!("`program {state}`: {e:?}"))?;
            Ok(Ev::Program {
                state: (*state).to_string(),
                ask,
            })
        }
        ["close-run", out] => {
            let o = [
                RunOut::Ok,
                RunOut::Fail,
                RunOut::Signal,
                RunOut::Gone,
                RunOut::Superseded,
                RunOut::Retracted,
                RunOut::Unknown,
            ]
            .into_iter()
            .find(|o| Out::Run(*o).word() == *out)
            .ok_or_else(|| format!("`{out}` is no run's outcome"))?;
            only(!screen, Ev::CloseRun(o))
        }
        ["handoff", "carried"] => Ok(Ev::Handoff(true)),
        ["handoff", "uncarried"] => Ok(Ev::Handoff(false)),
        ["half"] => only(screen, Ev::Half),
        ["elapse"] => only(screen, Ev::Elapse),
        ["wake"] => only(screen, Ev::Wake),
        ["vendor-turn"] => only(screen, Ev::VendorTurn),
        ["judge-refutes"] => only(screen, Ev::JudgeRefutes),
        ["admit", by, rest @ ..] => {
            let (opts, bare) = split_opts(rest);
            if let Some(k) = opts.keys().find(|k| **k != "act") {
                return Err(format!("`{k}=` is no option of `admit`"));
            }
            if !bare.is_empty() {
                return Err(format!("`{bare:?}` after `admit {by}`"));
            }
            let act = match opts.get("act") {
                None => None,
                Some(w) => Some(ActWord::from_word(w).ok_or_else(|| format!("`{w}` is no act"))?),
            };
            only(screen, Ev::Admit(Actor::from_word(by), act))
        }
        ["accept"] => only(screen, Ev::Accept),
        ["person"] => only(screen, Ev::Person),
        ["state", s] => only(
            !screen,
            Ev::State(TxState::from_word(s).ok_or_else(|| format!("`{s}` is no state"))?),
        ),
        ["file", ino] => only(!screen, Ev::File(num(ino)?)),
        ["bind", len] => only(!screen, Ev::Bind(num(len)?)),
        ["adopt", which, rest @ ..] => {
            let keep_tx = match *which {
                "keep-tx" => true,
                "drop-tx" => false,
                w => return Err(format!("`adopt {w}`: keep-tx or drop-tx")),
            };
            let (opts, bare) = split_opts(rest);
            if let Some(k) = opts.keys().find(|k| **k != "after") {
                return Err(format!("`{k}=` is no option of `adopt`"));
            }
            if !bare.is_empty() {
                return Err(format!("`{bare:?}` after `adopt {which}`"));
            }
            let after = opts.get("after").map_or(Ok(0), |v| num(v))?;
            only(!screen, Ev::Adopt { keep_tx, after })
        }
        ["tx", rest @ ..] => only(!screen, Ev::Tx(tx_row(rest)?)),
        _ => Err(format!("`{}` is no step", toks.join(" "))),
    }
}

/// The `mail dir=in` mark of a delivered mail, as aterm-gui's fabric
fn mail_mark(kind: &str, off: u64) -> Result<Mark, aterm_slog::FieldError> {
    let f = Fields::new()
        .word(Key::Dir, "in")?
        .word(Key::MailKind, kind)?
        .u(Key::Off, off)?;
    Ok(Mark::new(Kind::Mail, Src::Fabric, Conf::Exact)
        .at(Node::SESSION)
        .by(Actor::Bridge)
        .fields(f))
}

/// The program's own report at `node`, as aterm-gui's sweep writes it
fn program_mark(
    state: &str,
    ask: Option<&str>,
    node: Node,
) -> Result<Mark, aterm_slog::FieldError> {
    let mut f = Fields::new().word(Key::State, state)?;
    if let Some(a) = ask {
        f = f.word(Key::Ask, a)?;
    }
    Ok(Mark::new(Kind::Program, Src::Engine, Conf::Claim)
        .at(node)
        .fields(f))
}

/// A step that writes into the log alone (`mail`, `human`, `turn-mail`,
fn log_only(log: &mut Log, ev: &Ev, stamp: aterm_slog::Stamp) -> Result<bool, String> {
    let mark = match ev {
        Ev::Program { state, ask } => {
            let node = log.deepest_open(Lvl::Run).unwrap_or(Node::SESSION);
            program_mark(state, ask.as_deref(), node).map_err(|e| format!("{e:?}"))?
        }
        Ev::Mail { kind, off } => mail_mark(kind, *off).map_err(|e| format!("{e:?}"))?,
        Ev::Human => Mark::new(Kind::Human, Src::Input, Conf::Strong)
            .at(Node::SESSION)
            .by(Actor::Human),
        Ev::TurnMail => {
            let turn = log
                .deepest_open(Lvl::Turn)
                .ok_or("`turn-mail` with no turn open")?;
            Mark::new(Kind::Mail, Src::Aterm, Conf::Exact).at(turn)
        }
        _ => return Ok(false),
    };
    log.mark(mark, stamp)
        .map_err(|e| format!("the log refused it: {e:?}"))?;
    Ok(true)
}

/// Parse a `.seq` file.
pub fn parse(name: &str, text: &str) -> Result<Seq, String> {
    let mut lines = text.lines().enumerate().map(|(i, l)| (i + 1, l.trim()));
    let (_, head) = lines
        .find(|(_, l)| !l.is_empty() && !l.starts_with('#'))
        .ok_or_else(|| format!("{name}: no header"))?;
    let toks: Vec<&str> = head.split_whitespace().collect();
    let (opts, bare) = split_opts(&toks[1.min(toks.len())..]);
    if toks.first() != Some(&"seq") || !bare.is_empty() || opts.get("v") != Some(&"1") {
        return Err(format!("{name}: `{head}` is no `seq v=1` header"));
    }
    let rig = match opts.get("rig") {
        Some(&"screen") => RigKind::Screen,
        Some(&"transcript") => RigKind::Transcript,
        r => return Err(format!("{name}: rig {r:?}")),
    };
    let field = |k: &str| {
        opts.get(k)
            .map(|v| (*v).to_string())
            .ok_or_else(|| format!("{name}: the header names no {k}="))
    };
    let ring = opts
        .get("ring")
        .map(|r| num(r).map(|n| n as usize))
        .transpose()?;
    if let Some(k) = opts
        .keys()
        .find(|k| !["v", "rig", "program", "build", "ring"].contains(k))
    {
        return Err(format!("{name}: `{k}=` is no header field"));
    }
    let mut seq = Seq {
        name: name.to_string(),
        rig,
        program: field("program")?,
        build: field("build")?,
        ring,
        lines: Vec::new(),
    };
    for (no, text) in lines {
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let at_err = |e: String| format!("{name}:{no}: {e}");
        let (at, rest) = match text.strip_prefix("at ") {
            Some(r) => {
                let (ms, rest) = r.split_once(' ').unwrap_or((r, ""));
                (Some(num(ms).map_err(at_err)?), rest.trim())
            }
            None => (None, text),
        };
        let ev = if rest.is_empty() {
            Ev::Nothing
        } else if let Some(e) = rest.strip_prefix("expect ") {
            Ev::Expect(expect(e.trim()).map_err(at_err)?)
        } else {
            let toks: Vec<&str> = rest.split_whitespace().collect();
            step(rig, &toks).map_err(at_err)?
        };
        seq.lines.push(Line {
            no,
            text: text.to_string(),
            at,
            ev,
        });
    }
    Ok(seq)
}

/// Load and parse all `.seq` files in [`CORPUS`] in filename order.
pub fn corpus() -> Vec<Seq> {
    let mut paths: Vec<_> = std::fs::read_dir(CORPUS)
        .expect("the corpus directory")
        .map(|e| e.expect("a corpus entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "seq"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .expect("a name")
                .to_string();
            let text = std::fs::read_to_string(p).expect("a corpus file reads");
            parse(&name, &text).unwrap_or_else(|e| panic!("{e}"))
        })
        .collect()
}

/// Intentional tracker fault injected for negative-control tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Broken {
    /// Unmodified production tracker.
    No,
    /// Fault that opens a duplicate turn on repeated `busy`.
    SecondTurnOnRepeat,
    /// Fault that drops the first gap in [`cursor_contract`].
    DropsAGap,
}

/// Transcript file fixture rig for `transcript_replays`.
pub struct TxRig {
    pub log: Log,
    pub run: Node,
    pub tracker: TurnTracker,
    pub now: u64,
    pub off: u64,
    pub state: TxState,
    pub ino: u64,
    pub ledger: u64,
    pub limits: Limits,
    broken: Broken,
    /// Its birth order and the host calls it made, while the twins record
    born: usize,
    path: Vec<Took>,
}

impl Drop for TxRig {
    fn drop(&mut self) {
        seq_twins::died(self.born, std::mem::take(&mut self.path), &self.log);
    }
}

impl TxRig {
    pub fn new(limits: Limits, broken: Broken) -> TxRig {
        let mut log = Log::with_limits(1, limits);
        let run = agent_session(&mut log, 0);
        TxRig {
            log,
            run,
            tracker: TurnTracker::new(run),
            now: 0,
            off: 0,
            state: TxState::On,
            ino: INO,
            ledger: 0,
            limits,
            broken,
            born: seq_twins::born(),
            path: Vec::new(),
        }
    }

    /// Note one host call on the path, while the twins record: named as
    fn took(&mut self, step: String, seq0: u64) {
        if seq_twins::on() {
            let state = (self.state != TxState::Off).then(|| {
                project_parts(
                    &self.log,
                    self.run,
                    &self.tracker,
                    self.now,
                    self.state == TxState::Partial,
                    0,
                )
            });
            let lines = seq_twins::lines_since(&self.log, seq0);
            self.path.push(Took { step, state, lines });
        }
    }

    const fn pos(&self, off: u64, wall_ms: u64) -> TxPos {
        TxPos {
            fp: FP,
            id: Some((DEV, self.ino)),
            off,
            wall_ms,
        }
    }

    fn call(&mut self, ev: TxEvent<'_>, pos: TxPos) {
        let seq0 = self.log.last_seq();
        let (state, stamp) = (self.state, at(self.now));
        self.tracker
            .transcript(&mut self.log, ev, pos, state, stamp);
        self.took(seq_twins::tx_step(&ev, &pos, state, self.now), seq0);
    }

    fn wait(&mut self, ms: u64) {
        let (seq0, from) = (self.log.last_seq(), self.now);
        self.now += ms;
        if self.tracker.wake_at().is_some_and(|d| d <= self.now) {
            let stamp = at(self.now);
            self.tracker.tick(&mut self.log, stamp);
        }
        self.took(seq_twins::wait_step(ms, from), seq0);
    }

    fn apply(&mut self, ev: &Ev) -> Result<(), String> {
        let stamp = at(self.now);
        match ev {
            Ev::See { v, detail } => {
                if self.broken == Broken::SecondTurnOnRepeat
                    && *v == Verdict::Busy
                    && self.tracker.view(self.now).word == Verdict::Busy
                {
                    let _ = self.log.open(
                        Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(self.run),
                        stamp,
                    );
                }
                let (seq0, d) = (self.log.last_seq(), detail.as_ref().map(ShortId::as_str));
                self.tracker.observe(&mut self.log, *v, d, stamp);
                self.took(seq_twins::see_step(*v, d, self.now), seq0);
            }
            Ev::Wait(ms) => self.wait(*ms),
            Ev::Pass(ms) => self.now += ms,
            Ev::Input(by) => self.tracker.note_input(*by, stamp),
            Ev::Interrupted => self.tracker.note_interrupted(),
            Ev::Ctl => {
                let seq0 = self.log.last_seq();
                self.ledger += 1;
                self.tracker
                    .ctl_turn(&mut self.log, Actor::Harness, self.ledger, stamp);
                self.took(format!("{} ctl", self.now), seq0);
            }
            Ev::CtlCaused(cause) => {
                let seq0 = self.log.last_seq();
                self.ledger += 1;
                self.tracker.ctl_turn_caused(
                    &mut self.log,
                    Actor::Harness,
                    self.ledger,
                    Some(*cause),
                    stamp,
                );
                self.took(format!("{} ctl cause={cause}", self.now), seq0);
            }
            Ev::Mail { .. } | Ev::Human | Ev::TurnMail | Ev::Program { .. } => {
                log_only(&mut self.log, ev, stamp)?;
            }
            Ev::CloseRun(out) => {
                let close = Close::new(Out::Run(*out), Src::Status, Conf::Strong);
                self.log
                    .close(self.run, close, stamp)
                    .map_err(|e| format!("the run's close was refused: {e:?}"))?;
            }
            Ev::Handoff(carried) => {
                let seq0 = self.log.last_seq();
                self.run = handoff(
                    &mut self.log,
                    &mut self.tracker,
                    self.limits,
                    *carried,
                    stamp,
                );
                self.took(format!("{} handoff carried={carried}", self.now), seq0);
            }
            Ev::State(s) => self.state = *s,
            Ev::File(ino) => self.ino = *ino,
            Ev::Bind(len) => {
                self.off = self.off.max(*len);
                let bind = TxEvent::Bind {
                    len: *len,
                    floor_ms: PROCESS_START,
                };
                let pos = self.pos(*len, WALL0 + self.now);
                self.call(bind, pos);
            }
            Ev::Tx(tx) => {
                let wall = tx.dated.map_or(WALL0 + self.now, |d| {
                    u64::try_from(i64::try_from(WALL0).unwrap_or(i64::MAX) + d).unwrap_or(0)
                });
                let off = tx.off.unwrap_or_else(|| {
                    self.off += ROW_BYTES;
                    self.off
                });
                let pos = self.pos(off, wall);
                self.call(tx.row.event(), pos);
            }
            Ev::Adopt { keep_tx, after } => {
                let line = self.tracker.carry_line(stamp);
                let line = if *keep_tx {
                    line
                } else {
                    line.split(' ')
                        .filter(|t| !t.starts_with("tx_"))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                self.now += after;
                self.tracker = TurnTracker::from_carry_line(&line, at(self.now))
                    .ok_or("the tracker's own carry line does not read")?;
                let seq0 = self.log.last_seq();
                self.took(seq_twins::adopt_step(&line, self.now), seq0);
            }
            Ev::Half
            | Ev::Elapse
            | Ev::Wake
            | Ev::VendorTurn
            | Ev::JudgeRefutes
            | Ev::Admit(..)
            | Ev::Accept
            | Ev::Person => {
                return Err("a screen rig's step".to_string());
            }
            Ev::Nothing | Ev::Expect(_) => {}
        }
        Ok(())
    }
}

/// The rig a file replays on.
enum Live {
    Screen(Box<Rig>),
    Tx(Box<TxRig>),
}

impl Live {
    fn log(&self) -> &Log {
        match self {
            Live::Screen(r) => &r.log,
            Live::Tx(r) => &r.log,
        }
    }

    fn tracker(&self) -> &TurnTracker {
        match self {
            Live::Screen(r) => &r.tracker,
            Live::Tx(r) => &r.tracker,
        }
    }

    const fn now(&self) -> u64 {
        match self {
            Live::Screen(r) => r.now,
            Live::Tx(r) => r.now,
        }
    }

    /// The model states this rig is at: the turn model's (screen) or the
    fn state(&self) -> Option<State> {
        match self {
            Live::Screen(r) => Some(conformance_turn::project(r, None)),
            Live::Tx(r) => (r.state != TxState::Off).then(|| {
                project_parts(
                    &r.log,
                    r.run,
                    &r.tracker,
                    r.now,
                    r.state == TxState::Partial,
                    0,
                )
            }),
        }
    }
}

/// Trace of a single replayed step, resulting model state, and appended records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Traced {
    pub no: usize,
    pub text: String,
    pub state: Option<State>,
    pub appended: Vec<String>,
}

/// Complete replay trace, appended records, and final log window.
#[derive(Clone, Debug)]
pub struct Replay {
    pub rig: RigKind,
    pub trace: Vec<Traced>,
    pub every: Vec<String>,
    pub lines: Vec<String>,
    pub lost: u64,
    /// Count of `mail=@<off>~` tokens emitted in post-step briefs.
    pub mail_tokens: usize,
}

fn enc(r: &Rec) -> String {
    encode(r, Tier::Content).expect("a record encodes")
}

/// The records appended after `seq0`: an evicted one is a step that wrote
fn appended(log: &Log, seq0: u64) -> Result<Vec<Rec>, String> {
    let read = log.read(Cursor::new(log.ep(), seq0), usize::MAX);
    match read.gap {
        Some(g) if g.lost > 0 => Err(format!("a step appended past the ring: {g:?}")),
        _ => Ok(read.recs.into_iter().cloned().collect()),
    }
}

/// Whether `line` is the record `e` expects.
fn matches(line: &str, op: Op, node: &str, kind: Kind, toks: &[String]) -> bool {
    let w: Vec<&str> = line.split(' ').collect();
    w.len() >= 4
        && w[1] == op.word()
        && w[2] == node
        && w[3] == kind.word()
        && toks.iter().all(|t| {
            if t == "by=-" {
                !w[4..].iter().any(|x| x.starts_with("by="))
            } else {
                w[4..].contains(&t.as_str())
            }
        })
}

/// The `(op, level)` pair of a record.
fn pair(r: &Rec) -> (Op, Lvl) {
    (r.op, r.node.lvl)
}

/// A checkpoint against what the rig holds now.
fn check(
    live: &Live,
    all: &[Rec],
    next: &mut BTreeMap<(&'static str, Lvl), usize>,
    e: &Expect,
) -> Result<(), String> {
    let log = live.log();
    let tr = live.tracker();
    let got_want = |what: &str, got: String, want: String| {
        if got == want {
            Ok(())
        } else {
            Err(format!("{what}: got {got}, expected {want}"))
        }
    };
    match e {
        Expect::Rec {
            op,
            node,
            lvl,
            kind,
            toks,
        } => {
            let i = next.entry((op.word(), *lvl)).or_insert(0);
            let of_pair: Vec<&Rec> = all.iter().filter(|r| pair(r) == (*op, *lvl)).collect();
            let Some(r) = of_pair.get(*i) else {
                return Err(format!(
                    "no {} record of a {lvl:?} is appended yet (entry {} of the pair)",
                    op.word(),
                    *i + 1
                ));
            };
            let line = enc(r);
            if !matches(&line, *op, node, *kind, toks) {
                return Err(format!(
                    "entry {} of ({}, {lvl:?}) is `{line}`, not {node} {} {toks:?}",
                    *i + 1,
                    op.word(),
                    kind.word()
                ));
            }
            *i += 1;
            Ok(())
        }
        Expect::Turn(want) => {
            let got = tr.turn();
            match (want, got) {
                (None, None) | (Some(None), Some(_)) => Ok(()),
                (Some(Some(n)), Some(t)) if t.to_string() == *n => Ok(()),
                _ => Err(format!("turn: got {got:?}, expected {want:?}")),
            }
        }
        Expect::Need(want) => got_want(
            "need",
            format!("{:?}", log.state().top_need().map(|n| n.word)),
            format!("{:?}", want.as_deref()),
        ),
        Expect::Clause(want) => got_want(
            "clause",
            format!(
                "{:?}",
                log.fold()
                    .clause(WALL0 + live.now())
                    .map(|c| c.words.as_str().to_string())
            ),
            format!("{want:?}"),
        ),
        Expect::Wake(want) => got_want("wake", format!("{:?}", tr.wake_at()), format!("{want:?}")),
        Expect::Lost(n) => got_want("lost", log.lost().to_string(), n.to_string()),
        Expect::Link(want) => {
            let fold = log.fold();
            let st = fold.state();
            let newest = st.open[1]
                .as_ref()
                .map(|o| o.node)
                .or_else(|| st.last[1].as_ref().map(|c| c.node))
                .ok_or("link: no turn")?;
            let got = fold.mail_link(newest);
            if let Some(l) = got
                && l.conf != Conf::Heuristic
            {
                return Err(format!("link: {l:?} is not heuristic"));
            }
            got_want(
                "link",
                format!("{:?}", got.map(|l| l.off)),
                format!("{want:?}"),
            )
        }
        Expect::Tx(kv) => {
            let v = tr.tx_view();
            let opt = |o: Option<String>| o.unwrap_or_else(|| "none".to_string());
            for (k, want) in kv {
                let got = match k.as_str() {
                    "mapped" => opt(v.mapped.map(|n| n.to_string())),
                    "dropped" => v.dropped.to_string(),
                    "ended" => u8::from(v.ended).to_string(),
                    "refuted" => opt(v.refuted.map(|k| k.name().to_string())),
                    "desync" => u8::from(v.desync).to_string(),
                    "vref" => opt(v.vref.map(|i| i.as_str().to_string())),
                    "through" => opt(v.through.map(|t| t.off.to_string())),
                    other => return Err(format!("`expect tx {other}=` names nothing")),
                };
                got_want(&format!("tx {k}"), got, want.clone())?;
            }
            Ok(())
        }
    }
}

/// WELL-NESTING over the records in order: a node opens once, under an open
fn well_nested(trace: &[(bool, Vec<Rec>)]) -> Result<(), String> {
    let mut open: BTreeMap<Node, Option<Node>> = BTreeMap::new();
    let mut ever: BTreeSet<Node> = BTreeSet::new();
    let mut closed: BTreeSet<Node> = BTreeSet::new();
    for (fresh, recs) in trace {
        if *fresh {
            open.clear();
        }
        for r in recs {
            let l = enc(r);
            match r.op {
                Op::Open => {
                    if r.node != Node::SESSION && !ever.insert(r.node) {
                        return Err(format!("{} opened twice: `{l}`", r.node));
                    }
                    if let Some(p) = r.par
                        && !open.contains_key(&p)
                    {
                        return Err(format!("{} opened under {p}, not open: `{l}`", r.node));
                    }
                    open.insert(r.node, r.par);
                }
                Op::Close => {
                    if open.remove(&r.node).is_none() {
                        return Err(format!("{} closed while not open: `{l}`", r.node));
                    }
                    if let Some((child, _)) = open.iter().find(|(_, p)| **p == Some(r.node)) {
                        return Err(format!("{} closed before its child {child}: `{l}`", r.node));
                    }
                    if r.node != Node::SESSION && !closed.insert(r.node) {
                        return Err(format!("{} closed twice", r.node));
                    }
                }
                Op::Mark | Op::Amend => {
                    if r.node != Node::SESSION
                        && !ever.contains(&r.node)
                        && !open.contains_key(&r.node)
                    {
                        return Err(format!(
                            "a {} of {}, never opened: `{l}`",
                            r.op.word(),
                            r.node
                        ));
                    }
                }
                Op::Gap | Op::Snap => {}
            }
        }
    }
    Ok(())
}

/// Verify the [`SessionLogCursor`] chunked-read contract against `log`.
pub fn cursor_contract(log: &Log, chunk: usize, drop_gap: bool) -> Result<(), String> {
    let head = log.last_seq();
    let holes: Vec<(u64, u64)> = log.holes().collect();
    let in_hole = |s: u64| holes.iter().any(|&(lo, hi)| lo <= s && s <= hi);
    let mut cur = Cursor::new(log.ep(), log.first_seq().saturating_sub(1));
    let (mut counted, mut dropped) = (0u64, false);
    let mut seen = BTreeSet::new();
    while cur.seq < head {
        let read = log.read(cur, chunk);
        let gap = if drop_gap && !dropped && read.gap.is_some() {
            dropped = true;
            None
        } else {
            read.gap
        };
        let (from, to) = (cur.seq, read.next.seq);
        if to <= from {
            return Err(format!("a read from {from} made no progress"));
        }
        let back: BTreeSet<u64> = read.recs.iter().map(|r| r.seq).collect();
        if back.iter().any(|s| *s <= from || *s > to) {
            return Err(format!("a read from {from} to {to} returned {back:?}"));
        }
        let unseen = (from + 1..=to)
            .filter(|s| !back.contains(s) && !in_hole(*s))
            .count() as u64;
        let hole = (from + 1..=to).any(in_hole);
        match gap {
            None if unseen > 0 || hole => {
                return Err(format!(
                    "a read from {from} to {to} hid {unseen} records (a hole: {hole}) with no gap"
                ));
            }
            None => {}
            Some(g) => {
                let why = if hole {
                    GapWhy::Handoff
                } else {
                    GapWhy::Evicted
                };
                if g.lost != unseen || g.why != why {
                    return Err(format!(
                        "a read from {from} to {to}: {g:?}, expected why={} lost={unseen}",
                        why.word()
                    ));
                }
                let resync = back.first().map_or(to, |s| s - 1);
                if g.resync_to != resync {
                    return Err(format!("{g:?}: resync_to should be {resync}"));
                }
            }
        }
        counted += unseen;
        seen.extend(back);
        cur = read.next;
    }
    if counted != log.lost() {
        return Err(format!(
            "the reads counted {counted} lost, the log {}",
            log.lost()
        ));
    }
    let missing = (log.first_seq()..=head)
        .filter(|s| !seen.contains(s) && !in_hole(*s))
        .count() as u64;
    if missing != counted {
        return Err(format!("{missing} seqs unaccounted for, {counted} counted"));
    }
    Ok(())
}

/// The laws of every replay's records, each step's batch in order (`true`:
fn laws(nested: &[(bool, Vec<Rec>)], join_states: &[State]) -> Result<(), String> {
    let every: Vec<String> = nested.iter().flat_map(|(_, r)| r.iter().map(enc)).collect();
    time_order_holds(&every).map_err(|e| format!("time order: {e}"))?;
    well_nested(nested).map_err(|e| format!("nesting: {e}"))?;
    let jm = transcript_join_model();
    for s in join_states {
        // runs past the model's small bounds (eight tools, not two).
        if let Some(inv) = jm
            .invariants
            .iter()
            .filter(|i| i.kind == InvariantKind::Claim)
            .find(|i| !jm.check_invariant(i.name, s))
        {
            return Err(format!("the join's {} broken at {s:?}", inv.name));
        }
    }
    Ok(())
}

/// Replay `seq` with `broken` fault injection and verify all expectations and invariants.
pub fn replay(seq: &Seq, broken: Broken) -> Result<Replay, String> {
    let limits = match seq.ring {
        Some(n) => Limits::new(n, MAX_BYTES).ok_or("ring=0")?,
        None => Limits::SESSION,
    };
    let m = turn_tracker_model();
    let mut live = match seq.rig {
        RigKind::Screen => Live::Screen(Box::new(Rig::with_limits(limits))),
        RigKind::Transcript => Live::Tx(Box::new(TxRig::new(limits, broken))),
    };
    let mut all: Vec<Rec> = live.log().window().cloned().collect();
    let mut nested: Vec<(bool, Vec<Rec>)> = vec![(false, all.clone())];
    let mut trace = Vec::new();
    let mut next: BTreeMap<(&'static str, Lvl), usize> = BTreeMap::new();
    let mut named: BTreeMap<(&'static str, Lvl), usize> = BTreeMap::new();
    let mut states = Vec::new();
    let mut mail_tokens = 0;
    for line in &seq.lines {
        let at_line = |e: String| format!("{}:{}: `{}`: {e}", seq.name, line.no, line.text);
        let expect = match &line.ev {
            Ev::Expect(e) => Some(e),
            _ => None,
        };
        let seq0 = live.log().last_seq();
        let mut steps: Vec<Ev> = Vec::new();
        if let Some(t) = line.at {
            let now = live.now();
            if t < now {
                return Err(at_line(format!("time goes backward from {now}")));
            }
            if t > now {
                steps.push(Ev::Wait(t - now));
            }
        }
        match expect {
            Some(e) if steps.is_empty() => {
                if let Expect::Rec { op, lvl, .. } = e {
                    *named.entry((op.word(), *lvl)).or_insert(0) += 1;
                }
                check(&live, &all, &mut next, e).map_err(at_line)?;
                continue;
            }
            Some(_) => {}
            None => steps.push(line.ev.clone()),
        }
        for ev in &steps {
            match &mut live {
                Live::Screen(rig) => {
                    let held = conformance_turn::project(rig, None);
                    let stamp = rig.stamp();
                    if log_only(&mut rig.log, ev, stamp).map_err(at_line)? {
                        if conformance_turn::project(rig, None) != held {
                            return Err(at_line("a log-only step moved the turn model".into()));
                        }
                        continue;
                    }
                    let s = match ev {
                        Ev::Nothing => continue,
                        Ev::See { v, detail: None } => Step::See(*v),
                        Ev::See { v, detail: Some(d) } => Step::SeeWith(*v, *d),
                        Ev::Wait(ms) => Step::Wait(*ms),
                        Ev::Pass(ms) => Step::Pass(*ms),
                        Ev::Input(a) => Step::Input(*a),
                        Ev::Interrupted => Step::Interrupted,
                        Ev::Ctl => Step::Ctl,
                        Ev::CtlCaused(c) => Step::CtlCaused(*c),
                        Ev::Handoff(c) => Step::Handoff(*c),
                        Ev::Half => Step::Half,
                        Ev::Elapse => Step::Elapse,
                        Ev::Wake => Step::Wake,
                        Ev::VendorTurn => Step::TxEnd,
                        Ev::JudgeRefutes => Step::TxRefute,
                        Ev::Admit(by, act) => Step::Admit(*by, *act),
                        Ev::Accept => Step::Accept,
                        Ev::Person => Step::Person,
                        other => return Err(at_line(format!("{other:?} on the screen rig"))),
                    };
                    catch_unwind(AssertUnwindSafe(|| conformance_turn::apply(&m, rig, s)))
                        .map_err(|p| {
                            at_line(format!(
                                "the model refused it: {}",
                                p.downcast_ref::<String>()
                                    .cloned()
                                    .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_string()))
                                    .unwrap_or_default()
                            ))
                        })?;
                }
                Live::Tx(rig) => rig.apply(ev).map_err(at_line)?,
            }
        }
        let fresh = matches!(line.ev, Ev::Handoff(false));
        let recs = appended(live.log(), seq0).map_err(at_line)?;
        all.extend(recs.iter().cloned());
        nested.push((fresh, recs.clone()));
        let state = live.state();
        if let Some(s) = &state {
            states.push(s.clone());
        }
        trace.push(Traced {
            no: line.no,
            text: line.text.clone(),
            state,
            appended: recs.iter().map(enc).collect(),
        });
        mail_tokens +=
            support::brief_mail_links_hold(live.log(), WALL0 + live.now()).map_err(at_line)?;
        if let Some(e) = expect {
            if let Expect::Rec { op, lvl, .. } = e {
                *named.entry((op.word(), *lvl)).or_insert(0) += 1;
            }
            check(&live, &all, &mut next, e).map_err(at_line)?;
        }
    }
    let name = &seq.name;
    for (p, n) in &named {
        let of = |r: &&Rec| (r.op.word(), r.node.lvl) == *p;
        let got = all.iter().filter(of).count();
        if got != *n {
            let lines: Vec<String> = all.iter().filter(of).map(enc).collect();
            return Err(format!(
                "{name}: the ({}, {:?}) records are not the file's {n}: {got} appended:\n  {}",
                p.0,
                p.1,
                lines.join("\n  ")
            ));
        }
    }
    let log = live.log();
    cursor_contract(log, 5, broken == Broken::DropsAGap)
        .map_err(|e| format!("{name}: the cursor contract: {e}"))?;
    let every: Vec<String> = all.iter().map(enc).collect();
    let join_states: &[State] = if seq.rig == RigKind::Transcript {
        &states
    } else {
        &[]
    };
    laws(&nested, join_states).map_err(|e| format!("{name}: {e}"))?;
    Ok(Replay {
        rig: seq.rig,
        trace,
        every,
        lines: log.window().map(enc).collect(),
        lost: log.lost(),
        mail_tokens,
    })
}

/// A mutant's hand-built successor: the broken real call's state.
type Hand = fn(&State) -> State;

/// Single `Buggy` model mutant descriptor.
pub struct Mutant {
    pub join: bool,
    pub dial: i64,
    pub name: &'static str,
    pub law: &'static str,
    pub hand: Hand,
}

fn set(s: &State, kv: &[(&'static str, i64)]) -> State {
    let mut b = s.clone();
    for (k, v) in kv {
        b.insert(k, *v);
    }
    b
}

/// Turn-tracker model mutants.
pub const TURN_MUTANTS: &[Mutant] = &[
    Mutant {
        join: false,
        dial: 1,
        name: "SecondTurn",
        law: "AtMostOneOpenTurn",
        hand: |s| set(s, &[("extra", 1)]),
    },
    Mutant {
        join: false,
        dial: 1,
        name: "UnknownCloses",
        law: "UnknownNeverClosesATurn",
        hand: |s| {
            set(
                s,
                &[
                    ("turn", 0),
                    ("cw", 0),
                    ("cticks", 0),
                    ("step", 0),
                    ("sturn", 0),
                    ("pend", 0),
                    ("ticks", 0),
                    ("word", 0),
                ],
            )
        },
    },
    Mutant {
        join: false,
        dial: 1,
        name: "EarlyRest",
        law: "IdleClosesOnlyAfterSettle",
        hand: |s| {
            set(
                s,
                &[
                    ("turn", 0),
                    ("cw", 2),
                    ("cticks", s["ticks"]),
                    ("pend", 0),
                    ("ticks", 0),
                    ("step", 0),
                    ("sturn", 0),
                ],
            )
        },
    },
    Mutant {
        join: false,
        dial: 1,
        name: "DialogCloses",
        law: "DialogStaysInTurn",
        hand: |s| {
            set(
                s,
                &[
                    ("turn", 0),
                    ("cw", 3),
                    ("cticks", 0),
                    ("step", 0),
                    ("sturn", 0),
                    ("pend", 0),
                    ("ticks", 0),
                    ("word", 3),
                ],
            )
        },
    },
    Mutant {
        join: false,
        dial: 1,
        name: "StepAfterClose",
        law: "StepsNestInTheirTurn",
        hand: |s| set(s, &[("step", 1), ("sturn", 1)]),
    },
    Mutant {
        join: false,
        dial: 1,
        name: "StallCloses",
        law: "StallNeverClosesATurn",
        hand: |s| {
            set(
                s,
                &[
                    ("turn", 0),
                    ("cw", 6),
                    ("cticks", 0),
                    ("step", 4),
                    ("sturn", 0),
                    ("pend", 0),
                    ("ticks", 0),
                    ("word", 6),
                ],
            )
        },
    },
    Mutant {
        join: false,
        dial: 1,
        name: "TxCloseTakesWall",
        law: "WallNeedOutlivesTxClose",
        hand: |s| {
            set(
                s,
                &[
                    ("turn", 0),
                    ("cw", 12),
                    ("cticks", 0),
                    ("step", 0),
                    ("sturn", 0),
                    ("pend", 0),
                    ("ticks", 0),
                    ("wg", 1),
                ],
            )
        },
    },
    Mutant {
        join: false,
        dial: 1,
        name: "RefuteKeepsSettle",
        law: "RefutedWallStaysShut",
        hand: |s| set(s, &[("step", 0), ("sturn", 0), ("rf", 1)]),
    },
    Mutant {
        join: false,
        dial: 2,
        name: "ActBindsNextBox",
        law: "ActNeverBindsNextBox",
        hand: |s| {
            set(
                s,
                &[
                    ("bk", 1),
                    ("ba", s["pa"]),
                    ("bs", s["pa"]),
                    ("pp", 0),
                    ("ab", 0),
                    ("pa", 0),
                    ("pd", 0),
                    ("gn", 1),
                ],
            )
        },
    },
    Mutant {
        join: false,
        dial: 2,
        name: "AttributeStaleInput",
        law: "ApprovalByObservedInput",
        hand: |s| {
            let pd = if s["pd"] == 1 { 4 } else { s["pd"] };
            set(
                s,
                &[
                    ("bk", 0),
                    ("ba", 0),
                    ("bs", 0),
                    ("pp", 0),
                    ("ab", 0),
                    ("ga", 1),
                    ("pd", pd),
                    ("rf", 0),
                    ("turn", 1),
                    ("step", 0),
                    ("sturn", 0),
                    ("pend", 0),
                    ("ticks", 0),
                    ("word", 1),
                ],
            )
        },
    },
    Mutant {
        join: false,
        dial: 2,
        name: "ActOverPerson",
        law: "PersonSupersedesAct",
        hand: |s| set(s, &[("pp", 1)]),
    },
    Mutant {
        join: false,
        dial: 2,
        name: "ActLeadOverPerson",
        law: "PersonSupersedesAct",
        hand: |s| set(s, &[("pl", 1)]),
    },
    Mutant {
        join: false,
        dial: 2,
        name: "ActTwice",
        law: "AttributesOnce",
        hand: |s| {
            let pd = if s["pd"] == 1 { 4 } else { s["pd"] };
            set(
                s,
                &[
                    ("ab", 1),
                    ("pd", pd),
                    ("rf", 0),
                    ("turn", 1),
                    ("step", 1),
                    ("sturn", 1),
                    ("pend", 0),
                    ("ticks", 0),
                    ("word", 4),
                ],
            )
        },
    },
    Mutant {
        join: false,
        dial: 2,
        name: "ActorSwap",
        law: "ActorKeepsItsAct",
        hand: |s| set(s, &[("ba", 3 - s["ba"])]),
    },
];

/// `lv` of a turn closing or superseded: whether it held no `vref`.
fn lv(s: &State) -> i64 {
    i64::from(s["lt"] == 1)
}

/// Transcript-join model mutants.
pub const JOIN_MUTANTS: &[Mutant] = &[
    Mutant {
        join: true,
        dial: 1,
        name: "PromptOpensBeside",
        law: "AtMostOneOpenTurn",
        hand: |s| set(s, &[("lt", 2), ("vm", 1), ("ve", 0), ("vo", 0), ("g", 1)]),
    },
    Mutant {
        join: true,
        dial: 1,
        name: "LateStepOrphan",
        law: "StepsUnderOpenTurn",
        hand: |s| set(s, &[("ts", 1)]),
    },
    Mutant {
        join: true,
        dial: 1,
        name: "RestLeavesSteps",
        law: "StepsUnderOpenTurn",
        hand: |s| {
            let vm = if s["vm"] == 1 { 2 } else { s["vm"] };
            set(
                s,
                &[
                    ("lt", 0),
                    ("lc", 0),
                    ("lv", lv(s)),
                    ("old", 0),
                    ("vm", vm),
                    ("wr", 0),
                    ("wd", 1),
                    ("rf", 0),
                ],
            )
        },
    },
    Mutant {
        join: true,
        dial: 1,
        name: "ScreenConfirms",
        law: "ConfirmedOnlyFromTranscript",
        hand: |s| set(s, &[("g", 2)]),
    },
    Mutant {
        join: true,
        dial: 1,
        name: "ReplayAfterLapse",
        law: "NoReplay",
        hand: |s| set(s, &[("g", 3)]),
    },
    Mutant {
        join: true,
        dial: 1,
        name: "ReplayFromProcess",
        law: "NoTurnBeforeRunOpen",
        hand: |s| {
            set(
                s,
                &[
                    ("lt", 2),
                    ("old", 0),
                    ("vm", 1),
                    ("ve", 0),
                    ("vo", 0),
                    ("g", 4),
                ],
            )
        },
    },
    Mutant {
        join: true,
        dial: 1,
        name: "AttachEarly",
        law: "AttachOnlyWithinWindow",
        hand: |s| set(s, &[("lt", 2), ("vm", 1), ("ve", 0), ("vo", 0), ("g", 5)]),
    },
    Mutant {
        join: true,
        dial: 1,
        name: "OldPromptAttaches",
        law: "AttachOnlyWithinWindow",
        hand: |s| set(s, &[("lt", 2), ("vm", 1), ("ve", 0), ("vo", 0), ("g", 5)]),
    },
    Mutant {
        join: true,
        dial: 1,
        name: "LateNamesOpen",
        law: "PromptNamesNearestTurn",
        hand: |s| set(s, &[("lt", 2), ("vm", 1), ("ve", 0), ("vo", 0), ("g", 9)]),
    },
    Mutant {
        join: true,
        dial: 1,
        name: "MergeClosesBox",
        law: "TxNeverClosesScreenStep",
        hand: |s| {
            set(
                s,
                &[
                    ("lt", 2),
                    ("old", 0),
                    ("lc", 0),
                    ("lv", lv(s)),
                    ("ss", 0),
                    ("ts", 0),
                    ("vm", 1),
                    ("ve", 0),
                    ("vo", 0),
                    ("g", 6),
                ],
            )
        },
    },
    Mutant {
        join: true,
        dial: 1,
        name: "TxCloseTakesWall",
        law: "WallNeedOutlivesTxClose",
        hand: |s| {
            set(
                s,
                &[
                    ("lt", 0),
                    ("lc", 0),
                    ("lv", lv(s)),
                    ("old", 0),
                    ("ss", 0),
                    ("pend", 0),
                    ("ts", 0),
                    ("vm", 2),
                    ("ve", 1),
                    ("vo", 0),
                    ("g", 7),
                ],
            )
        },
    },
    Mutant {
        join: true,
        dial: 1,
        name: "RefuteLeavesSettle",
        law: "RefutedWallStaysShut",
        hand: |s| set(s, &[("ss", 0), ("rf", 1)]),
    },
    Mutant {
        join: true,
        dial: 1,
        name: "PartialCloses",
        law: "PartialNeverOverrules",
        hand: |s| {
            set(
                s,
                &[
                    ("lt", 0),
                    ("lc", 0),
                    ("lv", lv(s)),
                    ("old", 0),
                    ("ts", 0),
                    ("vm", 2),
                    ("ve", 1),
                    ("vo", 0),
                    ("g", 8),
                ],
            )
        },
    },
];

/// Whether the corpus state `s` catches `mu`: its hand-built successor is
fn catches(m: &Model, buggy: &Model, mu: &Mutant, s: &State) -> bool {
    if !buggy.action_enabled(mu.name, s) {
        return false;
    }
    let b = (mu.hand)(s);
    assert!(
        buggy.successors(mu.name, s).contains(&b),
        "{}'s hand-built successor is not the Buggy model's from {s:?}",
        mu.name
    );
    admits(m, s, &b).is_none() && !m.check_invariant(mu.law, &b)
}

/// Measure corpus mutant coverage across `TURN_MUTANTS` and `JOIN_MUTANTS`.
pub fn measure(
    replays: &[(String, Replay)],
) -> BTreeMap<(bool, &'static str), (Option<String>, bool)> {
    let (tm, jm) = (turn_tracker_model(), transcript_join_model());
    let mut out = BTreeMap::new();
    for mu in TURN_MUTANTS.iter().chain(JOIN_MUTANTS) {
        let (m, rig) = if mu.join {
            (&jm, RigKind::Transcript)
        } else {
            (&tm, RigKind::Screen)
        };
        let buggy = &with_buggy(m, mu.dial);
        assert!(
            m.actions.iter().any(|a| a.name == mu.name)
                && m.invariants.iter().any(|i| i.name == mu.law),
            "{} or {} is not the model's",
            mu.name,
            mu.law
        );
        let states = || {
            replays
                .iter()
                .filter(|(_, r)| r.rig == rig)
                .flat_map(|(n, r)| {
                    r.trace
                        .iter()
                        .filter_map(move |t| t.state.as_ref().map(|s| (n, s)))
                })
                .filter(|(_, s)| !mu.join || s["ts"] <= 2)
        };
        let by = states()
            .find(|(_, s)| catches(m, buggy, mu, s))
            .map(|(n, _)| n.clone());
        let reached = states().any(|(_, s)| buggy.action_enabled(mu.name, s));
        out.insert((mu.join, mu.name), (by, reached));
    }
    out
}

/// Replay every corpus file, or the first failure.
fn replay_all(seqs: &[Seq]) -> Vec<(String, Replay)> {
    let mut errs = Vec::new();
    let mut out = Vec::new();
    for s in seqs {
        match replay(s, Broken::No) {
            Ok(r) => out.push((s.name.clone(), r)),
            Err(e) => errs.push(e),
        }
    }
    assert!(
        errs.is_empty(),
        "{} of {} files failed:\n{}",
        errs.len(),
        seqs.len(),
        errs.join("\n")
    );
    out
}

/// Every corpus file replays and meets its expects and laws; each checks
#[test]
fn every_corpus_file_replays_and_meets_its_expects() {
    let seqs = corpus();
    assert!(seqs.len() >= 20, "the corpus holds {} files", seqs.len());
    for s in &seqs {
        assert!(
            s.lines
                .iter()
                .any(|l| matches!(l.ev, Ev::Expect(Expect::Rec { .. }))),
            "{} expects no record",
            s.name
        );
        let programs: &[&str] = match s.rig {
            RigKind::Screen => &["claude", "codex"],
            RigKind::Transcript => &["claude"],
        };
        assert!(
            programs.contains(&s.program.as_str()) && !s.build.is_empty(),
            "{}: program={} build={}",
            s.name,
            s.program,
            s.build
        );
    }
    for rig in [RigKind::Screen, RigKind::Transcript] {
        assert!(seqs.iter().any(|s| s.rig == rig), "no {rig:?} file");
    }
    let replays = replay_all(&seqs);
    assert!(
        replays.iter().any(|(_, r)| r.lost > 0),
        "no file evicts: the cursor contract never meets a gap"
    );
    assert!(
        replays.iter().any(|(_, r)| r.mail_tokens > 0),
        "no file's brief prints a mail link: the `~` check never met one"
    );
    for (s, (_, r)) in seqs.iter().zip(&replays) {
        let fresh = s.lines.iter().any(|l| matches!(l.ev, Ev::Handoff(false)));
        if r.lost == 0 && !fresh {
            assert_eq!(
                r.lines, r.every,
                "{}: the window is not what was appended",
                s.name
            );
        }
    }
}

/// MUTANT POWER (T5): every `Buggy` mutant of the turn model and of the
#[test]
fn the_corpus_catches_every_mutant() {
    let replays = replay_all(&corpus());
    let got = measure(&replays);
    let missing: Vec<_> = got
        .iter()
        .filter(|(_, (by, _))| by.is_none())
        .map(|(k, _)| *k)
        .collect();
    assert!(missing.is_empty(), "no corpus file catches {missing:?}");
    for ((join, name), (by, _)) in &got {
        let model = if *join { "join" } else { "turn" };
        println!("{model} {name}: {}", by.as_deref().unwrap_or("-"));
    }
}

/// NEGATIVE CONTROL (T5): a corpus of one busy-only file misses exactly the
#[test]
fn a_busy_only_corpus_misses_the_mutants_it_never_reaches() {
    let busy = parse(
        "busy-only.seq",
        "seq v=1 rig=screen program=claude build=2.1.0\nsee busy\nexpect open t1 turn\n",
    )
    .expect("parses");
    let replays = vec![(
        "busy-only.seq".to_string(),
        replay(&busy, Broken::No).expect("replays"),
    )];
    let got = measure(&replays);
    let missing: BTreeSet<_> = got
        .iter()
        .filter(|(_, (by, _))| by.is_none())
        .map(|(k, _)| *k)
        .collect();
    let unreached: BTreeSet<_> = got
        .iter()
        .filter(|(_, (_, reached))| !reached)
        .map(|(k, _)| *k)
        .collect();
    assert_eq!(
        missing, unreached,
        "a reached guard the busy file does not catch"
    );
    assert!(missing.contains(&(false, "EarlyRest")), "{missing:?}");
    assert!(
        JOIN_MUTANTS
            .iter()
            .all(|mu| missing.contains(&(true, mu.name)))
    );
    assert!(
        missing.len() < TURN_MUTANTS.len() + JOIN_MUTANTS.len(),
        "it catches some"
    );
}

/// The corpus file of `name`.
fn file(name: &str) -> Seq {
    corpus()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no corpus file {name}"))
}

/// NEGATIVE CONTROL (T3): on a file whose ring evicts, a reader that drops
#[test]
fn a_reader_that_drops_a_gap_breaks_the_cursor_contract() {
    for s in corpus().iter().filter(|s| s.ring.is_some()) {
        let r = replay(s, Broken::No).expect("the shipping reader");
        assert!(r.lost > 0, "{} evicts nothing", s.name);
        let e = replay(s, Broken::DropsAGap).expect_err("a dropped gap passed");
        assert!(e.contains("the cursor contract"), "{e}");
    }
}

/// NEGATIVE CONTROL (T4): a transcript file replayed through a tracker that
#[test]
fn a_tracker_that_opens_a_second_turn_fails_the_exact_lists() {
    let s = file("tx-repeat-busy-one-turn.seq");
    replay(&s, Broken::No).expect("the shipping tracker");
    let e = replay(&s, Broken::SecondTurnOnRepeat).expect_err("a split turn passed");
    assert!(
        e.contains("(open, Turn)") || e.contains("(close, Turn)"),
        "not caught by a turn list: {e}"
    );
}

/// A RECORD'S OWN `at=` FIELD AND THE LINE'S INSTANT ARE APART (6F-2's
#[test]
fn an_expect_names_a_records_at_field_apart_from_its_instant() {
    let mut log = Log::new(1);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("the session opens");
    let f = Fields::new()
        .with(Key::At, aterm_slog::Val::U(5))
        .expect("at=");
    log.mark(
        Mark::new(Kind::Render, Src::Aterm, Conf::Exact)
            .at(Node::SESSION)
            .fields(f),
        at(7),
    )
    .expect("the mark");
    let rec = log.window().last().expect("a record").clone();
    let line = enc(&rec);
    assert!(line.contains(" at=5 "), "{line}");
    let Expect::Rec {
        op,
        node,
        kind,
        toks,
        ..
    } = expect("mark - render at=5 @7").expect("parses")
    else {
        panic!("a record expect");
    };
    assert_eq!(toks, ["at=5".to_string(), format!("t={}", WALL0 + 7)]);
    assert!(matches(&line, op, &node, kind, &toks), "{line}");
    for wrong in ["mark - render at=7 @7", "mark - render at=5 @5"] {
        let Expect::Rec {
            op,
            node,
            kind,
            toks,
            ..
        } = expect(wrong).expect("parses")
        else {
            panic!("a record expect");
        };
        assert!(
            !matches(&line, op, &node, kind, &toks),
            "{wrong} matched {line}"
        );
    }
}

/// NEGATIVE CONTROL (T4, T5): a wrong `expect` of each kind, planted in a
#[test]
fn a_planted_wrong_expect_of_each_kind_fails() {
    let path = format!("{CORPUS}/tx-tool-call-and-result.seq");
    let text = std::fs::read_to_string(&path).expect("reads");
    let fails = |what: &str, text: &str| {
        let planted = parse("planted.seq", text).expect("parses");
        assert!(replay(&planted, Broken::No).is_err(), "{what} passed");
    };
    replay(&parse("good.seq", &text).expect("parses"), Broken::No).expect("the file passes");
    for plant in [
        "expect turn=open",
        "expect turn=t7",
        "expect need=approval:bash",
        "expect clause=limit · 3pm",
        "expect wake=1",
        "expect lost=3",
        "expect tx dropped=9",
        "expect tx mapped=none",
        "expect open t9 turn",
    ] {
        fails(plant, &format!("{text}\n{plant}\n"));
    }
    for (right, wrong) in [
        ("out=ok", "out=error"),
        ("vref=c1", "vref=c9"),
        ("tools=shell:2", "tools=shell:1"),
        ("expect open p2 tool", "expect open p2 subagent"),
        ("expect close p1 tool", "expect close p2 tool"),
    ] {
        assert!(text.contains(right), "{right}");
        fails(
            &format!("{right} as {wrong}"),
            &text.replacen(right, wrong, 1),
        );
    }
    let short = text.replacen("expect close p2 tool out=error\n", "", 1);
    assert_ne!(short, text);
    fails("a list one short", &short);
    let early = text.replacen("\ntx ", "\nexpect close t1 turn\ntx ", 1);
    fails("an early expect", &early);
}

/// NEGATIVE CONTROL for `expect link=` (T10): each mail-link file with its
#[test]
fn a_planted_wrong_link_fails() {
    let mut flipped = 0;
    for seq in corpus().iter().filter(|s| s.name.starts_with("t10-")) {
        let text = std::fs::read_to_string(format!("{CORPUS}/{}", seq.name)).expect("reads");
        replay(seq, Broken::No).expect("the file passes");
        for (right, wrong) in [
            ("expect link=none", "expect link=@3"),
            ("expect link=@3", "expect link=none"),
            ("expect link=@4", "expect link=@3"),
        ] {
            if !text.contains(right) {
                continue;
            }
            let planted = parse(&seq.name, &text.replacen(right, wrong, 1)).expect("parses");
            assert!(
                replay(&planted, Broken::No).is_err(),
                "{}: `{right}` as `{wrong}` passed",
                seq.name
            );
            flipped += 1;
        }
    }
    assert!(flipped >= 5, "every mail-link file planted: {flipped}");
}

/// NEGATIVE CONTROLS for the laws every replay meets: the records of a
#[test]
fn the_laws_refuse_what_a_log_never_writes() {
    let r = replay(&file("gone-closes-the-box-and-the-turn.seq"), Broken::No).expect("passes");
    let mut log = Log::new(0);
    let run = agent_session(&mut log, 0);
    let mut tr = TurnTracker::new(run);
    for v in [
        Verdict::Busy,
        Verdict::Prompt(aterm_slog::BoxWord::from_detail("bash:not-read-only")),
        Verdict::Gone,
    ] {
        tr.observe(&mut log, v, None, at(0));
    }
    let recs: Vec<Rec> = log.window().cloned().collect();
    assert_eq!(
        recs.iter().map(enc).collect::<Vec<_>>(),
        r.every,
        "the file's own records"
    );
    let good = vec![(false, recs.clone())];
    laws(&good, &[]).expect("the shipping log's records keep every law");
    let find = |op: Op, lvl: Lvl| {
        recs.iter()
            .position(|r| r.op == op && r.node.lvl == lvl)
            .expect("a record")
    };
    let (step_close, turn_close) = (find(Op::Close, Lvl::Step), find(Op::Close, Lvl::Turn));
    let (turn_open, step_open) = (find(Op::Open, Lvl::Turn), find(Op::Open, Lvl::Step));
    let mut swapped = recs.clone();
    swapped.swap(step_close, turn_close);
    let mut twice = recs.clone();
    twice.insert(step_open + 1, recs[step_open].clone());
    let mut stray = recs.clone();
    stray.push(Rec {
        node: Node::new(Lvl::Step, 9),
        ..recs[step_close].clone()
    });
    let mut late = recs.clone();
    let reopened = Rec {
        node: Node::new(Lvl::Step, 9),
        ..recs[step_open].clone()
    };
    late.push(reopened);
    let mut early = recs.clone();
    early[turn_close].t = early[turn_open].t - 1;
    for (what, broken) in [
        ("a parent closed before its child", swapped),
        ("an open repeated", twice),
        ("a close of a node never opened", stray),
        ("a step under a closed turn", late),
        ("a close dated before its open", early),
    ] {
        assert!(
            laws(&[(false, broken)], &[]).is_err(),
            "{what} kept every law"
        );
    }
    let mut ghost = transcript_join_model().init_state();
    ghost.insert("g", 1);
    assert!(laws(&good, &[ghost]).is_err(), "a join state with a ghost");
    let ring = replay(&file("ring-evicts-closed-turns.seq"), Broken::No).expect("passes");
    assert_ne!(ring.lines, ring.every);
}

/// The parser refuses what the format does not say.
#[test]
fn the_parser_refuses_what_the_format_does_not_say() {
    let head = "seq v=1 rig=screen program=claude build=2.1.0\n";
    for bad in [
        "see busyish",
        "see busy fixture=x colour=red",
        "bind 10",
        "tx prompt v1",
        "expect open t1 turnip",
        "expect open q1 turn",
        "expect tx colour=1",
        "expect nothing",
        "frobnicate",
        "at x see busy",
        "mail task",
        "mail task off=x",
        "mail tsk off=3",
        "mail task off=3 from=me",
        "ctl now",
        "ctl cause=",
        "expect link=3",
    ] {
        assert!(
            parse("bad.seq", &format!("{head}{bad}\n")).is_err(),
            "`{bad}` parsed"
        );
    }
    let tx = "seq v=1 rig=transcript program=claude build=2.1.0\n";
    for bad in [
        "half",
        "tx prompt",
        "tx tool c1 colour=red",
        "tx end fast",
        "tx wall usage-session maybe",
        "adopt maybe",
        "adopt keep-tx soon=1",
        "adopt drop-tx after=x",
        "adopt keep-tx later",
    ] {
        assert!(
            parse("bad.seq", &format!("{tx}{bad}\n")).is_err(),
            "`{bad}` parsed"
        );
    }
    assert!(parse("bad.seq", "seq v=2 rig=screen program=claude build=1\n").is_err());
    let back = parse(
        "back.seq",
        &format!("{head}at 500 see busy\nat 400 see idle\n"),
    )
    .expect("parses");
    assert!(replay(&back, Broken::No).unwrap_err().contains("backward"));
}

/// `at <ms>` on an `expect` line waits to that instant before the check, as
#[test]
fn an_expect_at_an_instant_is_checked_at_it() {
    let head = "seq v=1 rig=screen program=claude build=2.1.0\n\
                see busy\nexpect open t1 turn\nsee idle\n";
    let run = |tail: &str| {
        let seq = parse("at.seq", &format!("{head}{tail}")).expect("parses");
        replay(&seq, Broken::No)
    };
    assert!(
        run("at 5000 expect turn=open\n").is_err(),
        "the checkpoint was taken before its instant"
    );
    run("at 5000 expect turn=none\n").expect("closed by then");
    run("at 5000\nexpect turn=none\n").expect("the same, on two lines");
    assert!(run("at 5000\nexpect turn=open\n").is_err());
    let at = run("at 5000 expect close t1 turn\n").expect("the settle's close");
    assert!(
        at.trace.iter().any(|t| t.text.starts_with("at 5000")),
        "the wait is traced at its line"
    );
}

/// `seq` with a `program` step before each of its steps (the program's own
fn interleaved(seq: &Seq) -> Seq {
    const REPORTS: [(&str, Option<&str>); 3] = [
        ("working", None),
        ("blocked", Some("permission")),
        ("done", None),
    ];
    let mut out = seq.clone();
    out.name = format!("{} with program marks", seq.name);
    out.lines.clear();
    for (i, line) in seq.lines.iter().enumerate() {
        match &line.ev {
            Ev::Expect(Expect::Rec { .. } | Expect::Lost(_)) => continue,
            Ev::Expect(_) | Ev::Nothing => {}
            _ => {
                let (state, ask) = REPORTS[i % REPORTS.len()];
                out.lines.push(Line {
                    no: line.no,
                    text: format!("program {state}"),
                    at: None,
                    ev: Ev::Program {
                        state: state.to_string(),
                        ask: ask.map(str::to_string),
                    },
                });
            }
        }
        out.lines.push(line.clone());
    }
    out
}

/// A record line without its seq (its first token), and `None` for a
fn unnumbered(line: &str) -> Option<String> {
    let (_, rest) = line.split_once(' ')?;
    let toks: Vec<&str> = rest.split(' ').collect();
    match toks.as_slice() {
        ["mark", _, "program", ..] => None,
        ["mark", "-", "handoff", ..] => Some(
            toks.iter()
                .filter(|t| !t.starts_with("carried="))
                .copied()
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => Some(rest.to_string()),
    }
}

/// One host call as [`folded`] compares it: its name, the model state after
type FoldedCall = (String, Option<State>, Vec<String>);

/// A path's host calls and final window, seqs aside and program marks out.
fn folded(p: &seq_twins::Path) -> (Vec<FoldedCall>, Vec<String>) {
    let steps = p
        .steps
        .iter()
        .map(|t| {
            (
                t.step.clone(),
                t.state.clone(),
                t.lines.iter().filter_map(|l| unnumbered(l)).collect(),
            )
        })
        .collect();
    (
        steps,
        p.lines.iter().filter_map(|l| unnumbered(l)).collect(),
    )
}

/// A `.seq` WITH PROGRAM MARKS INTERLEAVED FOLDS TO THE SAME LINES (the fold never reads a program mark).
#[test]
fn program_marks_interleaved_fold_to_the_same_lines() {
    let mut ran = 0;
    let mut marks = 0;
    for seq in corpus() {
        if seq.ring.is_some() || seq.lines.iter().any(|l| matches!(l.ev, Ev::CtlCaused(_))) {
            continue;
        }
        let twin = interleaved(&seq);
        let plain = seq_twins::seq_path(&seq).expect("the file replays");
        let with = seq_twins::seq_path(&twin).unwrap_or_else(|e| panic!("{e}"));
        let (a, b) = (folded(&plain), folded(&with));
        if let Some((i, (x, y))) = a.0.iter().zip(&b.0).enumerate().find(|(_, (x, y))| x != y) {
            panic!(
                "{}: host call {i} differs:\n  plain: {x:?}\n  with:  {y:?}",
                seq.name
            );
        }
        assert_eq!(a.0.len(), b.0.len(), "{}: the host calls", seq.name);
        assert_eq!(a.1, b.1, "{}: the final window", seq.name);
        marks += with.lines.len() - b.1.len();
        ran += 1;
    }
    assert!(ran >= 60, "{ran} files compared");
    assert!(marks > 0, "no program mark stayed in any window");
}
