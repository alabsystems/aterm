// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seam: what a verb body needs from whatever is hosting the session.

use std::borrow::Cow;
use std::time::Duration;

use aterm_core::terminal::Terminal;

/// What a host can do. Verbs gate on these, so a host lacking a facility
/// answers `ERR unsupported` instead of a plausible empty answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostCapabilities {
    /// Pixels can be captured (`image`/`video`/`window`/`chrome`).
    pub frame_source: bool,
    /// A windowing event loop exists to repaint and to route synthetic input.
    pub event_loop: bool,
    /// A system clipboard is reachable (`copy`).
    pub clipboard: bool,
    /// A roster is kept, so [`SessionHost::sessions`] and
    /// [`SessionHost::resolve`] answer for real. `false` says "I keep no
    /// index", which an empty `Vec` alone could not distinguish from "no
    /// sessions".
    pub roster: bool,
    /// An input sink is reachable ([`SessionHost::write_input`] can land bytes).
    pub input_sink: bool,
}

/// A session's lifecycle as its host observes it: the `sessions` wire
/// vocabulary, onto which each host maps its own states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Registered, engine + input sink live, reader not yet confirmed.
    Spawning,
    /// Live: something is feeding the engine.
    Alive,
    /// The command exited; the engine is still readable.
    Exited,
}

/// One row of [`SessionHost::sessions`]: exactly the fields a `sessions`/`ls`
/// line carries.
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
    /// Whether any user metadata is set (the wire's `meta=<1|0>`), so a fleet
    /// driver knows which sessions to query without N round trips.
    pub has_meta: bool,
}

/// Where a rostered session lives and what it is doing: the trailing `window=`
/// `active=` `wfocus=` `detail=` columns of a `sessions`/`ls` line, keyed by
/// [`SessionEntry::sid`].
///
/// Separate from [`SessionEntry`] because it costs more: placement needs the
/// window layer (one main-thread turn per batch) and `detail` each engine's
/// lock, while the roster is a registry read. A host answers them together
/// ([`SessionHost::placements`]) or not at all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionPlacement {
    /// The process-local id this placement extends.
    pub sid: u64,
    /// The window the session is reported under (the front window when it shows
    /// the session, else the lowest window id), `None` when no window hosts it.
    pub window: Option<u64>,
    /// On that window's active tab.
    pub active_tab: bool,
    /// That window is the front window.
    pub window_focused: bool,
    /// The sanitized running command (program + allow-listed subcommand, never
    /// an argument), `None` when idle or when the engine lock was contended.
    pub detail: Option<String>,
}

/// A wire `@<selector>` target the roster can answer. `@`/`@.` (self) is
/// absent: self is the connection's own session, which the dispatcher knows
/// and the roster does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector<'a> {
    /// `@<n>` — the process-local sid.
    Local(u64),
    /// `@s-<hex>` — the stable fabric id.
    Id(&'a str),
}

impl<'a> Selector<'a> {
    /// Classify the body after the leading `@`: all-digits is a local sid,
    /// anything else a stable id. `None` for the self token (`.` or empty).
    /// One parser, so hosts cannot disagree about what `@12` addresses.
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
/// A handle rather than a `wait(sid, timeout)` method because `wait` must be
/// registered before it re-checks the entry snapshot, so a completion landing
/// in that gap still leaves a notify pending.
pub trait ChangeWait {
    /// Park until the session changes, or until `timeout` elapses. `true` on a
    /// wake, `false` on timeout. A spurious/coalesced wake is fine — every
    /// caller re-reads the state it is waiting on.
    fn wait(&self, timeout: Duration) -> bool;

    /// Whether the client that asked for this wait has hung up. A socket host
    /// checks its connection (parking in slices short enough to notice); the
    /// default is never gone. Waiting verbs return their lane when `true`.
    fn caller_gone(&self) -> bool {
        false
    }
}

/// A host of one or more terminal sessions, addressed by process-local sid.
///
/// # The roster answers fleet-wide; every other method fails closed on a foreign sid
///
/// [`SessionHost::sessions`] and [`SessionHost::resolve`] answer for the whole
/// roster. Per-session methods answer only for sids this host serves (possibly
/// one: `aterm-gui`'s host is session-scoped). A sid it does not serve gets
/// `None` (a no-op for returnless methods), never its own session under a
/// borrowed number, which keeps the cross-host sequence safe:
///
/// ```ignore
/// let sid = host.resolve(sel)?;      // fleet-wide answer
/// host.write_input(sid, bytes);      // None unless THIS host serves that sid
/// ```
///
/// Resolve to pick a target, then obtain a host bound to it.
///
/// Not object-safe: the `impl FnOnce` accessors cost no `Box` per verb and
/// keep the host's lock guard (and `aterm-gui`'s debug lock-hold tripwire) on
/// the host's side. Both hosts dispatch statically.
pub trait SessionHost {
    /// What this host can do; see [`HostCapabilities`].
    fn capabilities(&self) -> HostCapabilities;

