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
                    } => match (codec::ctl_args(request), reply_of(reply)) {
                        (Ok(words), Ok(r)) if door == doors::CTL || door == doors::TELLER => {
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
            }) if door == doors::CTL => {
                let words = codec::ctl_args(request).ok()?;
                let refs: Vec<&str> = words.iter().map(String::as_str).collect();
                super::ctl::is_act(&refs).then(|| {
                    let reply = reply_of(reply)
                        .map_or_else(|e| format!("unreadable: {e}"), |r| reply_brief(&r));
                    (words, reply)
                })
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
