// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab reduce` held to ruling T7 over every `supervise/fixtures` transcript:
//! the reduction folds as its source and keeps no text; and the negative
//! controls (a reducer that keeps content, one that replaces `type`, one that
//! blanks a notice's text) each fail the check they break.

use std::collections::{HashMap, HashSet};

use aterm_agent::harness::steps::{
    COMPACT, End, Step, StepEvent, StepFold, StepId, USER_MARKERS, tool_class,
};
use aterm_phase::WallKind;

use super::*;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-agent/src/supervise/fixtures"
);
const FP: u64 = 0x5eed_0000_0000_0007;
const ID: Option<(u64, u64)> = Some((16_777_220, 7));

/// One screen wall of each class, as the step fold's `confirms` is asked.
const CLASSES: [WallKind; 5] = [
    WallKind::UsageSession,
    WallKind::Context,
    WallKind::Auth,
    WallKind::Overloaded,
    WallKind::Memory,
];

/// Every transcript fixture: its name and text.
fn fixtures() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = std::fs::read_dir(FIXTURES)
        .expect("the fixtures directory")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("claude-") && n.contains("-transcript-") && n.ends_with(".jsonl"))
        .map(|n| {
            let text = std::fs::read_to_string(format!("{FIXTURES}/{n}")).expect("a fixture");
            (n, text)
        })
        .collect();
    out.sort();
    assert!(out.len() >= 20, "the transcript fixtures: {}", out.len());
    out
}

/// Whether a line is a row (no `#` header, not blank).
fn is_row(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with('#')
}

/// A file folded as the tap folds it: bound at its start (floor 0), every
/// line pushed, then taken twice (a held prompt's release); each step with
/// the ordinal of the row its offset lies in.
struct Folded {
    steps: Vec<(usize, Step)>,
    lost: u64,
    overflow: u64,
    end: End,
}

fn fold(text: &str) -> Folded {
    let mut f = StepFold::new(0, None);
    f.read_at(FP, ID, 0, false);
    let (mut end, mut rows) = (0u64, Vec::new());
    for line in text.lines() {
        end += line.len() as u64 + 1;
        f.push(line.as_bytes(), end);
        rows.push((end, is_row(line)));
    }
    let a = f.take();
    let b = f.take();
    let ordinal = |off: u64| {
        let line = rows.partition_point(|&(e, _)| e < off);
        rows[..line.min(rows.len())]
            .iter()
            .filter(|(_, r)| *r)
            .count()
    };
    let steps = a
        .steps
        .iter()
        .chain(b.steps.iter())
        .map(|s| (ordinal(s.pos.off), *s))
        .collect();
    Folded {
        steps,
        lost: a.lost + b.lost,
        overflow: a.overflow + b.overflow,
        end: b.end,
    }
}

/// One to one: `a` always maps to `b` and `b` from `a`.
#[derive(Default)]
struct OneToOne {
    fwd: HashMap<String, String>,
    back: HashMap<String, String>,
}

impl OneToOne {
    fn same(&mut self, a: &StepId, b: &StepId) -> Result<(), String> {
        self.pair(a.as_str(), b.as_str())
    }

    fn pair(&mut self, a: &str, b: &str) -> Result<(), String> {
        let (a, b) = (a.to_string(), b.to_string());
        let f = self.fwd.entry(a.clone()).or_insert_with(|| b.clone());
        let g = self.back.entry(b.clone()).or_insert_with(|| a.clone());
        if *f == b && *g == a {
            Ok(())
        } else {
            Err(format!("ids {a} and {b} are not one to one"))
        }
    }
}

fn kind(e: &StepEvent) -> &'static str {
    match e {
        StepEvent::Bind { .. } => "bind",
        StepEvent::Prompt { .. } => "prompt",
        StepEvent::Interrupt => "interrupt",
        StepEvent::ToolUse { .. } => "tool",
        StepEvent::ToolResult { .. } => "result",
        StepEvent::TurnEnd { .. } => "end",
        StepEvent::EndWall { .. } => "endwall",
        StepEvent::Compact => "compact",
        StepEvent::Model { .. } => "model",
        StepEvent::Off { .. } => "off",
    }
}

/// The step events of two folds agree: kinds in order and at the same
/// rows, the same states, closed words and numbers, ids one to one, and
/// times one shift apart.
fn same_event(a: &StepEvent, b: &StepEvent, ids: &mut OneToOne) -> Result<(), String> {
    use StepEvent as E;
    match (a, b) {
        (E::Bind { len: x }, E::Bind { len: y }) if x == y => Ok(()),
        (E::Prompt { vref: x }, E::Prompt { vref: y }) => ids.same(x, y),
        (E::Interrupt, E::Interrupt) | (E::Compact, E::Compact) => Ok(()),
        (
            E::ToolUse {
                id: i,
                tool: t,
                cls: c,
                subagent: s,
            },
            E::ToolUse {
                id: j,
                tool: u,
                cls: d,
                subagent: r,
            },
        ) => {
            ids.same(i, j)?;
            let named = !t.as_str().starts_with("mcp__") && tool_class(t.as_str()).0 != "other";
            if c != d || s != r || (named && t.as_str() != u.as_str()) {
                return Err(format!("tool {t:?} {c} became {u:?} {d}"));
            }
            Ok(())
        }
        (E::ToolResult { id: i, error: x }, E::ToolResult { id: j, error: y }) if x == y => {
            ids.same(i, j)
        }
        (E::TurnEnd { .. }, E::TurnEnd { .. })
        | (E::EndWall { .. }, E::EndWall { .. })
        | (E::Model { .. }, E::Model { .. })
        | (E::Off { .. }, E::Off { .. })
            if a == b =>
        {
            Ok(())
        }
        _ => Err(format!("{a:?} became {b:?}")),
    }
}

/// Claude Code's notices in a file, by row ordinal: an `isApiErrorMessage`
/// row's joined text, and a local command's output in its tags (the text
/// `wall_confirm` classes).
fn notices(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (n, line) in text.lines().filter(|l| is_row(l)).enumerate() {
        let row = parse(line.trim()).expect("a row");
        let kind = row.get("type").and_then(Node::as_str);
        if kind == Some("assistant") && row.get("isApiErrorMessage") == Some(&Node::Bool(true)) {
            out.push((n, notice_text(&row).0));
        }
        if kind == Some("system")
            && row.get("subtype").and_then(Node::as_str) == Some("local_command")
            && let Some(c) = row.get("content").and_then(Node::as_str)
        {
            let t = c.trim();
            // `command_notice`: only an output that reads as a full context
            // is a notice.
            if let Some(inner) = ["<local-command-stderr>", "<local-command-stdout>"]
                .iter()
                .find_map(|o| t.strip_prefix(o))
                .map(|i| {
                    i.split("</local-command-")
                        .next()
                        .unwrap_or_default()
                        .trim()
                })
                .filter(|i| aterm_phase::classify_wall(i) == Some(WallKind::Context))
            {
                out.push((n, inner.to_string()));
            }
        }
    }
    out
}

/// THE FOLD CHECK (T7): the reduction folds as its source.
fn check_folds(name: &str, source: &str, r: &Reduction) -> Result<(), String> {
    let (a, b) = (fold(source), fold(&r.text));
    let kinds =
        |f: &Folded| -> Vec<&'static str> { f.steps.iter().map(|(_, s)| kind(&s.ev)).collect() };
    if a.steps.len() != b.steps.len() {
        return Err(format!(
            "{name}: {} steps became {}: {:?} / {:?}",
            a.steps.len(),
            b.steps.len(),
            kinds(&a),
            kinds(&b)
        ));
    }
    let shift = r.shift_days * 86_400_000;
    let mut ids = OneToOne::default();
    for ((ra, sa), (rb, sb)) in a.steps.iter().zip(&b.steps) {
        let at = |s: &Step| i64::try_from(s.pos.at_ms).unwrap_or(i64::MAX);
        let timed = !matches!(sa.ev, StepEvent::Bind { .. });
        if ra != rb || sa.state != sb.state || (timed && at(sb) - at(sa) != shift) {
            return Err(format!(
                "{name}: {:?} at row {ra} ({}) became {:?} at row {rb} ({})",
                sa.ev, sa.pos.at_ms, sb.ev, sb.pos.at_ms
            ));
        }
        same_event(&sa.ev, &sb.ev, &mut ids).map_err(|e| format!("{name}: row {ra}: {e}"))?;
    }
    if (a.lost, a.overflow) != (b.lost, b.overflow) {
        return Err(format!(
            "{name}: lost/overflow {:?} became {:?}",
            (a.lost, a.overflow),
            (b.lost, b.overflow)
        ));
    }
    let shifted = a.end.at_ms.map(|t| i64::try_from(t).unwrap_or(0) + shift);
    let ends_agree = a.end.last == b.end.last
        && a.end.closed == b.end.closed
        && shifted == b.end.at_ms.map(|t| i64::try_from(t).unwrap_or(0))
        && a.end.wall() == b.end.wall()
        && CLASSES
            .iter()
            .all(|&k| a.end.confirms(k) == b.end.confirms(k));
    if !ends_agree {
        return Err(format!("{name}: the end {:?} became {:?}", a.end, b.end));
    }
    let (na, nb) = (notices(source), notices(&r.text));
    if na.len() != nb.len() {
        return Err(format!("{name}: {} notices became {}", na.len(), nb.len()));
    }
    for ((ra, ta), (rb, tb)) in na.iter().zip(&nb) {
        let (ka, kb) = (
            aterm_phase::classify_wall(ta),
            aterm_phase::classify_wall(tb),
        );
        if ra != rb || ka != kb || Reading::of(ta) != Reading::of(tb) {
            return Err(format!(
                "{name}: the notice at row {ra} classed {ka:?} became {kb:?} ({:?})",
                Reading::of(tb)
            ));
        }
    }
    let walls = |f: &Folded| -> Vec<StepEvent> {
        f.steps
            .iter()
            .map(|(_, s)| s.ev)
            .filter(|e| matches!(e, StepEvent::EndWall { .. }))
            .collect()
    };
    if walls(&a) != walls(&b) {
        return Err(format!(
            "{name}: walls {:?} became {:?}",
            walls(&a),
            walls(&b)
        ));
    }
    Ok(())
}

