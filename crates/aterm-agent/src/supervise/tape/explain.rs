// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive replay FILE --why --at …`: explain one recorded decision on a harness tape.

use std::sync::Arc;

use astream_effects::EffectRecord;

use super::codec::Item;
use super::core::{DecideMark, NOTE_TURN_END, TapeValue};
use super::header::{Header, digest_seed};
use super::replay::{Options, replay_traced};
use super::show::{self, Tape, shown};
use super::{codec, doors};
use crate::supervise::policy::approval::{ApprovalCtx, decide_traced};
use crate::supervise::policy::box_facts::BoxFacts;
use crate::supervise::policy::probe::{KeyedProbe, TapeIndex};
use crate::supervise::policy::trace::{self, Outcome, TraceStep};
use crate::supervise::run::{
    HEAD_SEARCH, WorkerSource, head_row, head_rows, joined_box, same_box, whole_box_decision,
};
use crate::supervise::screen::{Screen, parse_text_json};
use crate::supervise::{CtlReply, SupervisorConfig};

/// Where on a tape a `--why` stands ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    /// The decision on the screen of this sequence.
    Seq(u64),
    /// The decision in force at effect `#N`.
    Door(u64),
    /// Last decision at or before `secs` into local `day` (or the tape's single matching day).
    Time { day: Option<i64>, secs: u32 },
    /// Decision followed by a journal or printed line containing this text.
    Line(String),
}

impl At {
    /// Parse `--at` (`seq=N`, `door=[#]N`, `[YYYY-MM-DDT]HH:MM[:SS]`, or journal text).
    ///
    /// # Errors
    /// Unparseable `seq=`/`door=` number or empty input.
    pub fn parse(text: &str) -> Result<Self, String> {
        if let Some(n) = text.strip_prefix("seq=") {
            return n
                .parse()
                .map(Self::Seq)
                .map_err(|_| format!("--at seq= needs a number, not {n:?}"));
        }
        if let Some(n) = text.strip_prefix("door=") {
            let n = n.strip_prefix('#').unwrap_or(n);
            return n
                .parse()
                .map(Self::Door)
                .map_err(|_| format!("--at door= needs an effect's ordinal, not {n:?}"));
        }
        if let Some(secs) = time_of_day(text) {
            return Ok(Self::Time { day: None, secs });
        }
        if let Some((date, time)) = text.split_once(['T', ' '])
            && let Some(day) = civil_day(date)
            && let Some(secs) = time_of_day(time)
        {
            return Ok(Self::Time {
                day: Some(day),
                secs,
            });
        }
        if text.trim().is_empty() {
            return Err("--at needs seq=N, door=N, HH:MM:SS or a journal line".to_string());
        }
        Ok(Self::Line(text.trim().to_string()))
    }
}

/// `YYYY-MM-DD`, a day of the calendar, as days since 1970-01-01.
fn civil_day(text: &str) -> Option<i64> {
    let mut parts = text.split('-');
    let mut num = |len: usize| -> Option<i64> {
        let p = parts.next()?;
        (p.len() == len && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let (y, m, d) = (num(4)?, num(2)?, num(2)?);
    if parts.next().is_some() {
        return None;
    }
    let day = aterm_types::rfc3339::days_from_civil(y, m, d);
    (aterm_types::rfc3339::civil_from_days(day) == (y, m, d)).then_some(day)
}

/// `HH:MM` or `HH:MM:SS`, as seconds into the day.
fn time_of_day(text: &str) -> Option<u32> {
    let mut parts = text.split(':');
    let num = |p: Option<&str>| -> Option<u32> {
        let p = p?;
        (!p.is_empty() && p.len() <= 2 && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let h = num(parts.next())?;
    let m = num(parts.next())?;
    let s = match parts.next() {
        None => 0,
        some => num(some)?,
    };
    (parts.next().is_none() && h < 24 && m < 60 && s < 60).then_some(h * 3600 + m * 60 + s)
}

/// One fact a box was decided on, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// The fact's name (`cwd`, `worker`, `owner_asks` …).
    pub name: &'static str,
    /// Its value as the decision had it.
    pub value: String,
    /// Door and record ordinal it came through, or the loop's own memory.
    pub door: String,
}

/// The box this build's reader finds on the judged screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxRead {
    /// The reader's program.
    pub program: String,
    /// The box's kind (`bash`, `read`, `question` …).
    pub kind: String,
    pub title: String,
    /// Its options, as drawn: `1. Yes`, the focused one marked `❯`.
    pub options: Vec<String>,
}

/// One decision on a tape, explained ([module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Why {
    /// The tape's id.
    pub tape: String,
    /// The build that recorded it, from its header.
    pub recorded_by: Option<String>,
    /// The build explaining it (this one).
    pub explained_by: String,
    /// Mark door (`decide.approval`, `decide.question`, `decide.turn_end`).
    pub door: String,
    /// The mark's ordinal (`#N` in `tape cat`).
    pub mark: u64,
    /// When it was made, Unix milliseconds: the last wall reading before it.
    pub at_ms: Option<i64>,
    /// Local clock timestamp (`YYYY-MM-DD HH:MM:SS`) when queried by local time.
    pub at_local: Option<String>,
    /// The judged screen's sequence.
    pub seq: u64,
    /// The ordinal of the round trip whose reply was the judged screen.
    pub screen_record: Option<u64>,
    /// That screen's rows.
    pub screen: Option<Vec<String>>,
    /// The box this build's reader finds on it.
    pub parsed: Option<BoxRead>,
    /// The facts the box was decided on, each with its door.
    pub facts: Vec<Fact>,
    /// Facts or looks missing from the tape that the re-decision needed.
    pub needs: Vec<String>,
    /// The rule the recorded decision names.
    pub recorded_rule: Option<String>,
    /// The recorded decision, as the loop rendered it.
    pub recorded_outcome: String,
    /// Decision computed by this build's policy on the same facts.
    pub decided: Option<String>,
    /// Whether this build's policy agrees with the recorded decision, or `None` if unknown.
    pub agrees: Option<bool>,
    /// The policy's rule trace, where one could be computed.
    pub trace: Option<Vec<TraceStep>>,
    /// Why there is no trace, or what it stands on.
    pub trace_note: Option<String>,
    /// The first act after the decision: its words.
    pub act: Option<Vec<String>>,
    /// That act's reply, as one line.
    pub reply: Option<String>,
    /// The first line the loop journaled (or printed) after the decision.
    pub journal: Option<String>,
    /// The first approval-ledger row the loop wrote after the decision.
    pub ledger: Option<String>,
    /// Turn-end fold and reading notes recorded before a turn-end decision.
    pub notes: Vec<String>,
}

impl Why {
    /// The rule (or the check) the trace says decided.
    #[must_use]
    pub fn decided_by(&self) -> Option<&'static str> {
        self.trace
            .as_deref()
            .and_then(trace::deciding)
            .map(TraceStep::name)
    }
}

