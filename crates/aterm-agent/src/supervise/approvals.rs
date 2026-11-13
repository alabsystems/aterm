// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The approval ledger: one JSON line per box the approval policy decided ([`super::policy::approval::decide`]) and per act of the turn-end policy, for every `watch`, `supervise` and hosted run, at `<aterm state>/drive/<sid>.jsonl` ([`default_path`]).

use std::io::Write;
use std::path::{Path, PathBuf};

use super::journal::{Journal, json_str};
use super::policy::sleeps::{Boot, SleepStamp};
use super::policy::turn_end::{
    Conversation, Kept, LandBooks, LimitTry, OffStood, Replies, WallPass,
};
use crate::harness::profile::AccountKey;

/// How much of a command a row keeps.
pub const COMMAND_BYTES: usize = 4096;

/// What every rule of the facts seam opens with (`fact-hold@v1`, `fact-would@v1`,
/// `fact-intent@v1`, `fact-held@v1`, `fact-wake@v1`; [`super::facts`]).
pub const FACT_RULE_PREFIX: &str = "fact-";

/// A FACT ROW IS NO ACT (the occupancy red-team's F13): a line the facts seam typed
/// (`fact-hold@v1` `typed`) is told the agent beside its own work, never a policy act, so no
/// reader of acts counts it: no short streak, no `continue_per_hour`, no landing, no try, no
/// person's exchange ended. Whether `rule` is such a rule.
#[must_use]
pub fn is_fact_rule(rule: &str) -> bool {
    !facts_counted() && rule.starts_with(FACT_RULE_PREFIX)
}

/// Whether the ledger line `line` is a fact row ([`is_fact_rule`]): its `rule_id` field opens
/// with [`FACT_RULE_PREFIX`]. The pattern spans the field's own quotes, which [`json_str`]
/// escapes inside every value, so no command, reason or sid forges it.
#[must_use]
pub fn is_fact_line(line: &str) -> bool {
    !facts_counted() && line.contains(",\"rule_id\":\"fact-")
}

/// The ledger text `body`'s lines but its fact rows ([`is_fact_line`]): what every reader of
/// acts walks.
fn act_lines(body: &str) -> impl DoubleEndedIterator<Item = &str> {
    body.lines().filter(|l| !is_fact_line(l))
}

#[cfg(test)]
thread_local! {
    /// The negative control of [`is_fact_line`]: fact rows read as acts on this thread.
    static FACTS_COUNTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether fact rows read as acts on this thread (tests only: [`with_facts_counted`]).
#[cfg(test)]
fn facts_counted() -> bool {
    FACTS_COUNTED.with(std::cell::Cell::get)
}

/// The shipped build: never.
#[cfg(not(test))]
const fn facts_counted() -> bool {
    false
}

/// Run `f` with fact rows read as acts on this thread (the readers' negative control), undone
/// after it even on unwind.
#[cfg(test)]
pub(crate) fn with_facts_counted<T>(f: impl FnOnce() -> T) -> T {
    struct Undo;
    impl Drop for Undo {
        fn drop(&mut self) {
            FACTS_COUNTED.with(|c| c.set(false));
        }
    }
    FACTS_COUNTED.with(|c| c.set(true));
    let _undo = Undo;
    f()
}

/// What a row says was done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Approved,
    /// A box refused with a reason: the decline's Enter landed.
    Declined,
    /// The turn-end policy typed its act (a continuation, a command).
    Typed,
    Skipped,
    Escalated,
    /// The server turned the press away for now; it is tried again.
    Deferred,
    /// A record, no act or press: a wall waited on or its end, a probe panel seen closed, a reset step ended, an account move answered.
    Waiting,
}

impl Outcome {
    /// Every outcome, in the declaration's order.
    pub const ALL: [Outcome; 7] = [
        Outcome::Approved,
        Outcome::Declined,
        Outcome::Typed,
        Outcome::Skipped,
        Outcome::Escalated,
        Outcome::Deferred,
        Outcome::Waiting,
    ];

    /// The row's `decision` word ([`Self::from_word`] reads it back).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Outcome::Approved => "approved",
            Outcome::Declined => "declined",
            Outcome::Typed => "typed",
            Outcome::Skipped => "skipped",
            Outcome::Escalated => "escalated",
            Outcome::Deferred => "deferred",
            Outcome::Waiting => "waiting",
        }
    }

    /// The outcome a row's `decision` word names, by [`Self::word`]'s own words, so a reader of the ledger (`harness::stats`) cannot drift from its writer; `None` for a self-claim's row ([`SELF_CLAIM_DECISION`]) and for a word this build does not write.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.word() == word)
    }
}

/// One ledger row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row<'a> {
    pub rule_id: &'a str,
    pub outcome: Outcome,
    pub command: &'a str,
    pub reason: &'a str,
    pub box_seq: u64,
}

impl Row<'_> {
    /// The row as one JSON object, `ts` and `sid` given.
    pub fn to_json(&self, ts: i64, sid: Option<&str>) -> String {
        self.to_json_counted(ts, sid, None)
    }

    /// [`Self::to_json`] with the machine's count of its sleep at the row's stamp (`count`, [`SLEPT_MS`] and [`BOOT`]: an act's row, which a loop that follows sets its own count against, [`last_act_count_in`]); none where the count names no boot, which compares with no other process's.
    pub fn to_json_counted(
        &self,
        ts: i64,
        sid: Option<&str>,
        count: Option<&SleepStamp>,
    ) -> String {
        self.to_json_counted_with_boot_text(ts, sid, count, None)
    }

    /// A wall's optional top-level identity.
    #[cfg(test)]
    pub(crate) fn to_json_wall_wait(
        &self,
        ts: i64,
        sid: Option<&str>,
        conversation: &Conversation,
    ) -> String {
        self.to_json_scoped(ts, sid, None, None, conversation)
    }

    /// [`Self::to_json_wall_wait`] with the program the wall was met under ([`PROGRAM`]).
    #[cfg(test)]
    pub(crate) fn to_json_wall_wait_by(
        &self,
        ts: i64,
        sid: Option<&str>,
        conversation: &Conversation,
        program: Option<aterm_phase::Program>,
    ) -> String {
        self.to_json_scoped_by(ts, sid, None, None, conversation, program)
    }

    /// [`Self::to_json_scoped_by`] naming no program: a row as a loop wrote it before rows named the program their wall was met under.
    #[cfg(test)]
    pub(crate) fn to_json_scoped(
        &self,
        ts: i64,
        sid: Option<&str>,
        count: Option<&SleepStamp>,
        captured_boot: Option<&str>,
        conversation: &Conversation,
    ) -> String {
        self.to_json_scoped_by(ts, sid, count, captured_boot, conversation, None)
    }

    /// `Self::to_json_scoped`, with the wall's program or the program whose turns a point row's
    /// short streak counts ([`PROGRAM`]); a hostless row can name it too.
    pub(crate) fn to_json_scoped_by(
        &self,
        ts: i64,
        sid: Option<&str>,
        count: Option<&SleepStamp>,
        captured_boot: Option<&str>,
        conversation: &Conversation,
        program: Option<aterm_phase::Program>,
    ) -> String {
        let mut line = self.to_json_counted_with_boot_text(ts, sid, count, captured_boot);
        if *conversation != Conversation::Hostless {
            line.pop();
            let id = match conversation {
                Conversation::Named(id) => json_str(id),
                Conversation::Unknown | Conversation::Hostless => "null".to_owned(),
            };
            line.push_str(&format!(",\"conversation\":{id}}}"));
        }
        if let Some(program) = program {
            line.pop();
            line.push_str(&format!(",\"{PROGRAM}\":{}}}", json_str(program.name())));
        }
        line
    }

    fn to_json_counted_with_boot_text(
        &self,
        ts: i64,
        sid: Option<&str>,
        count: Option<&SleepStamp>,
        captured_boot: Option<&str>,
    ) -> String {
        let sid = sid.map_or_else(
            || "null".to_string(),
            |s| json_str(s.trim_start_matches('@')),
        );
        let counted = count
            .and_then(|c| Some((c.asleep.as_millis(), c.boot.as_ref()?)))
            .map_or_else(String::new, |(ms, boot)| {
                let text = captured_boot
                    .filter(|text| Boot::of(text).as_ref() == Some(boot))
                    .map_or_else(|| boot.text(), str::to_owned);
                format!(",\"{SLEPT_MS}\":{ms},\"{BOOT}\":{}", json_str(&text))
            });
        format!(
            "{{\"ts\":{ts},\"sid\":{sid},\"rule_id\":{},\"decision\":{},\"cmd_sha256\":{},\
             \"command\":{},\"reason\":{},\"box_seq\":{}{counted}}}",
            json_str(self.rule_id),
            json_str(self.outcome.word()),
            json_str(&sha256_hex(self.command)),
            json_str(&cut(self.command, COMMAND_BYTES)),
            json_str(self.reason),
            self.box_seq
        )
    }
}

pub const SLEPT_MS: &str = "slept_ms";

pub const PROGRAM: &str = "program";

pub const BOOT: &str = "boot";

/// The `decision` word of A PROVEN SELF-CLAIM'S ACT ([`SelfClaimRow`]): no [`Outcome`]'s word,
/// so no reader of aterm's own acts (`typed`, `approved`, `waiting`, …) reads the row as one of
/// its loop's, and a build before it reads a row it cannot place and counts no act of it.
pub const SELF_CLAIM_DECISION: &str = "self-claim";

/// The `rule_id` of every [`SelfClaimRow`]: one no rule of aterm's own loop is, so no reader
/// that keys on a rule reads the holder's act as its own; the holder's own rule is the row's
/// [`SELF_CLAIM_ACT`] field.
pub const RULE_SELF_CLAIM_ACT: &str = "self-claim-act@v1";

/// A [`SelfClaimRow`]'s field naming the holder of the proven claim.
pub const SELF_CLAIM_HOLDER: &str = "holder";

/// A [`SelfClaimRow`]'s field holding the act the holder recorded (`meta set harness_act
/// <act>`), as the session log's `harness_act=` holds it.
pub const SELF_CLAIM_ACT: &str = "harness_act";

/// A [`SelfClaimRow`]'s field holding the act's word ([`crate::harness::acts::self_claim_act`];
/// `unknown` for an act no word is), as the session log's `word=` holds it.
pub const SELF_CLAIM_WORD: &str = "act";

/// A PROVEN SELF-CLAIM'S ACT IN THE SESSION'S LEDGER: what an agent that supervises itself
/// (codex-alab) recorded on its proven claim with `meta set harness_act <act>`, booked beside
/// aterm's own rows in [`Row::to_json`]'s shape — its time, its session, a rule and a
/// `decision`, an empty command — under [`RULE_SELF_CLAIM_ACT`] and [`SELF_CLAIM_DECISION`],
/// then who held the claim, the act and its word. Written through [`Ledger`]'s one append, so a
/// line of it and a line of the loop's never interleave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfClaimRow<'a> {
    /// The claim's holder (`codex-alab@<pid>`).
    pub holder: &'a str,
    /// The act it recorded (`continue@v1`).
    pub act: &'a str,
}

impl SelfClaimRow<'_> {
    /// The row as one JSON object, `ts` and `sid` given.
    #[must_use]
    pub fn to_json(&self, ts: i64, sid: Option<&str>) -> String {
        let sid = sid.map_or_else(
            || "null".to_string(),
            |s| json_str(s.trim_start_matches('@')),
        );
        let word = crate::harness::acts::self_claim_act(self.act).map_or("unknown", |(w, _)| w);
        format!(
            "{{\"ts\":{ts},\"sid\":{sid},\"rule_id\":{},\"decision\":{},\"cmd_sha256\":{},\
             \"command\":\"\",\"reason\":\"\",\"box_seq\":0,\"{SELF_CLAIM_HOLDER}\":{},\
             \"{SELF_CLAIM_ACT}\":{},\"{SELF_CLAIM_WORD}\":{}}}",
            json_str(RULE_SELF_CLAIM_ACT),
            json_str(SELF_CLAIM_DECISION),
            json_str(&sha256_hex("")),
            json_str(self.holder),
            json_str(self.act),
            json_str(word)
        )
    }

    /// The row a ledger line `v` is, if it is a self-claim's: its holder and its act.
    #[must_use]
    pub fn of(v: &aterm_json::Value) -> Option<SelfClaimRow<'_>> {
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("decision")? != SELF_CLAIM_DECISION {
            return None;
        }
        Some(SelfClaimRow {
            holder: text(SELF_CLAIM_HOLDER).unwrap_or_default(),
            act: text(SELF_CLAIM_ACT).unwrap_or_default(),
        })
    }
}

/// A FACT A PROVEN SELF-CLAIM'S ACT CARRIED (gap 23): aterm's own turn-end fact row
/// (`fact-<kind>@v1`, `typed`, `command` the sentence, `reason`
/// [`fact_outcome_reason`](super::occupancy::door::fact_outcome_reason) at the turn-end door),
/// with the claim's holder ([`SELF_CLAIM_HOLDER`]) after it.
/// [`seed_rows_in`](super::occupancy::door::seed_rows_in) seeds it told, [`is_fact_line`] keeps
/// it out of every reader of acts, and `harness::stats` books it to its holder, never to aterm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfClaimFactRow<'a> {
    /// The claim's holder (`codex-alab@<pid>`).
    pub holder: &'a str,
    /// The fact its act carried, as it was offered.
    pub fact: &'a super::facts::Fact,
}

impl SelfClaimFactRow<'_> {
    /// The row as one JSON object, `ts` and `sid` given.
    #[must_use]
    pub fn to_json(&self, ts: i64, sid: Option<&str>) -> String {
        use super::occupancy::door::{FactDoor, Told, fact_outcome_reason};
        let reason = fact_outcome_reason(self.fact.key, FactDoor::TurnEnd, Told::Typed);
        let row = Row {
            rule_id: self.fact.key.kind.rule(),
            outcome: Outcome::Typed,
            command: &self.fact.text,
            reason: &reason,
            box_seq: 0,
        }
        .to_json(ts, sid);
        let head = row.strip_suffix('}').unwrap_or(&row);
        format!("{head},\"{SELF_CLAIM_HOLDER}\":{}}}", json_str(self.holder))
    }

    /// The holder and the rule of a ledger line `v` that is a self-claim's fact row: a `typed`
    /// row of a fact kind's rule that names a holder.
    #[must_use]
    pub fn of(v: &aterm_json::Value) -> Option<(&str, &str)> {
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        let rule = text("rule_id")?;
        super::facts::FactKind::of_rule(rule)?;
        if text("decision")? != Outcome::Typed.word() {
            return None;
        }
        Some((text(SELF_CLAIM_HOLDER)?, rule))
    }
}

#[must_use]
pub fn last_act_count_in(body: &str, sid: Option<&str>) -> Option<(i64, Option<SleepStamp>)> {
    let last = *typed_at_in(body, sid).last()?;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let prefix = format!("{{\"ts\":{last},");
    let count = act_lines(body)
        .rev()
        .filter(|l| l.starts_with(&prefix) && l.contains(",\"decision\":\"typed\","))
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .find(|v| {
            v.get("sid")
                .and_then(aterm_json::Value::as_str)
                .map(str::to_string)
                == want
        })
        .and_then(|v| {
            let ms = v.get(SLEPT_MS).and_then(aterm_json::Value::as_u64)?;
            let boot = v.get(BOOT).and_then(aterm_json::Value::as_str)?;
            Some(SleepStamp {
                asleep: std::time::Duration::from_millis(ms),
                boot: Some(Boot::of(boot)?),
            })
        });
    Some((last, count))
}

pub fn default_path(sid: Option<&str>) -> Option<PathBuf> {
    path_under(&aterm_types::dirs::state_dir()?, sid)
}

pub fn path_under(state: &Path, sid: Option<&str>) -> Option<PathBuf> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(state.join("drive")).ok()?;
    Some(ledger_under(state, sid))
}

#[must_use]
pub fn ledger_under(state: &Path, sid: Option<&str>) -> PathBuf {
    let name: String = sid
        .map(|s| s.trim_start_matches('@'))
        .filter(|s| !s.is_empty())
        .unwrap_or("self")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    state.join("drive").join(format!("{name}.jsonl"))
}

#[must_use]
pub fn journal_beside(ledger: &Path) -> PathBuf {
    let stem = ledger
        .file_stem()
        .map_or_else(|| "self".into(), |s| s.to_string_lossy().into_owned());
    ledger.with_file_name(format!("{stem}.journal.jsonl"))
}

pub const MAX_BYTES: u64 = 1024 * 1024;

pub const KEEP_ROWS: usize = 1024;

pub fn bound(path: &Path) {
    bound_with(path, &|rows, read| cut_rows(path, rows, read));
}