/// The words a reduction may hold that are no source's text: the markers,
/// the canonical phrases and their tags, and the reset clause's frame.
fn vocabulary() -> Vec<String> {
    let mut v: Vec<String> = USER_MARKERS.iter().map(|s| (*s).to_string()).collect();
    v.push(COMPACT.to_string());
    v.extend(
        phrases()
            .into_iter()
            .filter(|p| !p.is_empty())
            .map(str::to_string),
    );
    v.extend(
        [
            "</local-command-stdout>",
            "</local-command-stderr>",
            "API Error: ",
            " can't help with this",
        ]
        .iter()
        .map(|s| (*s).to_string()),
    );
    // Longest first, so a phrase is cut whole before a shorter one inside it.
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v
}

/// Every string a value holds (keys too).
fn strings<'a>(v: &'a Node, out: &mut Vec<&'a str>) {
    match v {
        Node::Str(s) => out.push(s),
        Node::Arr(a) => a.iter().for_each(|x| strings(x, out)),
        Node::Obj(m) => m.iter().for_each(|(k, x)| {
            out.push(k);
            strings(x, out);
        }),
        _ => {}
    }
}

/// An id the reducer wrote (module docs: a UUID of zeros and a count, or
/// `<prefix>_r<n>` / `r<n>`, with `/` when the source's was no `StepId`).
fn placeholder_id(s: &str) -> bool {
    let s = s.strip_suffix('/').unwrap_or(s);
    let count = |t: &str| t.len() >= 4 && t.bytes().all(|b| b.is_ascii_digit());
    s.strip_prefix("00000000-0000-4000-8000-")
        .is_some_and(|t| t.len() == 12 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        || s.strip_prefix('r').is_some_and(count)
        || s.split_once("_r")
            .is_some_and(|(p, t)| p.bytes().all(|b| b.is_ascii_lowercase()) && count(t))
}

/// THE TEXT CHECK (T7, `a_reduction_keeps_no_text`): no 8-byte run of any
/// source string (a key, a value, a `#` line) survives in the reduction but
/// in the kept vocabulary: a closed word kept as written under its key, a
/// call's tool name at its own place, a remapped id, a WHOLE rebased stamp,
/// a marker, a canonical phrase, a reset clause's rebased numbers, and an
/// identifier key outside a free subtree (the vendor's schema). Inside a
/// FREE subtree ([`Pos::free`]) nothing is exempt: every string there is
/// `""` or held to the 8-byte rule, and so is every key kept as written.
/// And wherever it sits, a tool's argument found by its shape keeps neither
/// its name nor a word ([`check_tool_arguments`]), and an id is one to one
/// across keys and values ([`check_ids`]).
fn check_no_text(name: &str, source: &str, r: &Reduction, program: Program) -> Result<(), String> {
    let src: Vec<Node> = source
        .lines()
        .filter(|l| is_row(l))
        .map(|l| parse(l.trim()).expect("a row"))
        .collect();
    let mut runs: HashSet<&[u8]> = HashSet::new();
    let mut all = Vec::new();
    for row in &src {
        strings(row, &mut all);
    }
    for l in source.lines().filter(|l| l.trim_start().starts_with('#')) {
        all.push(l);
    }
    for s in &all {
        for w in s.as_bytes().windows(8) {
            runs.insert(w);
        }
    }
    let vocab = vocabulary();
    let check = |what: &str, s: &str| -> Result<(), String> {
        let mut rest = s.split(" · resets ").next().unwrap_or_default().to_string();
        for v in &vocab {
            rest = rest.replace(v.as_str(), "\u{0}");
        }
        match rest.as_bytes().windows(8).find(|w| runs.contains(w)) {
            Some(w) => Err(format!(
                "{name}: {what} keeps the source's {:?} (in {s:?})",
                String::from_utf8_lossy(w)
            )),
            None => Ok(()),
        }
    };
    let mut lines = r.text.lines();
    // The reduction's own line: fixed words, numbers, and the build a row
    // named (a closed word).
    let head = lines.next().unwrap_or_default();
    if head != header(program, r.build.as_deref(), r.shift_days, r.rows)
        || !r.build.as_deref().is_none_or(is_word)
    {
        return Err(format!("{name}: the header {head:?} is no reduction's own"));
    }
    let red: Vec<Node> = lines.map(|l| parse(l).expect("a reduced row")).collect();
    if red.len() != src.len() {
        return Err(format!("{name}: {} rows became {}", src.len(), red.len()));
    }
    let mut calls = HashSet::new();
    for row in &src {
        call_ids(row, &mut calls);
    }
    let args = Args {
        calls: &calls,
        check: &check,
    };
    for (a, b) in src.iter().zip(&red) {
        let payload = a
            .get("payload")
            .and_then(|p| p.get("type"))
            .and_then(Node::as_str);
        let ctx = Ctx {
            program,
            words: match program {
                Program::Claude => CLAUDE_WORDS,
                Program::Codex => CODEX_WORDS,
            },
            call: program == Program::Codex
                && a.get("type").and_then(Node::as_str) == Some("response_item")
                && matches!(payload, Some("function_call" | "custom_tool_call")),
            check: &check,
        };
        same_text(a, b, Pos::ROOT, false, &ctx)?;
        check_tool_arguments(a, b, Arg::ROW, &args).map_err(|e| format!("{name}: {e}"))?;
    }
    check_ids(&src, &red).map_err(|e| format!("{name}: {e}"))
}

/// A key the reducer named (`k<n>`): it holds nothing of the key.
fn renamed(k: &str) -> bool {
    k.strip_prefix('k')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

/// The ids of a file's calls, found by their SHAPE: the `id` of any object
/// whose `type` ends in `tool_use`, wherever it sits.
fn call_ids<'a>(v: &'a Node, out: &mut HashSet<&'a str>) {
    match v {
        Node::Obj(m) => {
            if v.get("type")
                .and_then(Node::as_str)
                .is_some_and(|t| t.ends_with("tool_use"))
                && let Some(id) = v.get("id").and_then(Node::as_str)
            {
                out.insert(id);
            }
            m.iter().for_each(|(_, x)| call_ids(x, out));
        }
        Node::Arr(a) => a.iter().for_each(|x| call_ids(x, out)),
        _ => {}
    }
}

/// What [`check_tool_arguments`] reads a file by: its calls' ids
/// ([`call_ids`]) and the 8-byte rule.
struct Args<'a> {
    calls: &'a HashSet<&'a str>,
    check: &'a dyn Fn(&str, &str) -> Result<(), String>,
}

/// Where [`check_tool_arguments`] is in a row: `here`, its own keys are
/// names a schema's author wrote (a schema's `properties`, its `$defs`);
/// `below`, every key under it is a name a tool's caller wrote (a call's
/// input); `said`, every string under it is what either wrote.
#[derive(Clone, Copy)]
struct Arg {
    here: bool,
    below: bool,
    said: bool,
}

impl Arg {
    const ROW: Arg = Arg {
        here: false,
        below: false,
        said: false,
    };
}

