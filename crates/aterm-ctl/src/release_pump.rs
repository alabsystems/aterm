// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release pump's survey fact, `pump=`: what the pump on a host (`ship release --pump`) last
//! read and decided, who holds the line it reads, and how long ago its last cycle was.
//!
//! Each pump writes its own record ([`Record`]) in the harness state folder's [`DIRECTORY`], one
//! file per clone (a clone runs one pump), at each cycle and while it attends a verb's run, so a
//! live pump's record is at most about a minute old and no other pump on the host (a `--once` of
//! another clone, a second clone's pump) overwrites it. A survey answers each record as one
//! `release_pump` row ([`Record::row`]): the record with the age the answering host measured and
//! its pid judged by that host's kernel. A reader parses each row ([`Pump::parse`]). An aterm
//! older than the row sends none, and a reader takes a reply without one as unknown, never as a
//! host with no pump: a dead or stalled pump is told by its age and its process, never by
//! silence.

use aterm_json::{Value, json};

/// The folder of the pumps' records, in the harness state folder beside
/// [`crate::release_holder::DIRECTORY`]: one `*.json` per clone, named by the pump.
pub const DIRECTORY: &str = "release-pumps";

/// The most records a survey answers: the newest, so a host where many clones once ran a pump
/// still answers in a line each.
pub const MAX_RECORDS: usize = 16;

/// The survey row that carries it.
pub const ROW: &str = "release_pump ";

/// The most bytes a record may hold: a few lines of words, never a log.
pub const MAX_RECORD_BYTES: u64 = 16 * 1024;

/// The most characters of the decision a row carries, so a row stays a line.
const DECISION_CHARS: usize = 600;

/// The pump's record of its last cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// The fleet line's holder, as its claim names them; empty for none.
    pub holder: String,
    /// The line: `v<version>@<head>`, else `v<version>`.
    pub line: String,
    /// What the pump does: the verb it runs or `wait`.
    pub step: String,
    /// The pump's clone holds an unfinished journal of the line: the line is this pump's between
    /// its runs, whose release process (the claim's holder) is not running then.
    pub own: bool,
    /// Unix seconds of its last cycle, or of the last look at the verb it attends.
    pub at: u64,
    /// The cycle's decision, as the pump said it.
    pub decision: String,
    /// The pump's process: its pid, and where the kernel said them, its boot and start.
    pub pid: u32,
    pub boot: Option<String>,
    pub start: Option<u64>,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key].as_str().map(str::to_owned)
}

fn cut(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

impl Record {
    /// The record as the pump writes it.
    #[must_use]
    pub fn json(&self) -> String {
        json!({
            "holder": self.holder, "line": self.line, "step": self.step, "own": self.own,
            "at": self.at, "decision": self.decision, "pid": self.pid, "boot": self.boot,
            "start": self.start
        })
        .to_string()
    }

    /// A record [`Record::json`] wrote; `None` for anything else.
    #[must_use]
    pub fn from_json(bytes: &str) -> Option<Self> {
        let value: Value = aterm_json::from_str(bytes).ok()?;
        Some(Self {
            holder: text(&value, "holder")?,
            line: text(&value, "line")?,
            step: text(&value, "step")?,
            own: value["own"].as_bool()?,
            at: value["at"].as_u64()?,
            decision: text(&value, "decision")?,
            pid: u32::try_from(value["pid"].as_u64()?).ok()?,
            boot: text(&value, "boot"),
            start: value["start"].as_u64(),
        })
    }

    /// The survey row of this record at `now` on the answering host, `alive` its pid as that
    /// host's kernel judged it (`None`: not said). The age is the answering host's own
    /// difference, so no reader compares two clocks.
    #[must_use]
    pub fn row(&self, alive: Option<bool>, now: u64) -> String {
        let word = |text: &str| {
            if text.is_empty() {
                "-".to_owned()
            } else {
                aterm_codec::pct::encode(text)
            }
        };
        format!(
            "{ROW}alive={} pid={} holder={} line={} step={} own={} at={} age={} decision={}",
            alive.map_or("-", |alive| if alive { "1" } else { "0" }),
            self.pid,
            word(&self.holder),
            word(&self.line),
            word(&self.step),
            u8::from(self.own),
            self.at,
            now.saturating_sub(self.at),
            word(&cut(&self.decision, DECISION_CHARS)),
        )
    }
}

/// One `release_pump` row as a reader holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pump {
    /// Whether its pid runs, as the answering host's kernel said; `None` where it did not.
    pub alive: Option<bool>,
    /// The pump's process on the answering host: a reader on that host tells its own pump's row
    /// from another's by it.
    pub pid: u32,
    /// The line's holder; `None` for none.
    pub holder: Option<String>,
    pub line: String,
    pub step: String,
    /// Whether the pump's clone holds an unfinished journal of the line ([`Record::own`]);
    /// `None` from a row that does not say.
    pub own: Option<bool>,
    /// Unix seconds of the pump's last cycle, on the answering host's clock.
    pub at: u64,
    /// How long before the answer that was, as the answering host measured it.
    pub age_s: u64,
    pub decision: String,
}

