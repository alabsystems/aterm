// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PRIVACY ORACLE A REDUCTION IS HELD TO (ruling T7): [`check_no_text`] holds a reduction to
//! its source row by row, keys, ids and closed words aside, and refuses one that keeps any
//! eight-byte run of a string the source held (a prompt, a reply, a path, a tool's argument or
//! a schema's name). The reducer's tests hold every fixture and committed reduction to it, and
//! `lab claude-measure` holds a measured transcript's reduction to it against the transcript
//! itself.

use std::collections::HashSet;

use aterm_agent::harness::steps::{COMPACT, USER_MARKERS, tool_class};

use super::{
    CLAUDE_WORDS, CODEX_WORDS, Node, OneToOne, Program, Reduction, header, is_ident, is_key_name,
    is_row, is_schema, is_time_key, is_word, opens_free, parse, phrases, stamp_day,
};

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
    v.sort_by_key(|s| std::cmp::Reverse(s.len()));
    v
}

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

fn placeholder_id(s: &str) -> bool {
    let s = s.strip_suffix('/').unwrap_or(s);
    let count = |t: &str| t.len() >= 4 && t.bytes().all(|b| b.is_ascii_digit());
    s.strip_prefix("00000000-0000-4000-8000-")
        .is_some_and(|t| t.len() == 12 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        || s.strip_prefix('r').is_some_and(count)
        || s.split_once("_r")
            .is_some_and(|(p, t)| p.bytes().all(|b| b.is_ascii_lowercase()) && count(t))
}

/// The reduction `r` of `source` (by `program`'s rules) keeps no text of it: its own line is the
/// reducer's, it holds the source's rows one for one, every key is the vendor's or a renamed one,
/// every id is remapped one to one, and no string outside a closed word, a tool's name, a time
/// or an id holds an eight-byte run of any string (or `#` line) of the source; `name` names it
/// in the refusal.
/// # Errors
/// The first place it keeps text, or a row of either that is no JSON value.
pub(crate) fn check_no_text(
    name: &str,
    source: &str,
    r: &Reduction,
    program: Program,
) -> Result<(), String> {
    let src: Vec<Node> = source
        .lines()
        .filter(|l| is_row(l))
        .map(|l| parse(l.trim()).map_err(|e| format!("{name}: a source row: {e}")))
        .collect::<Result<_, _>>()?;
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
    let head = lines.next().unwrap_or_default();
    if head != header(program, r.build.as_deref(), r.moved_days, r.rows, r.again)
        || !r.build.as_deref().is_none_or(is_word)
    {
        return Err(format!("{name}: the header {head:?} is no reduction's own"));
    }
    let red: Vec<Node> = lines
        .map(|l| parse(l).map_err(|e| format!("{name}: a reduced row: {e}")))
        .collect::<Result<_, _>>()?;
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

fn renamed(k: &str) -> bool {
    is_key_name(k)
}

pub(super) fn call_ids<'a>(v: &'a Node, out: &mut HashSet<&'a str>) {
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

pub(super) struct Args<'a> {
    pub(super) calls: &'a HashSet<&'a str>,
    pub(super) check: &'a dyn Fn(&str, &str) -> Result<(), String>,
}

/// Where [`check_tool_arguments`] is in a row: `here`, its own keys are
/// names a schema's author wrote (a schema's `properties`, its `$defs`);
/// `below`, every key under it is a name a tool's caller wrote (a call's
/// input); `said`, every string under it is what either wrote.
#[derive(Clone, Copy)]
pub(super) struct Arg {
    here: bool,
    below: bool,
    said: bool,
}

impl Arg {
    pub(super) const ROW: Arg = Arg {
        here: false,
        below: false,
        said: false,
    };
}

pub(super) fn check_tool_arguments(
    a: &Node,
    b: &Node,
    at: Arg,
    args: &Args<'_>,
) -> Result<(), String> {
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
/// where whatever is there is what someone said or named.
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
            let free = at.free || is_schema(a);
            for ((ka, va), (kb, vb)) in x.iter().zip(y) {
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