/// THE ARGUMENT CHECK (T7), apart from where [`opens_free`] says a free
/// subtree opens: a tool's arguments, found by their SHAPE in the source row
/// `a` wherever they sit (the keys of a JSON schema's `properties`, an
/// object whose `properties` is an object and whose `type` is `"object"` or
/// absent, and of any `$defs` object, the definitions a schema names; every
/// key under the `input` of any object whose `type` ends in
/// `tool_use`; and every key under what a map keyed by a call's id holds,
/// [`call_ids`], as Claude Code 2.1.303's `wireToolInputs` copies each
/// call's input), never have their names kept as written in its reduction
/// `b` (each is `k<n>`), and every string there is `""` or held to the
/// 8-byte rule, a closed word under a closed key (`mode`) among them.
fn check_tool_arguments(a: &Node, b: &Node, at: Arg, args: &Args<'_>) -> Result<(), String> {
    match (a, b) {
        (Node::Obj(x), Node::Obj(y)) => {
            let kind = a.get("type");
            let schema = matches!(a.get("properties"), Some(Node::Obj(_)))
                && kind.is_none_or(|t| t.as_str() == Some("object"));
            let call = kind
                .and_then(Node::as_str)
                .is_some_and(|t| t.ends_with("tool_use"));
            for ((ka, va), (kb, vb)) in x.iter().zip(y) {
                if (at.here || at.below) && ka == kb && !renamed(kb) {
                    return Err(format!(
                        "a name a tool's caller or schema wrote, {ka:?}, is kept as written"
                    ));
                }
                let names =
                    (schema && ka == "properties") || (ka == "$defs" && matches!(va, Node::Obj(_)));
                let below = at.below || (call && ka == "input") || args.calls.contains(ka.as_str());
                let child = Arg {
                    here: names,
                    below,
                    said: at.said || names || below,
                };
                check_tool_arguments(va, vb, child, args)?;
            }
            Ok(())
        }
        (Node::Arr(x), Node::Arr(y)) => x
            .iter()
            .zip(y)
            .try_for_each(|(p, q)| check_tool_arguments(p, q, Arg { here: false, ..at }, args)),
        (Node::Str(_), Node::Str(t)) if at.said && !t.is_empty() => {
            (args.check)("a tool's argument", t)
        }
        _ => Ok(()),
    }
}

/// THE ID CHECK (T7: ids one to one across the file, its keys among them),
/// read off the reduction and not the reducer's rule: a source string the
/// reduction writes as a placeholder id ([`placeholder_id`]), where it is a
/// value or a key, is that same id wherever it is one, no two share one,
/// and none is kept as written as a key (a map keyed by a call's id).
fn check_ids(src: &[Node], red: &[Node]) -> Result<(), String> {
    fn pairs<'a>(
        a: &'a Node,
        b: &'a Node,
        ids: &mut Vec<(&'a str, &'a str)>,
        kept: &mut Vec<&'a str>,
    ) {
        match (a, b) {
            (Node::Str(s), Node::Str(t)) if s != t && placeholder_id(t) => ids.push((s, t)),
            (Node::Arr(x), Node::Arr(y)) => {
                x.iter().zip(y).for_each(|(p, q)| pairs(p, q, ids, kept))
            }
            (Node::Obj(x), Node::Obj(y)) => {
                for ((ka, va), (kb, vb)) in x.iter().zip(y) {
                    if ka == kb {
                        kept.push(ka);
                    } else if placeholder_id(kb) {
                        ids.push((ka, kb));
                    }
                    pairs(va, vb, ids, kept);
                }
            }
            _ => {}
        }
    }
    let (mut ids, mut kept) = (Vec::new(), Vec::new());
    for (a, b) in src.iter().zip(red) {
        pairs(a, b, &mut ids, &mut kept);
    }
    let mut one = OneToOne::default();
    for (s, t) in ids {
        one.pair(s, t)?;
    }
    match kept.into_iter().find(|k| one.fwd.contains_key(*k)) {
        Some(k) => Err(format!(
            "the id {k:?} is remapped as a value but kept as written as a key"
        )),
        None => Ok(()),
    }
}

/// Where a value sits, as the T7 contract names its places: its member's
/// key, how many keys deep, the content part it is in, and whether it is
/// inside a FREE subtree ([`opens_free`] and [`is_schema`], the reducer's
/// own rule: what a tool takes or returns, what a schema's author named,
/// what Claude Code sends or attaches for the model, a map keyed by
/// tracked files or by ids; a Codex item's `arguments`, `input`, `output`),
/// where whatever is there is what someone said or named. That rule is not
/// this check's oracle for itself: a place it misses is found by
/// [`check_tool_arguments`], which reads a tool's arguments by their shape,
/// by [`check_ids`], which reads ids off the reduction, and by the plants,
/// which hold a unique word at each place.
#[derive(Clone, Copy)]
struct Pos<'a> {
    key: &'a str,
    depth: usize,
    part: Option<&'a str>,
    free: bool,
}

impl Pos<'_> {
    const ROOT: Pos<'static> = Pos {
        key: "",
        depth: 0,
        part: None,
        free: false,
    };
}

/// What [`same_text`] reads one row by.
struct Ctx<'a> {
    program: Program,
    words: &'a [&'a str],
    /// The row is a Codex call (`response_item`, its payload a
    /// `function_call` or `custom_tool_call`): its `payload.name` is a
    /// tool's.
    call: bool,
    check: &'a dyn Fn(&str, &str) -> Result<(), String>,
}

fn same_text(
    a: &Node,
    b: &Node,
    at: Pos<'_>,
    in_content: bool,
    ctx: &Ctx<'_>,
) -> Result<(), String> {
    match (a, b) {
        (Node::Obj(x), Node::Obj(y)) if x.len() == y.len() => {
            let own = if in_content {
                a.get("type").and_then(Node::as_str)
            } else {
                None
            };
            let part = own.or(at.part);
            // A JSON schema is free whole, wherever it sits.
            let free = at.free || is_schema(a);
            for ((ka, va), (kb, vb)) in x.iter().zip(y) {
                // A key remapped as an id ([`check_ids`] holds it one to
                // one): what it keys is free.
                let id_key = ka != kb && placeholder_id(kb);
                if ka == kb && is_ident(ka) {
                    // Kept as written: the vendor's schema outside a free
                    // subtree; inside one, held to the 8-byte rule.
                    if free {
                        (ctx.check)("a key in a free subtree", kb)?;
                    }
                } else if !renamed(kb) && !id_key {
                    return Err(format!("the key {ka:?} became {kb:?}"));
                }
                let depth = at.depth + 1;
                let child = Pos {
                    key: ka,
                    depth,
                    part,
                    free: free || opens_free(ctx.program, at.key, ka, depth, part, id_key),
                };
                same_text(va, vb, child, false, ctx)?;
            }
            Ok(())
        }
        (Node::Arr(x), Node::Arr(y)) if x.len() == y.len() => x
            .iter()
            .zip(y)
            .try_for_each(|(p, q)| same_text(p, q, at, at.key == "content", ctx)),
        (Node::Str(_), Node::Str(t)) if at.free => {
            if t.is_empty() {
                Ok(())
            } else {
                (ctx.check)(at.key, t)
            }
        }
        (Node::Str(s), Node::Str(t)) => {
            let key = at.key;
            let word = ctx.words.contains(&key) && s == t && is_word(s);
            let tool = key == "name"
                && s == t
                && match ctx.program {
                    Program::Claude => {
                        at.part == Some("tool_use")
                            && at.depth == 3
                            && !s.starts_with("mcp__")
                            && tool_class(s).0 != "other"
                    }
                    Program::Codex => ctx.call && at.depth == 2 && is_word(s) && !s.contains("__"),
                };
            let time = is_time_key(key) && stamp_day(t).is_some();
            if t.is_empty() || word || tool || time || placeholder_id(t) {
                Ok(())
            } else {
                (ctx.check)(key, t)
            }
        }
        (Node::Num(_), Node::Num(_)) | (Node::Null, Node::Null) => Ok(()),
        (Node::Bool(x), Node::Bool(y)) if x == y => Ok(()),
        _ => Err(format!("a value under {:?} changed its shape", at.key)),
    }
}

/// Reduce every fixture by `rules` and run `check` on each: the names that
/// failed it, with why.
fn failures(
    rules: &dyn Rules,
    check: &dyn Fn(&str, &str, &Reduction) -> Result<(), String>,
) -> Vec<String> {
    fixtures()
        .iter()
        .filter_map(|(n, text)| {
            let r = reduce_with(text, Program::Claude, rules).expect("a reduction");
            check(n, text, &r).err()
        })
        .collect()
}

fn folds(n: &str, t: &str, r: &Reduction) -> Result<(), String> {
    check_folds(n, t, r)
}

fn no_text(n: &str, t: &str, r: &Reduction) -> Result<(), String> {
    check_no_text(n, t, r, Program::Claude)
}

