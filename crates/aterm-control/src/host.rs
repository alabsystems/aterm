// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seam: what a verb body needs from whatever is hosting the session.

use std::time::Duration;

use aterm_core::terminal::Terminal;

/// What a host can actually do. Verbs GATE on these rather than assuming, so a
/// host that lacks a facility answers `ERR unsupported` instead of a
/// plausible-looking lie (an empty frame, a silently-dropped clipboard write).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostCapabilities {
    /// Pixels can be captured (`image`/`video`/`window`/`chrome`).
    pub frame_source: bool,
    /// A windowing event loop exists to repaint and to route synthetic input.
    pub event_loop: bool,
    /// A system clipboard is reachable (`copy`).
    pub clipboard: bool,
}

/// A session's lifecycle as its host observes it. This is the `sessions` wire
/// VOCABULARY, not a host's internal state machine: a host maps its own states
/// onto these three so two hosts cannot spell the same lifecycle differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Registered, engine + input sink live, reader not yet confirmed.
    Spawning,
    /// Live: something is feeding the engine.
    Alive,
    /// The command exited; the engine is still readable.
    Exited,
}

impl SessionState {
    /// The stable wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Spawning => "spawning",
            SessionState::Alive => "alive",
            SessionState::Exited => "exited",
        }
    }
}

/// One row of [`SessionHost::sessions`] — exactly the fields a `sessions`/`ls`
/// line carries, so a host can be rostered without the verb reaching past the
/// seam for a seventh thing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    /// Process-local id: the sid every other method on this trait takes.
    pub sid: u64,
    /// Stable fabric identity (`s-<hex>`) — the `@<id>` selector form, and what
    /// survives a restart that renumbers `sid`.
    pub id: String,
    /// The spawning session's stable id, if any (the family tree).
    pub parent: Option<String>,
    /// Lifecycle; see [`SessionState`].
    pub state: SessionState,
    /// Live title, best effort.
    pub title: String,
    /// Whether any USER metadata is set (the wire's `meta=<1|0>`), so a fleet
    /// driver knows which sessions to query without N round trips.
    pub has_meta: bool,
}

/// A wire `@<selector>` target the ROSTER can answer.
///
/// `@`/`@.` (self) is deliberately absent: self is the CONNECTION's own session,
/// which the dispatcher knows and the roster does not — resolving it here would
/// make every host guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector<'a> {
    /// `@<n>` — the process-local sid.
    Local(u64),
    /// `@s-<hex>` — the stable fabric id.
    Id(&'a str),
}

impl<'a> Selector<'a> {
    /// Classify the body AFTER the leading `@`: all-digits is a local sid,
    /// anything else a stable id. `None` for the self token (`.` or empty).
    ///
    /// One parser, so two hosts cannot disagree about what `@12` addresses.
    #[must_use]
    pub fn parse(body: &'a str) -> Option<Self> {
        match body {
            "" | "." => None,
            b => Some(b.parse::<u64>().map_or(Selector::Id(b), Selector::Local)),
        }
    }
}

/// Blocking wake handle from [`SessionHost::subscribe`].
///
/// Separate from a `wait(sid, timeout)` method BECAUSE OF AN ORDERING BUG THIS
/// SHAPE PREVENTS: `wait` must be REGISTERED before it re-checks the entry
/// snapshot, so a completion landing in that gap still leaves a notify pending.
/// Only a handle the caller holds across the re-check can express that.
pub trait ChangeWait {
    /// Park until the session changes, or until `timeout` elapses. `true` on a
    /// wake, `false` on timeout. A spurious/coalesced wake is fine — every
    /// caller re-reads the state it is waiting on.
    fn wait(&self, timeout: Duration) -> bool;
}

