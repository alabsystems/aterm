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

use super::journal::{Journal, json_str, unix_ms};

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
/// `harness::cli::bound_ledger` does). A failure to cut is no failure of the
/// loop.
pub fn bound(path: &Path) {
    let _ = crate::harness::cli::bound_ledger(path, MAX_BYTES, KEEP_ROWS);
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

    /// Open `path` for a loop on `sid` (see [`Journal::open`]).
    pub fn open(path: Option<&Path>, sid: Option<&str>, warn: &mut dyn Write) -> Self {
        Self {
            journal: Journal::open(path, sid, warn),
            sid: sid.map(str::to_string),
        }
    }

    /// Append one row.
    pub fn write(&mut self, row: &Row<'_>, warn: &mut dyn Write) {
        let line = row.to_json(unix_ms(), self.sid.as_deref());
        self.journal.append_raw(&line, warn);
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

/// When each `typed` row of `sid`'s in the ledger at `path` was written
/// (`ts`, unix ms), oldest first: the turn-end policy's acts of earlier
/// loops on the session, which its typing budget counts
/// ([`super::policy::turn_end::TurnEndState::seed_acts`]). A missing or
/// unreadable file, and a row this build does not write, count nothing.
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
pub fn typed_at(path: &Path, sid: Option<&str>) -> Vec<i64> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
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

/// When each `typed` row of `sid`'s under `rule_id` in the ledger at `path`
/// was written (`ts`, unix ms), oldest first: the usage
/// act a previous loop on the session made, which a loop restarted at the
/// same wall works from
/// ([`super::policy::turn_end::TurnEndState::seed_usage_attempt`]). A missing
/// or unreadable file counts nothing.
pub fn typed_at_rule(path: &Path, sid: Option<&str>, rule_id: &str) -> Vec<i64> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let sid = sid.map_or_else(
        || "null".to_string(),
        |s| json_str(s.trim_start_matches('@')),
    );
    let want_sid = format!(",\"sid\":{sid},");
    let want_rule = format!(",\"rule_id\":{},", json_str(rule_id));
    let mut out: Vec<i64> = body
        .lines()
        .filter(|l| {
            l.contains(&want_sid)
                && l.contains(&want_rule)
                && l.contains(",\"decision\":\"typed\",")
        })
        .filter_map(|l| {
            let rest = l.strip_prefix("{\"ts\":")?;
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect();
    out.sort_unstable();
    out
}

/// WHEN THE USAGE-RESUME A LOOP RESTARTED AT A WALL TAKES UP WAS TYPED
/// (`ts`, unix ms; [`super::policy::turn_end::TurnEndState::seed_usage_attempt`]):
/// the last `typed` row of [`super::policy::turn_end::RULE_LIMIT_RESUME`] in
/// `sid`'s ledger at `path` — unless an account move MADE follows it
/// ([`AccountMoves::made_ms`]). That continuation was typed into the account
/// the session has LEFT: seeded, the new account's first limit was worked as
/// the retry of a wall it never met, its continuation backed off from an
/// act that was another account's.
#[must_use]
pub fn usage_attempt_at(path: &Path, sid: Option<&str>) -> Option<i64> {
    let typed = typed_at_rule(path, sid, super::policy::turn_end::RULE_LIMIT_RESUME)
        .into_iter()
        .max()?;
    let moved = account_moves(path, sid).made_ms.into_iter().max();
    moved.is_none_or(|moved| typed > moved).then_some(typed)
}

/// The reason a model fallback's row of the turn-end policy carries (`relaunch
/// --model <fallback>`, or an older `/model <fallback>` row): the switch as it
/// must be undone, the bucket's model and its reset (unix seconds, `-` when
/// unknown), so a loop that starts after it ([`open_model_switch`]) switches
/// back at the reset, as the loop that switched would have.
#[must_use]
pub fn model_switch_reason(from: Option<&str>, back_at_unix: Option<i64>) -> String {
    format!(
        "the turn-end policy (model switch: from={} back_at={})",
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
}

/// The switch `sid`'s ledger at `path` holds open ([`OpenSwitch`]), read
/// from [`model_switch_reason`]'s words; `None` when there is none, the
/// file cannot be read, or the row predates the words.
#[must_use]
pub fn open_model_switch(path: &Path, sid: Option<&str>) -> Option<OpenSwitch> {
    use super::policy::turn_end::{RULE_ACCOUNT, RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE};
    let body = std::fs::read_to_string(path).ok()?;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut open = None;
    for line in body.lines() {
        if !line.contains(",\"decision\":\"typed\",") {
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
                open = Some(OpenSwitch {
                    from: field("from"),
                    to: to.trim().to_string(),
                    back_at_unix: field("back_at").and_then(|w| w.parse().ok()),
                });
            }
            _ => {}
        }
    }
    open
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
/// to be served nor tells a bad answer a second time.
#[must_use]
pub fn account_answer_reason(to: &str, answer: super::policy::turn_end::AccountAnswer) -> String {
    use super::policy::turn_end::{AccountAnswer, AccountBad};
    let by = match answer {
        AccountAnswer::Served => "served",
        AccountAnswer::Bad(AccountBad::Wall) => "wall",
        AccountAnswer::Bad(AccountBad::SignedOut) => "signed-out",
    };
    format!("the turn-end policy (account move answered: to={to} by={by})")
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

/// [`AccountMoves`] of `sid`'s ledger at `path`, read from
/// [`account_switch_reason`]'s, [`account_unmade_reason`]'s and
/// [`account_answer_reason`]'s words. A
/// missing or unreadable file, and a session that never moved, know none.
/// A draft submitted under the rule (its command is the draft's text, not
/// [`ACCOUNT_MOVE_COMMAND`]) is no move.
#[must_use]
pub fn account_moves(path: &Path, sid: Option<&str>) -> AccountMoves {
    use super::policy::turn_end::{AccountAt, RULE_ACCOUNT};
    let mut out = AccountMoves::default();
    let Ok(body) = std::fs::read_to_string(path) else {
        return out;
    };
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
        });
    }
    out
}

/// The account move `sid`'s ledger at `path` holds open
/// ([`AccountMoves::open`]): beside [`open_model_switch`], for the same
/// reason — a loop that starts after the move still has it to answer.
#[must_use]
pub fn open_account_move(path: &Path, sid: Option<&str>) -> Option<OpenAccountMove> {
    account_moves(path, sid).open
}

/// A switch word's value on a ledger row: spaces as `_` (`extra high` →
/// `extra_high`), `-` for none.
fn switch_value(v: Option<&str>) -> String {
    match v.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => v.replace(' ', "_"),
        None => "-".to_string(),
    }
}

/// The words a Codex save-then-wait switch's rows carry
/// (`policy::turn_end::WindDown`): `(model switch: kind=wind-down
/// from=<model> effort=<effort> to=<model> back_at=<unix|-> marker=<m>
/// goal=<paused|-> pause=<typed|-> stops=<n> since=<unix|-> esc=<unix|->
/// opened=<unix|-> pressed=<unix|-> told=<keys|-> phase=<word>)`, on the
/// press's intent (`policy::turn_end::wind_phase_word`), approved rows and
/// every act and edge, so a loop that starts while it is open carries it on
/// ([`open_wind_down`]): whether and how the harness stopped the goal, the
/// stops spent, when the hold began, the last Esc still awaiting its point,
/// the floors under a person's keystrokes and the rollout, and the notes a
/// person was already told. Claude Code's relaunch switch keeps its own two
/// words ([`model_switch_reason`]).
#[must_use]
pub fn wind_switch_words(
    w: &super::policy::turn_end::WindDown,
    phase: &str,
    back_at_unix: Option<i64>,
    since_unix: Option<i64>,
    esc_unix: Option<i64>,
    opened_unix: Option<i64>,
    pressed_unix: Option<i64>,
) -> String {
    let unix = |u: Option<i64>| u.map_or_else(|| "-".to_string(), |s| s.to_string());
    let told = if w.said.is_empty() {
        "-".to_string()
    } else {
        w.said.join(",")
    };
    format!(
        "(model switch: kind=wind-down from={} effort={} to={} back_at={} marker={} goal={} \
         pause={} stops={} since={} esc={} opened={} pressed={} told={told} phase={phase})",
        switch_value(Some(&w.from.model)),
        switch_value(w.from.effort.as_deref()),
        switch_value(Some(&w.to)),
        unix(back_at_unix),
        switch_value(Some(&w.marker)),
        if w.goal_paused { "paused" } else { "-" },
        if w.pause_typed { "typed" } else { "-" },
        w.stops,
        unix(since_unix),
        unix(esc_unix),
        unix(opened_unix),
        unix(pressed_unix),
    )
}

/// A Codex save-then-wait switch a loop on the session opened and no row
/// closed ([`open_wind_down`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWind {
    pub from: super::policy::turn_end::CodexSetting,
    pub to: String,
    pub back_at_unix: Option<i64>,
    pub marker: String,
    pub goal_paused: bool,
    /// The harness's stop was its typed `/goal pause` (`pause=typed`).
    pub pause_typed: bool,
    /// The harness's stops of Codex's goal so far (`stops=`).
    pub stops: u32,
    /// When the hold began (`since=`, epoch seconds).
    pub since_unix: Option<i64>,
    /// When the harness's last Esc went, its point still to come (`esc=`,
    /// epoch seconds).
    pub esc_unix: Option<i64>,
    /// When the switch opened (`opened=`, epoch seconds).
    pub opened_unix: Option<i64>,
    /// When its switch was pressed (`pressed=`, epoch seconds).
    pub pressed_unix: Option<i64>,
    /// The notes a person was told already (`told=`).
    pub told: Vec<String>,
    /// When its last row was written (the row's `ts`, epoch milliseconds):
    /// a person's keystroke since is their hand in whatever runs when a loop
    /// carries the switch on mid-turn.
    pub row_ms: Option<i64>,
    /// The phase its last row left it in
    /// (`intent|owed|winding|restore|restore-free|holding`).
    pub phase: String,
}