/// T7, P11: over every `supervise/fixtures` transcript the reduction folds as
/// its source — the same steps at the same rows in the same states, the same
/// closed words and numbers, ids one to one, times one shift apart, the same
/// end (its wall and the classes it confirms) and the same `EndWall` events
/// — and every notice classes as its source's (`classify_wall(reduced) ==
/// classify_wall(source)`, and the readings `notice_wall` adds).
#[test]
fn a_reduction_folds_as_its_source() {
    let bad = failures(&P11, &folds);
    assert!(bad.is_empty(), "{bad:#?}");
    // Not vacuous: the fixtures hold walls, notices, calls and turn ends.
    let (mut walls, mut notes, mut tools, mut ends) = (0, 0, 0, 0);
    for (_, text) in fixtures() {
        let f = fold(&text);
        walls += f
            .steps
            .iter()
            .filter(|(_, s)| matches!(s.ev, StepEvent::EndWall { .. }))
            .count();
        tools += f
            .steps
            .iter()
            .filter(|(_, s)| matches!(s.ev, StepEvent::ToolUse { .. }))
            .count();
        ends += f
            .steps
            .iter()
            .filter(|(_, s)| matches!(s.ev, StepEvent::TurnEnd { .. }))
            .count();
        notes += notices(&text).len();
    }
    assert!(
        walls >= 10 && notes >= 15 && tools >= 5 && ends >= 10,
        "{walls} {notes} {tools} {ends}"
    );
}

/// T7, P11: no 8-byte run of any source string survives, but the kept
/// vocabulary (the anchor phrases among it).
#[test]
fn a_reduction_keeps_no_text() {
    let bad = failures(&P11, &no_text);
    assert!(bad.is_empty(), "{bad:#?}");
}

/// A Claude Code transcript holding words where the T7 rule keeps none: a
/// prompt, then a key an MCP tool named in its input (and one nested under
/// it), a key a question's answer is filed under in a result's output,
/// words after a stamp under a time key, and an attachment's key and its
/// word under a closed key; a prompt typed while Claude Code worked, whose
/// `origin` a reader reads; then the places the second review found (each
/// a shape Claude Code 2.1.303 writes or carries): an MCP tool's argument
/// name in an `api-request-shape` row's tool list, a question's answer in
/// a row a `progress` row nests, an MCP result's `structuredContent` key
/// in its row's `mcpMeta`, a server tool's input and its result's content,
/// and a tracked file's name keying a file-history snapshot; then the
/// places the land review found (2.1.303 writes each place): a deferred
/// MCP tool's argument name in an `api-request-blob` row's `tool_addition`
/// part (and a second tool's schema, as an MCP server may give one, whose
/// top names a definition under `$defs` rather than holding `properties`,
/// so only its key says it is a schema), and a call's id keying the row's
/// `wireIngestContext` (before the call) and `wireToolInputs` (after it,
/// the call's input again: an MCP argument name and a word under `mode`);
/// and last a schema at a place no name says is one (a function's
/// `parameters`, which 2.1.303 does not write), freed by its shape alone.
const CLAUDE_PLANT: [&str; 14] = [
    r#"{"type":"user","timestamp":"2026-10-16T10:00:00.000Z","uuid":"11111111-1111-4111-8111-111111111111","message":{"role":"user","content":"hello"},"version":"2.1.303"}"#,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:01.000Z","uuid":"21111111-1111-4111-8111-111111111111","message":{"id":"msg_01","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_01","name":"mcp__quokkadb__lookup","input":{"quokkaledgerInputKey":"x","customers":{"quokkaledgerCustomer":{"status":"open"}}}}]}}"#,
    r#"{"type":"user","timestamp":"2026-10-16T10:00:02.000Z","uuid":"31111111-1111-4111-8111-111111111111","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"x"}]},"toolUseResult":{"answers":{"quokkaledgerAnswer":"yes"}}}"#,
    r#"{"type":"system","timestamp":"2026-10-16T10:00:03.000Z quokkaledger tail","uuid":"41111111-1111-4111-8111-111111111111","content":"x"}"#,
    r#"{"type":"attachment","timestamp":"2026-10-16T10:00:04.000Z","uuid":"51111111-1111-4111-8111-111111111111","attachment":{"type":"todo","mode":"quokkaledgerMode","items":{"quokkaledgerTodo":{"status":"open"}}}}"#,
    r#"{"type":"attachment","timestamp":"2026-10-16T10:00:05.000Z","uuid":"61111111-1111-4111-8111-111111111111","attachment":{"type":"queued_command","prompt":"quokkaledger words","origin":{"kind":"human"}}}"#,
    r#"{"type":"api-request-shape","sessionId":"71111111-1111-4111-8111-111111111111","shapeHash":"x","timestamp":"2026-10-16T10:00:06.000Z","version":"2.1.303","shape":{"system":[{"type":"text","text":"x"}],"tools":[{"name":"mcp__quokkadb__lookup","description":"x","input_schema":{"type":"object","properties":{"quokkaledgerAccountKey":{"description":"y","type":"string"}},"required":["quokkaledgerAccountKey"]}}]}}"#,
    r#"{"type":"progress","timestamp":"2026-10-16T10:00:07.000Z","uuid":"81111111-1111-4111-8111-111111111111","data":{"type":"agent_progress","message":{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_02","content":"x"}]},"toolUseResult":{"answers":{"quokkaledgerNestedAnswer":"yes"}}}}}"#,
    r#"{"type":"user","timestamp":"2026-10-16T10:00:08.000Z","uuid":"91111111-1111-4111-8111-111111111111","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_01","content":"x"}]},"mcpMeta":{"structuredContent":{"quokkaledgerStructKey":"v"}}}"#,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:09.000Z","uuid":"a1111111-1111-4111-8111-111111111111","message":{"id":"msg_02","role":"assistant","model":"claude-opus-5-5","content":[{"type":"server_tool_use","id":"srvtoolu_01","name":"web_search","input":{"quokkaledgerQuery":"x"}},{"type":"web_search_tool_result","tool_use_id":"srvtoolu_01","content":{"quokkaledgerResultKey":"x"}}]}}"#,
    r#"{"type":"file-history-snapshot","messageId":"b1111111-1111-4111-8111-111111111111","snapshot":{"messageId":"b1111111-1111-4111-8111-111111111111","trackedFileBackups":{"quokkaledgerNotes":{"backupFileName":"x@v1","version":1,"backupTime":"2026-10-16T10:00:10.000Z"}},"timestamp":"2026-10-16T10:00:10.000Z"},"isSnapshotUpdate":false}"#,
    r##"{"type":"api-request-blob","sessionId":"c1111111-1111-4111-8111-111111111111","hash":"x","message":{"role":"system","content":[{"type":"tool_addition","tool":{"type":"tool_definition","definition":{"name":"mcp__quokkadb__lookup","description":"x","input_schema":{"type":"object","properties":{"quokkaledgerBlobArg":{"type":"string","description":"y"}},"required":["quokkaledgerBlobArg"]}}}},{"type":"tool_addition","tool":{"type":"tool_definition","definition":{"name":"mcp__quokkadb__find","description":"x","input_schema":{"$ref":"#/$defs/quokkaledgerDefName","$defs":{"quokkaledgerDefName":{"type":"object","properties":{"quokkaledgerDefArg":{"type":"string"}}}}}}}},{"type":"tool_removal","tool":{"type":"tool_reference","name":"mcp__quokkadb__lookup"}}]}}"##,
    r#"{"type":"assistant","timestamp":"2026-10-16T10:00:12.000Z","uuid":"d1111111-1111-4111-8111-111111111111","wireIngestContext":{"toolu_01quokkaledgerCallId":{"cwd":"/private/tmp/x"}},"message":{"id":"msg_03","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_01quokkaledgerCallId","name":"mcp__quokkadb__lookup","input":{"quokkaledgerWireKey":"x","mode":"quokkaledgerModeWord"}}]},"wireToolInputs":{"toolu_01quokkaledgerCallId":{"quokkaledgerWireKey":"x","mode":"quokkaledgerModeWord"}}}"#,
    r#"{"type":"tool-list","timestamp":"2026-10-16T10:00:13.000Z","uuid":"e1111111-1111-4111-8111-111111111111","tools":[{"name":"x","parameters":{"type":"object","properties":{"quokkaledgerParamArg":{"type":"string"}}}}]}"#,
];