/// [`bound`] with the rows kept chosen by `cut`, under the ledger's lock ([`ledger_stripe`]) from
/// the cut's first read to its rename.
fn bound_with(path: &Path, cut: &crate::harness::cli::LedgerCut<'_>) {
    let ledger_cut = ledger_stripe(path);
    let _held = ledger_cut
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = crate::harness::cli::bound_ledger_cut(path, MAX_BYTES, cut);
}

/// The locks [`ledger_stripe`] stripes the ledgers' paths over.
static LEDGER_LOCKS: [std::sync::Mutex<()>; 16] = [const { std::sync::Mutex::new(()) }; 16];

/// THE LOCK A LEDGER'S CUT AND A SELF-CLAIM'S BOOKING SHARE IN THIS PROCESS, the stripe of
/// [`LEDGER_LOCKS`] the ledger at `path` falls in: the cut ([`bound`]) holds it from its first
/// read to its rename, and [`book_self_claim`] from its open to its append, so a row a control
/// thread books while a loop's start cuts the same ledger lands in the file the cut leaves, never
/// in the copy the cut read nor in the file it renamed over. The journal also locks every append
/// and cut across processes by a stable sibling, reopening the current ledger for each append.
/// Two ledgers share this in-process lock only where their paths fall in one stripe.
fn ledger_stripe(path: &Path) -> &'static std::sync::Mutex<()> {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::hash::DefaultHasher::new();
    path.hash(&mut hasher);
    &LEDGER_LOCKS[usize::from(hasher.finish().to_le_bytes()[0]) % LEDGER_LOCKS.len()]
}

/// BOOK A PROVEN SELF-CLAIM'S ACT ([`SelfClaimRow`]) in session `sid`'s ledger at `path`, stamped
/// `t_ms`: opened and appended through [`Ledger::write_self_claim`] under the lock the ledger's
/// cut holds ([`ledger_stripe`]). The line, or `None` where nothing was written (why in `warn`).
pub fn book_self_claim(
    path: &Path,
    sid: Option<&str>,
    t_ms: i64,
    row: &SelfClaimRow<'_>,
    warn: &mut dyn Write,
) -> Option<String> {
    let ledger_cut = ledger_stripe(path);
    let _held = ledger_cut
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ledger::open(Some(path), sid, warn).write_self_claim(t_ms, row, warn)
}

/// BOOK A FACT A PROVEN SELF-CLAIM'S ACT CARRIED ([`SelfClaimFactRow`]) in session `sid`'s
/// ledger at `path`, stamped `t_ms`, as [`book_self_claim`] books its act: through
/// [`Ledger::write_self_claim_fact`] under the lock the ledger's cut holds ([`ledger_stripe`]).
/// The line, or `None` where nothing was written (why in `warn`).
pub fn book_self_claim_fact(
    path: &Path,
    sid: Option<&str>,
    t_ms: i64,
    row: &SelfClaimFactRow<'_>,
    warn: &mut dyn Write,
) -> Option<String> {
    let ledger_cut = ledger_stripe(path);
    let _held = ledger_cut
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Ledger::open(Some(path), sid, warn).write_self_claim_fact(t_ms, row, warn)
}

/// The text of the session ledger at `path` for a reader beside the loop (a proven self-claim's
/// facts account seeds from it once): `Ok(None)` where none stands; an error where one stands
/// that is no regular file, is not UTF-8 or is longer than [`HELD_BYTES`], so its reader seeds
/// nothing rather than reading part of it as all of it.
///
/// # Errors
///
/// The file could not be opened or read, or is one of the above.
pub fn read_ledger_text(path: &Path) -> std::io::Result<Option<String>> {
    crate::harness::accounts::read_bounded(path, HELD_BYTES as u64)
}

fn cut_rows(
    path: &Path,
    rows: &mut dyn Iterator<Item = String>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Option<Vec<String>> {
    cut_rows_with_work(path, rows, read, &mut CutWork::for_cut())
}

fn cut_rows_with_work(
    path: &Path,
    rows: &mut dyn Iterator<Item = String>,
    read: &dyn Fn(&Path) -> Option<String>,
    work: &mut CutWork,
) -> Option<Vec<String>> {
    let (held, start) = held_rows(rows)?;
    let Ok(mut older) = older_kept(path, &held, start, read, work) else {
        return Some(held);
    };
    older.extend(go_ahead_hours_kept(&held, start));
    older.sort_unstable();
    older.dedup();
    Some(
        held.into_iter()
            .enumerate()
            .filter(|(i, _)| *i >= start || older.binary_search(i).is_ok())
            .map(|(_, row)| row)
            .collect(),
    )
}

pub(crate) const HELD_BYTES: usize = 16 << 20;

/// Keep the row the hour reader would take for each session, including one older than the
/// ordinary row window. Clock order need not be append order; equal starts keep the later row.
fn go_ahead_hours_kept(rows: &[String], start: usize) -> Vec<usize> {
    let mut newest = std::collections::BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        if let Some((sid, from)) = go_ahead_hour_row(row) {
            let previous = newest.entry(sid).or_insert((from, index));
            if from >= previous.0 {
                *previous = (from, index);
            }
        }
    }
    newest
        .into_values()
        .filter_map(|(_, index)| (index < start).then_some(index))
        .collect()
}

fn held_rows(rows: &mut dyn Iterator<Item = String>) -> Option<(Vec<String>, usize)> {
    let mut newest = std::collections::VecDeque::new();
    let mut newest_bytes = 0_usize;
    let mut plain = 0_usize;
    let mut older = Vec::new();
    let mut older_bytes = 0_usize;
    for line in rows {
        if line.trim().is_empty() {
            continue;
        }
        if record_of(&line).is_none() {
            plain += 1;
        }
        newest_bytes = newest_bytes.checked_add(line.len())?;
        newest_bytes = newest_bytes.checked_sub(fold_records(&mut newest, line))?;
        while plain > KEEP_ROWS {
            let row = newest.pop_front()?;
            newest_bytes = newest_bytes.checked_sub(row.len())?;
            if record_of(&row).is_none() {
                plain -= 1;
            }
            if LandWalk::reads(&row)
                || SwitchWalk::reads(&row)
                || row.contains(RULE_EARLIER_BOOKS)
                || row.contains(super::policy::turn_end::RULE_GO_AHEAD_HOUR)
            {
                older_bytes = older_bytes.checked_add(row.len())?;
                older.push(row);
            }
        }
        if older_bytes.checked_add(newest_bytes)? > HELD_BYTES {
            return None;
        }
    }
    let start = older.len();
    older.extend(newest);
    Some((older, start))
}

fn older_kept(
    path: &Path,
    kept: &[String],
    start: usize,
    read: &dyn Fn(&Path) -> Option<String>,
    work: &mut CutWork,
) -> Result<Vec<usize>, ()> {
    let (take_up, from) = earlier_standing(path, kept, start, read, work)?;
    let mut older = standing_path(&kept[..start], from, &kept[start..], work)?;
    older.extend(take_up.filter(|&at| at < start));
    older.sort_unstable();
    older.dedup();
    Ok(older)
}

type Standing = std::collections::BTreeMap<Option<String>, (LandWalk, SwitchWalk)>;

struct CutWork {
    entries: usize,
    visits: usize,
    bytes: usize,
}

impl CutWork {
    const fn for_cut() -> Self {
        Self {
            entries: 65_536,
            visits: 1_048_576,
            bytes: 64 << 20,
        }
    }

    fn spend(&mut self, entries: usize, visits: usize, bytes: usize) -> Result<(), ()> {
        self.entries = self.entries.checked_sub(entries).ok_or(())?;
        self.visits = self.visits.checked_sub(visits).ok_or(())?;
        self.bytes = self.bytes.checked_sub(bytes).ok_or(())?;
        Ok(())
    }

    fn state(&mut self, state: &Standing, copies: usize) -> Result<(), ()> {
        self.spend(
            state.len().saturating_mul(copies),
            state.len().saturating_add(1),
            0,
        )?;
        let bytes = state
            .iter()
            .try_fold(0_usize, |bytes, (sid, (_, switch))| {
                let model = switch.open.as_ref().map_or(0, |s| {
                    s.to.len()
                        .saturating_add(s.from.as_ref().map_or(0, String::len))
                        .saturating_add(
                            s.proc
                                .as_ref()
                                .and_then(|p| p.conversation.as_ref())
                                .map_or(0, String::len),
                        )
                });
                bytes
                    .checked_add(sid.as_ref().map_or(0, String::len))?
                    .checked_add(model)
            })
            .ok_or(())?;
        self.spend(0, 0, bytes.saturating_mul(copies.max(1)))
    }

    fn books(&mut self, sid: &str, earlier: &[(String, String)]) -> Result<(), ()> {
        let to = sid.len().saturating_mul(6).saturating_add(8);
        for (tab, text) in earlier {
            let rows = text.lines().count();
            let expanded = text.len().saturating_add(rows.saturating_mul(to));
            let fields = tab
                .len()
                .saturating_mul(6)
                .saturating_add(8)
                .saturating_add(to);
            self.spend(0, rows, expanded.saturating_mul(2).saturating_add(fields))?;
        }
        Ok(())
    }

    fn copy(&mut self, state: &Standing) -> Result<Standing, ()> {
        self.state(state, 1)?;
        Ok(state.clone())
    }

    fn same(&mut self, left: &Standing, right: &Standing) -> Result<bool, ()> {
        self.state(left, 0)?;
        self.state(right, 0)?;
        Ok(left == right)
    }
}

type WalkRow = (Option<String>, aterm_json::Value, bool, bool);

fn standing_row(line: &str) -> Option<WalkRow> {
    let (land, switch) = (LandWalk::reads(line), SwitchWalk::reads(line));
    if !land && !switch {
        return None;
    }
    let v = aterm_json::from_str::<aterm_json::Value>(line).ok()?;
    let sid = v
        .get("sid")
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string);
    Some((sid, v, land, switch))
}

fn standing_step(
    state: &Standing,
    (sid, v, land, switch): &WalkRow,
    work: &mut CutWork,
) -> Result<Option<Standing>, ()> {
    work.state(state, 3)?;
    let row_bytes = sid
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(
            v.get("command")
                .and_then(aterm_json::Value::as_str)
                .map_or(0, str::len),
        )
        .saturating_add(
            v.get("reason")
                .and_then(aterm_json::Value::as_str)
                .map_or(0, str::len),
        );
    work.spend(
        1,
        1,
        row_bytes.saturating_mul(state.len().saturating_add(3)),
    )?;
    let was = state.get(sid).cloned().unwrap_or_default();
    let mut next = was.clone();
    if *land {
        next.0.step(v);
    }
    if *switch {
        next.1.step(v);
    }
    if next == was {
        return Ok(None);
    }
    let mut state = state.clone();
    if next == <(LandWalk, SwitchWalk)>::default() {
        state.remove(sid);
    } else {
        state.insert(sid.clone(), next);
    }
    Ok(Some(state))
}

fn standing_after<'a>(
    mut from: Standing,
    rows: impl IntoIterator<Item = &'a WalkRow>,
    work: &mut CutWork,
) -> Result<Standing, ()> {
    for row in rows {
        if let Some(next) = standing_step(&from, row, work)? {
            from = next;
        }
    }
    Ok(from)
}

fn standing_rows(
    rows: impl IntoIterator<Item = impl AsRef<str>>,
    work: &mut CutWork,
) -> Result<Vec<WalkRow>, ()> {
    let mut out = Vec::new();
    for row in rows {
        work.spend(0, 1, 0)?;
        if let Some(row) = standing_row(row.as_ref()) {
            out.push(row);
        }
    }
    Ok(out)
}

fn standing_path(
    rows: &[String],
    from: Standing,
    after: &[String],
    work: &mut CutWork,
) -> Result<Vec<usize>, ()> {
    let mut through = vec![from];
    let mut path: Vec<usize> = Vec::new();
    for (i, line) in rows.iter().enumerate() {
        work.spend(0, 1, 0)?;
        let Some(row) = standing_row(line) else {
            continue;
        };
        let Some(next) = standing_step(through.last().ok_or(())?, &row, work)? else {
            continue;
        };
        let mut found = None;
        for (k, state) in through.iter().enumerate() {
            if work.same(state, &next)? {
                found = Some(k);
                break;
            }
        }
        if let Some(k) = found {
            path.truncate(k);
            through.truncate(k + 1);
        } else {
            path.push(i);
            through.push(next);
        }
    }
    let after = standing_rows(after, work)?;
    if let Some(last) = through.last() {
        let copied = work.copy(last)?;
        let last = standing_after(copied, &after, work)?;
        for (k, state) in through.iter().enumerate() {
            let copied = work.copy(state)?;
            let end = standing_after(copied, &after, work)?;
            if work.same(&end, &last)? {
                path.truncate(k);
                break;
            }
        }
    }
    Ok(path)
}

fn earlier_standing(
    path: &Path,
    rows: &[String],
    start: usize,
    read: &dyn Fn(&Path) -> Option<String>,
    work: &mut CutWork,
) -> Result<(Option<usize>, Standing), ()> {
    work.spend(0, rows.len(), 0)?;
    let Some((at, sid)) = rows.iter().enumerate().find_map(|(i, l)| {
        if !l.contains(RULE_EARLIER_BOOKS) {
            return None;
        }
        let v = aterm_json::from_str::<aterm_json::Value>(l).ok()?;
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        (text("rule_id") == Some(RULE_EARLIER_BOOKS))
            .then(|| text("sid").map(str::to_string))
            .flatten()
            .map(|sid| (i, sid))
    }) else {
        return Ok((None, Standing::new()));
    };
    let earlier: Vec<(String, String)> =
        crate::harness::upgrade_drive::earlier_ledgers_in(path, Some(&sid), read)
            .into_iter()
            .filter_map(|(t, p)| Some((t, read(&p)?)))
            .collect();
    work.books(&sid, &earlier)?;
    let books = books_through("", Some(&sid), &earlier);
    let theirs = standing_rows(books.lines(), work)?;
    let from = standing_after(Standing::new(), &theirs, work)?;
    if at >= start {
        return Ok((Some(at), from));
    }
    let rows = standing_rows(rows, work)?;
    let copied = work.copy(&from)?;
    let with_earlier = standing_after(copied, &rows, work)?;
    let alone = standing_after(Standing::new(), &rows, work)?;
    if !work.same(&with_earlier, &alone)? {
        Ok((Some(at), from))
    } else {
        Ok((None, Standing::new()))
    }
}

fn sid_field(sid: &str) -> String {
    format!("\"sid\":{}", json_str(sid.trim_start_matches('@')))
}

#[must_use]
pub fn takes_up_earlier(body: &str, sid: Option<&str>) -> bool {
    let Some(sid) = sid else {
        return false;
    };
    let field = sid_field(sid);
    let rule = format!("\"rule_id\":{}", json_str(RULE_EARLIER_BOOKS));
    let mut rows = body.lines().filter(|l| l.contains(&field)).peekable();
    rows.peek().is_none() || rows.any(|l| l.contains(&rule))
}

#[must_use]
pub fn holds_rows_of(body: &str, sid: &str) -> bool {
    let field = sid_field(sid);
    body.lines().any(|l| l.contains(&field))
}

#[must_use]
pub fn took_up_earlier(body: &str, sid: &str) -> bool {
    let field = sid_field(sid);
    let rule = format!("\"rule_id\":{}", json_str(RULE_EARLIER_BOOKS));
    body.lines()
        .any(|l| l.contains(&field) && l.contains(&rule))
}

