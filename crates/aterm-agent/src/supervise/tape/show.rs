// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure readers and terminal-safe formatters for recorded harness tapes.

use std::collections::BTreeMap;

use astream_effects::EffectRecord;

use super::codec::{self, End, Item, Mark};
use super::core::{DecideMark, TapeValue};
use super::doors;
use super::recover::{self, Tail};
use crate::supervise::CtlReply;

/// A tape read back.
#[derive(Debug, Clone)]
pub struct Tape {
    /// The header's text, when its frame was whole.
    pub header: Option<String>,
    pub items: Vec<Item>,
    /// Where the tape ended: clean, torn (said), or — salvaged — at damage.
    pub tail: Tail,
    /// The tape's size in bytes, as stored.
    pub bytes: usize,
    /// Its whole frames (`None` for a tape read from its text).
    pub frames: Option<usize>,
}

/// Parses a binary or text tape from `bytes`, optionally salvaging frames before damage.
/// # Errors
/// Returns an error if `bytes` is damaged (without `salvage`), unsupported, or malformed.
pub fn open(bytes: &[u8], salvage: bool) -> Result<Tape, String> {
    if let Some(text) = bytes
        .strip_prefix(b"atape-text ")
        .and_then(|_| std::str::from_utf8(bytes).ok())
    {
        let (header, items) = super::text::parse(text)?;
        return Ok(Tape {
            header,
            items,
            tail: Tail::Clean,
            bytes: bytes.len(),
            frames: None,
        });
    }
    let read = if salvage {
        recover::salvage(bytes)
    } else {
        recover::recover(bytes)
    };
    if let Tail::Fault { at, why } = &read.tail
        && !salvage
    {
        return Err(format!(
            "refused at byte {at}: {why} (`--salvage` keeps the whole frames before it)"
        ));
    }
    Ok(Tape {
        header: read.header,
        items: read.items,
        tail: read.tail,
        bytes: bytes.len(),
        frames: Some(read.frames),
    })
}

/// Returns a note describing a torn or salvaged tail on `tape`, if any.
#[must_use]
pub fn tail_note(tape: &Tape) -> Option<String> {
    match &tape.tail {
        Tail::Clean => None,
        Tail::Torn { at } => Some(format!(
            "the tape is torn at byte {at}: the run ended there without closing its last frame"
        )),
        Tail::Fault { at, why } => Some(format!(
            "salvaged: the whole frames before byte {at} ({why})"
        )),
    }
}

/// Escapes active terminal and bidi characters in `text` for safe terminal display.
#[must_use]
pub fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if aterm_codec::tty::is_active(c) {
            aterm_codec::tty::push_escape(&mut out, c);
        } else {
            out.push(c);
        }
    }
    out
}

/// A body as one short line ([`shown`]).
fn brief(bytes: &[u8]) -> String {
    const SHOWN: usize = 72;
    let text = String::from_utf8_lossy(bytes);
    let mut out: String = text.chars().take(SHOWN).collect();
    out = shown(&out.replace('\n', "⏎"));
    if text.chars().count() > SHOWN {
        out.push('…');
    }
    format!("{} B {out}", bytes.len())
}

fn reply_of(bytes: &[u8]) -> Result<Result<CtlReply, String>, codec::CodecError> {
    <Result<CtlReply, String> as TapeValue>::from_tape(bytes)
}

fn reply_brief(reply: &Result<CtlReply, String>) -> String {
    match reply {
        Ok(r) => {
            let said = if r.stdout.trim().is_empty() {
                &r.stderr
            } else {
                &r.stdout
            };
            let first = said.lines().next().unwrap_or("");
            let first: String = first.chars().take(72).collect();
            format!("{} {}", r.code, shown(&first))
        }
        Err(why) => format!("transport error: {}", shown(why)),
    }
}

/// A request's words as one printed line ([`shown`]).
fn words_shown(words: &[String]) -> String {
    shown(&words.join(" "))
}