/// What an earlier rule reduced planted row `n` to, beside the prompt
/// (`lab reduce` of `[CLAUDE_PLANT[0], CLAUDE_PLANT[n]]`): at `cc1e1d84ea`,
/// rows 1 to 4 (the input's, the result's and the attachment's keys as
/// written, the stamp's tail, and the attachment's word); at `eac866795c`
/// (`8dac84738f`'s rule), rows 6 to 10 (the `api-request-shape`'s tool
/// argument, the nested result's key, the `mcpMeta` key, the server tool's
/// input and result keys, and the tracked file's name, each as written);
/// by `8b5f123bf1`'s rule, rows 11 to 13 (the `tool_addition`'s argument
/// name and definition name, the call's real id keying both maps, its
/// argument name and its `mode` word in `wireToolInputs`, and the
/// `parameters` schema's argument name, each as written).
const CLAUDE_PLANT_KEPT: [(usize, &str); 12] = [
    (
        1,
        r#"{"type":"assistant","timestamp":"2000-01-01T10:00:01.000Z","uuid":"00000000-0000-4000-8000-000000000002","message":{"id":"msg_r0003","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_r0004","name":"mcp__tool1","input":{"quokkaledgerInputKey":"","customers":{"quokkaledgerCustomer":{"status":""}}}}]}}"#,
    ),
    (
        2,
        r#"{"type":"user","timestamp":"2000-01-01T10:00:02.000Z","uuid":"00000000-0000-4000-8000-000000000002","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_r0003","content":""}]},"toolUseResult":{"answers":{"quokkaledgerAnswer":""}}}"#,
    ),
    (
        3,
        r#"{"type":"system","timestamp":"2000-01-01T10:00:03.000Z quokkaledger tail","uuid":"00000000-0000-4000-8000-000000000002","content":""}"#,
    ),
    (
        4,
        r#"{"type":"attachment","timestamp":"2000-01-01T10:00:04.000Z","uuid":"00000000-0000-4000-8000-000000000002","attachment":{"type":"todo","mode":"quokkaledgerMode","items":{"quokkaledgerTodo":{"status":"open"}}}}"#,
    ),
    (
        6,
        r#"{"type":"api-request-shape","sessionId":"00000000-0000-4000-8000-000000000002","shapeHash":"","timestamp":"2000-01-01T10:00:06.000Z","version":"2.1.303","shape":{"system":[{"type":"text","text":""}],"tools":[{"name":"","description":"","input_schema":{"type":"object","properties":{"quokkaledgerAccountKey":{"description":"","type":"string"}},"required":[""]}}]}}"#,
    ),
    (
        7,
        r#"{"type":"progress","timestamp":"2000-01-01T10:00:07.000Z","uuid":"00000000-0000-4000-8000-000000000002","data":{"type":"agent_progress","message":{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_r0003","content":""}]},"toolUseResult":{"answers":{"quokkaledgerNestedAnswer":""}}}}}"#,
    ),
    (
        8,
        r#"{"type":"user","timestamp":"2000-01-01T10:00:08.000Z","uuid":"00000000-0000-4000-8000-000000000002","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_r0003","content":""}]},"mcpMeta":{"structuredContent":{"quokkaledgerStructKey":""}}}"#,
    ),
    (
        9,
        r#"{"type":"assistant","timestamp":"2000-01-01T10:00:09.000Z","uuid":"00000000-0000-4000-8000-000000000002","message":{"id":"msg_r0003","role":"assistant","model":"claude-opus-5-5","content":[{"type":"server_tool_use","id":"srvtoolu_r0004","name":"","input":{"quokkaledgerQuery":""}},{"type":"web_search_tool_result","tool_use_id":"srvtoolu_r0004","content":{"quokkaledgerResultKey":""}}]}}"#,
    ),
    (
        10,
        r#"{"type":"file-history-snapshot","messageId":"00000000-0000-4000-8000-000000000002","snapshot":{"messageId":"00000000-0000-4000-8000-000000000002","trackedFileBackups":{"quokkaledgerNotes":{"backupFileName":"","version":1,"backupTime":""}},"timestamp":"2000-01-01T10:00:10.000Z"},"isSnapshotUpdate":false}"#,
    ),
    (
        11,
        r#"{"type":"api-request-blob","sessionId":"00000000-0000-4000-8000-000000000002","hash":"","message":{"role":"system","content":[{"type":"tool_addition","tool":{"type":"tool_definition","definition":{"name":"","description":"","input_schema":{"type":"object","properties":{"quokkaledgerBlobArg":{"type":"string","description":""}},"required":[""]}}}},{"type":"tool_addition","tool":{"type":"tool_definition","definition":{"name":"","description":"","input_schema":{"k1":"","k2":{"quokkaledgerDefName":{"type":"object","properties":{"quokkaledgerDefArg":{"type":"string"}}}}}}}},{"type":"tool_removal","tool":{"type":"tool_reference","name":""}}]}}"#,
    ),
    (
        12,
        r#"{"type":"assistant","timestamp":"2000-01-01T10:00:12.000Z","uuid":"00000000-0000-4000-8000-000000000002","wireIngestContext":{"toolu_01quokkaledgerCallId":{"cwd":""}},"message":{"id":"msg_r0003","role":"assistant","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"toolu_r0004","name":"mcp__tool1","input":{"k1":"","k2":""}}]},"wireToolInputs":{"toolu_01quokkaledgerCallId":{"quokkaledgerWireKey":"","mode":"quokkaledgerModeWord"}}}"#,
    ),
    (
        13,
        r#"{"type":"tool-list","timestamp":"2000-01-01T10:00:13.000Z","uuid":"00000000-0000-4000-8000-000000000002","tools":[{"name":"","parameters":{"type":"object","properties":{"quokkaledgerParamArg":{"type":"string"}}}}]}"#,
    ),
];