/// The effects of `tape` by ordinal, and each one's item index.
fn effects(tape: &Tape) -> (Vec<EffectRecord>, Vec<usize>) {
    let mut records = Vec::new();
    let mut items = Vec::new();
    for (at, item) in tape.items.iter().enumerate() {
        if let Some(r) = item.record() {
            records.push(r.clone());
            items.push(at);
        }
    }
    (records, items)
}

/// The wall time (Unix ms) at each item: the last `Clock` record or stamp at
/// or before it.
fn wall_times(tape: &Tape) -> Vec<Option<i64>> {
    let mut last = None;
    tape.items
        .iter()
        .map(|item| {
            if let Item::Effect { record, stamp } = item {
                if let EffectRecord::Clock(ns) = record {
                    last = i64::try_from(*ns).ok().map(|ns| ns.div_euclid(1_000_000));
                }
                if let Some(s) = stamp {
                    last = Some(s.wall_ms);
                }
            }
            last
        })
        .collect()
}

/// The local clock, as [`why`]'s `offset_at` reads it: each instant's local
/// time by its own offset, asked once per quarter hour of the tape (every
/// zone's offset changes on one), so a tape recorded across a change of
/// daylight saving reads each side as it was, and one recorded in another
/// season reads by its own offsets, never today's.
struct Zone<'a> {
    offset_at: &'a dyn Fn(i64) -> Option<i64>,
    read: std::collections::BTreeMap<i64, i64>,
}

impl<'a> Zone<'a> {
    fn new(offset_at: &'a dyn Fn(i64) -> Option<i64>) -> Self {
        Self {
            offset_at,
            read: std::collections::BTreeMap::new(),
        }
    }

    /// The Unix second `unix` on the local clock, in seconds since its
    /// 1970-01-01 00:00.
    fn local(&mut self, unix: i64) -> Result<i64, String> {
        let quarter = unix.div_euclid(900);
        let offset = match self.read.get(&quarter) {
            Some(&o) => o,
            None => {
                let o = (self.offset_at)(unix).ok_or_else(|| {
                    format!(
                        "the local zone's offset at {} does not read: name the point by seq= or \
                         door=",
                        utc_words(unix)
                    )
                })?;
                self.read.insert(quarter, o);
                o
            }
        };
        Ok(unix.saturating_add(offset))
    }
}

/// The local instant (seconds since the local 1970-01-01 00:00) a point
/// named by a local time stands for: `secs` into the local day `day`, or
/// with no day named, into the one day of the tape's run — from its first
/// wall reading to its last — that time falls within.
fn local_target(
    walls: &[Option<i64>],
    day: Option<i64>,
    secs: u32,
    zone: &mut Zone<'_>,
) -> Result<i64, String> {
    let secs = i64::from(secs);
    if let Some(day) = day {
        return Ok(day * 86_400 + secs);
    }
    let read = walls.iter().flatten().map(|ms| ms.div_euclid(1000));
    let (Some(lo), Some(hi)) = (read.clone().min(), read.max()) else {
        return Err("the tape holds no wall time: name the point by seq=, door= or a line".into());
    };
    let (lo, hi) = (zone.local(lo)?, zone.local(hi)?);
    let first_day = lo.div_euclid(86_400);
    if first_day == hi.div_euclid(86_400) {
        return Ok(first_day * 86_400 + secs);
    }
    let from = (lo - secs + 86_399).div_euclid(86_400);
    let to = (hi - secs).div_euclid(86_400);
    if from == to {
        return Ok(from * 86_400 + secs);
    }
    Err(format!(
        "--at {} is inside this tape's run on {} of its days (it runs from {} to {}, local): \
         name the day, --at YYYY-MM-DDTHH:MM[:SS]",
        clock(secs),
        if from > to { "none" } else { "more than one" },
        local_words(lo),
        local_words(hi)
    ))
}

/// Seconds into a day as `HH:MM:SS`.
fn clock(secs: i64) -> String {
    let s = secs.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

/// Seconds since a clock's 1970-01-01 00:00 as `YYYY-MM-DD HH:MM:SS`.
fn local_words(secs: i64) -> String {
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02} {}", clock(secs))
}

/// A Unix second in UTC, `YYYY-MM-DDTHH:MM:SSZ`.
fn utc_words(unix: i64) -> String {
    let mut out = local_words(unix).replacen(' ', "T", 1);
    out.push('Z');
    out
}