#[must_use]
pub fn last_row_ms(body: &str) -> Option<i64> {
    body.lines()
        .filter_map(|l| {
            let rest = l.trim_start().strip_prefix("{\"ts\":")?;
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .max()
}

#[must_use]
pub fn books_through(own: &str, sid: Option<&str>, earlier: &[(String, String)]) -> String {
    let Some(sid) = sid else {
        return own.to_string();
    };
    let to = sid_field(sid);
    let lead = |l: &str| -> Option<i64> {
        let rest = l
            .strip_prefix("{\"ts\":")
            .or_else(|| l.strip_prefix("{\"t\":"))?;
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let mut tabs: Vec<(Option<i64>, Vec<String>)> = earlier
        .iter()
        .filter(|(t, _)| t.trim_start_matches('@') != sid.trim_start_matches('@'))
        .map(|(t, text)| {
            let from = sid_field(t);
            let rows: Vec<String> = text
                .lines()
                .filter(|l| l.contains(&from))
                .map(|l| l.replacen(&from, &to, 1))
                .collect();
            (rows.first().and_then(|l| lead(l)), rows)
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect();
    tabs.sort_by_key(|(at, _)| at.unwrap_or(i64::MAX));
    let mut out = String::new();
    for (_, rows) in tabs {
        for row in rows {
            out.push_str(&row);
            out.push('\n');
        }
    }
    out.push_str(own);
    out
}

fn fold_records(kept: &mut std::collections::VecDeque<String>, row: String) -> usize {
    let mut discarded = 0;
    if let Some((sid, point)) = record_of(&row) {
        let back = |kept: &std::collections::VecDeque<String>, i: usize| {
            kept.len()
                .checked_sub(i)
                .and_then(|n| kept.get(n))
                .and_then(|l| record_of(l))
                .filter(|(s, _)| *s == sid)
                .map(|(_, p)| p)
        };
        match (point, back(kept, 1)) {
            (false, Some(false)) => return row.len(),
            (true, Some(false)) => {
                discarded += kept.pop_back().map_or(0, |row| row.len());
            }
            _ => {}
        }
        if point && back(kept, 1) == Some(true) && back(kept, 2) == Some(true) {
            discarded += kept.pop_back().map_or(0, |row| row.len());
        }
    }
    kept.push_back(row);
    discarded
}

fn record_of(line: &str) -> Option<(Option<String>, bool)> {
    let point = line.contains(&format!("\"rule_id\":\"{RULE_POINT}\""));
    if !point && !line.contains(&format!("\"rule_id\":\"{RULE_TURN_RAN}\"")) {
        return None;
    }
    let v = aterm_json::from_str::<aterm_json::Value>(line).ok()?;
    let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
    (text("decision") == Some(Outcome::Waiting.word())
        && text("rule_id") == Some(if point { RULE_POINT } else { RULE_TURN_RAN }))
    .then(|| (text("sid").map(str::to_string), point))
}

#[derive(Debug)]
pub struct Ledger {
    journal: Journal,
    sid: Option<String>,
}

impl Ledger {
    pub const fn off() -> Self {
        Self {
            journal: Journal::off(),
            sid: None,
        }
    }

    #[must_use]
    pub fn named(path: Option<&Path>, sid: Option<&str>) -> Self {
        Self {
            journal: Journal::named(path, sid),
            sid: sid.map(str::to_string),
        }
    }

    pub fn open(path: Option<&Path>, sid: Option<&str>, warn: &mut dyn Write) -> Self {
        Self {
            journal: Journal::open(path, sid, warn),
            sid: sid.map(str::to_string),
        }
    }

    pub fn write(&mut self, t_ms: i64, row: &Row<'_>, warn: &mut dyn Write) -> Option<String> {
        self.write_counted(t_ms, row, None, warn)
    }

    pub fn write_counted(
        &mut self,
        t_ms: i64,
        row: &Row<'_>,
        count: Option<&SleepStamp>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        self.write_counted_with_boot_text(t_ms, row, count, None, None, warn)
    }

    /// [`Self::write_counted_with_boot_text`] under a wall's or a point's scope (`conversation`,
    /// `program`, [`Row::to_json_scoped_by`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_scoped_counted(
        &mut self,
        t_ms: i64,
        row: &Row<'_>,
        count: Option<&SleepStamp>,
        captured_boot: Option<&str>,
        conversation: &Conversation,
        program: Option<aterm_phase::Program>,
        human: Option<u64>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        if !self.journal.is_on() {
            return None;
        }
        let line = with_human(
            row.to_json_scoped_by(
                t_ms,
                self.sid.as_deref(),
                count,
                captured_boot,
                conversation,
                program,
            ),
            human,
        );
        self.journal.append_raw(&line, warn);
        Some(line)
    }

    /// Append a proven self-claim's act ([`SelfClaimRow`]) stamped `t_ms`, through the one
    /// append the loop's own rows take.
    pub fn write_self_claim(
        &mut self,
        t_ms: i64,
        row: &SelfClaimRow<'_>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        if !self.journal.is_on() {
            return None;
        }
        let line = row.to_json(t_ms, self.sid.as_deref());
        self.journal.append_raw(&line, warn).then_some(line)
    }

    /// Append a fact a proven self-claim's act carried ([`SelfClaimFactRow`]) stamped `t_ms`,
    /// through the one append the loop's own rows take.
    pub fn write_self_claim_fact(
        &mut self,
        t_ms: i64,
        row: &SelfClaimFactRow<'_>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        if !self.journal.is_on() {
            return None;
        }
        let line = row.to_json(t_ms, self.sid.as_deref());
        self.journal.append_raw(&line, warn).then_some(line)
    }

    /// Append `row` stamped `t_ms`, with the Mac's sleep `count` (its boot spelled as
    /// `captured_boot` where that names the same boot) and, for the accounts probe's write, the
    /// person's key count `human` it was fenced on ([`PROBE_HUMAN`]).
    pub(crate) fn write_counted_with_boot_text(
        &mut self,
        t_ms: i64,
        row: &Row<'_>,
        count: Option<&SleepStamp>,
        captured_boot: Option<&str>,
        human: Option<u64>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        if !self.journal.is_on() {
            return None;
        }
        let line = with_human(
            row.to_json_counted_with_boot_text(t_ms, self.sid.as_deref(), count, captured_boot),
            human,
        );
        self.journal.append_raw(&line, warn);
        Some(line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub at_ms: i64,
    pub text: String,
    pub rule: String,
}

/// A best-effort display query. Decisions about carrying a conversation use
/// [`read_typed_texts`] so unreadable evidence is not mistaken for no acts.
#[must_use]
pub fn typed_texts(path: &Path, sid: Option<&str>) -> Vec<Typed> {
    read_typed_texts(path, sid).unwrap_or_default()
}

/// The acts typed for `sid`, preserving a read refusal as unknown evidence. An absent file
/// contains no acts; an unreadable, nonregular, oversized or non-text file is not an empty book.
/// # Errors
/// The existing ledger cannot be read as bounded regular text.
pub fn read_typed_texts(path: &Path, sid: Option<&str>) -> std::io::Result<Vec<Typed>> {
    Ok(
        crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024)?
            .map_or_else(Vec::new, |body| typed_texts_in(&body, sid)),
    )
}

/// The acts typed for `sid` in the books `body`, oldest first: when, what and under which rule.
#[must_use]
pub fn typed_texts_in(body: &str, sid: Option<&str>) -> Vec<Typed> {
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let mut out = Vec::new();
    for line in act_lines(body).filter(|l| l.contains("\"typed\"")) {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let field = |key: &str| v.get(key).and_then(aterm_json::Value::as_str);
        if field("sid") != sid || field("decision") != Some("typed") {
            continue;
        }
        if let (Some(at_ms), Some(text)) = (
            v.get("ts").and_then(aterm_json::Value::as_i64),
            field("command"),
        ) {
            out.push(Typed {
                at_ms,
                text: text.to_string(),
                rule: field("rule_id").unwrap_or_default().to_string(),
            });
        }
    }
    out
}

pub fn typed_at_in(body: &str, sid: Option<&str>) -> Vec<i64> {
    let sid = sid.map_or_else(
        || "null".to_string(),
        |s| json_str(s.trim_start_matches('@')),
    );
    let want_sid = format!(",\"sid\":{sid},");
    let move_rule = format!(
        ",\"rule_id\":{},",
        json_str(super::policy::turn_end::RULE_ACCOUNT)
    );
    let move_command = format!(",\"command\":\"{ACCOUNT_MOVE_COMMAND}");
    let probe_rule = probe_rule_field();
    let login_done = format!(
        ",\"rule_id\":{},",
        json_str(super::policy::login::RULE_LOGIN_DONE)
    );
    let mut out: Vec<i64> = act_lines(body)
        .filter(|l| l.contains(&want_sid) && l.contains(",\"decision\":\"typed\","))
        .filter(|l| !(l.contains(&move_rule) && l.contains(&move_command)))
        .filter(|l| !l.contains(&probe_rule) && !l.contains(&login_done))
        .filter_map(|l| {
            let rest = l.strip_prefix("{\"ts\":")?;
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect();
    out.sort_unstable();
    out
}

/// THE PERSON'S HOUR A LOOP BEFORE THIS ONE BOOKED for `sid`
/// ([`super::policy::turn_end::RULE_GO_AHEAD_HOUR`]): the latest start (unix ms) its rows name.
#[must_use]
pub fn go_ahead_hour_in(body: &str, sid: Option<&str>) -> Option<i64> {
    let want = sid.map(|s| s.trim_start_matches('@'));
    body.lines()
        .filter_map(go_ahead_hour_row)
        .filter_map(|(sid, from)| (sid.as_deref() == want).then_some(from))
        .max()
}

/// A row's session and signed start, shared by recovery and cut retention so both choose the
/// same hour. An invalid rule, JSON body or command contributes no start.
fn go_ahead_hour_row(line: &str) -> Option<(Option<String>, i64)> {
    use super::policy::turn_end::RULE_GO_AHEAD_HOUR;
    if !line.contains(RULE_GO_AHEAD_HOUR) {
        return None;
    }
    let row = aterm_json::from_str::<aterm_json::Value>(line).ok()?;
    if row.get("rule_id")?.as_str()? != RULE_GO_AHEAD_HOUR {
        return None;
    }
    let sid = row
        .get("sid")
        .and_then(aterm_json::Value::as_str)
        .map(str::to_string);
    let from = crate::parse_int(row.get("command")?.as_str()?.strip_prefix("from=")?)?;
    Some((sid, from))
}

#[must_use]
pub fn login_typed_last_in(body: &str, sid: Option<&str>) -> Option<i64> {
    use super::policy::turn_end::RULE_LOGIN;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last = None;
    for line in act_lines(body) {
        let typed = line.contains(",\"decision\":\"typed\",");
        if !typed && !line.contains(RULE_TURN_RAN) {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        match text("rule_id") {
            Some(RULE_LOGIN) if typed => last = v.get("ts").and_then(aterm_json::Value::as_i64),
            Some(RULE_TURN_RAN) => last = None,
            _ if typed => last = None,
            _ => {}
        }
    }
    last
}

pub const LEFT_REASON: &str = "a person's own exchange: nothing is typed until the next turn ends";

pub const LEFT_AT_WALL_REASON: &str =
    "a person's own exchange at a wall: its retry's short answer is theirs";

/// What closes a `LEFT` row's reason where the exchange it leaves opens the session (the
/// policy's own judgment as it was left, [`left_reason`]): such a record is the person's however
/// the tab's books read before it ([`left_after_work_in`]), a program started fresh in a tab
/// another ran included.
pub const LEFT_OPENS: &str = " (the session's opening message)";

/// A `LEFT` row's reason: `reason`, closed by [`LEFT_OPENS`] where the exchange `opens` the
/// session.
#[must_use]
pub fn left_reason(reason: &str, opens: bool) -> String {
    if opens {
        format!("{reason}{LEFT_OPENS}")
    } else {
        reason.to_string()
    }
}

pub const LEFT_AT_WALL: &str = "wall:";

#[must_use]
pub fn left_at_wall_key(turn: &str, work: std::time::Duration) -> String {
    format!("{LEFT_AT_WALL}{turn}@{}", work.as_secs())
}

#[must_use]
pub fn parse_left_at_wall(key: &str) -> Option<(&str, Option<std::time::Duration>)> {
    let rest = key.strip_prefix(LEFT_AT_WALL)?;
    let worked = rest.rsplit_once('@').and_then(|(turn, secs)| {
        let secs = crate::parse_uint(secs)?;
        Some((turn, std::time::Duration::from_secs(secs)))
    });
    Some(worked.map_or((rest, None), |(turn, work)| (turn, Some(work))))
}

pub const LEFT_RETRIED: &str = "retried:";

#[must_use]
pub fn left_retried_key(wall: &str, retry: &str) -> String {
    format!("{LEFT_RETRIED}{:016x}:{wall}", words_hash(retry))
}

#[must_use]
pub fn parse_left_retried(key: &str) -> Option<(u64, &str)> {
    let (hash, wall) = key.strip_prefix(LEFT_RETRIED)?.split_once(':')?;
    if hash.is_empty() || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((u64::from_str_radix(hash, 16).ok()?, wall))
}

pub const LEFT_BEFORE: &str = "before:";

#[must_use]
pub fn left_before_key(at_ms: i64) -> String {
    format!("{LEFT_BEFORE}{at_ms}")
}

#[must_use]
pub fn parse_left_before(key: &str) -> Option<u64> {
    crate::parse_uint(key.strip_prefix(LEFT_BEFORE)?)
}

#[must_use]
pub fn words_hash(text: &str) -> u64 {
    aterm_hash::fnv1a_64(
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_bytes(),
    )
}

pub const LEAD_WORDS: usize = 8;

#[must_use]
pub fn words_lead(text: &str) -> u64 {
    aterm_hash::fnv1a_64(
        text.split_whitespace()
            .take(LEAD_WORDS)
            .collect::<Vec<_>>()
            .join(" ")
            .as_bytes(),
    )
}

#[must_use]
pub fn open_left_in(body: &str, sid: Option<&str>) -> Option<String> {
    use super::policy::turn_end::RULE_LEFT;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut open: Option<String> = None;
    for line in act_lines(body) {
        let typed = line.contains(",\"decision\":\"typed\",");
        if !typed && !line.contains(",\"decision\":\"skipped\",") {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        match (typed, text("rule_id")) {
            (true, rule) => {
                open = open.and_then(|k| {
                    left_after_typed(&k, rule.unwrap_or(""), text("command").unwrap_or(""))
                });
            }
            (false, Some(RULE_LEFT)) => open = text("command").map(str::to_string),
            (false, _) => {}
        }
    }
    open
}

/// Whether the last person's exchange left to them for `sid` (a `skipped` row under
/// [`RULE_LEFT`](super::policy::turn_end::RULE_LEFT)) was left after the session worked. A row
/// whose reason closes with [`LEFT_OPENS`] was left as the session's opening message, so never;
/// any other, by the books before it: an act a loop typed (a fact line, an accounts probe, an
/// account move or a sign-in's done is none, as [`typed_at_in`] reads them), or a point it
/// recorded at an earlier screen (`box_seq`). Under Land only a session's opening message is the
/// person's, so a record of one left after work is an older build's,
/// under the superseded rule, or Off's.
#[must_use]
pub fn left_after_work_in(body: &str, sid: Option<&str>) -> bool {
    use super::policy::login::RULE_LOGIN_DONE;
    use super::policy::turn_end::{RULE_ACCOUNT, RULE_LEFT, RULE_USAGE_PROBE};
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let (mut typed, mut first_point, mut after_work) = (false, None::<u64>, false);
    for line in act_lines(body) {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        let rule = text("rule_id").unwrap_or("");
        let seq = v.get("box_seq").and_then(aterm_json::Value::as_u64);
        let moved = rule == RULE_ACCOUNT
            && text("command").is_some_and(|c| c.starts_with(ACCOUNT_MOVE_COMMAND));
        match text("decision") {
            Some("typed")
                if !is_fact_rule(rule)
                    && rule != RULE_USAGE_PROBE
                    && rule != RULE_LOGIN_DONE
                    && !moved =>
            {
                typed = true;
            }
            Some("waiting") if rule == RULE_POINT => {
                if let Some(seq) = seq {
                    first_point = Some(first_point.map_or(seq, |first| first.min(seq)));
                }
            }
            Some("skipped") if rule == RULE_LEFT => {
                after_work = !text("reason").is_some_and(|r| r.ends_with(LEFT_OPENS))
                    && (typed || first_point.zip(seq).is_some_and(|(first, at)| first < at));
            }
            _ => {}
        }
    }
    after_work
}

#[must_use]
pub fn left_after_typed(open: &str, rule: &str, command: &str) -> Option<String> {
    use super::policy::turn_end::{
        RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY, RULE_LIMIT_RESUME, RULE_LOGIN_BACK,
        RULE_OUTAGE_RESUME, RULE_USAGE_PROBE,
    };
    match rule {
        RULE_USAGE_PROBE => Some(open.to_string()),
        // A fact line ends no person's exchange ([`is_fact_rule`]).
        rule if is_fact_rule(rule) => Some(open.to_string()),
        RULE_API_RETRY | RULE_API_BACK | RULE_API_CUTOFF | RULE_OUTAGE_RESUME
        | RULE_LIMIT_RESUME | RULE_LOGIN_BACK
            if open.starts_with(LEFT_AT_WALL) =>
        {
            Some(left_retried_key(open, command))
        }
        _ => None,
    }
}

/// A best-effort display query. Decisions about carrying a conversation use
/// [`read_open_left`] so unreadable evidence does not lift a standing exchange.
#[must_use]
pub fn open_left(path: &Path, sid: Option<&str>) -> Option<String> {
    read_open_left(path, sid).ok().flatten()
}

/// The exchange left to the person in `sid`'s books, preserving unreadable evidence.
/// An absent ledger has no standing exchange.
/// # Errors
/// The existing ledger cannot be read as bounded regular text.
pub fn read_open_left(path: &Path, sid: Option<&str>) -> std::io::Result<Option<String>> {
    Ok(
        crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024)?
            .and_then(|body| open_left_in(&body, sid)),
    )
}

#[must_use]
pub fn typed_at(path: &Path, sid: Option<&str>) -> Vec<i64> {
    crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024)
        .ok()
        .flatten()
        .map_or_else(Vec::new, |body| typed_at_in(&body, sid))
}

pub const PROBE_TYPED_REASON: &str = "the accounts probe";

pub const PROBE_CLOSED_REASON: &str = "closed";

/// The field of the accounts probe's `typed` row holding the person's key count its write was
/// fenced on (`if-human=`): what proves, after a restart, that a `/usage` standing is still the
/// line that write left, with no person's key since ([`ProbeWrote`], v6 §14.17.5.8).
pub const PROBE_HUMAN: &str = "human";

/// `line` (one JSON object) with [`PROBE_HUMAN`] added where `human` is some.
fn with_human(mut line: String, human: Option<u64>) -> String {
    if let Some(n) = human {
        line.pop();
        line.push_str(&format!(",\"{PROBE_HUMAN}\":{n}}}"));
    }
    line
}

/// THE ACCOUNTS PROBE'S WRITE A RESTART FINDS OWED ([`probe_wrote`]): its row's screen sequence
/// (`box_seq`), the window's gesture count it was fenced on ([`PROBE_HUMAN`]; `None` in a row a
/// build before the field wrote, which proves nothing) and its stamp (`ts`, the ledger's clock),
/// since which a person's typing the window stamps makes a `/usage` standing theirs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeWrote {
    /// The sequence of the screen the write was judged on.
    pub seq: u64,
    /// The window's gesture count the write was fenced on.
    pub human: Option<u64>,
    /// When the row was booked, just past the write, in Unix milliseconds.
    pub ts: Option<i64>,
}

/// The accounts probe's last write for `sid` in the ledger text `body`, where its last row is
/// that write's `typed` one (no `closed` row after it): the probe a restart owes.
#[must_use]
pub fn probe_wrote(body: &str, sid: Option<&str>) -> Option<ProbeWrote> {
    let sid = sid.map_or_else(
        || "null".to_string(),
        |s| json_str(s.trim_start_matches('@')),
    );
    let want_sid = format!(",\"sid\":{sid},");
    let probe_rule = probe_rule_field();
    let line = body
        .lines()
        .rev()
        .find(|l| l.contains(&want_sid) && l.contains(&probe_rule))
        .filter(|l| l.contains(",\"decision\":\"typed\","))?;
    let v = aterm_json::from_str::<aterm_json::Value>(line).ok();
    let field = |key: &str| {
        v.as_ref()
            .and_then(|v| v.get(key))
            .and_then(aterm_json::Value::as_u64)
    };
    Some(ProbeWrote {
        seq: field("box_seq").unwrap_or(0),
        human: field(PROBE_HUMAN),
        ts: v
            .as_ref()
            .and_then(|v| v.get("ts"))
            .and_then(aterm_json::Value::as_i64),
    })
}

fn probe_rule_field() -> String {
    format!(
        ",\"rule_id\":{},",
        json_str(super::policy::turn_end::RULE_USAGE_PROBE)
    )
}

/// Whether the accounts probe's last write for `sid` is owed its close ([`probe_wrote`]).
#[must_use]
pub fn probe_owed(body: &str, sid: Option<&str>) -> bool {
    probe_wrote(body, sid).is_some()
}

pub const RESET_YES_2_1_287: &str = "Yes, use my reset";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetOpen {
    pub entered: bool,
    pub taken_ms: i64,
}

#[must_use]
pub fn reset_owed(body: &str, sid: Option<&str>) -> Option<ResetOpen> {
    let sid = sid.map_or_else(
        || "null".to_string(),
        |s| json_str(s.trim_start_matches('@')),
    );
    let want_sid = format!(",\"sid\":{sid},");
    let rule = format!(
        ",\"rule_id\":{},",
        json_str(super::policy::approval::RULE_LIMIT_RESET)
    );
    let yeses: Vec<String> = aterm_phase::ResetKind::ALL
        .into_iter()
        .map(aterm_phase::ResetKind::yes)
        .chain([RESET_YES_2_1_287.to_string()])
        .map(|yes| format!(",\"command\":{},", json_str(&yes)))
        .collect();
    let mut owed: Option<ResetOpen> = None;
    for row in body
        .lines()
        .filter(|l| l.contains(&want_sid) && l.contains(&rule))
    {
        if row.contains(",\"decision\":\"waiting\",") {
            owed = None;
        } else if row.contains(",\"decision\":\"approved\",") {
            let entered = yeses.iter().any(|yes| row.contains(yes.as_str()));
            owed = Some(match owed {
                Some(open) => ResetOpen {
                    entered: open.entered || entered,
                    ..open
                },
                None => ResetOpen {
                    entered,
                    taken_ms: aterm_json::from_str::<aterm_json::Value>(row)
                        .ok()
                        .and_then(|v| v.get("ts").and_then(aterm_json::Value::as_i64))
                        .unwrap_or(0),
                },
            });
        }
    }
    owed
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAttempt {
    pub conversation: Conversation,
    pub program: Option<aterm_phase::Program>,
    pub at_ms: i64,
    pub limit: LimitTry,
    pub met_ms: Option<i64>,
    pub read_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AwaitedTry {
    pub at_ms: i64,
    pub read_ms: Option<i64>,
    pub unread: bool,
    pub ended: bool,
    pub met: bool,
}

#[must_use]
pub fn awaited_try_in(body: &str, sid: Option<&str>) -> Option<AwaitedTry> {
    use super::policy::turn_end::{RULE_LIMIT_RESUME, RULE_USAGE_PROBE};
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<AwaitedTry> = None;
    for v in act_lines(body)
        .filter(|l| {
            l.contains(",\"decision\":\"typed\",") || l.contains(",\"decision\":\"waiting\",")
        })
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
    {
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        let rule = text("rule_id").unwrap_or("");
        let ts = v.get("ts").and_then(aterm_json::Value::as_i64);
        if text("decision") == Some("typed") {
            // The accounts probe's `/usage` is no act; an account move made
            // relaunched the agent, and nothing typed before it is awaited.
            if rule == RULE_USAGE_PROBE {
                continue;
            }
            let tried = match text("reason").and_then(limit_try_of) {
                Some(l) => !l.in_place,
                None => rule == RULE_LIMIT_RESUME,
            };
            last = ts.filter(|_| tried).map(|at_ms| AwaitedTry {
                at_ms,
                read_ms: None,
                unread: true,
                ended: false,
                met: false,
            });
            continue;
        }
        let Some(a) = last.as_mut() else {
            continue;
        };
        if rule == RULE_POINT {
            last = None;
            continue;
        }
        if !matches!(rule, RULE_WALL_WAIT | RULE_WALL_WAIT_END) {
            continue;
        }
        let first = std::mem::replace(&mut a.unread, false);
        match rule {
            RULE_WALL_WAIT => {
                if first {
                    a.read_ms = ts;
                }
                a.met = true;
            }
            RULE_WALL_WAIT_END => {
                a.ended = true;
                if text("reason") == Some(WALL_TAKEN) {
                    a.met = false;
                }
            }
            _ => {}
        }
    }
    last
}

#[must_use]
pub fn awaited_try(path: &Path, sid: Option<&str>) -> Option<AwaitedTry> {
    awaited_try_in(
        &crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024).ok()??,
        sid,
    )
}

#[must_use]
pub fn usage_attempt_in(body: &str, sid: Option<&str>) -> Option<UsageAttempt> {
    usage_attempt_taken_in(body, sid, &|_, _| false)
}

#[must_use]
pub fn usage_attempt_taken_in(
    body: &str,
    sid: Option<&str>,
    taken: &dyn Fn(i64, i64) -> bool,
) -> Option<UsageAttempt> {
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<UsageAttempt> = None;
    let mut standing: Option<LimitTry> = None;
    let mut seen = false;
    let mut untaken = false;
    let mut ended = false;
    let mut took_turn = false;
    let mut wall_program = None;
    for v in act_lines(body)
        .filter(|l| {
            l.contains(",\"decision\":\"typed\",") || l.contains(",\"decision\":\"waiting\",")
        })
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
    {
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        let read = match text("decision") {
            Some("typed") => text("rule_id") != Some(super::policy::turn_end::RULE_USAGE_PROBE),
            _ => matches!(
                text("rule_id"),
                Some(RULE_WALL_WAIT | RULE_WALL_WAIT_END | RULE_TRY_ANSWERED)
            ),
        };
        let first = read && last.is_some() && !std::mem::replace(&mut seen, true);
        if text("decision") == Some("waiting") {
            // Existing point/answer/wall records can retain an upgraded
            // unknown even after the try closed OpenWall. No new row kind.
            if matches!(
                text("rule_id"),
                Some(RULE_POINT | RULE_TRY_ANSWERED | RULE_WALL_WAIT)
            ) && v.get("conversation").is_some()
                && let Some(a) = last.as_mut()
                && row_conversation(&v) != a.conversation
            {
                // A later wall/point cannot relabel the actual try as
                // another conversation, nor prove a legacy try's owner.
                a.conversation = Conversation::Unknown;
            }
            if text("rule_id") == Some(RULE_WALL_WAIT) {
                wall_program = row_program(&v);
            }
            let took = text("rule_id") == Some(RULE_WALL_WAIT_END)
                && std::mem::take(&mut untaken)
                && last.as_ref().is_some_and(|a| {
                    v.get("ts")
                        .and_then(aterm_json::Value::as_i64)
                        .is_some_and(|end| taken(a.at_ms, end))
                });
            match text("rule_id") {
                Some(RULE_WALL_WAIT_END) => {
                    standing = None;
                    ended = true;
                }
                Some(RULE_WALL_WAIT) => {
                    if let Some(row) = text("reason").and_then(wall_wait_of).filter(|r| r.older) {
                        let kind = wall_kind(row.kind, row.message);
                        let bucket =
                            matches!(kind, Some(aterm_phase::WallKind::ModelBucket { .. }));
                        standing = Some(LimitTry {
                            bucket,
                            in_place: false,
                            attempts: standing
                                .filter(|s| s.bucket == bucket)
                                .map_or(0, |s| s.attempts),
                            entitlement: kind.is_some_and(|k| {
                                super::policy::turn_end::entitlement_ladder(
                                    row.reset_unix.is_some(),
                                    k,
                                )
                            }) && !super::limit::resumes_by_itself(row.message),
                        });
                    }
                }
                _ => {}
            }
            match text("rule_id") {
                Some(RULE_WALL_WAIT_END)
                    if matches!(
                        text("reason"),
                        Some(LIMIT_ENDED | LIMIT_ENDED_ELSEWHERE | WALL_LEFT_NO_TASK)
                    ) =>
                {
                    last = None;
                }
                Some(RULE_TRY_OVER) => last = None,
                Some(RULE_TRY_ANSWERED) => {
                    if let Some(a) = last.as_mut() {
                        let read = text("reason")
                            .and_then(try_answered_of)
                            .or_else(|| v.get("ts").and_then(aterm_json::Value::as_i64));
                        if first {
                            a.read_ms = read;
                        }
                        a.met_ms = a.met_ms.or(read);
                    }
                }
                Some(RULE_WALL_WAIT_END) if took || text("reason") == Some(WALL_TAKEN) => {
                    if let Some(a) = last.as_mut() {
                        a.met_ms = None;
                    }
                    took_turn = took;
                }
                Some(RULE_WALL_WAIT) if std::mem::take(&mut took_turn) => last = None,
                Some(RULE_WALL_WAIT) => {
                    let Some(row) = text("reason").and_then(wall_wait_of) else {
                        continue;
                    };
                    let kind = wall_kind(row.kind, row.message);
                    if let Some(a) = last.as_mut()
                        && of_try(kind, a.limit)
                    {
                        let ts = v.get("ts").and_then(aterm_json::Value::as_i64);
                        if first {
                            a.read_ms = ts;
                        }
                        if a.met_ms.is_none() && row.older && !ended {
                            untaken = true;
                        }
                        a.met_ms = a.met_ms.or(ts);
                        a.limit.entitlement = kind.is_some_and(|k| {
                            super::policy::turn_end::entitlement_ladder(row.reset_unix.is_some(), k)
                        }) && !super::limit::resumes_by_itself(row.message);
                    }
                }
                _ => {}
            }
            continue;
        }
        let unmarked = || {
            (text("rule_id") == Some(super::policy::turn_end::RULE_LIMIT_RESUME)).then(|| {
                standing.map_or(USAGE_RESUME_UNMARKED, |s| LimitTry {
                    attempts: s.attempts.saturating_add(1),
                    ..s
                })
            })
        };
        let Some(limit) = text("reason").and_then(limit_try_of).or_else(unmarked) else {
            continue;
        };
        if let Some(s) = standing.as_mut() {
            s.attempts = limit.attempts;
        }
        let Some(ts) = v.get("ts").and_then(aterm_json::Value::as_i64) else {
            continue;
        };
        if last.as_ref().is_none_or(|a| ts >= a.at_ms) {
            last = Some(UsageAttempt {
                conversation: row_conversation(&v),
                program: wall_program,
                at_ms: ts,
                limit,
                met_ms: None,
                read_ms: None,
            });
            seen = false;
            untaken = false;
            ended = false;
            took_turn = false;
        }
    }
    let last = last?;
    let moved = account_moves_in(body, sid).made_ms.into_iter().max();
    moved.is_none_or(|moved| last.at_ms > moved).then_some(last)
}

#[must_use]
pub fn usage_attempt(path: &Path, sid: Option<&str>) -> Option<UsageAttempt> {
    usage_attempt_in(
        &crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024).ok()??,
        sid,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckOwed {
    pub rule: &'static str,
    pub cut: bool,
    pub check: Option<(i64, bool)>,
    pub read_ms: Option<i64>,
    pub unread: bool,
    pub answered: bool,
    pub compact: bool,
}

#[must_use]
pub fn done_check_owed_in(body: &str, sid: Option<&str>) -> Option<CheckOwed> {
    let (owed, judged) = walk_done_check(body, sid);
    owed.filter(|o| judged == 0 || o.answered)
}

#[must_use]
pub fn check_point_last_in(body: &str, sid: Option<&str>) -> Option<&'static str> {
    let (owed, judged) = walk_done_check(body, sid);
    owed.filter(|o| judged == 1 && !o.answered && !o.cut && !o.compact)
        .map(|o| o.rule)
}

fn walk_done_check(body: &str, sid: Option<&str>) -> (Option<CheckOwed>, u32) {
    use super::policy::turn_end::{
        RULE_COMPACT, RULE_DONE_CHECK, RULE_OPEN_ITEMS, RULE_UNCOMMITTED, RULE_USAGE_PROBE,
        is_wall_retry,
    };
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut owed: Option<CheckOwed> = None;
    let mut judged = 0_u32;
    let mut api_cut = false;
    for v in act_lines(body)
        .filter(|l| {
            l.contains(",\"decision\":\"typed\",") || l.contains(",\"decision\":\"waiting\",")
        })
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
    {
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        let rule = text("rule_id").unwrap_or("");
        if text("decision") == Some("typed") {
            let judged_before = judged;
            if rule != RULE_USAGE_PROBE {
                judged = 0;
                api_cut = false;
            }
            owed = match rule {
                RULE_USAGE_PROBE => owed,
                RULE_DONE_CHECK | RULE_OPEN_ITEMS | RULE_UNCOMMITTED => {
                    let in_place = text("reason")
                        .and_then(limit_try_of)
                        .is_some_and(|l| l.in_place);
                    let check = v
                        .get("ts")
                        .and_then(aterm_json::Value::as_i64)
                        .map(|ts| (ts, in_place));
                    Some(CheckOwed {
                        rule: match rule {
                            RULE_OPEN_ITEMS => RULE_OPEN_ITEMS,
                            RULE_UNCOMMITTED => RULE_UNCOMMITTED,
                            _ => RULE_DONE_CHECK,
                        },
                        cut: false,
                        check,
                        read_ms: None,
                        unread: in_place && check.is_some(),
                        answered: false,
                        compact: false,
                    })
                }
                RULE_COMPACT
                    if text("command")
                        .is_some_and(|c| c.trim() == crate::harness::steps::COMPACT)
                        && owed.is_some_and(|o| o.compact || o.cut || judged_before == 1) =>
                {
                    owed.map(|o| CheckOwed {
                        cut: false,
                        check: None,
                        read_ms: None,
                        unread: false,
                        answered: false,
                        compact: true,
                        ..o
                    })
                }
                r if is_wall_retry(r)
                    && owed.is_some_and(|o| {
                        o.cut || (r == RULE_COMPACT && delivers_check(text("command")))
                    }) =>
                {
                    owed.map(|o| CheckOwed {
                        cut: false,
                        check: None,
                        read_ms: None,
                        unread: false,
                        answered: false,
                        compact: false,
                        ..o
                    })
                }
                _ => None,
            };
            continue;
        }
        if matches!(rule, RULE_WALL_WAIT | RULE_WALL_WAIT_END | RULE_POINT)
            && let Some(o) = owed.as_mut()
        {
            o.unread = false;
        }
        match rule {
            RULE_WALL_WAIT => {
                if let Some(o) = owed.as_mut().filter(|o| !o.compact) {
                    if !o.cut {
                        o.read_ms = v.get("ts").and_then(aterm_json::Value::as_i64);
                    }
                    o.cut = true;
                }
                api_cut = false;
            }
            RULE_WALL_WAIT_END => {
                owed = match owed {
                    Some(o) if o.cut && text("reason") == Some(WALL_TAKEN) => Some(CheckOwed {
                        cut: false,
                        read_ms: None,
                        ..o
                    }),
                    Some(o) if o.cut => None,
                    other => other,
                };
            }
            RULE_OFF_STOOD => {
                owed = owed.filter(|o| !o.cut && !o.compact && (judged == 0 || o.answered));
            }
            RULE_POINT
                if judged == 0
                    && owed.is_some_and(|o| !o.cut && !o.compact)
                    && text("reason").and_then(retry_mark_of).is_some() =>
            {
                if let Some(o) = owed.as_mut() {
                    o.cut = true;
                    o.check = None;
                    o.read_ms = None;
                }
                api_cut = true;
            }
            RULE_POINT if api_cut && owed.is_some_and(|o| o.cut) => {
                owed = None;
                api_cut = false;
            }
            RULE_POINT => {
                if let Some(o) = owed.as_mut().filter(|o| !o.cut && !o.compact) {
                    judged += 1;
                    o.check = None;
                    o.read_ms = None;
                }
                if judged > 1 {
                    owed = None;
                }
            }
            RULE_TASK_DONE => {
                if let Some(o) = owed.as_mut() {
                    o.answered = true;
                }
            }
            _ => {}
        }
    }
    (owed, judged)
}

fn delivers_check(command: Option<&str>) -> bool {
    use super::policy::turn_end::{DONE_CHECK, RETIRED_DONE_CHECKS, is_challenge_text};
    command.is_some_and(|c| {
        std::iter::once(DONE_CHECK)
            .chain(RETIRED_DONE_CHECKS)
            .any(|head| opens_with(head, c))
            || is_challenge_text(c)
    })
}

pub(crate) fn delivers_land(command: Option<&str>) -> bool {
    use super::policy::turn_end::{
        LAND, LAND_ANSWER, LAND_CHECK_HEAD, LAND_CONTRADICTED_HEAD, LAND_NUDGE,
        RETIRED_LAND_ANSWERS, RETIRED_LAND_NUDGES, RETIRED_LANDS, past_switch_why,
    };
    command.is_some_and(|c| {
        let c = past_switch_why(c);
        let c = c.as_ref();
        [
            LAND,
            LAND_NUDGE,
            LAND_CONTRADICTED_HEAD,
            LAND_ANSWER,
            LAND_CHECK_HEAD,
        ]
        .into_iter()
        .chain(RETIRED_LANDS)
        .chain(RETIRED_LAND_NUDGES)
        .chain(RETIRED_LAND_ANSWERS)
        .any(|head| opens_with(head, c))
    })
}

/// Whether `command` opens on Land's instruction, now's or a retired build's (no nudge, no answer).
fn gives_land(command: Option<&str>) -> bool {
    use super::policy::turn_end::{LAND, RETIRED_LANDS, past_switch_why};
    command.is_some_and(|c| {
        let c = past_switch_why(c);
        let c = c.as_ref();
        std::iter::once(LAND)
            .chain(RETIRED_LANDS)
            .any(|head| opens_with(head, c))
    })
}

fn opens_with(head: &str, text: &str) -> bool {
    let mut words = text.split_whitespace();
    head.split_whitespace().all(|w| words.next() == Some(w))
}

#[must_use]
pub fn done_check_owed(path: &Path, sid: Option<&str>) -> Option<CheckOwed> {
    done_check_owed_in(
        &crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024).ok()??,
        sid,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandSeed {
    pub nudges: u32,
    pub landing: bool,
    pub cut: bool,
    pub answered: bool,
    pub act: Option<LandAct>,
    pub hold: super::policy::turn_end::HoldStart,
    pub compact: bool,
    pub judged: bool,
    pub owed: Option<&'static str>,
    /// THE LAST LAND ROW WAS THE LAND CHECK (`land-check@v1`): a restarted loop judges the reply
    /// it reads as the check's answer, so a `LANDED` there lands
    /// ([`super::policy::turn_end::TurnEndState::seed_land_owed`]).
    pub checked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandAct {
    pub at_ms: i64,
    pub said: u64,
    pub lead: u64,
}

#[must_use]
pub fn land_episode_in(body: &str, sid: Option<&str>) -> Option<LandSeed> {
    use super::policy::turn_end::RULE_USAGE_PROBE;
    let want = sid.map(|s| s.trim_start_matches('@'));
    let mut walk = LandWalk::default();
    let mut act = None;
    for v in act_lines(body)
        .filter(|l| LandWalk::reads(l))
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
    {
        if v.get("sid").and_then(aterm_json::Value::as_str) != want {
            continue;
        }
        walk.step(&v);
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        let rule = text("rule_id").unwrap_or("");
        if text("decision") == Some("typed") && rule != RULE_USAGE_PROBE {
            act = walk
                .ep
                .filter(|e| e.landing)
                .and(v.get("ts").and_then(aterm_json::Value::as_i64))
                .zip(text("command"))
                .map(|(at_ms, said)| LandAct {
                    at_ms,
                    said: words_hash(said),
                    lead: words_lead(said),
                });
        }
    }
    walk.answer().map(|e| LandSeed {
        act: act.filter(|_| e.landing),
        ..e
    })
}

fn land_rule(rule: &str) -> Option<&'static str> {
    use super::policy::turn_end::{RULE_LAND, RULE_LAND_ANSWER, RULE_LAND_CHECK, RULE_LAND_NUDGE};
    [
        RULE_LAND,
        RULE_LAND_NUDGE,
        RULE_LAND_ANSWER,
        RULE_LAND_CHECK,
    ]
    .into_iter()
    .find(|r| *r == rule)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LandWalk {
    ep: Option<LandSeed>,
    judged: u32,
}

impl LandWalk {
    fn reads(line: &str) -> bool {
        line.contains(",\"decision\":\"typed\",") || line.contains(",\"decision\":\"waiting\",")
    }

    fn step(&mut self, v: &aterm_json::Value) {
        self.read(v);
        if self.ep.is_none() {
            self.judged = 0;
        }
    }

    fn read(&mut self, v: &aterm_json::Value) {
        use super::policy::turn_end::{
            HoldStart, RULE_COMPACT, RULE_LAND, RULE_LAND_NUDGE, RULE_USAGE_PROBE, is_cleared_line,
            is_continue_rule, is_land_rule, is_wall_retry,
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        let rule = text("rule_id").unwrap_or("");
        match text("decision") {
            Some("typed") => {
                let judged = self.judged;
                let owed = self.ep.and_then(|e| e.owed);
                let was_checked = self.ep.is_some_and(|e| e.checked);
                if rule != RULE_USAGE_PROBE {
                    self.judged = 0;
                }
                let land = |nudges: u32, landing: bool, hold: HoldStart| LandSeed {
                    nudges,
                    landing,
                    cut: false,
                    answered: false,
                    act: None,
                    hold,
                    compact: false,
                    judged: false,
                    owed: None,
                    checked: false,
                };
                let begun = |base: u32| HoldStart {
                    since_ms: v.get("ts").and_then(aterm_json::Value::as_i64),
                    base,
                    relanded: false,
                };
                // Land's instruction typed under another act's rule (a resume past a condition, or
                // a compaction's owed continuation, before the instruction was given) starts the
                // episode as Land's own act does, as the loop's `land_given` reads it.
                let rule = if self.ep.is_none() && gives_land(text("command")) {
                    RULE_LAND
                } else {
                    rule
                };
                self.ep = match (rule, self.ep) {
                    (RULE_USAGE_PROBE, ep) => ep,
                    (RULE_LAND, _) => Some(land(0, true, begun(0))),
                    (r, Some(e)) if is_land_rule(r) => {
                        let nudges = e.nudges + u32::from(r == RULE_LAND_NUDGE);
                        let hold = if e.hold.relanded {
                            begun(nudges)
                        } else {
                            e.hold
                        };
                        Some(land(nudges, true, hold))
                    }
                    (r, _) if is_continue_rule(r) => None,
                    (RULE_COMPACT, Some(e))
                        if text("command")
                            .is_some_and(|c| c.trim() == crate::harness::steps::COMPACT)
                            && (e.compact || e.cut || (e.landing && judged == 1)) =>
                    {
                        Some(LandSeed {
                            compact: true,
                            ..land(e.nudges, false, e.hold)
                        })
                    }
                    // A resume past a condition goes on with what Land asked, as the loop judges
                    // it ([`super::policy::turn_end::TurnEndState::acted`]).
                    (r, Some(e)) => Some(land(
                        e.nudges,
                        (e.cut && is_wall_retry(r))
                            || (r == RULE_COMPACT && delivers_land(text("command")))
                            || text("command").is_some_and(is_cleared_line),
                        e.hold,
                    )),
                    (_, None) => None,
                };
                if rule != RULE_USAGE_PROBE
                    && let Some(e) = self.ep.as_mut()
                {
                    e.checked = land_rule(rule).map_or(was_checked, |r| {
                        r == super::policy::turn_end::RULE_LAND_CHECK
                    });
                    e.owed = if let Some(rule) = land_rule(rule) {
                        Some(rule)
                    } else if e.compact || (e.landing && is_wall_retry(rule)) {
                        owed
                    } else {
                        None
                    };
                }
                return;
            }
            Some("waiting") => {}
            _ => return,
        }
        let Some(e) = self.ep.as_mut() else {
            return;
        };
        match rule {
            RULE_OFF_STOOD if text("reason") == Some(OFF_STOOD_RESTING) => {}
            RULE_LAND_OVER | RULE_OFF_STOOD => self.ep = None,
            RULE_TASK_DONE => {
                e.answered = true;
                e.hold = HoldStart::LANDED;
            }
            RULE_POINT
                if e.landing
                    && !e.cut
                    && self.judged == 0
                    && text("reason").and_then(retry_mark_of).is_some() =>
            {
                e.landing = false;
                e.cut = true;
            }
            RULE_POINT if e.landing && !e.cut => {
                self.judged += 1;
                if self.judged > 1 {
                    e.landing = false;
                }
            }
            RULE_POINT if e.cut && !e.landing => e.cut = false,
            RULE_WALL_WAIT if e.landing => {
                e.landing = false;
                e.cut = true;
            }
            RULE_WALL_WAIT_END if e.cut && text("reason") == Some(WALL_TAKEN) => {
                e.landing = true;
                e.cut = false;
            }
            RULE_WALL_WAIT_END => e.cut = false,
            _ => {}
        }
    }

    fn answer(self) -> Option<LandSeed> {
        self.ep.map(|mut e| {
            e.judged = e.landing && self.judged == 1 && !e.answered;
            e.landing &= self.judged == 0 || e.answered;
            e.owed = e
                .owed
                .filter(|_| e.landing || e.cut || e.compact || e.judged);
            e
        })
    }
}

#[must_use]
pub fn land_episode(path: &Path, sid: Option<&str>) -> Option<LandSeed> {
    land_episode_in(
        &crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024).ok()??,
        sid,
    )
}

const fn of_try(kind: Option<aterm_phase::WallKind>, limit: LimitTry) -> bool {
    use aterm_phase::WallKind;
    match kind {
        Some(WallKind::ModelBucket { .. }) => limit.bucket,
        Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend) => !limit.bucket,
        _ => false,
    }
}

const USAGE_RESUME_UNMARKED: LimitTry = LimitTry {
    bucket: false,
    in_place: false,
    attempts: 1,
    entitlement: false,
};

const IN_LIMITS_PLACE: &str = "in the limit's place";

const LIMITS_TRY: &str = "the limit's try";

const fn limit_word(bucket: bool) -> &'static str {
    if bucket { "bucket" } else { "usage" }
}

const ENTITLEMENT_LADDER: &str = "ladder=entitlement";

fn limit_try_of(reason: &str) -> Option<LimitTry> {
    let (_, group) = reason.rsplit_once(" (")?;
    let (head, fields) = group.strip_suffix(')')?.split_once(": ")?;
    let in_place = match head {
        IN_LIMITS_PLACE => true,
        LIMITS_TRY => false,
        _ => return None,
    };
    let field = |name: &str| {
        fields
            .split_whitespace()
            .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
    };
    let bucket = match field("wall")? {
        w if w == limit_word(true) => true,
        w if w == limit_word(false) => false,
        _ => return None,
    };
    let attempts = field("attempts")
        .and_then(crate::parse_uint::<usize>)
        .unwrap_or(1);
    Some(LimitTry {
        bucket,
        in_place,
        attempts,
        entitlement: fields.split_whitespace().any(|f| f == ENTITLEMENT_LADDER),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreakSeed {
    pub at_ms: i64,
    pub short: u32,
    pub point: bool,
    pub rule: String,
    pub recorded: bool,
    pub ran: bool,
    pub ran_ms: Option<i64>,
    pub esc: bool,
    pub retry: Option<RetryMark>,
    /// The short streak's replies ([`Carried::said`]).
    pub said: Replies,
    /// The reset the newest of them showed ([`Carried::said_reset_ms`]).
    pub said_reset_ms: Option<i64>,
    /// The done checks of the Cruise run so far ([`Carried::checks`]).
    pub checks: u8,
    /// The land check's books ([`Carried::land`]).
    pub land: LandBooks,
    /// THE HELD REPLY THE PERSON'S SWITCH'S ONE ASK WAS SPENT ON
    /// ([`super::policy::turn_end::TurnEndState::seed_switch_spent`]): the last kept mark of the
    /// row that typed the ask, carried past the rows after it until an Off the person set.
    pub switch_spent: Option<u64>,
    /// The last point's named program. A subsequent typed act keeps it; legacy unnamed points
    /// preserve their old unscoped streak behavior.
    pub program: Option<aterm_phase::Program>,
    /// The wall's pass at the row ([`Carried::pass`]): none from an older build's row.
    pub pass: WallPass,
    /// The needs set acked ([`Carried::needs_acked`]), as the latest point row or the latest row
    /// that notes one says: a box's key or a restart typed since tells nothing of it.
    pub needs_acked: u64,
}

#[must_use]
pub fn short_streak_in(body: &str, sid: Option<&str>) -> Option<StreakSeed> {
    use super::policy::turn_end::{
        RULE_ACCOUNT, RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE, RULE_USAGE_PROBE,
    };
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<StreakSeed> = None;
    let mut recorded = false;
    let mut acked: Option<(i64, u64)> = None;
    for v in act_lines(body)
        .filter(|l| {
            l.contains(",\"decision\":\"typed\",") || l.contains(",\"decision\":\"waiting\",")
        })
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
    {
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        let rule = text("rule_id").unwrap_or("");
        if rule == RULE_OFF_STOOD {
            if let Some(l) = last.as_mut() {
                if OffStood::of_reason(text("reason")) == OffStood::Person {
                    l.said = Replies::default();
                    l.said_reset_ms = None;
                    l.switch_spent = None;
                }
                l.checks = 0;
            }
            continue;
        }
        if rule == RULE_TURN_RAN {
            match last.as_mut() {
                Some(l) if l.point => {
                    l.ran = true;
                    if l.ran_ms.is_none() {
                        l.ran_ms = v.get("ts").and_then(aterm_json::Value::as_i64);
                    }
                }
                Some(_) => {}
                None => {
                    if let Some(at_ms) = v.get("ts").and_then(aterm_json::Value::as_i64) {
                        last = Some(StreakSeed {
                            at_ms,
                            short: 0,
                            point: true,
                            rule: rule.to_string(),
                            recorded: false,
                            ran: true,
                            ran_ms: Some(at_ms),
                            esc: false,
                            retry: None,
                            said: Replies::default(),
                            said_reset_ms: None,
                            checks: 0,
                            land: LandBooks::default(),
                            switch_spent: None,
                            program: None,
                            pass: WallPass::default(),
                            needs_acked: 0,
                        });
                    }
                }
            }
            continue;
        }
        let point = match text("decision") {
            Some("typed")
                if matches!(
                    rule,
                    RULE_USAGE_PROBE | RULE_MODEL_FALLBACK | RULE_MODEL_RESTORE
                ) || (rule == RULE_ACCOUNT
                    && text("command").is_some_and(|c| c.starts_with(ACCOUNT_MOVE_COMMAND))) =>
            {
                continue;
            }
            Some("typed") => false,
            _ if rule == RULE_POINT => {
                recorded = true;
                true
            }
            _ => continue,
        };
        let Some(at_ms) = v.get("ts").and_then(aterm_json::Value::as_i64) else {
            continue;
        };
        let noted = text("reason").and_then(needs_acked_note_of);
        if (point || noted.is_some()) && acked.is_none_or(|(ms, _)| at_ms >= ms) {
            acked = Some((at_ms, noted.unwrap_or(0)));
        }
        if last.as_ref().is_some_and(|l| at_ms < l.at_ms) {
            continue;
        }
        let short = text("reason").and_then(short_streak_of).unwrap_or(0);
        let esc = point && text("reason").is_some_and(|r| r.contains(&format!("({PERSONS_ESC})")));
        let (said, said_reset_ms) = text("reason").and_then(replies_note_of).unwrap_or_default();
        let checks = text("reason").and_then(checks_note_of).unwrap_or(0);
        let land = text("reason").and_then(land_books_of).unwrap_or_default();
        let switch_spent =
            if !point && text("command").is_some_and(super::policy::turn_end::is_switch_ask) {
                said.last().map(|k| k.mark)
            } else {
                last.as_ref().and_then(|l| l.switch_spent)
            };
        let program = if point {
            row_program(&v).filter(|p| *p != aterm_phase::Program::Generic)
        } else {
            last.as_ref().and_then(|l| l.program)
        };
        last = Some(StreakSeed {
            at_ms,
            short,
            point,
            rule: rule.to_string(),
            recorded: false,
            ran: false,
            ran_ms: None,
            esc,
            retry: text("reason").and_then(retry_mark_of),
            said,
            said_reset_ms,
            checks,
            land,
            switch_spent,
            program,
            pass: text("reason").map(wall_pass_of).unwrap_or_default(),
            needs_acked: 0,
        });
    }
    let needs_acked = acked.map_or(0, |(_, key)| key);
    last.map(|l| StreakSeed {
        recorded,
        needs_acked,
        ..l
    })
}

/// THE SHORT STREAK A ROW'S REASON NAMES ([`point_reason`], [`act_reason_after`]): its `(short
/// turns in a row: N)`, `None` where it names none.
#[must_use]
pub fn short_streak_of(reason: &str) -> Option<u32> {
    let (_, rest) = reason.split_once(&format!("({SHORT_STREAK}: "))?;
    crate::parse_uint(rest.split(')').next()?)
}

/// CRUISE'S CARRIES IN THE BOOKS: when each of `sid`'s carries at a break of its own background
/// work was typed (unix ms, oldest first) and the streak its row names
/// ([`background_carry_reason`]), since its books last started again on a row, so a loop after
/// the ones that typed them replays their ladder
/// ([`super::policy::cruise_carry::BackgroundCarries::replayed`]). A carry is any act typed whose
/// row names its streak (the go-ahead or the continuation owed typed in the carry's place at a
/// break goes on its ladder), or a [`RULE_BACKGROUND`](super::policy::cruise_carry::RULE_BACKGROUND) row written before rows
/// named it (no streak). The run ends, newest first, at an act of another rule typed, a point
/// judged, the books started again in memory
/// ([`RULE_BACKGROUND_RESET`](super::policy::cruise_carry::RULE_BACKGROUND_RESET)), a person's own
/// exchange left to them, the session lifted off Off, or a carry that names no time. A turn read
/// running after a point ([`RULE_TURN_RAN`]) starts no books: the point's own row ends the run,
/// and a loop's first live read writes one with no point before it. Empty with none since.
#[must_use]
pub fn background_carries_in(body: &str, sid: Option<&str>) -> Vec<(i64, Option<u32>)> {
    use super::policy::cruise_carry::{RULE_BACKGROUND, RULE_BACKGROUND_RESET};
    use super::policy::turn_end::RULE_LEFT;
    let want = sid.map(|s| s.trim_start_matches('@'));
    let mut run = Vec::new();
    for line in act_lines(body).rev() {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid") != want {
            continue;
        }
        match (text("rule_id").unwrap_or(""), text("decision")) {
            (rule, Some("typed")) => {
                let streak = text("reason").and_then(carry_streak_of);
                let Some(ts) = v.get("ts").and_then(aterm_json::Value::as_i64) else {
                    break;
                };
                if streak.is_none() && rule != RULE_BACKGROUND {
                    break;
                }
                run.push((ts, streak));
            }
            (RULE_POINT | RULE_OFF_STOOD | RULE_BACKGROUND_RESET, _)
            | (RULE_LEFT, Some("skipped")) => break,
            _ => {}
        }
    }
    run.reverse();
    run
}

/// The note a carry's row names its streak in ([`background_carry_reason`]).
const CARRY_STREAK: &str = "carries in a row";

/// A carry's row's reason: `why`, and the `streak` the live ladder typed it at (its books'
/// streak with this carry counted), which a later loop's replay takes up as it stands
/// ([`background_carries_in`]): books started again in memory alone (a turn of real work,
/// someone else's message) show in no gap between rows. A go-ahead or a continuation owed typed
/// in the carry's place names it too.
#[must_use]
pub fn background_carry_reason(why: &str, streak: u32) -> String {
    format!("{why} ({CARRY_STREAK}: {streak})")
}

/// The streak a carry's row names ([`background_carry_reason`]).
fn carry_streak_of(reason: &str) -> Option<u32> {
    let (_, rest) = reason.split_once(&format!("({CARRY_STREAK}: "))?;
    rest.split(')').next()?.parse().ok()
}

#[must_use]
pub fn carry_ended_in(body: &str, sid: Option<&str>) -> bool {
    use super::policy::turn_end::{RULE_ACCOUNT, RULE_LEFT, RULE_USAGE_PROBE};
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let esc = format!("({PERSONS_ESC})");
    let (mut ended, mut judged) = (false, false);
    for line in act_lines(body) {
        if !line.contains(",\"decision\":\"") {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        let rule = text("rule_id").unwrap_or("");
        match text("decision") {
            Some("typed")
                if rule == RULE_USAGE_PROBE
                    || (rule == RULE_ACCOUNT
                        && text("command")
                            .is_some_and(|c| c.starts_with(ACCOUNT_MOVE_COMMAND))) => {}
            Some("typed") => (ended, judged) = (false, false),
            Some("waiting") if rule == RULE_POINT => {
                ended |= !judged && text("reason").is_some_and(|r| r.contains(&esc));
                judged = true;
            }
            Some("waiting") if rule == RULE_TASK_DONE => ended = true,
            Some("waiting") if rule == RULE_OFF_STOOD && judged => ended = true,
            Some("skipped") if rule == RULE_LEFT => ended = true,
            _ => {}
        }
    }
    ended
}

#[must_use]
pub fn short_streak(path: &Path, sid: Option<&str>) -> Option<StreakSeed> {
    short_streak_in(
        &crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024).ok()??,
        sid,
    )
}

pub const RULE_TURN_RAN: &str = "turn-ran@v1";

pub const TURN_RAN: &str = "a turn was read running after the point the turn-end policy judged";

pub const RULE_POINT: &str = "point@v1";

#[must_use]
pub fn point_reason(after: Carried, esc: bool) -> String {
    format!(
        "the turn-end policy judged the point ({SHORT_STREAK}: {}){}{}{}{}{}{}{}",
        after.short,
        retry_mark(after.retry),
        replies_note(after.said, after.said_reset_ms),
        checks_note(after.checks),
        land_books_note(after.land),
        wall_pass_note(after.pass),
        needs_acked_note(after.needs_acked),
        if esc {
            format!(" ({PERSONS_ESC})")
        } else {
            String::new()
        }
    )
}

const PERSONS_ESC: &str = "a person's Esc stood at the point";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Carried {
    pub short: u32,
    pub retry: Option<RetryMark>,
    /// The short streak's replies ([`super::policy::turn_end::TurnEndState::replies`]).
    pub said: Replies,
    /// The reset the newest of them showed, unix milliseconds ([`super::policy::turn_end::TurnEndState::replies_reset`]).
    pub said_reset_ms: Option<i64>,
    /// The done checks this Cruise run typed ([`super::policy::turn_end::TurnEndState::checks`]), so a loop after this one asks the next angle.
    pub checks: u8,
    /// The land check's books ([`super::policy::turn_end::TurnEndState::land_books`]), so a loop
    /// after this one checks no defense twice.
    pub land: LandBooks,
    /// The wall's pass ([`super::policy::turn_end::TurnEndState::wall_pass`], v6 §14.17.5.7), so
    /// a loop after this one counts a wall's next cut act as this one would.
    pub pass: WallPass,
    /// The NEEDS YOU set whose ack an act typed ([`super::policy::turn_end::TurnEndState::needs_acked`]; `0` for none), so a loop after this one that adopts the set types it no second time.
    pub needs_acked: u64,
}

impl Carried {
    #[must_use]
    pub const fn streak(short: u32) -> Self {
        Self {
            short,
            retry: None,
            said: Replies::NONE,
            said_reset_ms: None,
            checks: 0,
            land: LandBooks {
                audited: [None; 3],
                sent: None,
                kept: [None; super::policy::turn_end::KEPT_TOKENS],
                kept_over: false,
                left_holds: 0,
            },
            pass: WallPass {
                spent: false,
                owed: false,
            },
            needs_acked: 0,
        }
    }
}

const WALL_PASS: &str = "the wall's pass";
const PASS_SPENT: &str = "spent at this streak";
const CUT_OWED: &str = "a cut act owed its delivery";

/// The ledger's note of the wall's pass ([`Carried::pass`]): none while it is unspent and no cut
/// act is owed, so a ledger that never met a wall reads as before.
fn wall_pass_note(pass: WallPass) -> String {
    let parts: Vec<&str> = [(pass.spent, PASS_SPENT), (pass.owed, CUT_OWED)]
        .into_iter()
        .filter_map(|(on, word)| on.then_some(word))
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(" ({WALL_PASS}: {})", parts.join("; "))
}

/// [`wall_pass_note`] read back: unspent and owing nothing where a reason holds no such note.
fn wall_pass_of(reason: &str) -> WallPass {
    let parts = reason
        .split_once(&format!("({WALL_PASS}: "))
        .and_then(|(_, rest)| rest.split(')').next())
        .unwrap_or_default();
    let has = |word: &str| parts.split("; ").any(|p| p == word);
    WallPass {
        spent: has(PASS_SPENT),
        owed: has(CUT_OWED),
    }
}

const DONE_CHECKS: &str = "done checks this Cruise run";

/// The ledger's note of the done checks this Cruise run typed ([`Carried::checks`]), beside the replies' note: none while there are none, so a ledger that never typed one reads as before.
fn checks_note(checks: u8) -> String {
    if checks == 0 {
        return String::new();
    }
    format!(" ({DONE_CHECKS}: {checks})")
}

/// [`checks_note`] read back: `None` where a reason holds no such note (an older build's, or none typed) or a count that is no number.
fn checks_note_of(reason: &str) -> Option<u8> {
    let (_, rest) = reason.split_once(&format!("({DONE_CHECKS}: "))?;
    crate::parse_uint(rest.split(')').next()?)
}

const NEEDS_ACKED: &str = "the needs set acked";

/// The ledger's note of the needs set whose ack was typed ([`Carried::needs_acked`]): none while none is, so a ledger that never typed one reads as before.
fn needs_acked_note(key: u64) -> String {
    if key == 0 {
        return String::new();
    }
    format!(" ({NEEDS_ACKED}: {key:016x})")
}

/// [`needs_acked_note`] read back: `None` where a reason holds no such note or a key that is no hex number.
fn needs_acked_note_of(reason: &str) -> Option<u64> {
    let (_, rest) = reason.split_once(&format!("({NEEDS_ACKED}: "))?;
    u64::from_str_radix(rest.split(')').next()?, 16).ok()
}

const LAND_BOOKS: &str = "the land check's books";

const AUDITED: &str = "audited ";

const SENT: &str = "sent ";

const KEPT: &str = "kept ";

const KEPT_OVER: &str = "kept past its books";

const LEFT_HOLDS: &str = "left holds ";

/// The ledger's note of the land check's books ([`Carried::land`]): the defenses it audited, then
/// the one its last check audits once answered, each a reply mark, then the tokens the episode's
/// defenses kept running, whether they kept more than the books hold, and the yields its left leg
/// held; none while all are empty, so a ledger with no land check reads as before.
fn land_books_note(books: LandBooks) -> String {
    let audited: Vec<String> = books
        .audited
        .iter()
        .flatten()
        .map(|m| format!("{m:016x}"))
        .collect();
    let mut parts = Vec::new();
    if !audited.is_empty() {
        parts.push(format!("{AUDITED}{}", audited.join(" ")));
    }
    if let Some(sent) = books.sent {
        parts.push(format!("{SENT}{sent:016x}"));
    }
    let kept: Vec<String> = books.kept.iter().flatten().map(|t| t.token()).collect();
    if !kept.is_empty() {
        parts.push(format!("{KEPT}{}", kept.join(" ")));
    }
    if books.kept_over {
        parts.push(KEPT_OVER.to_owned());
    }
    if books.left_holds > 0 {
        parts.push(format!("{LEFT_HOLDS}{}", books.left_holds));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(" ({LAND_BOOKS}: {})", parts.join("; "))
}

/// [`land_books_note`] read back: `None` where a reason holds no such note (an older build's, or
/// none kept) or a mark that is no hex.
fn land_books_of(reason: &str) -> Option<LandBooks> {
    let (_, rest) = reason.split_once(&format!("({LAND_BOOKS}: "))?;
    let mark = |m: &str| {
        (!m.is_empty() && m.len() <= 16 && m.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u64::from_str_radix(m, 16).ok())
            .flatten()
    };
    let mut books = LandBooks::default();
    for part in rest.split(')').next()?.split("; ") {
        if let Some(marks) = part.strip_prefix(AUDITED) {
            let marks: Vec<u64> = marks.split(' ').map(mark).collect::<Option<_>>()?;
            if marks.len() > books.audited.len() {
                return None;
            }
            let from = books.audited.len() - marks.len();
            for (slot, m) in books.audited[from..].iter_mut().zip(marks) {
                *slot = Some(m);
            }
        } else if part == KEPT_OVER {
            books.kept_over = true;
        } else if let Some(tokens) = part.strip_prefix(KEPT) {
            let tokens: Vec<aterm_uds::process::Identity> = tokens
                .split(' ')
                .map(crate::harness::leftover::token_of)
                .collect::<Option<_>>()?;
            if tokens.len() > books.kept.len() {
                return None;
            }
            let from = books.kept.len() - tokens.len();
            for (slot, t) in books.kept[from..].iter_mut().zip(tokens) {
                *slot = Some(t);
            }
        } else if let Some(n) = part.strip_prefix(LEFT_HOLDS) {
            books.left_holds = crate::parse_uint(n)?;
        } else {
            books.sent = Some(mark(part.strip_prefix(SENT)?)?);
        }
    }
    Some(books)
}

const SAME_REPLIES: &str = "the short streak's replies";

/// The ledger's note of the short streak's replies: each mark, its answers, and `w` with the wall's tries where a wall said it; then the no-work answers in a row and the reset the newest showed.
fn replies_note(said: Replies, reset_ms: Option<i64>) -> String {
    let kept: Vec<String> = said
        .kept()
        .map(|k| {
            let walled = if k.walled {
                format!("w{}", k.tries)
            } else {
                String::new()
            };
            format!("{:016x}:{}{walled}", k.mark, k.answers)
        })
        .collect();
    if kept.is_empty() {
        return String::new();
    }
    let reset = reset_ms.map_or_else(String::new, |ms| format!("; {RESET_AT}{ms}"));
    format!(
        " ({SAME_REPLIES}: {}; {}{IN_A_ROW}{reset})",
        kept.join(" "),
        said.run
    )
}

const IN_A_ROW: &str = " in a row";

const RESET_AT: &str = "reset at ";

fn replies_note_of(reason: &str) -> Option<(Replies, Option<i64>)> {
    let (_, rest) = reason.split_once(&format!("({SAME_REPLIES}: "))?;
    let mut parts = rest.split(')').next()?.split("; ");
    let note = parts.next()?;
    let (mut run, mut reset) = (0, None);
    for part in parts {
        match (part.strip_suffix(IN_A_ROW), part.strip_prefix(RESET_AT)) {
            (Some(n), _) => run = crate::parse_uint(n)?,
            (_, Some(ms)) => reset = Some(crate::parse_int(ms)?),
            _ => return None,
        }
    }
    let kept = note
        .split(' ')
        .map(|k| {
            let (mark, answers) = k.split_once(':')?;
            let (answers, tries) = match answers.split_once('w') {
                Some((n, tries)) => (n, Some(crate::parse_uint(tries)?)),
                None => (answers, None),
            };
            if mark.is_empty() || !mark.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some(Kept {
                mark: u64::from_str_radix(mark, 16).ok()?,
                answers: crate::parse_uint(answers)?,
                walled: tries.is_some(),
                tries: tries.unwrap_or(0),
            })
        })
        .collect::<Option<Vec<Kept>>>()?;
    Some((
        Replies {
            run,
            ..Replies::of(&kept)
        },
        reset,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryMark {
    pub kind: aterm_phase::WallKind,
    pub attempts: usize,
    pub ups: usize,
    pub since_ms: i64,
    /// The wall was read while the host measured the route down (` down=yes`; a row without the
    /// field, as older books write, reads false).
    pub down: bool,
    /// The outage's resume on a witness met the wall again (` witnessed=yes`,
    /// `WallTrack::witnessed`; a row without the field, as older books write, reads false): a loop
    /// that follows leaves the ladder to try it, as this one did, until the agent works again (the
    /// post-landing review, 2026-11-02).
    pub witnessed: bool,
}

const RETRY_LADDER: &str = "the wall's retries";

const API_CAUSES: [(aterm_phase::ApiCause, &str); 6] = [
    (aterm_phase::ApiCause::Server, "server"),
    (aterm_phase::ApiCause::Unreachable, "unreachable"),
    (aterm_phase::ApiCause::CutOff, "cut-off"),
    (aterm_phase::ApiCause::Config, "config"),
    (aterm_phase::ApiCause::Request, "request"),
    (aterm_phase::ApiCause::Overlong, "overlong"),
];

fn retry_mark(retry: Option<RetryMark>) -> String {
    use aterm_phase::WallKind;
    let Some(r) = retry else {
        return String::new();
    };
    let kind = match r.kind {
        WallKind::Overloaded => "kind=overloaded".to_string(),
        WallKind::ApiError {
            code,
            retryable,
            cause,
        } => {
            let Some((_, cause)) = API_CAUSES.iter().find(|(c, _)| *c == cause) else {
                return String::new();
            };
            format!(
                "kind=api-error cause={cause} code={} retryable={}",
                code.map_or_else(|| "-".to_string(), |c| c.to_string()),
                if retryable { "yes" } else { "no" }
            )
        }
        _ => return String::new(),
    };
    format!(
        " ({RETRY_LADDER}: {kind} attempts={} ups={} since={}{}{})",
        r.attempts,
        r.ups,
        r.since_ms,
        if r.down { " down=yes" } else { "" },
        if r.witnessed { " witnessed=yes" } else { "" }
    )
}

fn retry_mark_of(reason: &str) -> Option<RetryMark> {
    use aterm_phase::WallKind;
    let (_, rest) = reason.split_once(&format!("({RETRY_LADDER}: "))?;
    let fields = rest.split(')').next()?;
    let field = |name: &str| {
        fields
            .split_whitespace()
            .find_map(|f| f.strip_prefix(name)?.strip_prefix('='))
    };
    let kind = match field("kind")? {
        "overloaded" => WallKind::Overloaded,
        "api-error" => {
            let cause = field("cause")?;
            let (cause, _) = API_CAUSES.iter().find(|(_, w)| *w == cause)?;
            let code = match field("code")? {
                "-" => None,
                n => Some(crate::parse_uint(n)?),
            };
            let retryable = match field("retryable")? {
                "yes" => true,
                "no" => false,
                _ => return None,
            };
            WallKind::ApiError {
                code,
                retryable,
                cause: *cause,
            }
        }
        _ => return None,
    };
    Some(RetryMark {
        kind,
        attempts: crate::parse_uint(field("attempts")?)?,
        ups: crate::parse_uint(field("ups")?)?,
        since_ms: crate::parse_int(field("since")?)?,
        down: match field("down") {
            None => false,
            Some("yes") => true,
            Some(_) => return None,
        },
        witnessed: match field("witnessed") {
            None => false,
            Some("yes") => true,
            Some(_) => return None,
        },
    })
}

fn short_mark(short: u32) -> String {
    if short > 0 {
        format!(" ({SHORT_STREAK}: {short})")
    } else {
        String::new()
    }
}

#[must_use]
pub fn act_reason(limit: Option<LimitTry>) -> String {
    act_reason_after(Carried::default(), limit)
}

const SHORT_STREAK: &str = "short turns in a row";

#[must_use]
pub fn act_reason_after(after: Carried, limit: Option<LimitTry>) -> String {
    let mut reason = "the turn-end policy".to_string();
    reason.push_str(&short_mark(after.short));
    reason.push_str(&retry_mark(after.retry));
    reason.push_str(&replies_note(after.said, after.said_reset_ms));
    reason.push_str(&checks_note(after.checks));
    reason.push_str(&land_books_note(after.land));
    reason.push_str(&wall_pass_note(after.pass));
    reason.push_str(&needs_acked_note(after.needs_acked));
    if let Some(l) = limit {
        let head = if l.in_place {
            IN_LIMITS_PLACE
        } else {
            LIMITS_TRY
        };
        reason.push_str(&format!(
            " ({head}: wall={} attempts={}",
            limit_word(l.bucket),
            l.attempts
        ));
        if l.entitlement {
            reason.push(' ');
            reason.push_str(ENTITLEMENT_LADDER);
        }
        reason.push(')');
    }
    reason
}

#[must_use]
pub fn model_switch_reason(
    from: Option<&str>,
    launch: Option<&str>,
    back_at_unix: Option<i64>,
    made_unix: i64,
) -> String {
    format!(
        "the turn-end policy (model switch: from={} launch={} back_at={} made={made_unix})",
        from.unwrap_or("-"),
        launch.unwrap_or("-"),
        back_at_unix.map_or_else(|| "-".to_string(), |s| s.to_string())
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSwitch {
    pub from: Option<String>,
    pub to: String,
    /// The `--model` the launch named before the switch (`None`: none), which its return puts back.
    pub launch: Option<String>,
    pub back_at_unix: Option<i64>,
    pub asked: bool,
    pub made_unix: i64,
    /// The agent process the switch is in, by its newest [`switch_process_reason`] row.
    pub proc: Option<super::policy::turn_end::SwitchProc>,
}

pub const FALLBACK_ASKED: &str =
    "a turn was asked on the fallback: the bucket it left is a wall again";

const SWITCH_RUNS: &str = "the model switch is in the agent process";

/// The reason of the row that puts the open model switch in `p`
/// ([`super::policy::turn_end::SwitchScope::Moved`]).
#[must_use]
pub fn switch_process_reason(p: &super::policy::turn_end::SwitchProc) -> String {
    format!(
        "{SWITCH_RUNS} (proc={}@{} conversation={})",
        p.pid,
        p.start,
        p.conversation.as_deref().unwrap_or("-")
    )
}

/// The reason of the row that closes the open model switch
/// ([`super::policy::turn_end::SwitchScope::Closed`]).
pub const SWITCH_CLOSED: &str =
    "the model switch is over: another program, on another conversation, runs in the tab";

#[must_use]
pub fn open_model_switch_in(body: &str, sid: Option<&str>) -> Option<OpenSwitch> {
    let want = sid.map(|s| s.trim_start_matches('@'));
    let mut walk = SwitchWalk::default();
    for line in act_lines(body).filter(|l| SwitchWalk::reads(l)) {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if v.get("sid").and_then(aterm_json::Value::as_str) == want {
            walk.step(&v);
        }
    }
    walk.open
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SwitchWalk {
    open: Option<OpenSwitch>,
}

impl SwitchWalk {
    fn reads(line: &str) -> bool {
        line.contains(",\"decision\":\"typed\",")
            || line.contains(FALLBACK_ASKED)
            || line.contains(SWITCH_RUNS)
            || line.contains(SWITCH_CLOSED)
    }

    fn step(&mut self, v: &aterm_json::Value) {
        use super::policy::turn_end::{RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE, SwitchProc};
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("decision") != Some("typed") {
            if text("decision") != Some("waiting") {
                return;
            }
            let reason = text("reason").unwrap_or("");
            match text("rule_id") {
                Some(RULE_MODEL_RESTORE) if reason == SWITCH_CLOSED => self.open = None,
                Some(RULE_MODEL_FALLBACK) => {
                    let Some(open) = &mut self.open else {
                        return;
                    };
                    if reason == FALLBACK_ASKED {
                        open.asked = true;
                    } else if reason.starts_with(SWITCH_RUNS)
                        && let Some((pid, start)) = switch_field(reason, "proc")
                            .as_deref()
                            .and_then(|p| p.split_once('@'))
                            .and_then(|(pid, start)| {
                                Some((crate::parse_uint(pid)?, crate::parse_int(start)?))
                            })
                    {
                        open.proc = Some(SwitchProc {
                            pid,
                            start,
                            conversation: switch_field(reason, "conversation"),
                        });
                    }
                }
                _ => {}
            }
            return;
        }
        match text("rule_id") {
            Some(RULE_MODEL_RESTORE) => self.open = None,
            Some(RULE_MODEL_FALLBACK) => {
                let Some(to) = text("command").and_then(|c| {
                    c.strip_prefix("relaunch --model ")
                        .or_else(|| c.strip_prefix("/model "))
                }) else {
                    // The switch's own turn (a `/model` switch's `keep
                    // going`): asked on the fallback.
                    if let Some(open) = &mut self.open {
                        open.asked = true;
                    }
                    return;
                };
                let reason = text("reason").unwrap_or("");
                let field = |name: &str| switch_field(reason, name);
                // A switch row with no `launch=` is an older build's (before 2026-11-09's
                // removal), whose move this build never makes: it opens nothing.
                if !reason.contains("model switch:") || !reason.contains(" launch=") {
                    return;
                }
                let written = v.get("ts").and_then(aterm_json::Value::as_i64).unwrap_or(0);
                self.open = Some(OpenSwitch {
                    from: field("from"),
                    to: to.trim().to_string(),
                    launch: field("launch"),
                    back_at_unix: field("back_at").and_then(|w| crate::parse_int(&w)),
                    asked: false,
                    made_unix: field("made")
                        .and_then(|w| crate::parse_int(&w))
                        .unwrap_or_else(|| written.div_euclid(1000)),
                    proc: None,
                });
            }
            _ => {
                if let Some(open) = &mut self.open {
                    open.asked = true;
                }
            }
        }
    }
}

/// The word after `name=` in a switch row's reason, up to a space or `)`; `-` is none.
fn switch_field(reason: &str, name: &str) -> Option<String> {
    let at = reason.find(&format!("{name}="))? + name.len() + 1;
    let word: String = reason[at..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != ')')
        .collect();
    (word != "-" && !word.is_empty()).then_some(word)
}

#[must_use]
pub fn open_model_switch(path: &Path, sid: Option<&str>) -> Option<OpenSwitch> {
    open_model_switch_in(
        &crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024).ok()??,
        sid,
    )
}

pub const ACCOUNT_MOVE_COMMAND: &str = "relaunch under account ";

const fn account_at_word(at: super::policy::turn_end::AccountAt) -> &'static str {
    match at {
        super::policy::turn_end::AccountAt::Wall => "wall",
        super::policy::turn_end::AccountAt::Ahead => "ahead",
    }
}

#[must_use]
pub fn account_switch_reason(to: &str, at: super::policy::turn_end::AccountAt) -> String {
    format!(
        "the turn-end policy (account move: to={to} at={})",
        account_at_word(at)
    )
}

#[must_use]
pub fn account_unmade_reason(
    to: &str,
    at: super::policy::turn_end::AccountAt,
    step: Option<&str>,
) -> String {
    format!(
        "the turn-end policy (account move unmade: to={to} at={} step={})",
        account_at_word(at),
        step.map_or_else(|| "-".to_string(), |s| s.replace(char::is_whitespace, "_"))
    )
}

fn unmade_step(reason: &str) -> Option<&str> {
    let at = reason.find(" step=")? + " step=".len();
    let step = reason[at..].split([')', ' ']).next()?;
    (!step.is_empty() && step != "-").then_some(step)
}

#[must_use]
pub fn account_answer_reason(to: &str, answer: super::policy::turn_end::AccountAnswer) -> String {
    use super::policy::turn_end::{AccountAnswer, AccountBad};
    let (by, until) = match answer {
        AccountAnswer::Served => ("served", ""),
        AccountAnswer::Bad(AccountBad::Wall) => ("wall", " until=served"),
        AccountAnswer::Bad(AccountBad::SignedOut) => ("signed-out", " until=served"),
    };
    format!("the turn-end policy (account move answered: to={to} by={by}{until})")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAccountMove {
    pub to: String,
    pub at: super::policy::turn_end::AccountAt,
    pub at_ms: i64,
    pub asked: bool,
    pub told: bool,
    pub told_closes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountMoves {
    pub made_ms: Vec<i64>,
    pub open: Option<OpenAccountMove>,
    pub unmade: Option<super::policy::turn_end::Unmade>,
    pub unmade_ms: Option<i64>,
}

#[must_use]
pub fn account_moves_in(body: &str, sid: Option<&str>) -> AccountMoves {
    use super::policy::turn_end::{AccountAt, RULE_ACCOUNT, Unmade};
    let mut out = AccountMoves::default();
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    for line in act_lines(body) {
        let typed = line.contains(",\"decision\":\"typed\",");
        let skipped = line.contains(",\"decision\":\"skipped\",");
        let waits = line.contains(",\"decision\":\"waiting\",");
        if !typed && !skipped && !waits {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        let rule = text("rule_id");
        let label = text("command").and_then(|c| c.strip_prefix(ACCOUNT_MOVE_COMMAND));
        let reason = text("reason").unwrap_or("");
        if waits {
            if rule == Some(RULE_WALL_WAIT_END) {
                out.unmade = None;
                out.unmade_ms = None;
            }
            if rule == Some(RULE_ACCOUNT) && reason.contains("account move answered:") {
                if reason.contains(" by=served") {
                    out.open = None;
                } else if let Some(open) = &mut out.open {
                    open.told = true;
                    open.told_closes |= reason.contains(" until=served");
                }
            }
            continue;
        }
        if skipped {
            // Only a move asked at a wall latches (one asked ahead left no wall standing).
            if rule == Some(RULE_ACCOUNT)
                && reason.contains("account move unmade:")
                && reason.contains(" at=wall")
            {
                out.unmade = Some(Unmade::of(unmade_step(reason)));
                out.unmade_ms = v.get("ts").and_then(aterm_json::Value::as_i64);
            }
            continue;
        }
        out.unmade = None;
        out.unmade_ms = None;
        if rule != Some(RULE_ACCOUNT) || !reason.contains("account move:") {
            if let Some(open) = &mut out.open {
                open.asked = true;
            }
            continue;
        }
        let Some(to) = label.map(str::trim).filter(|l| !l.is_empty()) else {
            continue;
        };
        let at_ms = v.get("ts").and_then(aterm_json::Value::as_i64).unwrap_or(0);
        out.made_ms.push(at_ms);
        out.open = Some(OpenAccountMove {
            to: to.to_string(),
            at: if reason.contains(" at=ahead") {
                AccountAt::Ahead
            } else {
                AccountAt::Wall
            },
            at_ms,
            asked: false,
            told: false,
            told_closes: false,
        });
    }
    out
}

#[must_use]
pub fn account_moves(path: &Path, sid: Option<&str>) -> AccountMoves {
    crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024)
        .ok()
        .flatten()
        .map_or_else(AccountMoves::default, |body| account_moves_in(&body, sid))
}

#[must_use]
pub fn open_account_move(path: &Path, sid: Option<&str>) -> Option<OpenAccountMove> {
    account_moves(path, sid).open
}

pub const RULE_WALL_WAIT: &str = "wall-wait@v1";

pub const RULE_WALL_WAIT_END: &str = "wall-wait-end@v1";

/// Records the usage tap's reset and the notice's disagreeing by more than [`super::limit::CLOCK_SLACK_S`] at a wall: the later is waited to. A record only.
pub const RULE_TAP_RESET: &str = "tap-reset@v1";

/// A [`RULE_TAP_RESET`] row's reason: the notice's reset, the tap's and the one waited to (unix seconds), and how far apart the two are.
#[must_use]
pub fn tap_reset_reason(notice: i64, tap: i64, waited: i64) -> String {
    format!(
        "the usage tap and the notice disagree on the reset (notice_unix={notice}; \
         tap_unix={tap}; apart_s={}): waiting to the later, unix={waited}",
        tap.abs_diff(notice)
    )
}

/// The `(notice, tap)` resets a [`tap_reset_reason`] names.
fn tap_reset_of(reason: &str) -> Option<(i64, i64)> {
    let rest = reason.split_once("(notice_unix=")?.1;
    let (notice, rest) = rest.split_once("; tap_unix=")?;
    let (tap, _) = rest.split_once(';')?;
    Some((crate::parse_int(notice)?, crate::parse_int(tap)?))
}

/// The words a wall's reset is placed and remembered under: its kind's name and the reset the
/// notice printed (`-` for none), as the usage tap keys a wall and a [`RULE_WALL_WAIT`] row
/// names it.
#[must_use]
pub fn wall_reset_key(kind: aterm_phase::WallKind, reset: Option<&str>) -> String {
    format!("{}|{}", kind.name(), reset.unwrap_or("-"))
}

/// THE DISAGREEMENT A SESSION'S ROWS WROTE DOWN LAST ([`fold_tap_said`]): the `(notice, tap)`
/// resets of its last [`RULE_TAP_RESET`] row since its last typed act, and the wall that row was
/// written at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TapSaid {
    /// The notice's reset and the tap's (unix seconds).
    pub pair: (i64, i64),
    /// The [`wall_reset_key`] of the first [`RULE_WALL_WAIT`] row after the pair's: a loop
    /// writes a new wall's row in the point that writes that wall's pair, after the pair.
    /// `None` while no wall row has come since, the pair being the open wall's.
    pub key: Option<String>,
}

/// Fold one of a session's waiting or typed rows (`typed`, its `rule_id` and `reason`) into the
/// disagreement its rows wrote down last ([`TapSaid`]): a [`RULE_TAP_RESET`] row's pair replaces
/// it, the first [`RULE_WALL_WAIT`] row after the pair names its wall, and a typed act ends it
/// (a fact row, [`is_fact_rule`], is none). A [`RULE_WALL_WAIT_END`] row ends none: a point
/// that meets a wall of another class, or ends a limit's track under the same notice, writes
/// the end after the pair it wrote for the wall that stands. The loop folds each row it writes
/// and a loop that takes the session over folds the ledger's ([`tap_said_in`]), so the two
/// hold one pair, and the row of a pair at a wall is written once until an act is typed.
pub fn fold_tap_said(said: &mut Option<TapSaid>, typed: bool, rule_id: &str, reason: &str) {
    if is_fact_rule(rule_id) {
        return;
    }
    if typed {
        *said = None;
        return;
    }
    match rule_id {
        RULE_TAP_RESET => {
            if let Some(pair) = tap_reset_of(reason) {
                *said = Some(TapSaid { pair, key: None });
            }
        }
        RULE_WALL_WAIT => {
            if let Some(s) = said.as_mut().filter(|s| s.key.is_none())
                && let Some(row) = wall_wait_of(reason)
            {
                s.key = row.kind.map(|k| wall_reset_key(k, row.reset.as_deref()));
            }
        }
        _ => {}
    }
}

pub const RULE_OFF_STOOD: &str = "autopilot-off@v1";

/// Whether `body` holds a [`RULE_OFF_STOOD`] row for `sid`: the session ran under Off.
#[must_use]
pub fn off_stood_in(body: &str, sid: Option<&str>) -> bool {
    let want = sid.map(|s| s.trim_start_matches('@'));
    act_lines(body)
        .filter(|l| l.contains(RULE_OFF_STOOD))
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .any(|v| {
            let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
            text("rule_id") == Some(RULE_OFF_STOOD) && text("sid") == want
        })
}

/// WHAT THE PARK OF THE OFF A LIFT ENDS KEPT, as `body`'s rows for `sid` say it: the newest
/// [`RULE_OFF_STOOD`] row's ([`OffStood::of_reason`]: [`OFF_STOOD_HELD`], [`OFF_STOOD_RESTING`]),
/// where no point or typed act came after it; else the person's ([`OffStood::Person`]), for the
/// session ran under a word since that row, so the Off this lift ends wrote none (a revoked
/// worker's, a switch of program), and an older verdict is not its (the post-landing review,
/// 2026-11-01). A loop the host starts as that Off lifts keeps what the park kept.
#[must_use]
pub fn off_kept_in(body: &str, sid: Option<&str>) -> OffStood {
    let want = sid.map(|s| s.trim_start_matches('@'));
    act_lines(body)
        .rev()
        .filter(|l| {
            l.contains(RULE_OFF_STOOD)
                || l.contains(RULE_POINT)
                || l.contains(",\"decision\":\"typed\",")
        })
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .find_map(|v| {
            let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
            if text("sid") != want {
                return None;
            }
            match text("rule_id") {
                Some(RULE_OFF_STOOD) => Some(OffStood::of_reason(text("reason"))),
                Some(RULE_POINT) => Some(OffStood::Person),
                _ if text("decision") == Some("typed") => Some(OffStood::Person),
                _ => None,
            }
        })
        .unwrap_or(OffStood::Person)
}

pub const RULE_EARLIER_BOOKS: &str = "earlier-books@v1";

pub const EARLIER_BOOKS: &str = "a cold restore reopened this tab: the books of the tabs it was \
     reopened from are this session's";

pub const RULE_TASK_DONE: &str = "task-done@v1";

pub const TASK_DONE: &str = "the task is done: nothing is typed until someone else's turn";

pub const RULE_LAND_OVER: &str = "land-over@v1";

pub const LAND_OVER: &str =
    "the land episode is over (a reading in another mode): a Land after it is a new one";

pub const OFF_STOOD: &str = "autopilot off stood: the person drove the session; the land episode, \
     the carry, the replies the streak kept and an act a wall cut are over";

/// [`OFF_STOOD`] where no person set Off (`autopilot_by=` not `human:…`): the replies that hold
/// stand past it, for only the person's own switch lifts a hold
/// ([`super::policy::turn_end::TurnEndState::park_off_held`]).
pub const OFF_STOOD_HELD: &str = "autopilot off stood, set by no person: the land episode, the \
     carry and an act a wall cut are over; the replies that hold stand";

/// [`OFF_STOOD_HELD`] over Land's rest on a need: the land episode stands too, so a loop that
/// follows seeds it and reads its point again as the check's answer
/// ([`super::policy::turn_end::OffStood::Resting`]).
pub const OFF_STOOD_RESTING: &str = "autopilot off stood, set by no person, over Land's rest on a \
     need: the carry and an act a wall cut are over; the replies that hold, the land episode and \
     its need stand";

pub const LIMIT_ENDED: &str = "the limit's track ended (work, or another wall): its tries are over";

/// A limit ended where another program's screen was read in the tab
/// ([`super::policy::turn_end::TurnEndState::limit_ended_elsewhere`]); it ends
/// the episode and its tries as [`LIMIT_ENDED`] does.
pub const LIMIT_ENDED_ELSEWHERE: &str =
    "the limit's track ended (another program runs in the tab): its tries are over";

pub const RULE_TRY_OVER: &str = "try-over@v1";

pub const TRY_OVER: &str =
    "the limit's try a loop before this one made aged out: its tries are over; the wall stands";

pub const RULE_TRY_ANSWERED: &str = "try-answered@v1";

#[must_use]
pub fn try_answered_reason(read_ms: i64) -> String {
    format!("{TRY_ANSWERED}{read_ms}")
}

const TRY_ANSWERED: &str =
    "the limit's try a loop before this one made was answered at once: its first read_ms=";

fn try_answered_of(reason: &str) -> Option<i64> {
    crate::parse_int(reason.strip_prefix(TRY_ANSWERED)?)
}

pub const WALL_TAKEN: &str =
    "the act was taken: the wall read since it was typed was the screen it was typed at";

pub const WALL_LEFT_NO_TASK: &str =
    "the conversation holds no task (a /clear, or a new one): nothing is owed at the reset";

#[must_use]
pub fn wall_wait_reason(
    reset_unix: Option<i64>,
    reset: Option<&str>,
    kind: aterm_phase::WallKind,
    message: &str,
) -> String {
    wall_wait_reason_tapped(reset_unix, None, reset, kind, message)
}

/// [`wall_wait_reason`] with THE RESET THE USAGE TAP TIMED THE WALL TO
/// (`tap_unix`, unix seconds; accounts v2 S10a, `super::tap_clock`): the
/// reset it was waited to, kept apart from the notice's own placement
/// (`reset_unix`), so a loop that takes the session over waits to it as
/// this loop did, and still sets its own tap's reading against the notice's
/// reset ([`OpenWall::tap_unix`]). Written after the kind (`; tap_unix=`)
/// only where the tap timed it: every other row is the one before.
#[must_use]
pub fn wall_wait_reason_tapped(
    reset_unix: Option<i64>,
    tap_unix: Option<i64>,
    reset: Option<&str>,
    kind: aterm_phase::WallKind,
    message: &str,
) -> String {
    wall_wait_reason_of(reset_unix, tap_unix, None, reset, kind, message)
}

/// [`wall_wait_reason_tapped`] with THE ACCOUNT THE LIMIT WAS READ UNDER
/// (`login`, the session's profile's [`AccountKey`]): written after the tap's
/// reset (`; login=`) only where it was read, so a loop that takes the
/// session over sets its own read of the login against it
/// ([`OpenWall::login`], [`RULE_LOGIN_MOVED`]).
#[must_use]
pub fn wall_wait_reason_of(
    reset_unix: Option<i64>,
    tap_unix: Option<i64>,
    login: Option<AccountKey>,
    reset: Option<&str>,
    kind: aterm_phase::WallKind,
    message: &str,
) -> String {
    format!(
        "the turn-end policy (wall: reset_unix={}; reset={}; kind={}{}{}; message={})",
        reset_unix.map_or_else(|| "-".to_string(), |s| s.to_string()),
        reset.unwrap_or("-"),
        kind.name(),
        tap_unix.map_or_else(String::new, |t| format!("; tap_unix={t}")),
        login.map_or_else(String::new, |k| format!("; login={k}")),
        message.replace(['\n', '\r'], " ")
    )
}

/// Records the session's login moving off the account its standing limit
/// was read under ([`super::policy::turn_end::TurnEndReading::login_moved`]):
/// the limit's reset binds no longer, and one resume is owed. A record only.
pub const RULE_LOGIN_MOVED: &str = "login-moved@v1";

/// A [`RULE_LOGIN_MOVED`] row's reason: the account the limit was read
/// under and the one the profile names now, as their keys (no secret).
#[must_use]
pub fn login_moved_reason(from: AccountKey, to: AccountKey) -> String {
    format!(
        "the session's login moved off the account its limit was read under (from={from}; \
         to={to}): the limit's reset binds no longer; one resume is owed"
    )
}

fn kind_named(name: &str) -> Option<aterm_phase::WallKind> {
    use aterm_phase::WallKind;
    [
        WallKind::UsageSession,
        WallKind::UsageWeekly,
        WallKind::Spend,
        WallKind::ModelBucket { consent: false },
    ]
    .into_iter()
    .find(|k| k.name() == name)
}

#[must_use]
pub fn wall_kind(
    named: Option<aterm_phase::WallKind>,
    message: &str,
) -> Option<aterm_phase::WallKind> {
    let words = super::run::words_kind(message);
    match named {
        Some(kind) => Some(
            words
                .filter(|w| std::mem::discriminant(w) == std::mem::discriminant(&kind))
                .unwrap_or(kind),
        ),
        None => words,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWall {
    pub conversation: Conversation,
    pub program: Option<aterm_phase::Program>,
    pub at_ms: i64,
    pub message: String,
    pub reset: Option<String>,
    pub reset_unix: Option<i64>,
    /// The reset the usage tap timed the wall to ([`wall_wait_reason_tapped`], unix seconds); `None` where no tap timed it, and in an older build's row.
    pub tap_unix: Option<i64>,
    /// The account the limit was read under ([`wall_wait_reason_of`]); `None` where it was not read, and in an older build's row.
    pub login: Option<AccountKey>,
    pub kind: Option<aterm_phase::WallKind>,
    pub judged_ms: Option<i64>,
    pub retry_ms: Option<i64>,
}

fn row_conversation(v: &aterm_json::Value) -> Conversation {
    match v.get("conversation") {
        None => Conversation::Hostless,
        Some(id) => id
            .as_str()
            .filter(|id| !id.is_empty())
            .map_or(Conversation::Unknown, |id| {
                Conversation::Named(id.to_owned())
            }),
    }
}

fn row_program(v: &aterm_json::Value) -> Option<aterm_phase::Program> {
    let name = v.get(PROGRAM)?;
    Some(
        name.as_str()
            .and_then(|n| {
                aterm_phase::Program::ALL
                    .into_iter()
                    .find(|p| p.name() == n)
            })
            .unwrap_or(aterm_phase::Program::Generic),
    )
}

/// `sid`'s waiting and typed rows in `body` but its fact rows, in the order they were written,
/// each with whether it is a typed one.
fn waiting_or_typed<'a>(
    body: &'a str,
    sid: Option<&str>,
) -> impl Iterator<Item = (bool, aterm_json::Value)> + 'a {
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    act_lines(body).filter_map(move |line| {
        let waits = line.contains(",\"decision\":\"waiting\",");
        let typed = line.contains(",\"decision\":\"typed\",");
        if !waits && !typed {
            return None;
        }
        let v = aterm_json::from_str::<aterm_json::Value>(line).ok()?;
        (v.get("sid").and_then(aterm_json::Value::as_str) == want.as_deref()).then_some((typed, v))
    })
}

/// The disagreement `sid`'s rows in `body` wrote down last ([`fold_tap_said`]): what a loop
/// that takes the session over holds as its own, so it writes no row for that pair at that
/// wall again.
#[must_use]
pub fn tap_said_in(body: &str, sid: Option<&str>) -> Option<TapSaid> {
    let mut said = None;
    for (typed, v) in waiting_or_typed(body, sid) {
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str).unwrap_or("");
        fold_tap_said(&mut said, typed, text("rule_id"), text("reason"));
    }
    said
}

#[must_use]
pub fn open_wall_in(body: &str, sid: Option<&str>) -> Option<OpenWall> {
    let mut open: Option<OpenWall> = None;
    let mut retry_ms = None;
    for (typed, v) in waiting_or_typed(body, sid) {
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        let ts = v.get("ts").and_then(aterm_json::Value::as_i64).unwrap_or(0);
        if typed {
            open = None;
            let rule = text("rule_id").unwrap_or("");
            retry_ms = (super::policy::turn_end::is_wall_retry(rule)
                || text("reason").and_then(limit_try_of).is_some())
            .then_some(ts);
            continue;
        }
        match text("rule_id") {
            Some(RULE_WALL_WAIT_END) => open = None,
            Some(RULE_WALL_WAIT) => {
                let Some(row) = text("reason").and_then(wall_wait_of) else {
                    continue;
                };
                let conversation = row_conversation(&v);
                open = Some(OpenWall {
                    conversation,
                    program: row_program(&v),
                    at_ms: ts,
                    message: row.message.to_string(),
                    reset: row.reset,
                    reset_unix: row.reset_unix,
                    tap_unix: row.tap_unix,
                    login: row.login,
                    kind: row.kind,
                    judged_ms: None,
                    retry_ms,
                });
            }
            Some(RULE_POINT) => {
                if let Some(o) = open.as_mut() {
                    o.judged_ms.get_or_insert(ts);
                }
            }
            _ => {}
        }
    }
    open
}

struct WallRow<'a> {
    reset_unix: Option<i64>,
    tap_unix: Option<i64>,
    login: Option<AccountKey>,
    reset: Option<String>,
    kind: Option<aterm_phase::WallKind>,
    older: bool,
    message: &'a str,
}

fn wall_wait_of(reason: &str) -> Option<WallRow<'_>> {
    let body = reason
        .strip_prefix("the turn-end policy (wall: reset_unix=")
        .and_then(|r| r.strip_suffix(')'))?;
    let (unix, rest) = body.split_once("; reset=")?;
    let (head, message) = rest.split_once("; message=")?;
    let (reset, kind, tap_unix, login) = match head.split_once("; kind=") {
        Some((reset, named)) => {
            let (named, login) = named
                .split_once("; login=")
                .map_or((named, None), |(named, key)| {
                    (named, AccountKey::parse(key))
                });
            let (name, tap) = named
                .split_once("; tap_unix=")
                .map_or((named, None), |(name, tap)| (name, crate::parse_int(tap)));
            (reset, kind_named(name), tap, login)
        }
        None => (head, None, None, None),
    };
    Some(WallRow {
        reset_unix: crate::parse_int(unix),
        tap_unix,
        login,
        reset: (reset != "-").then(|| reset.to_string()),
        kind,
        older: !head.contains("; kind="),
        message,
    })
}

#[must_use]
pub fn open_wall(path: &Path, sid: Option<&str>) -> Option<OpenWall> {
    open_wall_in(
        &crate::harness::accounts::read_bounded(path, 16 * 1024 * 1024).ok()??,
        sid,
    )
}

fn sha256_hex(s: &str) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(s.as_bytes()))
}

fn cut(s: &str, n: usize) -> String {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
#[path = "approvals_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "approvals_switch_tests.rs"]
mod switch_tests;

#[cfg(test)]
#[path = "approvals_fact_tests.rs"]
mod fact_tests;
