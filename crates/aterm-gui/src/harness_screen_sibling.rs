// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sibling rows read the owning engine's per-session footprint, never a guessed PID tree.

use super::*;
use aterm_session::{LaunchNonce, SessionId};
use std::time::Duration;

// The whole peer visit shares one budget: a cold pull can take 1500 ms, the initial roster's
// placement hop up to 500 ms, and connect, framing and the remaining requests share the rest.
const WITHIN: Duration = Duration::from_secs(3);
const MAX_AGE_MS: u64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    local: u64,
    sid: SessionId,
    nonce: LaunchNonce,
}

struct Session {
    local: u64,
    identity: Option<Identity>,
    row: TabSpreadsheetRow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Memory {
    Complete(u64),
    AtLeast(u64),
    Unavailable,
}

impl Memory {
    fn words(self) -> String {
        match self {
            Self::Complete(bytes) => format!("RAM {}", HostVitals::human_bytes(bytes)),
            Self::AtLeast(bytes) => format!("RAM ≥{}", HostVitals::human_bytes(bytes)),
            Self::Unavailable => "RAM unavailable".to_owned(),
        }
    }
}

pub(super) fn read(computer: &str, pid: u32, socket: &str) -> Vec<TabSpreadsheetRow> {
    read_within(computer, pid, socket, WITHIN)
}

fn read_within(computer: &str, pid: u32, socket: &str, within: Duration) -> Vec<TabSpreadsheetRow> {
    let began = Instant::now();
    let remaining = || within.saturating_sub(began.elapsed());
    let Ok(mut peer) = aterm_ctl::PeerClient::connect(socket, remaining()) else {
        return Vec::new();
    };
    let Ok(before) = peer.lines("sessions", remaining()) else {
        return Vec::new();
    };
    let mut sessions: Vec<_> = before
        .iter()
        .filter_map(|line| session(computer, pid, line))
        .collect();
    if sessions.is_empty() {
        return Vec::new();
    }
    let reading = sessions
        .iter()
        .find_map(|s| s.identity.as_ref())
        .and_then(|id| {
            peer.lines(
                &format!("@{} sys aterm --json fresh={MAX_AGE_MS}", id.sid),
                remaining(),
            )
            .ok()
        })
        .filter(|lines| lines.len() == 1)
        .and_then(|lines| aterm_json::from_str::<aterm_json::Value>(&lines[0]).ok());
    let after = peer.lines("sessions bridge", remaining()).ok();
    apply(&mut sessions, after.as_deref(), reading.as_ref());

    let requests: Vec<_> = sessions
        .iter()
        .flat_map(|s| {
            [
                format!("@{} dims", s.row.sid),
                format!("@{} cwd", s.row.sid),
            ]
        })
        .collect();
    if let Ok(statuses) = peer.statuses(&requests, remaining()) {
        for (s, pair) in sessions.iter_mut().zip(statuses.chunks_exact(2)) {
            placement(&mut s.row, &pair[0], &pair[1]);
        }
    }
    sessions.into_iter().map(|s| s.row).collect()
}

fn session(computer: &str, pid: u32, line: &str) -> Option<Session> {
    let (local, row) = parse_sibling_roster_line(computer, pid, line)?;
    Some(Session {
        local,
        identity: identity(line),
        row,
    })
}

fn identity(line: &str) -> Option<Identity> {
    let mut words = line.split_whitespace();
    let local = uint(words.next()?)?;
    let sid = SessionId::parse(words.next()?)?;
    let mut nonces = words.filter_map(|w| w.strip_prefix("nonce="));
    let nonce = LaunchNonce::from_hex(nonces.next()?)?;
    if nonces.next().is_some() {
        return None;
    }
    Some(Identity { local, sid, nonce })
}

fn uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

#[derive(Default)]
struct Keys {
    locals: BTreeMap<u64, usize>,
    sids: BTreeMap<String, usize>,
}

impl Keys {
    fn of<'a>(rows: impl Iterator<Item = (u64, &'a str)>) -> Self {
        let mut keys = Self::default();
        for (local, sid) in rows {
            *keys.locals.entry(local).or_default() += 1;
            *keys.sids.entry(sid.to_owned()).or_default() += 1;
        }
        keys
    }

    fn unique(&self, id: &Identity) -> bool {
        self.locals.get(&id.local) == Some(&1) && self.sids.get(id.sid.as_str()) == Some(&1)
    }
}