impl OpenWind {
    /// The switch to carry on, its times — the reset, the hold's start, the
    /// harness's last Esc, the switch's opening and its press, and the last
    /// row's time — put on the new loop's clock by `clock` (epoch seconds to
    /// an instant; a hold whose start no row names counts from `now`; a
    /// switch whose opening no row names opened, as far as a person's
    /// keystrokes go, at its last row; one whose press no row names was
    /// pressed, as far as the thread's rollout goes, at its opening). A
    /// press only intended (`intent`) is carried on as owed, and as only
    /// intended ([`WindDown::intent`]).
    ///
    /// [`WindDown::intent`]: super::policy::turn_end::WindDown::intent
    #[must_use]
    pub fn into_wind(
        self,
        clock: impl Fn(i64) -> std::time::Instant,
        now: std::time::Instant,
    ) -> super::policy::turn_end::WindDown {
        use super::policy::turn_end::{WindDown, WindPhase, said_key};
        let back_at = self.back_at_unix.map(&clock);
        let since = self.since_unix.map(&clock);
        let esc = self.esc_unix.map(&clock);
        let opened = self.opened_unix.map(&clock);
        let pressed = self.pressed_unix.map(&clock);
        let row_at = self.row_ms.map(|ms| clock(ms.div_euclid(1000)));
        let phase = match self.phase.as_str() {
            "owed" | "intent" => WindPhase::Owed,
            "winding" => WindPhase::Winding,
            "restore-free" => WindPhase::Restore { hold: false },
            "holding" => WindPhase::Holding {
                since: since.unwrap_or(now),
            },
            _ => WindPhase::Restore { hold: true },
        };
        WindDown {
            from: self.from,
            to: self.to,
            back_at,
            phase,
            goal_paused: self.goal_paused,
            pause_typed: self.pause_typed,
            saved: None,
            marker: self.marker,
            restore_tries: 0,
            restore_at: None,
            said: self.told.iter().filter_map(|k| said_key(k)).collect(),
            pre: false,
            seeded: false,
            stops: self.stops,
            stop_at: esc,
            restore_shown: false,
            picker_gone_at: None,
            intent: self.phase == "intent",
            footer_since: None,
            seeded_at: row_at,
            opened_at: opened.or(row_at),
            pressed_at: pressed.or(opened).or(row_at),
            wound: std::time::Duration::ZERO,
            overran: false,
        }
    }
}