    /// The roster behind `sessions`/`ls`: every session this host serves,
    /// ascending by `sid`. A host serving one session returns one entry; only a
    /// host keeping no roster ([`HostCapabilities::roster`] false) may answer
    /// empty.
    fn sessions(&self) -> Vec<SessionEntry>;

    /// The roster's placements ([`SessionPlacement`]), in roster order. `None`
    /// means this host could not look (no window layer, or no answer), printed
    /// `window=-`; a detached session comes back inside `Some` with
    /// `window: None` (`window=none`). The two must stay distinct. Defaults to
    /// `None`.
    ///
    /// The shipped `sessions` line is written by the GUI's own verb from the
    /// same rows, and the GUI host mirrors it; no production path calls this
    /// yet (only `aterm-gui/src/control_host.rs` tests). It lets a non-GUI host
    /// answer placement through the trait.
    fn placements(&self) -> Option<Vec<SessionPlacement>> {
        None
    }

    /// Resolve a `@<selector>` against the roster; `None` when it holds no such
    /// session (the caller answers `ERR no such session` rather than falling
    /// back) and for every selector on a host with no roster. The sid need not
    /// be one this host serves (see [`SessionHost`]).
    fn resolve(&self, selector: Selector<'_>) -> Option<u64>;

    /// Run `f` against session `sid`'s terminal, or `None` if this host does not
    /// serve `sid`. Closure-based so the host owns the guard type.
    fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R>;

    /// [`SessionHost::with_terminal`] with mutable access.
    fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R>;

    /// Write `bytes` to session `sid`'s input sink (the raw path `send` and
    /// `feed` take to the child), whole frames only.
    ///
    /// `None` when this host does not serve `sid`, checked before the sink.
    /// `Some(false)` when the write did not happen (a wedged or absent sink,
    /// [`HostCapabilities::input_sink`] false).
    ///
    /// Empty `bytes` is the checkable no-op: it answers `Some(true)` iff a sink
    /// is there, letting conformance prove honest reporting without injecting a
    /// byte.
    ///
    /// The human-vocabulary verbs (`key`/`ctrl`/`mouse`/`paste`/`focus`) are not
    /// this method; their encoder reads live keyboard/mouse mode on the host's
    /// side.
    fn write_input(&self, sid: u64, bytes: &[u8]) -> Option<bool>;

    /// Ask the host to repaint `sid`. A no-op on a host with no event loop, and on
    /// a sid it does not serve.
    fn request_redraw(&self, sid: u64);

    /// Register interest in `sid`'s changes for the life of the returned handle
    /// (see [`ChangeWait`]). A sid this host does not serve gets a handle that
    /// only times out.
    fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_>;

    /// Whether session `sid` has ended — its program exited or it was closed —
    /// so a verb parked on it answers `ERR exited` rather than waiting out its
    /// deadline. The host notifies [`SessionHost::subscribe`]'s handles when it
    /// happens. Defaults to `false`: a host whose sessions outlive every verb.
    fn session_ended(&self, sid: u64) -> bool {
        let _ = sid;
        false
    }

    /// Place `text` on the system clipboard; `false` if the write failed. Only
    /// called when [`HostCapabilities::clipboard`] is set.
    fn clipboard_set(&self, text: &str) -> bool;

    /// A shell-reported working directory as this host's platform names it
    /// (what `blocks` prints for `cwd`).
    ///
    /// The engine stores the cwd as reported; for OSC 7 that is the RFC 8089
    /// URI path (`/C:/Users//x` on Windows from `file://host/C:/Users//x`), kept
    /// so recordings decode the same everywhere. The default is the identity,
    /// correct on POSIX; a Windows host converts, so a pwsh block reads
    /// `C:\Users\x` like a cmd block.
    fn native_cwd<'p>(&self, reported: &'p str) -> Cow<'p, str> {
        Cow::Borrowed(reported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The selector grammar: `.`/empty is the connection's own session (not a
    /// roster lookup), all-digits is a local sid, anything else a stable id.
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
}