/// Explain the decision at `at` on `bytes`, using `offset_at` for local-time queries.
///
/// # Errors
/// Unreadable tape, no matching decision, ambiguous/out-of-range local time, or unreadable zone offset.
pub fn why(bytes: &[u8], at: &At, offset_at: &dyn Fn(i64) -> Option<i64>) -> Result<Why, String> {
    let tape = show::open(bytes, false)?;
    let book = Book::open(&tape);
    if book.decisions.is_empty() {
        return Err("no decisions on this tape".to_string());
    }
    let mut zone = Zone::new(offset_at);
    let chosen = match at {
        At::Seq(n) => book.decisions.iter().find(|d| d.seq == *n),
        At::Door(n) => book
            .decisions
            .iter()
            .rev()
            .find(|d| book.ordinal_of(d.item).is_some_and(|o| o <= *n)),
        At::Time { day, secs } => {
            let target = local_target(&book.walls, *day, *secs, &mut zone)?;
            let mut found = None;
            for d in book.decisions.iter().rev() {
                if let Some(ms) = book.walls[d.item]
                    && zone.local(ms.div_euclid(1000))? <= target
                {
                    found = Some(d);
                    break;
                }
            }
            found
        }
        At::Line(text) => {
            let line_item = tape.items.iter().position(|item| match item.record() {
                Some(EffectRecord::Emit { door, bytes }) if door == doors::JOURNAL => {
                    std::str::from_utf8(bytes)
                        .ok()
                        .and_then(|t| crate::supervise::journal::JournalRecord::from_json(t).ok())
                        .is_some_and(|r| r.line.contains(text.as_str()))
                }
                Some(EffectRecord::Exchange { door, request, .. }) if door == doors::OUT => {
                    String::from_utf8_lossy(request).contains(text.as_str())
                }
                _ => false,
            });
            line_item.and_then(|li| book.decisions.iter().rev().find(|d| d.item < li))
        }
    };
    let d =
        chosen.ok_or_else(|| format!("no decision on this tape at {}", shown(&at_words(at))))?;
    let at_local = match (at, book.walls[d.item]) {
        (At::Time { .. }, Some(ms)) => Some(local_words(zone.local(ms.div_euclid(1000))?)),
        _ => None,
    };
    Ok(book.explain(d, at_local, bytes, None))
}

/// Opened tape state indexed for explaining decisions.
pub(crate) struct Book<'t> {
    tape: &'t Tape,
    pub(crate) decisions: Vec<show::Decision>,
    /// Its effects by ordinal, and their keyed index: built once, here, and
    /// shared by the keyed probe of every decision decided again on the tape.
    index: Arc<TapeIndex>,
    /// Each effect's item index, by ordinal (ascending).
    item_of: Vec<usize>,
    walls: Vec<Option<i64>>,
    /// The header, or why it does not read on this build.
    pub(crate) header: Result<Header, String>,
}

impl<'t> Book<'t> {
    /// `tape`, opened.
    pub(crate) fn open(tape: &'t Tape) -> Self {
        let decisions = show::decisions(tape);
        let (records, item_of) = effects(tape);
        let header = match tape.header.as_deref() {
            None => Err("the tape has no header: no policy to decide again with".to_string()),
            Some(text) => Header::decode(text).map_err(|e| {
                format!(
                    "the tape's header does not read on this build ({e}): no policy to decide \
                     again with"
                )
            }),
        };
        Self {
            tape,
            decisions,
            index: Arc::new(TapeIndex::new(records.into())),
            item_of,
            walls: wall_times(tape),
            header,
        }
    }

    /// The ordinal of the effect at item `item`.
    pub(crate) fn ordinal_of(&self, item: usize) -> Option<u64> {
        self.item_of.binary_search(&item).ok().map(|o| o as u64)
    }

    /// The header's `key=` line as written, where the header does not read.
    fn written(&self, key: &str) -> Option<String> {
        self.tape.header.as_deref().and_then(|text| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_string)
        })
    }

    /// Explain decision `d` under `policy` (or the header's policy when `None`).
    pub(crate) fn explain(
        &self,
        d: &show::Decision,
        at_local: Option<String>,
        bytes: &[u8],
        policy: Option<&SupervisorConfig>,
    ) -> Why {
        let mark_ordinal = self.ordinal_of(d.item).unwrap_or(0);
        let items = &self.tape.items;
        let mark = match items[d.item].record() {
            Some(EffectRecord::Emit { bytes, .. }) => {
                std::str::from_utf8(bytes).ok().and_then(DecideMark::decode)
            }
            _ => None,
        };
        let next = self
            .decisions
            .iter()
            .find(|n| n.item > d.item)
            .map_or(items.len(), |n| n.item);
        let ledger = items[d.item + 1..next]
            .iter()
            .find_map(|item| match item.record() {
                Some(EffectRecord::Emit { door, bytes }) if door == doors::LEDGER => {
                    Some(String::from_utf8_lossy(bytes).trim_end().to_string())
                }
                _ => None,
            });
        let previous = self
            .decisions
            .iter()
            .rev()
            .find(|p| p.item < d.item)
            .map_or(0, |p| p.item);
        let notes: Vec<String> = items[previous..d.item]
            .iter()
            .filter_map(|item| match item {
                Item::Note { kind, text } if kind == NOTE_TURN_END => Some(text.clone()),
                _ => None,
            })
            .collect();
        let header = self.header.as_ref();
        let mut why = Why {
            tape: header.map_or_else(
                |_| self.written("tape").unwrap_or_else(|| "-".to_string()),
                |h| h.tape.clone(),
            ),
            recorded_by: header.map_or_else(|_| self.written("build"), |h| Some(h.build.clone())),
            explained_by: aterm_types::version::build_id(),
            door: d.door.clone(),
            mark: mark_ordinal,
            at_ms: self.walls[d.item],
            at_local,
            seq: d.seq,
            screen_record: d.screen_record,
            screen: d.screen.clone(),
            parsed: None,
            facts: Vec::new(),
            needs: Vec::new(),
            recorded_rule: d.rule.clone(),
            recorded_outcome: d.outcome.clone(),
            decided: None,
            agrees: None,
            trace: None,
            trace_note: None,
            act: d.act.clone(),
            reply: d.reply.clone(),
            journal: d.journal.clone(),
            ledger,
            notes,
        };
        let header = match header {
            Ok(header) => header,
            Err(note) => {
                why.trace_note = Some(note.clone());
                return why;
            }
        };
        if d.door == doors::DECIDE_TURN_END {
            turn_end_trace(&mut why, bytes, header);
        } else {
            let screen = d.screen_record.and_then(|n| {
                let item = self.item_of.get(usize::try_from(n).ok()?).copied()?;
                Some((screen_of(&items[item])?, n))
            });
            let facts = mark.as_ref().and_then(|m| m.facts.as_deref());
            box_trace(
                &mut why,
                header,
                policy.unwrap_or(&header.opts.policy),
                screen.as_ref().map(|(s, n)| (s, *n)),
                facts,
                &self.index,
                mark_ordinal,
            );
        }
        why
    }
}

