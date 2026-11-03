// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one door every read of a provider's usage reports through (the owner's no-polling rule,
//! `docs/DESIGN-alab-lab-model-2026-11-01.md` §11 step 9): each read happens because an account
//! is in use, says why, and is logged so the journal can audit it (`probe` in §4.17). The
//! journal's publisher (step 8) fills the hook with [`install`]; until then, and in every test,
//! nothing is published. The `usage-read-on-timer` census guard keeps timers away from the reads.

use std::sync::OnceLock;

/// Which provider read ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// `/usage` typed into a live Claude Code session.
    ClaudePanel,
    /// `/usage` read in Kimi's temporary tab, a person's `accounts refresh`.
    KimiPanel,
    /// Codex's `/wham/usage` or `/api/codex/usage`.
    CodexEndpoint,
    /// `agy -p /usage`.
    AgyUsage,
    /// `muse serve`'s usage report.
    MuseUsage,
}

impl Read {
    /// The journal's CLI word for this read.
    #[must_use]
    pub const fn cli(self) -> &'static str {
        match self {
            Self::ClaudePanel => "claude",
            Self::KimiPanel => "kimi",
            Self::CodexEndpoint => "codex",
            Self::AgyUsage => "agy",
            Self::MuseUsage => "muse",
        }
    }
}

/// Why the account read is in use: there is no other reason to read one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InUse {
    /// The row a launch is about to start on.
    Launching,
    /// A session running on the account ended a turn, or first drew its footer.
    LiveSession,
    /// A running session on the account stopped at a wall on its screen.
    AtWall,
    /// A person ran a command that names the read.
    Asked,
}

impl InUse {
    /// The journal's short reason word for this read.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Launching => "launching",
            Self::LiveSession => "live-session",
            Self::AtWall => "at-wall",
            Self::Asked => "asked",
        }
    }
}

/// One provider read, as the journal's `probe` event takes it. It carries no address, URL, path
/// or provider output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeEvent {
    /// When it was made, Unix seconds.
    pub at: i64,
    /// Which read.
    pub read: Read,
    /// The roster label read; the unpooled agy and Muse have none.
    pub account: Option<String>,
    /// An unpooled read's login, as its account key by realm (`journal::realm_key`), where the
    /// read saw one: never the address itself.
    pub login: Option<super::journal::Key>,
    /// Why the account is in use.
    pub why: InUse,
}

type Sink = Box<dyn Fn(&ProbeEvent) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Fills the hook, once per process: the journal's publisher, at the composing executable's start.
/// Returns whether this call filled it. No test calls it, so no test publishes.
pub fn install(sink: Sink) -> bool {
    SINK.set(sink).is_ok()
}

/// Reports one read. Callers report before reading, so a read that hangs or fails is still on
/// record.
pub fn note(event: &ProbeEvent) {
    #[cfg(any(test, debug_assertions))]
    CAPTURE.with(|held| {
        if let Some(seen) = held.borrow_mut().as_mut() {
            seen.push(event.clone());
        }
    });
    if let Some(sink) = SINK.get() {
        sink(event);
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Vec<ProbeEvent>>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: runs `body` and returns what it reported on this thread.
#[cfg(any(test, debug_assertions))]
pub fn captured<R>(body: impl FnOnce() -> R) -> (R, Vec<ProbeEvent>) {
    CAPTURE.with(|held| *held.borrow_mut() = Some(Vec::new()));
    let out = body();
    let seen = CAPTURE.with(|held| held.borrow_mut().take().unwrap_or_default());
    (out, seen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> ProbeEvent {
        ProbeEvent {
            at: 1,
            read: Read::CodexEndpoint,
            account: Some("codex-a".to_owned()),
            login: None,
            why: InUse::Launching,
        }
    }

    /// CONTROL: a note outside `captured` is not in the next capture, so a capture sees only its
    /// own body's reads.
    #[test]
    fn a_capture_holds_only_what_its_body_reported() {
        note(&event());
        let ((), seen) = captured(|| note(&event()));
        assert_eq!(seen, vec![event()]);
        let ((), none) = captured(|| ());
        assert!(none.is_empty());
    }
}