/// A host of one or more terminal sessions, addressed by process-local sid.
///
/// # The sid contract is SPLIT, and mixing the halves writes to the wrong session
///
/// [`SessionHost::sessions`] and [`SessionHost::resolve`] answer for the whole
/// roster. The per-session methods do NOT: a host may be SESSION-SCOPED, built
/// against one session the dispatcher already resolved, in which case it serves
/// that session whatever sid you pass — `aterm-gui`'s host is exactly this shape.
///
/// So this is a bug, not an idiom:
///
/// ```ignore
/// let sid = host.resolve(sel)?;      // fleet-wide answer
/// host.write_input(sid, bytes);      // may land on a DIFFERENT session
/// ```
///
/// Resolve to pick a target, then obtain a host bound to it. A future
/// fleet-scoped host must honor sid on every method; until one exists,
/// [`HostCapabilities`] does not distinguish the two shapes and the caller is
/// what keeps them apart.
///
/// NOT OBJECT-SAFE, deliberately: the `impl FnOnce` accessors cost no `Box` per
/// verb and keep the host's lock guard (and, in `aterm-gui`, its debug
/// lock-hold tripwire) on the host's side of the seam. Both hosts dispatch
/// statically. A future host needing `Box<dyn SessionHost>` must re-cut the two
/// terminal accessors against an object-safe shape first.
pub trait SessionHost {
    /// What this host can do; see [`HostCapabilities`].
    fn capabilities(&self) -> HostCapabilities;

    /// The roster behind `sessions`/`ls`: every session this host serves, in the
    /// order the wire lists them (ascending `sid`). A host serving exactly one
    /// session returns one entry — never an empty roster standing in for "I don't
    /// keep one".
    fn sessions(&self) -> Vec<SessionEntry>;

    /// Resolve a `@<selector>` against the ROSTER; `None` when it holds no such
    /// session (fail closed — the caller answers `ERR no such session` rather
    /// than falling back to some other session).
    ///
    /// Not necessarily a sid the per-session methods honor: see the split-contract
    /// warning on [`SessionHost`].
    fn resolve(&self, selector: Selector<'_>) -> Option<u64>;

    /// Run `f` against session `sid`'s terminal, or `None` if the host does not
    /// resolve `sid`. Closure-based so the host owns the guard type.
    fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R>;

    /// [`SessionHost::with_terminal`] with mutable access.
    fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R>;

    /// Write `bytes` to session `sid`'s INPUT SINK — the raw hatch `send` and
    /// `feed` take to the child, whole frames only (no interleaving with another
    /// writer's bytes).
    ///
    /// `None` when the host does not resolve `sid`; `Some(false)` when the write
    /// did NOT happen, so a wedged or absent sink cannot answer `OK`. The
    /// human-vocabulary verbs (`key`/`ctrl`/`mouse`/`paste`/`focus`) are NOT this
    /// method — they need an encoder reading live keyboard/mouse mode, which stays
    /// on the host's side of the seam.
    fn write_input(&self, sid: u64, bytes: &[u8]) -> Option<bool>;

    /// Ask the host to repaint `sid`. A no-op on a host with no event loop.
    fn request_redraw(&self, sid: u64);

    /// Register interest in `sid`'s changes. Registration lasts as long as the
    /// returned handle (see [`ChangeWait`] for why that is load-bearing).
    fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_>;

    /// Place `text` on the system clipboard; `false` if the write failed. Only
    /// called when [`HostCapabilities::clipboard`] is set.
    fn clipboard_set(&self, text: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The selector grammar the wire has always used, now in ONE place: `.`/empty
    /// is the connection's own session (not a roster lookup), all-digits is a
    /// local sid, everything else is a stable id verbatim.
    #[test]
    fn selector_parses_the_wire_forms() {
        assert_eq!(Selector::parse(""), None);
        assert_eq!(Selector::parse("."), None);
        assert_eq!(Selector::parse("12"), Some(Selector::Local(12)));
        assert_eq!(Selector::parse("s-0f1e"), Some(Selector::Id("s-0f1e")));
        // Too big for a u64 local id ⇒ a stable id, never a silent wrap.
        let huge = "99999999999999999999";
        assert_eq!(Selector::parse(huge), Some(Selector::Id(huge)));
    }

    /// The state tokens are wire bytes; a host maps onto them, it does not coin
    /// its own spellings.
    #[test]
    fn session_state_tokens_are_stable() {
        assert_eq!(SessionState::Spawning.as_str(), "spawning");
        assert_eq!(SessionState::Alive.as_str(), "alive");
        assert_eq!(SessionState::Exited.as_str(), "exited");
    }
}