/// `at` in words, for a refusal.
fn at_words(at: &At) -> String {
    match at {
        At::Seq(n) => format!("seq={n}"),
        At::Door(n) => format!("door={n}"),
        At::Time { day: None, secs } => clock(i64::from(*secs)),
        At::Time {
            day: Some(day),
            secs,
        } => local_words(day * 86_400 + i64::from(*secs)),
        At::Line(t) => format!("the line {t:?}"),
    }
}

/// The screen a control round trip's reply carries, whole.
fn screen_of(item: &Item) -> Option<Screen> {
    let EffectRecord::Exchange { door, reply, .. } = item.record()? else {
        return None;
    };
    if door != doors::CTL {
        return None;
    }
    let reply = <Result<CtlReply, String> as TapeValue>::from_tape(reply)
        .ok()?
        .ok()?;
    parse_text_json(&reply.stdout).ok()
}

/// An approval's or a question's decision made again from its mark's facts
/// ([module docs](self)), into `why`.
fn box_trace(
    why: &mut Why,
    header: &Header,
    policy: &SupervisorConfig,
    screen: Option<(&Screen, u64)>,
    facts: Option<&str>,
    index: &Arc<TapeIndex>,
    mark: u64,
) {
    let records = index.records();
    let Some((screen, screen_record)) = screen else {
        why.trace_note = Some("the judged screen is not on the tape".to_string());
        return;
    };
    let Some(facts) = facts else {
        why.trace_note = Some(
            if why.recorded_outcome.starts_with("Escalate") {
                "the mark carries no facts: the loop handed the box over before it read the facts \
                 the policy decides on, for the reason recorded — or a build before the facts \
                 recorded the mark"
            } else {
                "the mark carries no facts: a build before the facts recorded it"
            }
            .to_string(),
        );
        return;
    };
    let (facts, needs) = match BoxFacts::from_json(facts, &|raw| header.roots.decode(raw)) {
        Ok(read) => read,
        Err(e) => {
            why.trace_note = Some(format!("the mark's facts do not read: {e}"));
            return;
        }
    };
    why.needs.extend(needs.iter().map(|n| format!("fact {n}")));
    why.needs.extend(supplied(header));
    why.facts = facts_with_doors(&facts, header, records, mark);
    let cut = aterm_phase::read(
        facts.program.as_deref(),
        &screen.rows,
        Some(screen.cursor_col),
    );
    let head = head_of(records, screen_record, mark, screen, &cut);
    match &head {
        Head::Joined {
            rows,
            search,
            lines,
            reread,
            ..
        } => why.facts.push(Fact {
            name: "box_head",
            value: format!("{} rows above the screen", rows.len() - screen.rows.len()),
            door: format!(
                "ctl search #{search}, ctl line {}, the screen read again (ctl text #{reread})",
                lines.map_or_else(|| "-".to_string(), |(a, b)| format!("#{a}..#{b}"))
            ),
        }),
        Head::Unknown { search, why: gap } => why.needs.push(format!(
            "the box's head off the screen, which the loop searched the history for (ctl search \
             #{search}): whether it joined the head the tape does not say ({gap}); decided again \
             on the screen's rows alone"
        )),
        Head::NotRead | Head::Cut => {}
    }
    let reading = match &head {
        Head::Joined { reading, .. } => reading.as_ref(),
        _ => &cut,
    };
    why.parsed = reading.prompt.as_ref().map(|p| BoxRead {
        program: reading.program.name().to_string(),
        kind: p.kind.name().to_string(),
        title: p.title.clone(),
        options: p
            .options
            .iter()
            .map(|o| {
                format!(
                    "{}{}{}",
                    if o.focused { "❯ " } else { "" },
                    o.n.map_or_else(String::new, |n| format!("{n}. ")),
                    o.label
                )
            })
            .collect(),
    });
    let seen = usize::try_from(mark).unwrap_or(usize::MAX);
    let probe = Arc::new(KeyedProbe::at(
        Arc::clone(index),
        seen,
        header.roots.clone(),
        digest_seed(&header.tape),
    ));
    let ctx = ApprovalCtx::from(
        policy,
        header.setup.approval_env(),
        &header.opts.allow(),
        &facts,
        probe.clone(),
    );
    let mut steps = Vec::new();
    let decision = match &head {
        Head::Joined { rows, reading, .. } => {
            let whole = decide_traced(reading, rows, &ctx, &mut steps);
            let mut held = None;
            let decision = whole_box_decision(whole, rows, &screen.rows, || {
                let mut cut_steps = Vec::new();
                let d = decide_traced(&cut, &screen.rows, &ctx, &mut cut_steps);
                held = Some(cut_steps);
                d
            });
            if let Some(cut_steps) = held {
                steps.push(TraceStep {
                    check: "whole-box",
                    rule: None,
                    outcome: Outcome::Overruled(
                        "an answer of the whole box a joined read does not carry out: the cut's \
                         own decision stands"
                            .to_string(),
                    ),
                });
                steps.extend(cut_steps);
            }
            decision
        }
        _ => decide_traced(&cut, &screen.rows, &ctx, &mut steps),
    };
    let decided = format!("{decision:?}");
    let missing_looks = probe.misses();
    why.agrees = missing_looks
        .is_empty()
        .then_some(decided == why.recorded_outcome);
    why.decided = Some(decided);
    why.needs
        .extend(missing_looks.iter().map(|m| format!("look {m}")));
    why.trace = Some(steps);
    let under = if std::ptr::eq(policy, &header.opts.policy) {
        if header.supplied.is_empty() {
            "the policy the header carries"
        } else {
            "the policy the header carries with what this build supplied in place of what it \
             does not say (under needs)"
        }
    } else {
        "a CANDIDATE policy in place of the header's"
    };
    why.trace_note = Some(if missing_looks.is_empty() {
        format!(
            "decided again by this build's policy ({}) on the facts the mark carries, {under} and \
             the looks the tape holds before #{mark}",
            why.explained_by,
        )
    } else {
        format!(
            "decided again by this build's policy ({}) on the facts the mark carries and {under}, \
             but it needs a look the tape does not hold before #{mark} (under needs): the \
             cautious answer it takes without one does not say what it decides",
            why.explained_by,
        )
    });
}