/// The Codex save-then-wait switch `sid`'s ledger at `path` holds open: the
/// last row carrying [`wind_switch_words`], unless its phase is `done` or
/// `released` — a press's intent row (`intent`) included, as only intended.
/// `None` when there is none, or the file cannot be read. A row written
/// before `pause=`, `stops=`, `since=`, `esc=`, `opened=`, `pressed=` and
/// `told=` were carried reads them as `-`, 0, `-`, `-`, `-`, `-` and none.
#[must_use]
pub fn open_wind_down(path: &Path, sid: Option<&str>) -> Option<OpenWind> {
    let body = std::fs::read_to_string(path).ok()?;
    let want = sid.map(|s| s.trim_start_matches('@').to_string());
    let mut last: Option<(String, Option<i64>)> = None;
    for line in body.lines() {
        if !line.contains("model switch: kind=wind-down") {
            continue;
        }
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
        if text("sid").map(str::to_string) != want {
            continue;
        }
        if let Some(reason) = text("reason") {
            let ts = v.get("ts").and_then(aterm_json::Value::as_i64);
            last = Some((reason.to_string(), ts));
        }
    }
    let (reason, row_ms) = last?;
    let field = |name: &str| {
        let at = reason.find(&format!(" {name}="))? + name.len() + 2;
        let word: String = reason[at..]
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != ')')
            .collect();
        (word != "-" && !word.is_empty()).then(|| word.replace('_', " "))
    };
    let phase = field("phase")?;
    if matches!(phase.as_str(), "done" | "released") {
        return None;
    }
    Some(OpenWind {
        from: super::policy::turn_end::CodexSetting {
            model: field("from")?,
            effort: field("effort"),
        },
        to: field("to")?,
        back_at_unix: field("back_at").and_then(|w| w.parse().ok()),
        marker: field("marker")?.replace(' ', "_"),
        goal_paused: field("goal").as_deref() == Some("paused"),
        pause_typed: field("pause").as_deref() == Some("typed"),
        stops: field("stops").and_then(|w| w.parse().ok()).unwrap_or(0),
        since_unix: field("since").and_then(|w| w.parse().ok()),
        esc_unix: field("esc").and_then(|w| w.parse().ok()),
        opened_unix: field("opened").and_then(|w| w.parse().ok()),
        pressed_unix: field("pressed").and_then(|w| w.parse().ok()),
        told: field("told")
            .map(|w| w.split(',').map(str::to_string).collect())
            .unwrap_or_default(),
        row_ms,
        phase,
    })
}

