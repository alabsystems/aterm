// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-session TURN LEDGER: a bounded, drop-oldest ring of one record per
//! completed [`crate::control_session::cmd_turn`] exchange. It is the durable
//! memory behind two capabilities:
//!
//! * the `turns` control verb — an orchestrator reads back WHAT it drove into a
//!   session (submitted text, whether the submit landed, whether the reply
//!   settled or timed out, how long it took, and a stable hash of the settled
//!   screen), keyed by the same monotonic turn id the `turn` reply prints; and
//! * the `subscribe … events` digest — the push loop scans this ledger by id
//!   watermark and emits one `EVENT <sid> turn <id> …` line per new record, so a
//!   fleet controller watches N sessions on one fd and pulls a full `image`/
//!   `screen` only when a turn actually settled.
//!
//! Shaped like the other bounded recorders on [`crate::SessionCtx`] (drop-oldest,
//! `Mutex`-wrapped, cheap when unused): a session that is never driven by `turn`
//! holds an empty ring.

use std::collections::VecDeque;

/// How many turn records a session retains. Sized so a long agent conversation
/// stays fully readable while the ring never grows unbounded; the screen-hash +
/// bounded text keep each record small (~a few hundred bytes).
pub(crate) const LEDGER_CAP: usize = 512;

/// The submitted-text is bounded in the record so a pathological multi-megabyte
/// `turn` payload cannot bloat the ring; the full text still reached the PTY.
const MAX_TEXT: usize = 512;

/// One completed turn, in the order `cmd_turn` finished it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnRecord {
    /// The process-unique turn id (the `id=` the `turn` reply prints, and the id
    /// the events digest and `ERR busy turn=<id>` refusals name).
    pub id: u64,
    /// Milliseconds since the process epoch when the turn began (monotonic; for
    /// ordering + aligning against the temporal/cast spines, not wall-clock).
    pub started_ms: u64,
    /// How long the whole type→submit→settle exchange took, in milliseconds.
    pub dur_ms: u64,
    /// Whether a submit keypress VERIFIABLY landed (content advanced). `false`
    /// for `submit=none` and for a swallowed submit that never took.
    pub submitted: bool,
    /// The settle verdict: `settled` (went quiet) or `timeout` (deadline hit).
    pub status: &'static str,
    /// The submitted message, truncated to [`MAX_TEXT`] bytes on a char boundary.
    pub text: String,
    /// FNV-1a/64 of the settled screen text — deterministic across processes, so
    /// a replay/eval harness can diff a re-driven turn's screen against this.
    pub screen_hash: u64,
    /// The engine `content_seq` at settle (matches the `turn` reply's `seq=`).
    pub seq: u64,
}

/// A session's bounded turn history, newest-last, drop-oldest at [`LEDGER_CAP`].
#[derive(Default)]
pub struct TurnLedger {
    records: VecDeque<TurnRecord>,
}

impl TurnLedger {
    /// Append one record, evicting the oldest past the cap.
    pub(crate) fn push(&mut self, rec: TurnRecord) {
        if self.records.len() == LEDGER_CAP {
            self.records.pop_front();
        }
        self.records.push_back(rec);
    }

    /// How many turns this session has retained (for the `who` verb's `turns=`).
    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    /// The highest recorded turn id, or `None` when empty — the events digest
    /// seeds its watermark to this so it streams only turns that land AFTER
    /// subscription (a live stream, never the historical backlog).
    pub(crate) fn high_id(&self) -> Option<u64> {
        self.records.back().map(|r| r.id)
    }

    /// The LOWEST retained turn id (the drop-oldest low-water), or `None` when empty.
    /// A `since-turn=<n>` resume with `n < low_id - 1` means records were EVICTED
    /// between the anchor and the retained window — the events stream emits a
    /// `GAP … events-resync=` so the resumed subscriber knows it missed some (turn
    /// ids come from a process-global counter, so a client cannot infer the loss from
    /// a per-session id gap the way it can for contiguous block ids).
    pub(crate) fn low_id(&self) -> Option<u64> {
        self.records.front().map(|r| r.id)
    }

    /// Records with `id > after`, oldest-first (the events digest's scan, and the
    /// `turns since=<id>` verb). Records only ever append with strictly increasing
    /// ids, so this is a suffix. `None` = all retained records.
    pub(crate) fn since(&self, after: Option<u64>) -> impl Iterator<Item = &TurnRecord> {
        self.records
            .iter()
            .filter(move |r| after.is_none_or(|a| r.id > a))
    }
}

/// Truncate `s` to at most [`MAX_TEXT`] bytes on a UTF-8 char boundary.
pub(crate) fn clamp_text(s: &str) -> String {
    if s.len() <= MAX_TEXT {
        return s.to_string();
    }
    let mut end = MAX_TEXT;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// FNV-1a/64 — deterministic across processes (unlike `DefaultHasher`), so the
/// screen hash is stable enough to diff a replayed turn against a recorded one.
pub(crate) fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Milliseconds since the process epoch (a lazily-pinned monotonic `Instant`).
/// Monotonic and cheap; used for turn `started_ms`/`dur_ms`, not wall-clock.
pub(crate) fn now_ms() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    Instant::now().saturating_duration_since(epoch).as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u64) -> TurnRecord {
        TurnRecord {
            id,
            started_ms: id,
            dur_ms: 1,
            submitted: true,
            status: "settled",
            text: format!("msg{id}"),
            screen_hash: id,
            seq: id,
        }
    }

    #[test]
    fn ring_drops_oldest_and_since_is_a_suffix() {
        let mut l = TurnLedger::default();
        for i in 1..=(LEDGER_CAP as u64 + 3) {
            l.push(rec(i));
        }
        assert_eq!(l.high_id(), Some(LEDGER_CAP as u64 + 3));
        assert_eq!(l.since(None).count(), LEDGER_CAP, "capped");
        // The three oldest were evicted; `since` past the new floor is a suffix.
        let hi = LEDGER_CAP as u64 + 3;
        let got: Vec<u64> = l.since(Some(hi - 2)).map(|r| r.id).collect();
        assert_eq!(
            got,
            vec![hi - 1, hi],
            "only ids strictly greater than `after`"
        );
    }

    #[test]
    fn fnv_is_deterministic_and_text_clamps_on_boundary() {
        assert_eq!(fnv1a_64(b"kitty"), fnv1a_64(b"kitty"));
        assert_ne!(fnv1a_64(b"cat"), fnv1a_64(b"dog"));
        let long = "é".repeat(400); // 800 bytes, 2 per char
        let c = clamp_text(&long);
        assert!(c.len() <= MAX_TEXT && c.chars().all(|ch| ch == 'é'));
    }
}
