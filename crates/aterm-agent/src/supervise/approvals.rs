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
//!  "decision":"approved|skipped|escalated|refused",
//!  "cmd_sha256":"<hex of the whole command>","command":"<the command, cut at 4 KiB>",
//!  "reason":"<the rule's reason or the classifier's>","box_seq":<n>}
//! ```
//!
//! `approved` is a press the server wrote; `typed` an act of the turn-end
//! policy the server took (its rule id is the policy's, its command the text
//! typed, `box_seq` the point's read); `skipped` a press it did not (the
//! guard matched no row, or the fenced screen moved); `escalated` a box no
//! rule approved; `refused` a press the server turned away (`ERR halted`,
//! `ERR busy …`, `ERR rate`). The hash is of the full command, so a command
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
    /// The turn-end policy typed its act (a continuation, a command).
    Typed,
    Skipped,
    Escalated,
    Refused,
}

impl Outcome {
    pub fn word(self) -> &'static str {
        match self {
            Outcome::Approved => "approved",
            Outcome::Typed => "typed",
            Outcome::Skipped => "skipped",
            Outcome::Escalated => "escalated",
            Outcome::Refused => "refused",
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
    let dir = state.join("drive");
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(&dir).ok()?;
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
    Some(dir.join(format!("{name}.jsonl")))
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

/// The reason a `/model <fallback>` row of the turn-end policy carries: the
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
/// last `typed` `/model …` row of `model-fallback@v1` with no
/// `model-restore@v1` row after it.
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
                let Some(to) = text("command").and_then(|c| c.strip_prefix("/model ")) else {
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