impl Pump {
    /// The row [`Record::row`] wrote; `None` for anything else, a row with a field twice
    /// included.
    #[must_use]
    pub fn parse(row: &str) -> Option<Self> {
        let words = row.strip_prefix(ROW)?;
        if words.len() > 4 * usize::try_from(MAX_RECORD_BYTES).ok()? {
            return None;
        }
        let mut fields: Vec<(&str, String)> = Vec::new();
        for word in words.split_ascii_whitespace() {
            let (key, value) = word.split_once('=')?;
            if fields.iter().any(|(seen, _)| *seen == key) {
                return None;
            }
            fields.push((key, aterm_codec::pct::decode(value)));
        }
        let get = |key: &str| {
            fields
                .iter()
                .find(|(seen, _)| *seen == key)
                .map(|(_, value)| value.clone())
        };
        let word = |key: &str| get(key).filter(|value| value != "-");
        Some(Self {
            alive: match get("alive")?.as_str() {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            },
            pid: get("pid")?.parse().ok()?,
            holder: word("holder"),
            line: get("line")?,
            step: get("step")?,
            own: match get("own").as_deref() {
                Some("1") => Some(true),
                Some("0") => Some(false),
                _ => None,
            },
            at: get("at")?.parse().ok()?,
            age_s: get("age")?.parse().ok()?,
            decision: word("decision").unwrap_or_default(),
        })
    }

    /// The row as a survey's text shows it, one line.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "  pump={} line={} holder={} age={}s alive={} pid={} last={}",
            self.step,
            self.line,
            self.holder.as_deref().unwrap_or("-"),
            self.age_s,
            match self.alive {
                Some(true) => "live",
                Some(false) => "dead",
                None => "unknown",
            },
            self.pid,
            self.decision
        )
    }

    /// The row as a survey's JSON shows it.
    #[must_use]
    pub fn json(&self) -> String {
        json!({
            "alive": self.alive, "pid": self.pid, "holder": self.holder, "line": self.line,
            "step": self.step, "own": self.own, "at": self.at, "age_s": self.age_s,
            "decision": self.decision
        })
        .to_string()
    }

    /// The pump a survey's JSON shows ([`Pump::json`]); `None` for anything else.
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        Some(Self {
            alive: value["alive"].as_bool(),
            pid: u32::try_from(value["pid"].as_u64()?).ok()?,
            holder: text(value, "holder"),
            line: text(value, "line")?,
            step: text(value, "step")?,
            own: value["own"].as_bool(),
            at: value["at"].as_u64()?,
            age_s: value["age_s"].as_u64()?,
            decision: text(value, "decision").unwrap_or_default(),
        })
    }
}

#[cfg(test)]
#[path = "release_pump_tests.rs"]
mod tests;