/// What a header this build decoded does not give the run's own, each
/// supplied by this build (`Header::supplied`: a key a build before it did
/// not write, a policy value an inexact header's reader refused, the
/// inexactness itself), as a need of a decision made again under it.
fn supplied(header: &Header) -> impl Iterator<Item = String> + '_ {
    header
        .supplied
        .iter()
        .map(|s| format!("supplied by this build: {s}"))
}

/// What the loop made of a box whose head is off the screen
/// (`run::box_head`), as the tape's replies say ([`head_of`]).
enum Head {
    /// No history searched: the box was decided on the screen's rows.
    NotRead,
    /// Searched, and the join did not prove itself: decided on the cut.
    Cut,
    /// Joined: the rows from the box's top rule to the screen's foot and
    /// their reading, and the records the join came through (the search, the
    /// first and last `line` read, the screen read again).
    Joined {
        rows: Vec<String>,
        reading: Box<aterm_phase::Reading>,
        search: u64,
        lines: Option<(u64, u64)>,
        reread: u64,
    },
    /// Searched, and the tape does not hold a read the join was made on.
    Unknown { search: u64, why: String },
}

/// The loop's join of a box's head (`Session::box_from_history`) rebuilt
/// from the control round trips strictly between the judged screen's record
/// and the mark: the search for the box's top rule, a `line` read of each
/// row it names, the screen read again by the request that read the judged
/// one — each reply read by the loop's own reader of it
/// (`run::head_rows`, `run::head_row`, `run::joined_box`), so the join is
/// the loop's, never a second copy of its rule.
fn head_of(
    records: &[EffectRecord],
    screen_record: u64,
    mark: u64,
    screen: &Screen,
    cut: &aterm_phase::Reading,
) -> Head {
    let Some(prompt) = cut
        .prompt
        .as_ref()
        .filter(|p| p.head_off_screen && p.kind != aterm_phase::PromptKind::Question)
    else {
        return Head::NotRead;
    };
    let words_of = |request: &[u8]| -> Vec<String> {
        codec::ctl_args(request)
            .map(|w| w.into_iter().filter(|w| !w.starts_with('@')).collect())
            .unwrap_or_default()
    };
    let Some(EffectRecord::Exchange {
        request: screen_request,
        ..
    }) = usize::try_from(screen_record)
        .ok()
        .and_then(|n| records.get(n))
    else {
        return Head::NotRead;
    };
    let screen_request = words_of(screen_request);
    let from = usize::try_from(screen_record).map_or(usize::MAX, |n| n.saturating_add(1));
    let to = records
        .len()
        .min(usize::try_from(mark).unwrap_or(usize::MAX));
    let lane: Vec<(u64, Vec<String>, Option<CtlReply>)> = records
        .get(from..to)
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            EffectRecord::Exchange {
                door,
                request,
                reply,
            } if door == doors::CTL => Some((
                (from + i) as u64,
                words_of(request),
                <Result<CtlReply, String> as TapeValue>::from_tape(reply)
                    .ok()
                    .and_then(Result::ok),
            )),
            _ => None,
        })
        .collect();
    let Some(at) = lane.iter().rposition(|(_, w, _)| *w == HEAD_SEARCH) else {
        return Head::NotRead;
    };
    let search = lane[at].0;
    let unknown = |why: String| Head::Unknown { search, why };
    let Some(reply) = &lane[at].2 else {
        return unknown("its reply does not read".to_string());
    };
    let Some(rows) = head_rows(reply) else {
        return Head::Cut;
    };
    let mut head = Vec::new();
    let mut next = at + 1;
    let mut lines = None;
    for row in rows {
        let Some(k) = lane[next..]
            .iter()
            .position(|(_, w, _)| w.first().map(String::as_str) == Some("line"))
            .map(|k| next + k)
        else {
            return unknown(format!("no read of the history's row {row}"));
        };
        let (n, words, reply) = &lane[k];
        if words.get(1).map(String::as_str) != Some(row.to_string().as_str()) {
            return unknown(format!("the read #{n} is not of the history's row {row}"));
        }
        let Some(reply) = reply else {
            return unknown(format!("the reply of #{n} does not read"));
        };
        let Some(text) = head_row(reply) else {
            return Head::Cut;
        };
        head.push(text);
        lines = Some((lines.map_or(*n, |(a, _)| a), *n));
        next = k + 1;
    }
    let Some((reread, _, again)) = lane[next..].iter().find(|(_, w, _)| *w == screen_request)
    else {
        return unknown("no read of the screen after the head".to_string());
    };
    let Some(now) = again
        .as_ref()
        .filter(|r| r.ok())
        .and_then(|r| parse_text_json(&r.stdout).ok())
    else {
        return unknown(format!("the screen read again (#{reread}) does not read"));
    };
    if !same_box(&now.rows, &screen.rows) {
        return Head::Cut;
    }
    match joined_box(&head, &screen.rows, prompt, screen.cursor_col) {
        Some((rows, reading)) => Head::Joined {
            rows,
            reading: Box::new(reading),
            search,
            lines,
            reread: *reread,
        },
        None => Head::Cut,
    }
}