/// Formats `tape`'s items as one line each for `tape cat`.
#[must_use]
pub fn skim(tape: &Tape) -> String {
    let mut out = String::new();
    let mut ordinal = 0u64;
    for item in &tape.items {
        let line = match item {
            Item::Effect { record, stamp } => {
                let at =
                    stamp.map_or_else(String::new, |s| format!(" @{:.3}s", s.mono_ns as f64 / 1e9));
                let body = match record {
                    EffectRecord::Clock(ns) => format!("clock {}", *ns as i64),
                    EffectRecord::Mono(ns) => format!("mono {:.6}s", *ns as f64 / 1e9),
                    EffectRecord::Rand(w) => format!("rand {w}"),
                    EffectRecord::File { path, bytes } => {
                        format!("file {} {}", shown(path), brief(bytes))
                    }
                    EffectRecord::FileErr { path, kind } => {
                        format!("file {} {kind:?}", shown(path))
                    }
                    EffectRecord::Exchange {
                        door,
                        request,
                        reply,
                    } => match (
                        super::ctl::reported_args(door, request),
                        super::ctl::control_reply(door, reply),
                    ) {
                        (Ok(words), Ok(Some(r))) if super::ctl::is_control_door(door) => {
                            format!(
                                "{} {} → {}",
                                shown(door),
                                words_shown(&words),
                                reply_brief(&r)
                            )
                        }
                        _ => format!("{} {} → {}", shown(door), brief(request), brief(reply)),
                    },
                    EffectRecord::Emit { door, bytes } => {
                        format!("{} {}", shown(door), brief(bytes))
                    }
                    EffectRecord::Raised { door, false_reads } => {
                        format!("{} raised after {false_reads} false reads", shown(door))
                    }
                };
                let line = format!("#{ordinal}{at} {body}");
                ordinal += 1;
                line
            }
            Item::Note { kind, text } => {
                format!("note {} {}", shown(kind), brief(text.as_bytes()))
            }
            Item::Mark(Mark::Nested { inner, outer }) => format!(
                "mark: {} was reached inside {} and taped nowhere",
                shown(inner),
                shown(outer)
            ),
            Item::Mark(Mark::Truncated { door }) => {
                format!("mark: the run reached its cap; effect #{door} on are not on the tape")
            }
            Item::End(End::Returned { how }) => format!("end: returned {}", shown(how)),
            Item::End(End::Suspended) => "end: suspended for a seamless update".to_string(),
            Item::End(End::Unwound { message }) => format!("end: unwound: {}", shown(message)),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// One recorded decision on a tape, with the judged screen and follow-up action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// The mark's door (`decide.approval`, `decide.question`, or `decide.turn_end`).
    pub door: String,
    /// The mark's index among the tape's items.
    pub item: usize,
    /// The rule that decided.
    pub rule: Option<String>,
    /// The decision as the loop rendered it.
    pub outcome: String,
    /// The judged screen's sequence.
    pub seq: u64,
    /// The ordinal of the round trip whose reply was the judged screen.
    pub screen_record: Option<u64>,
    /// That screen's rows, read back from its reply.
    pub screen: Option<Vec<String>>,
    /// The first act after the mark, before the next decision: its words.
    pub act: Option<Vec<String>>,
    /// That act's reply, as one line.
    pub reply: Option<String>,
    /// The first journaled or printed line after the mark before the next decision.
    pub journal: Option<String>,
}

/// Extracts every valid decision from `tape` in order.
#[must_use]
pub fn decisions(tape: &Tape) -> Vec<Decision> {
    let effects: Vec<usize> = tape
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| i.record().is_some())
        .map(|(at, _)| at)
        .collect();
    let marks: Vec<(usize, &str, DecideMark)> = tape
        .items
        .iter()
        .enumerate()
        .filter_map(|(at, item)| match item.record() {
            Some(EffectRecord::Emit { door, bytes }) if door.starts_with("decide.") => {
                let mark = DecideMark::decode(std::str::from_utf8(bytes).ok()?)?;
                Some((at, door.as_str(), mark))
            }
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for (k, (at, door, mark)) in marks.iter().enumerate() {
        let until = marks
            .get(k + 1)
            .map_or(tape.items.len(), |(next, ..)| *next);
        let after = &tape.items[at + 1..until];
        let screen = mark
            .screen
            .and_then(|n| effects.get(usize::try_from(n).ok()?))
            .and_then(|&i| screen_rows(&tape.items[i]));
        let act = after.iter().find_map(|item| match item.record() {
            Some(EffectRecord::Exchange {
                door,
                request,
                reply,
            }) if door == doors::CTL || door == doors::CTL_DUTY => {
                let argv = super::ctl::argv(door, request).ok()?;
                let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
                if !super::ctl::is_act(&refs) {
                    return None;
                }
                let words = super::ctl::reported_args(door, request).ok()?;
                let reply = super::ctl::control_reply(door, reply).map_or_else(
                    |e| format!("unreadable: {e}"),
                    |r| r.map_or_else(|| "adapter unavailable".to_string(), |r| reply_brief(&r)),
                );
                Some((words, reply))
            }
            _ => None,
        });
        let journal = after
            .iter()
            .find_map(|item| match item.record() {
                Some(EffectRecord::Emit { door, bytes }) if door == doors::JOURNAL => {
                    let text = std::str::from_utf8(bytes).ok()?;
                    crate::supervise::journal::JournalRecord::from_json(text)
                        .ok()
                        .map(|r| r.line)
                }
                _ => None,
            })
            .or_else(|| {
                after.iter().find_map(|item| match item.record() {
                    Some(EffectRecord::Exchange { door, request, .. }) if door == doors::OUT => {
                        String::from_utf8(request.clone()).ok()
                    }
                    _ => None,
                })
            });
        let (act, reply) = act.map_or((None, None), |(w, r)| (Some(w), Some(r)));
        out.push(Decision {
            door: (*door).to_string(),
            item: *at,
            rule: mark.rule.clone(),
            outcome: mark.outcome.clone(),
            seq: mark.seq,
            screen_record: mark.screen,
            screen,
            act,
            reply,
            journal,
        });
    }
    out
}

fn screen_rows(item: &Item) -> Option<Vec<String>> {
    let EffectRecord::Exchange { door, reply, .. } = item.record()? else {
        return None;
    };
    if door != doors::CTL {
        return None;
    }
    let reply = reply_of(reply).ok()?.ok()?;
    crate::supervise::screen::parse_text_json(&reply.stdout)
        .ok()
        .map(|s| s.rows)
}

/// `decisions` as text.
#[must_use]
pub fn render_decisions(decisions: &[Decision]) -> String {
    if decisions.is_empty() {
        return "no decisions on this tape\n".to_string();
    }
    let mut out = String::new();
    for (n, d) in decisions.iter().enumerate() {
        out.push_str(&format!(
            "decision {} at item {}: {} rule={}\n  outcome: {}\n",
            n + 1,
            d.item,
            shown(&d.door),
            shown(d.rule.as_deref().unwrap_or("-")),
            shown(&d.outcome)
        ));
        match (&d.screen, d.screen_record) {
            (Some(rows), Some(record)) => {
                out.push_str(&format!("  screen (seq {}, record #{record}):\n", d.seq));
                for row in rows {
                    out.push_str("    │ ");
                    out.push_str(&shown(row));
                    out.push('\n');
                }
            }
            _ => out.push_str(&format!("  screen (seq {}): not on the tape\n", d.seq)),
        }
        out.push_str(&format!(
            "  act: {}\n  reply: {}\n  journal: {}\n",
            d.act
                .as_ref()
                .map_or_else(|| "none".to_string(), |w| words_shown(w)),
            d.reply.as_deref().unwrap_or("-"),
            shown(d.journal.as_deref().unwrap_or("-")),
        ));
    }
    out
}

/// One text a turn-end decision wrote into the session, timed off the tape ([`typed`]): aterm's
/// act, apart from how long its program took to show it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Typed {
    /// The rule that decided it (`land@v1`, …).
    pub rule: Option<String>,
    /// The judged screen's sequence.
    pub seq: u64,
    /// The last wall clock (unix ms) the loop read before its decision: when it decided.
    pub decided_ms: Option<i64>,
    /// When the write went in (unix ms, the `send` or `turn` exchange's stamp): aterm's act.
    pub sent_ms: i64,
    /// The first screen read after the write that shows the text, more often than the read
    /// before the write did: the program's draw, as aterm first saw it. `None`: no read showed it.
    pub drawn_ms: Option<i64>,
    /// How long the program demonstrably held the text, where a read showed it drawn: the write
    /// to the last read that did not show it, and the time aterm then spent in `await seq` waits
    /// (which wake at any move of the screen) until the read that did. Never the time aterm spent
    /// on anything else, so a late look by aterm stays aterm's ([`Self::aterm_ms`]).
    pub held_ms: Option<i64>,
    /// The last read after the write that did not show it (unix ms), where no read did.
    pub unshown_ms: Option<i64>,
    /// The `status` read between the write and that draw that found the input unread longest.
    pub unread: Option<Unread>,
    /// Whether the first `await seq` past the write saw the screen move (`true`) or ran out
    /// (`false`), where one followed it: the loop journals its held-text `NOTE` on the second.
    pub reaction: Option<bool>,
    /// When the write's Enter went in: the first `key … enter` the host took after it, under a
    /// decision of the same rule (the write's own, or a later one sending the line it left), before
    /// the next write.
    pub enter_ms: Option<i64>,
}

impl Typed {
    /// ATERM'S OWN SHARE OF THE ACT from `point` (unix ms) to the Enter: all of it but the
    /// program's demonstrable hold ([`Self::held_ms`]). `None` where no Enter was taken.
    #[must_use]
    pub fn aterm_ms(&self, point: i64) -> Option<i64> {
        self.enter_ms
            .map(|enter| enter - point - self.held_ms.unwrap_or(0))
    }
}

/// A `status` read that found the session's input unread ([`Typed::unread`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unread {
    /// When it was read (unix ms).
    pub at_ms: i64,
    /// `input=`: `pending` or `typeahead`.
    pub word: String,
    /// `input_bytes=`: the bytes the program had not read.
    pub bytes: Option<u64>,
    /// `input_wait_ms=`: how long the oldest of them had waited.
    pub wait_ms: Option<u64>,
}

impl Unread {
    /// `input=<w> input_bytes=<n> input_wait_ms=<ms>`, as `status` says it.
    #[must_use]
    pub fn said(&self) -> String {
        let n = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
        format!(
            "input={} input_bytes={} input_wait_ms={}",
            self.word,
            n(self.bytes),
            n(self.wait_ms)
        )
    }

    /// The unread input a `status` reply names, if any.
    #[must_use]
    pub fn of_status(stdout: &str, at_ms: i64) -> Option<Self> {
        let field = |key: &str| {
            stdout
                .split_whitespace()
                .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
        };
        let word = field("input").filter(|w| matches!(*w, "pending" | "typeahead"))?;
        let num = |key: &str| field(key).and_then(|v| v.parse().ok());
        Some(Self {
            at_ms,
            word: word.to_string(),
            bytes: num("input_bytes"),
            wait_ms: num("input_wait_ms"),
        })
    }
}

/// How many squashed characters of a written text a read must show to show it ([`typed`]).
const DRAWN_HEAD: usize = 48;

/// What [`typed`] follows of the last write while it is the one the exchanges after it belong to.
struct Following {
    /// The text's head, squashed ([`DRAWN_HEAD`]).
    head: String,
    /// How often the read before the write showed that head.
    before: usize,
    /// When the last read since the write that did not show it came (the write's own stamp first).
    unshown_ms: i64,
    /// The time spent in `await seq` waits since that read.
    watched_ms: i64,
}

/// How often `rows` show `head` (squashed), the rows run together as a wrapped line reads.
fn shows(rows: &[String], head: &str) -> usize {
    if head.is_empty() {
        return 0;
    }
    crate::supervise::policy::approval::squashed(&rows.concat())
        .matches(head)
        .count()
}

/// THE TEXTS THE LOOP WROTE, TIMED: each turn-end decision followed by a `send` (or `turn`) the
/// host took, with the decision's clock, the write's stamp, the program's draw as the screen reads
/// after it show it, how long the program held it, the longest unread-input read between, and
/// the Enter. Every wall time is the tape's own: an exchange's stamp (taken as its reply came),
/// else the last clock read before it; an exchange began at the item before it.
#[must_use]
pub fn typed(tape: &Tape) -> Vec<Typed> {
    let mut out: Vec<Typed> = Vec::new();
    let mut clock: Option<i64> = None;
    // The turn-end decision whose write is still to come: its rule, its seq and its clock.
    let mut open: Option<(Option<String>, u64, Option<i64>)> = None;
    // The rule of the last decision of any kind (`None` where it was no turn end's).
    let mut deciding: Option<String> = None;
    // The last write, while the reads after it are its.
    let mut follow: Option<Following> = None;
    let mut last_rows: Vec<String> = Vec::new();
    for item in &tape.items {
        let began = clock;
        if let Item::Effect { stamp: Some(s), .. } = item {
            clock = Some(s.wall_ms);
        }
        match item.record() {
            Some(EffectRecord::Clock(ns)) => clock = i64::try_from(ns / 1_000_000).ok(),
            Some(EffectRecord::Emit { door, bytes }) if door.starts_with("decide.") => {
                let mark = (door == doors::DECIDE_TURN_END)
                    .then(|| std::str::from_utf8(bytes).ok().and_then(DecideMark::decode))
                    .flatten();
                deciding = mark.as_ref().and_then(|m| m.rule.clone());
                open = mark.map(|mark| (mark.rule, mark.seq, clock));
            }
            Some(EffectRecord::Exchange {
                door,
                request,
                reply,
            }) if door == doors::CTL || door == doors::CTL_DUTY => {
                let Ok(argv) = super::ctl::argv(door, request) else {
                    continue;
                };
                let words: Vec<&str> = argv
                    .iter()
                    .map(String::as_str)
                    .filter(|w| !w.starts_with('@'))
                    .collect();
                let Ok(Some(Ok(r))) = super::ctl::control_reply(door, reply) else {
                    continue;
                };
                let at = clock.unwrap_or(0);
                let took = r.ok() && r.stdout.trim_start().starts_with("OK") && !r.skipped();
                if matches!(words.first(), Some(&("send" | "turn"))) {
                    // A write the host turned away wrote nothing: the decision is spent, and the
                    // last write's reads go on.
                    let decision = open.take();
                    if !took {
                        continue;
                    }
                    follow = None;
                    if let Some((rule, seq, decided)) = decision {
                        let text = words
                            .iter()
                            .position(|w| *w == "--")
                            .map(|i| words[i + 1..].join(" "))
                            .unwrap_or_default();
                        let head: String = crate::supervise::policy::approval::squashed(&text)
                            .chars()
                            .take(DRAWN_HEAD)
                            .collect();
                        follow = Some(Following {
                            before: shows(&last_rows, &head),
                            head,
                            unshown_ms: at,
                            watched_ms: 0,
                        });
                        out.push(Typed {
                            rule,
                            seq,
                            decided_ms: decided,
                            sent_ms: at,
                            ..Typed::default()
                        });
                    }
                    continue;
                }
                if words.first() == Some(&"text")
                    && let Some(rows) = crate::supervise::screen::parse_text_json(&r.stdout)
                        .ok()
                        .map(|s| s.rows)
                {
                    if let (Some(f), Some(last)) = (follow.as_mut(), out.last_mut())
                        && last.drawn_ms.is_none()
                    {
                        if shows(&rows, &f.head) > f.before {
                            last.drawn_ms = Some(at);
                            last.held_ms = Some(f.unshown_ms - last.sent_ms + f.watched_ms);
                            last.unshown_ms = None;
                        } else {
                            f.unshown_ms = at;
                            f.watched_ms = 0;
                            last.unshown_ms = Some(at);
                        }
                    }
                    last_rows = rows;
                    continue;
                }
                let (Some(f), Some(last)) = (follow.as_mut(), out.last_mut()) else {
                    continue;
                };
                match words.as_slice() {
                    ["await", "seq", ..] => {
                        if last.reaction.is_none() {
                            last.reaction = Some(r.stdout.trim_start().starts_with("OK seq"));
                        }
                        if last.drawn_ms.is_none() {
                            f.watched_ms += at - began.unwrap_or(at);
                        }
                    }
                    ["status", ..] if last.drawn_ms.is_none() => {
                        if let Some(u) = Unread::of_status(&r.stdout, at)
                            && last
                                .unread
                                .as_ref()
                                .is_none_or(|was| u.wait_ms >= was.wait_ms)
                        {
                            last.unread = Some(u);
                        }
                    }
                    ["key", .., "enter"]
                        if took && last.enter_ms.is_none() && deciding == last.rule =>
                    {
                        last.enter_ms = Some(at);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    out
}

/// One door's share of a tape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DoorStats {
    /// Its records.
    pub records: usize,
    /// Decoded byte count carried by those records before delta and compression.
    pub raw: usize,
}

/// Size, item counts, and rate statistics for a tape.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    /// The tape's size as stored.
    pub bytes: usize,
    pub frames: Option<usize>,
    pub items: usize,
    pub effects: usize,
    /// Effects by kind (`Mono`, `Exchange`, …).
    pub kinds: BTreeMap<&'static str, usize>,
    /// Round trips, outputs and flags by door.
    pub doors: BTreeMap<String, DoorStats>,
    /// Decisions' marks, by door.
    pub decisions: BTreeMap<String, usize>,
    /// Span of the run in nanoseconds from origin to the latest monotonic reading.
    pub span_ns: u64,
    /// Stored tape size per hour of span in kB (1000 bytes), or `None` if zero span.
    pub kb_per_hour: Option<f64>,
    /// How the run ended, if the tape says.
    pub end: Option<String>,
}

/// What `tape` cost.
#[must_use]
pub fn stats(tape: &Tape) -> Stats {
    let mut kinds = BTreeMap::new();
    let mut doors = BTreeMap::<String, DoorStats>::new();
    let mut decisions = BTreeMap::new();
    let mut span_ns = 0u64;
    let mut effects = 0;
    let mut end = None;
    for item in &tape.items {
        match item {
            Item::Effect { record, stamp } => {
                effects += 1;
                *kinds.entry(record.kind()).or_insert(0) += 1;
                if let Some(s) = stamp {
                    span_ns = span_ns.max(s.mono_ns);
                }
                let (door, raw) = match record {
                    EffectRecord::Mono(ns) => {
                        span_ns = span_ns.max(*ns);
                        continue;
                    }
                    EffectRecord::Exchange {
                        door,
                        request,
                        reply,
                    } => (door, request.len() + reply.len()),
                    EffectRecord::Emit { door, bytes } => {
                        if door.starts_with("decide.") {
                            *decisions.entry(door.clone()).or_insert(0) += 1;
                        }
                        (door, bytes.len())
                    }
                    EffectRecord::Raised { door, .. } => (door, 0),
                    EffectRecord::Clock(_)
                    | EffectRecord::Rand(_)
                    | EffectRecord::File { .. }
                    | EffectRecord::FileErr { .. } => continue,
                };
                let d = doors.entry(door.clone()).or_default();
                d.records += 1;
                d.raw += raw;
            }
            Item::End(e) => {
                end = Some(match e {
                    End::Returned { how } => {
                        format!("returned {}", how.lines().next().unwrap_or(""))
                    }
                    End::Suspended => "suspended".to_string(),
                    End::Unwound { message } => format!("unwound: {message}"),
                });
            }
            Item::Note { .. } | Item::Mark(_) => {}
        }
    }
    let hours = span_ns as f64 / 3.6e12;
    Stats {
        bytes: tape.bytes,
        frames: tape.frames,
        items: tape.items.len(),
        effects,
        kinds,
        doors,
        decisions,
        span_ns,
        kb_per_hour: (span_ns > 0).then(|| tape.bytes as f64 / 1000.0 / hours),
        end,
    }
}

/// `stats` as text.
#[must_use]
pub fn render_stats(stats: &Stats) -> String {
    let mut out = format!("bytes {}", stats.bytes);
    if let Some(frames) = stats.frames {
        out.push_str(&format!(" in {frames} frames"));
    }
    let secs = stats.span_ns as f64 / 1e9;
    out.push_str(&format!(
        "\nspan {secs:.1} s ({:.2} h)\nkB/h {}\nitems {} effects {}\nend {}\n",
        secs / 3600.0,
        stats
            .kb_per_hour
            .map_or_else(|| "-".to_string(), |k| format!("{k:.1}")),
        stats.items,
        stats.effects,
        shown(stats.end.as_deref().unwrap_or("none on the tape")),
    ));
    let kinds: Vec<String> = stats
        .kinds
        .iter()
        .map(|(k, n)| format!("{k} {n}"))
        .collect();
    out.push_str(&format!("kinds {}\n", kinds.join(", ")));
    out.push_str("doors\n");
    for (door, d) in &stats.doors {
        out.push_str(&format!(
            "  {} {} ({} B raw)\n",
            shown(door),
            d.records,
            d.raw
        ));
    }
    let total: usize = stats.decisions.values().sum();
    let each: Vec<String> = stats
        .decisions
        .iter()
        .map(|(door, n)| format!("{} {n}", shown(door.trim_start_matches("decide."))))
        .collect();
    out.push_str(&format!("decisions {total}"));
    if !each.is_empty() {
        out.push_str(&format!(" ({})", each.join(", ")));
    }
    out.push('\n');
    out
}

#[cfg(test)]
#[path = "show_tests.rs"]
mod tests;
