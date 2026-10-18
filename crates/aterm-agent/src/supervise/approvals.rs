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
        let sid = sid.map_or_else(
            || "null".to_string(),
            |s| json_str(s.trim_start_matches('@')),
        );
        format!(
            "{{\"ts\":{ts},\"sid\":{sid},\"rule_id\":{},\"decision\":{},\"cmd_sha256\":{},\
             \"command\":{},\"reason\":{},\"box_seq\":{}}}",
            json_str(self.rule_id),
            json_str(self.outcome.word()),
            json_str(&sha256_hex(self.command)),
            json_str(&cut(self.command, COMMAND_BYTES)),
            json_str(self.reason),
            self.box_seq
        )
    }
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

/// How many rows a cut keeps: the typing budget reads the last hour's
/// `typed` rows and an open model switch its last `/model` row — far fewer.
pub const KEEP_ROWS: usize = 1024;

/// Cut `path` back to its newest [`KEEP_ROWS`] rows once it passes
/// [`MAX_BYTES`] (streamed to a sibling and renamed over it, as
/// `harness::cli::bound_ledger` does), a run of the loop's records folded
/// to what its readers read of it ([`fold_records`]). A failure to cut is no
/// failure of the loop.
pub fn bound(path: &Path) {
    let _ =
        crate::harness::cli::bound_ledger_folding(path, MAX_BYTES, KEEP_ROWS, &mut fold_records);
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
/// stand for days — a landing spent, a model fallback owed its return at the
/// bucket's reset — and a loop that follows typed the land instruction anew
/// where the loop before it types nothing (the verifier of 75b03ac6d, RS9-R1).
fn fold_records(kept: &mut std::collections::VecDeque<String>, row: String) {
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
            (false, Some(false)) => return,
            // A point after a turn read running: the turn says nothing now.
            (true, Some(false)) => {
                kept.pop_back();
            }
            _ => {}
        }
        // A point after the run's first and a later one: it is the last now.
        if point && back(kept, 1) == Some(true) && back(kept, 2) == Some(true) {
            kept.pop_back();
        }
    }
    kept.push_back(row);
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
        if !self.journal.is_on() {
            return None;
        }
        let line = row.to_json(t_ms, self.sid.as_deref());
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
    let mut out: Vec<i64> = body
        .lines()
        .filter(|l| l.contains(&want_sid) && l.contains(",\"decision\":\"typed\","))
        .filter(|l| !(l.contains(&move_rule) && l.contains(&move_command)))
        .filter(|l| !l.contains(&probe_rule))
        .filter_map(|l| {
            let rest = l.strip_prefix("{\"ts\":")?;
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect();
    out.sort_unstable();
    out
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

/// THE PERSON'S OWN EXCHANGE AN EARLIER LOOP LEFT TO THEM (the review's
/// RR3-TE-2): the key (`command`) of `sid`'s last `skipped` row under
/// [`super::policy::turn_end::RULE_LEFT`] in the ledger text `body` — the
/// conversation's last turn as that loop read it
/// (`crate::harness::upgrade::last_turn_key`) — where no `typed` row of
/// `sid`'s came after it, save the accounts probe's `/usage`, which asks the
/// model nothing. `None`: no such row, one a typed row closed, a missing or
/// unreadable file.
#[must_use]
pub fn open_left_in(body: &str, sid: Option<&str>) -> Option<String> {
    use super::policy::turn_end::{RULE_LEFT, RULE_USAGE_PROBE};
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut open = None;
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
            (true, Some(RULE_USAGE_PROBE)) => {}
            (true, _) => open = None,
            (false, Some(RULE_LEFT)) => open = text("command").map(str::to_string),
            (false, _) => {}
        }
    }
    open
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

/// A limit reset's step the session ledger says is still open
/// ([`reset_owed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetOpen {
    /// The confirmation's Enter was written since the row was taken (an
    /// `approved` row on the vendor's yes).
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
    let yes = format!(
        ",\"command\":{},",
        json_str(aterm_phase::anchor("reset.yes"))
    );
    let mut owed: Option<ResetOpen> = None;
    for row in body
        .lines()
        .filter(|l| l.contains(&want_sid) && l.contains(&rule))
    {
        if row.contains(",\"decision\":\"waiting\",") {
            owed = None;
        } else if row.contains(",\"decision\":\"approved\",") {
            let entered = row.contains(&yes);
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
    /// ([`super::policy::turn_end::TurnEndState::seed_try_met`]). Read from
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
/// [`super::policy::turn_end::RULE_LIMIT_RESUME`] unmarked (0.104.0's rows,
/// and a continuation no limit's track stood behind): a usage window's first
/// try ([`USAGE_RESUME_UNMARKED`]) — the
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
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<UsageAttempt> = None;
    // A row of the session's came since the try ([`UsageAttempt::read_ms`]).
    let mut seen = false;
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
            _ => matches!(text("rule_id"), Some(RULE_WALL_WAIT | RULE_WALL_WAIT_END)),
        };
        let first = read && last.is_some() && !std::mem::replace(&mut seen, true);
        if text("decision") == Some("waiting") {
            match text("rule_id") {
                Some(RULE_WALL_WAIT_END)
                    if matches!(text("reason"), Some(LIMIT_ENDED | WALL_LEFT_NO_TASK)) =>
                {
                    last = None;
                }
                Some(RULE_TRY_OVER) => last = None,
                Some(RULE_WALL_WAIT_END) if text("reason") == Some(WALL_TAKEN) => {
                    if let Some(a) = last.as_mut() {
                        a.met_ms = None;
                    }
                }
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
        let Some(limit) = text("reason").and_then(limit_try_of).or_else(|| {
            (text("rule_id") == Some(super::policy::turn_end::RULE_LIMIT_RESUME))
                .then_some(USAGE_RESUME_UNMARKED)
        }) else {
            continue;
        };
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
    /// the open-items challenge ([`super::policy::turn_end::RULE_OPEN_ITEMS`]),
    /// whose done reply ends the task as the check's does and after which
    /// the job is challenged (`TurnEndState::seed_challenged`; the merge
    /// review's MG7-A3).
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
    /// ITS ANSWER WAS JUDGED: the task ended after it ([`RULE_TASK_DONE`]).
    pub answered: bool,
}

/// THE DONE CHECK `sid`'s LEDGER TEXT `body` SAYS IS OWED ITS ANSWER, as the
/// loop that asked it awaits that answer: the last act is
/// [`super::policy::turn_end::RULE_DONE_CHECK`] (or the open-items challenge,
/// [`super::policy::turn_end::RULE_OPEN_ITEMS`], the check's kin), or a wall's retry
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
        RULE_DONE_CHECK, RULE_OPEN_ITEMS, RULE_USAGE_PROBE, is_wall_retry,
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
                RULE_DONE_CHECK | RULE_OPEN_ITEMS => Some(CheckOwed {
                    rule: if rule == RULE_OPEN_ITEMS {
                        RULE_OPEN_ITEMS
                    } else {
                        RULE_DONE_CHECK
                    },
                    cut: false,
                    check: v.get("ts").and_then(aterm_json::Value::as_i64).map(|ts| {
                        let in_place = text("reason")
                            .and_then(limit_try_of)
                            .is_some_and(|l| l.in_place);
                        (ts, in_place)
                    }),
                    read_ms: None,
                    answered: false,
                }),
                r if is_wall_retry(r) && owed.is_some_and(|o| o.cut) => owed.map(|o| CheckOwed {
                    cut: false,
                    check: None,
                    read_ms: None,
                    answered: false,
                    ..o
                }),
                _ => None,
            };
            continue;
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
    /// wall's retry that delivers one a wall cut again): its done reply ends
    /// the task (`TurnEndState::landing`).
    pub landing: bool,
    /// A WALL ANSWERED THAT ACT, its row after it and no take after that
    /// ([`WALL_TAKEN`]: the screen it was typed at): the wall's retry
    /// delivers it again (`TurnEndState::cut_land`), as for the done check
    /// ([`CheckOwed::cut`]).
    pub cut: bool,
    /// That act's answer was judged: the task ended after it
    /// ([`RULE_TASK_DONE`]).
    pub answered: bool,
    /// THE EPISODE ENDED QUIET: its nudges spent
    /// ([`super::policy::turn_end::LAND_NUDGES`]) and the last land act's
    /// answer judged ([`RULE_POINT`], no wall cutting it) with no task's end
    /// there — as the loop that judged it holds it (`LandEpisode::quiet`,
    /// which the task's face reads as spent). A land act typed in a quiet
    /// episode (a question's land answer) leaves it quiet; its done reply
    /// ends the quiet. Unseeded, a loop that followed told the window `open`
    /// over a landing spent, where nothing more is typed.
    pub quiet: bool,
}

/// THE LAND EPISODE `sid`'s LEDGER TEXT `body` SAYS IS IN HAND, as the loop
/// that typed its acts holds it (`TurnEndState::land`): the last
/// [`super::policy::turn_end::RULE_LAND`] row with no end of the episode
/// after it — no act only another mode types (a continuation, the done
/// check, the reversible answer: [`super::policy::turn_end::is_continue_rule`]
/// and not one of Land's), no [`RULE_LAND_OVER`], no [`RULE_OFF_STOOD`] —
/// and the nudges typed since. `None` where no episode is in hand. Kept in
/// the loop's memory alone, every successor began the episode again: the
/// instruction typed where the first loop nudged, and a landing spent
/// landed again at each restart (the verifier of 341e1fa81, RS7-2).
#[must_use]
pub fn land_episode_in(body: &str, sid: Option<&str>) -> Option<LandSeed> {
    use super::policy::turn_end::{
        LAND_NUDGES, RULE_LAND, RULE_LAND_NUDGE, RULE_USAGE_PROBE, is_continue_rule, is_land_rule,
        is_wall_retry,
    };
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut ep: Option<LandSeed> = None;
    // The points judged since Land's last act, no wall cutting it
    // ([`RULE_POINT`], as [`done_check_owed`] counts them).
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
            let land = |nudges: u32, landing: bool, quiet: bool| LandSeed {
                nudges,
                landing,
                cut: false,
                answered: false,
                quiet,
            };
            ep = match (rule, ep) {
                (RULE_USAGE_PROBE, ep) => ep,
                (RULE_LAND, _) => Some(land(0, true, false)),
                (r, Some(e)) if is_land_rule(r) => Some(land(
                    e.nudges + u32::from(r == RULE_LAND_NUDGE),
                    true,
                    e.quiet,
                )),
                (r, _) if is_continue_rule(r) => None,
                (r, Some(e)) => Some(land(e.nudges, e.cut && is_wall_retry(r), e.quiet)),
                (_, None) => None,
            };
            continue;
        }
        let Some(e) = ep.as_mut() else {
            continue;
        };
        match rule {
            RULE_LAND_OVER | RULE_OFF_STOOD => ep = None,
            RULE_TASK_DONE => {
                e.answered = true;
                // The land act's done reply, at its own point: landed, so
                // the episode did not end quiet.
                if e.landing && judged == 1 {
                    e.quiet = false;
                }
            }
            // The act's answer judged; a second point is a later turn's.
            RULE_POINT if e.landing && !e.cut => {
                judged += 1;
                if judged == 1 {
                    // Its answer, no done yet: quiet where the nudges are
                    // spent (a `task-done@v1` at this point takes it back).
                    e.quiet = e.nudges >= LAND_NUDGES;
                }
                if judged > 1 {
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
    // Its answer judged and the task not ended there: no landing in hand
    // ([`done_check_owed`]'s RS8-R4).
    ep.map(|e| LandSeed {
        landing: e.landing && (judged == 0 || e.answered),
        ..e
    })
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
    /// after that point. Else the streak an act was typed after, its yield
    /// still to be judged (a loop records every point that judges an act's
    /// answer).
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
    /// THE JOB HAD THE OPEN-ITEMS CHALLENGE as of the row
    /// ([`Carried::challenged`], [`CHALLENGED_JOB`]).
    pub challenged: bool,
    /// A PERSON'S ESC STOOD AT THE POINT RECORDED ([`PERSONS_ESC`], a point
    /// record's alone): a loop that follows, reading that point again, holds
    /// for the person's grace from the record, as the loop that judged it
    /// does, not from its own attach (`TurnEndState::seed_esc`).
    pub esc: bool,
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
/// 97daeeace, RS8-R3, RS8-F2, RS8-F3). The same row says whether the job
/// had the open-items challenge ([`StreakSeed::challenged`]).
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
            if let Some(l) = last.as_mut().filter(|l| l.point) {
                l.ran = true;
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
        let challenged = text("reason").is_some_and(|r| r.contains(&format!("({CHALLENGED_JOB})")));
        let esc = point && text("reason").is_some_and(|r| r.contains(&format!("({PERSONS_ESC})")));
        last = Some(StreakSeed {
            at_ms,
            short,
            point,
            rule: rule.to_string(),
            recorded: false,
            ran: false,
            challenged,
            esc,
        });
    }
    last.map(|l| StreakSeed { recorded, ..l })
}

/// [`short_streak_in`] over the ledger at `path`: `None` when it cannot be read.
#[must_use]
pub fn short_streak(path: &Path, sid: Option<&str>) -> Option<StreakSeed> {
    short_streak_in(&std::fs::read_to_string(path).ok()?, sid)
}

/// The rule id of the record of A TURN READ RUNNING, the first since the
/// point a loop recorded last ([`RULE_POINT`];
/// [`super::policy::turn_end::TurnEndState::take_ran`]): a loop that follows
/// reads its first idle point as that turn's end ([`StreakSeed::ran`]), as
/// the loop that read it running would have, not as the recorded point read
/// again. A record: no act.
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
/// (`after`: the short streak, and the job's challenge where it had one),
/// marked as an act's is ([`act_reason_after`]), and [`PERSONS_ESC`] where
/// `esc`: a person's Esc stood at the point.
#[must_use]
pub fn point_reason(after: Carried, esc: bool) -> String {
    format!(
        "the turn-end policy judged the point ({SHORT_STREAK}: {}){}{}",
        after.short,
        challenged_mark(after.challenged),
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
    /// THE JOB HAD THE OPEN-ITEMS CHALLENGE
    /// ([`super::policy::turn_end::RULE_OPEN_ITEMS`],
    /// `TurnEndState::challenged`): a FINISHED after it ends the task
    /// whatever it still lists ([`CHALLENGED_JOB`]).
    pub challenged: bool,
}

impl Carried {
    /// The short streak `short`, the job unchallenged.
    #[must_use]
    pub const fn streak(short: u32) -> Self {
        Self {
            short,
            challenged: false,
        }
    }
}

/// The words of the mark that THE JOB HAD THE OPEN-ITEMS CHALLENGE
/// ([`Carried::challenged`]): ` (the open-items challenge was typed this
/// job)`, after the short streak's mark. Only the challenge's own row said
/// it ([`done_check_owed`]), and only until its answer was judged: a loop
/// that followed past the answer — an act after it, an Off that lifted
/// over it cut — challenged the job a second time where the first loop
/// ended the task (the merge review's MG9-S3). A row an older build wrote
/// carries none, and seeds nothing.
const CHALLENGED_JOB: &str = "the open-items challenge was typed this job";

/// The mark of [`CHALLENGED_JOB`], or nothing for a job unchallenged.
fn challenged_mark(challenged: bool) -> String {
    if challenged {
        format!(" ({CHALLENGED_JOB})")
    } else {
        String::new()
    }
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
/// next short yield as the loop that typed it does, and the job's
/// challenge ([`CHALLENGED_JOB`]), so it ends the task at the job's next
/// FINISHED as that loop does ([`short_streak`]). None is marked for no
/// streak, and none for a job unchallenged.
#[must_use]
pub fn act_reason_after(after: Carried, limit: Option<LimitTry>) -> String {
    let mut reason = "the turn-end policy".to_string();
    reason.push_str(&short_mark(after.short));
    reason.push_str(&challenged_mark(after.challenged));
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
    use super::policy::turn_end::{RULE_ACCOUNT, RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE};
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut open: Option<OpenSwitch> = None;
    for line in body.lines() {
        let typed = line.contains(",\"decision\":\"typed\",");
        if !typed && !line.contains(FALLBACK_ASKED) {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        if !typed {
            if text("decision") == Some("waiting")
                && text("rule_id") == Some(RULE_MODEL_FALLBACK)
                && text("reason") == Some(FALLBACK_ASKED)
                && let Some(open) = &mut open
            {
                open.asked = true;
            }
            continue;
        }
        match text("rule_id") {
            Some(RULE_MODEL_RESTORE) => open = None,
            // An account move MADE (its own words, as [`account_moves`]
            // reads them — a draft submitted under the rule is none).
            Some(RULE_ACCOUNT)
                if text("command").is_some_and(|c| c.starts_with(ACCOUNT_MOVE_COMMAND))
                    && text("reason").is_some_and(|r| r.contains("account move:")) =>
            {
                open = None;
            }
            Some(RULE_MODEL_FALLBACK) => {
                let Some(to) = text("command").and_then(|c| {
                    c.strip_prefix("relaunch --model ")
                        .or_else(|| c.strip_prefix("/model "))
                }) else {
                    // The switch's own turn (a `/model` switch's `keep
                    // going`): asked on the fallback.
                    if let Some(open) = &mut open {
                        open.asked = true;
                    }
                    continue;
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
                    continue;
                }
                let written = v.get("ts").and_then(aterm_json::Value::as_i64).unwrap_or(0);
                open = Some(OpenSwitch {
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
                if let Some(open) = &mut open {
                    open.asked = true;
                }
            }
        }
    }
    open
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
/// (`TurnEndState::park_off`, [`OFF_STOOD`]). A record: no act.
pub const RULE_OFF_STOOD: &str = "autopilot-off@v1";

/// The rule id of the record that THE TASK ENDED AT A POINT a loop judged
/// (`Session::turn_end_execute`): the worker's done reply to the done check
/// or to a land act, or its own finish token — nothing is typed until
/// someone else's turn. A record: no act. A loop that follows reads the act
/// awaited before it as answered ([`CheckOwed::answered`],
/// [`LandSeed::answered`]): its answer's screen still ends the task, and a
/// turn read busy since is a later one, whose done report is a first one.
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
    let mut open = None;
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
        if typed {
            open = None;
            continue;
        }
        match text("rule_id") {
            Some(RULE_WALL_WAIT_END) => open = None,
            Some(RULE_WALL_WAIT) => {
                let Some(row) = text("reason").and_then(wall_wait_of) else {
                    continue;
                };
                open = Some(OpenWall {
                    at_ms: v.get("ts").and_then(aterm_json::Value::as_i64).unwrap_or(0),
                    message: row.message.to_string(),
                    reset: row.reset,
                    reset_unix: row.reset_unix,
                    kind: row.kind,
                });
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
    /// the fields (its quotes are escaped).
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
        let body = [
            row(Outcome::Typed, "keep going").to_json(3_000, Some("@s-1")),
            row(Outcome::Typed, "keep going").to_json(1_000, Some("@s-1")),
            row(Outcome::Escalated, forged).to_json(2_000, Some("@s-1")),
            row(Outcome::Typed, "keep going").to_json(4_000, Some("@s-2")),
            row(Outcome::Approved, "ls").to_json(5_000, Some("@s-1")),
        ]
        .join("\n");
        std::fs::write(&path, body).expect("ledger");
        assert_eq!(typed_at(&path, Some("@s-1")), [1_000, 3_000]);
        assert_eq!(typed_at(&path, Some("s-2")), [4_000]);
        assert!(typed_at(&dir.join("none.jsonl"), Some("@s-1")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
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
    /// taken owes one again. No row of the reset's is a `typed` act.
    /// NEGATIVE CONTROLS: a ledger with no row of the rule owes nothing
    /// (every ledger written with the key off); a press that did not land
    /// (`skipped`, `deferred`) and a `declined` row open no step; another
    /// session's rows owe this one nothing; another rule's row on the same
    /// words, and a command that spells the rule's fields, forge nothing.
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
        const GRANT: &str = "Use your reset now (2 left, until Oct 5)";
        const YES: &str = "Yes, use my reset";
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
                "Use your reset?",
                "focus",
            ),
        ]);
        assert_eq!(owed(), Some(false));
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
                "Use your reset?",
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
        // THE JOB'S CHALLENGE (the merge review's MG9-S3) is the last row's
        // mark, an act's or a point's, beside the streak's and before a
        // limit's, which reads back as it was written; a point that ended
        // the job after it marks none. NEGATIVE CONTROL: the unmarked rows
        // above, as an older build wrote them, say no challenge.
        assert!(!s.challenged, "an unmarked act");
        let limit = LimitTry {
            bucket: true,
            in_place: true,
            attempts: 2,
            entitlement: false,
        };
        let challenged = Carried {
            short: 2,
            challenged: true,
        };
        let marked = act_reason_after(challenged, Some(limit));
        assert_eq!(limit_try_of(&marked), Some(limit), "{marked}");
        let path = seed_ledger(
            &dir,
            "streak-challenged-act",
            &[
                act(1_000, 1),
                (2_000, RULE_CONTINUE, Outcome::Typed, "keep going", marked),
            ],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.short, s.challenged), (2_000, 2, true));
        let point_marked = |ts: i64| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(
                    Carried {
                        short: 0,
                        challenged: true,
                    },
                    false,
                ),
            )
        };
        let path = seed_ledger(
            &dir,
            "streak-challenged-point",
            &[act(1_000, 1), point_marked(3_000)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!(
            (s.at_ms, s.short, s.point, s.challenged),
            (3_000, 0, true, true)
        );
        let path = seed_ledger(
            &dir,
            "streak-challenge-over",
            &[point_marked(3_000), point(4_000, 0)],
        );
        let s = short_streak(&path, sid).expect("a streak");
        assert_eq!((s.at_ms, s.challenged), (4_000, false));
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

    /// A LANDING SPENT QUIET IS READ BACK QUIET ([`LandSeed::quiet`]; the
    /// merge of the ledger's land episode onto the task's face): the
    /// instruction and both nudges typed, the last nudge's answer judged with
    /// no task's end there — quiet, as the loop that judged it holds it; a
    /// question's land answer typed after keeps it so. NEGATIVE CONTROLS: the
    /// last nudge's done reply (`task-done@v1` at its point), its answer not
    /// judged yet, the nudges not spent, a wall cutting the last nudge, and
    /// the land answer's own done reply — none quiet.
    #[test]
    fn a_landing_spent_quiet_is_read_back_quiet() {
        use super::super::policy::turn_end::{RULE_LAND, RULE_LAND_ANSWER, RULE_LAND_NUDGE};
        let dir = crate::supervise::ScratchDir::new("apr-seed", "quiet");
        let sid = Some("@s-9");
        let act =
            |ts: i64, rule: &'static str| (ts, rule, Outcome::Typed, "(land)", act_reason(None));
        let point = |ts: i64| {
            (
                ts,
                RULE_POINT,
                Outcome::Waiting,
                "",
                point_reason(Carried::streak(0), false),
            )
        };
        let done = |ts: i64| {
            (
                ts,
                RULE_TASK_DONE,
                Outcome::Waiting,
                "",
                TASK_DONE.to_string(),
            )
        };
        let wall = |ts: i64| {
            (
                ts,
                RULE_WALL_WAIT,
                Outcome::Waiting,
                "",
                spend_wall_reason(),
            )
        };
        let spent = vec![
            act(1_000, RULE_LAND),
            point(2_000),
            act(3_000, RULE_LAND_NUDGE),
            point(4_000),
            act(5_000, RULE_LAND_NUDGE),
        ];
        let quiet = |name: &str, rows: &[(i64, &str, Outcome, &str, String)]| {
            land_episode(&seed_ledger(&dir, name, rows), sid)
                .expect("an episode")
                .quiet
        };
        let with = |more: &[(i64, &'static str, Outcome, &'static str, String)]| {
            let mut rows = spent.clone();
            rows.extend(more.iter().cloned());
            rows
        };
        assert!(quiet("quiet", &with(&[point(6_000)])));
        assert!(quiet(
            "quiet-answer",
            &with(&[point(6_000), act(7_000, RULE_LAND_ANSWER)])
        ));
        // NEGATIVE CONTROLS.
        assert!(!quiet("landed", &with(&[point(6_000), done(6_001)])));
        assert!(!quiet("unjudged", &spent));
        assert!(!quiet("unspent", &spent[..4]));
        assert!(!quiet("cut", &with(&[wall(5_500), point(6_000)])));
        assert!(!quiet(
            "answer-landed",
            &with(&[
                point(6_000),
                act(7_000, RULE_LAND_ANSWER),
                point(8_000),
                done(8_001),
            ])
        ));
    }

    /// A CUT KEEPS WHAT THE READERS READ OF A RUN OF THE LOOP'S RECORDS
    /// ([`bound`], [`fold_records`]; the verifier of 75b03ac6d, RS9-R1). A
    /// tab's ledger past [`MAX_BYTES`]: older approvals, a model fallback
    /// owed its return at the bucket's reset three days on, a landing spent
    /// (the land instruction and two nudges, each answered), then 1100 wakes
    /// — a turn read running and its point judged, each — and a last turn
    /// read running. Every reader says after the cut what it said before:
    /// the switch open, the landing spent, the streak's last word and the
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
        assert!(kept.lines().count() <= KEEP_ROWS);
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
}