/// The rule id of the ledger row that records a wall the loop waits on
/// ([`super::wall_memory`]).
pub const RULE_WALL_WAIT: &str = "wall-wait@v1";

/// The rule id of the row that ends it: the worker worked, the screen left the
/// wall, or another wall replaced it.
pub const RULE_WALL_WAIT_END: &str = "wall-wait-end@v1";

/// The reason a [`RULE_WALL_WAIT`] row carries: the wall the loop waits on,
/// as a loop that starts after it ([`open_wall`]) needs it to go on waiting
/// when the wall's rows are not on the screen — the reset PLACED (unix
/// seconds, which a loop with only the words could not do without the print),
/// the reset as the notice words it, and the notice. The notice comes last:
/// it is free text.
#[must_use]
pub fn wall_wait_reason(reset_unix: Option<i64>, reset: Option<&str>, message: &str) -> String {
    format!(
        "the turn-end policy (wall: reset_unix={}; reset={}; message={})",
        reset_unix.map_or_else(|| "-".to_string(), |s| s.to_string()),
        reset.unwrap_or("-"),
        message.replace(['\n', '\r'], " ")
    )
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
}

/// The wall `sid`'s ledger at `path` holds open: the last [`RULE_WALL_WAIT`]
/// row with no [`RULE_WALL_WAIT_END`] row after it and no `typed` row of any
/// rule after it (an act on the wall is the loop's own record of what became
/// of it; the wall it met again is written again, at its new print). `None`
/// when there is none or the file cannot be read.
#[must_use]
pub fn open_wall(path: &Path, sid: Option<&str>) -> Option<OpenWall> {
    let body = std::fs::read_to_string(path).ok()?;
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
                let reason = text("reason").unwrap_or("");
                let Some(body) = reason
                    .strip_prefix("the turn-end policy (wall: reset_unix=")
                    .and_then(|r| r.strip_suffix(')'))
                else {
                    continue;
                };
                let Some((unix, rest)) = body.split_once("; reset=") else {
                    continue;
                };
                let Some((reset, message)) = rest.split_once("; message=") else {
                    continue;
                };
                open = Some(OpenWall {
                    at_ms: v.get("ts").and_then(aterm_json::Value::as_i64).unwrap_or(0),
                    message: message.to_string(),
                    reset: (reset != "-").then(|| reset.to_string()),
                    reset_unix: unix.parse().ok(),
                });
            }
            _ => {}
        }
    }
    open
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
        // The known vector: sha256("abc").
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
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

    /// [`typed_at_rule`] reads one rule's `typed` rows of the session, oldest
    /// first, and no other: not another rule's, not another decision's, not
    /// another session's, not a command that spells the fields.
    #[test]
    fn typed_at_rule_reads_only_that_rule() {
        let dir = std::env::temp_dir().join(format!("aterm-typed-rule-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("s-1.jsonl");
        let row = |rule_id, outcome, command: &'static str| Row {
            rule_id,
            outcome,
            command,
            reason: "the turn-end policy",
            box_seq: 1,
        };
        let forged = ",\"rule_id\":\"usage-resume@v1\",\"decision\":\"typed\",";
        let body = [
            row("usage-resume@v1", Outcome::Typed, "keep going").to_json(3_000, Some("@s-1")),
            row("usage-resume@v1", Outcome::Typed, "keep going").to_json(1_000, Some("@s-1")),
            row("continue@v1", Outcome::Typed, "keep going").to_json(2_000, Some("@s-1")),
            row("usage-resume@v1", Outcome::Waiting, "-").to_json(2_500, Some("@s-1")),
            row("continue@v1", Outcome::Typed, forged).to_json(3_500, Some("@s-1")),
            row("usage-resume@v1", Outcome::Typed, "keep going").to_json(4_000, Some("@s-2")),
        ]
        .join("\n");
        std::fs::write(&path, body).expect("ledger");
        assert_eq!(
            typed_at_rule(&path, Some("@s-1"), "usage-resume@v1"),
            [1_000, 3_000]
        );
        assert_eq!(
            typed_at_rule(&path, Some("s-1"), "continue@v1"),
            [2_000, 3_500]
        );
        assert_eq!(
            typed_at_rule(&path, Some("s-2"), "usage-resume@v1"),
            [4_000]
        );
        assert!(typed_at_rule(&dir.join("none.jsonl"), Some("@s-1"), "usage-resume@v1").is_empty());
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
}