/// A turn end's trace, from a strict replay of the tape to its mark on the
/// build that recorded it ([module docs](self)), into `why`.
fn turn_end_trace(why: &mut Why, bytes: &[u8], header: &Header) {
    if header.build != why.explained_by {
        why.trace_note = Some(format!(
            "recorded by {} and explained by {}: a turn end decides on a fold over the whole run, \
             which only a strict replay rebuilds, and only the recording build replays its tape \
             (the fold and reading the recorder noted are below)",
            header.build, why.explained_by
        ));
        return;
    }
    let options = Options {
        until: Some(why.mark),
        trace_at: Some(why.mark),
        ..Options::default()
    };
    match replay_traced(bytes, &options) {
        Ok((_, Some(steps))) => {
            why.trace = Some(steps);
            why.needs.extend(supplied(header));
            why.trace_note = Some(format!(
                "decided again in a strict replay of the tape to #{}, on the fold the replay \
                 rebuilt{}",
                why.mark,
                if header.supplied.is_empty() {
                    ""
                } else {
                    ", under the policy the header carries with what this build supplied in \
                     place of what it does not say (under needs)"
                }
            ));
        }
        Ok((report, None)) => {
            why.trace_note = Some(format!(
                "the replay did not reach the decision: {}",
                report.render().lines().next().unwrap_or("")
            ));
        }
        Err(e) => why.trace_note = Some(format!("the tape does not replay: {e}")),
    }
}

/// The latest record at or before `mark` that crossed `door` (a control
/// request's first word naming the verb, for the control lane): its ordinal.
fn last_at(records: &[EffectRecord], mark: u64, door: &str, verb: Option<&str>) -> Option<u64> {
    let upto = records
        .len()
        .min(usize::try_from(mark).unwrap_or(usize::MAX));
    records[..upto]
        .iter()
        .rposition(|r| match r {
            EffectRecord::Exchange {
                door: d, request, ..
            } if d == door => verb.is_none_or(|v| {
                codec::ctl_args(request).is_ok_and(|w| {
                    w.iter()
                        .find(|w| !w.starts_with('@'))
                        .is_some_and(|w| w == v)
                })
            }),
            _ => false,
        })
        .map(|o| o as u64)
}

/// `facts`, each with the door it came through ([`Fact::door`]): the latest
/// read of it among the first `mark` records, or the loop's own memory.
fn facts_with_doors(
    facts: &BoxFacts,
    header: &Header,
    records: &[EffectRecord],
    mark: u64,
) -> Vec<Fact> {
    let at = |door: &str, verb: Option<&str>| {
        let name = verb.map_or_else(|| door.to_string(), |v| format!("{door} {v}"));
        last_at(records, mark, door, verb).map_or_else(
            || format!("{name} (no such read on the tape before the decision)"),
            |n| format!("{name} #{n}"),
        )
    };
    let host = |method: &str| at(&format!("{}{method}", doors::HOST), None);
    let memory = |what: &str| format!("the loop's memory ({what})");
    let answer = |r: Result<String, &String>| match r {
        Ok(v) => v,
        Err(why) => format!("unreadable: {why}"),
    };
    let path = |p: Option<&std::path::Path>| {
        p.map_or_else(|| "unknown".to_string(), |p| p.display().to_string())
    };
    let mut out = vec![
        Fact {
            name: "program",
            value: facts
                .program
                .clone()
                .unwrap_or_else(|| "- (the screen names its reader)".to_string()),
            door: at(doors::CTL, Some("status")),
        },
        Fact {
            name: "cwd",
            value: path(facts.cwd.as_deref()),
            door: at(doors::CTL, Some("meta")),
        },
        Fact {
            name: "footer",
            value: if facts.bypass { "bypass" } else { "not bypass" }.to_string(),
            door: memory("the footer last read"),
        },
        Fact {
            name: "screen",
            value: format!("{} cols × {} rows", facts.screen_cols, facts.screen_rows),
            door: at(doors::CTL, Some("text")),
        },
        Fact {
            name: "limit_reset_passed",
            value: facts.limit_reset_passed.to_string(),
            door: memory("this dialog's mount"),
        },
        Fact {
            name: "limit_retry",
            value: facts.limit_retry.to_string(),
            door: memory("the reset the dialog names, past its grace"),
        },
    ];
    if let Some(trusted) = &facts.trusted_cwd {
        out.push(Fact {
            name: "trusted_cwd",
            value: trusted.display().to_string(),
            door: at(doors::CTL, Some("meta")),
        });
    }
    if let Some(worker) = &facts.worker {
        let fixed = matches!(header.setup.approval_env().worker, WorkerSource::Fixed(_));
        out.push(Fact {
            name: "worker",
            value: answer(worker.as_ref().map(|w| {
                format!(
                    "digest {:016x}{}",
                    w.digest(0),
                    w.pid().map_or_else(String::new, |p| format!(" pid {p}"))
                )
            })),
            door: if fixed {
                "the run's fixed worker (its setup)".to_string()
            } else {
                at(doors::PROBE_WORKER_PROC, None)
            },
        });
    }
    if !facts.shell_cwds.is_empty() {
        out.push(Fact {
            name: "shell_cwds",
            value: facts
                .shell_cwds
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
            door: at(doors::PROBE_SHELL_CWDS, None),
        });
    }
    if let Some(shell) = &facts.shell {
        out.push(Fact {
            name: "shell",
            value: answer(shell.as_ref().map(|s| {
                let names: Vec<String> = s
                    .names()
                    .iter()
                    .map(|(kind, name, _)| format!("{kind} {name}"))
                    .collect();
                if names.is_empty() {
                    "the modelled defaults".to_string()
                } else {
                    names.join(", ")
                }
            })),
            door: at(doors::PROBE_SHELL_STARTUP, None),
        });
    }
    if let Some(asks) = &facts.owner_asks {
        out.push(Fact {
            name: "owner_asks",
            value: answer(asks.as_ref().map(|a| {
                if a.is_empty() {
                    "none".to_string()
                } else {
                    a.iter()
                        .map(|a| a.text.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            })),
            door: at(doors::PROBE_OWNER_ASKS, None),
        });
    }
    if let Some(q) = facts.question {
        out.push(Fact {
            name: "question",
            value: if q.answer {
                format!(
                    "answered by {}{}{}",
                    q.priority.word(),
                    q.own.map_or_else(String::new, |o| format!(
                        "; the loop wrote {}'s text",
                        o.word()
                    )),
                    if q.fell_back { "; fell back" } else { "" }
                )
            } else {
                "handed to a person".to_string()
            },
            door: format!(
                "{} and {}",
                at(doors::CTL, Some("meta")),
                memory("the dialog")
            ),
        });
    }
    if let Some(call) = &facts.box_call {
        out.push(Fact {
            name: "box_call",
            value: call.clone(),
            door: host("transcript_tail"),
        });
    }
    if facts.login_back {
        out.push(Fact {
            name: "login_back",
            value: "the login came back in another tab".to_string(),
            door: host("login_shared"),
        });
    }
    if facts.login_carry {
        out.push(Fact {
            name: "login_carry",
            value: facts
                .login_method
                .map_or_else(|| "the method unread".to_string(), |m| m.word().to_string()),
            door: host("login_method"),
        });
    }
    out
}

/// `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Text,
    Md,
    Html,
}

impl Format {
    /// `text`, `md` (`markdown`) or `html`.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "text" => Some(Self::Text),
            "md" | "markdown" => Some(Self::Md),
            "html" => Some(Self::Html),
            _ => None,
        }
    }
}

