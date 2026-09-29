// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The approval ledger: one JSON object per line for every box the
//! supervisor's approval policy decided ([`super::policy::approval::decide`]),
//! kept for every `watch`, `supervise` and hosted run without a flag, at
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
//! reason the worker reads (a decline carried out to its Enter,
//! [`super::policy::approval::Decision::Decline`]; its `reason` is the text
//! typed); `typed` an act of the turn-end
//! policy the server took (its rule id is the policy's, its command the text
//! typed, `box_seq` the point's read); `skipped` a press it did not (the
//! guard matched no row, or the fenced screen moved); `escalated` a box no
//! rule approved; `deferred` a press the server turned away FOR NOW (`ERR
//! halted`, `ERR busy …` — another writer's turn — `ERR rate`), which the
//! loop tries again once the hold lifts or its back-off passes: a delivery
//! retry, never a policy's refusal (the live E2E of 2026-09-25 read two
//! `ERR busy turn=1` rows as `refused` boxes). The hash is of the full command, so a command
//! cut at 4 KiB is still identified. The file is opened through
//! [`super::journal::Journal`] (append-only, `0600`, one warning and the loop
//! goes on when it cannot be written).

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

/// The loop's JOURNAL beside a session's ledger: `<state>/drive/<sid>.jsonl`
/// → `<state>/drive/<sid>.journal.jsonl`. The in-GUI host keeps one for
/// every supervised session, so its `SKIPPED`, `WAITING`, `ESCALATED …
/// attention=<reply>` and `CLEARED` lines — journal-only — can be read (the
/// live E2E of 2026-09-24, D5: a refused attention and a guard that never
/// matched were visible nowhere).
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
/// [`MAX_BYTES`] (streamed, written to a sibling and renamed over it —
/// `harness::cli::bound_ledger`). Both files were append-only and never
/// pruned, for every session of a host that is on by default (the
/// reliability review of 2026-09-24); a failure to cut is no failure of the
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

/// THE TEXTS THE LOOP TYPED into `sid`'s session: the command of every
/// `typed` row of its in the ledger at `path`, once each, as the loop wrote
/// them — the turn-end policy's continuations (`continue_text` and the
/// standing rules riding in it), its answers (`answer_text`), the agent's
/// own suggestions it accepted, the wall's retries and its commands. The
/// harness's own turns, which make no conversation's task
/// (`crate::harness::upgrade::TaskScan`, D1 of the live E2E of 2026-09-26: a
/// `keep going` or an `answer_text` is the supervisor's, however the vendor
/// records it). A missing or unreadable file, and a row that does not
/// parse, know none.
#[must_use]
pub fn typed_texts(path: &Path, sid: Option<&str>) -> Vec<String> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let sid = sid.map(|s| s.trim_start_matches('@'));
    let mut out: Vec<String> = Vec::new();
    for line in body.lines().filter(|l| l.contains("\"typed\"")) {
        let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
            continue;
        };
        let field = |key: &str| v.get(key).and_then(aterm_json::Value::as_str);
        if field("sid") != sid || field("decision") != Some("typed") {
            continue;
        }
        if let Some(command) = field("command").filter(|c| !out.iter().any(|o| o == c)) {
            out.push(command.to_string());
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
pub fn typed_at(path: &Path, sid: Option<&str>) -> Vec<i64> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let sid = sid.map_or_else(
        || "null".to_string(),
        |s| json_str(s.trim_start_matches('@')),
    );
    let want_sid = format!(",\"sid\":{sid},");
    let mut out: Vec<i64> = body
        .lines()
        .filter(|l| l.contains(&want_sid) && l.contains(",\"decision\":\"typed\","))
        .filter_map(|l| {
            let rest = l.strip_prefix("{\"ts\":")?;
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect();
    out.sort_unstable();
    out
}

/// The reason a model fallback's row of the turn-end policy carries (`relaunch
/// --model <fallback>`; a `/model <fallback>` row before D7): the
/// switch as it must be undone — the bucket's model and its reset (unix
/// seconds), `-` for what is not known — so a loop that starts after it
/// ([`open_model_switch`]) switches back at the reset, as the loop that
/// switched would have. Owner decision 3's "back at reset" held in one
/// loop's memory, and every host restart (a policy edit, each seamless
/// update) between the switch and the reset left the session — and, `/model`
/// saving it, every new session — on the fallback for good (the reliability
/// review of 2026-09-24).
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
/// `model-fallback@v1` with no `model-restore@v1` row after it.
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
    use super::policy::turn_end::{RULE_MODEL_FALLBACK, RULE_MODEL_RESTORE};
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

/// A switch word's value on a ledger row: spaces as `_` (`extra high` →
/// `extra_high`), `-` for none.
fn switch_value(v: Option<&str>) -> String {
    match v.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => v.replace(' ', "_"),
        None => "-".to_string(),
    }
}

/// THE WORDS A CODEX SAVE-THEN-WAIT SWITCH'S ROWS CARRY
/// (`policy::turn_end::WindDown`): `(model switch: kind=wind-down
/// from=<model> effort=<effort> to=<model> back_at=<unix|-> marker=<m>
/// goal=<paused|-> pause=<typed|-> stops=<n> since=<unix|-> esc=<unix|->
/// opened=<unix|-> pressed=<unix|-> told=<keys|-> phase=<word>)` — on the
/// press's intent (`phase=intent`, and every row while the press is only
/// intended: `policy::turn_end::wind_phase_word`) and
/// approved rows, on every act and edge of it — so a loop that starts while
/// it is open carries it on ([`open_wind_down`]): whether the harness
/// stopped the goal (`goal`) and whether that stop was its typed `/goal
/// pause` rather than its Esc (`pause`), how many stops it spent (`stops`),
/// when the hold began (`since`: a restart never lengthens it), when its
/// last Esc went while that turn's point is still to come (`esc`: the
/// interrupt a restarted loop then sees is the harness's own, in any phase),
/// when the switch opened (`opened`: the floor under a person's keystrokes,
/// as the live loop keeps it), when it was pressed (`pressed`: the floor
/// under the thread's rollout), and the notes a person was told already
/// (`told=goal,unsaved`: never raised again by a restart). Claude Code's relaunch switch keeps its own two words
/// ([`model_switch_reason`]).
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
/// LAST row carrying [`wind_switch_words`], unless its phase is `done` or
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

/// Lowercase hex SHA-256 of `s`.
fn sha256_hex(s: &str) -> String {
    aterm_digest::Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
    /// back past their bound (the reliability review of 2026-09-24: append-only
    /// and never pruned). NEGATIVE CONTROL: a file within the bound is left
    /// byte for byte.
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

    /// [`typed_texts`] is every text the session's loop TYPED, once each, as
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
        assert_eq!(
            typed_texts(&path, Some("@s-1")),
            ["keep going", "Decide for yourself.", forged]
        );
        assert_eq!(typed_texts(&path, Some("s-2")), ["carry on"]);
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
