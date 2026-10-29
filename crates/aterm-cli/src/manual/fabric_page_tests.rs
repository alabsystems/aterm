// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm help fabric` held to the code it describes: the `mirror=` words to
//! [`MirrorState`], and THE SESSION LOG ON THE BUS to the session log's own
//! vocabulary and encoder (`aterm_slog`'s `Rec::is_bus_class` over `Op::ALL` and
//! `Kind::ALL`, the bodies `encode_mirror`, `cover_body` and a take's snapshot
//! write, and the take's snapshot cadence). Each test reads only the page's
//! vocabulary words and grammar lines, never its sentences.

use std::collections::{BTreeMap, BTreeSet};

use aterm_slog::field::Cap;
use aterm_slog::{
    Actor, Close, Conf, CoverWhy, Fields, Key, Kind, Log, Lvl, Node, Op, Open, Out, Rec, RunOut,
    SNAP_EVERY, ShortId, SlotKind, Src, Stamp, Tier, cover_body, decode_bus, encode, encode_mirror,
};
use aterm_types::fabric_grammar::{MirrorState, mirror_words_named};

use super::FABRIC_PAGE;

/// The page's THE SESSION LOG ON THE BUS: its heading through the line before the next
/// heading (a line after a blank one that starts with a capital).
fn bus_section(page: &str) -> &str {
    let start = page
        .find("\nTHE SESSION LOG ON THE BUS\n")
        .expect("the fabric page has THE SESSION LOG ON THE BUS")
        + 1;
    let rest = &page[start..];
    let end = rest
        .match_indices("\n\n")
        .map(|(at, _)| at)
        .find(|&at| rest[at + 2..].starts_with(|c: char| c.is_ascii_uppercase()))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The leading spaces of `line`.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Each row of `text` whose first line `starts` (trimmed), joined with the lines indented
/// deeper under it.
fn rows(text: &str, starts: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    (0..lines.len())
        .filter(|&i| lines[i].trim_start().starts_with(starts))
        .map(|i| {
            let deeper = lines[i + 1..]
                .iter()
                .take_while(|l| indent(l) > indent(lines[i]));
            std::iter::once(&lines[i])
                .chain(deeper)
                .map(|l| l.trim())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The backticked spans of `text`.
fn spans(text: &str) -> impl Iterator<Item = &str> {
    text.split('`').skip(1).step_by(2)
}

/// The key of a body's `key=value` token (the token itself when it has no `=`).
fn key_of(tok: &str) -> &str {
    tok.split_once('=').map_or(tok, |(k, _)| k)
}

/// The keys a body, or a log line, writes, in order.
fn body_keys(body: &str) -> Vec<&str> {
    body.split(' ')
        .filter(|tok| tok.contains('='))
        .map(key_of)
        .collect()
}

/// What one token of a page body line stands for.
#[derive(Clone, Copy, Debug)]
enum Slot<'a> {
    /// A token the body writes as it stands (`v=2`, `op=snap`, `by=aterm`).
    Lit(&'a str),
    /// A key and the placeholder its value fills (`seq=<n>`, `open=<node>:<kind>,...`).
    Key(&'a str, &'a str),
    /// The record's own fields (`<key>=<value> ...`): every token whose key is a [`Key`].
    Fields,
}

/// One token of a page body line: its text, whether brackets mark it optional, and what it
/// stands for.
#[derive(Clone, Copy, Debug)]
struct Gram<'a> {
    tok: &'a str,
    optional: bool,
    slot: Slot<'a>,
}

/// The tokens of a page body line, a space inside `<...>` or `[...]` kept (`t=<wall ms>`,
/// `[<key>=<value> ...]`).
fn tokens(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, None);
    for (i, c) in line.char_indices() {
        match c {
            '<' | '[' => depth += 1,
            '>' | ']' => depth -= 1,
            _ => {}
        }
        if c == ' ' && depth == 0 {
            if let Some(s) = start.take() {
                out.push(&line[s..i]);
            }
        } else if start.is_none() && c != ' ' {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&line[s..]);
    }
    out
}

/// A page body line as the slots a body fills, in order.
fn grammar(line: &str) -> Vec<Gram<'_>> {
    tokens(line)
        .into_iter()
        .map(|tok| {
            let (inner, optional) = match tok.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
                Some(inner) => (inner, true),
                None => (tok, false),
            };
            let slot = if inner.starts_with("<key>=") {
                Slot::Fields
            } else {
                let (key, value) = inner
                    .split_once('=')
                    .unwrap_or_else(|| panic!("`{tok}` is no `key=`: {line}"));
                if value.starts_with('<') {
                    Slot::Key(key, value)
                } else {
                    Slot::Lit(inner)
                }
            };
            Gram {
                tok,
                optional,
                slot,
            }
        })
        .collect()
}

