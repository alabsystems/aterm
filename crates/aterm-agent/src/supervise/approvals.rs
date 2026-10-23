// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The approval ledger: one JSON line per box the approval policy decided ([`super::policy::approval::decide`]) and per act of the turn-end policy, for every `watch`, `supervise` and hosted run, at `<aterm state>/drive/<sid>.jsonl` ([`default_path`]).

use std::io::Write;
use std::path::{Path, PathBuf};

use super::journal::{Journal, json_str};
use super::policy::sleeps::{Boot, SleepStamp};
use super::policy::turn_end::{Conversation, LimitTry};

/// How much of a command a row keeps.
pub const COMMAND_BYTES: usize = 4096;

/// What every rule of the facts seam opens with (`fact-hold@v1`, `fact-would@v1`,
/// `fact-intent@v1`, `fact-held@v1`; [`super::facts`]).
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

    /// The outcome a row's `decision` word names, by [`Self::word`]'s own words, so a reader of the ledger (`harness::stats`) cannot drift from its writer; `None` for a word this build does not write.
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

    /// [`Self::to_json_scoped`], and where `program` is given, the program the wall was met under ([`PROGRAM`]), whatever the conversation: a hostless wall names it too.
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
    let _ = crate::harness::cli::bound_ledger_cut(path, MAX_BYTES, &|rows, read| {
        cut_rows(path, rows, read)
    });
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
    let Ok(older) = older_kept(path, &held, start, read, work) else {
        return Some(held);
    };
    Some(
        held.into_iter()
            .enumerate()
            .filter(|(i, _)| *i >= start || older.binary_search(i).is_ok())
            .map(|(_, row)| row)
            .collect(),
    )
}