/// `why` as `format`.
#[must_use]
pub fn render(why: &Why, format: Format) -> String {
    match format {
        Format::Text => render_text(why),
        Format::Md => render_md(why),
        Format::Html => render_html(why),
    }
}

/// The heading line: which decision, when, by which builds.
fn heading(why: &Why) -> String {
    let made = why.at_ms.map_or_else(String::new, |ms| {
        let utc = utc_words(ms.div_euclid(1000));
        match &why.at_local {
            Some(local) => format!(" made {utc} ({local} local)"),
            None => format!(" made {utc}"),
        }
    });
    format!(
        "{} at #{} (seq {}){made} on tape {} · recorded by {} · explained by {}",
        why.door,
        why.mark,
        why.seq,
        why.tape,
        why.recorded_by.as_deref().unwrap_or("-"),
        why.explained_by
    )
}

/// The verdict line: what was decided, by which rule, and whether this build
/// agrees.
fn verdict(why: &Why) -> String {
    let mut out = format!(
        "recorded: rule={} {}",
        why.recorded_rule.as_deref().unwrap_or("-"),
        why.recorded_outcome
    );
    if let Some(by) = why.decided_by() {
        out.push_str(&format!("\ndecided by: {by}"));
    }
    match why.agrees {
        Some(true) => out.push_str("\nthis build's policy decides the same"),
        Some(false) => out.push_str(&format!(
            "\nthis build's policy decides otherwise (a change since, a need below, or a \
             fence the loop puts after the policy): {}",
            why.decided.as_deref().unwrap_or("-")
        )),
        None => {}
    }
    out
}

/// The text form: what `aterm drive replay --why` prints by default.
#[must_use]
pub fn render_text(why: &Why) -> String {
    let mut out = format!("why: {}\n", shown(&heading(why)));
    for line in verdict(why).lines() {
        out.push_str(&format!("  {}\n", shown(line)));
    }
    match &why.screen {
        Some(rows) => {
            out.push_str(&format!(
                "screen (record #{}):\n",
                why.screen_record.unwrap_or(0)
            ));
            for row in rows {
                out.push_str(&format!("    │ {}\n", shown(row)));
            }
        }
        None => out.push_str("screen: not on the tape\n"),
    }
    if let Some(p) = &why.parsed {
        out.push_str(&format!(
            "box ({} reader): {} — {}\n",
            shown(&p.program),
            shown(&p.kind),
            shown(&p.title)
        ));
        for o in &p.options {
            out.push_str(&format!("    {}\n", shown(o)));
        }
    }
    if !why.facts.is_empty() {
        out.push_str("facts:\n");
        for f in &why.facts {
            out.push_str(&format!(
                "  {} = {}  ← {}\n",
                f.name,
                shown(&f.value),
                shown(&f.door)
            ));
        }
    }
    if !why.needs.is_empty() {
        out.push_str("needs:\n");
        for n in &why.needs {
            out.push_str(&format!("  {}\n", shown(n)));
        }
    }
    match &why.trace {
        Some(steps) => {
            out.push_str("trace:\n");
            out.push_str(&shown_lines(&trace::render(steps, "  ")));
        }
        None => out.push_str("trace: none\n"),
    }
    if let Some(note) = &why.trace_note {
        out.push_str(&format!("  ({})\n", shown(note)));
    }
    out.push_str(&format!(
        "act: {}\nreply: {}\njournal: {}\nledger: {}\n",
        why.act
            .as_ref()
            .map_or_else(|| "none".to_string(), |w| shown(&w.join(" "))),
        shown(why.reply.as_deref().unwrap_or("-")),
        shown(why.journal.as_deref().unwrap_or("-")),
        shown(why.ledger.as_deref().unwrap_or("-")),
    ));
    for note in &why.notes {
        out.push_str("turn-end note:\n");
        for line in note.lines() {
            out.push_str(&format!("    {}\n", shown(line)));
        }
    }
    out
}

/// Each line of `text` printed as [`shown`] writes it, its newline kept.
fn shown_lines(text: &str) -> String {
    text.lines().map(|l| format!("{}\n", shown(l))).collect()
}

/// A Markdown cell or line: one line, `|` and the backslash escaped.
fn md(text: &str) -> String {
    shown(text).replace('\\', "\\\\").replace('|', "\\|")
}

