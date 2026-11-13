// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native release-holder observations. Progress age and session presence grant no liveness.

use aterm_json::{Value, json};

/// The native registry reader admits at most four MiB before removing its check rows.
const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseLine {
    alive: Option<bool>,
    version: String,
    base: String,
    head: String,
    node: String,
    host: String,
    sid: String,
    pid: u32,
    boot: String,
    start: u64,
    step: String,
    boundary: String,
    at: u64,
    /// The step's 95th-percentile duration its progress is judged by (0: none recorded).
    p95_s: u64,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
}

impl ReleaseLine {
    pub(super) fn parse(row: &str) -> Option<Self> {
        let words = row.strip_prefix("release_line ")?;
        if words.len() > MAX_RECORD_BYTES * 3 + 32 {
            return None;
        }
        let (mut alive, mut data) = (None, None);
        for word in words.split_ascii_whitespace() {
            match word.split_once('=')? {
                ("alive", value) if alive.is_none() => alive = Some(value),
                ("data", value) if data.is_none() => data = Some(value),
                // Duplicate identity/liveness fields never acquire a last-writer meaning.
                _ => return None,
            }
        }
        let bytes = aterm_codec::pct::decode(data?);
        if bytes.len() > MAX_RECORD_BYTES {
            return None;
        }
        let record: Value = aterm_json::from_str(&bytes).ok()?;
        let line = &record["line"];
        let base = text(line, "base")?;
        let hops = line["hops"].as_array()?;
        let head = match hops.last() {
            Some(hop) => text(hop, "commit")?,
            None => base.clone(),
        };
        let holder = &record["holder"];
        let pid = u32::try_from(holder["pid"].as_u64()?).ok()?;
        if pid == 0 {
            return None;
        }
        Some(Self {
            alive: match alive {
                Some("1") => Some(true),
                Some("0") => Some(false),
                _ => None,
            },
            version: text(line, "version")?,
            base,
            head,
            node: text(holder, "node")?,
            host: text(holder, "host")?,
            sid: text(holder, "sid")?,
            pid,
            boot: text(holder, "boot")?,
            start: holder["start"].as_u64()?,
            step: text(&record, "step")?,
            boundary: text(&record, "boundary")?,
            at: record["at"].as_u64()?,
            p95_s: record["p95_s"].as_u64()?,
        })
    }

    /// Only a proven observation from the holder's own node can name its liveness.
    fn alive_at(&self, node: Option<&str>, unproven: bool) -> Option<bool> {
        self.alive
            .filter(|_| !unproven && node == Some(self.node.as_str()))
    }

    pub(super) fn json(&self, node: Option<&str>, unproven: bool) -> String {
        json!({
            "alive": self.alive_at(node, unproven),
            "line": {"version": self.version, "base": self.base, "head": self.head},
            "holder": {
                "node": self.node, "host": self.host, "sid": self.sid,
                "pid": self.pid, "boot": self.boot, "start": self.start
            },
            "step": self.step, "boundary": self.boundary, "at": self.at, "p95_s": self.p95_s
        })
        .to_string()
    }

    pub(super) fn summary(&self, node: Option<&str>, unproven: bool) -> String {
        let alive = match self.alive_at(node, unproven) {
            Some(true) => "live",
            Some(false) => "dead",
            None => "unknown",
        };
        super::shown(
            &format!(
                "  release v{} holder={alive} pid={} stage={} at={}",
                super::shown(&self.version, 24),
                self.pid,
                super::shown(&self.step, 20),
                self.at
            ),
            super::LINE_CHARS,
        )
    }
}
