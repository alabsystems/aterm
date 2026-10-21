// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The approval ledger: one JSON object per line for every box the
//! supervisor's approval policy decided ([`super::policy::approval::decide`]),
//! kept for every `watch`, `supervise` and hosted run, at
//! `<aterm state>/drive/<sid>.jsonl` ([`default_path`];
//! [`aterm_types::dirs::state_dir`] is the state root).
//!
//! ```text
//! {"ts":<unix ms>,"sid":"<sid>"|null,"rule_id":"<rule>|-",
//!  "decision":"approved|declined|typed|skipped|escalated|deferred",
//!  "cmd_sha256":"<hex of the whole command>","command":"<the command, cut at 4 KiB>",
//!  "reason":"<the rule's reason or the classifier's>","box_seq":<n>}
//! ```
//!
//! `approved` is a press the server wrote; `declined` a box refused with a
//! reason typed for the worker ([`super::policy::approval::Decision::Decline`]);
//! `typed` an act of the turn-end policy the server took; `skipped` a press it
//! did not (the guard matched no row, or the screen moved); `escalated` a box
//! no rule approved; `deferred` a press the server turned away for now (`ERR
//! halted`, `ERR busy …`, `ERR rate`), retried later and never a refusal. The
//! hash identifies a command cut at 4 KiB. The file is written through
//! [`super::journal::Journal`] (append-only, `0600`; a write failure warns
//! once and the loop goes on).

use std::io::Write;
use std::path::{Path, PathBuf};

use super::journal::{Journal, json_str};
use super::policy::sleeps::{Boot, SleepStamp};
use super::policy::turn_end::LimitTry;

/// How much of a command a row keeps.
pub const COMMAND_BYTES: usize = 4096;

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
    /// A wall the loop waits on, remembered for the loop that follows it, or
    /// its end: no act, no press — a record.
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
    pub fn word(self) -> &'static str {
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

    /// The outcome a row's `decision` word names, by [`Self::word`]'s own
    /// words, so a reader of the ledger (`harness::stats`) cannot drift from
    /// its writer; `None` for a word this build does not write.
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

    /// [`Self::to_json`] with the machine's count of its sleep at the row's
    /// stamp (`count`, [`SLEPT_MS`] and [`BOOT`]: an act's row, which a loop
    /// that follows sets its own count against, [`last_act_count_in`]); none
    /// where the count names no boot, which compares with no other process's.
    pub fn to_json_counted(
        &self,
        ts: i64,
        sid: Option<&str>,
        count: Option<&SleepStamp>,
    ) -> String {
        self.to_json_counted_with_boot_text(ts, sid, count, None)
    }

    /// A replay's validated captured spelling, including noncanonical forms
    /// the boot reader admits. The typed id must still name this count's boot.
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

/// An act's row's field: the machine's count of its sleep since boot at the
/// row's stamp, in milliseconds ([`Row::to_json_counted`]).
pub const SLEPT_MS: &str = "slept_ms";

/// An act's row's field: the boot that count is on
/// (`astream_sys::boot_session`), in the kernel's own spelling ([`Boot::text`]:
/// hyphenated, upper-case on a Mac and lower-case on Linux, as the text the
/// builds before the bytes wrote, which compare it as text) and read back by
/// astream's reader of it ([`Boot::of`]), so a row of either case names the
/// same boot, and one that names no boot (the all-zero id) holds no count.
pub const BOOT: &str = "boot";