/// The Markdown form.
#[must_use]
pub fn render_md(why: &Why) -> String {
    let mut out = format!("# Why — {}\n\n", md(&heading(why)));
    for line in verdict(why).lines() {
        out.push_str(&format!("- {}\n", md(line)));
    }
    out.push_str("\n## Screen\n\n```text\n");
    match &why.screen {
        Some(rows) => {
            for row in rows {
                out.push_str(&shown(row).replace("```", "`\u{200b}``"));
                out.push('\n');
            }
        }
        None => out.push_str("(not on the tape)\n"),
    }
    out.push_str("```\n");
    if let Some(p) = &why.parsed {
        out.push_str(&format!(
            "\n**Box** ({} reader): {} — {}\n\n",
            md(&p.program),
            md(&p.kind),
            md(&p.title)
        ));
        for o in &p.options {
            out.push_str(&format!("- {}\n", md(o)));
        }
    }
    if !why.facts.is_empty() {
        out.push_str("\n## Facts\n\n| fact | value | from |\n|---|---|---|\n");
        for f in &why.facts {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                f.name,
                md(&f.value),
                md(&f.door)
            ));
        }
    }
    if !why.needs.is_empty() {
        out.push_str("\n## Needs\n\n");
        for n in &why.needs {
            out.push_str(&format!("- {}\n", md(n)));
        }
    }
    out.push_str("\n## Trace\n\n");
    match &why.trace {
        Some(steps) if !steps.is_empty() => {
            out.push_str("| step | check | outcome |\n|---|---|---|\n");
            for s in steps {
                out.push_str(&format!(
                    "| {} | {} | {} |\n",
                    md(s.name()),
                    md(s.check),
                    md(&outcome_words(&s.outcome))
                ));
            }
        }
        _ => out.push_str("None.\n"),
    }
    if let Some(note) = &why.trace_note {
        out.push_str(&format!("\n_{}_\n", md(note)));
    }
    out.push_str(&format!(
        "\n## After\n\n- act: {}\n- reply: {}\n- journal: {}\n- ledger: {}\n",
        md(&why
            .act
            .as_ref()
            .map_or_else(|| "none".to_string(), |w| w.join(" "))),
        md(why.reply.as_deref().unwrap_or("-")),
        md(why.journal.as_deref().unwrap_or("-")),
        md(why.ledger.as_deref().unwrap_or("-")),
    ));
    out
}

/// A step's outcome in words.
fn outcome_words(o: &trace::Outcome) -> String {
    match o {
        trace::Outcome::Passed => "passed".to_string(),
        trace::Outcome::Unproven(why) => format!("unproven — {why}"),
        trace::Outcome::Decided(what) => format!("decided — {what}"),
        trace::Outcome::Overruled(why) => format!("overruled — {why}"),
    }
}

/// Text for HTML.
fn html(text: &str) -> String {
    crate::supervise::ledger::html_text(text)
}

/// The HTML form: one self-contained page, no script, no fetch.
#[must_use]
pub fn render_html(why: &Why) -> String {
    let mut out = String::from(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; \
         style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\n",
    );
    out.push_str(&format!(
        "<title>{}</title>\n",
        html(&format!("why — {}", why.door))
    ));
    out.push_str(
        "<style>\nbody{font:14px/1.45 ui-sans-serif,-apple-system,Helvetica,Arial,sans-serif;\
         margin:0 auto;max-width:1100px;padding:24px 16px;background:#f9f9f7;color:#0b0b0b}\
         \n@media (prefers-color-scheme: dark){body{background:#0d0d0d;color:#fff}}\
         \npre{background:rgba(127,127,127,.12);padding:8px;overflow-x:auto}\
         \ntable{border-collapse:collapse;width:100%}td,th{text-align:left;vertical-align:top;\
         padding:3px 8px 3px 0;border-bottom:1px solid rgba(127,127,127,.3)}\n</style>\n</head>\n<body>\n",
    );
    out.push_str(&format!(
        "<h1>Why</h1>\n<p>{}</p>\n<ul>\n",
        html(&heading(why))
    ));
    for line in verdict(why).lines() {
        out.push_str(&format!("<li>{}</li>\n", html(line)));
    }
    out.push_str("</ul>\n<h2>Screen</h2>\n<pre>");
    match &why.screen {
        Some(rows) => {
            for row in rows {
                out.push_str(&html(&shown(row)));
                out.push('\n');
            }
        }
        None => out.push_str("(not on the tape)\n"),
    }
    out.push_str("</pre>\n");
    if !why.facts.is_empty() {
        out.push_str(
            "<h2>Facts</h2>\n<table>\n<tr><th>fact</th><th>value</th><th>from</th></tr>\n",
        );
        for f in &why.facts {
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                html(f.name),
                html(&shown(&f.value)),
                html(&shown(&f.door))
            ));
        }
        out.push_str("</table>\n");
    }
    if !why.needs.is_empty() {
        out.push_str("<h2>Needs</h2>\n<ul>\n");
        for n in &why.needs {
            out.push_str(&format!("<li>{}</li>\n", html(&shown(n))));
        }
        out.push_str("</ul>\n");
    }
    out.push_str("<h2>Trace</h2>\n");
    match &why.trace {
        Some(steps) if !steps.is_empty() => {
            out.push_str("<table>\n<tr><th>step</th><th>check</th><th>outcome</th></tr>\n");
            for s in steps {
                out.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                    html(s.name()),
                    html(s.check),
                    html(&shown(&outcome_words(&s.outcome)))
                ));
            }
            out.push_str("</table>\n");
        }
        _ => out.push_str("<p>None.</p>\n"),
    }
    if let Some(note) = &why.trace_note {
        out.push_str(&format!("<p><i>{}</i></p>\n", html(&shown(note))));
    }
    out.push_str(&format!(
        "<h2>After</h2>\n<ul>\n<li>act: {}</li>\n<li>reply: {}</li>\n<li>journal: {}</li>\n\
         <li>ledger: {}</li>\n</ul>\n",
        html(&shown(
            &why.act
                .as_ref()
                .map_or_else(|| "none".to_string(), |w| w.join(" "))
        )),
        html(&shown(why.reply.as_deref().unwrap_or("-"))),
        html(&shown(why.journal.as_deref().unwrap_or("-"))),
        html(&shown(why.ledger.as_deref().unwrap_or("-"))),
    ));
    for note in &why.notes {
        out.push_str(&format!(
            "<details><summary>turn-end note</summary><pre>{}</pre></details>\n",
            html(&shown_lines(note))
        ));
    }
    out.push_str("</body>\n</html>\n");
    out
}

#[cfg(test)]
#[path = "explain_tests.rs"]
mod tests;