pub(crate) const HELD_BYTES: usize = 16 << 20;

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
            if LandWalk::reads(&row) || SwitchWalk::reads(&row) || row.contains(RULE_EARLIER_BOOKS)
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
        self.write_counted_with_boot_text(t_ms, row, count, None, warn)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_scoped_counted(
        &mut self,
        t_ms: i64,
        row: &Row<'_>,
        count: Option<&SleepStamp>,
        captured_boot: Option<&str>,
        conversation: &Conversation,
        program: Option<aterm_phase::Program>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        if !self.journal.is_on() {
            return None;
        }
        let line = row.to_json_scoped_by(
            t_ms,
            self.sid.as_deref(),
            count,
            captured_boot,
            conversation,
            program,
        );
        self.journal.append_raw(&line, warn);
        Some(line)
    }

    pub(crate) fn write_counted_with_boot_text(
        &mut self,
        t_ms: i64,
        row: &Row<'_>,
        count: Option<&SleepStamp>,
        captured_boot: Option<&str>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        if !self.journal.is_on() {
            return None;
        }
        let line =
            row.to_json_counted_with_boot_text(t_ms, self.sid.as_deref(), count, captured_boot);
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

#[must_use]
pub fn typed_texts(path: &Path, sid: Option<&str>) -> Vec<Typed> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let mut out = Vec::new();
    for line in act_lines(&body).filter(|l| l.contains("\"typed\"")) {
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

pub const LEFT_AT_WALL: &str = "wall:";

#[must_use]
pub fn left_at_wall_key(turn: &str, work: std::time::Duration) -> String {
    format!("{LEFT_AT_WALL}{turn}@{}", work.as_secs())
}

#[must_use]
pub fn parse_left_at_wall(key: &str) -> Option<(&str, Option<std::time::Duration>)> {
    let rest = key.strip_prefix(LEFT_AT_WALL)?;
    let worked = rest.rsplit_once('@').and_then(|(turn, secs)| {
        let secs = secs.parse().ok()?;
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
    Some((u64::from_str_radix(hash, 16).ok()?, wall))
}

pub const LEFT_BEFORE: &str = "before:";

#[must_use]
pub fn left_before_key(at_ms: i64) -> String {
    format!("{LEFT_BEFORE}{at_ms}")
}

#[must_use]
pub fn parse_left_before(key: &str) -> Option<u64> {
    key.strip_prefix(LEFT_BEFORE)?.parse().ok()
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

#[must_use]
pub fn left_after_typed(open: &str, rule: &str, command: &str) -> Option<String> {
    use super::policy::turn_end::{
        RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY, RULE_LIMIT_RESUME, RULE_LOGIN_BACK,
        RULE_USAGE_PROBE,
    };
    match rule {
        RULE_USAGE_PROBE => Some(open.to_string()),
        // A fact line ends no person's exchange ([`is_fact_rule`]).
        rule if is_fact_rule(rule) => Some(open.to_string()),
        RULE_API_RETRY | RULE_API_BACK | RULE_API_CUTOFF | RULE_LIMIT_RESUME | RULE_LOGIN_BACK
            if open.starts_with(LEFT_AT_WALL) =>
        {
            Some(left_retried_key(open, command))
        }
        _ => None,
    }
}

#[must_use]
pub fn open_left(path: &Path, sid: Option<&str>) -> Option<String> {
    open_left_in(&std::fs::read_to_string(path).ok()?, sid)
}

#[must_use]
pub fn typed_at(path: &Path, sid: Option<&str>) -> Vec<i64> {
    std::fs::read_to_string(path).map_or_else(|_| Vec::new(), |body| typed_at_in(&body, sid))
}

pub const PROBE_TYPED_REASON: &str = "the accounts probe";

pub const PROBE_CLOSED_REASON: &str = "closed";

fn probe_rule_field() -> String {
    format!(
        ",\"rule_id\":{},",
        json_str(super::policy::turn_end::RULE_USAGE_PROBE)
    )
}

#[must_use]
pub fn probe_owed(body: &str, sid: Option<&str>) -> bool {
    let sid = sid.map_or_else(
        || "null".to_string(),
        |s| json_str(s.trim_start_matches('@')),
    );
    let want_sid = format!(",\"sid\":{sid},");
    let probe_rule = probe_rule_field();
    body.lines()
        .rev()
        .find(|l| l.contains(&want_sid) && l.contains(&probe_rule))
        .is_some_and(|l| l.contains(",\"decision\":\"typed\","))
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
    awaited_try_in(&std::fs::read_to_string(path).ok()?, sid)
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
    usage_attempt_in(&std::fs::read_to_string(path).ok()?, sid)
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

fn delivers_land(command: Option<&str>) -> bool {
    use super::policy::turn_end::{
        LAND, LAND_ANSWER, LAND_NUDGE, RETIRED_LAND_ANSWERS, RETIRED_LAND_NUDGES, RETIRED_LANDS,
    };
    command.is_some_and(|c| {
        [LAND, LAND_NUDGE, LAND_ANSWER]
            .into_iter()
            .chain(RETIRED_LANDS)
            .chain(RETIRED_LAND_NUDGES)
            .chain(RETIRED_LAND_ANSWERS)
            .any(|head| opens_with(head, c))
    })
}

fn opens_with(head: &str, text: &str) -> bool {
    let mut words = text.split_whitespace();
    head.split_whitespace().all(|w| words.next() == Some(w))
}

#[must_use]
pub fn done_check_owed(path: &Path, sid: Option<&str>) -> Option<CheckOwed> {
    done_check_owed_in(&std::fs::read_to_string(path).ok()?, sid)
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
    use super::policy::turn_end::{RULE_LAND, RULE_LAND_ANSWER, RULE_LAND_NUDGE};
    [RULE_LAND, RULE_LAND_NUDGE, RULE_LAND_ANSWER]
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
            HoldStart, RULE_COMPACT, RULE_LAND, RULE_LAND_NUDGE, RULE_USAGE_PROBE,
            is_continue_rule, is_land_rule, is_wall_retry,
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        let rule = text("rule_id").unwrap_or("");
        match text("decision") {
            Some("typed") => {
                let judged = self.judged;
                let owed = self.ep.and_then(|e| e.owed);
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
                };
                let begun = |base: u32| HoldStart {
                    since_ms: v.get("ts").and_then(aterm_json::Value::as_i64),
                    base,
                    relanded: false,
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
                    (r, Some(e)) => Some(land(
                        e.nudges,
                        (e.cut && is_wall_retry(r))
                            || (r == RULE_COMPACT && delivers_land(text("command"))),
                        e.hold,
                    )),
                    (_, None) => None,
                };
                if rule != RULE_USAGE_PROBE
                    && let Some(e) = self.ep.as_mut()
                {
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
    land_episode_in(&std::fs::read_to_string(path).ok()?, sid)
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
        .and_then(|n| n.parse::<usize>().ok())
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
}

#[must_use]
pub fn short_streak_in(body: &str, sid: Option<&str>) -> Option<StreakSeed> {
    use super::policy::turn_end::{
        RULE_ACCOUNT, RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE, RULE_USAGE_PROBE,
    };
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<StreakSeed> = None;
    let mut recorded = false;
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
        if last.as_ref().is_some_and(|l| at_ms < l.at_ms) {
            continue;
        }
        let short = text("reason")
            .and_then(|r| r.split_once(&format!("({SHORT_STREAK}: ")))
            .and_then(|(_, rest)| rest.split(')').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let esc = point && text("reason").is_some_and(|r| r.contains(&format!("({PERSONS_ESC})")));
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
        });
    }
    last.map(|l| StreakSeed { recorded, ..l })
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
    short_streak_in(&std::fs::read_to_string(path).ok()?, sid)
}

pub const RULE_TURN_RAN: &str = "turn-ran@v1";

pub const TURN_RAN: &str = "a turn was read running after the point the turn-end policy judged";

pub const RULE_POINT: &str = "point@v1";

#[must_use]
pub fn point_reason(after: Carried, esc: bool) -> String {
    format!(
        "the turn-end policy judged the point ({SHORT_STREAK}: {}){}{}",
        after.short,
        retry_mark(after.retry),
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
}

impl Carried {
    #[must_use]
    pub const fn streak(short: u32) -> Self {
        Self { short, retry: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryMark {
    pub kind: aterm_phase::WallKind,
    pub attempts: usize,
    pub ups: usize,
    pub since_ms: i64,
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
        " ({RETRY_LADDER}: {kind} attempts={} ups={} since={})",
        r.attempts, r.ups, r.since_ms
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
                n => Some(n.parse().ok()?),
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
        attempts: field("attempts")?.parse().ok()?,
        ups: field("ups")?.parse().ok()?,
        since_ms: field("since")?.parse().ok()?,
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
    back_at_unix: Option<i64>,
    made_unix: i64,
) -> String {
    format!(
        "the turn-end policy (model switch: from={} back_at={} made={made_unix})",
        from.unwrap_or("-"),
        back_at_unix.map_or_else(|| "-".to_string(), |s| s.to_string())
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSwitch {
    pub from: Option<String>,
    pub to: String,
    pub back_at_unix: Option<i64>,
    pub asked: bool,
    pub made_unix: i64,
}

pub const FALLBACK_ASKED: &str =
    "a turn was asked on the fallback: the bucket it left is a wall again";

#[must_use]
pub fn relaunch_waits_reason(made_unix: i64) -> String {
    format!("{RELAUNCH_WAITS} (made={made_unix})")
}

const RELAUNCH_WAITS: &str = "the fallback's relaunch was asked and waits for the host";

#[must_use]
pub fn relaunch_waiting_in(body: &str, sid: Option<&str>) -> Option<i64> {
    use super::policy::turn_end::RULE_MODEL_FALLBACK;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut made = None;
    for line in act_lines(body) {
        let typed = line.contains(",\"decision\":\"typed\",");
        if !typed && !line.contains(RELAUNCH_WAITS) {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        if typed {
            made = None;
        } else if text("decision") == Some("waiting")
            && text("rule_id") == Some(RULE_MODEL_FALLBACK)
        {
            made = text("reason")
                .and_then(|r| r.strip_prefix(RELAUNCH_WAITS))
                .and_then(|r| r.strip_prefix(" (made="))
                .and_then(|r| r.strip_suffix(')'))
                .and_then(|r| r.parse().ok());
        }
    }
    made
}

#[must_use]
pub fn relaunch_waiting(path: &Path, sid: Option<&str>) -> Option<i64> {
    relaunch_waiting_in(&std::fs::read_to_string(path).ok()?, sid)
}

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
        line.contains(",\"decision\":\"typed\",") || line.contains(FALLBACK_ASKED)
    }

    fn step(&mut self, v: &aterm_json::Value) {
        use super::policy::turn_end::{RULE_ACCOUNT, RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE};
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("decision") != Some("typed") {
            if text("decision") == Some("waiting")
                && text("rule_id") == Some(RULE_MODEL_FALLBACK)
                && text("reason") == Some(FALLBACK_ASKED)
                && let Some(open) = &mut self.open
            {
                open.asked = true;
            }
            return;
        }
        match text("rule_id") {
            Some(RULE_MODEL_RESTORE) => self.open = None,
            Some(RULE_ACCOUNT)
                if text("command").is_some_and(|c| c.starts_with(ACCOUNT_MOVE_COMMAND))
                    && text("reason").is_some_and(|r| r.contains("account move:")) =>
            {
                self.open = None;
            }
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
                let field = |name: &str| {
                    let at = reason.find(&format!("{name}="))? + name.len() + 1;
                    let word: String = reason[at..]
                        .chars()
                        .take_while(|c| !c.is_whitespace() && *c != ')')
                        .collect();
                    (word != "-" && !word.is_empty()).then_some(word)
                };
                if !reason.contains("model switch:") {
                    return;
                }
                let written = v.get("ts").and_then(aterm_json::Value::as_i64).unwrap_or(0);
                self.open = Some(OpenSwitch {
                    from: field("from"),
                    to: to.trim().to_string(),
                    back_at_unix: field("back_at").and_then(|w| w.parse().ok()),
                    asked: false,
                    made_unix: field("made")
                        .and_then(|w| w.parse().ok())
                        .unwrap_or_else(|| written.div_euclid(1000)),
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

#[must_use]
pub fn open_model_switch(path: &Path, sid: Option<&str>) -> Option<OpenSwitch> {
    open_model_switch_in(&std::fs::read_to_string(path).ok()?, sid)
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
    std::fs::read_to_string(path).map_or_else(
        |_| AccountMoves::default(),
        |body| account_moves_in(&body, sid),
    )
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

pub const RULE_OFF_STOOD: &str = "autopilot-off@v1";

pub const RULE_EARLIER_BOOKS: &str = "earlier-books@v1";

pub const EARLIER_BOOKS: &str = "a cold restore reopened this tab: the books of the tabs it was \
     reopened from are this session's";

pub const RULE_TASK_DONE: &str = "task-done@v1";

pub const TASK_DONE: &str = "the task is done: nothing is typed until someone else's turn";

pub const RULE_LAND_OVER: &str = "land-over@v1";

pub const LAND_OVER: &str =
    "the land episode is over (a reading in another mode): a Land after it is a new one";

pub const OFF_STOOD: &str = "autopilot off stood: the person drove the session; the land episode, \
     the carry and an act a wall cut are over";

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
    reason.strip_prefix(TRY_ANSWERED)?.parse().ok()
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
    format!(
        "the turn-end policy (wall: reset_unix={}; reset={}; kind={}{}; message={})",
        reset_unix.map_or_else(|| "-".to_string(), |s| s.to_string()),
        reset.unwrap_or("-"),
        kind.name(),
        tap_unix.map_or_else(String::new, |t| format!("; tap_unix={t}")),
        message.replace(['\n', '\r'], " ")
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

#[must_use]
pub fn open_wall_in(body: &str, sid: Option<&str>) -> Option<OpenWall> {
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut open: Option<OpenWall> = None;
    let mut retry_ms = None;
    for line in act_lines(body) {
        let waits = line.contains(",\"decision\":\"waiting\",");
        let typed = line.contains(",\"decision\":\"typed\",");
        if !waits && !typed {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
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
    let (reset, kind, tap_unix) = match head.split_once("; kind=") {
        Some((reset, named)) => {
            let (name, tap) = named
                .split_once("; tap_unix=")
                .map_or((named, None), |(name, tap)| (name, tap.parse().ok()));
            (reset, kind_named(name), tap)
        }
        None => (head, None, None),
    };
    Some(WallRow {
        reset_unix: unix.parse().ok(),
        tap_unix,
        reset: (reset != "-").then(|| reset.to_string()),
        kind,
        older: !head.contains("; kind="),
        message,
    })
}

#[must_use]
pub fn open_wall(path: &Path, sid: Option<&str>) -> Option<OpenWall> {
    open_wall_in(&std::fs::read_to_string(path).ok()?, sid)
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
mod tests {

    #[test]
    fn the_newest_rows_time_is_the_books_last() {
        let body = "{\"ts\":1792594893546,\"sid\":null}\n\
                    {\"ts\":1792594893533,\"sid\":null}\n\
                    \n\
                    not a row\n";
        assert_eq!(super::last_row_ms(body), Some(1_792_594_893_546));
        assert_eq!(super::last_row_ms(""), None);
        assert_eq!(super::last_row_ms("{\"t\":5}\n"), None, "a journal line");
    }

    #[test]
    fn an_acts_row_counts_its_sleep_on_a_boot_in_the_kernels_spelling() {
        let id = [
            0x4D, 0x5A, 0x1B, 0x2C, 0x3E, 0x4F, 0x50, 0x61, 0x72, 0x83, 0x94, 0xA5, 0xB6, 0xC7,
            0xD8, 0xE9,
        ];
        let (upper, lower) = (
            "4D5A1B2C-3E4F-5061-7283-94A5B6C7D8E9",
            "4d5a1b2c-3e4f-5061-7283-94a5b6c7d8e9",
        );
        let row = Row {
            rule_id: super::super::policy::turn_end::RULE_LIMIT_RESUME,
            outcome: Outcome::Typed,
            command: "continue",
            reason: "the limit reset",
            box_seq: 3,
        };
        let at = 1_790_000_000_000;
        let count = |boot: Option<Boot>| SleepStamp {
            asleep: std::time::Duration::from_millis(1_800_003),
            boot,
        };
        let tail = |text: &str| format!(",\"slept_ms\":1800003,\"boot\":\"{text}\"}}");
        let read = |line: &str| last_act_count_in(&format!("{line}\n"), Some("s-1"));
        for text in [upper, lower] {
            let read_back = count(Boot::of(text));
            assert_eq!(read_back.boot.map(|b| b.id()), Some(id), "{text}");
            let line = row.to_json_counted(at, Some("@s-1"), Some(&read_back));
            assert!(line.ends_with(&tail(text)), "written as read: {line}");
            assert_eq!(read(&line), Some((at, Some(read_back.clone()))), "{text}");
            assert_eq!(
                read(&line).and_then(|(_, c)| c?.since(&count(Some(Boot::kernel(id))))),
                Some(std::time::Duration::ZERO),
                "{text}"
            );
        }
        let kernel = if cfg!(any(target_os = "linux", target_os = "android")) {
            lower
        } else {
            upper
        };
        let live = count(Some(Boot::kernel(id)));
        let line = row.to_json_counted(at, Some("@s-1"), Some(&live));
        assert!(
            line.ends_with(&tail(kernel)),
            "this kernel's spelling: {line}"
        );
        for damaged in ["00000000-0000-0000-0000-000000000000", "test-boot", ""] {
            assert_eq!(
                read(&line.replace(kernel, damaged)),
                Some((at, None)),
                "CONTROL {damaged:?}"
            );
        }
        assert_eq!(
            row.to_json_counted(1, Some("@s-1"), Some(&count(None))),
            row.to_json(1, Some("@s-1")),
            "CONTROL: no boot, no count"
        );
    }

    #[test]
    fn the_journal_sits_beside_the_ledger_and_both_are_bounded() {
        let dir = std::env::temp_dir().join(format!("aterm-apr-bound-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ledger = path_under(&dir, Some("@s-7")).expect("the drive dir");
        assert_eq!(
            journal_beside(&ledger),
            dir.join("drive").join("s-7.journal.jsonl")
        );
        let row = format!("{{\"ts\":1,\"pad\":\"{}\"}}", "x".repeat(300));
        let rows = usize::try_from(MAX_BYTES).unwrap() / row.len() + 50;
        let body: String = (0..rows).map(|k| format!("{row}{k}\n")).collect();
        std::fs::write(&ledger, &body).expect("write");
        bound(&ledger);
        let cut = std::fs::read_to_string(&ledger).expect("read");
        assert_eq!(cut.lines().count(), KEEP_ROWS);
        assert!(
            cut.ends_with(&format!("{row}{}\n", rows - 1)),
            "the newest rows kept"
        );
        let small = dir.join("drive").join("small.jsonl");
        std::fs::write(&small, "{\"ts\":1}\n").expect("write");
        bound(&small);
        assert_eq!(
            std::fs::read_to_string(&small).expect("read"),
            "{\"ts\":1}\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_past_sixteen_mib_is_streamed_and_cut() {
        use super::super::policy::turn_end::RULE_MODEL_FALLBACK;
        let dir = std::env::temp_dir().join(format!("aterm-apr-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let past = 17 << 20;
        let sid = "s-195cbb43bd1fc3dc2c7b";
        let journal = journal_beside(&path_under(&dir, Some(sid)).expect("the drive dir"));
        let mut body = String::with_capacity(past + 4096);
        let mut t = 1_791_661_422_112_u64;
        while body.len() <= past {
            body.push_str(&format!(
                "{{\"t\":{t},\"sid\":\"{sid}\",\"kind\":\"other\",\"phase\":\"-\",\"seq\":null,\
                 \"complete\":null,\"rows\":null,\"summary\":\"HOST seq=1 upgrade \
                 step=wait:session-files-unreadable\",\"line\":\"HOST seq=1 upgrade \
                 step=wait:session-files-unreadable\",\"turn\":null,\"report\":null}}\n"
            ));
            t += 545;
        }
        std::fs::write(&journal, &body).expect("write");
        bound(&journal);
        let cut = std::fs::read_to_string(&journal).expect("read");
        assert_eq!(cut.lines().count(), KEEP_ROWS, "the journal is cut");
        assert!(body.ends_with(&cut), "the newest rows kept");

        let ledger = path_under(&dir, Some("@s-9")).expect("the drive dir");
        let switch = Row {
            rule_id: RULE_MODEL_FALLBACK,
            outcome: Outcome::Typed,
            command: "relaunch --model sonnet",
            reason: &model_switch_reason(Some("opus"), None, 1),
            box_seq: 1,
        }
        .to_json(1_000, Some("@s-9"));
        let mut body = format!("{switch}\n");
        let mut ts = 2_000;
        while body.len() <= past {
            body.push_str(
                &Row {
                    rule_id: "allow-once@v1",
                    outcome: Outcome::Approved,
                    command: "echo captured",
                    reason: "an ordinary approval",
                    box_seq: 1,
                }
                .to_json(ts, Some("@s-9")),
            );
            body.push('\n');
            ts += 1;
        }
        std::fs::write(&ledger, &body).expect("write");
        let standing = open_model_switch_in(&body, Some("@s-9"));
        assert!(standing.is_some(), "the switch stands");
        bound(&ledger);
        let cut = std::fs::read_to_string(&ledger).expect("read");
        assert_eq!(cut.lines().count(), KEEP_ROWS + 1, "the ledger is cut");
        assert!(cut.len() as u64 <= MAX_BYTES, "{} bytes", cut.len());
        assert_eq!(
            open_model_switch_in(&cut, Some("@s-9")),
            standing,
            "the standing switch survives the cut"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cut_refuses_oversized_retained_rows_but_still_folds_records() {
        let tmp = aterm_tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("approvals.jsonl");
        let reason = "x".repeat(HELD_BYTES / KEEP_ROWS);
        for kind in ["plain", "interleaved-records", "folded-records"] {
            let record = kind != "plain";
            let mut body = "older ordinary row\n".to_string();
            let mut last = String::new();
            for i in 0..KEEP_ROWS {
                let sid = if kind == "interleaved-records" && i % 2 == 0 {
                    "@s-10"
                } else {
                    "@s-9"
                };
                last = Row {
                    rule_id: if record { RULE_POINT } else { "allow-once@v1" },
                    outcome: if record {
                        Outcome::Waiting
                    } else {
                        Outcome::Approved
                    },
                    command: "",
                    reason: &reason,
                    box_seq: 1,
                }
                .to_json(i64::try_from(i).unwrap(), Some(sid));
                body.push_str(&last);
                body.push('\n');
            }
            assert!(body.len() > HELD_BYTES);
            std::fs::write(&path, &body).unwrap();
            let changed = crate::harness::cli::bound_ledger_cut(&path, MAX_BYTES, &|rows, read| {
                cut_rows(&path, rows, read)
            })
            .unwrap();
            let after = std::fs::read_to_string(&path).unwrap();
            if kind == "folded-records" {
                assert!(changed, "folded-away rows do not consume retained bytes");
                assert!(after.len() as u64 <= MAX_BYTES);
                assert!(after.ends_with(&(last + "\n")));
            } else {
                assert!(!changed, "{kind} must refuse the cut");
                assert_eq!(after, body, "{kind} must preserve the original bytes");
            }
        }
    }
    use super::*;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn a_row_carries_the_hash_of_the_whole_command_and_a_cut_copy() {
        let long = "x".repeat(COMMAND_BYTES + 10);
        let row = Row {
            rule_id: "rm-breaker@v1",
            outcome: Outcome::Approved,
            command: &long,
            reason: "every operand under a scratch root",
            box_seq: 42,
        };
        let json = row.to_json(1_000, Some("@s-1"));
        assert!(json.starts_with("{\"ts\":1000,\"sid\":\"s-1\",\"rule_id\":\"rm-breaker@v1\""));
        assert!(json.contains("\"decision\":\"approved\""));
        assert!(json.contains(&format!("\"command\":\"{}\"", "x".repeat(COMMAND_BYTES))));
        assert!(!json.contains(&"x".repeat(COMMAND_BYTES + 1)));
        assert!(json.contains(&format!("\"cmd_sha256\":\"{}\"", sha256_hex(&long))));
        assert!(json.ends_with("\"box_seq\":42}"));
    }

    #[test]
    fn typed_at_reads_the_sessions_typed_rows_only() {
        let dir = std::env::temp_dir().join(format!("aterm-typed-at-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("s-1.jsonl");
        let row = |outcome, command: &'static str| Row {
            rule_id: "continue@v1",
            outcome,
            command,
            reason: "-",
            box_seq: 1,
        };
        let forged = ",\"sid\":\"s-1\",\"decision\":\"typed\",";
        let login = |rule_id, command, ts| {
            Row {
                rule_id,
                outcome: Outcome::Typed,
                command,
                reason: "-",
                box_seq: 1,
            }
            .to_json(ts, Some("@s-1"))
        };
        let body = [
            row(Outcome::Typed, "keep going").to_json(3_000, Some("@s-1")),
            row(Outcome::Typed, "keep going").to_json(1_000, Some("@s-1")),
            row(Outcome::Escalated, forged).to_json(2_000, Some("@s-1")),
            row(Outcome::Typed, "keep going").to_json(4_000, Some("@s-2")),
            row(Outcome::Approved, "ls").to_json(5_000, Some("@s-1")),
            login(super::super::policy::turn_end::RULE_LOGIN, "/login", 6_000),
            login(super::super::policy::login::RULE_LOGIN_DONE, "enter", 7_000),
        ]
        .join("\n");
        std::fs::write(&path, body).expect("ledger");
        assert_eq!(typed_at(&path, Some("@s-1")), [1_000, 3_000, 6_000]);
        assert_eq!(typed_at(&path, Some("s-2")), [4_000]);
        assert!(typed_at(&dir.join("none.jsonl"), Some("@s-1")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_login_a_loop_before_typed_is_read_while_it_is_the_last_act() {
        use super::super::policy::login::RULE_LOGIN_DONE;
        use super::super::policy::turn_end::RULE_LOGIN;
        let typed = |rule_id, command, ts, sid| {
            Row {
                rule_id,
                outcome: Outcome::Typed,
                command,
                reason: "-",
                box_seq: 1,
            }
            .to_json(ts, Some(sid))
        };
        let waiting = |rule_id, ts| {
            Row {
                rule_id,
                outcome: Outcome::Waiting,
                command: "",
                reason: "-",
                box_seq: 1,
            }
            .to_json(ts, Some("@s-1"))
        };
        let login = typed(RULE_LOGIN, "/login", 6_000, "@s-1");
        let body = |after: &[String]| {
            let mut rows = vec![
                typed("continue@v1", "keep going", 1_000, "@s-1"),
                waiting(RULE_POINT, 5_000),
                login.clone(),
                waiting(RULE_POINT, 7_000),
            ];
            rows.extend(after.iter().cloned());
            rows.join("\n")
        };
        assert_eq!(login_typed_last_in(&body(&[]), Some("@s-1")), Some(6_000));
        assert_eq!(
            login_typed_last_in(
                &body(&[typed(RULE_LOGIN, "/login", 9_000, "@s-2")]),
                Some("s-1")
            ),
            Some(6_000),
            "another session's /login is no act of this one's"
        );
        for after in [
            typed(RULE_LOGIN_DONE, "enter", 8_000, "@s-1"),
            typed("continue@v1", "keep going", 8_000, "@s-1"),
            waiting(RULE_TURN_RAN, 8_000),
        ] {
            assert_eq!(
                login_typed_last_in(&body(std::slice::from_ref(&after)), Some("@s-1")),
                None,
                "{after}"
            );
        }
        assert_eq!(login_typed_last_in(&body(&[]), Some("@s-2")), None);
        assert_eq!(login_typed_last_in("", Some("@s-1")), None);
    }

    #[test]
    fn a_left_at_wall_key_carries_the_exchanges_work() {
        use std::time::Duration;
        let key = left_at_wall_key("prompt:u-1", Duration::from_millis(361_900));
        assert_eq!(key, "wall:prompt:u-1@361");
        assert_eq!(
            parse_left_at_wall(&key),
            Some(("prompt:u-1", Some(Duration::from_secs(361))))
        );
        assert_eq!(
            parse_left_at_wall("wall:said:00ff00ff00ff00ff"),
            Some(("said:00ff00ff00ff00ff", None))
        );
        assert_eq!(parse_left_at_wall("answer:a-1@361"), None);
        assert_eq!(
            parse_left_at_wall("wall:answer:a@b"),
            Some(("answer:a@b", None))
        );
    }

    #[test]
    fn open_left_is_the_last_left_row_no_typed_row_closed() {
        use super::super::policy::turn_end::{RULE_LEFT, RULE_USAGE_PROBE};
        let dir = std::env::temp_dir().join(format!("aterm-open-left-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("s-1.jsonl");
        let row = |rule, outcome, command: &'static str| Row {
            rule_id: rule,
            outcome,
            command,
            reason: LEFT_REASON,
            box_seq: 1,
        };
        let write = |rows: &[String]| std::fs::write(&path, rows.join("\n")).expect("ledger");
        let left = |key, ts| row(RULE_LEFT, Outcome::Skipped, key).to_json(ts, Some("@s-1"));
        let typed = |rule, text, ts| row(rule, Outcome::Typed, text).to_json(ts, Some("@s-1"));
        write(&[
            left("answer:a", 1_000),
            typed("continue@v1", "keep going", 2_000),
            left("answer:b", 3_000),
            typed(RULE_USAGE_PROBE, "/usage", 4_000),
        ]);
        assert_eq!(open_left(&path, Some("@s-1")).as_deref(), Some("answer:b"));
        assert_eq!(open_left(&path, Some("s-1")).as_deref(), Some("answer:b"));
        write(&[
            left("answer:a", 1_000),
            typed("continue@v1", "keep going", 2_000),
        ]);
        assert_eq!(
            open_left(&path, Some("@s-1")),
            None,
            "a typed row closed it"
        );
        write(&[left("answer:a", 1_000)]);
        assert_eq!(open_left(&path, Some("@s-2")), None, "another session's");
        write(&[row("continue@v1", Outcome::Skipped, "answer:a").to_json(1_000, Some("@s-1"))]);
        assert_eq!(open_left(&path, Some("@s-1")), None, "another rule's");
        assert_eq!(open_left(&dir.join("none.jsonl"), Some("@s-1")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_carry_ends_at_the_tasks_end_an_exchange_left_or_off_after_the_answer() {
        use super::super::policy::turn_end::{RULE_LEFT, RULE_USAGE_PROBE};
        let row = |rule, outcome, ts| {
            Row {
                rule_id: rule,
                outcome,
                command: "",
                reason: "",
                box_seq: 1,
            }
            .to_json(ts, Some("@s-1"))
        };
        let act = || row("continue@v1", Outcome::Typed, 1_000);
        let point = || row(RULE_POINT, Outcome::Waiting, 2_000);
        let ended = |rows: &[String]| carry_ended_in(&(rows.join("\n") + "\n"), Some("@s-1"));
        let done = row(RULE_TASK_DONE, Outcome::Waiting, 2_000);
        let left = row(RULE_LEFT, Outcome::Skipped, 2_000);
        let off = row(RULE_OFF_STOOD, Outcome::Waiting, 3_000);
        let esc_reason = point_reason(Carried::streak(1), true);
        let esc = Row {
            rule_id: RULE_POINT,
            outcome: Outcome::Waiting,
            command: "",
            reason: &esc_reason,
            box_seq: 1,
        }
        .to_json(2_000, Some("@s-1"));
        assert!(ended(&[act(), esc.clone()]), "a person's Esc on the answer");
        assert!(ended(&[act(), point(), done.clone()]), "the task's end");
        assert!(ended(&[act(), point(), left]), "an exchange left");
        assert!(
            ended(&[act(), point(), off.clone()]),
            "Off after the answer"
        );
        assert!(
            ended(&[
                act(),
                point(),
                done.clone(),
                row(RULE_USAGE_PROBE, Outcome::Typed, 4_000)
            ]),
            "the probe is no act"
        );
        assert!(!ended(&[act(), off]), "Off before the answer");
        assert!(
            !ended(&[act(), point(), esc.clone()]),
            "an Esc at a later point"
        );
        assert!(!ended(&[esc, act()]), "an Esc before the act");
        assert!(
            !ended(&[act(), point(), done.clone(), act()]),
            "an act since"
        );
        assert!(!ended(&[act(), point()]), "nothing ended it");
        assert!(!ended(&[]), "no act");
        assert!(
            !carry_ended_in(&(done + "\n"), Some("@s-2")),
            "another session's row"
        );
    }

    #[test]
    fn a_walls_retry_keeps_a_persons_exchange_at_the_wall_open() {
        use super::super::policy::turn_end::{
            RULE_API_BACK, RULE_API_RETRY, RULE_COMPACT, RULE_LEFT, RULE_LIMIT_RESUME,
        };
        let row = |rule, outcome, command: &'static str, ts| {
            Row {
                rule_id: rule,
                outcome,
                command,
                reason: LEFT_AT_WALL_REASON,
                box_seq: 1,
            }
            .to_json(ts, Some("@s-1"))
        };
        let wall = "wall:prompt:u-1@120";
        let retry = "Claude Code reported an overloaded API (529).  Carry on.";
        let body = |rows: &[String]| rows.join("\n") + "\n";
        let left = row(RULE_LEFT, Outcome::Skipped, wall, 1_000);
        for rule in [RULE_API_RETRY, RULE_API_BACK, RULE_LIMIT_RESUME] {
            let got = open_left_in(
                &body(&[left.clone(), row(rule, Outcome::Typed, retry, 2_000)]),
                Some("@s-1"),
            );
            assert_eq!(got, Some(left_retried_key(wall, retry)), "{rule}");
            let key = got.expect("open");
            assert_eq!(
                parse_left_retried(&key),
                Some((
                    words_hash("Claude Code reported an overloaded API (529). Carry on."),
                    wall
                )),
                "{rule}"
            );
        }
        let then = |rows: &[String]| open_left_in(&body(rows), Some("@s-1"));
        assert_eq!(
            then(&[
                left.clone(),
                row(RULE_API_RETRY, Outcome::Typed, retry, 2_000),
                row("continue@v1", Outcome::Typed, "keep going", 3_000),
            ]),
            None,
            "a continuation after the retry"
        );
        assert_eq!(
            then(&[
                left.clone(),
                row(RULE_COMPACT, Outcome::Typed, "/compact", 2_000)
            ]),
            None,
            "a command"
        );
        assert_eq!(
            then(&[
                row(RULE_LEFT, Outcome::Skipped, "prompt:u-1", 1_000),
                row(RULE_API_RETRY, Outcome::Typed, retry, 2_000),
            ]),
            None,
            "a retry after a row no wall's"
        );
        assert_eq!(parse_left_retried(wall), None);
    }

    #[test]
    fn the_probes_rows_are_no_typed_act_and_say_whether_a_panel_is_owed() {
        use super::super::policy::turn_end::{RULE_COMPACT, RULE_USAGE_PROBE};
        let dir = std::env::temp_dir().join(format!("aterm-probe-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("s-1.jsonl");
        let row = |rule_id, outcome, command: &'static str, reason: &'static str| Row {
            rule_id,
            outcome,
            command,
            reason,
            box_seq: 1,
        };
        let typed = || {
            row(
                RULE_USAGE_PROBE,
                Outcome::Typed,
                "/usage",
                PROBE_TYPED_REASON,
            )
        };
        let closed = || {
            row(
                RULE_USAGE_PROBE,
                Outcome::Waiting,
                "/usage",
                PROBE_CLOSED_REASON,
            )
        };
        let write = |rows: &[String]| std::fs::write(&path, rows.join("\n")).expect("ledger");

        write(&[
            row(RULE_COMPACT, Outcome::Typed, "/compact", "-").to_json(1_000, Some("@s-1")),
            typed().to_json(2_000, Some("@s-1")),
            closed().to_json(2_500, Some("@s-1")),
            row("continue@v1", Outcome::Typed, "/usage", "-").to_json(3_000, Some("@s-1")),
        ]);
        assert_eq!(typed_at(&path, Some("@s-1")), [1_000, 3_000]);
        let texts: Vec<(i64, String)> = typed_texts(&path, Some("@s-1"))
            .into_iter()
            .map(|t| (t.at_ms, t.text))
            .collect();
        assert_eq!(
            texts,
            [
                (1_000, "/compact".to_string()),
                (2_000, "/usage".to_string()),
                (3_000, "/usage".to_string()),
            ]
        );
        assert!(
            !probe_owed(&read(&path), Some("@s-1")),
            "the panel was seen gone, and the continue rule's row is no probe row"
        );

        write(&[
            typed().to_json(1_000, Some("@s-1")),
            closed().to_json(1_500, Some("@s-1")),
            typed().to_json(2_000, Some("@s-1")),
            row("continue@v1", Outcome::Typed, "keep going", "-").to_json(3_000, Some("@s-1")),
        ]);
        assert!(probe_owed(&read(&path), Some("@s-1")));
        assert!(
            probe_owed(&read(&path), Some("s-1")),
            "with or without the `@`"
        );
        assert!(
            !probe_owed(&read(&path), Some("@s-2")),
            "another session owes nothing"
        );

        let forged = ",\"rule_id\":\"usage-probe@v1\",\"decision\":\"typed\",";
        write(&[
            row("continue@v1", Outcome::Typed, "keep going", "-").to_json(1_000, Some("@s-1")),
            row("continue@v1", Outcome::Typed, forged, "-").to_json(2_000, Some("@s-1")),
        ]);
        assert!(!probe_owed(&read(&path), Some("@s-1")));
        assert_eq!(typed_at(&path, Some("@s-1")), [1_000, 2_000]);
        assert!(!probe_owed(&read(&dir.join("none.jsonl")), Some("@s-1")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reset_row_taken_and_not_ended_is_owed_and_says_whether_it_was_entered() {
        use super::super::policy::approval::{RULE_LIMIT_RESET, RULE_LIMIT_WAIT};
        let dir = std::env::temp_dir().join(format!("aterm-reset-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("s-1.jsonl");
        let row = |rule_id, outcome, command: &'static str, reason: &'static str| {
            Row {
                rule_id,
                outcome,
                command,
                reason,
                box_seq: 1,
            }
            .to_json(1_000, Some("@s-1"))
        };
        const GRANT: &str = "Use a weekly reset (2 left, until Oct 5)";
        const YES: &str = "Yes, use my weekly reset";
        let taken = || {
            row(
                RULE_LIMIT_RESET,
                Outcome::Approved,
                GRANT,
                "taken: wall until 9",
            )
        };
        let entered = || row(RULE_LIMIT_RESET, Outcome::Approved, YES, "Refills your …");
        let ended = || {
            row(
                RULE_LIMIT_RESET,
                Outcome::Waiting,
                "limit reset",
                "ended: kept",
            )
        };
        let write = |rows: &[String]| std::fs::write(&path, rows.join("\n")).expect("ledger");
        let owed = || reset_owed(&read(&path), Some("@s-1")).map(|open| open.entered);

        write(&[
            taken(),
            row("continue@v1", Outcome::Typed, "keep going", "-"),
        ]);
        assert_eq!(owed(), Some(false));
        assert_eq!(
            reset_owed(&read(&path), Some("s-1")).map(|open| open.entered),
            Some(false),
            "with or without the `@`"
        );
        assert_eq!(
            reset_owed(&read(&path), Some("@s-2")),
            None,
            "another session's"
        );
        assert_eq!(
            typed_at(&path, Some("@s-1")).len(),
            1,
            "no reset row is typed"
        );
        write(&[
            taken(),
            row(
                RULE_LIMIT_RESET,
                Outcome::Declined,
                "Use a weekly reset?",
                "focus",
            ),
        ]);
        assert_eq!(owed(), Some(false));
        for yes in [
            "Yes, use my 5-hour reset",
            YES,
            "Yes, use my full reset",
            "Yes, use my Fable reset",
            RESET_YES_2_1_287,
        ] {
            write(&[
                taken(),
                row(RULE_LIMIT_RESET, Outcome::Approved, yes, "Refills your …"),
            ]);
            assert_eq!(owed(), Some(true), "{yes}");
        }
        for yes in [
            "Yes, use my browser",
            "Yes, use my",
            "Yes, use my weekly resets",
            "Yes, use my resets",
        ] {
            write(&[
                taken(),
                row(RULE_LIMIT_RESET, Outcome::Approved, yes, "Refills your …"),
            ]);
            assert_eq!(owed(), Some(false), "CONTROL: {yes}");
        }
        write(&[taken(), entered().replace("\"ts\":1000,", "\"ts\":9000,")]);
        assert_eq!(
            reset_owed(&read(&path), Some("@s-1")),
            Some(ResetOpen {
                entered: true,
                taken_ms: 1_000
            })
        );
        write(&[taken(), entered(), ended()]);
        assert_eq!(owed(), None);
        write(&[taken(), entered(), ended(), taken()]);
        assert_eq!(owed(), Some(false));

        let forged = ",\"rule_id\":\"limit-reset@v1\",\"decision\":\"approved\",";
        write(&[
            row(
                RULE_LIMIT_RESET,
                Outcome::Skipped,
                GRANT,
                "the screen moved",
            ),
            row(RULE_LIMIT_RESET, Outcome::Deferred, GRANT, "ERR rate"),
            row(
                RULE_LIMIT_RESET,
                Outcome::Declined,
                "Use a weekly reset?",
                "focus",
            ),
            row(RULE_LIMIT_WAIT, Outcome::Approved, GRANT, "-"),
            row("no-spend@v1", Outcome::Approved, YES, "-"),
            row("continue@v1", Outcome::Typed, forged, "-"),
        ]);
        assert_eq!(owed(), None);
        assert_eq!(
            reset_owed(&read(&dir.join("none.jsonl")), Some("@s-1")),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn typed_texts_are_what_the_sessions_loop_typed() {
        let dir = std::env::temp_dir().join(format!("aterm-typed-texts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = ledger_under(&dir, Some("@s-1"));
        assert!(!dir.exists(), "a reader's path makes nothing");
        assert_eq!(path_under(&dir, Some("@s-1")), Some(path.clone()));
        let row = |rule_id, outcome, command: &'static str| Row {
            rule_id,
            outcome,
            command,
            reason: "the turn-end policy",
            box_seq: 1,
        };
        let forged = ",\"sid\":\"s-1\",\"decision\":\"typed\",";
        let body = [
            row("continue@v1", Outcome::Typed, "keep going").to_json(1_000, Some("@s-1")),
            row("answer@v1", Outcome::Typed, "Decide for yourself.").to_json(2_000, Some("@s-1")),
            row("continue@v1", Outcome::Typed, "keep going").to_json(3_000, Some("@s-1")),
            row("context-compact@v1", Outcome::Typed, forged).to_json(4_000, Some("@s-1")),
            row("continue@v1", Outcome::Typed, "carry on").to_json(5_000, Some("@s-2")),
            row("safe-read@v1", Outcome::Approved, "ls").to_json(6_000, Some("@s-1")),
        ]
        .join("\n");
        std::fs::write(&path, body).expect("ledger");
        let typed = |sid| {
            typed_texts(&path, Some(sid))
                .into_iter()
                .map(|t| (t.at_ms, t.text))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            typed("@s-1"),
            [
                (1_000, "keep going".to_string()),
                (2_000, "Decide for yourself.".to_string()),
                (3_000, "keep going".to_string()),
                (4_000, forged.to_string()),
            ]
        );
        assert_eq!(typed("s-2"), [(5_000, "carry on".to_string())]);
        assert!(typed_texts(&dir.join("drive/none.jsonl"), Some("@s-1")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_ledger_lives_under_the_state_root_by_sid_and_appends() {
        let root = std::env::temp_dir().join(format!("aterm-approvals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let path = path_under(&root, Some("@s-ab/../x")).expect("path");
        assert_eq!(path, root.join("drive").join("s-ab____x.jsonl"));
        assert_eq!(
            path_under(&root, None).expect("path"),
            root.join("drive").join("self.jsonl")
        );
        let mut warn = Vec::new();
        let mut l = Ledger::open(Some(&path), Some("@s-ab"), &mut warn);
        for seq in [1, 2] {
            l.write(
                crate::supervise::journal::unix_ms(),
                &Row {
                    rule_id: "-",
                    outcome: Outcome::Escalated,
                    command: "rm -rf /",
                    reason: "not read-only",
                    box_seq: seq,
                },
                &mut warn,
            );
        }
        let text = std::fs::read_to_string(&path).expect("ledger");
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(
            text.lines()
                .all(|l| l.contains("\"decision\":\"escalated\""))
        );
        assert!(warn.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn seed_ledger(dir: &Path, name: &str, rows: &[(i64, &str, Outcome, &str, String)]) -> PathBuf {
        let path = dir.join(format!("{name}.jsonl"));
        let body: String = rows
            .iter()
            .map(|(ts, rule_id, outcome, command, reason)| {
                let row = Row {
                    rule_id,
                    outcome: *outcome,
                    command,
                    reason,
                    box_seq: 1,
                };
                format!("{}\n", row.to_json(*ts, Some("@s-9")))
            })
            .collect();
        std::fs::write(&path, body).expect("write");
        path
    }

    fn spend_wall_reason() -> String {
        wall_wait_reason(
            None,
            None,
            aterm_phase::WallKind::Spend,
            "You've hit your monthly spend limit.",
        )
    }

    #[test]
    fn a_record_between_a_try_and_its_first_read_is_no_read() {
        use super::super::policy::turn_end::{RULE_LIMIT_RESUME, RULE_MODEL_FALLBACK};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "first-read");
        let limit = LimitTry {
            bucket: false,
            in_place: false,
            attempts: 1,
            entitlement: true,
        };
        let records: [(&str, &str, &str); 5] = [
            ("none", "", ""),
            ("off", RULE_OFF_STOOD, OFF_STOOD),
            ("land-over", RULE_LAND_OVER, LAND_OVER),
            ("task-done", RULE_TASK_DONE, TASK_DONE),
            ("fallback-asked", RULE_MODEL_FALLBACK, FALLBACK_ASKED),
        ];
        for (tag, rule, reason) in records {
            let mut rows = vec![
                (
                    1_000,
                    RULE_WALL_WAIT,
                    Outcome::Waiting,
                    "",
                    spend_wall_reason(),
                ),
                (
                    10_000,
                    RULE_LIMIT_RESUME,
                    Outcome::Typed,
                    "keep going",
                    act_reason(Some(limit)),
                ),
            ];
            if !rule.is_empty() {
                rows.push((12_000, rule, Outcome::Waiting, "", reason.to_string()));
            }
            rows.push((
                30_000,
                RULE_WALL_WAIT,
                Outcome::Waiting,
                "",
                spend_wall_reason(),
            ));
            let path = seed_ledger(&dir, &format!("first-read-{tag}"), &rows);
            let a = awaited_try(&path, Some("@s-9")).expect("the try");
            assert_eq!(
                (a.read_ms, a.unread, a.met),
                (Some(30_000), false, true),
                "{tag}"
            );
            let u = usage_attempt(&path, Some("@s-9")).expect("the try's episode");
            assert_eq!((u.read_ms, u.met_ms), (Some(30_000), Some(30_000)), "{tag}");
            rows.pop();
            let path = seed_ledger(&dir, &format!("first-read-{tag}-unread"), &rows);
            let a = awaited_try(&path, Some("@s-9")).expect("the try");
            assert!(a.unread && a.read_ms.is_none(), "{tag}: {a:?}");
        }
    }

    #[test]
    fn a_try_aged_out_for_a_successor_ends_no_wall() {
        use super::super::policy::turn_end::{AccountAt, RULE_ACCOUNT, RULE_DONE_CHECK, RULE_LAND};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "try-over");
        let limit = LimitTry {
            bucket: false,
            in_place: true,
            attempts: 1,
            entitlement: true,
        };
        for (act_rule, check) in [(RULE_DONE_CHECK, true), (RULE_LAND, false)] {
            for (tag, over_rule, over_reason, ended) in [
                ("try-over", RULE_TRY_OVER, TRY_OVER, false),
                (
                    "limit-ended (control)",
                    RULE_WALL_WAIT_END,
                    LIMIT_ENDED,
                    true,
                ),
            ] {
                let rows = vec![
                    (
                        1_000,
                        RULE_WALL_WAIT,
                        Outcome::Waiting,
                        "",
                        spend_wall_reason(),
                    ),
                    (
                        10_000,
                        act_rule,
                        Outcome::Typed,
                        "(the act)",
                        act_reason(Some(limit)),
                    ),
                    (
                        10_002,
                        RULE_WALL_WAIT,
                        Outcome::Waiting,
                        "",
                        spend_wall_reason(),
                    ),
                    (
                        10_500,
                        RULE_ACCOUNT,
                        Outcome::Skipped,
                        "relaunch under account w2",
                        account_unmade_reason("w2", AccountAt::Wall, Some("refused")),
                    ),
                    (
                        9_000_000,
                        over_rule,
                        Outcome::Waiting,
                        "",
                        over_reason.to_string(),
                    ),
                    (
                        9_000_000,
                        RULE_WALL_WAIT,
                        Outcome::Waiting,
                        "",
                        spend_wall_reason(),
                    ),
                ];
                let path = seed_ledger(&dir, &format!("try-over-{act_rule}-{over_rule}"), &rows);
                let sid = Some("@s-9");
                assert_eq!(usage_attempt(&path, sid), None, "{act_rule} {tag}");
                assert!(open_wall(&path, sid).is_some(), "{act_rule} {tag}");
                assert_eq!(
                    account_moves(&path, sid).unmade.is_some(),
                    !ended,
                    "{act_rule} {tag}"
                );
                if check {
                    let owed = done_check_owed(&path, sid);
                    assert_eq!(owed.is_some_and(|o| o.cut), !ended, "{tag}: {owed:?}");
                } else {
                    let ep = land_episode(&path, sid).expect("the episode");
                    assert_eq!(ep.cut, !ended, "{tag}: {ep:?}");
                }
            }
        }
    }

    #[test]
    fn the_short_streak_is_the_last_act_s_or_point_s() {
        use super::super::policy::turn_end::{RULE_CONTINUE, RULE_MODEL_FALLBACK};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "streak");
        let act = |ts: i64, n: u32| {
            (
                ts,
                RULE_CONTINUE,
                Outcome::Typed,
                "keep going",
                act_reason_after(Carried::streak(n), None),
            )
        };
        let point = |ts: i64, n: u32| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried::streak(n), false),
            )
        };
        let relaunch = (
            9_000,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model sonnet",
            model_switch_reason(Some("opus"), None, 9),
        );
        let sid = Some("@s-9");
        let path = seed_ledger(&dir, "streak-acts", &[act(1_000, 2), act(2_000, 3)]);
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!(
            (s.at_ms, s.short, s.point, s.rule.as_str(), s.recorded),
            (2_000, 3, false, RULE_CONTINUE, false)
        );
        let path = seed_ledger(
            &dir,
            "streak-point",
            &[act(1_000, 2), act(2_000, 3), point(3_000, 4)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.short, s.point), (3_000, 4, true));
        let path = seed_ledger(
            &dir,
            "streak-relaunch",
            &[
                act(1_000, 2),
                act(2_000, 3),
                point(3_000, 4),
                relaunch.clone(),
            ],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.short, s.point), (3_000, 4, true));
        let path = seed_ledger(
            &dir,
            "streak-act-after",
            &[act(1_000, 2), point(3_000, 4), relaunch, act(10_000, 4)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!(
            (s.at_ms, s.short, s.point, s.recorded),
            (10_000, 4, false, true)
        );
        let path = seed_ledger(&dir, "streak-ended", &[act(1_000, 2), point(3_000, 0)]);
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.short, s.point), (0, true));
        let ran = |ts: i64| {
            (
                ts,
                RULE_TURN_RAN,
                Outcome::Waiting,
                "",
                TURN_RAN.to_string(),
            )
        };
        let path = seed_ledger(
            &dir,
            "streak-ran",
            &[act(1_000, 2), point(3_000, 3), ran(4_000)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.short, s.point, s.ran), (3_000, 3, true, true));
        let path = seed_ledger(
            &dir,
            "streak-ran-point",
            &[point(3_000, 3), ran(4_000), point(5_000, 4)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.short, s.point, s.ran), (5_000, 4, true, false));
        let path = seed_ledger(
            &dir,
            "streak-ran-act",
            &[point(3_000, 3), act(4_000, 3), ran(5_000)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.point, s.ran), (4_000, false, false));
        let limit = LimitTry {
            bucket: true,
            in_place: true,
            attempts: 2,
            entitlement: false,
        };
        let older = format!(
            "the turn-end policy ({SHORT_STREAK}: 2) (the open-items challenge was typed this \
             job) (in the limit's place: wall=bucket attempts=2)"
        );
        assert_eq!(limit_try_of(&older), Some(limit), "{older}");
        let path = seed_ledger(
            &dir,
            "streak-challenged-act",
            &[
                act(1_000, 1),
                (2_000, RULE_CONTINUE, Outcome::Typed, "keep going", older),
            ],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.short), (2_000, 2));
        let esc_point = |ts: i64, esc: bool| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried::streak(0), esc),
            )
        };
        let path = seed_ledger(&dir, "streak-esc", &[act(1_000, 1), esc_point(3_000, true)]);
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.point, s.esc), (3_000, true, true));
        let path = seed_ledger(&dir, "streak-no-esc", &[esc_point(3_000, false)]);
        assert!(!short_streak(&path, sid).expect("a streak").esc);
        let words = format!(
            "{} ({PERSONS_ESC})",
            act_reason_after(Carried::streak(1), None)
        );
        let path = seed_ledger(
            &dir,
            "streak-esc-act",
            &[(2_000, RULE_CONTINUE, Outcome::Typed, "keep going", words)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.point, s.esc), (false, false));
    }

    #[test]
    fn the_wall_s_retries_read_back_as_written() {
        use super::super::policy::turn_end::RULE_API_RETRY;
        use aterm_phase::WallKind;
        let dir = crate::supervise::ScratchDir::new("apr-seed", "retries");
        let sid = Some("@s-9");
        let mut kinds = vec![WallKind::Overloaded];
        for (cause, _) in API_CAUSES {
            for code in [None, Some(529)] {
                for retryable in [false, true] {
                    kinds.push(WallKind::ApiError {
                        code,
                        retryable,
                        cause,
                    });
                }
            }
        }
        for (n, kind) in kinds.into_iter().enumerate() {
            let mark = RetryMark {
                kind,
                attempts: n,
                ups: n / 2,
                since_ms: 1_789_682_580_000 + i64::try_from(n).expect("small"),
            };
            let carried = Carried {
                short: 2,
                retry: Some(mark),
            };
            let reason = act_reason_after(carried, None);
            let path = seed_ledger(
                &dir,
                &format!("retries-act-{n}"),
                &[(2_000, RULE_API_RETRY, Outcome::Typed, "try again", reason)],
            );
            let s = short_streak(&path, sid).expect("a streak");
            assert_eq!((s.short, s.retry), (2, Some(mark)), "{kind:?}");
            let path = seed_ledger(
                &dir,
                &format!("retries-point-{n}"),
                &[(
                    3_000,
                    RULE_POINT,
                    Outcome::Waiting,
                    "",
                    point_reason(carried, true),
                )],
            );
            let s = short_streak(&path, sid).expect("a streak");
            assert_eq!(
                (s.point, s.esc, s.retry),
                (true, true, Some(mark)),
                "{kind:?}"
            );
        }
        let limit = LimitTry {
            bucket: false,
            in_place: false,
            attempts: 1,
            entitlement: true,
        };
        let both = act_reason_after(
            Carried {
                short: 0,
                retry: Some(RetryMark {
                    kind: WallKind::Overloaded,
                    attempts: 3,
                    ups: 0,
                    since_ms: 7,
                }),
            },
            Some(limit),
        );
        assert_eq!(limit_try_of(&both), Some(limit), "{both}");
        assert!(retry_mark_of(&both).is_some(), "{both}");
        assert_eq!(
            retry_mark_of(&act_reason_after(Carried::streak(2), None)),
            None
        );
        let garbled = format!("the turn-end policy ({RETRY_LADDER}: kind=overloaded attempts=x)");
        assert_eq!(retry_mark_of(&garbled), None);
    }

    #[test]
    fn a_compactions_continuation_that_types_the_check_again_keeps_it_owed() {
        use super::super::policy::turn_end::{
            DONE_CHECK, DONE_CHECK_MARKER_BEFORE, DONE_CHECK_ORDINARY_BEFORE, LAND, RULE_COMPACT,
            RULE_DONE_CHECK, RULE_LAND,
        };
        let ledger = |rows: &[(i64, &str, Outcome, &str, String)]| -> String {
            rows.iter()
                .map(|(ts, rule_id, outcome, command, reason)| {
                    let row = Row {
                        rule_id,
                        outcome: *outcome,
                        command,
                        reason,
                        box_seq: 1,
                    };
                    format!("{}\n", row.to_json(*ts, Some("@s-9")))
                })
                .collect()
        };
        let sid = Some("@s-9");
        let wall = (
            20_000,
            RULE_WALL_WAIT,
            Outcome::Waiting,
            "",
            spend_wall_reason(),
        );
        let point = |ts: i64| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried::streak(0), false),
            )
        };
        let rows = |act: (&'static str, &'static str), again: &'static str| {
            vec![
                (10_000, act.0, Outcome::Typed, act.1, act_reason(None)),
                wall.clone(),
                (
                    30_000,
                    RULE_COMPACT,
                    Outcome::Typed,
                    "/compact",
                    act_reason(None),
                ),
                point(40_000),
                (
                    50_000,
                    RULE_COMPACT,
                    Outcome::Typed,
                    again,
                    act_reason(None),
                ),
            ]
        };
        for check in [
            DONE_CHECK,
            DONE_CHECK_ORDINARY_BEFORE,
            DONE_CHECK_MARKER_BEFORE,
        ] {
            let body = ledger(&rows((RULE_DONE_CHECK, check), check));
            let owed = done_check_owed_in(&body, sid).expect("owed");
            assert!(!owed.cut && !owed.answered, "{owed:?}");
            assert_eq!(owed.rule, RULE_DONE_CHECK);
            let wrapped = check.split_whitespace().collect::<Vec<_>>().join("\n");
            assert!(delivers_check(Some(&wrapped)));
        }
        let body = ledger(&rows((RULE_LAND, LAND), LAND));
        let ep = land_episode_in(&body, sid).expect("the episode");
        assert!(ep.landing && !ep.cut, "{ep:?}");
        let body = ledger(&rows((RULE_DONE_CHECK, DONE_CHECK), "keep going"));
        assert_eq!(done_check_owed_in(&body, sid), None);
        assert!(!delivers_check(None));
        assert!(!delivers_check(Some("keep going")));
        let body = ledger(&rows((RULE_LAND, LAND), "keep going"));
        assert!(land_episode_in(&body, sid).is_none_or(|ep| !ep.landing));
    }

    #[test]
    fn an_answer_judged_is_owed_no_more() {
        use super::super::policy::turn_end::{RULE_DONE_CHECK, RULE_LAND, RULE_LIMIT_RESUME};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "owed");
        let sid = Some("@s-9");
        let check = (
            10_000,
            RULE_DONE_CHECK,
            Outcome::Typed,
            "(the check)",
            act_reason(None),
        );
        let point = |ts: i64| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried::streak(1), false),
            )
        };
        let done_at = |ts: i64| {
            (
                ts,
                RULE_TASK_DONE,
                Outcome::Waiting,
                "",
                TASK_DONE.to_string(),
            )
        };
        let done = done_at(20_000);
        let wall = (
            20_000,
            RULE_WALL_WAIT,
            Outcome::Waiting,
            "",
            spend_wall_reason(),
        );
        let path = seed_ledger(&dir, "owed", std::slice::from_ref(&check));
        let owed = done_check_owed(&path, sid).expect("owed");
        assert!(owed.check.is_some() && !owed.answered, "{owed:?}");
        let path = seed_ledger(&dir, "owed-not-done", &[check.clone(), point(20_000)]);
        assert_eq!(done_check_owed(&path, sid), None);
        let path = seed_ledger(
            &dir,
            "owed-done",
            &[check.clone(), point(20_000), done.clone()],
        );
        let owed = done_check_owed(&path, sid).expect("answered");
        assert!(owed.answered && owed.check.is_none(), "{owed:?}");
        let path = seed_ledger(
            &dir,
            "owed-later",
            &[check.clone(), point(20_000), done, point(30_000)],
        );
        assert_eq!(done_check_owed(&path, sid), None);
        let path = seed_ledger(&dir, "owed-cut", &[check, wall, point(20_000)]);
        let owed = done_check_owed(&path, sid).expect("cut");
        assert!(owed.cut, "{owed:?}");
        let limit = LimitTry {
            bucket: false,
            in_place: false,
            attempts: 1,
            entitlement: true,
        };
        let tried = (
            10_000,
            RULE_LIMIT_RESUME,
            Outcome::Typed,
            "keep going",
            act_reason(Some(limit)),
        );
        let path = seed_ledger(&dir, "try-judged", &[tried.clone(), point(20_000)]);
        assert_eq!(awaited_try(&path, sid), None);
        let path = seed_ledger(&dir, "try-unjudged", &[tried]);
        assert!(awaited_try(&path, sid).is_some());
        let land = (
            10_000,
            RULE_LAND,
            Outcome::Typed,
            "(the land instruction)",
            act_reason(None),
        );
        let landing = |name: &str, rows: &[(i64, &str, Outcome, &str, String)]| {
            let e = land_episode(&seed_ledger(&dir, name, rows), sid).expect("an episode");
            (e.landing, e.answered)
        };
        assert_eq!(landing("land", std::slice::from_ref(&land)), (true, false));
        assert_eq!(
            landing("land-not-done", &[land.clone(), point(20_000)]),
            (false, false)
        );
        assert_eq!(
            landing("land-done", &[land, point(20_000), done_at(20_001)]),
            (true, true)
        );
    }

    #[test]
    fn a_done_check_an_api_error_cut_is_owed_to_the_walls_retry() {
        use super::super::policy::turn_end::{RULE_API_RETRY, RULE_DONE_CHECK};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "check-cut-by-api");
        let sid = Some("@s-9");
        let check = (
            10_000,
            RULE_DONE_CHECK,
            Outcome::Typed,
            "(the check)",
            act_reason(None),
        );
        let mark = |attempts: usize| RetryMark {
            kind: aterm_phase::WallKind::Overloaded,
            attempts,
            ups: 0,
            since_ms: 310_000,
        };
        let point = |ts: i64, retry: Option<RetryMark>| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried { short: 1, retry }, false),
            )
        };
        let retry = |ts: i64| {
            (
                ts,
                RULE_API_RETRY,
                Outcome::Typed,
                "(the retry)",
                act_reason_after(
                    Carried {
                        short: 1,
                        retry: Some(mark(1)),
                    },
                    None,
                ),
            )
        };
        let cut = point(310_000, Some(mark(0)));
        let owed = |name: &str, rows: &[(i64, &str, Outcome, &str, String)]| {
            done_check_owed(&seed_ledger(&dir, name, rows), sid)
        };
        let o = owed("cut", &[check.clone(), cut.clone()]).expect("cut");
        assert!(o.cut && o.check.is_none() && !o.answered, "{o:?}");
        let o = owed("retried", &[check.clone(), cut.clone(), retry(370_000)]).expect("again");
        assert!(!o.cut && o.check.is_none(), "{o:?}");
        let answered = [
            check.clone(),
            cut.clone(),
            retry(370_000),
            point(550_000, None),
        ];
        assert_eq!(owed("answered", &answered), None);
        for (name, theirs) in [
            ("theirs-cut", point(880_000, Some(mark(0)))),
            ("theirs", point(880_000, None)),
        ] {
            let rows = [check.clone(), cut.clone(), theirs];
            assert_eq!(owed(name, &rows), None, "{name}");
            let rows = [rows.as_slice(), &[retry(940_000)]].concat();
            assert_eq!(owed(&format!("{name}-retried"), &rows), None, "{name}");
        }
        assert_eq!(owed("unmarked", &[check, point(310_000, None)]), None);
    }

    #[test]
    fn a_compact_at_the_checks_point_owes_the_check_to_its_continuation() {
        use super::super::policy::turn_end::{
            DONE_CHECK, RULE_API_RETRY, RULE_COMPACT, RULE_DONE_CHECK, RULE_LIMIT_RESUME,
        };
        let sid = Some("@s-9");
        let body = |rows: &[(i64, &str, Outcome, &str, String)]| -> String {
            rows.iter()
                .map(|(ts, rule_id, outcome, command, reason)| {
                    let row = Row {
                        rule_id,
                        outcome: *outcome,
                        command,
                        reason,
                        box_seq: 1,
                    };
                    format!("{}\n", row.to_json(*ts, sid))
                })
                .collect()
        };
        let owed =
            |rows: &[(i64, &str, Outcome, &str, String)]| done_check_owed_in(&body(rows), sid);
        let typed = |ts: i64, rule: &'static str, words: &'static str| {
            (ts, rule, Outcome::Typed, words, act_reason(None))
        };
        let check = typed(10_000, RULE_DONE_CHECK, DONE_CHECK);
        let mark = RetryMark {
            kind: aterm_phase::WallKind::Overloaded,
            attempts: 0,
            ups: 0,
            since_ms: 20_000,
        };
        let point = |ts: i64, retry: Option<RetryMark>| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried { short: 0, retry }, false),
            )
        };
        let waiting =
            |ts: i64, rule: &'static str, reason: String| (ts, rule, Outcome::Waiting, "", reason);
        let compact = |ts: i64| typed(ts, RULE_COMPACT, "/compact");
        let cut = [check.clone(), point(20_000, None), compact(30_000)];
        let compacted = [cut.as_slice(), &[point(40_000, None)]].concat();
        for (name, rows) in [
            ("the /compact", cut.to_vec()),
            ("its answer", compacted.clone()),
            (
                "again on the ladder",
                [
                    compacted.as_slice(),
                    &[compact(50_000), point(60_000, None)],
                ]
                .concat(),
            ),
            (
                "after a 529's retry",
                vec![
                    check.clone(),
                    point(20_000, Some(mark)),
                    typed(25_000, RULE_API_RETRY, "(the retry)"),
                    point(28_000, None),
                    compact(30_000),
                ],
            ),
            (
                "under a wall row's cut",
                vec![
                    check.clone(),
                    waiting(20_000, RULE_WALL_WAIT, spend_wall_reason()),
                    compact(30_000),
                ],
            ),
            (
                "a wall the /compact met",
                [
                    cut.as_slice(),
                    &[waiting(40_000, RULE_WALL_WAIT, spend_wall_reason())],
                ]
                .concat(),
            ),
        ] {
            let o = owed(&rows).unwrap_or_else(|| panic!("{name}: owed"));
            assert!(o.compact && !o.cut && o.check.is_none(), "{name}: {o:?}");
        }
        let again = [
            compacted.as_slice(),
            &[typed(50_000, RULE_COMPACT, DONE_CHECK)],
        ]
        .concat();
        let o = owed(&again).expect("again");
        assert!(!o.compact && !o.cut, "{o:?}");
        for (name, rows) in [
            (
                "keep going",
                [
                    compacted.as_slice(),
                    &[typed(50_000, RULE_COMPACT, "keep going")],
                ]
                .concat(),
            ),
            (
                "Off before the /compact",
                vec![
                    check.clone(),
                    point(20_000, None),
                    waiting(25_000, RULE_OFF_STOOD, OFF_STOOD.to_string()),
                    compact(30_000),
                ],
            ),
            (
                "Off after it",
                [
                    cut.as_slice(),
                    &[waiting(35_000, RULE_OFF_STOOD, OFF_STOOD.to_string())],
                ]
                .concat(),
            ),
            (
                "a wall's try",
                [
                    cut.as_slice(),
                    &[
                        waiting(40_000, RULE_WALL_WAIT, spend_wall_reason()),
                        typed(50_000, RULE_LIMIT_RESUME, "keep going"),
                    ],
                ]
                .concat(),
            ),
            (
                "a 529's retry",
                [
                    cut.as_slice(),
                    &[
                        point(40_000, Some(mark)),
                        typed(50_000, RULE_API_RETRY, "(the retry)"),
                    ],
                ]
                .concat(),
            ),
            (
                "a later turn's",
                vec![
                    check.clone(),
                    point(20_000, None),
                    point(25_000, None),
                    compact(30_000),
                ],
            ),
        ] {
            assert_eq!(owed(&rows), None, "{name}");
        }
        let last =
            |rows: &[(i64, &str, Outcome, &str, String)]| check_point_last_in(&body(rows), sid);
        assert_eq!(
            last(&[check.clone(), point(20_000, None)]),
            Some(RULE_DONE_CHECK)
        );
        for (name, rows) in [
            ("the check unanswered", vec![check.clone()]),
            (
                "its point cut",
                vec![check.clone(), point(20_000, Some(mark))],
            ),
            (
                "a later turn's",
                vec![check.clone(), point(20_000, None), point(25_000, None)],
            ),
            (
                "Off",
                vec![
                    check.clone(),
                    point(20_000, None),
                    waiting(25_000, RULE_OFF_STOOD, OFF_STOOD.to_string()),
                ],
            ),
            ("the /compact", cut.to_vec()),
        ] {
            assert_eq!(last(&rows), None, "{name}");
        }
    }

    #[test]
    fn a_compact_at_a_land_acts_point_owes_the_act_to_its_continuation() {
        use super::super::policy::turn_end::{
            LAND, LAND_ANSWER, LAND_NUDGE, RULE_API_RETRY, RULE_COMPACT, RULE_LAND,
            RULE_LAND_ANSWER, RULE_LAND_NUDGE, RULE_LIMIT_RESUME,
        };
        type R = (i64, &'static str, Outcome, &'static str, String);
        let sid = Some("@s-9");
        let ep = |rows: &[R]| -> Option<LandSeed> {
            let body: String = rows
                .iter()
                .map(|(ts, rule_id, outcome, command, reason)| {
                    let row = Row {
                        rule_id,
                        outcome: *outcome,
                        command,
                        reason,
                        box_seq: 1,
                    };
                    format!("{}\n", row.to_json(*ts, sid))
                })
                .collect();
            land_episode_in(&body, sid)
        };
        let typed = |ts: i64, rule: &'static str, words: &'static str| -> R {
            (ts, rule, Outcome::Typed, words, act_reason(None))
        };
        let mark = RetryMark {
            kind: aterm_phase::WallKind::Overloaded,
            attempts: 0,
            ups: 0,
            since_ms: 20_000,
        };
        let point = |ts: i64, retry: Option<RetryMark>| -> R {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried { short: 0, retry }, false),
            )
        };
        let waiting = |ts: i64, rule: &'static str, reason: String| -> R {
            (ts, rule, Outcome::Waiting, "", reason)
        };
        let compact = |ts: i64| typed(ts, RULE_COMPACT, "/compact");
        let given = typed(10_000, RULE_LAND, LAND);
        let cut = [given.clone(), point(20_000, None), compact(30_000)];
        let compacted = [cut.as_slice(), &[point(40_000, None)]].concat();
        let owes = |name: &str, rows: &[R], rule: &str| {
            let e = ep(rows).unwrap_or_else(|| panic!("{name}: in hand"));
            assert!(e.compact && !e.landing && !e.cut, "{name}: {e:?}");
            assert_eq!(e.owed, Some(rule), "{name}");
        };
        for (name, rows) in [
            ("the /compact", cut.to_vec()),
            ("its answer", compacted.clone()),
            (
                "again on the ladder",
                [
                    compacted.as_slice(),
                    &[compact(50_000), point(60_000, None)],
                ]
                .concat(),
            ),
            (
                "after a 529's retry",
                vec![
                    given.clone(),
                    point(20_000, Some(mark)),
                    typed(25_000, RULE_API_RETRY, "(the retry)"),
                    point(28_000, None),
                    compact(30_000),
                ],
            ),
            (
                "under a wall row's cut",
                vec![
                    given.clone(),
                    waiting(20_000, RULE_WALL_WAIT, spend_wall_reason()),
                    compact(30_000),
                ],
            ),
            (
                "a wall the /compact met",
                [
                    cut.as_slice(),
                    &[waiting(40_000, RULE_WALL_WAIT, spend_wall_reason())],
                ]
                .concat(),
            ),
        ] {
            owes(name, &rows, RULE_LAND);
        }
        for (rule, words) in [
            (RULE_LAND_NUDGE, LAND_NUDGE),
            (RULE_LAND_ANSWER, LAND_ANSWER),
        ] {
            let rows = [
                given.clone(),
                point(12_000, None),
                typed(14_000, rule, words),
                point(20_000, None),
                compact(30_000),
            ];
            owes(rule, &rows, rule);
        }
        let answer = [
            given.clone(),
            point(12_000, None),
            typed(14_000, RULE_LAND_ANSWER, LAND_ANSWER),
        ];
        for (rule, words, mut rows) in [
            (RULE_LAND, LAND, vec![given.clone()]),
            (RULE_LAND_ANSWER, LAND_ANSWER, answer.to_vec()),
        ] {
            rows.extend([
                point(20_000, None),
                compact(30_000),
                point(40_000, None),
                typed(50_000, RULE_COMPACT, words),
            ]);
            let e = ep(&rows).expect("again");
            assert!(e.landing && !e.compact, "{rule}: {e:?}");
            assert_eq!(e.owed, Some(rule), "{rule}");
            rows.extend([point(60_000, None), compact(70_000)]);
            owes("its continuation's own cut", &rows, rule);
        }
        for (name, rows) in [
            (
                "keep going",
                [
                    compacted.as_slice(),
                    &[typed(50_000, RULE_COMPACT, "keep going")],
                ]
                .concat(),
            ),
            (
                "Off before the /compact",
                vec![
                    given.clone(),
                    point(20_000, None),
                    waiting(25_000, RULE_OFF_STOOD, OFF_STOOD.to_string()),
                    compact(30_000),
                ],
            ),
            (
                "Off after it",
                [
                    cut.as_slice(),
                    &[waiting(35_000, RULE_OFF_STOOD, OFF_STOOD.to_string())],
                ]
                .concat(),
            ),
            (
                "a wall's try",
                [
                    cut.as_slice(),
                    &[
                        waiting(40_000, RULE_WALL_WAIT, spend_wall_reason()),
                        typed(50_000, RULE_LIMIT_RESUME, "keep going"),
                    ],
                ]
                .concat(),
            ),
            (
                "a 529's retry",
                [
                    cut.as_slice(),
                    &[
                        point(40_000, Some(mark)),
                        typed(50_000, RULE_API_RETRY, "(the retry)"),
                    ],
                ]
                .concat(),
            ),
            (
                "a later turn's",
                vec![
                    given.clone(),
                    point(20_000, None),
                    point(25_000, None),
                    compact(30_000),
                ],
            ),
        ] {
            let e = ep(&rows);
            assert!(
                e.is_none_or(|e| !e.compact && !e.landing && e.owed.is_none()),
                "{name}: {e:?}"
            );
        }
        let e = ep(&[given.clone(), point(20_000, None)]).expect("judged");
        assert!(e.judged && !e.landing, "{e:?}");
        assert_eq!(e.owed, Some(RULE_LAND));
        for (name, rows) in [
            ("the act unanswered", vec![given.clone()]),
            (
                "its point cut",
                vec![given.clone(), point(20_000, Some(mark))],
            ),
            (
                "a later turn's",
                vec![given.clone(), point(20_000, None), point(25_000, None)],
            ),
            (
                "Off",
                vec![
                    given.clone(),
                    point(20_000, None),
                    waiting(25_000, RULE_OFF_STOOD, OFF_STOOD.to_string()),
                ],
            ),
            (
                "landed there",
                vec![
                    given.clone(),
                    point(20_000, None),
                    waiting(21_000, RULE_TASK_DONE, TASK_DONE.to_string()),
                ],
            ),
            ("the /compact", cut.to_vec()),
        ] {
            assert!(ep(&rows).is_none_or(|e| !e.judged), "{name}");
        }
    }

    #[test]
    fn a_cut_keeps_what_the_readers_read_of_a_run_of_records() {
        use super::super::policy::turn_end::{RULE_LAND, RULE_LAND_NUDGE, RULE_MODEL_FALLBACK};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "cut");
        let sid = Some("@s-9");
        let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
        let mut rows: Vec<(i64, &str, Outcome, &str, String)> = (0..3000)
            .map(|i| {
                let why = "the safe rules proved the command reads and runs tests".to_string();
                (i, "allow-once@v1", Outcome::Approved, filler, why)
            })
            .collect();
        let t = 1_000_000_i64;
        let switch = model_switch_reason(Some("opus"), Some(t / 1000 + 3 * 86_400), t / 1000);
        rows.push((
            t,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model sonnet",
            switch,
        ));
        let point = |ts: i64, n: u32| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried::streak(n), false),
            )
        };
        let ran = |ts: i64| {
            (
                ts,
                RULE_TURN_RAN,
                Outcome::Waiting,
                "",
                TURN_RAN.to_string(),
            )
        };
        for (k, rule) in [RULE_LAND, RULE_LAND_NUDGE, RULE_LAND_NUDGE]
            .into_iter()
            .enumerate()
        {
            let at = t + 1_000 + 1_000 * i64::try_from(k).unwrap();
            rows.push((at, rule, Outcome::Typed, "(land)", act_reason(None)));
            rows.push(point(at + 500, 0));
        }
        let wakes = 1100_u32;
        for n in 1..=wakes {
            let at = t + 10_000 + 300_000 * i64::from(n);
            rows.push(ran(at));
            rows.push(point(at + 20_000, n));
        }
        rows.push(ran(t + 10_000 + 300_000 * i64::from(wakes + 1)));
        let read = |path: &Path| {
            (
                open_model_switch(path, sid).map(|o| o.to),
                land_episode(path, sid),
                short_streak(path, sid),
                done_check_owed(path, sid),
                awaited_try(path, sid),
            )
        };
        let path = seed_ledger(&dir, "whole", &rows);
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
        let before = read(&path);
        assert!(before.0.is_some() && before.1.is_some(), "{before:?}");
        let streak = before.2.clone().expect("a streak");
        assert!(
            streak.point && streak.ran && streak.short == wakes,
            "{streak:?}"
        );
        bound(&path);
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(kept.lines().filter(|l| record_of(l).is_none()).count() <= KEEP_ROWS);
        assert_eq!(read(&path), before);
        let records: Vec<&str> = kept
            .lines()
            .filter(|l| l.contains("point@v1") || l.contains("turn-ran@v1"))
            .collect();
        assert_eq!(records.len(), 2 + 3, "{records:#?}");
        let ts = |l: &str| l.split(',').next().map(str::to_string);
        let at = |ms: i64| Some(format!("{{\"ts\":{ms}"));
        assert_eq!(ts(records[2]), at(t + 3_500));
        assert_eq!(
            ts(records[3]),
            at(t + 10_000 + 300_000 * i64::from(wakes) + 20_000)
        );
        assert!(records[4].contains(RULE_TURN_RAN), "{}", records[4]);
        let path = seed_ledger(&dir, "whole-plain", &rows);
        crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
        let plain = read(&path);
        assert!(plain.0.is_none() && plain.1.is_none(), "CONTROL: {plain:?}");
    }

    #[test]
    fn a_cut_keeps_the_rows_of_the_states_that_stand_past_a_run_of_acts() {
        use super::super::policy::turn_end::{
            LAND, LAND_ANSWER, RULE_API_RETRY, RULE_COMPACT, RULE_CONTINUE, RULE_LAND,
            RULE_LAND_ANSWER, RULE_LAND_NUDGE, RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE,
        };
        let dir = crate::supervise::ScratchDir::new("apr-seed", "cut-acts");
        let sid = Some("@s-9");
        let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
        let mut rows: Vec<(i64, &str, Outcome, &str, String)> = (0..3000)
            .map(|i| {
                let why = "the safe rules proved the command reads and runs tests".to_string();
                (i, "allow-once@v1", Outcome::Approved, filler, why)
            })
            .collect();
        let t = 1_000_000_i64;
        let switch = model_switch_reason(Some("opus"), Some(t / 1000 + 3 * 86_400), t / 1000);
        rows.push((
            t,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model sonnet",
            switch,
        ));
        let point = |ts: i64| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried::streak(0), false),
            )
        };
        for (k, rule) in [RULE_LAND, RULE_LAND_NUDGE, RULE_LAND_NUDGE]
            .into_iter()
            .enumerate()
        {
            let at = t + 1_000 + 1_000 * i64::try_from(k).unwrap();
            rows.push((at, rule, Outcome::Typed, "(land)", act_reason(None)));
            rows.push(point(at + 500));
        }
        let retry =
            "Claude Code reported an API error (overloaded). Carry on from where you stopped.";
        for n in 1..=1100_i64 {
            let at = t + 10_000 + 300_000 * n;
            rows.push((at, RULE_API_RETRY, Outcome::Typed, retry, act_reason(None)));
            rows.push(point(at + 3_000));
        }
        let read = |path: &Path| {
            (
                open_model_switch(path, sid).map(|o| o.to),
                land_episode(path, sid),
                short_streak(path, sid),
                done_check_owed(path, sid),
                awaited_try(path, sid),
            )
        };
        let path = seed_ledger(&dir, "acts", &rows);
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
        let before = read(&path);
        assert!(before.0.is_some() && before.1.is_some(), "{before:?}");
        bound(&path);
        let kept = std::fs::read_to_string(&path).unwrap();
        assert_eq!(read(&path), before);
        let first = kept.lines().next().expect("rows");
        assert!(first.contains(RULE_MODEL_FALLBACK), "{first}");
        let land_only: Vec<_> = rows
            .iter()
            .filter(|r| r.1 != RULE_MODEL_FALLBACK)
            .cloned()
            .collect();
        let path = seed_ledger(&dir, "acts-land", &land_only);
        let before = read(&path);
        assert!(before.0.is_none() && before.1.is_some(), "{before:?}");
        bound(&path);
        assert_eq!(read(&path), before);
        let mut done = rows.clone();
        let end = t + 10_000 + 300_000 * 1101;
        done.push((
            end,
            RULE_MODEL_RESTORE,
            Outcome::Typed,
            "relaunch --model opus",
            act_reason(None),
        ));
        done.push((
            end + 1,
            RULE_CONTINUE,
            Outcome::Typed,
            "keep going",
            act_reason(None),
        ));
        let path = seed_ledger(&dir, "acts-over", &done);
        let over = read(&path);
        assert!(over.0.is_none() && over.1.is_none(), "{over:?}");
        bound(&path);
        let cut = std::fs::read_to_string(&path).unwrap();
        let acts = cut.lines().filter(|l| record_of(l).is_none()).count();
        assert_eq!(acts, KEEP_ROWS, "{} rows", cut.lines().count());
        assert_eq!(read(&path), over);
        let held = |name: &str, rows: &[(i64, &str, Outcome, &str, String)]| {
            let path = seed_ledger(&dir, name, rows);
            let before = read(&path);
            bound(&path);
            let kept = std::fs::read_to_string(&path).unwrap();
            let len = u64::try_from(kept.len()).unwrap();
            assert!(
                len <= MAX_BYTES,
                "{name}: the cut left {len} bytes, {} rows",
                kept.lines().count()
            );
            assert_eq!(read(&path), before, "{name}");
            before
        };
        let mut land: Vec<(i64, &str, Outcome, &str, String)> = rows
            .iter()
            .filter(|r| r.1 == RULE_LAND || r.1 == RULE_LAND_NUDGE || r.1 == RULE_POINT)
            .take(6)
            .cloned()
            .collect();
        for i in 0..9000_i64 {
            let why = "the safe rules proved the command reads and runs tests".to_string();
            land.push((
                t + 10_000 + i * 60_000,
                "allow-once@v1",
                Outcome::Approved,
                filler,
                why,
            ));
        }
        let landed = held("land-held", &land);
        assert!(landed.1.is_some_and(|e| e.nudges == 2), "{landed:?}");
        let mut compact_answered = vec![
            (t, RULE_LAND, Outcome::Typed, LAND, act_reason(None)),
            point(t + 1),
            (
                t + 2,
                RULE_LAND_ANSWER,
                Outcome::Typed,
                LAND_ANSWER,
                act_reason(None),
            ),
            point(t + 3),
            (
                t + 4,
                RULE_COMPACT,
                Outcome::Typed,
                "/compact",
                act_reason(None),
            ),
        ];
        compact_answered.extend(land.iter().skip(6).cloned());
        let owed = held("land-answer-compacted", &compact_answered)
            .1
            .expect("the answer's compact");
        assert!(owed.compact && !owed.landing, "{owed:?}");
        assert_eq!(owed.owed, Some(RULE_LAND_ANSWER));
        let mut answered: Vec<(i64, &str, Outcome, &str, String)> =
            land.iter().take(6).cloned().collect();
        let mut at = t + 10_000;
        for _ in 0..3000 {
            at += 600_000;
            let act = act_reason(None);
            answered.push((at, RULE_LAND_ANSWER, Outcome::Typed, "(land)", act));
            answered.push(point(at + 300_000));
        }
        answered.push(point(at + 400_000));
        for i in 0..1100_i64 {
            let why = "the safe rules proved the command reads and runs tests".to_string();
            answered.push((
                at + 500_000 + i,
                "allow-once@v1",
                Outcome::Approved,
                filler,
                why,
            ));
        }
        let landed = held("land-answered", &answered);
        assert!(
            landed.1.is_some_and(|e| e.nudges == 2 && !e.landing),
            "{landed:?}"
        );
        let mut switched: Vec<(i64, &str, Outcome, &str, String)> = vec![(
            t,
            RULE_MODEL_FALLBACK,
            Outcome::Typed,
            "relaunch --model sonnet",
            model_switch_reason(Some("opus"), None, t / 1000),
        )];
        for n in 1..=4000_i64 {
            let at = t + 600_000 * n;
            let act = act_reason(None);
            switched.push((at, RULE_CONTINUE, Outcome::Typed, "keep going", act));
            switched.push(point(at + 300_000));
        }
        let path = seed_ledger(&dir, "switch-held-before", &switched);
        let open = open_model_switch(&path, sid).expect("the switch");
        assert!(open.asked && open.back_at_unix.is_none(), "{open:?}");
        assert_eq!(held("switch-held", &switched).0.as_deref(), Some("sonnet"));
        let path = seed_ledger(&dir, "acts-plain", &rows);
        crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
        let plain = read(&path);
        assert!(plain.0.is_none() && plain.1.is_none(), "CONTROL: {plain:?}");
    }

    #[test]
    fn a_land_episodes_since_is_its_instruction_rows_stamp_through_a_cut() {
        use super::super::policy::turn_end::{
            RULE_API_RETRY, RULE_CONTINUE, RULE_LAND, RULE_LAND_NUDGE,
        };
        let dir = crate::supervise::ScratchDir::new("apr-seed", "land-since");
        let sid = Some("@s-9");
        let point = |ts: i64| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried::streak(0), false),
            )
        };
        let typed =
            |ts: i64, rule: &'static str| (ts, rule, Outcome::Typed, "(land)", act_reason(None));
        let t = 1_771_000_000_000_i64;
        let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
        let mut rows: Vec<(i64, &str, Outcome, &str, String)> = (0..3000)
            .map(|i| {
                let why = "the safe rules proved the command reads and runs tests".to_string();
                (
                    t - 200_000 + i,
                    "allow-once@v1",
                    Outcome::Approved,
                    filler,
                    why,
                )
            })
            .collect();
        rows.extend([
            typed(t - 90_000, RULE_LAND),
            point(t - 80_000),
            typed(t - 70_000, RULE_CONTINUE),
            typed(t, RULE_LAND),
            typed(t + 1_000, RULE_LAND),
            point(t + 1_500),
            typed(t + 2_000, RULE_LAND_NUDGE),
            point(t + 2_500),
        ]);
        let path = seed_ledger(&dir, "since", &rows);
        let ep = land_episode(&path, sid).expect("in hand");
        assert_eq!(
            (ep.hold.since_ms, ep.nudges),
            (Some(t + 1_000), 1),
            "{ep:?}"
        );
        let retry =
            "Claude Code reported an API error (overloaded). Carry on from where you stopped.";
        for n in 1..=1100_i64 {
            let at = t + 10_000 + 300_000 * n;
            rows.push((at, RULE_API_RETRY, Outcome::Typed, retry, act_reason(None)));
            rows.push(point(at + 3_000));
        }
        let path = seed_ledger(&dir, "since-cut", &rows);
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
        let before = land_episode(&path, sid).expect("in hand");
        assert_eq!(before.hold.since_ms, Some(t + 1_000), "{before:?}");
        bound(&path);
        let after = land_episode(&path, sid).expect("kept");
        assert_eq!(after, before);
        let mut over = rows.clone();
        let end = t + 10_000 + 300_000 * 1101;
        over.push(typed(end, RULE_CONTINUE));
        let path = seed_ledger(&dir, "since-over", &over);
        assert_eq!(land_episode(&path, sid), None);
        over.push(typed(end + 1_000, RULE_LAND));
        let path = seed_ledger(&dir, "since-anew", &over);
        let anew = land_episode(&path, sid).expect("anew");
        assert_eq!((anew.hold.since_ms, anew.nudges), (Some(end + 1_000), 0));
        let path = seed_ledger(&dir, "since-plain", &rows);
        crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
        assert_eq!(land_episode(&path, sid), None, "CONTROL");
    }

    #[test]
    fn a_landing_ends_the_hold_a_loop_that_follows_seeds() {
        use super::super::policy::turn_end::{HoldStart, RULE_LAND, RULE_LAND_NUDGE};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "land-relanded");
        let sid = Some("@s-9");
        let point = |ts: i64| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried::streak(0), false),
            )
        };
        let typed =
            |ts: i64, rule: &'static str| (ts, rule, Outcome::Typed, "(land)", act_reason(None));
        let t = 1_771_000_000_000_i64;
        let mut rows = vec![
            typed(t, RULE_LAND),
            point(t + 500),
            typed(t + 1_000, RULE_LAND_NUDGE),
            point(t + 1_500),
        ];
        let path = seed_ledger(&dir, "held", &rows);
        let ep = land_episode(&path, sid).expect("in hand");
        let instructed = HoldStart {
            since_ms: Some(t),
            base: 0,
            relanded: false,
        };
        assert_eq!((ep.hold, ep.nudges), (instructed, 1), "CONTROL: {ep:?}");
        rows.push((
            t + 1_600,
            RULE_TASK_DONE,
            Outcome::Waiting,
            "",
            TASK_DONE.to_string(),
        ));
        let path = seed_ledger(&dir, "landed", &rows);
        let ep = land_episode(&path, sid).expect("in hand");
        assert_eq!((ep.hold, ep.nudges), (HoldStart::LANDED, 1), "{ep:?}");
        rows.extend([typed(t + 9_000, RULE_LAND_NUDGE), point(t + 9_500)]);
        let path = seed_ledger(&dir, "anew", &rows);
        let ep = land_episode(&path, sid).expect("in hand");
        let anew = HoldStart {
            since_ms: Some(t + 9_000),
            base: 2,
            relanded: false,
        };
        assert_eq!((ep.hold, ep.nudges), (anew, 2), "{ep:?}");
        rows.extend([typed(t + 12_000, RULE_LAND_NUDGE), point(t + 12_500)]);
        let path = seed_ledger(&dir, "after", &rows);
        let ep = land_episode(&path, sid).expect("in hand");
        assert_eq!((ep.hold, ep.nudges), (anew, 3), "{ep:?}");
    }

    #[test]
    fn a_restored_tab_s_cut_keeps_its_take_up_while_a_state_of_the_earlier_books_stands() {
        use super::super::policy::turn_end::{RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "restored-cut");
        let root: &Path = &dir;
        let (was, tab) = ("s-a0", "s-b1");
        std::fs::create_dir_all(root.join("drive")).expect("the drive");
        let row = |ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str, sid| {
            let row = Row {
                rule_id,
                outcome,
                command,
                reason,
                box_seq: 1,
            };
            format!("{}\n", row.to_json(ts, Some(sid)))
        };
        let t = 1_000_000_i64;
        let switch = model_switch_reason(Some("opus"), Some(t / 1000 + 3 * 86_400), t / 1000);
        let theirs = ledger_under(root, Some(was));
        std::fs::write(
            &theirs,
            row(
                t,
                RULE_MODEL_FALLBACK,
                Outcome::Typed,
                "relaunch --model sonnet",
                &switch,
                was,
            ),
        )
        .expect("the earlier tab's books");
        crate::harness::upgrade_drive::note_earlier_tab_under(root, tab, was);
        let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
        let why = "the safe rules proved the command reads and runs tests";
        let approvals = |from: i64| -> String {
            (0..3000)
                .map(|i| {
                    row(
                        from + i,
                        "allow-once@v1",
                        Outcome::Approved,
                        filler,
                        why,
                        tab,
                    )
                })
                .collect()
        };
        let take_up = row(
            t + 1_000,
            RULE_EARLIER_BOOKS,
            Outcome::Waiting,
            "",
            EARLIER_BOOKS,
            tab,
        );
        let books = |path: &Path| {
            let own = std::fs::read_to_string(path).unwrap_or_default();
            let earlier = vec![(was.to_string(), std::fs::read_to_string(&theirs).unwrap())];
            let up = takes_up_earlier(&own, Some(tab));
            let books = if up {
                books_through(&own, Some(tab), &earlier)
            } else {
                own
            };
            (up, open_model_switch_in(&books, Some(tab)).map(|o| o.to))
        };
        let path = ledger_under(root, Some(tab));
        std::fs::write(&path, format!("{take_up}{}", approvals(t + 2_000))).expect("the tab's");
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);
        let before = books(&path);
        assert!(before.0 && before.1.is_some(), "{before:?}");
        bound(&path);
        assert_eq!(books(&path), before, "the switch stands: the take-up kept");
        let back = row(
            t + 1_500,
            RULE_MODEL_RESTORE,
            Outcome::Typed,
            "relaunch --model opus",
            &act_reason(None),
            tab,
        );
        std::fs::write(&path, format!("{take_up}{back}{}", approvals(t + 2_000))).expect("over");
        assert_eq!(books(&path), (true, None));
        bound(&path);
        assert_eq!(books(&path), (false, None), "the take-up cut");
        std::fs::write(&path, format!("{take_up}{}", approvals(t + 2_000))).expect("plain");
        crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
        assert_eq!(books(&path), (false, None), "CONTROL");
    }

    #[test]
    fn the_books_through_the_earlier_tabs_are_theirs_re_signed_oldest_first() {
        let row = |ts: i64, sid: &str| {
            Row {
                rule_id: "allow-once@v1",
                outcome: Outcome::Approved,
                command: "ls",
                reason: "reads",
                box_seq: 1,
            }
            .to_json(ts, Some(sid))
        };
        let a = format!("{}\n{}\n", row(1, "s-a0"), row(2, "s-ff"));
        let b = format!("{}\n", row(5, "s-b0"));
        let own = format!("{}\n", row(9, "s-c0"));
        let earlier = [("s-b0".to_string(), b), ("s-a0".to_string(), a)];
        let books = books_through(&own, Some("@s-c0"), &earlier);
        assert_eq!(
            books.lines().collect::<Vec<_>>(),
            [row(1, "s-c0"), row(5, "s-c0"), row(9, "s-c0")]
        );
        let line =
            |t: i64, sid: &str| format!("{{\"t\":{t},\"sid\":\"{sid}\",\"kind\":\"waiting\"}}");
        let journal = books_through(
            "",
            Some("s-c0"),
            &[
                ("s-b0".to_string(), line(7, "s-b0")),
                ("s-a0".to_string(), line(3, "s-a0")),
            ],
        );
        assert_eq!(
            journal,
            format!("{}\n{}\n", line(3, "s-c0"), line(7, "s-c0"))
        );
        assert_eq!(books_through(&own, None, &earlier), own, "CONTROL");
    }

    #[test]
    fn a_second_restore_reads_no_state_the_tab_between_ended_before_its_cut() {
        use super::super::policy::turn_end::{RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "restored-chain");
        let root: &Path = &dir;
        let (a, b, c) = ("s-a0", "s-b1", "s-c2");
        std::fs::create_dir_all(root.join("drive")).expect("the drive");
        let row = |ts: i64, rule_id: &str, outcome: Outcome, command: &str, reason: &str, sid| {
            let row = Row {
                rule_id,
                outcome,
                command,
                reason,
                box_seq: 1,
            };
            format!("{}\n", row.to_json(ts, Some(sid)))
        };
        let t = 1_000_000_i64;
        let switch = model_switch_reason(Some("opus"), Some(t / 1000 + 3 * 86_400), t / 1000);
        std::fs::write(
            ledger_under(root, Some(a)),
            row(
                t,
                RULE_MODEL_FALLBACK,
                Outcome::Typed,
                "relaunch --model sonnet",
                &switch,
                a,
            ),
        )
        .expect("A's books");
        crate::harness::upgrade_drive::note_earlier_tab_under(root, b, a);
        let filler = "cd /Users//someone/Developer/project && cargo test -p project-core --lib -- parser::tests 2>&1 | tail -40";
        let why = "the safe rules proved the command reads and runs tests";
        let take_up = row(
            t + 1_000,
            RULE_EARLIER_BOOKS,
            Outcome::Waiting,
            "",
            EARLIER_BOOKS,
            b,
        );
        let back = row(
            t + 1_500,
            RULE_MODEL_RESTORE,
            Outcome::Typed,
            "relaunch --model opus",
            &act_reason(None),
            b,
        );
        let approvals: String = (0..3000)
            .map(|i| {
                row(
                    t + 2_000 + i,
                    "allow-once@v1",
                    Outcome::Approved,
                    filler,
                    why,
                    b,
                )
            })
            .collect();
        let books_of = |tab: &str| -> String {
            let path = ledger_under(root, Some(tab));
            let read = |p: &Path| std::fs::read_to_string(p).ok();
            let own = read(&path).unwrap_or_default();
            let earlier: Vec<(String, String)> =
                crate::harness::upgrade_drive::earlier_ledgers_in(&path, Some(tab), &read)
                    .into_iter()
                    .filter_map(|(t, p)| Some((t, read(&p)?)))
                    .collect();
            if takes_up_earlier(&own, Some(tab)) && earlier.iter().any(|(t, b)| holds_rows_of(b, t))
            {
                books_through(&own, Some(tab), &earlier)
            } else {
                own
            }
        };
        let switched = |tab: &str| open_model_switch_in(&books_of(tab), Some(tab)).map(|o| o.to);
        let bp = ledger_under(root, Some(b));
        std::fs::write(&bp, format!("{take_up}{back}{approvals}")).expect("B's books");
        assert_eq!(switched(b), None, "B switched back");
        bound(&bp);
        assert!(
            !took_up_earlier(&std::fs::read_to_string(&bp).unwrap(), b),
            "B's take-up cut"
        );
        assert_eq!(switched(b), None, "B after its cut");
        crate::harness::upgrade_drive::note_earlier_tab_under(root, c, b);
        assert_eq!(switched(c), None, "C reads no switch B ended");
        let cont = dir.join("continuous.jsonl");
        let a_rows = std::fs::read_to_string(ledger_under(root, Some(a)))
            .unwrap()
            .replace("\"sid\":\"s-a0\"", "\"sid\":\"s-b1\"");
        std::fs::write(&cont, format!("{a_rows}{back}{approvals}")).unwrap();
        bound(&cont);
        assert!(
            open_model_switch(&cont, Some(b)).is_none(),
            "CONTROL: continuous"
        );
        std::fs::write(&bp, format!("{take_up}{approvals}")).expect("B's books, no switch back");
        bound(&bp);
        assert!(
            took_up_earlier(&std::fs::read_to_string(&bp).unwrap(), b),
            "CONTROL: kept"
        );
        assert_eq!(switched(c).as_deref(), Some("sonnet"), "CONTROL: through B");
    }

    #[test]
    fn a_legacy_limits_next_try_never_wraps_its_recorded_count() {
        use super::super::policy::turn_end::RULE_LIMIT_RESUME;
        let row = |rule: &str, outcome: Outcome, reason: &str, ts| {
            Row {
                rule_id: rule,
                outcome,
                command: "keep going",
                reason,
                box_seq: 1,
            }
            .to_json(ts, Some("@s-9"))
        };
        let wall = row(
            RULE_WALL_WAIT,
            Outcome::Waiting,
            "the turn-end policy (wall: reset_unix=-; reset=-; message=You've hit your monthly spend limit.)",
            1_000,
        );
        let marked = |attempts| {
            row(
                RULE_LIMIT_RESUME,
                Outcome::Typed,
                &act_reason(Some(LimitTry {
                    bucket: false,
                    in_place: false,
                    attempts,
                    entitlement: true,
                })),
                2_000,
            )
        };
        let unmarked = row(
            RULE_LIMIT_RESUME,
            Outcome::Typed,
            "the turn-end policy",
            3_000,
        );
        let read = |body: &str| {
            usage_attempt_taken_in(body, Some("@s-9"), &|_, _| false).expect("the recorded try")
        };
        let ordinary = read(&format!("{wall}\n{}\n{unmarked}\n", marked(3)));
        assert_eq!((ordinary.at_ms, ordinary.limit.attempts), (3_000, 4));
        assert!(ordinary.limit.entitlement && !ordinary.limit.bucket);
        let ended = row(RULE_WALL_WAIT_END, Outcome::Waiting, LIMIT_ENDED, 2_500);
        let fresh = read(&format!("{wall}\n{}\n{ended}\n{unmarked}\n", marked(3)));
        assert_eq!(
            fresh.limit, USAGE_RESUME_UNMARKED,
            "an ended wall is a fresh try"
        );
        let body = format!("{wall}\n{}\n{unmarked}\n", marked(3));
        assert!(
            usage_attempt_in(&body, Some("@s-8")).is_none(),
            "another session"
        );
        let at_ceiling = read(&format!("{wall}\n{}\n{unmarked}\n", marked(usize::MAX - 1)));
        assert_eq!(
            at_ceiling.limit.attempts,
            usize::MAX,
            "the last exact increment"
        );
        let beyond = read(&format!("{wall}\n{}\n{unmarked}\n", marked(usize::MAX)));
        assert_eq!(
            (
                beyond.at_ms,
                beyond.limit.attempts,
                beyond.limit.entitlement
            ),
            (3_000, usize::MAX, true),
            "LEGACY_LIMIT_COUNT_MUST_NOT_WRAP"
        );
    }

    #[test]
    fn a_cut_keeps_every_standing_row_when_its_standing_workspace_is_full() {
        use super::super::policy::turn_end::RULE_MODEL_FALLBACK;
        let mut rows = vec![String::new()];
        for sid in ["@s-9", "@s-10"] {
            rows.push(
                Row {
                    rule_id: RULE_MODEL_FALLBACK,
                    outcome: Outcome::Typed,
                    command: "relaunch --model sonnet",
                    reason: &model_switch_reason(Some("opus"), None, 1),
                    box_seq: 1,
                }
                .to_json(1_000, Some(sid)),
            );
        }
        for i in 0..KEEP_ROWS + 8 {
            rows.push(
                Row {
                    rule_id: "allow-once@v1",
                    outcome: Outcome::Approved,
                    command: "echo captured",
                    reason: "a captured ordinary approval",
                    box_seq: 1,
                }
                .to_json(i64::try_from(i).unwrap() + 2_000, Some("@s-9")),
            );
        }
        rows.push(String::new());
        let body = rows.join("\n") + "\n";
        let original: Vec<String> = body.lines().map(str::to_string).collect();
        let path = Path::new("/captured/self.approvals.jsonl");
        let read = |_: &Path| None;
        let cut_of = |body: &str,
                      path: &Path,
                      read: &dyn Fn(&Path) -> Option<String>,
                      work: &mut CutWork| {
            cut_rows_with_work(path, &mut body.lines().map(str::to_string), read, work)
                .expect("held")
        };
        let held = |body: &str| -> Vec<String> {
            let rows: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
            let from = rows.len() - KEEP_ROWS;
            rows.iter()
                .enumerate()
                .filter(|(i, l)| {
                    *i >= from
                        || LandWalk::reads(l)
                        || SwitchWalk::reads(l)
                        || l.contains(RULE_EARLIER_BOOKS)
                })
                .map(|(_, l)| (*l).to_string())
                .collect()
        };
        let cut = cut_of(&body, path, &read, &mut CutWork::for_cut());
        assert!(cut.len() < original.len(), "an ordinary cut still cuts");
        for sid in ["@s-9", "@s-10"] {
            assert_eq!(
                open_model_switch_in(&cut.join("\n"), Some(sid)),
                open_model_switch_in(&body, Some(sid)),
                "the standing switch survives"
            );
        }
        let earlier = (0..64)
            .map(|i| {
                Row {
                    rule_id: "allow-once@v1",
                    outcome: Outcome::Approved,
                    command: "echo captured",
                    reason: "an earlier ordinary approval",
                    box_seq: 1,
                }
                .to_json(i, Some("@s-1"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let take_up = |sid: &str| {
            let mut body = Row {
                rule_id: RULE_EARLIER_BOOKS,
                outcome: Outcome::Waiting,
                command: "",
                reason: "the earlier tabs books are taken up",
                box_seq: 1,
            }
            .to_json(1_000, Some(sid));
            for row in rows.iter().skip(3) {
                body.push('\n');
                body.push_str(row);
            }
            body.push('\n');
            body
        };
        let read_earlier = |path: &Path| {
            if path.extension().is_some_and(|e| e == "earlier-tabs") {
                Some("s-1\n".to_string())
            } else if path.file_name().is_some_and(|n| n == "s-1.jsonl") {
                Some(earlier.clone())
            } else {
                None
            }
        };
        let allowance = || CutWork {
            entries: 65_536,
            visits: 1_048_576,
            bytes: earlier.len() * 3 + 4_096,
        };
        let under_drive = Path::new("/captured/drive/self.jsonl");
        let short = take_up("@s-2");
        assert!(
            cut_of(&short, under_drive, &read_earlier, &mut allowance()).len() < held(&short).len(),
            "ordinary predecessor rows still admit a cut"
        );
        let long = take_up(&format!("@s-{}", "f".repeat(120)));
        assert_eq!(
            cut_of(&long, under_drive, &read_earlier, &mut allowance()),
            held(&long),
            "PREDECESSOR_SID_EXPANSION_MUST_BE_ADMITTED_BEFORE_ASSEMBLY"
        );
        for mut work in [
            CutWork {
                entries: 1,
                visits: usize::MAX,
                bytes: usize::MAX,
            },
            CutWork {
                entries: usize::MAX,
                visits: 1,
                bytes: usize::MAX,
            },
            CutWork {
                entries: usize::MAX,
                visits: usize::MAX,
                bytes: 1,
            },
        ] {
            let kept = cut_of(&body, path, &read, &mut work);
            assert_eq!(
                kept,
                held(&body),
                "STANDING_WORKSPACE_EXCESS_MUST_PRESERVE_EVERY_STANDING_ROW"
            );
            for sid in ["@s-9", "@s-10"] {
                assert_eq!(
                    open_model_switch_in(&kept.join("\n"), Some(sid)),
                    open_model_switch_in(&body, Some(sid)),
                    "the standing switch survives an unadmitted workspace"
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "approvals_fact_tests.rs"]
mod fact_tests;