/// T7: a key a tool or a person named inside a free subtree
/// ([`opens_free`]: a tool's input, a result's output at any depth, an
/// `api-request-shape`'s `shape`, a schema wherever it sits, an
/// attachment's field, a tracked file's name, what a call's id keys) is
/// renamed `k<n>` (a call's id remapped as that id), every string there is
/// replaced,
/// and a string under a time key that is no whole stamp is replaced whole,
/// so the planted words survive nowhere and the text check passes; a whole
/// stamp is still rebased, and an attachment's `type` and a queued prompt's
/// `origin` are kept. NEGATIVE CONTROL: each planted row as an earlier rule
/// reduced it (its keys as written, the stamp's tail and the attachment's
/// word kept) fails the text check, naming the words.
#[test]
fn a_reduction_keeps_no_key_or_stamp_tail_anyone_wrote() {
    let src: String = CLAUDE_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let r = reduce(&src, Program::Claude).expect("reduced");
    check_no_text("plant", &src, &r, Program::Claude).expect("no text");
    assert!(!r.text.contains("quokka"), "{}", r.text);
    let rows: Vec<&str> = r.text.lines().collect();
    assert!(
        rows[2].contains(r#""input":{"k1":"","k2":{"k3":{"k4":""}}}"#),
        "{}",
        rows[2]
    );
    assert!(
        rows[3].contains(r#""toolUseResult":{"k5":{"k6":""}}"#),
        "{}",
        rows[3]
    );
    assert!(
        rows[1].contains(r#""timestamp":"2000-01-01T10:00:00.000Z""#)
            && rows[4].contains(r#""timestamp":"""#),
        "{}",
        r.text
    );
    // One key, one name across the file: the attachment's `status` is the
    // input's `k4`.
    assert!(
        rows[5].contains(r#""attachment":{"type":"todo","mode":"","items":{"k7":{"k4":""}}}"#),
        "{}",
        rows[5]
    );
    assert!(
        rows[6].contains(
            r#""attachment":{"type":"queued_command","prompt":"","origin":{"kind":"human"}}"#
        ),
        "{}",
        rows[6]
    );
    // The tool list Claude Code sends, the shape of its request: every key
    // renamed, its `type` among them.
    assert!(
        rows[7].contains(
            r#""shape":{"k8":[{"k9":"","k10":""}],"k11":[{"k12":"","k13":"","k14":{"k9":"","k15":{"k16":{"k13":"","k9":""}},"k17":[""]}}]}"#
        ),
        "{}",
        rows[7]
    );
    // A result in a row a `progress` row nests: `answers` is row 3's `k5`.
    assert!(
        rows[8].contains(r#""toolUseResult":{"k5":{"k18":""}}"#),
        "{}",
        rows[8]
    );
    assert!(
        rows[9].contains(r#""mcpMeta":{"k19":{"k20":""}}"#),
        "{}",
        rows[9]
    );
    assert!(
        rows[10].contains(r#""input":{"k21":""}"#) && rows[10].contains(r#""content":{"k22":""}"#),
        "{}",
        rows[10]
    );
    assert!(
        rows[11].contains(r#""trackedFileBackups":{"k23":{"k24":"","k25":1,"k26":""}},"timestamp":"2000-01-01T10:00:10.000Z""#),
        "{}",
        rows[11]
    );
    // A deferred tool's definition in a request's `tool_addition`: its
    // schema renamed whole, as the `api-request-shape`'s is (`type`,
    // `properties`, `description`, `required` are its `k9`, `k15`, `k13`,
    // `k17`); and so is a schema whose top names a definition (`$ref`,
    // `$defs` and the definition's name are `k28`, `k29`, `k30`).
    assert!(
        rows[12].contains(r#""definition":{"name":"","description":"","input_schema":{"k9":"","k15":{"k27":{"k9":"","k13":""}},"k17":[""]}}"#)
            && rows[12].contains(r#""input_schema":{"k28":"","k29":{"k30":{"k9":"","k15":{"k31":{"k9":""}}}}}"#),
        "{}",
        rows[12]
    );
    // A call's id keying both maps, before the call and after it: the
    // call's own remapped id, and under it the call's input renamed as the
    // input is.
    assert!(
        rows[13].contains(r#""wireIngestContext":{"toolu_r0019":{"k32":""}}"#)
            && rows[13]
                .contains(r#""id":"toolu_r0019","name":"mcp__tool1","input":{"k33":"","k34":""}"#)
            && rows[13].contains(r#""wireToolInputs":{"toolu_r0019":{"k33":"","k34":""}}"#),
        "{}",
        rows[13]
    );
    // A schema at a place no name rule names: free whole by its shape.
    assert!(
        rows[14]
            .contains(r#""tools":[{"name":"","parameters":{"k9":"","k15":{"k35":{"k9":""}}}}]"#),
        "{}",
        rows[14]
    );
    for (n, kept) in CLAUDE_PLANT_KEPT {
        let src = format!("{}\n{}\n", CLAUDE_PLANT[0], CLAUDE_PLANT[n]);
        let r = reduce(&src, Program::Claude).expect("reduced");
        let mut lines = r.text.lines();
        let (head, prompt) = (lines.next().expect("head"), lines.next().expect("prompt"));
        let earlier = Reduction {
            text: format!("{head}\n{prompt}\n{kept}\n"),
            ..r.clone()
        };
        assert!(kept.contains("quokkaledger"), "{n}: {kept}");
        let e = check_no_text("plant", &src, &earlier, Program::Claude)
            .expect_err("the earlier rule's reduction keeps the planted words");
        // It names a planted word, or the first key the earlier rule kept as
        // written in what is now a free subtree (the tool list's
        // `description`, `mcpMeta`'s `structuredContent`, a schema's
        // `properties`, `wireIngestContext`'s `cwd`); on the
        // `api-request-shape`, `tool_addition` and `wireToolInputs` rows the
        // argument check names the planted one
        // (`a_tool_argument_name_is_never_kept_as_written`).
        assert!(
            e.contains("quokkale")
                || e.contains("okkaledg")
                || e.contains("a key in a free subtree")
                || e.contains("is kept as written"),
            "{n}: {e}"
        );
    }
}

/// The committed 2.1.303 reduction (5H-2b), made by an earlier rule.
const STEPS_2_1_303: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-agent/src/harness/testdata/steps/2.1.303.jsonl"
);

/// A file's rows.
fn rows_of(text: &str) -> Vec<Node> {
    text.lines()
        .filter(|l| is_row(l))
        .map(|l| parse(l.trim()).expect("a row"))
        .collect()
}

/// T7, THE ARGUMENT CHECK, which finds a tool's arguments by their shape
/// and not by the reducer's rule: the committed 2.1.303 reduction, made
/// before `api-request-shape` was a free subtree, keeps the argument names
/// of the tools that row lists, and the check names them there (and Bash's
/// `command` in its calls); reduced again by [`P11`] it keeps none, and
/// passes the whole text check. NEGATIVE CONTROLS, each apart from
/// [`opens_free`]: the planted `api-request-shape`, `tool_addition` (its
/// argument name, and with that renamed, its `$defs` name), `wireToolInputs`
/// and `parameters` rows as an earlier rule reduced them fail the argument
/// check; that `wireToolInputs` row with its keys renamed but its `mode`
/// word kept fails it too; and the call's real id kept as the maps' key
/// fails the id check alone.
#[test]
fn a_tool_argument_name_is_never_kept_as_written() {
    let committed = std::fs::read_to_string(STEPS_2_1_303).expect("the committed reduction");
    let src = rows_of(&committed);
    let mut calls = HashSet::new();
    for row in &src {
        call_ids(row, &mut calls);
    }
    let keys_only = |_: &str, _: &str| Ok(());
    let args = Args {
        calls: &calls,
        check: &keys_only,
    };
    let kept = |r: &Node| check_tool_arguments(r, r, Arg::ROW, &args).err();
    let shape = src
        .iter()
        .find(|r| r.get("type").and_then(Node::as_str) == Some("api-request-shape"))
        .expect("its api-request-shape row");
    let e = kept(shape).expect("its tool list keeps its argument names");
    assert!(e.contains(r#""description""#), "{e}");
    let calls: Vec<String> = src.iter().filter_map(kept).collect();
    assert!(
        calls.iter().any(|e| e.contains(r#""command""#)),
        "{calls:?}"
    );
    let again = reduce(&committed, Program::Claude).expect("reduced");
    let red = rows_of(&again.text);
    assert_eq!(src.len(), red.len());
    for (a, b) in src.iter().zip(&red) {
        check_tool_arguments(a, b, Arg::ROW, &args).expect("every argument name renamed");
    }
    check_no_text("2.1.303", &committed, &again, Program::Claude).expect("no text");
    // Planted row `n` and an earlier rule's reduction of it, `edit` (a
    // `from` written as `to`) made to it.
    let earlier = |n: usize, edit: Option<(&str, &str)>| {
        let (_, kept) = CLAUDE_PLANT_KEPT
            .iter()
            .find(|(m, _)| *m == n)
            .expect("an earlier reduction");
        let kept = edit.map_or_else(|| (*kept).to_string(), |(f, t)| kept.replace(f, t));
        (
            parse(CLAUDE_PLANT[n]).expect("a row"),
            parse(&kept).expect("a row"),
        )
    };
    // The `wireIngestContext` before the call is read as the call's too;
    // with its key renamed, the check names the argument in
    // `wireToolInputs`.
    let cwd = (r#""cwd":"""#, r#""k5":"""#);
    for (n, word, edit) in [
        (6, "quokkaledgerAccountKey", None),
        (11, "quokkaledgerBlobArg", None),
        (
            11,
            "quokkaledgerDefName",
            Some((r#""quokkaledgerBlobArg":{"#, r#""k27":{"#)),
        ),
        (12, "cwd", None),
        (12, "quokkaledgerWireKey", Some(cwd)),
        (13, "quokkaledgerParamArg", None),
    ] {
        let (a, b) = earlier(n, edit);
        let mut calls = HashSet::new();
        call_ids(&a, &mut calls);
        let args = Args {
            calls: &calls,
            check: &keys_only,
        };
        let e = check_tool_arguments(&a, &b, Arg::ROW, &args)
            .expect_err("the earlier rule kept the MCP tool's argument name");
        assert!(e.contains(&format!("{word:?}")), "{n}: {e}");
    }
    // The `mode` word an earlier rule kept in `wireToolInputs`, with every
    // key there renamed: the argument check still names it.
    let src = format!("{}\n{}\n", CLAUDE_PLANT[0], CLAUDE_PLANT[12]);
    let r = reduce(&src, Program::Claude).expect("reduced");
    let (_, kept) = CLAUDE_PLANT_KEPT[10];
    let renamed_but_word = kept
        .replace(cwd.0, cwd.1)
        .replace(r#""quokkaledgerWireKey":"""#, r#""k1":"""#)
        .replace(
            r#""mode":"quokkaledgerModeWord""#,
            r#""k2":"quokkaledgerModeWord""#,
        );
    assert!(
        !renamed_but_word.contains("Key") && renamed_but_word.contains("ModeWord"),
        "{renamed_but_word}"
    );
    let mut lines = r.text.lines();
    let (head, prompt) = (lines.next().expect("head"), lines.next().expect("prompt"));
    let word = Reduction {
        text: format!("{head}\n{prompt}\n{renamed_but_word}\n"),
        ..r.clone()
    };
    let e = check_no_text("plant", &src, &word, Program::Claude)
        .expect_err("the argument check reads the word");
    assert!(
        e.contains("a tool's argument keeps") && e.contains("quokkale"),
        "{e}"
    );
    // The call's real id kept as both maps' key, all under it renamed and
    // emptied: the id check names it, apart from the argument check.
    let renamed_and_empty = renamed_but_word.replace("quokkaledgerModeWord", "");
    let id = Reduction {
        text: format!("{head}\n{prompt}\n{renamed_and_empty}\n"),
        ..r.clone()
    };
    let e = check_no_text("plant", &src, &id, Program::Claude).expect_err("the id kept as a key");
    assert!(
        e.contains(
            r#""toolu_01quokkaledgerCallId" is remapped as a value but kept as written as a key"#
        ),
        "{e}"
    );
}

/// A Codex rollout as its readers would commit it: its meta, a command that
/// read a file (`parsed_cmd`'s `read` names the file in `name`), a call by
/// its namespace and name, and a turn's end.
const CODEX_PLANT: [&str; 4] = [
    r#"{"timestamp":"2026-10-16T10:00:00.000Z","type":"session_meta","payload":{"id":"0199a0b1-0000-7000-8000-000000000abc","cwd":"/private/tmp/x/proj","cli_version":"0.163.0"}}"#,
    r#"{"timestamp":"2026-10-16T10:00:01.000Z","type":"event_msg","payload":{"type":"exec_command_begin","call_id":"call_abc","command":["bash","-lc","cat quokkaledger-plan.md"],"parsed_cmd":[{"type":"read","cmd":"cat quokkaledger-plan.md","name":"quokkaledger-plan.md","path":"/private/tmp/x/quokkaledger-plan.md"}]}}"#,
    r#"{"timestamp":"2026-10-16T10:00:02.000Z","type":"response_item","payload":{"type":"function_call","namespace":"collaboration","name":"spawn_agent","arguments":"{\"message\":\"say hi\"}","call_id":"call_x"}}"#,
    r#"{"timestamp":"2026-10-16T10:00:03.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","started_at_ms":1792144800123}}"#,
];

/// A reducer that keeps any Codex `name` as a tool's (the provisional
/// rule's): wherever it sits.
struct CodexNamesAnywhere;

impl Rules for CodexNamesAnywhere {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        if at.program == Program::Codex && !at.free && at.key == "name" {
            Treat::Tool
        } else {
            P11.string(at, s)
        }
    }
}

/// T7 over a Codex rollout: no 8-byte run of its text survives — a call's
/// own name and namespace are kept, a file a command read is not, though
/// Codex files it under `name`. NEGATIVE CONTROL: a reducer that keeps any
/// `name` as a tool's keeps the file's name, and fails the text check.
#[test]
fn a_codex_reduction_keeps_no_text() {
    let src: String = CODEX_PLANT.iter().map(|l| format!("{l}\n")).collect();
    let r = reduce(&src, Program::Codex).expect("reduced");
    check_no_text("codex", &src, &r, Program::Codex).expect("no text");
    assert!(!r.text.contains("quokka"), "{}", r.text);
    let rows: Vec<&str> = r.text.lines().collect();
    assert!(
        rows[2].contains(r#""parsed_cmd":[{"type":"read","cmd":"","name":"","path":""}]"#),
        "{}",
        rows[2]
    );
    assert!(
        rows[3].contains(r#""namespace":"collaboration","name":"spawn_agent","arguments":"""#),
        "{}",
        rows[3]
    );
    let kept = reduce_with(&src, Program::Codex, &CodexNamesAnywhere).expect("reduced");
    let e = check_no_text("codex", &src, &kept, Program::Codex)
        .expect_err("a file's name kept as a tool's");
    assert!(e.contains("quokkale") || e.contains("okkaledg"), "{e}");
}

/// A reducer that keeps every `content` and `text` string as written.
struct KeepsContent;

impl Rules for KeepsContent {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        if matches!(at.key, "content" | "text") {
            Treat::Keep
        } else {
            P11.string(at, s)
        }
    }
}

/// A reducer that replaces every `type`.
struct ReplacesType;

impl Rules for ReplacesType {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        if at.key == "type" {
            Treat::Replace
        } else {
            P11.string(at, s)
        }
    }
}

/// A reducer that blanks a notice's text.
struct BlanksNotices;

impl Rules for BlanksNotices {
    fn string(&self, at: &At<'_>, s: &str) -> Treat {
        match P11.string(at, s) {
            Treat::Notice => Treat::Replace,
            t => t,
        }
    }
}

/// NEGATIVE CONTROL: a reducer that keeps content fails the text check.
#[test]
fn a_reducer_that_keeps_content_fails_the_text_check() {
    let bad = failures(&KeepsContent, &no_text);
    assert!(bad.len() >= 20, "{} of 24 failed: {bad:#?}", bad.len());
    assert!(
        failures(&KeepsContent, &folds).is_empty(),
        "it still folds as its source"
    );
}

/// NEGATIVE CONTROL: a reducer that replaces `type` fails the fold check.
#[test]
fn a_reducer_that_replaces_type_fails_the_fold_check() {
    let bad = failures(&ReplacesType, &folds);
    assert!(bad.len() >= 20, "{} of 24 failed: {bad:#?}", bad.len());
}

/// NEGATIVE CONTROL (T7): a reducer that blanks a notice's text fails the
/// fold check on every fixture that holds a notice (its class, or its wall).
#[test]
fn a_reducer_that_blanks_notice_text_fails_the_wall_check() {
    let bad = failures(&BlanksNotices, &folds);
    let with = fixtures()
        .iter()
        .filter(|(_, t)| t.contains("\"isApiErrorMessage\":true"))
        .count();
    assert!(
        with >= 15 && bad.len() == with,
        "{} of {with}: {bad:#?}",
        bad.len()
    );
}

/// Every canonical phrase reads as a wall (or as the notice that is none, or
/// as nothing) no earlier phrase reads as: the table holds no dead row.
#[test]
fn every_canonical_phrase_is_the_first_of_its_reading() {
    let ps = phrases();
    for (i, p) in ps.iter().enumerate() {
        let r = Reading::of(p);
        assert!(
            ps[..i].iter().all(|q| Reading::of(q) != r),
            "{p:?} reads as an earlier phrase ({r:?})"
        );
        assert_eq!(
            canonical(p, 0, 0, 2026).as_deref(),
            Ok(*p),
            "{p:?} is its own phrase"
        );
    }
    // A status is kept: `API Error: 503 …` reads as `API Error: 503`.
    assert_eq!(
        canonical("API Error: 503 upstream connect error", 0, 0, 2026).as_deref(),
        Ok("API Error: 503")
    );
}

/// A reset clause keeps its time of day, moves its date by whole days and
/// drops its zone; a clause of another form is no clause.
#[test]
fn a_reset_clause_is_rebased_by_whole_days() {
    let shift = days_from_civil(2000, 1, 1) - days_from_civil(2026, 10, 8);
    assert_eq!(
        reset_clause(
            "You've hit your session limit · resets 11:33am (America/Los_Angeles)",
            shift,
            2026
        )
        .as_deref(),
        Some(" · resets 11:33am")
    );
    assert_eq!(
        reset_clause(
            "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
            shift,
            2026
        )
        .as_deref(),
        Some(" · resets Jan 4 at 9:35am")
    );
    assert_eq!(reset_clause("resets soon", shift, 2026), None);
    assert_eq!(
        canonical(
            "You've hit your weekly limit · resets Oct 11 at 9:35am (America/Los_Angeles)",
            0,
            shift,
            2026
        )
        .as_deref(),
        Ok("You've hit your weekly limit · resets Jan 4 at 9:35am")
    );
}

/// The ordered tree reads and writes JSON as written: key order, numbers as
/// spelled, escapes as `JSON.stringify` writes them, a surrogate pair as one
/// character; and refuses what is no JSON.
#[test]
fn the_tree_keeps_order_numbers_and_escapes() {
    let src = r#"{"z":1,"a":[1.50,-0,2e3,{"q":null,"b":true}],"s":"a\"b\\c\n\t\u0001😀é/"}"#;
    let v = parse(src).expect("json");
    assert_eq!(
        v.dump(),
        "{\"z\":1,\"a\":[1.50,-0,2e3,{\"q\":null,\"b\":true}],\"s\":\"a\\\"b\\\\c\\n\\t\\u0001\u{1f600}é/\"}"
    );
    for bad in [
        "{",
        "[1,]",
        "{\"a\" 1}",
        "01",
        "\"x",
        "1 2",
        "{\"a\":\"\u{1}\"}",
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

/// Ids are remapped one to one, a valid `StepId` to a valid one and an
/// invalid one to an invalid one, a UUID to a UUID, a prefix kept.
#[test]
fn ids_are_remapped_one_to_one_their_validity_kept() {
    let src = concat!(
        r#"{"type":"assistant","uuid":"a1b2c3d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d","timestamp":"2026-10-08T16:34:58.921Z","message":{"id":"msg_01abc","content":[{"type":"tool_use","id":"toolu_lab1","name":"mcp__notes__find","input":{"q":"secretword"}},{"type":"tool_use","id":"an id with spaces","name":"Bash","input":{}}]}}"#,
        "\n",
        r#"{"type":"user","parentUuid":"a1b2c3d4-5e6f-4a7b-8c9d-0e1f2a3b4c5d","timestamp":"2026-10-08T16:35:00.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_lab1","content":"secretword found"}]}}"#,
        "\n"
    );
    let r = reduce(src, Program::Claude).expect("reduced");
    let rows: Vec<&str> = r.text.lines().collect();
    assert_eq!(rows.len(), 3, "{}", r.text);
    assert!(
        rows[0].starts_with("# claude - · lab reduce"),
        "{}",
        rows[0]
    );
    assert_eq!(
        rows[1],
        r#"{"type":"assistant","uuid":"00000000-0000-4000-8000-000000000001","timestamp":"2000-01-01T16:34:58.921Z","message":{"id":"msg_r0002","content":[{"type":"tool_use","id":"toolu_r0003","name":"mcp__tool1","input":{"k1":""}},{"type":"tool_use","id":"r0004/","name":"Bash","input":{}}]}}"#
    );
    assert_eq!(
        rows[2],
        r#"{"type":"user","parentUuid":"00000000-0000-4000-8000-000000000001","timestamp":"2000-01-01T16:35:00.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_r0003","content":""}]}}"#
    );
    assert!(!r.text.contains("secretword"));
    assert_eq!(
        (r.rows, r.ids, r.shift_days),
        (
            2,
            4,
            days_from_civil(2000, 1, 1) - days_from_civil(2026, 10, 8)
        )
    );
}

/// A user row keeps the marker it opens with and nothing past it; a local
/// command's output that reads as a full context keeps its canonical phrase
/// in its tags; a key that is no identifier is renamed.
#[test]
fn a_user_row_keeps_its_marker_and_nothing_past_it() {
    let row = |content: &str| {
        let src = format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":{}}}}}\n",
            Node::Str(content.to_string()).dump()
        );
        let r = reduce(&src, Program::Claude).expect("reduced");
        let line = r.text.lines().nth(1).expect("a row").to_string();
        let v = parse(&line).expect("json");
        v.get("message")
            .and_then(|m| m.get("content"))
            .and_then(Node::as_str)
            .expect("content")
            .to_string()
    };
    assert_eq!(
        row("<task-notification>\n<task-id>b1</task-id>"),
        "<task-notification>"
    );
    assert_eq!(
        row("<bash-input>cat notes.txt</bash-input>"),
        "<bash-input>"
    );
    assert_eq!(
        row("[Request interrupted by user for tool use]"),
        "[Request interrupted by user"
    );
    assert_eq!(row("/compact keep the notes"), "/compact");
    assert_eq!(row("/compactor"), "");
    assert_eq!(row("seventh try"), "");
    let sys = "{\"type\":\"system\",\"subtype\":\"local_command\",\"content\":\"<local-command-stderr>Compaction failed · conversation could not be reduced below the context limit</local-command-stderr>\",\"files\":{\"/private/tmp/x/notes.txt\":1}}\n";
    let r = reduce(sys, Program::Claude).expect("reduced");
    assert_eq!(
        r.text.lines().nth(1),
        Some(
            "{\"type\":\"system\",\"subtype\":\"local_command\",\"content\":\"<local-command-stderr>Context limit reached</local-command-stderr>\",\"files\":{\"k1\":1}}"
        )
    );
}

/// A Codex rollout keeps its closed words, ids one to one and numbers, its
/// reset epoch rebased with its stamps, and none of its text.
#[test]
fn a_codex_rollout_keeps_words_ids_and_numbers_only() {
    let src = concat!(
        r#"{"timestamp":"2026-10-16T10:00:00.000Z","type":"session_meta","payload":{"id":"0199a0b1-0000-7000-8000-000000000abc","cwd":"/private/tmp/x/proj","cli_version":"0.163.0","instructions":"be brief please"}}"#,
        "\n",
        r#"{"timestamp":"2026-10-16T10:00:01.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"touch zebrafile\"}","call_id":"call_abc"}}"#,
        "\n",
        r#"{"timestamp":"2026-10-16T10:00:02.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":120}},"rate_limits":{"primary":{"used_percent":40.0,"resets_at":1792144800}}}}"#,
        "\n"
    );
    let r = reduce(src, Program::Codex).expect("reduced");
    let shift = days_from_civil(2000, 1, 1) - days_from_civil(2026, 10, 16);
    let rows: Vec<&str> = r.text.lines().collect();
    assert!(
        rows[0].starts_with("# codex 0.163.0 · lab reduce"),
        "{}",
        rows[0]
    );
    assert_eq!(
        rows[1],
        r#"{"timestamp":"2000-01-01T10:00:00.000Z","type":"session_meta","payload":{"id":"00000000-0000-4000-8000-000000000001","cwd":"","cli_version":"0.163.0","instructions":""}}"#
    );
    assert_eq!(
        rows[2],
        r#"{"timestamp":"2000-01-01T10:00:01.000Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"","call_id":"call_r0002"}}"#
    );
    assert_eq!(
        rows[3],
        format!(
            r#"{{"timestamp":"2000-01-01T10:00:02.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":120}}}},"rate_limits":{{"primary":{{"used_percent":40.0,"resets_at":{}}}}}}}}}"#,
            1_792_144_800 + shift * 86_400
        )
    );
    for word in ["zebrafile", "be brief", "/private/tmp/x"] {
        assert!(!r.text.contains(word), "{word}");
    }
}

/// THE LAB'S CODEX 0.163.0 ROLLOUTS (session log 5G): a call's `namespace`
/// and an abort's `reason` are closed words kept; the millisecond stamps
/// (`started_at_ms`) and an item's fractional `create_time` move with the
/// stamps; a call's free-form `arguments` and a message's `content` go.
/// NEGATIVE CONTROL: the source's own epochs appear nowhere in the
/// reduction.
#[test]
fn a_codex_rollout_keeps_its_namespace_and_moves_its_millisecond_times() {
    let src = concat!(
        r#"{"timestamp":"2026-10-19T12:00:00.010Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","arguments":"{\"message\":\"say hi pelicanhinge\"}","call_id":"call_lab0004"}}"#,
        "\n",
        r#"{"timestamp":"2026-10-19T12:00:00.020Z","type":"event_msg","payload":{"type":"item_completed","started_at_ms":1792411200015,"item":{"type":"AgentMessage","content":[{"type":"Text","text":"hello pelicanhinge"}]}}}"#,
        "\n",
        r#"{"timestamp":"2026-10-19T12:00:00.030Z","type":"compacted","payload":{"replacement_history":[{"type":"message","role":"user","internal_chat_message_metadata_passthrough":{"create_time":1792411200.025125}}]}}"#,
        "\n",
        r#"{"timestamp":"2026-10-19T12:00:01.000Z","type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted","duration_ms":4311}}"#,
        "\n"
    );
    let r = reduce(src, Program::Codex).expect("reduced");
    let day_s = (days_from_civil(2000, 1, 1) - days_from_civil(2026, 10, 19)) * 86_400;
    let rows: Vec<&str> = r.text.lines().collect();
    assert_eq!(
        rows[1],
        r#"{"timestamp":"2000-01-01T12:00:00.010Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","arguments":"","call_id":"call_r0001"}}"#
    );
    assert_eq!(
        rows[2],
        format!(
            r#"{{"timestamp":"2000-01-01T12:00:00.020Z","type":"event_msg","payload":{{"type":"item_completed","started_at_ms":{},"item":{{"type":"AgentMessage","content":[{{"type":"Text","text":""}}]}}}}}}"#,
            1_792_411_200_015 + day_s * 1000
        )
    );
    assert_eq!(
        rows[3],
        format!(
            r#"{{"timestamp":"2000-01-01T12:00:00.030Z","type":"compacted","payload":{{"replacement_history":[{{"type":"message","role":"user","internal_chat_message_metadata_passthrough":{{"create_time":{}.025125}}}}]}}}}"#,
            1_792_411_200 + day_s
        )
    );
    assert_eq!(
        rows[4],
        r#"{"timestamp":"2000-01-01T12:00:01.000Z","type":"event_msg","payload":{"type":"turn_aborted","reason":"interrupted","duration_ms":4311}}"#
    );
    // NEGATIVE CONTROL: no epoch of the source's day survives.
    for epoch in ["1792411200", "pelicanhinge"] {
        assert!(!r.text.contains(epoch), "{epoch}");
    }
}

/// A line that is no JSON refuses the file; nothing is written for it.
#[test]
fn a_line_that_is_no_json_refuses_the_file() {
    let e = reduce("{\"type\":\"user\"}\n{\"type\":", Program::Claude).expect_err("refused");
    assert!(e.starts_with("line 2: not one JSON value"), "{e}");
}

/// The verb never writes over a file: `--out` an existing path is refused
/// and the file is left as it was.
#[test]
fn the_verb_never_writes_over_a_file() {
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let src = dir.path().join("t.jsonl");
    let out = dir.path().join("r.jsonl");
    std::fs::write(
        &src,
        "{\"type\":\"user\",\"message\":{\"content\":\"hello there friend\"}}\n",
    )
    .expect("source");
    std::fs::write(&out, "kept\n").expect("an existing file");
    let args = |o: &std::path::Path| {
        vec![
            src.to_string_lossy().into_owned(),
            "--program".to_string(),
            "claude".to_string(),
            "--out".to_string(),
            o.to_string_lossy().into_owned(),
        ]
    };
    let e = cmd_reduce(&args(&out)).expect_err("refused");
    assert_eq!(e.code, 1);
    assert_eq!(std::fs::read_to_string(&out).expect("kept"), "kept\n");
    let fresh = dir.path().join("fresh.jsonl");
    assert_eq!(cmd_reduce(&args(&fresh)).expect("written"), 0);
    let text = std::fs::read_to_string(&fresh).expect("written");
    assert!(
        text.lines().nth(1) == Some("{\"type\":\"user\",\"message\":{\"content\":\"\"}}"),
        "{text}"
    );
    assert_eq!(cmd_reduce(&["x".to_string()]).expect_err("usage").code, 2);
}