/// THE LAST ACT ON `sid`'s LEDGER TEXT `body` and the machine's count of its
/// sleep at its stamp: its `ts` (unix ms) and its count, `None` where the row
/// holds none (an older build wrote it, or a machine that keeps no count) —
/// the acts [`typed_at_in`] counts (not an account move's row, nor the
/// accounts probe's `/usage`). A loop that follows sets its own count
/// against it: how long the machine slept since the act it never watched
/// (`policy::sleeps::SleepBook::count_since_act`). `None` with no act.
#[must_use]
pub fn last_act_count_in(body: &str, sid: Option<&str>) -> Option<(i64, Option<SleepStamp>)> {
    let last = *typed_at_in(body, sid).last()?;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let prefix = format!("{{\"ts\":{last},");
    let count = body
        .lines()
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

/// `<state>/drive/<sid>.jsonl` (`self` for a loop given no `@sid`), with the
/// `drive` directory made (`0700`) when missing; `None` when no state root
/// resolves or the directory cannot be made.
pub fn default_path(sid: Option<&str>) -> Option<PathBuf> {
    path_under(&aterm_types::dirs::state_dir()?, sid)
}

/// [`default_path`] under a given state root.
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

/// Where [`path_under`] keeps the ledger of `sid` under `state`, nothing
/// made: for a reader ([`typed_texts`]), which must not create what it only
/// looks for.
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

/// The loop's journal beside a session's ledger: `<state>/drive/<sid>.jsonl`
/// → `<state>/drive/<sid>.journal.jsonl`. The in-GUI host keeps one for every
/// supervised session, so its journal-only lines (`SKIPPED`, `WAITING`,
/// `ESCALATED … attention=<reply>`, `CLEARED`) can be read.
#[must_use]
pub fn journal_beside(ledger: &Path) -> PathBuf {
    let stem = ledger
        .file_stem()
        .map_or_else(|| "self".into(), |s| s.to_string_lossy().into_owned());
    ledger.with_file_name(format!("{stem}.journal.jsonl"))
}

/// The most either per-session file ([`default_path`]'s ledger, the host's
/// [`journal_beside`]) is let grow before it is cut back to its newest
/// [`KEEP_ROWS`] rows, at a loop's start ([`bound`]).
pub const MAX_BYTES: u64 = 1024 * 1024;

/// How many rows a cut keeps, the loop's records among them not counted
/// ([`bound`]): the typing budget reads the last hour's `typed` rows —
/// far fewer.
pub const KEEP_ROWS: usize = 1024;

/// Cut `path` back to its newest [`KEEP_ROWS`] rows once it passes
/// [`MAX_BYTES`] (written to a sibling and renamed over it, as
/// `harness::cli::bound_ledger` does), a run of the loop's records folded
/// to what its readers read of it ([`fold_records`]) and the records kept
/// among those rows taking no row's place: every act writes its point's
/// record too ([`RULE_POINT`]), and counted, they halved the acts a cut keeps
/// — the typing budget's, Settings ▸ Harness's counts (`Partial` twice as
/// soon). Of the older rows, those that move a state that stands for days
/// are kept ([`standing_path`]), so its readers read after the cut what they
/// read before it. The file is streamed ([`held_rows`]); an oversized row or
/// retained workspace keeps it unchanged. A failure to cut is no failure of the loop.
pub fn bound(path: &Path) {
    let _ = crate::harness::cli::bound_ledger_cut(path, MAX_BYTES, &|rows, read| {
        cut_rows(path, rows, read)
    });
}

/// The rows [`bound`]'s cut keeps of `rows`, the ledger at `path` past its
/// bound, the files beside it read by `read`: its newest [`KEEP_ROWS`] rows
/// that are no record and the records among and after them, and of the
/// rows before those, the take-up's row while the earlier tabs' books
/// matter to a state that stands ([`earlier_standing`]) and the rows that
/// move such a state ([`standing_path`]). `None` keeps the file as it is
/// ([`held_rows`]).
fn cut_rows(
    path: &Path,
    rows: &mut dyn Iterator<Item = String>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> Option<Vec<String>> {
    cut_rows_with_work(path, rows, read, &mut CutWork::for_cut())
}

/// The actual cut of the rows [`held_rows`] holds, keeping every one of them
/// if its standing-state workspace cannot be admitted: they are every row a
/// state that stands reads and the newest, so its readers read of them what
/// they read of the whole. The caller's complete-input check is separate:
/// an input byte cap alone does not bound cloned maps or searches.
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

/// The most source-row bytes a cut holds, older standing rows and newest rows
/// together. The streaming reader uses the same bound for one row and all
/// predecessor input. Exceeding it preserves the original file.
pub(crate) const HELD_BYTES: usize = 16 << 20;

/// THE ROWS A CUT CAN KEEP of `rows` (a ledger, oldest first), read one at a
/// time and never held whole: the newest [`KEEP_ROWS`] rows that are no
/// record and the records among and after them (folded as [`fold_records`]
/// folds them, in a ring), and of the rows before those, the ones a walk of
/// [`Standing`] reads and the take-up's ([`RULE_EARLIER_BOOKS`]) — no other
/// older row is kept by any cut ([`older_kept`]). With where the
/// newest begin among them. `None` where all retained rows together pass
/// [`HELD_BYTES`]. Before, the cut read the file whole and kept one past
/// 16 MiB as it was, so the 171 MB journal of one tab of an older build's
/// (ATERM-KI-1016-1) was never cut.
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
        // A row with more than KEEP_ROWS rows that are no record from it on
        // is an older one for good: a fold drops only records, at the back.
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

/// The places among `kept` ([`held_rows`]: the newest from `start` on) of
/// the older rows a cut keeps, ascending.
fn older_kept(
    path: &Path,
    kept: &[String],
    start: usize,
    read: &dyn Fn(&Path) -> Option<String>,
    work: &mut CutWork,
) -> Result<Vec<usize>, ()> {
    // A tab a cold restore reopened: the take-up's row stays while a state
    // the earlier tabs' books began stands ([`RULE_EARLIER_BOOKS`]), and the
    // states walk on from what those books leave.
    let (take_up, from) = earlier_standing(path, kept, start, read, work)?;
    let mut older = standing_path(&kept[..start], from, &kept[start..], work)?;
    older.extend(take_up.filter(|&at| at < start));
    older.sort_unstable();
    older.dedup();
    Ok(older)
}

/// WHAT THE READERS OF THE STATES THAT STAND FOR DAYS HOLD OF EACH SESSION as
/// they walk its rows: the Land episode in hand ([`land_episode_in`]'s
/// [`LandWalk`]) and the model switch open ([`open_model_switch_in`]'s
/// [`SwitchWalk`]), by the rows' `sid`; a session they hold nothing of is
/// left out.
type Standing = std::collections::BTreeMap<Option<String>, (LandWalk, SwitchWalk)>;

/// Cumulative workspace admission for the standing-state calculation.
/// `entries` bounds all potential cloned map entries (therefore retained
/// entries too); `visits` bounds row/map visits, including linear searches;
/// `bytes` also charges dynamic session/model strings before copies or
/// comparisons. These are work allowances, not a wall-clock deadline or an
/// exact bound on every allocation made while parsing the admitted input.
struct CutWork {
    entries: usize,
    visits: usize,
    bytes: usize,
}

impl CutWork {
    fn for_cut() -> Self {
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

    /// Reserve before books_through re-signs predecessor rows: both its
    /// per-tab rows and joined output may hold each expanded row at once.
    /// Six bytes per source byte covers JSON escaping in the replacement SID.
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

/// A row as the walks of [`Standing`] read it ([`standing_row`]): its
/// session, the row, and whether each walk reads it at all
/// ([`LandWalk::reads`], [`SwitchWalk::reads`]).
type WalkRow = (Option<String>, aterm_json::Value, bool, bool);

/// `line` as the walks of [`Standing`] read it; `None` for a row neither
/// reads.
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

/// [`Standing`] past one row ([`standing_row`]); `None` where the row moves
/// neither walk.
fn standing_step(
    state: &Standing,
    (sid, v, land, switch): &WalkRow,
    work: &mut CutWork,
) -> Result<Option<Standing>, ()> {
    // Admit before cloning either one entry or the entire map. The row's
    // strings also cover new model fields and key comparisons in lookups.
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

/// [`Standing`] after `rows` ([`standing_row`]), from `from`.
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

/// Parse each row once for a pass, charging its visit even if it cannot
/// move a standing state.
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

/// THE ROWS OF `rows` THAT A STATE THAT STANDS FOR DAYS READS, as their
/// places among them, the rows `after` them kept whole: a walk of
/// [`Standing`] from `from` (what the rows before them leave) to where
/// `rows` leave it, each row a step of it — or to an earlier state of that
/// walk from which `after` leaves the states where it leaves them (an
/// episode, a switch, that ended among those rows). A row that moves
/// neither walk is left out, and so is every run of rows after which both
/// walks stand where they stood before it. Each walk reads a row by itself
/// alone ([`LandWalk::step`], [`SwitchWalk::step`]), so its readers read the
/// same of what is kept as of the whole: the row of a model switch no loop
/// has undone (its return owed at the bucket's reset, up to a week on, or,
/// where the notice named no reset, never — it stands while the session
/// runs on the fallback), the land instruction of a Land episode no
/// continuation, Off or other mode has ended, and what moved them since.
/// What is kept passes no state twice, so it is no longer than the walks
/// have states, whatever runs between. Every act writes two rows (its own,
/// and its answer's point record: [`RULE_POINT`]), which no fold folds, so a
/// cut to the newest [`KEEP_ROWS`] dropped those rows about 500 acts on: a
/// loop that follows did not switch the model back at the reset, or typed
/// the land instruction anew (ATERM-KI-1016-1, "a loop that follows
/// another"). Kept
/// whole from the state's row on, a Land episode held under Autopilot Land
/// (no act of another mode is typed to end it) kept every row after its
/// instruction, 3.2 MB past 9000 approvals, and a switch whose return is
/// never due 1.9 MB past 4000 continuations (the review of 3af6c3da7d,
/// KRF-2).
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

/// THE TAKE-UP'S ROW ([`RULE_EARLIER_BOOKS`]) of the ledger at `path`,
/// its rows `rows` (folded, the newest from `start` on kept whole), as its
/// place among them where it stays, and what the states that stand for days
/// ([`Standing`]) hold where the ledger's own rows begin. The row stays
/// while the books of the tabs a cold restore reopened its tab from (their
/// rows, then this ledger's: [`books_through`]) leave those states other
/// than this ledger alone does — a state their rows began stands still —
/// or where it is among the newest rows; the states then walk on from what
/// those books leave. With none, a cut past it leaves the earlier tabs'
/// books over, as a continuous ledger's cut drops their rows, and the
/// states walk from nothing. The note and the earlier ledgers read by
/// `read`.
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

/// A row's or a journal line's `"sid":` field for session `sid`, as both
/// write it (`@` dropped).
fn sid_field(sid: &str) -> String {
    format!("\"sid\":{}", json_str(sid.trim_start_matches('@')))
}

/// Whether the session's books in `body` (its ledger) TAKE UP THE EARLIER
/// TABS' ([`RULE_EARLIER_BOOKS`]): `body` holds no row of session `sid` yet,
/// or the take-up's row is among them. A ledger a loop of this tab wrote
/// before any took the earlier books up (a build before this one's) is its
/// own books, as it was.
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

/// Whether `body` (a ledger or a journal) holds a row of session `sid`.
#[must_use]
pub fn holds_rows_of(body: &str, sid: &str) -> bool {
    let field = sid_field(sid);
    body.lines().any(|l| l.contains(&field))
}

/// Whether `body` (a ledger) holds the take-up's row of session `sid`
/// ([`RULE_EARLIER_BOOKS`]).
#[must_use]
pub fn took_up_earlier(body: &str, sid: &str) -> bool {
    let field = sid_field(sid);
    let rule = format!("\"rule_id\":{}", json_str(RULE_EARLIER_BOOKS));
    body.lines()
        .any(|l| l.contains(&field) && l.contains(&rule))
}

/// THE BOOKS OF SESSION `sid` THROUGH THE TABS A COLD RESTORE REOPENED ITS
/// TAB FROM (ATERM-KI-1016-1, follow-7), as one ledger (or one journal): the
/// rows each earlier tab's loops wrote of that tab's own session
/// (`earlier`: the tab and the text of its ledger or journal), re-signed
/// with `sid` (each one's `"sid":` field) and the tabs oldest first (by their
/// first row's time), then `own` as it stands. What a continuous ledger of
/// the conversation would hold: the restore minted the tab another session
/// id, and its first loop's ledger is new.
#[must_use]
pub fn books_through(own: &str, sid: Option<&str>, earlier: &[(String, String)]) -> String {
    let Some(sid) = sid else {
        return own.to_string();
    };
    let to = sid_field(sid);
    // The leading time of a ledger row (`{"ts":`) or a journal line (`{"t":`).
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

/// A RUN OF THE LOOP'S RECORDS, AS THE CUT KEEPS IT ([`bound`]): of the point
/// records ([`RULE_POINT`]) and turn records ([`RULE_TURN_RAN`]) of one
/// session with no other row between them, the run's first point, its last,
/// and a turn read running after that last one — all its readers tell apart:
/// [`short_streak`] the last point and a turn after it, [`done_check_owed`]
/// and [`land_episode`] none, one or more than one point since an act, and
/// [`awaited_try`] any. `row` joins `kept` (the newest rows kept, oldest
/// first) in their place. A loop writes one or two of them at every turn
/// nobody here typed (Claude Code's own wakes, an orchestrator's turns), with
/// no bound: kept whole, a thousand wakes cut the rows of the states that
/// stand for days — a land episode in hand, a model fallback owed its return at the
/// bucket's reset — and a loop that follows typed the land instruction anew
/// where the loop before it types nothing (the verifier of 75b03ac6d, RS9-R1).
// Returns the bytes discarded, including an incoming record folded away.
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
            // A turn read running after the turn read running last: the
            // first says it.
            (false, Some(false)) => return row.len(),
            // A point after a turn read running: the turn says nothing now.
            (true, Some(false)) => {
                discarded += kept.pop_back().map_or(0, |row| row.len());
            }
            _ => {}
        }
        // A point after the run's first and a later one: it is the last now.
        if point && back(kept, 1) == Some(true) && back(kept, 2) == Some(true) {
            discarded += kept.pop_back().map_or(0, |row| row.len());
        }
    }
    kept.push_back(row);
    discarded
}

/// The session of a row that is a record [`fold_records`] folds, and whether
/// it is a point's ([`RULE_POINT`]) or a turn's ([`RULE_TURN_RAN`]).
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

/// The open ledger of one loop.
#[derive(Debug)]
pub struct Ledger {
    journal: Journal,
    sid: Option<String>,
}

impl Ledger {
    /// No ledger: [`Self::write`] does nothing.
    pub fn off() -> Self {
        Self {
            journal: Journal::off(),
            sid: None,
        }
    }

    /// A ledger at `path` for a loop on `sid` that is named and never opened
    /// ([`Journal::named`]: a replay's).
    #[must_use]
    pub fn named(path: Option<&Path>, sid: Option<&str>) -> Self {
        Self {
            journal: Journal::named(path, sid),
            sid: sid.map(str::to_string),
        }
    }

    /// Open `path` for a loop on `sid` (see [`Journal::open`]).
    pub fn open(path: Option<&Path>, sid: Option<&str>, warn: &mut dyn Write) -> Self {
        Self {
            journal: Journal::open(path, sid, warn),
            sid: sid.map(str::to_string),
        }
    }

    /// Append one row, stamped `t_ms` (Unix milliseconds, from the caller's
    /// clock: the supervising loop's stamp clock, `Fx::stamp_ms` behind its
    /// crate-private clock door `supervise::fx`; [`super::journal::unix_ms`]
    /// elsewhere). Returns the row, for a ledger it was given (`None` for
    /// [`Self::off`]): what a harness tape holds of the ledger.
    pub fn write(&mut self, t_ms: i64, row: &Row<'_>, warn: &mut dyn Write) -> Option<String> {
        self.write_counted(t_ms, row, None, warn)
    }

    /// [`Self::write`] with the machine's count of its sleep at the stamp
    /// ([`Row::to_json_counted`]): an act's row.
    pub fn write_counted(
        &mut self,
        t_ms: i64,
        row: &Row<'_>,
        count: Option<&SleepStamp>,
        warn: &mut dyn Write,
    ) -> Option<String> {
        self.write_counted_with_boot_text(t_ms, row, count, None, warn)
    }

    /// The counted writer with a replay's validated captured boot text.
    /// Ordinary live and recorded rows keep [`Boot::kernel`]'s host spelling.
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

/// One text the loop typed, and when ([`typed_texts`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    /// When its row was written (`ts`, unix ms): as the Enter that submitted
    /// it landed.
    pub at_ms: i64,
    /// What was typed (`command`).
    pub text: String,
    /// The rule it was typed under (`rule_id`; empty where the row names
    /// none): what tells a wall's retry from a continuation of the same
    /// words (`crate::harness::upgrade::FinishedScan`).
    pub rule: String,
}

/// THE TEXTS THE LOOP TYPED into `sid`'s session, each with WHEN: the
/// command of every `typed` row of its in the ledger at `path`, as the loop
/// wrote them — the turn-end policy's continuations (`continue_text` and the
/// standing rules riding in it), its answers (`answer_text`), the agent's
/// own suggestions it accepted, the wall's retries and its commands. The
/// harness's own turns, which make no conversation's task
/// (`crate::harness::upgrade::TaskScan`: a
/// `keep going` or an `answer_text` is the supervisor's, however the vendor
/// records it). The ledger is the SESSION's (the tab's), kept across every
/// conversation it holds: the time is what places a text in one. A missing
/// or unreadable file, and a row that does not parse or carries no time,
/// know none.
#[must_use]
pub fn typed_texts(path: &Path, sid: Option<&str>) -> Vec<Typed> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let mut out = Vec::new();
    for line in body.lines().filter(|l| l.contains("\"typed\"")) {
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

/// When each `typed` row of `sid`'s in the ledger text `body` was written
/// (`ts`, unix ms), oldest first: the turn-end policy's acts of earlier
/// loops on the session, which its typing budget counts
/// ([`super::policy::turn_end::TurnEndState::seed_acts`]). An empty ledger,
/// and a row this build does not write, count nothing.
/// The fields are matched as the row writes them — a `"` inside a value is
/// escaped, so a command cannot forge one.
///
/// NOT AN ACCOUNT MOVE MADE ([`account_switch_reason`]'s row, under the
/// move's rule and command): it is `typed` so that a later loop reads the
/// move back, and it is a relaunch — the loop that made it counted no act
/// for it, and a loop that started after it counted one toward the owner's
/// `continue_per_hour`.
///
/// NOR THE ACCOUNTS PROBE'S `/usage` ([`PROBE_TYPED_REASON`]'s row, under
/// [`super::policy::turn_end::RULE_USAGE_PROBE`]): it is `typed` so that the
/// command row a transcript may carry reads as the harness's
/// ([`typed_texts`]) and so that a later loop knows a panel may be owed
/// ([`probe_owed`]) — and it is no act of the turn-end policy's. The loop
/// that typed it booked nothing for it, and a successor that counted it
/// would spend the owner's budget on a read of the account's windows.
///
/// NOR `/login`'s SUCCESS ENTER ([`super::policy::login::RULE_LOGIN_DONE`]'s
/// row): it is `typed` because a key went into the session, and the login
/// it answers is saved already — the loop that pressed it counted no act of
/// the hour's ([`super::policy::turn_end::TurnEndState::acted`]), and so a
/// successor counts none.
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
    let mut out: Vec<i64> = body
        .lines()
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

/// THE `/login` A LOOP BEFORE THIS ONE TYPED, ITS ANSWER STILL TO COME: when
/// (`ts`, unix ms) `sid`'s last act in the ledger text `body` was
/// [`super::policy::turn_end::RULE_LOGIN`]'s `/login`, with no turn read
/// running after it ([`RULE_TURN_RAN`]) — no other act since, `/login`'s
/// success Enter included, which answers it. A loop that follows takes it up
/// as its own where the screen shows that `/login` at a lost login
/// ([`super::policy::turn_end::TurnEndState::seed_login`]). `None`: another
/// act last, a turn run since, no such row.
#[must_use]
pub fn login_typed_last_in(body: &str, sid: Option<&str>) -> Option<i64> {
    use super::policy::turn_end::RULE_LOGIN;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last = None;
    for line in body.lines() {
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

/// The reason a [`super::policy::turn_end::RULE_LEFT`] row carries: the
/// turn its loop left to a person ([`open_left`]).
pub const LEFT_REASON: &str = "a person's own exchange: nothing is typed until the next turn ends";

/// The reason the [`super::policy::turn_end::RULE_LEFT`] row a person's own
/// exchange at a wall writes carries
/// (`super::policy::turn_end::TurnEndState::exchange_at_wall`, the review's
/// RR4-TE-2): the wall's retry is typed, and its short answer is theirs.
pub const LEFT_AT_WALL_REASON: &str =
    "a person's own exchange at a wall: its retry's short answer is theirs";

/// What the key of that row starts with, before the turn's key: read at a
/// wall alone, never as the exchange left to the person once its turn
/// ended (`open_left`).
pub const LEFT_AT_WALL: &str = "wall:";

/// The key of the row a person's own exchange at a wall writes
/// ([`LEFT_AT_WALL`]): the turn's key, then `@` and the exchange's work so
/// far in whole seconds — the work before the wall, which a new loop whose
/// first point is that wall adds the retry's answer to (main's 7fffc8156: a
/// wall splits a job, it never makes it an exchange; the merge review's F3).
#[must_use]
pub fn left_at_wall_key(turn: &str, work: std::time::Duration) -> String {
    format!("{LEFT_AT_WALL}{turn}@{}", work.as_secs())
}

/// The turn's key and the exchange's work a [`left_at_wall_key`] names, or
/// `None` for a key that is no wall's. A row written before the work was
/// carried (`wall:<turn>`) names the turn and no work.
#[must_use]
pub fn parse_left_at_wall(key: &str) -> Option<(&str, Option<std::time::Duration>)> {
    let rest = key.strip_prefix(LEFT_AT_WALL)?;
    let worked = rest.rsplit_once('@').and_then(|(turn, secs)| {
        let secs = secs.parse().ok()?;
        Some((turn, std::time::Duration::from_secs(secs)))
    });
    Some(worked.map_or((rest, None), |(turn, work)| (turn, Some(work))))
}

/// What the key of an open exchange at a wall ([`LEFT_AT_WALL`]) becomes
/// once the wall's retry is typed under it ([`open_left_in`],
/// [`left_retried_key`]): read as the person's exchange where the
/// conversation's last turn is that retry's, ended within the exchange's
/// bound (`crate::harness::upgrade::LastTurn::names`).
pub const LEFT_RETRIED: &str = "retried:";

/// The key of a person's exchange at a wall whose retry was typed: the
/// retry's words, hashed ([`words_hash`]), then the wall's own key
/// ([`left_at_wall_key`]), which carries the exchange's work before it.
#[must_use]
pub fn left_retried_key(wall: &str, retry: &str) -> String {
    format!("{LEFT_RETRIED}{:016x}:{wall}", words_hash(retry))
}

/// The retry's words' hash and the wall's key a [`left_retried_key`] names,
/// or `None` for a key that is no retried exchange's.
#[must_use]
pub fn parse_left_retried(key: &str) -> Option<(u64, &str)> {
    let (hash, wall) = key.strip_prefix(LEFT_RETRIED)?.split_once(':')?;
    Some((u64::from_str_radix(hash, 16).ok()?, wall))
}

/// What the key of A PERSON'S OWN EXCHANGE AN OLDER BUILD JOURNALED AS
/// LEFT starts with (ATERM-KI-1016-1, done-5): a build through 0.105.0
/// notes `LEFT seq=<n> rule=left-to-person@v1 …` and writes no row naming
/// the turn it left (no [`open_left`] key). The note's time stands for the
/// turn: the conversation's last turn then is its last turn still where no
/// turn opened since (`crate::harness::upgrade::LastTurn::names`: its
/// prompt recorded at or before the note). Never written to a ledger: a
/// loop that follows takes it up from the journal (`run::journaled_left`).
pub const LEFT_BEFORE: &str = "before:";

/// The key [`LEFT_BEFORE`] gives an exchange an older build noted left at
/// `at_ms` (unix ms).
#[must_use]
pub fn left_before_key(at_ms: i64) -> String {
    format!("{LEFT_BEFORE}{at_ms}")
}

/// The time (unix ms) a [`left_before_key`] names, or `None` for a key
/// that is no such note's.
#[must_use]
pub fn parse_left_before(key: &str) -> Option<u64> {
    key.strip_prefix(LEFT_BEFORE)?.parse().ok()
}

/// A text's words, hashed however it is spaced or wrapped: what a typed
/// retry ([`left_retried_key`]) and the transcript's row of it are matched
/// by.
#[must_use]
pub fn words_hash(text: &str) -> u64 {
    aterm_hash::fnv1a_64(
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_bytes(),
    )
}

/// How many of a message's first words [`words_lead`] hashes.
pub const LEAD_WORDS: usize = 8;

/// A text's FIRST WORDS ([`LEAD_WORDS`]), hashed however they are spaced or
/// wrapped: what a message drawn on a screen that may cut or fold it
/// (Gemini CLI's band, agy's and Muse Code's message rows) is matched to an
/// act's words by ([`super::policy::turn_end::TurnEndReading::opened_by`]).
/// A text of fewer words hashes them all, so no shorter message matches a
/// longer act's lead.
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

/// THE PERSON'S OWN EXCHANGE AN EARLIER LOOP LEFT TO THEM (the review's
/// RR3-TE-2): the key (`command`) of `sid`'s last `skipped` row under
/// [`super::policy::turn_end::RULE_LEFT`] in the ledger text `body` — the
/// conversation's last turn as that loop read it
/// (`crate::harness::upgrade::LastTurn::key`) — where no `typed` row of
/// `sid`'s came after it, save the accounts probe's `/usage`, which asks the
/// model nothing. `None`: no such row, one a typed row closed, a missing or
/// unreadable file.
///
/// A WALL'S RETRY DELIVERS THE PERSON'S EXCHANGE AGAIN (ATERM-KI-1016-1): a
/// row of an exchange at a wall ([`LEFT_AT_WALL`]) that the wall's retry,
/// its usage resume or its API's return was typed after stays open as
/// [`left_retried_key`] — the retry's short answer is the person's, as in
/// the loop that typed it (`TurnEndState::exchange_at_wall`). Closed by it,
/// a loop that stopped between typing the retry and reading its answer left
/// no open row, and a relaunch typed its carry-on, a new loop the done check
/// or `keep going`, into the person's exchange. Any other act closes it (a
/// `/compact`, which makes the task the policy's, among them).
#[must_use]
pub fn open_left_in(body: &str, sid: Option<&str>) -> Option<String> {
    use super::policy::turn_end::RULE_LEFT;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut open: Option<String> = None;
    for line in body.lines() {
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

/// The key an open exchange left to a person (`open`) stays open under once
/// an act of `rule` typed `command`, or `None` where the act closes it — the
/// ledger's fold ([`open_left_in`]) and a loop's memory of its own rows
/// (`Session::ledger_row`) alike. The accounts probe's `/usage` asks the
/// model nothing: the key stands. A wall's retry, its usage resume, its
/// API's return or its login's return typed under an exchange at a wall
/// ([`LEFT_AT_WALL`]) delivers the person's message again: it stays open as the retry's
/// ([`left_retried_key`]). Any other act — a continuation, a command (a
/// `/compact` makes the task the policy's), a retry under a row that is no
/// wall's — closes it.
#[must_use]
pub fn left_after_typed(open: &str, rule: &str, command: &str) -> Option<String> {
    use super::policy::turn_end::{
        RULE_API_BACK, RULE_API_CUTOFF, RULE_API_RETRY, RULE_LIMIT_RESUME, RULE_LOGIN_BACK,
        RULE_USAGE_PROBE,
    };
    match rule {
        RULE_USAGE_PROBE => Some(open.to_string()),
        RULE_API_RETRY | RULE_API_BACK | RULE_API_CUTOFF | RULE_LIMIT_RESUME | RULE_LOGIN_BACK
            if open.starts_with(LEFT_AT_WALL) =>
        {
            Some(left_retried_key(open, command))
        }
        _ => None,
    }
}

/// [`open_left_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn open_left(path: &Path, sid: Option<&str>) -> Option<String> {
    open_left_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// [`typed_at_in`] over the ledger at `path`: a missing or unreadable file
/// counts nothing.
#[must_use]
pub fn typed_at(path: &Path, sid: Option<&str>) -> Vec<i64> {
    std::fs::read_to_string(path).map_or_else(|_| Vec::new(), |body| typed_at_in(&body, sid))
}

/// The reason the accounts probe's `typed` row carries: `/usage` was
/// written into the session's composer by the loop
/// (`docs/SPEC-claude-accounts-probe-2026-10-01.md`, P17).
pub const PROBE_TYPED_REASON: &str = "the accounts probe";

/// The reason the probe's `waiting` row carries: the loop saw the session
/// with no panel of its own left on it — the panel closed, or it never
/// opened and the composer is back without the command in it.
pub const PROBE_CLOSED_REASON: &str = "closed";

/// The probe's rule as a row spells it (`,"rule_id":"usage-probe@v1",`): the
/// field matched whole, as [`typed_at`] matches its own — a `"` inside a
/// value is escaped, so no command or reason can forge it.
fn probe_rule_field() -> String {
    format!(
        ",\"rule_id\":{},",
        json_str(super::policy::turn_end::RULE_USAGE_PROBE)
    )
}

/// WHETHER A `/usage` PANEL THE HARNESS OPENED MAY STILL STAND on `sid`'s
/// session: the last row of the accounts probe's in the ledger `body` (the
/// file as the loop read it, once, at its start) is
/// its `typed` one ([`PROBE_TYPED_REASON`]) — an earlier loop wrote `/usage`
/// and no loop has written since that it saw the panel gone
/// ([`PROBE_CLOSED_REASON`]). A loop that starts on such a ledger takes the
/// panel up and closes it; one that starts on any other closes nothing it
/// finds (the spec's P18: a loop closes only a panel the harness opened).
///
/// `false` for an empty ledger, for one with no row of
/// the probe's — every ledger of a machine with no roster, where the probe
/// never runs — and for another session's rows.
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

/// The vendor's yes as the session ledger's `approved` row named it before
/// Claude Code 2.1.304 named a kind in it (2.1.287–2.1.303; a reset step an
/// older aterm opened): read by [`reset_owed`] beside every kind's
/// `Yes, use my <kind>` (`aterm_phase::ResetKind::yes`), never written.
pub const RESET_YES_2_1_287: &str = "Yes, use my reset";

/// A limit reset's step the session ledger says is still open
/// ([`reset_owed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetOpen {
    /// The confirmation's Enter was written since the row was taken (an
    /// `approved` row on the vendor's yes: any kind's `Yes, use my <kind>`,
    /// or an older build's [`RESET_YES_2_1_287`]).
    pub entered: bool,
    /// When the grant row's `approved` row was written (`ts`, unix ms); `0`
    /// where the row carries no time.
    pub taken_ms: i64,
}

/// WHETHER A LIMIT RESET'S STEP THE HARNESS OPENED IS STILL OWED on `sid`'s
/// session (`docs/SPEC-claude-accounts-resets-2026-10-02.md` §3.1): among
/// the rows of [`super::policy::approval::RULE_LIMIT_RESET`] in the ledger
/// `body` (the file as read), an `approved` one — the grant row taken — has no `waiting` row
/// after it (`ended: <outcome>`, written when the loop told its host how the
/// step ended). `Some` then: when the row was taken, and whether the
/// confirmation's Enter was written since ([`ResetOpen`]). A loop that starts
/// on such a ledger takes the step up — and never enters: it closes what it
/// finds — unless the row is older than a step can be, which only ends it.
///
/// `None` for an empty ledger, for one with no row of the
/// rule — every ledger of a session that never took a grant row, and so
/// every one written with `[harness] limit_reset` off — for one whose last
/// such row is the `waiting` one, and for another session's rows. A row of
/// the rule that took nothing (`skipped`, `deferred`: a press that did not
/// land) opens no step.
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
    // Every yes a build has written, each a whole command: the bundle's
    // other dialogs open their yes `Yes, use my …` too.
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

/// [`usage_attempt`]'s try, and whether (and when) its limit was MET AGAIN
/// after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageAttempt {
    /// When the try was typed (unix ms).
    pub at_ms: i64,
    /// The limit's try its row names — ON THE LADDER THE WALL THAT MET IT
    /// AGAIN puts its track on, where one did ([`Self::met_ms`]): the
    /// entitlement ladder for a spend limit or a model's bucket no reset
    /// ends and none goes on by itself, the short one otherwise
    /// ([`LimitTry::entitlement`]), as the loop that read that wall put it
    /// (`TurnEndState::track_wall`). The mark says the ladder the try was
    /// typed on: a five-hour window's try met by the account's monthly spend
    /// limit was marked the short ladder's, and a successor between 30
    /// minutes and two hours after it took up nothing and typed 30 minutes
    /// from its start, up to an hour before the first loop, whose track had
    /// moved onto the entitlement ladder at the spend limit's read (the
    /// verifier of 5b72b1613, M7-3).
    pub limit: LimitTry,
    /// THE TRY WAS MET AGAIN, at the time of the first such row (unix ms;
    /// `None`: not met again): a [`RULE_WALL_WAIT`] row of the try's limit
    /// (a model's bucket for a try at one, else a usage window or a spend
    /// limit: [`LimitTry::bucket`]) follows the try's row — the wall a loop
    /// read after the try ([`super::wall_memory`] writes it again after every
    /// act), whether it stands open still or a later row closed it: a live
    /// turn's busy read ends the remembered wall. The try's own turn is over,
    /// and work seen since is a later turn's
    /// ([`super::policy::turn_end::TurnEndState::seed_met_attempt`]). Read from
    /// the open wall alone, a successor that attached during a person's turn
    /// after the try was met again (the first loop's busy read had closed
    /// that wall) took the person's work for the try's own, started the
    /// episode again, and typed `usage-resume@v1` into the standing spend
    /// limit thirty minutes from it, where the first loop kept the try's
    /// two-hour rung (the verifier of e9e6f65e9, B-1). NOT a wall row the
    /// try's TAKING follows ([`WALL_TAKEN`]: read inside its take window,
    /// before Claude Code drew the line, that wall was the screen the try was
    /// typed at): its own turn is still to come, and its work starts the
    /// episode again, as for the loop that typed it (the verifier of
    /// e09018898, B5-2: met, a successor held the try's two-hour rung where
    /// the first loop typed 30 minutes from the wall that turn ended on).
    /// The track's appearance is that row's, as the first loop moved it
    /// where it read the try's answer, not the try's
    /// ([`super::policy::turn_end::TurnEndState::seed_usage_attempt`]):
    /// counted from the try, a successor typed into the standing limit as
    /// much sooner as the try's own short turn ran before it met the limit
    /// again (the verifier of 5b72b1613, R5-2: 100 to 260 s).
    pub met_ms: Option<i64>,
    /// THE TRY'S FIRST READ (unix ms): the first read of the session's after
    /// the try (a wall's row or its end, or an act typed: no record of the
    /// loop's, [`awaited_try`]), where it is the wall of the try's limit — the screen the
    /// loop that typed it read first after it, no live turn read before it
    /// (which ends the wall, [`super::wall_memory`]). That read opened that
    /// loop's take window, and the try met again there was judged met at
    /// the window's end ([`Self::met_ms`] is this row's;
    /// [`super::policy::turn_end::TurnEndState::seed_awaited`]). `None`
    /// otherwise.
    pub read_ms: Option<i64>,
}

/// THE LIMIT'S TRY THE LAST ACT ON THE SESSION IS, as a loop that follows
/// awaits it ([`awaited_try`]; `TurnEndState::seed_awaited`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AwaitedTry {
    /// When it was typed (unix ms).
    pub at_ms: i64,
    /// ITS FIRST READ (unix ms): the first row of a read after it (a wall's
    /// row, or its end), where it is a wall's — any limit's: the screen the loop that typed it
    /// read first after it, where that loop's take window began. `None`
    /// otherwise.
    pub read_ms: Option<i64>,
    /// NO READ OF IT YET: a read of the wall it was typed at writes the wall
    /// again, a live turn ends it ([`super::wall_memory`]); a record of the
    /// loop's after it (Off stood, the task's end) is no read — so that
    /// window has not begun.
    pub unread: bool,
    /// AN END OF THE WALL CAME SINCE ([`RULE_WALL_WAIT_END`]: a take, the
    /// screen that left it, the track's end): its first read opens no
    /// window of this loop's.
    pub ended: bool,
    /// A WALL WAS READ SINCE, no take after it ([`WALL_TAKEN`]): the wall
    /// answered it (`Awaited::met`), whichever limit's.
    pub met: bool,
}

/// THE LIMIT'S TRY `sid`'s LEDGER TEXT `body` ENDS ON: its last `typed` row (as
/// [`typed_at`] counts acts) is [`super::policy::turn_end::RULE_LIMIT_RESUME`]
/// or an act marked the limit's try ([`act_reason`]; not one typed in its
/// place, which is the done check's to await), and the rows after it
/// ([`AwaitedTry`]). Read apart from [`usage_attempt`], which takes up the
/// limit's episode: an act is awaited whatever its limit's track does. A
/// five-hour window's try met inside its take window by a model's bucket
/// ends the window's track there (the bucket's wall in its place,
/// [`LIMIT_ENDED`]), and taken up from the episode alone, a successor
/// attached in that window awaited nothing and counted the bucket's first
/// rung from its own read, 29 s before the loop that typed the try counted
/// it from its window's end (the verifier of 341e1fa81's refix).
#[must_use]
pub fn awaited_try_in(body: &str, sid: Option<&str>) -> Option<AwaitedTry> {
    use super::policy::turn_end::{RULE_LIMIT_RESUME, RULE_USAGE_PROBE};
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<AwaitedTry> = None;
    for v in body
        .lines()
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
        // Its point judged ([`RULE_POINT`]): no loop awaits it.
        if rule == RULE_POINT {
            last = None;
            continue;
        }
        // Only a READ of the act writes a row of the wall's: the wall again,
        // or its end ([`super::wall_memory`]). A record of the loop's — Off
        // stood, the task's end, a Land episode's, the fallback asked — is no
        // read: read as one, Off standing between a try and its first read
        // left the try's window unplaced, and a successor attached inside the
        // first loop's window judged the try met at the screen it was typed
        // at (the verifier of 97daeeace, RS8-R1).
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

/// [`awaited_try_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn awaited_try(path: &Path, sid: Option<&str>) -> Option<AwaitedTry> {
    awaited_try_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// WHEN AND AT WHICH LIMIT THE USAGE ACT A LOOP RESTARTED AT A WALL TAKES UP
/// WAS TYPED ([`UsageAttempt`]: `ts`, unix ms, the limit's try its row names —
/// [`super::policy::turn_end::TurnEndState::seed_usage_attempt`] — and whether
/// it was met again since, [`UsageAttempt::met_ms`]): the last
/// `typed` row in `sid`'s ledger text `body` that [`act_reason`] marks as the
/// limit's try — [`super::policy::turn_end::RULE_LIMIT_RESUME`], or an act
/// typed IN ITS PLACE at a limit's end (the done check, the reversible
/// answer, or under Autopilot Land the land act that stands in for either),
/// whatever its rule, which the loop that typed it counted as the wall's
/// attempt as it counts the continuation — or of
/// [`super::policy::turn_end::RULE_LIMIT_RESUME`] unmarked (the rows of every
/// build through 0.106.0, and a continuation no limit's track stood behind):
/// a try at the limit an older build's open wall rows say, its tries since
/// the wall opened its attempts, else a usage window's first try
/// ([`USAGE_RESUME_UNMARKED`]) — the
/// wall's attempts the act left read from its mark, so the successor's next
/// try is on the rung the loop that typed it is on. Unless an
/// account move MADE follows it ([`AccountMoves::made_ms`]): that act was
/// typed into the account the session has LEFT, and seeded, the new
/// account's first limit was worked as the retry of a wall it never met, its
/// continuation backed off from an act that was another account's. Nor
/// where the limit's track ended after it by no act of the loop's — the
/// worker's work, another wall in its place ([`LIMIT_ENDED`]'s row), a
/// conversation with no task ([`WALL_LEFT_NO_TASK`]'s), the try aged out for
/// a loop that followed it ([`RULE_TRY_OVER`]): the episode that try was of
/// is over, as it is for the loop that typed it, and a wall met since is a
/// new appearance.
#[must_use]
pub fn usage_attempt_in(body: &str, sid: Option<&str>) -> Option<UsageAttempt> {
    usage_attempt_taken_in(body, sid, &|_, _| false)
}

/// [`usage_attempt_in`], where `taken(try_ms, end_ms)` says that the loop
/// that typed a try saw it TAKEN — the try's first read its take window's
/// screen, the try's own turn after it — by the end of the wall at `end_ms`
/// (unix ms), as a build through 0.106.0 journaled it and did not write it
/// ([`WALL_TAKEN`] is newer: that build's row said only
/// `the wall left the screen or the worker worked`). Such a try's first
/// wall row ([`WallRow::older`]) was not its answer: it is not met again,
/// and its own turn is still to come (ATERM-KI-1016-1, follow-5). Read as
/// met, a successor typed into a spend limit 30 minutes before the loop that
/// typed the try, which waited for the turn the try's take began.
#[must_use]
pub fn usage_attempt_taken_in(
    body: &str,
    sid: Option<&str>,
    taken: &dyn Fn(i64, i64) -> bool,
) -> Option<UsageAttempt> {
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<UsageAttempt> = None;
    // THE LIMIT AN OLDER BUILD'S STANDING WALL ROWS SAY ([`WallRow::older`]:
    // its kind, its ladder) and the tries typed at it since it opened: what
    // an unmarked try typed there was a try at ([`USAGE_RESUME_UNMARKED`]).
    let mut standing: Option<LimitTry> = None;
    // A row of the session's came since the try ([`UsageAttempt::read_ms`]).
    let mut seen = false;
    // The try MET AGAIN by an older build's row with no end of the wall
    // before it ([`WallRow::older`]): the screen the try was typed at where
    // the end that follows it is the try's take (`taken`).
    let mut untaken = false;
    // An end of the wall came since the try.
    let mut ended = false;
    // That try was TAKEN (`taken`): the wall read next ends its own turn,
    // whose work ended the limit's track for the loop that typed it — a new
    // appearance ([`LIMIT_ENDED`]'s, which that build did not write).
    let mut took_turn = false;
    for v in body
        .lines()
        .filter(|l| {
            l.contains(",\"decision\":\"typed\",") || l.contains(",\"decision\":\"waiting\",")
        })
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
    {
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        // The try's first READ since: its first read where it is its wall.
        // An act typed is read after; a record of the loop's is no read
        // ([`awaited_try`]), nor is the accounts probe's `/usage`.
        let read = match text("decision") {
            Some("typed") => text("rule_id") != Some(super::policy::turn_end::RULE_USAGE_PROBE),
            _ => matches!(
                text("rule_id"),
                Some(RULE_WALL_WAIT | RULE_WALL_WAIT_END | RULE_TRY_ANSWERED)
            ),
        };
        let first = read && last.is_some() && !std::mem::replace(&mut seen, true);
        if text("decision") == Some("waiting") {
            let took = text("rule_id") == Some(RULE_WALL_WAIT_END)
                && std::mem::take(&mut untaken)
                && last.is_some_and(|a| {
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
                    if matches!(text("reason"), Some(LIMIT_ENDED | WALL_LEFT_NO_TASK)) =>
                {
                    last = None;
                }
                Some(RULE_TRY_OVER) => last = None,
                // The try's answer a loop dated: met at the first read it
                // names, that read the try's first.
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
                        // The ladder that wall puts the track on, read after
                        // read, as the loop that read it moved it.
                        a.limit.entitlement = kind.is_some_and(|k| {
                            super::policy::turn_end::entitlement_ladder(row.reset_unix.is_some(), k)
                        }) && !super::limit::resumes_by_itself(row.message);
                    }
                }
                _ => {}
            }
            continue;
        }
        // An unmarked try (a build before the marks wrote it) at the wall
        // the ledger's rows stand at was a try at THAT limit, on its ladder,
        // the wall's tries since it opened its attempts: read as a usage
        // window's first, a spend limit's try was tried again 10 minutes on,
        // where the loop that typed it waited 2 hours (ATERM-KI-1016-1,
        // follow-5).
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
        if last.is_none_or(|a| ts >= a.at_ms) {
            last = Some(UsageAttempt {
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

/// [`usage_attempt_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn usage_attempt(path: &Path, sid: Option<&str>) -> Option<UsageAttempt> {
    usage_attempt_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// THE DONE CHECK A LOOP ASKED that no act has followed ([`done_check_owed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckOwed {
    /// Which check: [`super::policy::turn_end::RULE_DONE_CHECK`], or its kin
    /// an older build's challenge ([`super::policy::turn_end::RULE_OPEN_ITEMS`],
    /// [`super::policy::turn_end::RULE_UNCOMMITTED`]), whose answer is read
    /// as the check's yield (the merge review's MG7-A3); nothing it says ends
    /// the task (FOREVER CRUISE).
    pub rule: &'static str,
    /// A WALL ANSWERED IT, its row after it: the wall's retry delivers it
    /// again (`TurnEndState::cut_check`). Else its answer is the check's.
    pub cut: bool,
    /// The check itself is the last act — typed then (unix ms), in a limit's
    /// place or not ([`act_reason`]'s mark) — rather than a wall's retry
    /// that delivers it again.
    pub check: Option<(i64, bool)>,
    /// The check's first read (unix ms): the wall's row that cut it
    /// ([`Self::cut`]), the screen the loop that typed it read first after
    /// it, as [`UsageAttempt::read_ms`] is a try's.
    pub read_ms: Option<i64>,
    /// NO READ OF IT YET, where it was typed in a limit's place — the limit's
    /// try in kind, as [`AwaitedTry::unread`] is the continuation's: a read
    /// of the wall it was typed at writes the wall again, a live turn ends
    /// it, a point judged writes its record ([`RULE_POINT`]), and a record of
    /// the loop's (Off stood, the task's end) is no read. The window of the
    /// loop that typed it has not begun. A check typed at an ordinary turn
    /// end has no wall to write, and its rows never say.
    pub unread: bool,
    /// ITS ANSWER WAS JUDGED: an older build ended the task after it
    /// ([`RULE_TASK_DONE`]; no check's answer ends one now). Nothing ends
    /// here: a turn read busy since is a later one.
    pub answered: bool,
}

/// THE DONE CHECK `sid`'s LEDGER TEXT `body` SAYS IS OWED ITS ANSWER, as the
/// loop that asked it awaits that answer: the last act is
/// [`super::policy::turn_end::RULE_DONE_CHECK`] (or an older build's
/// challenge, [`super::policy::turn_end::is_challenge_rule`], the check's
/// kin), or a wall's retry
/// (`is_wall_retry`) typed while a check a wall cut stood — a wall's row
/// after the check, no take after it ([`WALL_TAKEN`]: that wall was the
/// screen it was typed at) and no other end ([`RULE_WALL_WAIT_END`]: the
/// worker's work, a person's turn) since. A loop that follows reads that
/// answer's done report as the check's, as the loop that asked it does
/// (`TurnEndState::seed_done_check`): read as a first one, it typed the
/// check again after the task had ended, and through a whole wall's wait
/// after the retry that delivered a check the wall cut (the verifier of
/// 5b72b1613, M7-5). `None` where the last act is any other, or none.
#[must_use]
pub fn done_check_owed_in(body: &str, sid: Option<&str>) -> Option<CheckOwed> {
    use super::policy::turn_end::{
        RULE_COMPACT, RULE_DONE_CHECK, RULE_OPEN_ITEMS, RULE_UNCOMMITTED, RULE_USAGE_PROBE,
        is_wall_retry,
    };
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut owed: Option<CheckOwed> = None;
    // The points judged since the act owed, no wall cutting it ([`RULE_POINT`]).
    let mut judged = 0_u32;
    for v in body
        .lines()
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
            if rule != RULE_USAGE_PROBE {
                judged = 0;
            }
            owed = match rule {
                // The accounts probe's `/usage` is no act.
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
                        ..o
                    })
                }
                _ => None,
            };
            continue;
        }
        // A read of it: the wall's row, its end, a point judged.
        if matches!(rule, RULE_WALL_WAIT | RULE_WALL_WAIT_END | RULE_POINT)
            && let Some(o) = owed.as_mut()
        {
            o.unread = false;
        }
        match rule {
            RULE_WALL_WAIT => {
                if let Some(o) = owed.as_mut() {
                    if !o.cut {
                        o.read_ms = v.get("ts").and_then(aterm_json::Value::as_i64);
                    }
                    o.cut = true;
                }
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
            // Off stood: a check a wall cut is delivered again no more.
            RULE_OFF_STOOD => owed = owed.filter(|o| !o.cut),
            // THE CHECK'S ANSWER JUDGED: the first point a loop judged after
            // it, no wall cutting it — and a second is a later turn's, after
            // which the check is nobody's to answer.
            RULE_POINT => {
                if let Some(o) = owed.as_mut().filter(|o| !o.cut) {
                    judged += 1;
                    // Awaited no more: its point is judged.
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
    // Judged and the task not ended there: the loop that asked it read its
    // answer as no done report, and a done report after it is a first one.
    // Owed still, a successor read a person's later done report as the
    // check's answer and ended the task where the loop that asked it asked
    // the check (the verifier of 97daeeace, RS8-R4).
    owed.filter(|o| judged == 0 || o.answered)
}

/// Whether a typed row's `command` is the done check, or an older build's
/// check or challenge, again, as a `/compact`'s continuation types the one a
/// full context cut (`TurnEndState::compact_owes`, ATERM-KI-1016-1 done-3):
/// by its opening words ([`super::policy::turn_end::DONE_CHECK`], every
/// retired check's [`super::policy::turn_end::RETIRED_DONE_CHECKS`], every
/// retired challenge's [`super::policy::turn_end::is_challenge_text`]).
fn delivers_check(command: Option<&str>) -> bool {
    use super::policy::turn_end::{DONE_CHECK, RETIRED_DONE_CHECKS, is_challenge_text};
    command.is_some_and(|c| {
        std::iter::once(DONE_CHECK)
            .chain(RETIRED_DONE_CHECKS)
            .any(|head| opens_with(head, c))
            || is_challenge_text(c)
    })
}

/// Whether a typed row's `command` is a land act again, as a `/compact`'s
/// continuation types the one a full context cut ([`delivers_check`]): by
/// its opening words ([`super::policy::turn_end::LAND`],
/// [`super::policy::turn_end::LAND_NUDGE`], or an older build's
/// [`super::policy::turn_end::RETIRED_LANDS`] and
/// [`super::policy::turn_end::RETIRED_LAND_NUDGES`]).
fn delivers_land(command: Option<&str>) -> bool {
    use super::policy::turn_end::{LAND, LAND_NUDGE, RETIRED_LAND_NUDGES, RETIRED_LANDS};
    command.is_some_and(|c| {
        [LAND, LAND_NUDGE]
            .into_iter()
            .chain(RETIRED_LANDS)
            .chain(RETIRED_LAND_NUDGES)
            .any(|head| opens_with(head, c))
    })
}

/// Whether `text` opens with `head`'s words, however spaced or wrapped.
fn opens_with(head: &str, text: &str) -> bool {
    let mut words = text.split_whitespace();
    head.split_whitespace().all(|w| words.next() == Some(w))
}

/// [`done_check_owed_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn done_check_owed(path: &Path, sid: Option<&str>) -> Option<CheckOwed> {
    done_check_owed_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// THE LAND EPISODE A LOOP ON THE SESSION HAD IN HAND ([`land_episode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandSeed {
    /// The nudges typed since the instruction.
    pub nudges: u32,
    /// The last act is one of Land's, typed once the instruction was (or a
    /// wall's retry that delivers one a wall cut again): its clean defense
    /// lands the task (`TurnEndState::landing`).
    pub landing: bool,
    /// A WALL ANSWERED THAT ACT, its row after it and no take after that
    /// ([`WALL_TAKEN`]: the screen it was typed at): the wall's retry
    /// delivers it again (`TurnEndState::cut_land`), as for the done check
    /// ([`CheckOwed::cut`]).
    pub cut: bool,
    /// That act's answer was judged: the task landed after it
    /// ([`RULE_TASK_DONE`]).
    pub answered: bool,
    /// THAT ACT, as its row holds it, while its clean defense would land the
    /// task (`landing`): when it was typed and its words hashed
    /// ([`LandAct`]). `None` where the row names no time or words. Set by
    /// [`land_episode_in`] alone: the walk's own states leave it out, so a
    /// ledger's cut keeps no row for each act ([`standing_path`]).
    pub act: Option<LandAct>,
}

/// THE LAST ACT OF A LAND EPISODE IN HAND ([`LandSeed::act`]): what a loop
/// that follows tells its taking by
/// (`super::policy::turn_end::TurnEndState::seed_land_act`) — its own `❯`
/// row over the reply, or the record's last turn opened by its words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandAct {
    /// When it was typed (`ts`, unix ms).
    pub at_ms: i64,
    /// Its words (`command`), hashed ([`words_hash`]).
    pub said: u64,
    /// Its first words, hashed ([`words_lead`]).
    pub lead: u64,
}

/// THE LAND EPISODE `sid`'s LEDGER TEXT `body` SAYS IS IN HAND, as the loop
/// that typed its acts holds it (`TurnEndState::land`): the last
/// [`super::policy::turn_end::RULE_LAND`] row with no end of the episode
/// after it — no act only another mode types (a continuation, the done
/// check, the reversible answer: [`super::policy::turn_end::is_continue_rule`]
/// and not one of Land's), no [`RULE_LAND_OVER`], no [`RULE_OFF_STOOD`] —
/// and the nudges typed since. `None` where no episode is in hand. Kept in
/// the loop's memory alone, every successor began the episode again: the
/// instruction typed where the first loop nudged (the verifier of 341e1fa81,
/// RS7-2).
#[must_use]
pub fn land_episode_in(body: &str, sid: Option<&str>) -> Option<LandSeed> {
    let want = sid.map(|s| s.trim_start_matches('@'));
    let mut walk = LandWalk::default();
    let mut act = None;
    for v in body
        .lines()
        .filter(|l| LandWalk::reads(l))
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
    {
        if v.get("sid").and_then(aterm_json::Value::as_str) != want {
            continue;
        }
        walk.step(&v);
        // A typed row the walk leaves landing on is the episode's last act
        // (one of Land's, or a wall's retry or `/compact` that delivers it).
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("decision") == Some("typed")
            && text("rule_id") != Some(super::policy::turn_end::RULE_USAGE_PROBE)
        {
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

/// WHAT [`land_episode_in`] HOLDS AS IT WALKS ONE SESSION'S ROWS: the
/// episode in hand, and the points judged since Land's last act, no wall
/// cutting it ([`RULE_POINT`], as [`done_check_owed`] counts them) — `0`
/// while no episode is in hand, where nothing reads them. A row moves it by
/// itself alone ([`Self::step`]), so the ledger's cut can keep just the rows
/// that move it ([`cut_rows`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LandWalk {
    ep: Option<LandSeed>,
    judged: u32,
}

impl LandWalk {
    /// Whether [`land_episode_in`] reads the row `line` at all: a `typed`
    /// or a `waiting` one.
    fn reads(line: &str) -> bool {
        line.contains(",\"decision\":\"typed\",") || line.contains(",\"decision\":\"waiting\",")
    }

    /// The walk past row `v` (of the walk's session, one [`Self::reads`]).
    fn step(&mut self, v: &aterm_json::Value) {
        self.read(v);
        if self.ep.is_none() {
            self.judged = 0;
        }
    }

    fn read(&mut self, v: &aterm_json::Value) {
        use super::policy::turn_end::{
            RULE_COMPACT, RULE_LAND, RULE_LAND_NUDGE, RULE_USAGE_PROBE, is_continue_rule,
            is_land_rule, is_wall_retry,
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        let rule = text("rule_id").unwrap_or("");
        match text("decision") {
            Some("typed") => {
                if rule != RULE_USAGE_PROBE {
                    self.judged = 0;
                }
                let land = |nudges: u32, landing: bool| LandSeed {
                    nudges,
                    landing,
                    cut: false,
                    answered: false,
                    act: None,
                };
                self.ep = match (rule, self.ep) {
                    (RULE_USAGE_PROBE, ep) => ep,
                    (RULE_LAND, _) => Some(land(0, true)),
                    (r, Some(e)) if is_land_rule(r) => {
                        Some(land(e.nudges + u32::from(r == RULE_LAND_NUDGE), true))
                    }
                    (r, _) if is_continue_rule(r) => None,
                    (r, Some(e)) => Some(land(
                        e.nudges,
                        (e.cut && is_wall_retry(r))
                            || (r == RULE_COMPACT && delivers_land(text("command"))),
                    )),
                    (_, None) => None,
                };
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
            RULE_TASK_DONE => e.answered = true,
            // The act's answer judged; a second point is a later turn's.
            RULE_POINT if e.landing && !e.cut => {
                self.judged += 1;
                if self.judged > 1 {
                    e.landing = false;
                }
            }
            // At the act's point: the wall's retry delivers it again.
            RULE_WALL_WAIT if e.landing => {
                e.landing = false;
                e.cut = true;
            }
            // That wall was the screen it was typed at.
            RULE_WALL_WAIT_END if e.cut && text("reason") == Some(WALL_TAKEN) => {
                e.landing = true;
                e.cut = false;
            }
            RULE_WALL_WAIT_END => e.cut = false,
            _ => {}
        }
    }

    /// The episode in hand at the walk's end ([`land_episode_in`]): its
    /// answer judged and the task not ended there, no landing in hand
    /// ([`done_check_owed`]'s RS8-R4).
    fn answer(self) -> Option<LandSeed> {
        self.ep.map(|e| LandSeed {
            landing: e.landing && (self.judged == 0 || e.answered),
            ..e
        })
    }
}

/// [`land_episode_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn land_episode(path: &Path, sid: Option<&str>) -> Option<LandSeed> {
    land_episode_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// Whether a wall of `kind` (a [`RULE_WALL_WAIT`] row's, [`wall_kind`]) is of
/// the limit `limit` was a try at ([`LimitTry::bucket`]).
fn of_try(kind: Option<aterm_phase::WallKind>, limit: LimitTry) -> bool {
    use aterm_phase::WallKind;
    match kind {
        Some(WallKind::ModelBucket { .. }) => limit.bucket,
        Some(WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend) => !limit.bucket,
        _ => false,
    }
}

/// The limit's try an unmarked [`super::policy::turn_end::RULE_LIMIT_RESUME`]
/// row is read as ([`usage_attempt`]): a usage window's, as every loop
/// before the rows named their limit read it.
const USAGE_RESUME_UNMARKED: LimitTry = LimitTry {
    bucket: false,
    in_place: false,
    attempts: 1,
    entitlement: false,
};

/// The words that open the mark of an act typed IN A LIMIT'S PLACE
/// ([`act_reason`]).
const IN_LIMITS_PLACE: &str = "in the limit's place";

/// The words that open the mark of the limit's continuation itself
/// ([`act_reason`]).
const LIMITS_TRY: &str = "the limit's try";

/// The word a mark names a limit's class by ([`LimitTry::bucket`]).
fn limit_word(bucket: bool) -> &'static str {
    if bucket { "bucket" } else { "usage" }
}

/// The field a mark names the entitlement ladder by
/// ([`LimitTry::entitlement`]): absent, the try was on the usage windows'
/// short ladder, as every mark before it was.
const ENTITLEMENT_LADDER: &str = "ladder=entitlement";

/// The limit's try `reason`'s mark names ([`act_reason`]): its last
/// parenthesised group, opened by [`IN_LIMITS_PLACE`] or [`LIMITS_TRY`].
/// `None` for any other reason (the save-then-wait switch's words alone, a
/// model switch's, an account move's, the plain `the turn-end policy`).
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
    // A count the mark lacks or garbles is the first try, as every row was
    // read before the rows counted.
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

/// THE SHORT STREAK A LOOP ON THE SESSION LAST HELD ([`short_streak`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreakSeed {
    /// When its row was written (unix ms).
    pub at_ms: i64,
    /// The short turns in a row ([`act_reason_after`]'s mark; 0 for a row
    /// that marks none).
    pub short: u32,
    /// The row is the record of A POINT JUDGED ([`RULE_POINT`]): the streak
    /// after that point — or, with [`Self::ran`], the books held no word on
    /// the streak before a loop read a turn running ([`RULE_TURN_RAN`] the
    /// first row). Else the streak an act was typed after, its yield still to
    /// be judged (a loop records every point that judges an act's answer).
    pub point: bool,
    /// The row's rule: the act's, whose yield is still to be judged.
    pub rule: String,
    /// The ledger holds a point record ([`RULE_POINT`]): its loops record
    /// every act's answer, so an act no record follows is one whose yield
    /// is still to come. A ledger an older build wrote holds none, and its
    /// last act may have been answered already.
    pub recorded: bool,
    /// A TURN WAS READ RUNNING after the point recorded ([`RULE_TURN_RAN`]):
    /// a loop's next idle point is that turn's end, not the point again.
    pub ran: bool,
    /// When that turn was first read running (its row's `ts`, unix ms).
    pub ran_ms: Option<i64>,
    /// A PERSON'S ESC STOOD AT THE POINT RECORDED ([`PERSONS_ESC`], a point
    /// record's alone): a loop that follows, reading that point again, holds
    /// for the person's grace from the record, as the loop that judged it
    /// does, not from its own attach (`TurnEndState::seed_esc`).
    pub esc: bool,
    /// THE RETRY LADDER OF THE API ERROR the loop waited at as of the row
    /// ([`Carried::retry`], [`RETRY_LADDER`]).
    pub retry: Option<RetryMark>,
}

/// THE SHORT STREAK `sid`'s LEDGER TEXT `body` LAST SAYS ([`StreakSeed`]): the
/// mark of its last act (a `typed` row as [`typed_at`] counts them, but for a
/// relaunch's, which moves no streak: `TurnEndState::restarted`) or of its
/// last point record ([`RULE_POINT`]), whichever is later, however old —
/// the loop that wrote it holds its streak until real work. `None` where
/// neither was written. Read from the acts alone, a successor missed every
/// point the streak moved at with nothing typed (someone else's short turn,
/// a wake's, the act's own yield backed off) and counted from its own
/// attach, sooner and later than the loop before it (the verifier of
/// 97daeeace, RS8-R3, RS8-F2, RS8-F3).
#[must_use]
pub fn short_streak_in(body: &str, sid: Option<&str>) -> Option<StreakSeed> {
    use super::policy::turn_end::{
        RULE_ACCOUNT, RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE, RULE_USAGE_PROBE,
    };
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<StreakSeed> = None;
    let mut recorded = false;
    for v in body
        .lines()
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
                // A loop's first busy read on books with no word on the
                // streak yet: as after a point, its end is that turn's, read
                // running then.
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
            // An act, as [`typed_at`] counts them: no account move made, no
            // accounts probe — and no relaunch onto a model.
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

/// THE CARRY THE LAST ACT GAVE IS OVER, by `sid`'s ledger text `body`
/// (ATERM-KI-1016-1): after its last act (a `typed` row as [`typed_at`]
/// counts them), the loop that kept it judged a point that landed the task
/// ([`RULE_TASK_DONE`], or an older build's end there) or left a person's
/// exchange to them (a `skipped`
/// row under [`super::policy::turn_end::RULE_LEFT`]) — both end the carry
/// (`TurnEndState::observe`: done twice) — or Off stood after the act's
/// answer was judged ([`RULE_OFF_STOOD`] after a [`RULE_POINT`]: the park
/// drops the carry, `TurnEndState::park_off`). A loop that follows holds no
/// task its acts carried, as that loop holds none
/// (`TurnEndState::seed_carry_over`). Seeded from the acts alone, a
/// successor carried the task the loop before had stopped carrying, read a
/// person's own short turn after it as a turn of the task's, and typed
/// `keep going` into it where the loop before left it to them (7 of 4051
/// cells of the seed matrix with every read stamped with the person). Not
/// where Off stood before the act's answer was judged: that answer gives
/// the carry back (`TurnEndState::observe`'s act arm). And A PERSON'S ESC
/// STOOD AT THE POINT THAT JUDGED THE ACT'S ANSWER (the first point record
/// after the act, its [`PERSONS_ESC`] mark, [`point_reason`]): the act's
/// arm ends the carry of an answer someone other than the harness stopped,
/// so a person's short turn after it is theirs. Read past, a successor
/// attached after that point typed `keep going` into the person's exchange
/// the loop before left to them (the review's KR-P6-ESC). The mark is
/// written wherever a stop that is no Esc of the harness's stands at the
/// point (`TurnEndState::persons_esc_stands`), which the act's arm reads
/// as the person's. `false`: no such record since the last act, or no act.
///
/// AN OLDER BUILD'S [`RULE_TASK_DONE`] ROW — written at a done reply to its
/// done check or at the retired token, and spelled as a landing's (the rule
/// that made it is only in the journal's `DONE` note) — ends the carry too,
/// and is kept so (FOREVER CRUISE / FOREVER LAND, decided 2026-10-19):
/// ending the carry seeds no end — `TurnEndState::seed_carry_over` clears
/// the carry alone, never the task's done, its landing or its face, and the
/// first point is decided afresh, a done claim there getting Cruise's prompt
/// or Land's act — and such a row marks a reply whose worker said it was
/// done, after which the policy leaves a person's own exchange to them as
/// well (`TurnEndState::observe` counts the carry only while no done claim
/// stands). Read past, a successor would type `keep going` after a person's
/// quick question where the policy, judging that reply itself, would not.
#[must_use]
pub fn carry_ended_in(body: &str, sid: Option<&str>) -> bool {
    use super::policy::turn_end::{RULE_ACCOUNT, RULE_LEFT, RULE_USAGE_PROBE};
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let esc = format!("({PERSONS_ESC})");
    let (mut ended, mut judged) = (false, false);
    for line in body.lines() {
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

/// [`short_streak_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn short_streak(path: &Path, sid: Option<&str>) -> Option<StreakSeed> {
    short_streak_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// The rule id of the record of A TURN READ RUNNING, the first since the
/// point a loop recorded last ([`RULE_POINT`]) — or since its start, on
/// books with no word on the streak yet
/// ([`super::policy::turn_end::TurnEndState::take_ran`]): a loop that follows
/// reads its first idle point as that turn's end ([`StreakSeed::ran`]), as
/// the loop that read it running would have, not as the recorded point read
/// again, nor as a point no read saw run
/// ([`super::policy::turn_end::TurnEndState::take_gap`]). A record: no act.
pub const RULE_TURN_RAN: &str = "turn-ran@v1";

/// The reason of the [`RULE_TURN_RAN`] row.
pub const TURN_RAN: &str = "a turn was read running after the point the turn-end policy judged";

/// The rule id of the record of A POINT THE TURN-END POLICY JUDGED that
/// moved what a loop that follows must hold as the loop that judged it does
/// (`Session::fold_point`, [`super::policy::turn_end::TurnEndState::take_point_moved`]):
/// the short streak after it (its mark, [`point_reason`]: [`short_streak`]),
/// and an act's answer judged — every act's, so an act no record follows is
/// one whose yield is still to come — the done check's or a land act's among
/// them ([`done_check_owed`], [`land_episode`], [`awaited_try`]) — and the
/// point's time, which every turn's end the policy judges moves. A loop that
/// follows reads that point again with the streak and the time the loop that
/// judged it left (`TurnEndState::seed_point`) — unless a turn was read
/// running after it ([`RULE_TURN_RAN`]) — and a second point judged after an
/// act as a later turn's. A record: no act.
pub const RULE_POINT: &str = "point@v1";

/// The reason of the [`RULE_POINT`] row: what the loop holds after the point
/// (`after`: the short streak, and the retry ladder where one stands),
/// marked as an act's is ([`act_reason_after`]), and [`PERSONS_ESC`] where
/// `esc`: a person's Esc stood at the point.
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

/// The words of the mark that A PERSON'S ESC STOOD AT THE POINT a record was
/// written for ([`point_reason`], [`StreakSeed::esc`]): the hold after a
/// person's Esc counts from the policy's first read of it
/// (`TurnEndState::esc_at`), which at a point that ended the turn is the
/// point itself. Unmarked, a loop that follows read the Esc first at its
/// own attach and held from there, later than the loop that judged it (the
/// merge of main's 28b288273 with RS9-F2). A row an older build wrote
/// carries none, and seeds nothing.
const PERSONS_ESC: &str = "a person's Esc stood at the point";

/// WHAT A ROW OF THE LOOP'S CARRIES of the books the loop holds after it,
/// marked on an act's row ([`act_reason_after`]) and on a point's record
/// ([`point_reason`]) for a loop that follows to take up ([`short_streak`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Carried {
    /// The short turns in a row (`TurnEndState::short_streak`).
    pub short: u32,
    /// THE RETRY LADDER OF THE API ERROR the loop waits at
    /// ([`RetryMark`], [`RETRY_LADDER`]), where one stands.
    pub retry: Option<RetryMark>,
}

impl Carried {
    /// The short streak `short`, no retry ladder.
    #[must_use]
    pub const fn streak(short: u32) -> Self {
        Self { short, retry: None }
    }
}

/// THE RETRY LADDER OF AN API ERROR'S WALL (an overload, a server's failure,
/// a network never reached, a reply cut off: the turn-end policy's retry
/// track) as a row of the loop's leaves it ([`Carried::retry`]): a loop that
/// follows works that wall from the rung the loop before it was on, counted
/// from the appearance it counted from (`TurnEndState::seed_retry`). Taken
/// up from nothing, it retried on the ladder's first rung (a minute) from
/// its own read, whatever rung the loop before was on: up to an hour sooner
/// (ATERM-KI-1016-1, "a loop that follows another").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryMark {
    /// The wall as its notice last read: an overload, or an API error's
    /// cause, status and retry rule, which choose its ladder.
    pub kind: aterm_phase::WallKind,
    /// The retries typed at it this episode.
    pub attempts: usize,
    /// Those typed because the host measured the API reachable again.
    pub ups: usize,
    /// The appearance its ladder counts from (unix ms).
    pub since_ms: i64,
}

/// The words that open the mark of THE WALL'S RETRIES ([`RetryMark`]):
/// ` (the wall's retries: kind=overloaded attempts=2 ups=0 since=<unix ms>)`,
/// an API error's ` kind=api-error cause=server code=500 retryable=yes`. A
/// row an older build wrote carries none, and seeds nothing.
const RETRY_LADDER: &str = "the wall's retries";

/// An API error's cause as the retry mark words it ([`RETRY_LADDER`]).
const API_CAUSES: [(aterm_phase::ApiCause, &str); 6] = [
    (aterm_phase::ApiCause::Server, "server"),
    (aterm_phase::ApiCause::Unreachable, "unreachable"),
    (aterm_phase::ApiCause::CutOff, "cut-off"),
    (aterm_phase::ApiCause::Config, "config"),
    (aterm_phase::ApiCause::Request, "request"),
    (aterm_phase::ApiCause::Overlong, "overlong"),
];

/// The mark of [`RETRY_LADDER`], or nothing where no retry's wall stands —
/// nor one of a kind the mark does not word.
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

/// The [`RetryMark`] a row's `reason` carries ([`retry_mark`]); `None` for
/// none, or one it garbles.
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

/// The mark of THE SHORT STREAK a row of the loop's was written after
/// ([`act_reason_after`]): ` (short turns in a row: 3)`, or nothing for none.
fn short_mark(short: u32) -> String {
    if short > 0 {
        format!(" ({SHORT_STREAK}: {short})")
    } else {
        String::new()
    }
}

/// The reason the row of an act the turn-end policy typed carries: `the
/// turn-end policy` — and, where the act was THE LIMIT'S TRY (`limit`,
/// [`super::policy::turn_end::TurnEndState::limit_try`]: `usage-resume@v1`,
/// or the done check, the reversible answer or under Autopilot Land a land
/// act typed in its place at a limit's end), its mark last:
/// [`IN_LIMITS_PLACE`] or [`LIMITS_TRY`], the limit, the wall's attempts
/// the act left and, for a try at a wall no reset ends, the entitlement
/// ladder it was on ([`ENTITLEMENT_LADDER`]) — `(in the limit's place:
/// wall=bucket attempts=2)`, `(the limit's try: wall=usage attempts=3
/// ladder=entitlement)` — so a loop that starts after it
/// ([`usage_attempt`]) works that limit from that act on that rung of
/// that ladder, as the loop that typed it does, as
/// [`model_switch_reason`] carries a model switch across a loop's restart.
/// An act anywhere else carries the words it always has.
#[must_use]
pub fn act_reason(limit: Option<LimitTry>) -> String {
    act_reason_after(Carried::default(), limit)
}

/// The words that open the mark of THE SHORT STREAK an act was typed after
/// ([`act_reason_after`], [`short_streak`]).
const SHORT_STREAK: &str = "short turns in a row";

/// [`act_reason`], with what the loop holds after the act (`after`,
/// [`Carried`]) marked before the limit's mark where there is one: THE
/// SHORT STREAK it was typed after (`TurnEndState::short_streak`) — `(short
/// turns in a row: 3)` — so a loop that starts after it backs off the act's
/// next short yield as the loop that typed it does ([`short_streak`]). None
/// is marked for no streak. A mark an older build wrote that the job had
/// the open-items challenge (`the open-items challenge was typed this job`)
/// seeds nothing: no challenge ends a job with work left (the owner's rule
/// of 2026-10-19).
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

/// The reason a model fallback's row of the turn-end policy carries (`relaunch
/// --model <fallback>`, or an older `/model <fallback>` row): the switch as it
/// must be undone, the bucket's model and its reset (unix seconds, `-` when
/// unknown), so a loop that starts after it ([`open_model_switch`]) switches
/// back at the reset, as the loop that switched would have — and when the
/// relaunch was made (`made_unix`, no later than its line was typed:
/// [`OpenSwitch::made_unix`]).
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

/// A model switch a loop on the session made and no loop has undone: the
/// last `typed` `relaunch --model …` (or, from before D7, `/model …`) row of
/// `model-fallback@v1` with no `model-restore@v1` row after it — and no
/// account move MADE after it ([`account_switch_reason`]'s row): that
/// relaunch came back on the model the fallback replaced and the policy let
/// the switch go (`TurnEndState::restarted`), so a loop that starts after it
/// has no return to make. Read back as open, the switch relaunched the
/// session once more at the OLD account's bucket's reset, onto the model it
/// already ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSwitch {
    /// The bucket's model (`None`: none `/model` knows).
    pub from: Option<String>,
    /// The fallback switched to.
    pub to: String,
    /// The bucket's reset, unix seconds (`None`: the notice named none).
    pub back_at_unix: Option<i64>,
    /// A `typed` row of any rule follows the relaunch's, or the row of a
    /// turn asked on the fallback that typed none ([`FALLBACK_ASKED`]). Until
    /// one does, the bucket's notice the relaunched process redraws is no
    /// wall ([`super::policy::turn_end::TurnEndState::seed_fallback_unasked`]).
    pub asked: bool,
    /// WHEN THE RELAUNCH WAS MADE, unix seconds, as the loop that made it
    /// told its policy ([`super::policy::turn_end::TurnEndState::fallback_made_at`]:
    /// the first ask of its run of tries, no later than the line was typed)
    /// — or, in a row that names none (an older build's), when the row was
    /// written. A notice of the bucket the transcript wrote after it answered
    /// a turn asked on the fallback. Read off the row's time alone (written
    /// once the host's step said the relaunch was adopted, up to 90 s and
    /// more after the line was typed: `wait:resume`, then `adopted`), a
    /// bucket's notice that answered a person's prompt between the two read
    /// as the relaunched process's redraw, and `continue@v1` went into the
    /// standing bucket (the verifier of b2c17b2dd, A-2).
    pub made_unix: i64,
}

/// The reason of the `waiting` row of
/// [`super::policy::turn_end::RULE_MODEL_FALLBACK`] a loop writes where A
/// TURN WAS ASKED ON THE FALLBACK and no typed row of its own says so (the
/// point after the relaunched process's redraw drew no notice of the bucket
/// it left, or the host typed a turn: `Session::fallback_asked`): the
/// bucket's notice is a wall again, for a loop that follows too
/// ([`OpenSwitch::asked`]). Without it the successor read only typed rows,
/// took the notice that answered a person's turn for the redraw, and
/// typed `continue@v1` into the standing bucket two days before its reset
/// (the verifier of fde9e066a, A1).
pub const FALLBACK_ASKED: &str =
    "a turn was asked on the fallback: the bucket it left is a wall again";

/// The reason of the `waiting` row of
/// [`super::policy::turn_end::RULE_MODEL_FALLBACK`] a loop writes where THE
/// FALLBACK'S RELAUNCH WAS ASKED AND MUST WAIT (the host's `wait:…`, at the
/// run's first wait: `wait:resume` says its line was typed and the new
/// process is not found yet), naming when the run's first ask was made
/// (`made_unix`, unix seconds: the line an ask of the run types is typed no
/// sooner). A loop that follows before the relaunch's own row
/// ([`relaunch_waiting`]) takes that run up as its own, so the relaunch its
/// host carries on is made then, as for the loop that asked it. Written at
/// the adoption alone, a successor started while the relaunch was in flight
/// asked it itself and took its own first ask as when it was made: a
/// bucket's notice that answered a person's prompt after the line and before
/// that ask read as the relaunched process's redraw, and `continue@v1` went
/// into the standing Sonnet bucket two days before its reset (the verifier
/// of 5b72b1613, M7-4).
#[must_use]
pub fn relaunch_waits_reason(made_unix: i64) -> String {
    format!("{RELAUNCH_WAITS} (made={made_unix})")
}

/// The words that open [`relaunch_waits_reason`].
const RELAUNCH_WAITS: &str = "the fallback's relaunch was asked and waits for the host";

/// The run of the fallback's relaunch a loop asked and no row has ended
/// ([`relaunch_waits_reason`]'s row with no `typed` row of any rule after
/// it: the relaunch's own row, or any act, says what became of it): when
/// its first ask was made, unix seconds. `None` when there is none.
#[must_use]
pub fn relaunch_waiting_in(body: &str, sid: Option<&str>) -> Option<i64> {
    use super::policy::turn_end::RULE_MODEL_FALLBACK;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut made = None;
    for line in body.lines() {
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

/// [`relaunch_waiting_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn relaunch_waiting(path: &Path, sid: Option<&str>) -> Option<i64> {
    relaunch_waiting_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// The switch `sid`'s ledger text `body` holds open ([`OpenSwitch`]), read
/// from [`model_switch_reason`]'s words; `None` when there is none, the
/// file cannot be read, or the row predates the words.
#[must_use]
pub fn open_model_switch_in(body: &str, sid: Option<&str>) -> Option<OpenSwitch> {
    let want = sid.map(|s| s.trim_start_matches('@'));
    let mut walk = SwitchWalk::default();
    for line in body.lines().filter(|l| SwitchWalk::reads(l)) {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        if v.get("sid").and_then(aterm_json::Value::as_str) == want {
            walk.step(&v);
        }
    }
    walk.open
}

/// WHAT [`open_model_switch_in`] HOLDS AS IT WALKS ONE SESSION'S ROWS: the
/// switch open. A row moves it by itself alone ([`Self::step`]), so the
/// ledger's cut can keep just the rows that move it ([`cut_rows`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SwitchWalk {
    open: Option<OpenSwitch>,
}

impl SwitchWalk {
    /// Whether [`open_model_switch_in`] reads the row `line` at all: a
    /// `typed` one, or [`FALLBACK_ASKED`]'s.
    fn reads(line: &str) -> bool {
        line.contains(",\"decision\":\"typed\",") || line.contains(FALLBACK_ASKED)
    }

    /// The walk past row `v` (of the walk's session, one [`Self::reads`]).
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
            // An account move MADE (its own words, as [`account_moves`]
            // reads them — a draft submitted under the rule is none).
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

/// [`open_model_switch_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn open_model_switch(path: &Path, sid: Option<&str>) -> Option<OpenSwitch> {
    open_model_switch_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// What an account move's ledger row carries as its command, before the
/// label: `relaunch under account work-2`. The label is the only name an
/// account has in any record; the login directory is never written here.
pub const ACCOUNT_MOVE_COMMAND: &str = "relaunch under account ";

/// The word a row names [`AccountAt`] by.
fn account_at_word(at: super::policy::turn_end::AccountAt) -> &'static str {
    match at {
        super::policy::turn_end::AccountAt::Wall => "wall",
        super::policy::turn_end::AccountAt::Ahead => "ahead",
    }
}

/// The reason an account move's row of the turn-end policy carries
/// (`account-rotate@v1`, `typed`: the relaunch was MADE): the label moved to
/// and where the move was asked, so a loop that starts after it
/// ([`account_moves`]) counts it toward the session's bound and knows a move
/// is still to be answered — as [`model_switch_reason`] carries a model
/// switch across a loop's restart.
#[must_use]
pub fn account_switch_reason(to: &str, at: super::policy::turn_end::AccountAt) -> String {
    format!(
        "the turn-end policy (account move: to={to} at={})",
        account_at_word(at)
    )
}

/// The reason the row of an account move that could NOT be made carries
/// (`account-rotate@v1`, `skipped`): the label, where it was asked and the
/// host's word (`-`: no host makes one), so a loop that starts at the same
/// wall does not ask again ([`AccountMoves::unmade`]).
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

/// The reason the row carries that says WHAT ANSWERED an account move
/// (`account-rotate@v1`, `waiting`: a record, no act): the label and the
/// answer the loop told its host — `served`, or `wall` / `signed-out` — so a
/// loop that starts after it ([`account_moves`]) neither forgets a move still
/// to be served nor tells a bad answer a second time. A bad answer's row
/// says `until=served`: this build's loops tell a told move `served` where
/// the worker works past it, so the move stands until such a row closes it,
/// whatever its age ([`OpenAccountMove::told_closes`]).
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

/// An account move a loop on the session made that its ledger does not show
/// SERVED ([`open_account_move`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAccountMove {
    /// The label moved to.
    pub to: String,
    /// Where the move was asked.
    pub at: super::policy::turn_end::AccountAt,
    /// When the row was written (unix ms).
    pub at_ms: i64,
    /// A `typed` row of any rule follows it: a turn has been asked under the
    /// new login since.
    pub asked: bool,
    /// The host was told of a wall or a lost login that answered it
    /// ([`account_answer_reason`]'s row): told, and not told again.
    pub told: bool,
    /// That told row says `until=served`: a build whose loops tell the move
    /// `served` where it ends wrote it, so the move is open until a row says
    /// so, whatever its age. A told row without it is an older build's,
    /// which closed told moves silently: open on its ledger for good, and
    /// history past the window
    /// ([`super::policy::turn_end::TurnEndState::seed_account`]).
    pub told_closes: bool,
}

/// What `sid`'s ledger says of its account moves ([`account_moves`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountMoves {
    /// When each move MADE was written (unix ms), oldest first.
    pub made_ms: Vec<i64>,
    /// The last move made, until a row says it was SERVED
    /// ([`account_answer_reason`]): still to be answered by the loop that
    /// reads this.
    pub open: Option<OpenAccountMove>,
    /// A move could not be made, and neither an act of the policy's nor the
    /// end of the wall it waited on ([`RULE_WALL_WAIT_END`]) follows that
    /// row: the wall it was asked at still stood when the loop ended.
    pub unmade: bool,
}

/// [`AccountMoves`] of `sid`'s ledger text `body`, read from
/// [`account_switch_reason`]'s, [`account_unmade_reason`]'s and
/// [`account_answer_reason`]'s words. A
/// missing or unreadable file, and a session that never moved, know none.
/// A draft submitted under the rule (its command is the draft's text, not
/// [`ACCOUNT_MOVE_COMMAND`]) is no move.
#[must_use]
pub fn account_moves_in(body: &str, sid: Option<&str>) -> AccountMoves {
    use super::policy::turn_end::{AccountAt, RULE_ACCOUNT};
    let mut out = AccountMoves::default();
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    for line in body.lines() {
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
                out.unmade = false;
            }
            // What answered the move: served closes it, anything else is
            // told and stands.
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
            // Only a move asked AT A WALL latches (one asked ahead of a wall
            // left no wall standing).
            if rule == Some(RULE_ACCOUNT)
                && reason.contains("account move unmade:")
                && reason.contains(" at=wall")
            {
                out.unmade = true;
            }
            continue;
        }
        // A `typed` row: an act on the point, whatever its rule — and a
        // turn asked under the login an open move chose.
        out.unmade = false;
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

/// [`account_moves_in`] over the ledger at `path`: a missing or unreadable file
/// knows nothing.
#[must_use]
pub fn account_moves(path: &Path, sid: Option<&str>) -> AccountMoves {
    std::fs::read_to_string(path).map_or_else(
        |_| AccountMoves::default(),
        |body| account_moves_in(&body, sid),
    )
}

/// The account move `sid`'s ledger at `path` holds open
/// ([`AccountMoves::open`]): beside [`open_model_switch`], for the same
/// reason — a loop that starts after the move still has it to answer.
#[must_use]
pub fn open_account_move(path: &Path, sid: Option<&str>) -> Option<OpenAccountMove> {
    account_moves(path, sid).open
}

/// The rule id of the ledger row that records a wall the loop waits on
/// ([`super::wall_memory`]).
pub const RULE_WALL_WAIT: &str = "wall-wait@v1";

/// The rule id of the row that ends it: the worker worked, the screen left the
/// wall, or another wall replaced it.
pub const RULE_WALL_WAIT_END: &str = "wall-wait-end@v1";

/// The rule id of the record that THE SESSION'S AUTOPILOT OFF STOOD
/// (`Session::off_stood`: the loop parked under Off, or the host started it
/// as Off lifted): the person drove the session meanwhile, and what the park
/// ends is over for a loop that follows too
/// (`TurnEndState::park_off`, [`OFF_STOOD`]). A record: no act. A build
/// through 0.105.0 writes none, and its park's journal note stands for one
/// (`run::with_parks_journaled`: `PARKED seq=<n> autopilot=off`).
pub const RULE_OFF_STOOD: &str = "autopilot-off@v1";

/// The rule id of the record that A LOOP TOOK UP THE BOOKS OF THE TABS A COLD
/// RESTORE REOPENED ITS TAB FROM (ATERM-KI-1016-1, follow-7): written once,
/// as the first row of the session in the tab's ledger. While it stands, the
/// session's books are the earlier tabs' rows and then this ledger's
/// ([`books_through`], [`takes_up_earlier`]); a cut that drops it
/// ([`bound`]) dropped every row a continuous ledger's cut would, and the
/// earlier tabs' books are over. A record: no act.
pub const RULE_EARLIER_BOOKS: &str = "earlier-books@v1";

/// The reason of the [`RULE_EARLIER_BOOKS`] row.
pub const EARLIER_BOOKS: &str = "a cold restore reopened this tab: the books of the tabs it was \
     reopened from are this session's";

/// The rule id of the record that THE TASK LANDED AT A POINT a loop judged
/// (`Session::task_edge`): a clean defense at a land act's yield (FOREVER
/// LAND) — nothing is typed until someone else's turn. Older builds wrote it
/// at a done reply to the done check or at the retired token too, which ends
/// no task now: a loop takes a landing on only from the `DONE rule=land@v1`
/// journal line and the task record (`run.rs`'s `landed_before`,
/// `supervise::task_mark`), never from this row. A record: no act. A loop
/// that follows reads the act awaited before it as answered
/// ([`CheckOwed::answered`], [`LandSeed::answered`]): a turn read busy since
/// is a later one, whose done report is a first one.
/// Unwritten, a successor attached during a person's turn after the check's
/// answer read that turn's done report as the check's answer and typed
/// nothing, where the loop that asked it asked the check again (the verifier
/// of 341e1fa81, RS7-4).
pub const RULE_TASK_DONE: &str = "task-done@v1";

/// The reason of the [`RULE_TASK_DONE`] row.
pub const TASK_DONE: &str = "the task is done: nothing is typed until someone else's turn";

/// The rule id of the record that THE LAND EPISODE IN HAND IS OVER by no act
/// of Land's (`Session::land_over`: a reading or a decision in another
/// mode): a Land after it is a new episode, its instruction typed again
/// ([`land_episode`]). A record: no act.
pub const RULE_LAND_OVER: &str = "land-over@v1";

/// The reason of the [`RULE_LAND_OVER`] row.
pub const LAND_OVER: &str =
    "the land episode is over (a reading in another mode): a Land after it is a new one";

/// The reason of the [`RULE_OFF_STOOD`] row. A loop that follows reads, from
/// it on, no Land episode before it ([`land_episode`]) and no act a wall cut
/// delivered again by a retry ([`done_check_owed`]).
/// Kept in the park's memory alone, a successor read a done check the weekly
/// limit cut before the park as delivered again by the retry at the reset,
/// and ended the task on the retry's done reply where the loop that parked
/// asked the check again (the verifier of 341e1fa81, RS7-3).
pub const OFF_STOOD: &str = "autopilot off stood: the person drove the session; the land episode, \
     the carry and an act a wall cut are over";

/// The reason of the [`RULE_WALL_WAIT_END`] row the loop writes where A
/// LIMIT'S TRACK ENDED by no act of the loop's, by the policy's own rule
/// ([`super::policy::turn_end::TurnEndState::limit_ended`]: the worker
/// worked, or another wall took its place): the episode the limit's tries
/// belong to is over, and a try before the row is no seed of a loop that
/// follows ([`usage_attempt`]). The memory's own end row
/// ([`super::wall_memory`]) says no such thing: a notice that left the
/// screen with no work since ends it too, while the policy's track stands.
pub const LIMIT_ENDED: &str = "the limit's track ended (work, or another wall): its tries are over";

/// The rule id of the record that THE LIMIT'S TRY A LOOP BEFORE THIS ONE MADE
/// IS OVER for the loop that writes it, its seed having found the try aged
/// out ([`super::policy::turn_end::TurnEndState::take_try_over`]: its
/// appearance older than the try's next rung), written before that loop's
/// first wall row: a loop that follows takes up no try before it
/// ([`usage_attempt`]), as for the loop that wrote it. A record, and NO END
/// OF THE WALL: the wall stands, and so does what it holds — a done check or
/// a land act it cut ([`done_check_owed`], [`land_episode`]), a move refused
/// at it ([`AccountMoves::unmade`]). Written as the wall's end
/// ([`LIMIT_ENDED`]), every reader of the end read the wall over at once, and
/// a third loop typed the done check after the retry's done reply where the
/// loop that wrote it ended the task, and asked a move again at the wall it
/// was refused at (the verifier of 97daeeace, RS8-R2).
pub const RULE_TRY_OVER: &str = "try-over@v1";

/// The reason of the [`RULE_TRY_OVER`] row.
pub const TRY_OVER: &str =
    "the limit's try a loop before this one made aged out: its tries are over; the wall stands";

/// The rule id of the record that THE LIMIT'S TRY A LOOP BEFORE THIS ONE MADE,
/// which no loop had read, WAS ANSWERED AT ONCE where the loop that writes it
/// dated the answer ([`super::policy::turn_end::TurnEndState::date_unread_try`]:
/// the screen's done row, the transcript's notice): written before that
/// loop's first wall row, its reason naming the first read it dated — the
/// latest the loop that typed the try made it ([`try_answered_reason`]). A loop that
/// follows reads that read as the try's, the try met there
/// ([`UsageAttempt::met_ms`]), as the loop that wrote it took it up (and
/// awaits it no more: the point that loop judged at once follows,
/// [`RULE_POINT`]). Read from the wall row after it, a third loop counted
/// the rung from the second loop's attach: hours after the second loop's
/// next try.
pub const RULE_TRY_ANSWERED: &str = "try-answered@v1";

/// The reason of the [`RULE_TRY_ANSWERED`] row: the try's first read at
/// `read_ms` (unix ms).
#[must_use]
pub fn try_answered_reason(read_ms: i64) -> String {
    format!("{TRY_ANSWERED}{read_ms}")
}

/// [`try_answered_reason`]'s words before the read's time.
const TRY_ANSWERED: &str =
    "the limit's try a loop before this one made was answered at once: its first read_ms=";

/// The first read a [`RULE_TRY_ANSWERED`] row's `reason` names (unix ms).
fn try_answered_of(reason: &str) -> Option<i64> {
    reason.strip_prefix(TRY_ANSWERED)?.parse().ok()
}

/// The reason of the remembered wall's [`RULE_WALL_WAIT_END`] row at a live
/// turn that TAKES THE WALL'S RETRY a wall answered inside its take window
/// ([`super::policy::turn_end::TurnEndState::retry_met_untaken`]): that wall
/// was the screen the retry was typed at, not its answer, and the try is not
/// met again ([`UsageAttempt::met_ms`]). Otherwise the row says [`super::wall_memory`]'s
/// own words.
pub const WALL_TAKEN: &str =
    "the act was taken: the wall read since it was typed was the screen it was typed at";

/// The reason of the [`RULE_WALL_WAIT_END`] row where THE CONVERSATION HOLDS
/// NO TASK — a `/clear` at the limit, or a conversation started since — so
/// nothing is owed at its reset: the policy's track ends, and its tries with
/// it ([`LIMIT_ENDED`]'s; [`super::wall_memory`] writes it for the memory as
/// for the policy).
pub const WALL_LEFT_NO_TASK: &str =
    "the conversation holds no task (a /clear, or a new one): nothing is owed at the reset";

/// The reason a [`RULE_WALL_WAIT`] row carries: the wall the loop waits on,
/// as a loop that starts after it ([`open_wall`]) needs it to go on waiting
/// when the wall's rows are not on the screen — the reset PLACED (unix
/// seconds, which a loop with only the words could not do without the print),
/// the reset as the notice words it, THE KIND the loop read and tracked it as
/// (`kind`, [`aterm_phase::WallKind::name`]: read back by [`wall_kind`]), and
/// the notice. The notice comes last: it is free text. The kind is the
/// loop's reading, which the words alone do not always carry: a model's
/// bucket that only its `rateLimitType` named is written as words that name
/// no limit, with the type after them where the type names a model and the
/// screen's countdown after that where the vendor goes on by itself
/// (`super::wall_confirm`'s `limit_words`, `named_by`). Read back by the
/// words, the countdown's words or a `seven_day_overage_included` (no model
/// to write) made that bucket a usage window: a try at it was never met
/// again ([`UsageAttempt::met_ms`]), and a loop that follows put a window's
/// track in the bucket's place and typed `usage-resume@v1` into the standing
/// bucket at once, ten minutes before the loop that wrote the row (the
/// verifier of b5a94f3c1, R5-1).
#[must_use]
pub fn wall_wait_reason(
    reset_unix: Option<i64>,
    reset: Option<&str>,
    kind: aterm_phase::WallKind,
    message: &str,
) -> String {
    format!(
        "the turn-end policy (wall: reset_unix={}; reset={}; kind={}; message={})",
        reset_unix.map_or_else(|| "-".to_string(), |s| s.to_string()),
        reset.unwrap_or("-"),
        kind.name(),
        message.replace(['\n', '\r'], " ")
    )
}

/// The limit a [`wall_wait_reason`]'s `kind=` names ([`aterm_phase::WallKind::name`]),
/// among the walls a row is written for that a name alone gives back; `None`
/// for another (a 429's, whose code the name does not carry).
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

/// THE KIND OF A WALL A [`RULE_WALL_WAIT`] ROW RECORDS: the one it names
/// (`named`, [`wall_wait_reason`]'s `kind=`), as the loop that wrote it read
/// it — its words' own reading of that kind where they read it (a bucket's
/// consent asked) — or, for a row that names none (0.104.0's, and this
/// branch's before the kind was written), its words' (`super::run::words_kind`).
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

/// A wall a loop on the session waits on and no loop has ended
/// ([`open_wall`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWall {
    /// When the row was written (unix ms).
    pub at_ms: i64,
    /// The notice, as the reader gave it.
    pub message: String,
    /// The reset as the notice words it.
    pub reset: Option<String>,
    /// The reset as the loop placed it (unix seconds).
    pub reset_unix: Option<i64>,
    /// The kind the row names ([`wall_wait_reason`]'s `kind=`; `None` in a row
    /// that names none): read with the words by [`wall_kind`].
    pub kind: Option<aterm_phase::WallKind>,
    /// THE POINT JUDGED AT IT: the first [`RULE_POINT`] record after the row
    /// (unix ms), where the loop that wrote it counted the wall's appearance
    /// from (`TurnEndState::seed_unnamed_wall`) — the row is written at the
    /// wall's first read, which inside an act's take window comes before
    /// the point that judges it.
    pub judged_ms: Option<i64>,
    /// The act typed last before the row was THE WALL'S OWN RETRY (a
    /// limit's try, an act in its place, an API error's retry: unix ms),
    /// whose answer the row read: the appearance moves to the point that
    /// judges it.
    pub retry_ms: Option<i64>,
}

/// The wall `sid`'s ledger text `body` holds open: the last [`RULE_WALL_WAIT`]
/// row with no [`RULE_WALL_WAIT_END`] row after it and no `typed` row of any
/// rule after it (an act on the wall is the loop's own record of what became
/// of it; the wall it met again is written again, at its new print or the
/// same one: the loop's `wall_closed_by_act`). `None` when there is none or
/// the file cannot be read.
#[must_use]
pub fn open_wall_in(body: &str, sid: Option<&str>) -> Option<OpenWall> {
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut open: Option<OpenWall> = None;
    // The act typed last, where it was the wall's own retry (unix ms).
    let mut retry_ms = None;
    for line in body.lines() {
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
                open = Some(OpenWall {
                    at_ms: ts,
                    message: row.message.to_string(),
                    reset: row.reset,
                    reset_unix: row.reset_unix,
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

/// A [`RULE_WALL_WAIT`] row's reason read back ([`wall_wait_of`]).
struct WallRow<'a> {
    /// The reset placed (unix seconds).
    reset_unix: Option<i64>,
    /// The reset as the notice words it.
    reset: Option<String>,
    /// The kind the row names (`None`: a row that names none).
    kind: Option<aterm_phase::WallKind>,
    /// The row names no kind at all: a build through 0.106.0 wrote it, whose
    /// end of the wall says nothing of why the wall ended ([`WALL_TAKEN`],
    /// [`LIMIT_ENDED`] are newer).
    older: bool,
    /// The notice.
    message: &'a str,
}

/// A [`RULE_WALL_WAIT`] row's reason read back ([`wall_wait_reason`]): the
/// reset placed, the reset as the notice words it, the kind where the row
/// names one, and the notice.
fn wall_wait_of(reason: &str) -> Option<WallRow<'_>> {
    let body = reason
        .strip_prefix("the turn-end policy (wall: reset_unix=")
        .and_then(|r| r.strip_suffix(')'))?;
    let (unix, rest) = body.split_once("; reset=")?;
    let (head, message) = rest.split_once("; message=")?;
    let (reset, kind) = match head.split_once("; kind=") {
        Some((reset, name)) => (reset, kind_named(name)),
        None => (head, None),
    };
    Some(WallRow {
        reset_unix: unix.parse().ok(),
        reset: (reset != "-").then(|| reset.to_string()),
        kind,
        older: !head.contains("; kind="),
        message,
    })
}

/// [`open_wall_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn open_wall(path: &Path, sid: Option<&str>) -> Option<OpenWall> {
    open_wall_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// Lowercase hex SHA-256 of `s`.
fn sha256_hex(s: &str) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(s.as_bytes()))
}

/// The first `n` bytes of `s` on a character boundary.
fn cut(s: &str, n: usize) -> String {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {

    /// AN ACT'S ROW COUNTS ITS SLEEP ON A BOOT IN THE KERNEL'S SPELLING
    /// ([`BOOT`]: hyphenated, upper-case as macOS spells it, lower-case as
    /// Linux does) and a loop that follows reads the count back as the same
    /// 16 bytes, from a row of either spelling. A boot read off a tape (a
    /// replay's count) is written back exactly as it was spelled, so the act's
    /// row a replay writes is the recorded one, byte for byte, whichever host
    /// recorded it — and this host's boot is written as this host's kernel
    /// spells it, the text a build before the bytes wrote, which compares the
    /// text. NEGATIVE CONTROL: a row whose boot is the all-zero id a damaged
    /// answer reads as, or the name a test once wrote, holds no count (the act
    /// is still the last); a count that names no boot writes no count at all.
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
            // Either spelling is the one boot this loop counts on.
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

    /// The host's journal sits beside the session's ledger, and both are cut
    /// back past their bound. Negative control: a file within the bound is
    /// left byte for byte.
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

    /// A file past the 16 MiB the cut once read whole is cut too: it is
    /// streamed ([`held_rows`]). The journal is the shape of the 171 MB one
    /// an older build wrote on this Mac (ATERM-KI-1016-1: one `HOST seq=1
    /// upgrade step=wait:session-files-unreadable` line every ~540 ms for
    /// four days), past 16 MiB; the ledger holds a model switch no loop has
    /// undone, its row the oldest, under 16 MiB of ordinary approvals.
    /// Before, both were kept byte for byte.
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

    /// The ledger at `path` as a loop reads it at its start: a missing file
    /// is an empty ledger.
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

    /// [`typed_at`] reads the session's `typed` rows and nothing else: not
    /// another decision, not another session's, not a command that spells
    /// the fields (its quotes are escaped), and not `/login`'s success Enter,
    /// which the loop that pressed it counted no act (the review's F8: a
    /// successor spent the owner's budget on it). NEGATIVE CONTROL: the
    /// `/login` it answers is an act.
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

    /// THE `/login` A LOOP BEFORE THIS ONE TYPED, its answer still to come,
    /// is read as the session's last act: no other act after it, its success
    /// Enter included, and no turn read running. NEGATIVE CONTROLS: each of
    /// those after it; another session's `/login`; a ledger with none.
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
        // NEGATIVE CONTROLS.
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

    /// THE AT-WALL ROW'S KEY CARRIES THE EXCHANGE'S WORK (the merge review's
    /// F3): [`left_at_wall_key`] and [`parse_left_at_wall`] round-trip it in
    /// whole seconds; a row written before it was carried names the turn
    /// alone.
    #[test]
    fn a_left_at_wall_key_carries_the_exchanges_work() {
        use std::time::Duration;
        let key = left_at_wall_key("prompt:u-1", Duration::from_millis(361_900));
        assert_eq!(key, "wall:prompt:u-1@361");
        assert_eq!(
            parse_left_at_wall(&key),
            Some(("prompt:u-1", Some(Duration::from_secs(361))))
        );
        // A row written before the work was carried: the turn, no work.
        assert_eq!(
            parse_left_at_wall("wall:said:00ff00ff00ff00ff"),
            Some(("said:00ff00ff00ff00ff", None))
        );
        // NEGATIVE CONTROLS: a key that is no wall's; an `@` naming no
        // seconds is the turn's own.
        assert_eq!(parse_left_at_wall("answer:a-1@361"), None);
        assert_eq!(
            parse_left_at_wall("wall:answer:a@b"),
            Some(("answer:a@b", None))
        );
    }

    /// A LEFT EXCHANGE STAYS OPEN UNTIL THE LOOP TYPES (the review's
    /// RR3-TE-2): [`open_left`] is the key of the session's last
    /// `left-to-person@v1` row with no `typed` row of its after it — the
    /// accounts probe's `/usage` asks nothing and closes none. NEGATIVE
    /// CONTROLS: a continuation typed after it closes it; another session's
    /// rows, a row of another rule, and a missing file name none.
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
        // NEGATIVE CONTROLS.
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

    /// THE CARRY THE LAST ACT GAVE IS OVER (ATERM-KI-1016-1): after the act,
    /// the task's end, a person's exchange left to them, Off once the act's
    /// answer was judged, or a person's Esc standing at the point that judged
    /// that answer (the review's KR-P6-ESC). NEGATIVE CONTROLS, still
    /// carried: Off before the act's answer was judged (that answer gives it
    /// back), an Esc at a later point than the answer's or before the act, an
    /// act after the end, no act at all; the accounts probe's `/usage` after
    /// the end is no act.
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
        // NEGATIVE CONTROLS.
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

    /// A WALL'S RETRY KEEPS A PERSON'S EXCHANGE AT A WALL OPEN
    /// (ATERM-KI-1016-1): the row of an exchange at a wall (`wall:`) that
    /// the wall's retry was typed after is open still, keyed by the retry's
    /// words and the wall's row ([`left_retried_key`]) — and so after the
    /// usage resume and the API's return. NEGATIVE CONTROLS, closed: a
    /// continuation after the retry, a `/compact` (a command makes the task
    /// the policy's), and a retry after a row that is no wall's.
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
        // NEGATIVE CONTROLS.
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

    /// THE ACCOUNTS PROBE'S ROWS (`usage-probe@v1`): its `typed` row is no
    /// act of the turn-end policy's, so [`typed_at`] leaves it out — and
    /// [`typed_texts`] keeps it, a command row in a transcript being the
    /// harness's own. NEGATIVE CONTROL: a slash command the policy typed
    /// (`/compact`) is counted, and so is one whose TEXT is the probe's
    /// command under another rule.
    ///
    /// [`probe_owed`]: the last probe row `typed` is a panel that may still
    /// stand; a `waiting` row after it is the panel seen gone; a later
    /// `typed` owes it again. NEGATIVE CONTROLS: a ledger with no probe row
    /// owes nothing (every ledger of a machine with no roster), another
    /// session's row owes this one nothing, and a command that spells the
    /// rule's field forges nothing.
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

        // The budget's reader and the transcript's.
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

        // Owed: the last probe row is the typed one, whatever follows it
        // under another rule.
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

        // NEGATIVE CONTROLS: no probe row at all; the field spelled inside a
        // command (its quotes are escaped); no file.
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

    /// [`reset_owed`]: a grant row's `approved` row with no `waiting` row
    /// after it is a step owed, and says whether the confirmation's Enter
    /// was written since; the step's `waiting` row ends it, and a later row
    /// taken owes one again. No row of the reset's is a `typed` act. The
    /// Enter is read in BOTH wordings a build has written: each 2.1.304
    /// kind's `Yes, use my <kind>`, and an older aterm's `Yes, use my reset`
    /// (2.1.287–2.1.303), so a successor never takes an Enter an older build
    /// wrote for none. NEGATIVE CONTROLS: a ledger with no row of the rule
    /// owes nothing (every ledger written with the key off); a press that
    /// did not land (`skipped`, `deferred`) and a `declined` row open no
    /// step; another session's rows owe this one nothing; another rule's row
    /// on the same words, and a command that spells the rule's fields, forge
    /// nothing; a yes of another dialog's (`Yes, use my browser`) or one that
    /// only opens with a kind's is no Enter.
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

        // The row taken: owed, not entered — whatever follows it under
        // another rule, and through a `declined` row of its own.
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
        // Each yes a build has written is the Enter: every kind's, and the
        // older wording's.
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
        // CONTROL: a yes no build of the step writes is no Enter.
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
        // The Enter written: owed, entered — and the step is as old as its
        // ROW's taking, whatever was written since.
        write(&[taken(), entered().replace("\"ts\":1000,", "\"ts\":9000,")]);
        assert_eq!(
            reset_owed(&read(&path), Some("@s-1")),
            Some(ResetOpen {
                entered: true,
                taken_ms: 1_000
            })
        );
        // Ended: nothing owed; a row taken after it owes a new step, not
        // entered.
        write(&[taken(), entered(), ended()]);
        assert_eq!(owed(), None);
        write(&[taken(), entered(), ended(), taken()]);
        assert_eq!(owed(), Some(false));

        // NEGATIVE CONTROLS.
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

    /// [`typed_texts`] is every text the session's loop TYPED, with when, as
    /// written — a command that spells the fields included, read as the
    /// text it is — and nothing else: not another decision, not another
    /// session's. [`ledger_under`] names the same file [`path_under`] makes,
    /// and makes nothing.
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

    /// A seed's ledger for the readers below, in a test's own `dir` (a
    /// [`crate::supervise::ScratchDir`], gone when the test ends): `rows` of
    /// `@s-9`, each `(ts, rule, outcome, command, reason)`.
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

    /// A RECORD OF THE LOOP'S BETWEEN A TRY AND ITS FIRST READ IS NO READ
    /// (RS8-R1): Off stood, the task's end, a Land episode's end and the
    /// fallback asked leave the try unread, and the wall row after them is
    /// its first read, for the take window ([`awaited_try`]) and for the
    /// track's appearance ([`usage_attempt`]). NEGATIVE CONTROL: with no
    /// record between, the same read. Read as the first row, a record left
    /// both reads unplaced, and a successor judged the try met at the screen
    /// it was typed at.
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
            // Nothing read yet past the record: the try is unread still.
            rows.pop();
            let path = seed_ledger(&dir, &format!("first-read-{tag}-unread"), &rows);
            let a = awaited_try(&path, Some("@s-9")).expect("the try");
            assert!(a.unread && a.read_ms.is_none(), "{tag}: {a:?}");
        }
    }

    /// THE TRY A LOOP'S SEED FOUND AGED OUT IS OVER, AND THE WALL STANDS
    /// (RS8-R2, [`RULE_TRY_OVER`]): a done check typed in the limit's place
    /// and cut by the wall, a move refused at it, then the try-over record
    /// and the wall again — the try is no seed ([`usage_attempt`]), the cut
    /// check is owed its retry ([`done_check_owed`]), the latch stands
    /// ([`account_moves`]) and so does the wall ([`open_wall`]). The same
    /// for a land act ([`land_episode`]). NEGATIVE CONTROL: the wall's end
    /// row the record replaced ([`LIMIT_ENDED`]) ends all three.
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
                assert_eq!(account_moves(&path, sid).unmade, !ended, "{act_rule} {tag}");
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

    /// THE SHORT STREAK A SEED TAKES UP IS THE LEDGER'S LAST WORD ON IT
    /// (RS8-R3, RS8-F2): an act's mark, then the record of a point judged
    /// with nothing typed — that record's, at its time; a relaunch onto a
    /// model, which moves no streak, says nothing of it (RS8-F2's (d)); an
    /// act after the record — the act's again, its yield to be judged.
    /// NEGATIVE CONTROL: the acts alone read as before.
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
        // A point record of 0: the streak ended there.
        let path = seed_ledger(&dir, "streak-ended", &[act(1_000, 2), point(3_000, 0)]);
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.short, s.point), (0, true));
        // A turn read running after the point (RS9-F1): the next idle point
        // is its end; a point after it is the last word again, and a turn
        // read running after an act says nothing (its yield is that turn).
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
        // A ROW AN OLDER BUILD MARKED WITH THE JOB'S CHALLENGE (`the
        // open-items challenge was typed this job`, before the owner's rule
        // of 2026-10-19) still reads back its streak and its limit: the mark
        // itself seeds nothing.
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
        // A PERSON'S ESC AT THE POINT is the point record's mark alone
        // ([`StreakSeed::esc`]). NEGATIVE CONTROLS: a point unmarked, and an
        // act's row with the same words in its reason, mark none.
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

    /// THE WALL'S RETRIES (ATERM-KI-1016-1, "a loop that follows another")
    /// read back from the last row as they were written, an act's or a
    /// point's, for every API error's cause, status and retry rule, beside
    /// the streak's mark and before a limit's, which still reads back. The
    /// seed matrix holds a 529's to the first loop; this holds the words
    /// for the kinds it does not run. NEGATIVE CONTROLS: a row with no
    /// retry's wall marks none, and a garbled mark reads as none.
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
        // Beside a limit's mark, the limit's still reads back.
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

    /// A `/COMPACT`'S CONTINUATION THAT TYPES THE CUT ACT AGAIN KEEPS IT OWED
    /// TO A LOOP THAT FOLLOWS (ATERM-KI-1016-1, done-3,
    /// `TurnEndState::compact_owes`): the done check, a full context's wall
    /// that cut it, the `/compact`, its point, and the continuation typed
    /// under the compaction's rule with the check's words — the check is
    /// owed its answer, as for the loop that typed it; so too LAND's words
    /// keep the landing in hand. FAILS WITHOUT THE FIX: the continuation was
    /// a wall's retry under no cut, which owes nothing, and a successor read
    /// the done reply as a first one. NEGATIVE CONTROL: a continuation that
    /// types `keep going` owes no check and lands nothing.
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
            // The screen/ledger may wrap any version of the check's words.
            let wrapped = check.split_whitespace().collect::<Vec<_>>().join("\n");
            assert!(delivers_check(Some(&wrapped)));
        }
        let body = ledger(&rows((RULE_LAND, LAND), LAND));
        let ep = land_episode_in(&body, sid).expect("the episode");
        assert!(ep.landing && !ep.cut, "{ep:?}");
        // NEGATIVE CONTROL: `keep going` after the compaction.
        let body = ledger(&rows((RULE_DONE_CHECK, DONE_CHECK), "keep going"));
        assert_eq!(done_check_owed_in(&body, sid), None);
        assert!(!delivers_check(None));
        assert!(!delivers_check(Some("keep going")));
        let body = ledger(&rows((RULE_LAND, LAND), "keep going"));
        assert!(land_episode_in(&body, sid).is_none_or(|ep| !ep.landing));
    }

    /// AN ACT'S ANSWER JUDGED IS AWAITED NO MORE, AND ITS VERDICT IS READ
    /// (RS8-R4, RS8-F1): the done check, then the record of the point that
    /// judged its answer — owed no more where the task did not end there
    /// (its answer read as no done report), answered where it did, and
    /// nobody's to answer once a later point is judged; a wall that cut it
    /// keeps it owed to the retry. A try whose point was judged is awaited
    /// by no loop ([`awaited_try`]). So too a land act's answer
    /// ([`land_episode`]): judged and the task not ended there, no landing
    /// is in hand (RS9-F4); ended there, it is answered. NEGATIVE CONTROL:
    /// the check, the try and the land act with no record after it are
    /// owed, their take window open to a successor, the landing in hand.
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

    /// A CUT KEEPS WHAT THE READERS READ OF A RUN OF THE LOOP'S RECORDS
    /// ([`bound`], [`fold_records`]; the verifier of 75b03ac6d, RS9-R1). A
    /// tab's ledger past [`MAX_BYTES`]: older approvals, a model fallback
    /// owed its return at the bucket's reset three days on, a land episode
    /// (the land instruction and two nudges, each answered), then 1100 wakes
    /// — a turn read running and its point judged, each — and a last turn
    /// read running. Every reader says after the cut what it said before:
    /// the switch open, the land episode, the streak's last word and the
    /// turn after it, no check owed and no try awaited; of the run the last
    /// nudge's answer opens the cut keeps that first point, the last wake's
    /// and the turn after it.
    /// NEGATIVE CONTROL: the plain cut (`harness::cli::bound_ledger`) keeps
    /// the newest rows whole — every one a wake's — and the switch and the
    /// landing are gone.
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
        // The first two acts' answers (each a run of its own), then the run
        // the last nudge's answer opens: that first point, the last wake's
        // and the turn read running after it.
        assert_eq!(records.len(), 2 + 3, "{records:#?}");
        let ts = |l: &str| l.split(',').next().map(str::to_string);
        let at = |ms: i64| Some(format!("{{\"ts\":{ms}"));
        assert_eq!(ts(records[2]), at(t + 3_500));
        assert_eq!(
            ts(records[3]),
            at(t + 10_000 + 300_000 * i64::from(wakes) + 20_000)
        );
        assert!(records[4].contains(RULE_TURN_RAN), "{}", records[4]);
        // CONTROL: the plain cut.
        let path = seed_ledger(&dir, "whole-plain", &rows);
        crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
        let plain = read(&path);
        assert!(plain.0.is_none() && plain.1.is_none(), "CONTROL: {plain:?}");
    }

    /// A CUT KEEPS THE ROWS OF THE STATES THAT STAND FOR DAYS past a run of
    /// ACTS (ATERM-KI-1016-1, "a loop that follows another"): a model switch
    /// owed its return at the bucket's reset, and a Land episode — the land
    /// instruction and its nudges spent — then 1100 acts that end neither
    /// (an API error's retries, each with its point's record: two rows an
    /// act, which no fold folds). Every reader says after the cut what it
    /// said before. Cut to its newest rows, about 500 acts after the switch
    /// or the land instruction cut that row: a loop that follows did not
    /// switch the model back at the reset, and typed the land instruction
    /// anew. Past the states' rows the cut keeps its newest rows as before.
    /// And the cut still BOUNDS THE FILE while such a state stands for as long
    /// as the session runs so (the review of 3af6c3da7d, KRF-2): a Land
    /// episode under Autopilot Land, where no act of another mode is typed to
    /// end it, then 9000 approvals; a switch at a bucket whose notice named no
    /// reset, whose return is never due, then 4000 continuations each with its
    /// point's record. Kept from the state's row on, the first left 3.2 MB and
    /// the second 1.9 MB. So too where every row moves the episode and moves
    /// it back: 3000 of its questions answered under Land, each judged at its
    /// point, then a later turn's. NEGATIVE CONTROL: the plain cut loses the
    /// switch and the landing.
    #[test]
    fn a_cut_keeps_the_rows_of_the_states_that_stand_past_a_run_of_acts() {
        use super::super::policy::turn_end::{
            RULE_API_RETRY, RULE_CONTINUE, RULE_LAND, RULE_LAND_ANSWER, RULE_LAND_NUDGE,
            RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE,
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
        // The cut kept the switch's row onward, and none of the older rows.
        let first = kept.lines().next().expect("rows");
        assert!(first.contains(RULE_MODEL_FALLBACK), "{first}");
        // The Land episode alone: kept from its instruction on.
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
        // Past both states (the switch back made, a continuation that ends
        // the episode) the cut keeps its newest rows.
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
        // Its newest rows: KEEP_ROWS of them that are no record — 1024
        // acts, where counted with their records the cut kept 512.
        let cut = std::fs::read_to_string(&path).unwrap();
        let acts = cut.lines().filter(|l| record_of(l).is_none()).count();
        assert_eq!(acts, KEEP_ROWS, "{} rows", cut.lines().count());
        assert_eq!(read(&path), over);
        // The states stand for as long as the session runs so: the cut keeps
        // what their readers read of them, and the file within its bound.
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
        // Its questions answered under Land, each answer judged at its point:
        // the episode back where it stood after each, and every row a move;
        // then a later turn's point, and approvals.
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
        // CONTROL: the plain cut.
        let path = seed_ledger(&dir, "acts-plain", &rows);
        crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
        let plain = read(&path);
        assert!(plain.0.is_none() && plain.1.is_none(), "CONTROL: {plain:?}");
    }

    /// A TAB A COLD RESTORE REOPENED (ATERM-KI-1016-1, follow-7): its loops
    /// read the earlier tab's books and then its own while the take-up's
    /// row stands ([`RULE_EARLIER_BOOKS`], [`books_through`]). Past its
    /// bound, its ledger's cut keeps that row while a state the earlier
    /// books began stands (a model switch owed its return at the bucket's
    /// reset), as a continuous ledger's cut keeps the switch's row; with the
    /// switch back made, the cut drops it, and the earlier books are over,
    /// as a continuous ledger's cut drops their rows. CONTROL: the plain cut
    /// drops the take-up, and the switch with it.
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
            // As the seed reads them: through the earlier books while the
            // take-up stands.
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
        // The switch back made in the tab's own books: the earlier books
        // are over past the cut.
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
        // CONTROL: the plain cut.
        std::fs::write(&path, format!("{take_up}{}", approvals(t + 2_000))).expect("plain");
        crate::harness::cli::bound_ledger(&path, MAX_BYTES, KEEP_ROWS).expect("cut");
        assert_eq!(books(&path), (false, None), "CONTROL");
    }

    /// THE BOOKS THROUGH THE TABS A COLD RESTORE REOPENED A TAB FROM
    /// (ATERM-KI-1016-1, follow-7): each earlier tab's rows of its own
    /// session, re-signed with the tab's, the tabs oldest first whatever
    /// order the notes name them in, then the tab's own; a row of another
    /// session left out; a journal's lines alike. CONTROL: with no session,
    /// the tab's own books alone.
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

    /// A SECOND COLD RESTORE (A, then B, then C) READS NO STATE THE TAB
    /// BETWEEN ENDED BEFORE ITS CUT (the review of 3af6c3da7d, KRF-3): B's
    /// loop took A's books up and switched A's model back, then B's ledger
    /// passed its bound and its cut dropped the take-up's row and that switch
    /// back, as a continuous ledger's cut drops them. C's books are B's own
    /// from then on ([`crate::harness::upgrade_drive::earlier_ledgers_in`]
    /// stops at B). Walked on through B's notes to A, C read A's switch open
    /// again, and its loop relaunched the agent at the bucket's reset onto
    /// the model it ran. CONTROLS: a continuous ledger A+B, cut, holds no
    /// switch; with A's switch still standing, B's cut keeps the take-up and
    /// C reads A's switch through B.
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
        // A tab's books, as the seed reads them (`Session::seed_from_ledger_text`):
        // through the earlier tabs while the take-up stands.
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
        // CONTROL: the continuous ledger A+B, cut, holds no switch.
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
        // CONTROL: A's switch standing, B's cut keeps the take-up, and C
        // reads A's switch through B.
        std::fs::write(&bp, format!("{take_up}{approvals}")).expect("B's books, no switch back");
        bound(&bp);
        assert!(
            took_up_earlier(&std::fs::read_to_string(&bp).unwrap(), b),
            "CONTROL: kept"
        );
        assert_eq!(switched(c).as_deref(), Some("sonnet"), "CONTROL: through B");
    }

    /// Captured legacy wall rows take up the recorded try count through the
    /// public fold, including a count at the representable ceiling.
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

    /// Exercise the actual cut and its public state reader with captured
    /// rows. An unadmitted workspace keeps every row a standing walk reads,
    /// the take-up's, and the newest.
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
        // What the cut holds: the rows a walk reads or the take-up's, and the
        // newest KEEP_ROWS rows that are no record — every row a state that
        // stands reads.
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
        // Earlier ordinary rows do not move a standing state, but re-signing
        // each with a long current SID still expands the assembly. The same
        // captured source and allowance admit a short SID's ordinary cut.
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