fn apply(sessions: &mut [Session], after: Option<&[String]>, reading: Option<&aterm_json::Value>) {
    let after: Vec<_> = after
        .and_then(|lines| {
            lines
                .iter()
                .map(|line| identity(line))
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default();
    let mut parts = Vec::new();
    let mut part_keys = Keys::default();
    if let Some(reading) = reading.filter(|r| fresh(r))
        && let Some(rows) = reading.get("parts").and_then(|v| v.as_array())
    {
        for row in rows {
            if row.get("name").and_then(|v| v.as_str()) != Some("tab") {
                continue;
            }
            // An unproved pane still participates in ambiguity detection, but cannot erase
            // another pane's independently proved reading.
            if let Some(local) = row.get("local").and_then(|v| v.as_u64()) {
                *part_keys.locals.entry(local).or_default() += 1;
            }
            if let Some(sid) = row.get("sid").and_then(|v| v.as_str()) {
                *part_keys.sids.entry(sid.to_owned()).or_default() += 1;
            }
            let id = (|| {
                Some(Identity {
                    local: row.get("local")?.as_u64()?,
                    sid: SessionId::parse(row.get("sid")?.as_str()?)?,
                    nonce: LaunchNonce::from_hex(row.get("nonce")?.as_str()?)?,
                })
            })();
            let Some(id) = id else {
                continue;
            };
            parts.push((id, memory(row)));
        }
    }
    let before_keys = Keys::of(sessions.iter().map(|s| (s.local, s.row.sid.as_str())));
    let after_keys = Keys::of(after.iter().map(|id| (id.local, id.sid.as_str())));
    let after: BTreeMap<_, _> = after.into_iter().map(|id| (id.local, id)).collect();
    let parts: BTreeMap<_, _> = parts
        .into_iter()
        .map(|(id, memory)| (id.local, (id, memory)))
        .collect();
    for session in sessions {
        let memory = session
            .identity
            .as_ref()
            .and_then(|id| {
                if !before_keys.unique(id)
                    || !after_keys.unique(id)
                    || !part_keys.unique(id)
                    || after.get(&id.local) != Some(id)
                {
                    return None;
                }
                parts
                    .get(&id.local)
                    .filter(|(other, _)| other == id)
                    .map(|(_, memory)| *memory)
            })
            .unwrap_or(Memory::Unavailable);
        session.row.vitals = memory.words();
    }
}

fn fresh(reading: &aterm_json::Value) -> bool {
    if reading.get("schema").and_then(|v| v.as_u64()) != Some(1)
        || reading.get("kind").and_then(|v| v.as_str()) != Some("sys.aterm")
    {
        return false;
    }
    let Some(as_of) = reading.get("as_of") else {
        return false;
    };
    if !matches!(
        as_of.get("state").and_then(|v| v.as_str()),
        Some("fresh" | "partial")
    ) || !as_of
        .get("age_ms")
        .and_then(|v| v.as_u64())
        .is_some_and(|age| age <= MAX_AGE_MS)
    {
        return false;
    }
    // Unrelated stalled lanes do not erase a measured footprint. A stalled process/aterm
    // lane does: a machine frame's age cannot stand in for its missing process observation.
    if let Some(lanes) = reading.get("lanes") {
        let Some(lanes) = lanes.as_array() else {
            return false;
        };
        for lane in lanes {
            let Some(figures) = lane.get("figures").and_then(|v| v.as_array()) else {
                return false;
            };
            if figures.iter().any(|v| v.as_str().is_none()) {
                return false;
            }
            if figures
                .iter()
                .any(|v| matches!(v.as_str(), Some("aterm" | "tab" | "procs")))
            {
                return false;
            }
        }
    } else if as_of.get("state").and_then(|v| v.as_str()) == Some("partial") {
        return false;
    }
    true
}

fn memory(part: &aterm_json::Value) -> Memory {
    let Some(unread) = part
        .get("footprint_unread")
        .and_then(|v| v.as_u64())
        .filter(|n| *n <= u64::from(u32::MAX))
    else {
        return Memory::Unavailable;
    };
    let Some(value) = part.get("footprint_bytes") else {
        return Memory::Unavailable;
    };
    if let Some(bytes) = value.as_u64() {
        return if unread == 0 {
            Memory::Complete(bytes)
        } else {
            Memory::Unavailable
        };
    }
    if value.get("bound").and_then(|v| v.as_str()) == Some("at_least")
        && value.get("cause").and_then(|v| v.as_str()) == Some("refused")
        && unread > 0
        && value.get("unread").and_then(|v| v.as_u64()) == Some(unread)
        && let Some(bytes) = value.get("v").and_then(|v| v.as_u64())
    {
        return Memory::AtLeast(bytes);
    }
    Memory::Unavailable
}

fn placement(row: &mut TabSpreadsheetRow, dims: &str, cwd: &str) {
    if let Some(rest) = dims.strip_prefix("OK ") {
        let mut words = rest.split_whitespace();
        if let (Some(r), Some(c)) = (
            words.next().and_then(uint::<u32>),
            words.next().and_then(uint::<u32>),
        ) && r > 0
            && c > 0
        {
            row.place.push_str(crate::native_ui::FACT_SEPARATOR);
            row.place.push_str(&format!("{c}×{r}"));
        }
    }
    if let Some(rest) = cwd.strip_prefix("OK ") {
        let decoded = aterm_control::wire::pct_decode(rest.trim());
        if !decoded.is_empty() && decoded != "-" {
            row.place.push_str(crate::native_ui::FACT_SEPARATOR);
            row.place.push_str(&shorten_home_dir(&decoded));
        }
    }
}

#[cfg(test)]
#[path = "harness_screen_sibling_tests.rs"]
mod tests;