/// The keys a page body line names, in order (a placeholder field names none).
fn page_keys(line: &str) -> Vec<&str> {
    grammar(line)
        .into_iter()
        .filter_map(|g| match g.slot {
            Slot::Lit(tok) => Some(key_of(tok)),
            Slot::Key(key, _) => Some(key),
            Slot::Fields => None,
        })
        .collect()
}

/// The words a page body line's `<key>=<a|b|c>` placeholder offers.
fn offered<'a>(line: &'a str, key: &str) -> Vec<&'a str> {
    let ph = grammar(line)
        .into_iter()
        .find_map(|g| match g.slot {
            Slot::Key(k, ph) if k == key => Some(ph),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the line offers `{key}=<...>`: {line}"));
    ph.trim_start_matches('<')
        .trim_end_matches('>')
        .split('|')
        .collect()
}

/// Whether `v` is a value of the field `key` by the key's own cap ([`Key::cap`]).
fn field_ok(key: Key, v: &str) -> bool {
    match key.cap() {
        Cap::Word(words) => words.contains(&v),
        Cap::U => v.parse::<u64>().is_ok(),
        Cap::I => v.parse::<i64>().is_ok(),
        Cap::Id => ShortId::new(v).is_some(),
        Cap::Tok(..) | Cap::Text(_) => !v.is_empty(),
    }
}

/// Whether `v` is what the body writes after `key=`: a header key by the producer's reader of
/// it, a field by its key's cap.
fn key_ok(key: &str, v: &str) -> bool {
    match key {
        "t" | "ep" | "seq" | "ms" => v.parse::<u64>().is_ok(),
        "op" => Op::from_word(v).is_some(),
        "node" | "par" => Node::parse(v).is_some(),
        "k" => Kind::from_word(v).word() == v,
        "by" => Actor::from_word(v).word() == v,
        "src" => Src::from_word(v).word() == v,
        "conf" => Conf::from_word(v).word() == v,
        _ => field_ok(
            Key::from_word(key).unwrap_or_else(|| panic!("`{key}=` is no key of a body")),
            v,
        ),
    }
}

/// Whether `v` is the part of a list item a placeholder names (`<node>`, `<kind>`, `<need>`,
/// `<by>`, `<src>`), by the producer's reader of it.
fn part_ok(name: &str, v: &str) -> bool {
    match name {
        "node" => Node::parse(v).is_some(),
        "kind" => Kind::from_word(v).word() == v,
        "need" => field_ok(Key::Need, v),
        "by" => Actor::from_word(v).word() == v,
        "src" => Src::from_word(v).word() == v,
        other => panic!("no reader for `<{other}>`"),
    }
}

/// Whether `value` is what `pattern` says: each item (a list when the pattern ends `,...`)
/// its placeholders' parts, joined by the pattern's separators, in the pattern's order.
fn items_ok(pattern: &str, value: &str) -> bool {
    let (item, list) = match pattern.strip_suffix(",...") {
        Some(item) => (item, true),
        None => (pattern, false),
    };
    let mut parts: Vec<(&str, &str)> = Vec::new();
    let mut rest = item;
    while let Some(after) = rest.strip_prefix('<') {
        let (name, tail) = after
            .split_once('>')
            .unwrap_or_else(|| panic!("an unclosed `<` in {pattern}"));
        let sep_end = tail.find('<').unwrap_or(tail.len());
        parts.push((name, &tail[..sep_end]));
        rest = &tail[sep_end..];
    }
    assert!(
        rest.is_empty() && !parts.is_empty(),
        "no item pattern: {pattern}"
    );
    let items: Vec<&str> = if list {
        value.split(',').collect()
    } else {
        vec![value]
    };
    items.into_iter().all(|it| {
        let mut rest = it;
        parts.iter().all(|&(name, sep)| {
            let part = if sep.is_empty() {
                std::mem::take(&mut rest)
            } else {
                match rest.split_once(sep) {
                    Some((part, tail)) => {
                        rest = tail;
                        part
                    }
                    None => return false,
                }
            };
            part_ok(name, part)
        }) && rest.is_empty()
    })
}

/// Whether `v` fills the placeholder `ph` of `key`: one of its words when it offers some
/// (`<a|b>`), each item's parts when it is an item pattern (`<node>:<kind>,...`), and
/// otherwise a value of `key`.
fn fills(key: &str, ph: &str, v: &str) -> bool {
    let single = ph.starts_with('<') && ph.ends_with('>') && ph.matches('<').count() == 1;
    if !single {
        return items_ok(ph, v);
    }
    let inner = &ph[1..ph.len() - 1];
    if inner.contains('|') && !inner.split('|').any(|w| w == v) {
        return false;
    }
    key_ok(key, v)
}

/// `body` read through the page line's slots, in order: which slots it fills, or why it does
/// not fit (a slot the line does not bracket left out, a value no placeholder takes, a token
/// past the line's last slot).
fn align(slots: &[Gram<'_>], body: &str) -> Result<Vec<bool>, String> {
    let toks: Vec<&str> = body.split(' ').collect();
    let mut i = 0;
    let mut filled = Vec::with_capacity(slots.len());
    for g in slots {
        let here = toks.get(i).copied();
        let took = match g.slot {
            Slot::Lit(lit) => usize::from(here == Some(lit)),
            Slot::Key(key, ph) => match here {
                Some(tok) if key_of(tok) == key => {
                    let v = &tok[key.len() + 1..];
                    if !fills(key, ph, v) {
                        return Err(format!("`{tok}` does not fill `{}`", g.tok));
                    }
                    1
                }
                _ => 0,
            },
            Slot::Fields => toks[i..]
                .iter()
                .take_while(|tok| Key::from_word(key_of(tok)).is_some())
                .count(),
        };
        if took == 0 && !g.optional {
            return Err(format!("no `{}` at `{}`", g.tok, here.unwrap_or("<end>")));
        }
        i += took;
        filled.push(took > 0);
    }
    match toks.get(i) {
        Some(tok) => Err(format!("`{tok}` past the line's last slot")),
        None => Ok(filled),
    }
}

/// Holds a page body line to the bodies it describes: each fits the line in order, and each
/// bracketed slot is one some body fills and some leaves out.
fn holds(line: &str, bodies: &[&str]) {
    let slots = grammar(line);
    let mut filled = vec![0usize; slots.len()];
    for body in bodies {
        let row = align(&slots, body)
            .unwrap_or_else(|why| panic!("{why}\n  page: {line}\n  body: {body}"));
        for (n, f) in filled.iter_mut().zip(row) {
            *n += usize::from(f);
        }
    }
    for (g, n) in slots.iter().zip(filled) {
        if g.optional {
            assert!(
                n > 0 && n < bodies.len(),
                "`{}` is bracketed, so some body writes it and some leaves it out: {n} of {} \
                 write it\n  page: {line}\n  bodies: {bodies:#?}",
                g.tok,
                bodies.len()
            );
        }
    }
}

/// Whether the mirror carries a record of `op` and `k` ([`Rec::is_bus_class`] on such a
/// record, at the level `k` names), or `None` when no record of `op` has that kind.
fn carried(op: Op, k: Kind) -> Option<bool> {
    let lvl = match op {
        Op::Open | Op::Close | Op::Amend => k.node_level()?,
        Op::Mark | Op::Gap | Op::Snap => Lvl::Session,
    };
    if !k.fits(op, lvl) {
        return None;
    }
    let node = match lvl {
        Lvl::Session => Node::SESSION,
        Lvl::Instance => Node::INSTANCE,
        numbered => Node::new(numbered, 1),
    };
    let rec = Rec {
        seq: 1,
        t: 0,
        op,
        node,
        par: None,
        k,
        src: Src::Aterm,
        conf: Conf::Exact,
        by: Actor::Aterm,
        f: Fields::new(),
    };
    Some(rec.is_bus_class())
}

/// A wall clock for the producer's records.
const WALL0: u64 = 1_700_000_000_000;

/// A stamp `ms` past [`WALL0`].
fn at(ms: u64) -> Stamp {
    Stamp::new(WALL0 + ms, ms)
}

/// A run's open that names every part of a body: a parent, a wire field, a W* field (a
/// program's name), a host field the bus never carries, and an actor.
fn a_run_open() -> Rec {
    let f = Fields::new()
        .u(Key::Blk, 3)
        .and_then(|f| f.tok(Key::Prog, "make"))
        .and_then(|f| f.u(Key::Off, 9))
        .expect("the run's fields");
    Rec {
        seq: 7,
        t: WALL0 + 5,
        op: Op::Open,
        node: Node::new(Lvl::Run, 2),
        par: Some(Node::SESSION),
        k: Kind::Cmd,
        src: Src::Shell,
        conf: Conf::Strong,
        by: Actor::Human,
        f,
    }
}

/// A mark that names the least a body can: no parent, no field, and an edge session's sid
/// the node has no alias for, so no `by=`.
fn a_bare_mark() -> Rec {
    Rec {
        seq: 8,
        t: WALL0 + 6,
        op: Op::Mark,
        node: Node::SESSION,
        par: None,
        k: Kind::Clear,
        src: Src::Ctl,
        conf: Conf::Exact,
        by: Actor::sid(ShortId::new("jsmith1").expect("a short id")),
        f: Fields::new(),
    }
}

/// A log whose session is open.
fn a_session() -> Log {
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
    log
}

/// The snapshot a first take of `log` writes.
fn first_snapshot(log: &mut Log, ms: u64) -> String {
    let take = log.bus_copy(64, at(ms)).encode(Tier::WireMeta, &|_| None);
    take.slots
        .into_iter()
        .find(|slot| slot.kind == SlotKind::Snap)
        .expect("a space's first take snapshots")
        .body
}

/// The snapshot a session's first take writes, with a need standing on an open approval
/// step, so `open=` and `needs=` are both written.
fn a_snapshot() -> String {
    let mut log = a_session();
    let agent = Fields::new()
        .tok(Key::Agent, "claude")
        .expect("agent is a token key");
    let run = log
        .open(
            Open::new(Kind::Agent, Src::Status, Conf::Strong).fields(agent),
            at(1),
        )
        .expect("the agent run opens");
    let turn = log
        .open(
            Open::new(Kind::Turn, Src::Screen, Conf::Strong).under(run),
            at(2),
        )
        .expect("a turn opens");
    let bash = Fields::new().word(Key::Box, "bash").expect("box");
    log.open(
        Open::new(Kind::Approval, Src::Screen, Conf::Strong)
            .under(turn)
            .fields(bash),
        at(3),
    )
    .expect("an approval opens");
    first_snapshot(&mut log, 4)
}

/// The snapshot a quiet session's first take writes: nothing open under it and no need, so
/// neither `open=` nor `needs=`.
fn a_quiet_snapshot() -> String {
    first_snapshot(&mut a_session(), 1)
}

/// `aterm help fabric` names exactly the words `fabric status` prints after `mirror=`
/// ([`MirrorState`], one generated list with its word table), read only from the backticked
/// `mirror=` slots of its `aterm ctl fabric status` row, so prose elsewhere on the page that
/// repeats a word never stands in for one the row dropped.
#[test]
fn the_fabric_page_names_exactly_the_mirror_words_fabric_status_prints() {
    let row = rows(FABRIC_PAGE, "aterm ctl fabric status");
    assert_eq!(row.len(), 1, "one `aterm ctl fabric status` row: {row:?}");
    let slots = spans(&row[0])
        .filter(|span| span.starts_with("mirror="))
        .collect::<Vec<_>>()
        .join(" ");
    let printed: BTreeSet<&str> = MirrorState::ALL.iter().map(|s| s.word()).collect();
    assert_eq!(mirror_words_named(&slots), printed, "{}", row[0]);
}

/// THE SESSION LOG ON THE BUS names exactly the records the mirror carries. Its rows
/// (`op=<ops> [k=<kinds>]`; a row with no `k=` takes every kind its ops have) are read
/// against [`Rec::is_bus_class`] over every [`Op`] and [`Kind`], and its backticked `k=` span
/// names exactly the steps that stay local.
#[test]
fn the_fabric_page_names_exactly_the_records_the_bus_mirror_carries() {
    let section = bus_section(FABRIC_PAGE);
    let mut named: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in section
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("op=") && !l.starts_with("op=<"))
    {
        let mut toks = row.split_whitespace();
        let ops = toks
            .next()
            .and_then(|t| t.strip_prefix("op="))
            .unwrap_or("");
        let kinds = toks.next().map(|t| {
            t.strip_prefix("k=")
                .unwrap_or_else(|| panic!("a row is `op=` then `k=`: {row}"))
        });
        assert_eq!(toks.next(), None, "a row is `op=` and `k=` alone: {row}");
        for word in ops.split('|') {
            let op = Op::from_word(word).unwrap_or_else(|| panic!("`{word}` is no op: {row}"));
            let set = named.entry(op.word()).or_default();
            match kinds {
                Some(kinds) => set.extend(kinds.split('|')),
                None => set.extend(
                    Kind::ALL
                        .iter()
                        .filter(|k| carried(op, **k).is_some())
                        .map(|k| k.word()),
                ),
            }
        }
    }
    let carries: BTreeMap<&str, BTreeSet<&str>> = Op::ALL
        .iter()
        .map(|op| {
            let kinds = Kind::ALL
                .iter()
                .filter(|k| carried(*op, **k) == Some(true))
                .map(|k| k.word());
            (op.word(), kinds.collect::<BTreeSet<_>>())
        })
        .filter(|(_, kinds)| !kinds.is_empty())
        .collect();
    assert_eq!(named, carries, "the page's rows against Rec::is_bus_class");
    let local: BTreeSet<&str> = spans(section)
        .filter_map(|span| span.strip_prefix("k="))
        .flat_map(|words| words.split('|'))
        .filter(|word| !word.is_empty())
        .collect();
    let steps_kept: BTreeSet<&str> = Kind::ALL
        .iter()
        .filter(|k| k.node_level() == Some(Lvl::Step) && carried(Op::Open, **k) == Some(false))
        .map(|k| k.word())
        .collect();
    assert_eq!(local, steps_kept, "the steps the page says stay local");
}

/// The body lines THE SESSION LOG ON THE BUS gives are the mirror's own. Each line is read
/// as slots (a key and its placeholder, a literal token, the record's fields), and every
/// body it describes fills them in order: the record line every body ([`encode_mirror`]'s
/// fullest and barest, a [`cover_body`], a take's snapshot with a need standing and a quiet
/// one's), the cover line a cover, the snapshot line both snapshots. A bracketed slot is
/// one some of those bodies write and some leave out, an unbracketed one every body
/// writes; each value fills its placeholder by the producer's own reader (a node by
/// [`Node::parse`], a kind, actor or source by its word, a field by its [`Key::cap`], an item
/// list such as `needs=` part by part); and the record line's `op=` and `conf=` words are
/// [`Op::ALL`]'s and [`Conf::ALL`]'s.
#[test]
fn the_fabric_pages_body_lines_are_the_mirrors_own() {
    let lines = rows(bus_section(FABRIC_PAGE), "v=2 ");
    let line = |pick: &dyn Fn(&str) -> bool, what: &str| -> String {
        let found: Vec<&String> = lines.iter().filter(|l| pick(l)).collect();
        assert_eq!(found.len(), 1, "one {what} line: {lines:?}");
        found[0].clone()
    };
    let record = line(&|l| l.contains(" op=<"), "record");
    let cover = line(&|l| l.contains(" op=gap "), "cover");
    let snap = line(&|l| l.contains(" op=snap "), "snapshot");

    let ops: Vec<&str> = Op::ALL.iter().map(|op| op.word()).collect();
    assert_eq!(offered(&record, "op"), ops, "{record}");
    let confs: Vec<&str> = Conf::ALL.iter().map(|c| c.word()).collect();
    assert_eq!(offered(&record, "conf"), confs, "{record}");

    let fullest = encode_mirror(&a_run_open(), Tier::WireMeta, 5, Some(4), &|_| None)
        .expect("a run's open is of the bus class");
    let barest = encode_mirror(&a_bare_mark(), Tier::WireMeta, 5, Some(5), &|_| None)
        .expect("a clear mark is of the bus class");
    assert!(!barest.contains(" by="), "{barest}");
    let written = cover_body(5, 7, 4, 2, CoverWhy::Evicted, WALL0);
    let (standing, quiet) = (a_snapshot(), a_quiet_snapshot());
    holds(&record, &[&fullest, &barest, &written, &standing, &quiet]);
    holds(&cover, &[&written]);
    holds(&snap, &[&standing, &quiet]);
}

/// A body is `aterm ctl help log`'s line at the wire tier, as THE SESSION LOG ON THE BUS
/// says: the line's leading words are the body's keys the page maps them to, in order; the
/// line's last token (`t=`) is the body's second; past the record line's header keys that
/// the line has no word for (`v=`, `ep=`, `ms=`), the rest is the line's tokens in the
/// line's order; and a host field the line holds never reaches the body.
#[test]
fn the_fabric_pages_body_is_the_log_line_at_the_wire_tier() {
    let section = bus_section(FABRIC_PAGE);
    let all: Vec<&str> = spans(section).collect();
    let at = all
        .iter()
        .position(|span| span.starts_with("<seq> "))
        .expect("the page maps the log line's leading words");
    let words = all[at].split_whitespace().count();
    let mapped: Vec<&str> = all
        .get(at + 1)
        .expect("the keys the leading words map to")
        .split_whitespace()
        .map(|k| k.strip_suffix('=').unwrap_or(k))
        .collect();
    assert_eq!(mapped.len(), words, "{:?} -> {mapped:?}", all[at]);

    let rec = a_run_open();
    let host = encode(&rec, Tier::Host).expect("the line at the host tier");
    let line = encode(&rec, Tier::WireMeta).expect("the line at the wire tier");
    let body = encode_mirror(&rec, Tier::WireMeta, 5, Some(4), &|_| None).expect("the body");
    assert_eq!(
        encode_mirror(&rec, Tier::Content, 5, Some(4), &|_| None).as_deref(),
        Ok(body.as_str()),
        "the mirror never writes past the wire tier"
    );
    let off = format!("{}=9", Key::Off.word());
    assert!(
        host.contains(&off) && !body.contains(&off),
        "{host}\n{body}"
    );

    let line_toks: Vec<&str> = line.split(' ').collect();
    let body_toks: Vec<&str> = body.split(' ').collect();
    for (key, word) in mapped.iter().zip(&line_toks) {
        let tok = format!("{key}={word}");
        assert!(body_toks.contains(&tok.as_str()), "`{tok}` in {body}");
    }
    let t = line_toks.last().expect("a line");
    assert!(t.starts_with("t="), "{line}");
    assert_eq!(body_toks.get(1), Some(t), "`t=` right after `v=2`: {body}");

    let record = rows(section, "v=2 ")
        .into_iter()
        .find(|l| l.contains(" op=<"))
        .expect("the page's record line");
    let line_keys = body_keys(&line);
    let added: BTreeSet<&str> = page_keys(&record)
        .into_iter()
        .filter(|k| !line_keys.contains(k) && !mapped.contains(k))
        .collect();
    let rest_of_line: Vec<&str> = line_toks[words..line_toks.len() - 1].to_vec();
    let rest_of_body: Vec<&str> = body_toks
        .iter()
        .copied()
        .filter(|tok| {
            let key = key_of(tok);
            key != "t" && !mapped.contains(&key) && !added.contains(key)
        })
        .collect();
    assert_eq!(
        rest_of_body, rest_of_line,
        "{line}\n{body} (added {added:?})"
    );
}

/// One take of `log` (at most `room` slots), landed at its last slot: whether the log held
/// more past it, how many slots it wrote, and the `upto=` of each snapshot among them.
fn take(log: &mut Log, room: usize, ms: u64) -> (bool, usize, Vec<u64>) {
    let take = log.bus_copy(room, at(ms)).encode(Tier::WireMeta, &|_| None);
    if let Some(last) = take.slots.last() {
        log.land(last.at).expect("the take lands");
    }
    let uptos = take
        .slots
        .iter()
        .filter(|slot| slot.kind == SlotKind::Snap)
        .map(|slot| {
            let snap = decode_bus(&slot.body).expect("a snapshot decodes");
            snap.rec
                .f
                .get_u(Key::Upto)
                .expect("a snapshot says `upto=`")
        })
        .collect();
    (take.more, take.slots.len(), uptos)
}

/// `n` commands run and closed: `2 * n` mirrored records.
fn cmds(log: &mut Log, n: u64, ms: u64) {
    for i in 0..n {
        let c = log
            .open(
                Open::new(Kind::Cmd, Src::Shell, Conf::Strong),
                at(ms + 2 * i),
            )
            .expect("a command opens");
        log.close(
            c,
            Close::new(Out::Run(RunOut::Ok), Src::Shell, Conf::Exact),
            at(ms + 2 * i + 1),
        )
        .expect("the command closes");
    }
}

/// The snapshot cadence THE SESSION LOG ON THE BUS gives is the take's. A take writes a
/// snapshot only when it reaches the session's head: the first take writes one, a steady
/// session gets the next once [`SNAP_EVERY`] mirrored records passed (the number the page
/// names), and a burst the bridge takes in several takes (32 slots a take, as its batch) gets
/// none from a take that stops short of the head, so its snapshot comes once a take reaches
/// its end, however many records that is past the last.
#[test]
fn a_snapshot_comes_only_on_a_take_that_reaches_the_sessions_head() {
    let every = SNAP_EVERY.to_string();
    assert!(
        bus_section(FABRIC_PAGE)
            .split_whitespace()
            .any(|w| w == every),
        "the page names SNAP_EVERY ({every})"
    );
    let mut log = a_session();
    assert_eq!(take(&mut log, 64, 1), (false, 2, vec![1]), "the first take");

    let half = SNAP_EVERY / 2;
    cmds(&mut log, half - 1, 10);
    let short = take(&mut log, 64, 200);
    assert_eq!((short.0, short.2), (false, vec![]), "short of SNAP_EVERY");
    cmds(&mut log, 1, 300);
    let steady = log.ms_head();
    assert_eq!(steady, 1 + SNAP_EVERY, "SNAP_EVERY records since the first");
    assert_eq!(
        take(&mut log, 64, 400),
        (false, 3, vec![steady]),
        "a steady session"
    );

    cmds(&mut log, 2 * SNAP_EVERY + 5, 1_000);
    let head = log.ms_head();
    let mut takes = Vec::new();
    let snapshot = loop {
        assert!(takes.len() < 64, "no take reached the head: {takes:?}");
        let (more, slots, uptos) = take(&mut log, 32, 5_000);
        takes.push((more, slots, uptos.clone()));
        if more {
            assert!(
                uptos.is_empty(),
                "a take that stops short of the head: {takes:?}"
            );
        } else if !uptos.is_empty() {
            break uptos;
        } else {
            assert!(slots > 0, "a take at the head with nothing owed: {takes:?}");
        }
    };
    assert!(
        takes.iter().filter(|t| t.0).count() >= 2,
        "the burst took several takes: {takes:?}"
    );
    assert_eq!(snapshot, vec![head], "the burst's snapshot: {takes:?}");
    assert!(
        head - steady > SNAP_EVERY,
        "the burst's snapshot comes {} records past the last",
        head - steady
    );
}
