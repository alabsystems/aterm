// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live introspection control socket (aterm introspection control protocol v1).
//!
//! A background thread binds a Unix domain socket and serves newline-delimited
//! text requests so an out-of-process client can read the live screen
//! (text/cursor/cell/search), drive the shell (send/key), snapshot pixels
//! (image), drive selection (plain ranges and `word`/`line`/`block`/`extend`
//! gestures, selection/copy), and resize the engine and PTY, all against the
//! running terminal the window presents.
//!
//! Threading: text/cursor/cell/search read the shared [`Terminal`] directly;
//! send/key/resize write the PTY master fd. `image` needs the renderer on the
//! main thread, so this thread pushes an [`ImageReq`], wakes the event loop with
//! [`Wake::Control`] and blocks on the reply; the main thread renders, writes
//! the PNG and replies with the frame dimensions.
//!
//! In a window, `image` (and the SIGUSR1 snapshot) re-renders the current
//! terminal through the same renderer and splice/composite sequence as the
//! window present, including the live cursor blink phase and the hollow
//! unfocused cursor, so a focused blinking session may capture no cursor
//! pixels. It does not observe compositor visibility or scanout. Headless
//! sessions render a semantic frame with the blink phase pinned on. For
//! deterministic cursor state use the `cursor` verb.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

use aterm_uds::{CtlListener, CtlStream};

use aterm_containment::log_denial;
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_session::{EdgeToken, Op, SessionId, decide_edge};
use winit::event_loop::EventLoopProxy;

use crate::control_auth::{self, AuthOutcome};
use crate::input::{Egress, InputEvent, InputOutcome, PasteFraming, ScrollIntent, Source};
use crate::session_store::Store;
use crate::subscribe::{self, InstanceStreams, PushOptions, PushScopes, Requested, Subscribers};
use crate::{SessionCtx, Wake, term_lock};

/// Read-only screen introspection serializers, dispatched as
/// `control_query::cmd_*` from [`handle`]. The file is `src/control_query.rs`.
#[path = "control_query.rs"]
mod control_query;
// Query serializers reached as `crate::control::NAME` (`crate::subscribe`).
pub(crate) use control_query::{
    ScreenGen, gather_styled_frame, rows_hash, screen_gen, screen_stamp, screen_text,
    serialize_styled_frame, trimmed_len, visible_row,
};
// The driver-geometry conformance reads a generation the way a fenced driver
// does, through the verb's own function.
#[cfg(test)]
pub(crate) use control_query::{cmd_text_json_looked, text_args};
// Full-history search (shared by ⌘F and the `search` verb) and its index depth
// cap, reached as `crate::control::NAME`.
#[cfg(test)]
pub(crate) use control_query::search_full_history;
pub(crate) use control_query::{
    search_full_history_direction, search_full_history_point, set_search_max_lines,
};
// The combined single-lock wrapper is test-only at the crate level now: prod
// callers split gather/serialize so JSON rendering runs off the mutex.
#[cfg(test)]
pub(crate) use control_query::styled_frame_payload;
// Test-only serialization of searches that share the process-global index cache, so a
// test that asserts reuse or counts a cold build's copy is not churned by another's.
#[cfg(test)]
pub(crate) use control_query::search_cap_test_guard;

/// Input-injection verbs and their parsers
/// (key/ctrl/send/feed/signal/mouse/paste/focus/resize/scroll/tab), dispatched
/// as `control_input::cmd_*` from [`handle`]. The file is `src/control_input.rs`.
#[path = "control_input.rs"]
mod control_input;
// Parsers reached as `crate::control::NAME` (`crate::input`, `input_stall`), and
// `no_such_tab`, the refusal `App::apply_tab_cmd_in` formats.
pub(crate) use control_input::{
    feed_bytes, no_such_tab, parse_ctrl, parse_key, parse_mouse, send_bytes,
};
// `input_stall`'s restart test sends `signal term` through the verb itself,
// and the harness's restart tests drive `signal` and `restart` as the
// dispatch runs them (`signal_verb`, `restart_verb`).
#[cfg(all(test, target_os = "macos"))]
pub(crate) use control_input::{cmd_signal, restart_verb, signal_verb};
// The press path's wiring test drives a `key`/`ctrl`/`paste` body through the
// control fence and then the verb's own post, as `handle` does.
#[cfg(test)]
pub(crate) use control_input::{control_fence, paste_event};

/// Selection/copy/block verbs (`select`/`selection`/`copy`/`blocks`/
/// `blocktext`/`wait`), implemented in the winit-free `aterm-control` crate over
/// a [`SessionHost`](aterm_control::SessionHost); aliased for [`handle`].
use aterm_control::selection as control_selection;
// Smart-selection gesture helpers for `app_mouse.rs`'s double/triple click;
// they take a bare `&mut Terminal`.
pub(crate) use aterm_control::selection::{select_line, select_word, word_span};
// Response-framing primitives, re-exported so `crate::control::NAME` and
// `super::` callers resolve.
pub(crate) use aterm_control::wire::{json_escape, json_escape_into, pct_encode, visible_char};
use aterm_control::wire::{json_ok, json_str_field};

/// This crate's [`SessionHost`](aterm_control::SessionHost): a borrowed adapter
/// over the target [`handle`] resolved. The file is `src/control_host.rs`.
#[path = "control_host.rs"]
mod control_host;
use control_host::GuiHost;

/// Main-thread conformance gate: the verb matrix against a [`GuiHost`] holding a
/// real `EventLoopProxy`, which no `#[test]` can build. A child of `control`
/// because `control_host` is private; a debug build's only (aterm-control's
/// suite is). The file is `src/control_redraw_conformance.rs`.
#[cfg(debug_assertions)]
#[path = "control_redraw_conformance.rs"]
pub(crate) mod control_redraw_conformance;

/// Platform clipboard layer (`pbcopy`/`pbpaste`, X11 PRIMARY), AppKit/aterm-x11/
/// Win32 code used across the GUI. The file is `src/clipboard.rs`.
#[path = "clipboard.rs"]
mod clipboard;
// Re-export `pbcopy`/`pbpaste` (GUI OSC-52 path, `main.rs`, menu Paste), which
// reach through the stable `crate::control::NAME` path, so those paths keep resolving.
pub(crate) use clipboard::{pbcopy, pbpaste};
// The Windows terminal paste's file-list (`CF_HDROP`) arm, routed like the rest.
#[cfg(windows)]
pub(crate) use clipboard::{pbpaste_paths, write_with_owner};
// Test clipboard board, reached as `crate::control::PBPASTE_STUB`; test builds
// never touch the real clipboard.
#[cfg(test)]
pub(crate) use clipboard::PBPASTE_STUB;
// Its PRIMARY twin, for tests driving the Linux middle-click paste path.
#[cfg(all(test, target_os = "linux"))]
pub(crate) use clipboard::PRIMARY_STUB;
// Linux-only clipboard helpers: `pbpaste_owned` (non-blocking own-selection
// read for the OSC 52 query, which must not block under the terminal lock),
// `primary_get`/`primary_get_owned` (PRIMARY paste) and `primary_set`. One
// grouped re-export (a second `pbpaste_owned` import is E0252 on Linux).
#[cfg(target_os = "linux")]
pub(crate) use clipboard::{pbpaste_owned, primary_get, primary_get_owned, primary_set};

/// Media-capture verbs (`image`/`image read`/`window`/`chrome`), dispatched as
/// `control_media::cmd_*` from [`handle`]. The file is `src/control_media.rs`.
#[path = "control_media.rs"]
// `pub(crate)` so sibling verb modules (`control_privacy`) can reach
// `call_main`.
pub(crate) mod control_media;
/// The aimed-spawn target: built in `control_media::cmd_spawn`, carried in
/// `Wake::SpawnSession`.
pub(crate) use control_media::{IdentitySpec, SpawnAim, resolve_spawn_identity};
// Re-export `image_payload`: `control_query::styled_image_json` reaches it through
// `super::image_payload`, which now resolves to this sibling module's serializer.
pub(crate) use control_media::image_payload;

/// The handshake's `ERR auth` audit line, thinned per refusal kind.
#[path = "control_denials.rs"]
pub(crate) mod control_denials;
/// Session-graph and capability verbs (`sessions`/`family`/`ready`/`cast`/
/// `edges`/`grant`/`revoke`/`whoami`), dispatched as `control_session::cmd_*`
/// from [`handle`]. The file is `src/control_session.rs`.
#[path = "control_session.rs"]
mod control_session;
#[cfg(test)]
pub(crate) use control_session::cmd_sessions_store;
// The turn-id counter a self-update handoff carries (`crate::seamless`).
pub(crate) use control_session::{TURN_ID_RESERVE, raise_turn_ids, turn_ids_minted};
#[cfg(any(unix, test))]
pub(crate) use control_session::{turn_ids_for_handoff, turn_ids_past_handoff_ceiling};
// The two reads a handoff's end-to-end test drives `aterm drive report` through
// (the handoff is unix-only, and so is that test).
#[cfg(all(test, unix))]
pub(crate) use control_query::cmd_offscreen;
#[cfg(all(test, unix))]
pub(crate) use control_session::cmd_history;
/// The `lease` verb, for the driver-geometry conformance (macOS: its pty pair
/// is `input_stall`'s).
#[cfg(all(test, target_os = "macos"))]
pub(crate) use control_session::cmd_lease;
/// The `meta` verb, for Autopilot's timer tests (`crate::autopilot_timers`),
/// which drive the wire's `until=` against the App's index.
#[cfg(test)]
pub(crate) use control_session::cmd_meta;
/// The `turn` composite and its io, for the presence tests that drive a REAL
/// turn through the lease seam (`app_presence`'s driver-attribution tests).
#[cfg(test)]
pub(crate) use control_session::{TurnIo, cmd_turn, end_turn_input};
/// The `log` and `brief` reads, for the session log's own tests.
#[cfg(test)]
pub(crate) use control_session::{cmd_brief, cmd_log, cmd_log_json};

/// The `await` verb, for the exit and restore paths' `await log` tests
/// (`app_tabs`, `app_restore_tests`).
#[cfg(test)]
pub(crate) use control_session::cmd_await;

/// The containment subsystem name used in audit denials from this socket.
const AUDIT_SUBSYSTEM: &str = "control_socket";

/// The active tab's engine and PTY master, shared with the GUI so control verbs
/// follow tab switches. Updated by `App::sync_active_session`; each request
/// resolves its target from it ([`resolve_active`]). Affects only which session
/// a verb targets, never the auth gates.
pub(crate) struct ActiveSession {
    pub term: Arc<Mutex<Terminal>>,
    pub master: i32,
    /// The active session's stable id, so a control verb that DIRECTLY mutates the
    /// engine (scroll/select) can request a repaint of the right tab.
    pub id: u64,
    /// The active session's fabric context (sink + edge table + identity), so the
    /// op-scope gate and the writer verbs resolve the live tab's sink/table.
    pub ctx: Arc<SessionCtx>,
}

/// Shared optional front-terminal handle. `None` means the front content is a
/// native tab with no PTY or `Terminal`; session requests then get the typed
/// no-terminal error unless they name a live session.
pub(crate) type ActiveHandle = Arc<Mutex<Option<ActiveSession>>>;

/// One coherent `dims` observation assembled on the main thread: the session's
/// grid (`rows`/`cols`) plus the selected window's font metrics, composed frame
/// and raw surface, so the resize/zoom remainder is visible.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DimsSnapshot {
    pub(crate) session: u64,
    pub(crate) rows: u32,
    pub(crate) cols: u32,
    pub(crate) pixel_w: u32,
    pub(crate) pixel_h: u32,
    pub(crate) cell_w: u32,
    pub(crate) cell_h: u32,
    pub(crate) font_px: f32,
    /// DPI scale the selected window's geometry derives from
    /// (`WindowState::scale`: `scale_factor()` or a `--scale` pin); `font_px`,
    /// `pad`, `pad_top` and `head` all follow from it. A headless boot still
    /// selects a window (`geometry` = `headless`).
    ///
    /// When no window holds the session (`geometry` = `detached`, read off the
    /// shared backend) this is `App::detached_scale`: a `--scale` pin, else the
    /// scale of the window the backend is tuned to, else the front window's,
    /// else the lowest window id's, else `1.0`.
    pub(crate) scale: f64,
    pub(crate) window: Option<u64>,
    pub(crate) window_rows: u32,
    pub(crate) window_cols: u32,
    pub(crate) composed_rows: u32,
    pub(crate) frame_w: u32,
    pub(crate) frame_h: u32,
    pub(crate) surface_w: u32,
    pub(crate) surface_h: u32,
    pub(crate) offset_x: i64,
    pub(crate) offset_y: i64,
    pub(crate) band_left: u32,
    pub(crate) band_right: u32,
    pub(crate) band_top: u32,
    pub(crate) band_bottom: u32,
    pub(crate) crop_left: u32,
    pub(crate) crop_right: u32,
    pub(crate) crop_top: u32,
    pub(crate) crop_bottom: u32,
    /// Base padding on the left/right/bottom edges.
    pub(crate) pad: u32,
    pub(crate) pad_top: u32,
    /// Explicit alias for the bottom edge, so introspection consumers never
    /// have to infer the old (and now invalid) `2*pad - pad_top` rule.
    pub(crate) pad_bottom: u32,
    pub(crate) head: u32,
    pub(crate) tab_rows: u32,
    pub(crate) viewers: u32,
    pub(crate) visible_viewers: u32,
    pub(crate) geometry: &'static str,
    /// CURRENT recovery state of the selected window (`ready`, `backoff`,
    /// `parked`, or `detached`), unlike the process-global last-drop metric.
    pub(crate) present_retry_state: &'static str,
    pub(crate) present_retry_count: u32,
    pub(crate) present_retry_remaining: u32,
    pub(crate) present_retry_in_ms: Option<u64>,
    /// The GPU swapchain layer's presentation state where one exists:
    /// `(contentsGravity, contentsScale, contentsAreFlipped)`. Reports whether
    /// the live-resize anchor is in effect; the layer belongs to
    /// `raw-window-metal`, which may overwrite `CALayer` properties.
    pub(crate) layer_presentation: Option<(String, f64, bool)>,
}

/// Snapshot the active engine, PTY master and id for one request. Recovers from
/// poison so a panicked GUI thread cannot wedge introspection.
fn resolve_active(active: &ActiveHandle) -> Option<Target> {
    let g = active.lock().unwrap_or_else(|p| p.into_inner());
    g.as_ref().map(|active| {
        (
            active.term.clone(),
            active.master,
            active.id,
            active.ctx.clone(),
        )
    })
}

const NO_ACTIVE_TERMINAL: &str = "ERR no active terminal\n";

/// Why an owner-only verb is refused, or `None` when it may run: it names no
/// session but the caller's own, and the caller holds the owner token.
fn owner_only_refusal(verb: &str, selector: Option<&Selector>, scope: Scope) -> Option<String> {
    if !matches!(selector, None | Some(Selector::SelfTok)) {
        return Some(format!("ERR denied: {verb} takes no @<sid>\n"));
    }
    if !scope.is_owner_class() {
        return Some(format!("ERR denied: {verb} is owner-only\n"));
    }
    None
}

/// The self-feed floor's refusal ([`crate::inject_floor`]: a 256 KiB burst,
/// refilled at 256 KiB/s). Drivers key on the `ERR rate` head and back off.
const RATE_REFUSAL: &str = "ERR rate: input to this session is over 256 KiB/s; retry in a moment\n";

/// Principal/target projection consumed by the drift-free native-control model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NativeControlPrincipal {
    Owner,
    Edge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NativeControlTarget {
    App,
    BareSession,
    ExplicitSession,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NativeControlDecision {
    WithoutSession,
    ResolveSession,
    Denied,
    NoActiveTerminal,
    NoSuchSession,
}

/// Pure authority/target decision used by the shipping dispatcher and Tier-1
/// conformance. It cannot observe or manufacture a hidden terminal fallback.
#[must_use]
pub(crate) const fn native_control_decision(
    front_has_terminal: bool,
    explicit_session_live: bool,
    principal: NativeControlPrincipal,
    target: NativeControlTarget,
) -> NativeControlDecision {
    match target {
        NativeControlTarget::App => match principal {
            NativeControlPrincipal::Owner => NativeControlDecision::WithoutSession,
            NativeControlPrincipal::Edge => NativeControlDecision::Denied,
        },
        NativeControlTarget::BareSession => {
            if front_has_terminal {
                NativeControlDecision::ResolveSession
            } else {
                NativeControlDecision::NoActiveTerminal
            }
        }
        NativeControlTarget::ExplicitSession => {
            if explicit_session_live {
                NativeControlDecision::ResolveSession
            } else {
                NativeControlDecision::NoSuchSession
            }
        }
    }
}

/// Resolve a bare/self `paste` when a native Editor owns the front (no
/// `ActiveSession`), matching a human paste into the focused view.
///
/// `None` keeps the terminal resolver. Only an Owner's bare/self paste takes the
/// app-authority lane, by [`native_control_decision`]'s App case; explicit
/// selectors and Edges stay session-bound.
///
/// `text` is a thunk so the payload (up to `MAX_FEED_BIN` for `paste-bin`) is
/// only materialized in the arm that consumes it.
fn sessionless_front_paste_event(
    verb: &str,
    text: impl FnOnce() -> String,
    selector: Option<&Selector>,
    front_has_terminal: bool,
    principal: NativeControlPrincipal,
) -> Option<Result<InputEvent, &'static str>> {
    if verb != "paste" || front_has_terminal || !matches!(selector, None | Some(Selector::SelfTok))
    {
        return None;
    }
    Some(
        match native_control_decision(false, true, principal, NativeControlTarget::App) {
            NativeControlDecision::WithoutSession => {
                Ok(InputEvent::Paste(text(), PasteFraming::AtDrain))
            }
            NativeControlDecision::Denied => Err("ERR denied\n"),
            NativeControlDecision::ResolveSession
            | NativeControlDecision::NoActiveTerminal
            | NativeControlDecision::NoSuchSession => Err("ERR invalid control target\n"),
        },
    )
}

/// Resolve a bare/self `key` when a native view owns the front (no
/// `ActiveSession`): the same named-key press a person's keyboard makes on
/// that view, so Settings' arrows, Tab and Return can be driven headless.
///
/// `None` keeps the terminal resolver. Only an Owner's bare/self key takes the
/// app-authority lane, by [`native_control_decision`]'s App case; explicit
/// selectors and Edges stay session-bound.
fn sessionless_front_key_event(
    rest: &str,
    selector: Option<&Selector>,
    front_has_terminal: bool,
    principal: NativeControlPrincipal,
) -> Option<Result<InputEvent, &'static str>> {
    if front_has_terminal || !matches!(selector, None | Some(Selector::SelfTok)) {
        return None;
    }
    Some(
        match native_control_decision(false, true, principal, NativeControlTarget::App) {
            NativeControlDecision::WithoutSession => {
                control_input::parse_key(rest).ok_or(control_input::KEY_USAGE)
            }
            NativeControlDecision::Denied => Err("ERR denied\n"),
            NativeControlDecision::ResolveSession
            | NativeControlDecision::NoActiveTerminal
            | NativeControlDecision::NoSuchSession => Err("ERR invalid control target\n"),
        },
    )
}

/// Resolve `resize px <w> <h>` when a native tab owns the front (no
/// `ActiveSession`). A window resize is a window fact, so it must work with
/// Settings in front, as an edge drag does.
///
/// `None` keeps the session resolver: the cell form resizes engine and PTY, and
/// an explicit `@<sid>` names a terminal. Only an Owner's bare/self request takes
/// the app-authority route, by [`native_control_decision`]'s App case.
fn sessionless_front_window_resize(
    verb: &str,
    rest: &str,
    selector: Option<&Selector>,
    front_has_terminal: bool,
    principal: NativeControlPrincipal,
) -> Option<Result<InputEvent, String>> {
    if verb != "resize" || front_has_terminal || !matches!(selector, None | Some(Selector::SelfTok))
    {
        return None;
    }
    let parsed = control_input::parse_resize_px(rest)?;
    Some(
        match native_control_decision(false, true, principal, NativeControlTarget::App) {
            // Keep the parser's own answer for a malformed request (missing vs
            // unspellable argument), whatever tab is in front.
            NativeControlDecision::WithoutSession => parsed,
            NativeControlDecision::Denied => Err("ERR denied\n".to_string()),
            NativeControlDecision::ResolveSession
            | NativeControlDecision::NoActiveTerminal
            | NativeControlDecision::NoSuchSession => {
                Err("ERR invalid control target\n".to_string())
            }
        },
    )
}

/// Lane for an aimed `spawn`/`tab` (`@<sid> spawn …`, `@<sid> tab …`). `None`
/// means not aimed; flagless and `@.` forms use the ordinary App lane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AimedAppLane {
    /// Owner: the selector reaches the session resolver, which passes the
    /// session to `cmd_spawn(.., Some(session))` / `cmd_tab_aimed`. A dead sid
    /// answers `ERR no such session`.
    ResolveSession,
    /// Edge: `ERR denied`, BEFORE the sid is looked up — an unauthorised
    /// caller learns nothing about which sessions exist.
    Denied,
}

/// App-target verbs whose aimed form (`@<sid> <verb> …`) goes to the session
/// resolver instead of the App lane.
///
/// One list for two readers: [`aimed_app_lane`] uses it to route past
/// `dispatch_before_session`'s [`crate::fabric::app_halt_refusal`] gate, and
/// the session dispatch re-asks that App-lane question after resolution (an
/// aimed App verb drives the window, not the session; see [`handle`]).
fn is_aimed_app_verb(verb: &str) -> bool {
    matches!(verb, "spawn" | "tab" | "theme")
}

/// Classify an aimed `spawn`/`tab`. The selector is the verb's argument ("the
/// window hosting `<sid>`"), so for an Owner it must reach the session resolver;
/// the ordinary App lane would validate and drop it, degrading to the flagless
/// form.
///
/// Authority does not travel with the routing: an Edge is refused by
/// [`native_control_decision`]'s App verdict (Tier-1 pins
/// `edge_app_allowed = 0`). Otherwise a write-input edge on one session could
/// spawn or close tabs in that session's window. Non-App verdicts fail closed.
fn aimed_app_lane(
    verb: &str,
    selector: Option<&Selector>,
    principal: NativeControlPrincipal,
) -> Option<AimedAppLane> {
    if !is_aimed_app_verb(verb) || matches!(selector, None | Some(Selector::SelfTok)) {
        return None;
    }
    Some(
        match native_control_decision(false, true, principal, NativeControlTarget::App) {
            NativeControlDecision::WithoutSession => AimedAppLane::ResolveSession,
            NativeControlDecision::Denied
            | NativeControlDecision::ResolveSession
            | NativeControlDecision::NoActiveTerminal
            | NativeControlDecision::NoSuchSession => AimedAppLane::Denied,
        },
    )
}

/// What a connection is authorized to do, resolved at handshake. `Debug` is
/// safe to derive: an [`EdgeToken`] redacts itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    /// The per-instance token: full control of the active session, all verbs
    /// including grant/revoke, and sibling sessions in the same process. A scoped
    /// `Edge` needs an explicit edge per target (`cross_session_authorized`).
    Owner,
    /// An edge token scoped to one op against the handshake target. Only the
    /// [`EdgeToken`] is carried; authority is re-derived per request
    /// (`decide_edge`/`authorize` against the resolved target's table and
    /// nonce), so it grants nothing on other sessions and a moved `@.` grants no
    /// stale power. The connect-time op is not cached, so whoami and audit
    /// always reflect the token's real authority.
    Edge(EdgeToken),
    /// The fabric bridge: the near end of a `socketpair` the instance keeps when
    /// launching its `aterm-link serve` child. Authority is the connection,
    /// pre-resolved for the inherited fd; there is no token to steal.
    ///
    /// Owner's power plus the `Access::BridgeOnly` verbs (`deliver`, `link`,
    /// `outbox`, `outbox sent`) and the fleet form of `hold`; `hold` handlers never
    /// let an Owner act set or lift an `origin=fleet` hold ([`crate::fabric::HoldIssuer`]).
    /// Every in-session client holds Owner, so a prompt-injected agent must not
    /// forge human orders, lift a fleet halt or read all outbound traffic.
    ///
    /// Unix-only (Windows has no inherited socketpair); the authority matrix is
    /// tested on every host.
    #[cfg(any(unix, test))]
    Bridge,
}

impl Scope {
    /// Owner-class: full instance authority. `Bridge` is (Owner plus the bridge
    /// verbs); `Edge` never is.
    ///
    /// Sites that deliberately keep `Owner` and `Bridge` apart:
    ///
    /// * [`dispatch_hold_verb`] and [`dispatch_fabric_verb`]: `hold` issues the
    ///   fleet halt from `Bridge` and the local one from `Owner`; `fabric` is
    ///   `Scope::Owner` only.
    /// * [`crate::control_session::cmd_whoami`] answers `owner` vs `bridge`,
    ///   which `Bridge::new`'s startup probe reads.
    /// * [`crate::control_session::caller_actor`] writes `by=ctl` vs
    ///   `by=bridge` on the exits ledger.
    pub(crate) fn is_owner_class(self) -> bool {
        match self {
            Scope::Owner => true,
            #[cfg(any(unix, test))]
            Scope::Bridge => true,
            Scope::Edge(_) => false,
        }
    }

    /// Whether this is the inherited bridge connection — never, where there is
    /// no bridge lane.
    pub(crate) fn is_bridge(self) -> bool {
        #[cfg(any(unix, test))]
        {
            matches!(self, Scope::Bridge)
        }
        #[cfg(not(any(unix, test)))]
        {
            false
        }
    }
}

/// WHO PRESENTED an Owner connection's token (design §2.5, phase 3A): the
/// instance token beside the socket, or the harness's in-memory token
/// ([`control_auth::harness_token`]). A label for the session log's `by=`
/// and nothing else: it grants nothing, and [`Scope`] is unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Principal {
    /// The instance token (every in-session client, `aterm ctl`): `by=ctl`.
    #[default]
    Instance,
    /// The in-GUI harness's token, held only in this process's memory:
    /// `by=harness`.
    Harness,
}

/// The op-class a verb needs, from [`VERBS`] (the one verb table, from which
/// the help catalog is generated and against which the dispatch router is
/// checked). `ReadScreen` observes; `WriteInput` injects human input and
/// drives the app; `Signal` sends signals; `ConfigWrite` (settings) and
/// `ClipboardWrite` (copy) are separate fine ops an inherited base edge cannot
/// reach. `None` for Owner-only or meta verbs (gated before the op check) or an
/// unknown verb (`ERR unknown verb`). `turn` also requires read authority for
/// scoped callers, enforced in its arm.
fn required_op(verb: &str) -> Option<Op> {
    use aterm_types::control_verbs::OpClass;
    aterm_types::control_verbs::spec(verb).and_then(|s| match s.op {
        OpClass::Read => Some(Op::ReadScreen),
        OpClass::Write => Some(Op::WriteInput),
        OpClass::Signal => Some(Op::Signal),
        // `settings` and `copy` have their own fine ops, so inherited base
        // edges cannot reach them; only Owner can.
        OpClass::ConfigWrite => Some(Op::ConfigWrite),
        OpClass::ClipboardWrite => Some(Op::ClipboardWrite),
        OpClass::Owner => None,
    })
}

/// Argument-dependent authority: seams where a plain `WriteInput` edge could
/// otherwise reach a fenced op that [`required_op`] cannot see.
///
///   * `invoke <action>`: each menu action is classified exhaustively by
///     [`menu::MenuAction::invoke_authority`]. Clipboard actions
///     (`Copy`/`Paste`/`SelectAll`) need `ClipboardWrite`; config surfaces
///     (`Preferences`, `ToggleSettings`) need `ConfigWrite`; `OpenPalette` (a
///     gateway to every action) and `SoftwareUpdate`/`ApplyUpdate` are
///     Owner-only; view-state actions stay `WriteInput`. The wire name is the
///     exact `MenuAction` Debug token (`controls menu` lists them).
///   * `open prefs` / `open settings` and `open app settings /route` need
///     `ConfigWrite`.
///   * `act app/v1 ...` resolves its view kind on the main thread after
///     authorization, so the gateway needs `ConfigWrite`; a Settings ▸ Harness
///     session press (`harness/session/…`) writes an Autopilot word and is
///     Owner-only. Both are defence in depth today: `act` is an App-target
///     verb, which `dispatch_before_session` refuses every Edge before this
///     is asked (`native_control_decision`).
///   * `open menu` / `open palette` and `open update` are Owner-only, like their
///     `invoke` twins.
///
/// Returns the escalated requirement, else `None`. The dispatch checks it with
/// the same per-session predicate as the base gate.
fn escalated_op(verb: &str, rest: &str) -> Option<Escalation> {
    use crate::menu::{InvokeAuthority, MenuAction};
    // Classify what the verb will run, not its raw spelling: `hwkey`, `pointer` and
    // `invoke` take a leading `unread=ok` off before they parse (`take_app_unread_ok`),
    // so `pointer unread=ok click` or `invoke unread=ok OpenPalette` must escalate
    // exactly as the bare forms do.
    let (_, rest) = control_input::take_app_unread_ok(verb, rest);
    match verb {
        "invoke" => {
            let action = MenuAction::from_invoke_name(rest.trim())?;
            match action.invoke_authority() {
                // Benign: the base `WriteInput` gate already covers it.
                InvokeAuthority::WriteInput => None,
                InvokeAuthority::ConfigWrite => Some(Escalation::Op(Op::ConfigWrite)),
                InvokeAuthority::ClipboardWrite => Some(Escalation::Op(Op::ClipboardWrite)),
                // The palette (a gateway to EVERY action) and the update-apply twins:
                // no single fine op expresses "only the god token", so require Owner.
                InvokeAuthority::OwnerOnly => Some(Escalation::OwnerOnly),
            }
        }
        // Classify by the target token only: `cmd_open` treats a trailing
        // `close` as the dismiss flag, and `open <target> close` must escalate
        // like `open <target>`.
        "open"
            if matches!(
                aterm_types::app_inspection::parse_open_app(rest),
                Ok(aterm_types::app_inspection::OpenAppRequest::Settings { .. })
            ) =>
        {
            // The versioned Settings app is the same capability as `open prefs`;
            // classify it here, before the main-thread hop.
            Some(Escalation::Op(Op::ConfigWrite))
        }
        // A Settings ▸ Harness session press writes that session's Autopilot word,
        // which is Owner-only like `meta set autopilot` (`meta_questions_denied`):
        // the Settings fine op does not reach it. `act_app` presses only the action
        // the live node carries, so the action's spelling is the press.
        "act"
            if aterm_types::app_inspection::parse_act(rest).is_ok_and(|act| {
                act.action
                    .starts_with(crate::harness_screen::SESSION_ACTION_PREFIX)
            }) =>
        {
            Some(Escalation::OwnerOnly)
        }
        // `act app/v1 ...` resolves its view kind only on the main thread, so the
        // gateway requires ConfigWrite rather than trusting the action spelling.
        "act" if aterm_types::app_inspection::parse_act(rest).is_ok() => {
            Some(Escalation::Op(Op::ConfigWrite))
        }
        // A connected spawn (`spawn connected=… of=<sid>`) mints session-connection
        // authority, and `identity=` opens a session under another login's
        // credentials: both Owner-only. A plain `spawn [cwd=…]` keeps WriteInput.
        // Tokenized with `split_quoted_tokens`, like `parse_spawn_args`, so no
        // quoting walks past the fence; unparseable input escalates too.
        "spawn"
            if control_media::split_quoted_tokens(rest).is_none_or(|tokens| {
                tokens
                    .iter()
                    .any(|t| t.starts_with("connected=") || t.starts_with("identity="))
            }) =>
        {
            Some(Escalation::OwnerOnly)
        }
        // Surfaces showing the aggregated connection graph (`open`, `controls`,
        // `window connections`) are Owner-only like `flows`. Classified by the
        // target token, so the `close` variant escalates too.
        "open" | "controls" | "window" if rest.split_whitespace().next() == Some("connections") => {
            Some(Escalation::OwnerOnly)
        }
        "open" => match crate::app_introspect::AuxTarget::parse(
            rest.split_whitespace().next().unwrap_or(""),
        ) {
            // The Settings tab flips default-OFF security knobs => ConfigWrite.
            Some(crate::app_introspect::AuxTarget::Prefs) => Some(Escalation::Op(Op::ConfigWrite)),
            // The palette, software-update overlay and in-grid tab menu are
            // gateways, Owner-only like their `invoke` twins.
            Some(
                crate::app_introspect::AuxTarget::Menu
                | crate::app_introspect::AuxTarget::TabMenu
                | crate::app_introspect::AuxTarget::AutopilotMenu
                | crate::app_introspect::AuxTarget::Update,
            ) => Some(Escalation::OwnerOnly),
            _ => None,
        },
        // `meta set`/`meta unset` write session metadata, so they need
        // `WriteInput`; bare `meta` is a read.
        "meta" => match rest.split_whitespace().next() {
            Some("set" | "unset") => Some(Escalation::Op(Op::WriteInput)),
            _ => None,
        },
        // `theme` reads; its preview lane changes what every window draws
        // (runtime-only, like `rain`), so it needs `WriteInput`; `set` and
        // `commit` rewrite `aterm.toml` like `settings set`, so `ConfigWrite`.
        // Classified by the sub-form word the parser reads, so no spelling of a
        // write form passes as a read.
        // `theme session set|clear` changes one session's colours (runtime
        // only, carried by an update and a restore): `WriteInput`, like a
        // preview; `theme session [status]` and `theme palette` read.
        "theme" => {
            let mut words = rest.split_whitespace();
            match (words.next(), words.next()) {
                (None | Some("status" | "list" | "palette"), _) => None,
                (Some("session"), None | Some("status")) => None,
                (Some("session"), Some(_)) => Some(Escalation::Op(Op::WriteInput)),
                (Some("preview" | "revert"), _) => Some(Escalation::Op(Op::WriteInput)),
                (Some(_), _) => Some(Escalation::Op(Op::ConfigWrite)),
            }
        }
        // `inbox seen` advances the handled watermark, so it needs `WriteInput`;
        // the listing and `inbox get` are reads.
        "inbox" => match rest.split_whitespace().next() {
            Some("seen") => Some(Escalation::Op(Op::WriteInput)),
            _ => None,
        },
        // `family <sid>` walks the tree from an arbitrary node: Owner-only.
        "family" if !rest.trim().is_empty() => Some(Escalation::OwnerOnly),
        // `brief all` reads every session's log, the whole roster: Owner-only
        // like `sessions`. `brief [<node>]` reads only its target. Classified by
        // the verb's own parser (which takes `all` anywhere among the
        // arguments), fail-closed: only a request it reads as a target's brief
        // stays a read, so no spelling of the instance-wide form passes as one.
        "brief" if !crate::session_log::BriefQuery::parse(rest).is_ok_and(|q| !q.all) => {
            Some(Escalation::OwnerOnly)
        }
        // `pointer click` presses whatever is under the pointer, including every
        // message-band capsule, so it is Owner-only like `notice act` and spends
        // its budget (`App::pointer_cmd`). `pointer mods` sets the modifiers the
        // window reads for keys and clicks until the person next touches it
        // (`App::release_scripted_mods`): with `link` a plain click opens a link,
        // with `cmd` a typed `v` pastes, so it is Owner-only like the click it arms.
        "pointer" if matches!(rest.split_whitespace().next(), Some("click" | "mods")) => {
            Some(Escalation::OwnerOnly)
        }
        _ => None,
    }
}

/// Escalated requirement from [`escalated_op`]: a fine [`Op`] a granted edge can
/// satisfy, or Owner-only where no op expresses it. Resolved by
/// [`dispatch_authorized`]; Owner satisfies both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Escalation {
    /// Requires the fine op — `scope_holds_op` decides (an edge granted exactly this op
    /// passes; Owner always passes).
    Op(Op),
    /// Requires the per-instance Owner token — no fine op is sufficient (a scoped Edge,
    /// however privileged, is DENIED).
    OwnerOnly,
}

/// What owns controller-directed input in the front window (internal,
/// main-thread). Includes the native Settings tab, so a focused Settings app is
/// never mistaken for terminal input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FrontControlSurface {
    /// No app/overlay consumes front input.
    None,
    /// A transient own-rendered overlay consumes input.
    Overlay(crate::overlay::OverlayKind),
    /// The process-singleton native Settings tab consumes input.
    NativeSettings,
    /// The native Settings tab consumes input on Settings ▸ Harness, whose
    /// session buttons write a session's Autopilot word: Owner-only, like
    /// `meta set autopilot` (`harness_screen::SESSION_ACTION_PREFIX`).
    HarnessSettings,
    /// A native app other than Settings consumes input.
    OtherNative,
}

/// Whether `scope` may exercise `need` against `ctx`'s session, shared by the
/// base op gate and the [`escalated_op`] fence. Owner always holds; an `Edge`
/// needs a [`decide_edge`]-permitted token for exactly `need`, bound to this
/// session's table and launch nonce (default deny).
fn scope_holds_op(scope: Scope, need: Op, ctx: &SessionCtx) -> bool {
    match scope {
        // Owner-class scopes hold every op.
        Scope::Owner => true,
        #[cfg(any(unix, test))]
        Scope::Bridge => true,
        Scope::Edge(presented) => {
            let table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            decide_edge(&table, &presented, &ctx.self_id, need, &ctx.nonce).is_permitted()
        }
    }
}

fn scope_holds_escalation(scope: Scope, need: Escalation, ctx: &SessionCtx) -> bool {
    match need {
        Escalation::Op(op) => scope_holds_op(scope, op, ctx),
        Escalation::OwnerOnly => scope.is_owner_class(),
    }
}

/// `help [<verb> | --full | --json]` (alias `verbs`): the catalog of every verb
/// this build supports, generated from the `VERBS` table in aterm-types.
/// Answered for any authenticated scope before target resolution.
///
/// Bare: the short catalog (a three-line header and one summary row per verb,
/// under `SHORT_CATALOG_MAX_BYTES`). `help <verb>`: that verb's full entry,
/// wrapped. `help --full`: the complete catalog under the protocol header,
/// pinned to a golden. Anything else is `ERR usage`.
fn cmd_help(rest: &str) -> String {
    use aterm_types::control_verbs::{JSON_CAPABLE_VERBS, catalog_lines, spec};
    let tail = rest.trim();
    if tail == "--full" {
        return cmd_help_full();
    }
    if tail == "--json" {
        return help_catalog_json();
    }
    if tail.starts_with('-') || tail.contains(char::is_whitespace) {
        return "ERR usage: help [<verb> | --full | --json]\n".to_string();
    }
    if !tail.is_empty() {
        return match spec(tail) {
            Some(s) => {
                let lines = s.entry_lines();
                let mut body = format!("OK {}\n", lines.len());
                for line in lines {
                    body.push_str(&line);
                    body.push('\n');
                }
                body
            }
            None => format!("ERR unknown verb '{tail}' (help lists them)\n"),
        };
    }
    // Short form: framing, the generated `--json` set, pointers to the other
    // forms, then one summary row per verb.
    let json_verbs = JSON_CAPABLE_VERBS.join("/");
    let mut body = format!(
        "# Framing: one \"OK …\"/\"ERR …\" status line, or \"OK <count>\" + <count> content lines; \
         EMPTY content = no data, never an error\n\
         # --json on {json_verbs} = structured output · \"@<sid> <verb>\" = run it on that session\n\
         # help <verb> = the full entry · help --full = the complete catalog\n"
    );
    for line in catalog_lines() {
        body.push_str(&line);
        body.push('\n');
    }
    let n = body.lines().count();
    format!("OK {n} aterm introspection protocol v1\n{body}")
}

/// `help --json`: `OK 1` then one JSON line,
/// `{"verbs":[{"name","op","framing","target","access","summary"}, …]}` in table
/// order, projected from the `VerbSpec` table.
fn help_catalog_json() -> String {
    use aterm_control::wire::json_escape;
    let rows: Vec<String> = aterm_types::control_verbs::VERBS
        .iter()
        .map(|s| {
            format!(
                "{{\"name\":\"{}\",\"op\":\"{}\",\"framing\":\"{}\",\"target\":\"{}\",\
                 \"access\":\"{}\",\"summary\":\"{}\"}}",
                json_escape(s.name),
                s.op.wire_name(),
                s.framing.wire_name(),
                s.target.wire_name(),
                s.access.wire_name(),
                json_escape(s.summary),
            )
        })
        .collect();
    format!("OK 1\n{{\"verbs\":[{}]}}\n", rows.join(","))
}

/// `help --full`: the complete catalog under the full protocol header, pinned to
/// a golden.
fn cmd_help_full() -> String {
    // "OK <count> …\n" plus <count> body lines so aterm-ctl streams it like other
    // list verbs. A fixed header (framing, cross-session, targeting, seeing
    // modes), then the catalog generated from `VERBS`; the `--json` list comes
    // from `JSON_CAPABLE_VERBS`.
    let json_verbs = aterm_types::control_verbs::JSON_CAPABLE_VERBS.join("/");
    let header = &format!(
        r#"# Framing: every reply is a STATUS line ("OK …" / "ERR …") OR raw CONTENT lines. Content
# verbs prefix "OK <count>" then <count> lines — except the Bytes-framed verbs (cast,
# temporal, inbox get, outbox), which prefix "OK <nbytes>" then exactly that many raw bytes;
# aterm-ctl prints the content and consumes the OK line, so an EMPTY content reply means NO
# DATA — it is never an error (errors are always "ERR …"). Add "--json" to {json_verbs} for
# structured output; a verb without a json form answers "ERR json: not supported for <verb>"
# or, outside that allowlist, reads "--json" as an ordinary argument — do not probe with it.
# Cross-session: prefix a verb with "@<selector>" (e.g. "@s-ab12 text"). Same-user only;
# the per-launch token is auto-discovered by aterm-ctl. A sid hosted by ANOTHER same-user
# aterm instance is relayed to it transparently (owner connections): every session of every
# instance is addressable here. Discover peers with `sessions` (this instance) or the
# client-side `aterm-ctl ls` (all instances).
# TARGETING RULE: the @<sid> selects WHERE the verb runs. SESSION verbs (text/screen/
# image/turn/send/…) act on that session. APP-LEVEL verbs (window/chrome/controls/open/
# settings/invoke/…) act on the resolved instance's FRONT window — the selector routes to
# the instance (cross-instance via relay), e.g. `@peer window` screenshots the peer's
# window, `@peer invoke NewTab` opens a tab in the peer. `tab` and `spawn` differ: an
# explicit `@<sid>` aims them at the window HOSTING that session; flagless stays FRONT.
#
# SEEING a terminal (yours or a peer's) — five modes, pick by need:
#   text / screen        what the PROGRAM wrote (plain rows / lossless styled grid)
#   image                what ATERM submitted (client frame: decorations, tab strip,
#                        overlays; `image plain` strips host bling for bare pixels;
#                        compositor visibility/scanout is outside this boundary)
#   subscribe            the LIVE movie (pushed frames; every-frame for animations, mail)
#   cast | temporal <t>  HISTORY: replayable timestamped recording | the screen as it WAS
#                        at instant <t> (temporal needs config temporal_recording=true)
#   search <pat>         FIND across the full scrollback history (trigram-indexed, = ⌘F)
#
# NATIVE TAB APPS (the active window is retained; canonical documents are reused):
#   open app settings /about              About inside the Settings tab
#   open app settings /updates            Software Update inside the Settings tab
#   open app markdown file:///abs/doc.md  read one bounded, local UTF-8 file
#   open app editor file:///abs/doc.md    edit that file through a read/write grant
#   inspect app/v1 tabs                    discover stable native view ids
#   inspect app/v1 view <id> text|controls|tree|audit
#   act app/v1 view <id> <key> <action> [value]
# Local documents require an absolute file: URI. The host canonicalizes it, rejects remote
# authorities/non-files/oversize/non-UTF-8 input, and grants only that exact file.
#
"#
    );
    let mut body = String::from(header);
    for line in aterm_types::control_verbs::catalog_lines_full() {
        body.push_str(&line);
        body.push('\n');
    }
    let n = body.lines().count();
    format!("OK {n} aterm introspection protocol v1\n{body}")
}

#[cfg(test)]
mod help_tests {
    use aterm_types::control_verbs::{
        CATALOG_TEXT_COLUMN, ENTRY_WRAP_COLUMNS, VERBS, catalog_lines, spec,
    };

    /// The short form has three `#` header lines naming the other forms, then
    /// exactly the table's summary rows (whose byte budget `aterm-types` checks at
    /// compile time).
    #[test]
    fn help_short_form_is_the_summary_catalog() {
        let h = super::cmd_help("");
        let mut lines = h.lines();
        let status = lines.next().unwrap();
        assert_eq!(
            status,
            format!("OK {} aterm introspection protocol v1", VERBS.len() + 3)
        );
        let header: Vec<&str> = lines.clone().take(3).collect();
        assert!(header.iter().all(|l| l.starts_with("# ")), "{header:?}");
        assert!(
            header[0].contains("ERR") && header[0].contains("EMPTY"),
            "framing line documents the OK/ERR/empty framing: {}",
            header[0]
        );
        let rows: Vec<&str> = lines.skip(3).collect();
        let want: Vec<String> = catalog_lines().collect();
        assert_eq!(rows, want, "the rows are exactly the table's summary rows");
        // No row carries the detail: the short form must not quietly grow back.
        assert!(
            !h.contains("image --meta"),
            "a full entry leaked into the short form"
        );
        // Same reply for the alias tail spellings the dispatch hands over.
        assert_eq!(super::cmd_help("  "), h);
    }

    /// `help <verb>`: `OK <n>` plus the verb's full entry, wrapped with
    /// continuation rows indented; lossless against `help_line`; surrounding
    /// whitespace tolerated; a one-row verb equals its full-catalog row.
    #[test]
    fn help_verb_form_is_the_wrapped_full_entry() {
        let h = super::cmd_help("image");
        let mut lines = h.lines();
        let status = lines.next().unwrap();
        let rows: Vec<&str> = lines.collect();
        assert_eq!(status, format!("OK {}", rows.len()));
        assert!(rows.len() > 5, "image's entry must wrap into several rows");
        let gutter = " ".repeat(CATALOG_TEXT_COLUMN);
        for row in &rows[1..] {
            assert!(
                row.starts_with(&gutter),
                "continuation row not indented: {row:?}"
            );
        }
        for row in &rows {
            assert!(
                row.chars().count() <= ENTRY_WRAP_COLUMNS,
                "row wider than {ENTRY_WRAP_COLUMNS}: {row:?}"
            );
        }
        let joined: Vec<&str> = std::iter::once(&rows[0][CATALOG_TEXT_COLUMN..])
            .chain(rows[1..].iter().map(|r| &r[CATALOG_TEXT_COLUMN..]))
            .collect();
        assert_eq!(joined.join(" "), spec("image").unwrap().help_line());
        assert_eq!(super::cmd_help(" image "), h, "the tail is trimmed");
    }

    /// An unknown verb is named back with the way to the list; any other flag
    /// or extra token is a usage error, never the whole catalog.
    #[test]
    fn help_rejects_unknown_verbs_and_bad_tails() {
        assert_eq!(
            super::cmd_help("nosuch"),
            "ERR unknown verb 'nosuch' (help lists them)\n"
        );
        for tail in [
            "-h",
            "--jsn",
            "image --full",
            "--full image",
            "--json image",
            "a b",
        ] {
            assert_eq!(
                super::cmd_help(tail),
                "ERR usage: help [<verb> | --full | --json]\n",
                "tail {tail:?}"
            );
        }
    }

    /// `help --full` is the one wire surface people pipe for the whole catalog,
    /// and it is GENERATED: the header plus exactly `catalog_lines_full()`. No
    /// checked-in copy of that text is kept (it was a 178 KB fixture that 289
    /// commits touched in ten days, every one a merge conflict for the next):
    /// what is worth asserting is the shape, not the wording — the tail is
    /// trimmed, nothing is empty, and every verb's full row is in the answer
    /// exactly once and in catalog order.
    #[test]
    fn help_full_is_the_header_and_every_catalog_row_in_order() {
        let got = super::cmd_help("--full");
        assert_eq!(super::cmd_help(" --full "), got, "the tail is trimmed");
        assert!(!got.trim().is_empty(), "help --full is empty");
        let mut rest = got.as_str();
        for row in aterm_types::control_verbs::catalog_lines_full() {
            let at = rest
                .find(&row)
                .unwrap_or_else(|| panic!("help --full lost (or reordered) the row: {row}"));
            rest = &rest[at + row.len()..];
        }
    }
}

/// Whether an `update` sub-command is Owner-only: bare/`status` is a read for
/// any scope; `check` (stages a build) and `apply` (handoff or disk replacement)
/// mutate, like the Owner-only `invoke ApplyUpdate` / `open update`.
fn update_is_owner_only_subcmd(rest: &str) -> bool {
    matches!(rest.trim(), "check" | "apply")
}

/// `aterm_update::installed_update_facts()` cached for [`INSTALLED_FACTS_TTL`]:
/// on macOS the probe spawns codesign/spctl/PlistBuddy, and `update status` is
/// polled by any scope. A pure observation.
fn cached_installed_update_facts() -> Option<aterm_update::InstalledUpdateFacts> {
    const INSTALLED_FACTS_TTL: std::time::Duration = std::time::Duration::from_secs(20);
    static CACHE: std::sync::Mutex<
        Option<(
            std::time::Instant,
            Option<aterm_update::InstalledUpdateFacts>,
        )>,
    > = std::sync::Mutex::new(None);
    let mut guard = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, facts)) = guard.as_ref()
        && at.elapsed() < INSTALLED_FACTS_TTL
    {
        return facts.clone();
    }
    let facts = aterm_update::installed_update_facts();
    *guard = Some((std::time::Instant::now(), facts.clone()));
    facts
}

/// Brackets a person's `update check` for the window
/// (`Wake::UpdateCheckAsked`): `begun: true` at `begin`, `false` on drop, so a
/// panic still ends it.
struct UpdateCheckAsked<'a>(&'a EventLoopProxy<Wake>);

impl<'a> UpdateCheckAsked<'a> {
    fn begin(proxy: &'a EventLoopProxy<Wake>) -> Self {
        let _ = proxy.send_event(Wake::UpdateCheckAsked { begun: true });
        Self(proxy)
    }
}

impl Drop for UpdateCheckAsked<'_> {
    fn drop(&mut self) {
        let _ = self.0.send_event(Wake::UpdateCheckAsked { begun: false });
    }
}

/// Whether a control server may publish and serve now: immediately, except for a
/// per-process update candidate, which waits for the Commit to release its gate
/// (`publish_gate`). `false` only when the gate is never released (the attempt
/// stood down and the candidate is reaped).
fn await_commit_to_publish(gate: Option<&crate::spawn::DeferredReaderGate>) -> bool {
    gate.is_none_or(|gate| gate.wait_until_released(&AtomicBool::new(false)))
}

/// `update check`'s answer when the main thread refuses the settings read
/// (finding 40): the refusal in its own words behind the verb's prefix —
/// `ERR update check: aterm is showing a dialog; answer it, then retry`, or
/// `… main thread stalled <N>s; retry`, which the harness rides
/// out as a request turned away.
fn update_check_refused(refusal: &crate::control::control_media::MainHopError) -> String {
    format!("ERR update check: {refusal}\n")
}

/// `update status` when the updater answers no status. On macOS that means its
/// ledger cannot be read — the updater exists, so not "no updater" — and the
/// cause is the one `aterm update status` names
/// (`aterm_update::ledger_unreadable_cause`). Elsewhere (Linux always answers)
/// there is no updater at all.
fn update_status_unreadable_line() -> String {
    #[cfg(target_os = "macos")]
    {
        format!(
            "OK enabled=true outcome=\"can\u{2019}t read the update state: {}\"\n",
            aterm_update::ledger_unreadable_cause()
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        "OK enabled=false outcome=\"no updater on this platform\"\n".to_string()
    }
}

#[cfg(test)]
mod update_status_unreadable_tests {
    /// A MAC WHOSE LEDGER CAN'T BE READ IS NOT A PLATFORM WITHOUT AN UPDATER: the
    /// line names the cause `aterm update status` names, word for word.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_unreadable_ledger_says_why_not_no_updater() {
        let line = super::update_status_unreadable_line();
        assert!(line.starts_with("OK enabled=true "), "{line}");
        assert!(
            line.contains(&aterm_update::ledger_unreadable_cause()),
            "{line}"
        );
        assert!(!line.contains("no updater"), "{line}");
    }
}

fn cmd_update(rest: &str, scope: Scope, proxy: &EventLoopProxy<Wake>) -> String {
    let build = crate::build_info::build_number()
        .parse::<u64>()
        .unwrap_or(0);
    // `check` and `apply` are Owner-only (see `update_is_owner_only_subcmd`);
    // `status` answers any scope. The in-session AI drives with the Owner token.
    if update_is_owner_only_subcmd(rest) && !scope.is_owner_class() {
        log_denial(
            AUDIT_SUBSYSTEM,
            &format!("update {}", rest.trim()),
            aterm_containment::mode_or_containment(),
            "owner-only sub-command (check/apply) from a scoped edge",
        );
        return "ERR denied: update check/apply is owner-only\n".to_string();
    }
    let st = match rest.trim() {
        "" | "status" => aterm_update::status(build),
        "check" => {
            // The check reads live settings on the main thread; under a dialog or
            // stall that hop is refused, which would otherwise surface as an
            // empty `OK` status hiding a stage on disk. Refuse typed, up front.
            if let Some(refusal) = crate::control::control_media::main_thread_refusal() {
                return update_check_refused(&refusal);
            }
            let provider = crate::update_control::check_settings_provider(proxy.clone());
            // A person asked, so the download takes the animated row; the guard
            // brackets the check however it ends.
            let _asked = UpdateCheckAsked::begin(proxy);
            let status = aterm_update::check_now_with_settings(build, &provider);
            crate::update_control::announce_stage(&status, |wake| {
                let _ = proxy.send_event(wake);
            });
            Some(status)
        }
        // Apply a staged update now. The reducer validates durable facts and
        // dirty native state asynchronously. macOS acknowledges the request;
        // Linux returns its completed disk transaction. Neither claims a live
        // handoff occurred.
        "apply" => {
            #[cfg(target_os = "linux")]
            {
                return match aterm_update::linux::apply() {
                    Ok(message) => format!("OK {message}\n"),
                    Err(error) => format!("ERR {error}\n"),
                };
            }
            #[cfg(not(target_os = "linux"))]
            {
                if proxy.send_event(Wake::ApplyStagedUpdate).is_err() {
                    return "ERR event loop gone\n".to_string();
                }
                // Report the stage the reducer will act on (read as `status`
                // reads it) and where the verdict lands.
                let staged = aterm_update::status(build).and_then(|mut st| {
                    fold_installed_activation(&mut st);
                    st.staged_build
                        .filter(|staged| *staged > st.current_build)
                        .map(|staged| (staged, st.staged_version))
                });
                return match staged {
                    Some((staged, version)) => {
                        let version = version
                            .filter(|v| !v.starts_with("build "))
                            .map_or_else(String::new, |v| format!(" ({v})"));
                        format!(
                            "OK apply requested: build {staged}{version} over build {build}; \
                             `update status` reports the verdict (apply_posture=, \
                             apply_refusal=)\n"
                        )
                    }
                    None => "OK apply requested; no build is staged — `update status` reports \
                             the verdict (apply_refusal=)\n"
                        .to_string(),
                };
            }
        }
        _ => return "ERR usage: update [status|check|apply]\n".to_string(),
    };
    let Some(mut st) = st else {
        return update_status_unreadable_line();
    };
    fold_installed_activation(&mut st);
    let staged_build = st
        .staged_build
        .map(|b| b.to_string())
        .unwrap_or_else(|| "-".to_string());
    let staged_version = st.staged_version.as_deref().unwrap_or("-");
    let staged_commit = st.staged_commit.as_deref().unwrap_or("-");
    // Historical wire key: a newer stage EXISTS. Apply permission and scheduling
    // are separate facts; a stage can be blocked or require manual action.
    let relaunch_ready = st.staged_build.is_some_and(|b| b > st.current_build);
    let apply_snapshot = crate::control::control_media::call_main_within(
        proxy,
        std::time::Duration::from_secs(2),
        |reply| Wake::ReadUpdateControl { reply },
    )
    .ok();
    let apply_posture = apply_snapshot
        .as_ref()
        .map_or("unknown", |live| live.apply_posture(&st));
    let apply_policy_reason = apply_snapshot
        .as_ref()
        .and_then(|live| live.apply_policy_reason(&st));
    let apply_phase = apply_snapshot
        .as_ref()
        .and_then(|live| live.apply_phase(&st));
    let apply_held = apply_snapshot
        .as_ref()
        .and_then(|live| live.apply_held(&st));
    // failing=<consecutive>:<kind> (0 when healthy) and failing_applies=<n>, so a
    // driver sees a broken pipeline from one line.
    let failing = if st.failing_checks == 0 {
        "0".to_string()
    } else {
        // The class of the standing acquisition streak, not `failing_kind` (the
        // latest failure of any class); the apply lane has its own tokens.
        let kind = if st.failing_checks_kind.is_empty() {
            st.failing_kind.as_str()
        } else {
            st.failing_checks_kind.as_str()
        };
        format!("{}:{kind}", st.failing_checks)
    };
    // `stale_check=` flags a last-check stamp older than
    // `aterm_update::STALE_CHECK_AFTER`; absent when healthy.
    let stale_check = apply_snapshot.as_ref().map_or_else(String::new, |live| {
        let source = aterm_update::Source::resolve(live.owner.as_deref(), live.repo.as_deref());
        match aterm_update::last_check_at(st.current_build, &source) {
            Some(stamp)
                if aterm_update::rfc3339_older_than(
                    &stamp,
                    aterm_update::STALE_CHECK_AFTER.as_secs(),
                ) =>
            {
                format!(" stale_check={}", pct_encode(&stamp))
            }
            None if st.enabled && st.installable => " check_unrecorded=true".to_string(),
            _ => String::new(),
        }
    });
    // `checked_at=` is the completed-check receipt's stamp for this source and
    // build; `next_check=` is this process's loop timer (a sibling may check
    // sooner). Each is absent while unknown.
    let checked_at = apply_snapshot
        .as_ref()
        .and_then(|live| {
            let source = aterm_update::Source::resolve(live.owner.as_deref(), live.repo.as_deref());
            aterm_update::last_check_at(st.current_build, &source)
        })
        .map_or_else(String::new, |stamp| {
            format!(" checked_at={}", pct_encode(&stamp))
        });
    let next_check = aterm_update::next_check_at().map_or_else(String::new, |stamp| {
        format!(" next_check={}", pct_encode(&stamp))
    });
    // The check loop's live heartbeat: `checker_stalled=<secs>:<phase>` when
    // quiet past its phase budget, `checker_respawns=` once the watchdog replaced
    // it, `checker_deferred=` while another aterm has held the checker lock
    // through consecutive cycles without progress. Absent when healthy.
    let checker = crate::update_checker_watch::checker_status_tokens(
        aterm_update::checker_watch::checker_snapshot(),
        aterm_update::checker_watch::now_secs(),
    );
    // commit= is the running binary's source commit, staged_commit= the staged
    // build's. Widths differ (short-12 vs full-40, possibly `-dirty`), so
    // `staged_is_same_commit=` is the canonical comparison via `commit_matches`:
    // true only for a relaunch of the same source.
    let staged_is_same_commit =
        aterm_update::commit_matches(crate::build_info::git_commit(), staged_commit);
    let mut out = format!(
        "OK enabled={} current_build={} commit={} staged_build={} staged_version={} \
         staged_commit={} staged_is_same_commit={} relaunch_ready={} apply_posture={} failing={} \
         failing_applies={} persistent={}{stale_check}{checked_at}{next_check}{checker} \
         outcome={:?}\n",
        st.enabled,
        st.current_build,
        crate::build_info::git_commit(),
        staged_build,
        staged_version,
        staged_commit,
        staged_is_same_commit,
        relaunch_ready,
        apply_posture,
        failing,
        // Separate from `failing=`: acquisition and apply streaks answer
        // different questions.
        st.failing_applies,
        st.is_failing_persistently(),
        st.outcome
    );
    // Where the automatic lane is on its ladder; present only while that lane
    // owns the apply.
    if let Some(phase) = apply_phase {
        out = format!("{} apply_phase={phase}\n", out.trim_end_matches('\n'));
    }
    // What holds the automatic lane past its ladder (the restored agents'
    // relaunch, which refuses `land` too).
    if let Some(held) = apply_held {
        out = format!("{} apply_held={held}\n", out.trim_end_matches('\n'));
    }
    if let Some(reason) = apply_policy_reason {
        out = format!(
            "{} apply_policy_reason={}\n",
            out.trim_end_matches('\n'),
            pct_encode(reason)
        );
    }
    // `delivery=` (`deferred` or `blocked`) for the last check's assets, only
    // when the ledger recorded one.
    if let Some(delivery) = aterm_update::delivery() {
        let suffix = delivery.status_line_suffix();
        if !suffix.is_empty() {
            let line = out.trim_end_matches('\n');
            out = format!("{line}{suffix}\n");
        }
    }
    // `installable=false` when there is no bundle to replace (run from the DMG,
    // translocated, dev-marked): no check thread starts, so the other fields
    // would otherwise look healthy. Emitted only when false.
    if !st.installable {
        let line = out.trim_end_matches('\n');
        out = format!("{line} installable=false\n");
    }
    if let Some(native) = &st.linux {
        out = format!(
            "{} linux_installed_build={} linux_staged_build={} linux_trial={} linux_trial_starts={} linux_trial_healthy={}\n",
            out.trim_end_matches('\n'),
            native.installed_build,
            native
                .staged_build
                .map_or_else(|| "-".into(), |build| build.to_string()),
            native.trial_phase.as_deref().unwrap_or("none"),
            native.trial_starts,
            native.trial_healthy
        );
    }
    // Apply lane: `apply_refusal=` is why the lane last declined (refusals are
    // not failures and are not counted), `apply_failure=` explains the
    // `failing_applies=` streak. Pct-encoded prose, emitted only when non-empty.
    if let Some(apply) = aterm_update::apply_lane_report(st.current_build) {
        // Every value here is prose read back from a ledger, so every value is
        // pct-encoded: an embedded space or newline would split this single-line
        // Status reply and desync every following read on a pipelined connection
        // (the same hazard `changelog=` below is encoded for).
        // `trial_wait=`: this build's boot trial waiting for a window to confirm or
        // revert it, and until when (round eight, #8 and S1's second review), or the
        // recovery proof that refused a launch's confirmation of it.
        for (name, value) in [
            ("apply_refusal", apply.last_refusal.as_str()),
            ("apply_refusal_at", apply.last_refusal_at.as_str()),
            ("apply_failure", apply.last_failure.as_str()),
            ("trial_wait", apply.trial_wait.as_str()),
        ] {
            if !value.is_empty() {
                let line = out.trim_end_matches('\n');
                out = format!("{line} {name}={}\n", pct_encode(value));
            }
        }
        // The first freeze rung's seed: the rung the last dry run seeded, the
        // capture it timed and the last landed park's real cost. Each only once
        // measured.
        if let Some(capture) = &apply.handoff_capture {
            let ms = |us: u64| us as f64 / 1e3;
            if capture.capture_us > 0 {
                let line = out.trim_end_matches('\n');
                out = format!(
                    "{line} freeze_seed_ms={} handoff_capture_ms={:.1}\n",
                    capture.freeze_seed_ms,
                    ms(capture.capture_us)
                );
            }
            if capture.park_us > 0 {
                let line = out.trim_end_matches('\n');
                out = format!("{line} handoff_park_ms={:.1}\n", ms(capture.park_us));
            }
        }
    }
    // The staged build's notes as a pct-encoded `changelog=` token: `update` is
    // Status-framed (one line), so multi-line notes would be dropped and desync
    // a pipelined connection. Absent notes append nothing.
    if let Some(cl) = st
        .changelog
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let line = out.trim_end_matches('\n');
        out = format!("{line} changelog={}\n", pct_encode(cl));
    }
    out
}

/// Fold in the activation lane the ledger cannot see: a verified installed
/// bundle newer than this process is staged in memory by the GUI reducer (no
/// `ready.toml`) and outranks any download on disk. Derived from the same
/// source the reducer uses, so the line and `update apply` agree. The probe
/// runs codesign and is cached.
fn fold_installed_activation(st: &mut aterm_update::UpdateStatus) {
    if st.enabled
        && let Some(installed) = cached_installed_update_facts()
        && installed.build_number > st.current_build
        && !installed.yanked
        && crate::native_updater_service::usable_commit_identity(&installed.git_commit)
    {
        st.staged_build = Some(installed.build_number);
        st.staged_version = installed
            .version
            .clone()
            .or_else(|| Some(format!("build {}", installed.build_number)));
        st.staged_commit = Some(installed.git_commit.clone());
        st.staged_dmg_sha256 = None;
        st.changelog = None;
        // The reducer's sentence for the same state, word for word, so this verb and
        // Software Update never describe it twice.
        st.outcome = format!(
            "{} (ledger: {})",
            crate::native_updater_service::NativeUpdaterService::installed_activation_outcome(
                installed.version.as_deref()
            ),
            st.outcome
        );
    }
}

/// The scope a connection's first line authenticates to (with any folded-in
/// verb), or the denial `serve` logs before `ERR auth`.
///
/// Tier 1: the per-instance token is `Scope::Owner`. Tier 2: otherwise it is
/// tried as an edge token against the active session's table (`Scope::Edge`),
/// which is what a forwarded connection presents, so it never forwards on
/// (`proxy_forward_plan` forwards Owner-class only). A line with no token is
/// its own refusal kind (`control_denials`).
///
/// The harness's in-memory token is the instance's power under another
/// name ([`Principal::Harness`]): Owner, labelled for the session log only.
fn first_line_scope(
    first: &str,
    token: &str,
    active_ctx: impl FnOnce() -> Option<Arc<SessionCtx>>,
) -> Result<(Scope, Principal, Option<String>), &'static str> {
    match control_auth::check_auth_line(first, token) {
        AuthOutcome::Ok(verb) => Ok((Scope::Owner, Principal::Instance, verb)),
        AuthOutcome::Denied => {
            if let AuthOutcome::Ok(verb) = control_auth::check_harness_line(first) {
                return Ok((Scope::Owner, Principal::Harness, verb));
            }
            // A blind dial (no token) is its own refusal kind.
            let line = first.strip_suffix('\r').unwrap_or(first);
            let head = line.split_once(' ').map_or(line, |(head, _)| head);
            if !matches!(head, "AUTH" | "TOKEN") {
                return Err(control_denials::NO_TOKEN);
            }
            let ctx = active_ctx().ok_or(control_denials::NO_CONTEXT)?;
            // The connect-time op proves the token is a LIVE edge; it is not
            // stored — per-request `decide_edge` re-derives it.
            edge_scope_from_first_line(first, &ctx)
                .map(|(_op, tok, verb)| (Scope::Edge(tok), Principal::Instance, verb))
                .ok_or(control_denials::WRONG_TOKEN)
        }
    }
}

/// Interpret the handshake hex as an edge token against the active session's
/// table: the authorized op, the presented token (re-checked against a target's
/// table on cross-session calls) and any folded-in verb.
fn edge_scope_from_first_line(
    first: &str,
    ctx: &SessionCtx,
) -> Option<(Op, EdgeToken, Option<String>)> {
    let first = first.strip_suffix('\r').unwrap_or(first);
    let (head, rest) = first.split_once(' ')?;
    let (hex, inline) = match head {
        "AUTH" => (rest.trim_end(), None),
        "TOKEN" => match rest.split_once(' ') {
            Some((h, v)) => (h.trim_end(), Some(v.to_string())),
            None => (rest.trim_end(), None),
        },
        _ => return None,
    };
    let tok = EdgeToken::from_hex(hex)?;
    let edges = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
    // authorize() is the connect-time op-resolving lookup: it checks dst == self_id
    // AND nonce, fail-closed.
    let op = edges.authorize(&tok, &ctx.self_id, &ctx.nonce)?;
    Some((op, tok, inline))
}

/// A request for the main thread to render the live screen to a PNG.
///
/// The control thread fills the confined target (canonical dir plus a single
/// filename from `confine_image_path`), sends [`Wake::Control`] and blocks on
/// `reply` for `(width, height)`. Passing dir plus filename lets the writer
/// `openat` under a dir fd, so no path component can be swapped after the check.
pub(crate) struct ImageReq {
    /// A confined image target for file output, or a descriptor-free sentinel
    /// for `--bytes` that the encode worker consumes before any file operation.
    pub target: control_auth::ConfinedImage,
    /// Clean capture (`image plain <name>`): empty the host-owned decoration
    /// layers (cursor trail, glow, sparkle words, Scene) to get bare terminal
    /// pixels.
    pub clean: bool,
    /// Render the window displaying this session id (`@<sid> image`) instead of
    /// the frontmost window (`None`).
    pub session: Option<u64>,
    /// `image --bytes`: return the PNG over the wire instead of writing a file,
    /// for a driver that cannot read this host's runtime directory.
    pub want_bytes: bool,
    /// `image --meta`: return the encoded-frame identity; separate so plain
    /// captures skip the hash and inventory cost.
    pub want_metadata: bool,
    /// Identity of the exact encoded frame, set by the main thread once the final
    /// RGBA and every retained leaf identity are known, and read by the control
    /// worker after the image reply.
    pub frame_metadata: Arc<std::sync::OnceLock<ImageFrameMetadata>>,
    /// Linearizes a control-thread timeout against final artifact publication:
    /// if the timeout wins, the requested path never becomes observable.
    pub cancel: CaptureCancellation,
    /// Reply channel: `(width, height, png?)`, with `png` only in `--bytes` mode
    /// (else the PNG is at the confined path); `Ok((0, 0, None))` when there was
    /// nothing to capture; `Err` when no window displays the target or the
    /// encode/write failed.
    pub reply: Sender<ImageReply>,
    /// Global artifact capacity reserved before queueing. File replies keep it
    /// through socket write, ACK and any failed-ACK quarantine; byte replies
    /// release it after the flush. Declared last so teardown closes accounted
    /// resources first.
    pub(crate) handoff: ReplyRetentionPermit,
}

const CAPTURE_CANCEL_LIVE: u8 = 0;
const CAPTURE_CANCELLED: u8 = 1;
const CAPTURE_COMMIT_AUTHORIZED: u8 = 2;

/// One-shot cancellation/publication election for `image` and `window`.
///
/// The encode worker calls [`Self::authorize_commit`] immediately before the
/// handle-relative final-name operation. The control worker calls
/// [`Self::cancel`] when its bounded wait expires. Exactly one can win.
#[derive(Clone, Debug, Default)]
pub(crate) struct CaptureCancellation(Arc<AtomicU8>);

impl CaptureCancellation {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns true only when cancellation won before publication authority.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "Cancel",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    pub(crate) fn cancel(&self) -> bool {
        self.0
            .compare_exchange(
                CAPTURE_CANCEL_LIVE,
                CAPTURE_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[must_use]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == CAPTURE_CANCELLED
    }

    /// Returns true only for the single worker that wins the irrevocable
    /// final-name publication boundary.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "AuthorizeCommit",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    pub(crate) fn authorize_commit(&self) -> bool {
        self.0
            .compare_exchange(
                CAPTURE_CANCEL_LIVE,
                CAPTURE_COMMIT_AUTHORIZED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

/// A filesystem-artifact guard carried to the socket edge. `prepare_write`
/// revalidates names an OK response certifies and takes publication ownership
/// before any OK bytes are visible; the guard lives through write, flush and the
/// client's acknowledgement. Inline PNG bytes use the write-only handoff
/// instead.
pub(crate) trait WireRetention: Send {
    fn prepare_write(&mut self) -> Result<(), String>;
}

/// Bound on queued, in-flight and quarantined artifact handoffs together. The
/// descriptor-unit cap also counts retained directory-chain depth, so a few
/// deep paths cannot defeat a count-only limit.
const ARTIFACT_HANDOFF_LIMIT: usize = 4;
const ARTIFACT_HANDOFF_SUFFIX_UNITS: usize =
    crate::pinned_dir::PINNED_DIR_OPERATION_DESCRIPTOR_UNITS;
const ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT: usize =
    crate::pinned_dir::PINNED_DIR_OPEN_COMPONENT_LIMIT + ARTIFACT_HANDOFF_SUFFIX_UNITS;
const ARTIFACT_CLEANUP_TEST_LIMIT: usize = 32;
pub(crate) const ARTIFACT_HANDOFF_BUSY: &str = "other captures are still pending; retry";

#[derive(Debug, Default, PartialEq, Eq)]
struct ArtifactHandoffUsage {
    live: usize,
    descriptor_units: usize,
}

#[derive(Debug)]
struct ArtifactHandoffPermit {
    usage: &'static Mutex<ArtifactHandoffUsage>,
    descriptor_units: usize,
    descriptor_limit: usize,
}

/// Capacity reserved before an artifact job starts; binding it to a guard
/// carries the permit through reply, ACK and failed-ACK quarantine.
#[derive(Debug)]
pub(crate) struct ReplyRetentionPermit(ArtifactHandoffPermit);

/// Admission for an in-memory artifact body: no path to preserve after the
/// bounded flush, so it is outside the acknowledgement/quarantine machine.
pub(crate) struct WriteOnlyRetention {
    _permit: ArtifactHandoffPermit,
}

/// One reserved descriptor chain for the asynchronous retention worker,
/// separate from the video-retention lane so best-effort GC never starves an
/// armed video's sweep.
pub(crate) struct ArtifactCleanupPermit {
    live: &'static AtomicUsize,
}

impl Drop for ArtifactCleanupPermit {
    fn drop(&mut self) {
        let previous = self.live.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "artifact cleanup permit underflow");
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct VideoRetentionUsage {
    live: usize,
    descriptor_units: usize,
}

/// A slot reserved while one video artifact is live. The final lease hands it,
/// with its pinned root, to the cleanup worker, so the final Drop's enqueue
/// cannot block or fail.
#[derive(Debug)]
pub(crate) struct VideoRetentionPermit {
    usage: &'static Mutex<VideoRetentionUsage>,
    descriptor_units: usize,
}

impl Drop for VideoRetentionPermit {
    fn drop(&mut self) {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(usage.live > 0, "video retention permit underflow");
        debug_assert!(
            usage.descriptor_units >= self.descriptor_units,
            "video retention descriptor budget underflow"
        );
        usage.live = usage.live.saturating_sub(1);
        usage.descriptor_units = usage.descriptor_units.saturating_sub(self.descriptor_units);
    }
}

#[cfg(test)]
impl ReplyRetentionPermit {
    /// Isolate unit fixtures from the process-global admission pool so parallel
    /// tests do not race for its slots.
    pub(crate) fn unmetered_for_test() -> Self {
        static USAGE: Mutex<ArtifactHandoffUsage> = Mutex::new(ArtifactHandoffUsage {
            live: 0,
            descriptor_units: 0,
        });
        Self(
            ArtifactHandoffPermit::try_acquire_from(&USAGE, usize::MAX, usize::MAX, 1)
                .expect("unmetered test permit capacity"),
        )
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactHandoffCapacity",
        action = "Acquire",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_handoff_capacity"
    )
)]
fn artifact_handoff_acquire_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactHandoffCapacity",
        action = "RefuseAtCap",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_handoff_capacity"
    )
)]
fn artifact_handoff_refuse_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactHandoffCapacity",
        action = "Release",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_handoff_capacity"
    )
)]
fn artifact_handoff_release_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactHandoffCapacity",
        action = "ReconcileGrow",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_handoff_capacity"
    )
)]
fn artifact_handoff_reconcile_grow_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactHandoffCapacity",
        action = "ReconcileShrink",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_handoff_capacity"
    )
)]
fn artifact_handoff_reconcile_shrink_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactHandoffCapacity",
        action = "RefuseReconcile",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_handoff_capacity"
    )
)]
fn artifact_handoff_refuse_reconcile_anchor() {}

impl ArtifactHandoffPermit {
    fn try_acquire_from(
        usage: &'static Mutex<ArtifactHandoffUsage>,
        live_limit: usize,
        descriptor_limit: usize,
        descriptor_units: usize,
    ) -> Option<Self> {
        let mut current = usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let available = current.live < live_limit
            && descriptor_units > 0
            && descriptor_units <= descriptor_limit
            && current.descriptor_units <= descriptor_limit - descriptor_units;
        if available {
            current.live += 1;
            current.descriptor_units += descriptor_units;
            drop(current);
            artifact_handoff_acquire_anchor();
            Some(Self {
                usage,
                descriptor_units,
                descriptor_limit,
            })
        } else {
            drop(current);
            artifact_handoff_refuse_anchor();
            None
        }
    }

    fn try_acquire(descriptor_units: usize) -> Option<Self> {
        static USAGE: Mutex<ArtifactHandoffUsage> = Mutex::new(ArtifactHandoffUsage {
            live: 0,
            descriptor_units: 0,
        });
        Self::try_acquire_from(
            &USAGE,
            ARTIFACT_HANDOFF_LIMIT,
            ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT,
            descriptor_units,
        )
    }

    /// Replace the provisional charge with the exact pinned path's depth,
    /// atomically; on failure the old charge stays so the drop balances.
    fn try_reconcile_descriptor_units(&mut self, descriptor_units: usize) -> bool {
        if descriptor_units == 0 || descriptor_units > self.descriptor_limit {
            artifact_handoff_refuse_reconcile_anchor();
            return false;
        }
        let previous_units = self.descriptor_units;
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(usage.descriptor_units >= self.descriptor_units);
        let other_units = usage.descriptor_units.saturating_sub(self.descriptor_units);
        if other_units > self.descriptor_limit - descriptor_units {
            drop(usage);
            artifact_handoff_refuse_reconcile_anchor();
            return false;
        }
        usage.descriptor_units = other_units + descriptor_units;
        self.descriptor_units = descriptor_units;
        drop(usage);
        match descriptor_units.cmp(&previous_units) {
            std::cmp::Ordering::Greater => artifact_handoff_reconcile_grow_anchor(),
            std::cmp::Ordering::Less => artifact_handoff_reconcile_shrink_anchor(),
            std::cmp::Ordering::Equal => {}
        }
        true
    }
}

impl Drop for ArtifactHandoffPermit {
    fn drop(&mut self) {
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        debug_assert!(usage.live > 0, "artifact handoff permit underflow");
        debug_assert!(
            usage.descriptor_units >= self.descriptor_units,
            "artifact handoff descriptor budget underflow"
        );
        usage.live = usage.live.saturating_sub(1);
        usage.descriptor_units = usage.descriptor_units.saturating_sub(self.descriptor_units);
        drop(usage);
        artifact_handoff_release_anchor();
    }
}

/// Type-erased ownership that stays live until a reply is consumed and
/// acknowledged; concrete guards revalidate filesystem identities at the wire.
pub(crate) struct ReplyRetention {
    guard: Option<Box<dyn WireRetention>>,
    phase: ReplyRetentionPhase,
    _permit: Option<ArtifactHandoffPermit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplyRetentionPhase {
    Queued,
    Prepared,
    Classified,
}

impl ReplyRetention {
    #[cfg(all(test, unix))]
    pub(crate) fn try_reserve() -> Option<ReplyRetentionPermit> {
        Self::try_reserve_in_memory()
    }

    /// Reserve a live-object slot for a target-free body; no directory chain is
    /// charged because none is owned.
    pub(crate) fn try_reserve_in_memory() -> Option<ReplyRetentionPermit> {
        ArtifactHandoffPermit::try_acquire(1).map(ReplyRetentionPermit)
    }

    /// Reserve the count slot plus a lexical descriptor estimate before resolving
    /// a path; `PinnedDir` reconciles the exact depth before opening. A saturated
    /// request does no filesystem I/O.
    pub(crate) fn try_reserve_for_path(path: &std::path::Path) -> Option<ReplyRetentionPermit> {
        let descriptor_units = artifact_descriptor_units(path);
        ArtifactHandoffPermit::try_acquire(descriptor_units).map(ReplyRetentionPermit)
    }

    /// Transfer one open directory chain to the cleanup worker. Paths outside the
    /// pinned-depth contract are refused; at most one such capability outlives
    /// its reply permit.
    pub(crate) fn try_reserve_cleanup_for_path(
        path: &std::path::Path,
    ) -> Option<ArtifactCleanupPermit> {
        if artifact_descriptor_units(path) > ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT {
            return None;
        }
        static LIVE: AtomicUsize = AtomicUsize::new(0);
        let limit = if cfg!(test) {
            ARTIFACT_CLEANUP_TEST_LIMIT
        } else {
            1
        };
        LIVE.try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current < limit).then_some(current + 1)
        })
        .ok()
        .map(|_| ArtifactCleanupPermit { live: &LIVE })
    }

    fn try_reserve_video_retention_from(
        usage: &'static Mutex<VideoRetentionUsage>,
        live_limit: usize,
        descriptor_limit: usize,
        descriptor_units: usize,
    ) -> Option<VideoRetentionPermit> {
        let mut current = usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let available = current.live < live_limit
            && descriptor_units > 0
            && descriptor_units <= descriptor_limit
            && current.descriptor_units <= descriptor_limit - descriptor_units;
        if !available {
            return None;
        }
        current.live += 1;
        current.descriptor_units += descriptor_units;
        Some(VideoRetentionPermit {
            usage,
            descriptor_units,
        })
    }

    /// Pre-reserve the root capacity an armed video sweep will hand the cleanup
    /// worker, independent of best-effort cleanup admission.
    pub(crate) fn try_reserve_video_retention_for_path(
        path: &std::path::Path,
    ) -> Option<VideoRetentionPermit> {
        static USAGE: Mutex<VideoRetentionUsage> = Mutex::new(VideoRetentionUsage {
            live: 0,
            descriptor_units: 0,
        });
        let units = artifact_descriptor_units(path);
        let (live_limit, descriptor_limit) = if cfg!(test) {
            (
                ARTIFACT_CLEANUP_TEST_LIMIT,
                ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT.saturating_mul(ARTIFACT_CLEANUP_TEST_LIMIT),
            )
        } else {
            (ARTIFACT_HANDOFF_LIMIT, ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT)
        };
        Self::try_reserve_video_retention_from(&USAGE, live_limit, descriptor_limit, units)
    }

    #[cfg(all(test, unix))]
    fn try_new<G: WireRetention + 'static>(guard: G) -> Result<Self, G> {
        let Some(permit) = Self::try_reserve() else {
            return Err(guard);
        };
        Ok(permit.retain(guard))
    }

    #[cfg(test)]
    fn new(guard: impl WireRetention + 'static) -> Self {
        Self {
            guard: Some(Box::new(guard)),
            phase: ReplyRetentionPhase::Queued,
            _permit: None,
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "PrepareWire",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    pub(crate) fn prepare_write(&mut self) -> Result<(), String> {
        let result = self
            .guard
            .as_mut()
            .expect("live reply retention owns its guard")
            .prepare_write();
        if result.is_ok() {
            self.phase = ReplyRetentionPhase::Prepared;
        }
        result
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "AcknowledgePeer",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn acknowledge_peer_anchor(&mut self) {
        self.phase = ReplyRetentionPhase::Classified;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "AcknowledgeFailed",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn acknowledge_failed_anchor(&mut self) {
        self.phase = ReplyRetentionPhase::Classified;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "PrepareFailed",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn prepare_failed_anchor(&mut self) {
        self.phase = ReplyRetentionPhase::Classified;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "WriteFailed",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn write_failed_anchor(&mut self) {
        self.phase = ReplyRetentionPhase::Classified;
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "AbortQueued",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn abort_queued_anchor(&self) {}

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "ReleaseGuard",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn release_guard_anchor(&self) {}
}

impl ReplyRetentionPermit {
    /// Reconcile the provisional charge after path resolution, before the
    /// ancestor chain opens (called by `PinnedDir`).
    pub(crate) fn try_reconcile_for_resolved_path(&mut self, path: &std::path::Path) -> bool {
        self.0
            .try_reconcile_descriptor_units(artifact_descriptor_units(path))
    }

    pub(crate) fn retain(self, guard: impl WireRetention + 'static) -> ReplyRetention {
        ReplyRetention {
            guard: Some(Box::new(guard)),
            phase: ReplyRetentionPhase::Queued,
            _permit: Some(self.0),
        }
    }

    pub(crate) fn release_path_and_retain_in_memory<T>(
        mut self,
        path_capability: T,
    ) -> WriteOnlyRetention {
        // Close the filesystem capability before changing its charge.
        drop(path_capability);
        assert!(
            self.0.try_reconcile_descriptor_units(1),
            "shrinking a live artifact permit to its in-memory charge must fit"
        );
        WriteOnlyRetention { _permit: self.0 }
    }
}

fn artifact_descriptor_units(path: &std::path::Path) -> usize {
    path.components()
        .count()
        .saturating_add(ARTIFACT_HANDOFF_SUFFIX_UNITS)
}

impl Drop for ReplyRetention {
    fn drop(&mut self) {
        if self.phase == ReplyRetentionPhase::Queued {
            self.abort_queued_anchor();
            self.phase = ReplyRetentionPhase::Classified;
        }
        // Fire the refinement anchor after the guard really drops (a Drop body
        // runs before its fields), so Tier-1 conformance sees the release.
        drop(self.guard.take());
        self.release_guard_anchor();
    }
}

/// Two completion boundaries: filesystem publications need a peer ACK; an
/// in-memory body only needs the bounded write and flush.
pub(crate) enum ReplyHandoff {
    Acknowledged(ReplyRetention),
    WriteOnly(WriteOnlyRetention),
}

impl ReplyHandoff {
    pub(crate) fn prepare_write(&mut self) -> Result<(), String> {
        match self {
            Self::Acknowledged(retention) => retention.prepare_write(),
            Self::WriteOnly(_) => Ok(()),
        }
    }

    fn prepare_failed_anchor(&mut self) {
        match self {
            Self::Acknowledged(retention) => retention.prepare_failed_anchor(),
            Self::WriteOnly(_) => unreachable!("write-only handoff preparation cannot fail"),
        }
    }
}

/// A value coupled to artifact retention through the complete wire handoff.
pub(crate) struct Retained<T> {
    pub(crate) value: T,
    pub(crate) retention: Option<ReplyHandoff>,
}

impl<T> Retained<T> {
    pub(crate) fn plain(value: T) -> Self {
        Self {
            value,
            retention: None,
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "ArtifactReplyPublication",
            action = "QueueGuard",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    pub(crate) fn guarded(value: T, retention: ReplyRetention) -> Self {
        Self {
            value,
            retention: Some(ReplyHandoff::Acknowledged(retention)),
        }
    }

    pub(crate) fn write_retained(value: T, retention: WriteOnlyRetention) -> Self {
        Self {
            value,
            retention: Some(ReplyHandoff::WriteOnly(retention)),
        }
    }

    pub(crate) fn into_parts(self) -> (T, Option<ReplyHandoff>) {
        (self.value, self.retention)
    }
}

impl<T> std::ops::Deref for Retained<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Retained<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Retained")
            .field("value", &self.value)
            .field("has_guard", &self.retention.is_some())
            .finish()
    }
}

impl<T: PartialEq> PartialEq for Retained<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq> Eq for Retained<T> {}

impl PartialEq<&str> for Retained<String> {
    fn eq(&self, other: &&str) -> bool {
        self.value == *other
    }
}

impl<T: std::fmt::Display> std::fmt::Display for Retained<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.value.fmt(formatter)
    }
}

/// One RPC response plus any artifact ownership that must span its socket write
/// (handles until ACK for paths, a memory permit until flush for inline bytes).
pub(crate) struct ControlReply {
    body: String,
    retention: Option<ReplyHandoff>,
    /// Released once the reply is written and flushed (or cannot be): what a
    /// seamless update's Commit waits on before `_exit` ([`ReplyWire`]).
    written: Option<ReplyWritten>,
}

/// A latch the connection releases once a reply is on the wire (written and
/// flushed, or failed, closed or dropped). The main thread holds [`ReplyWire`];
/// the connection thread carries [`ReplyWritten`] in its [`ControlReply`], and
/// dropping it releases.
///
/// A seamless update's Commit waits (bounded) on these so its `_exit` cannot
/// land between handing a `video` reply to the channel and writing it.
#[derive(Clone, Debug, Default)]
pub(crate) struct ReplyWire(Arc<(Mutex<bool>, std::sync::Condvar)>);

/// The connection thread's half of a [`ReplyWire`]; dropping it releases the
/// latch.
#[derive(Debug)]
pub(crate) struct ReplyWritten(ReplyWire);

impl ReplyWire {
    /// A fresh, unreleased latch and the half that releases it.
    #[must_use]
    pub(crate) fn new() -> (Self, ReplyWritten) {
        let wire = Self::default();
        (wire.clone(), ReplyWritten(wire))
    }

    /// Whether the reply is written (or never will be).
    #[must_use]
    pub(crate) fn is_released(&self) -> bool {
        *self
            .0
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait until the latch is released or `deadline` passes; whether it was
    /// released.
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) fn wait_released(&self, deadline: std::time::Instant) -> bool {
        let (released, changed) = &*self.0;
        let mut released = released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*released {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            released = changed
                .wait_timeout(released, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        true
    }
}

impl Drop for ReplyWritten {
    fn drop(&mut self) {
        let (released, changed) = &*(self.0).0;
        *released
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

/// What a verb handler hands the connection beside its body: the artifact
/// retention that must span the socket write, and the [`ReplyWritten`] latch
/// the write releases.
#[derive(Default)]
pub(crate) struct ReplyTail {
    retention: Option<ReplyHandoff>,
    written: Option<ReplyWritten>,
}

impl ControlReply {
    /// This reply with the unread-input warning on its first line
    /// ([`crate::input_stall::with_warning`]).
    fn warned(mut self, warning: Option<&str>) -> Self {
        if warning.is_some() {
            self.body = crate::input_stall::with_warning(std::mem::take(&mut self.body), warning);
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn guarded(body: String, retention: Option<ReplyRetention>) -> Self {
        Self {
            body,
            retention: retention.map(ReplyHandoff::Acknowledged),
            written: None,
        }
    }

    pub(crate) fn with_handoff(body: String, retention: Option<ReplyHandoff>) -> Self {
        Self {
            body,
            retention,
            written: None,
        }
    }

    /// This reply, releasing `written` once the connection has written it.
    #[must_use]
    pub(crate) fn releasing_on_write(mut self, written: ReplyWritten) -> Self {
        self.written = Some(written);
        self
    }

    fn with_tail(body: String, tail: ReplyTail) -> Self {
        Self {
            body,
            retention: tail.retention,
            written: tail.written,
        }
    }

    fn into_body_retaining(self, slot: &mut ReplyTail) -> String {
        let Self {
            body,
            retention,
            written,
        } = self;
        slot.retention = retention;
        slot.written = written;
        body
    }

    #[cfg(test)]
    pub(crate) fn prepare_retention_for_test(&mut self) -> Result<(), String> {
        self.retention
            .as_mut()
            .map_or_else(|| Ok(()), ReplyHandoff::prepare_write)
    }
}

impl From<String> for ControlReply {
    fn from(body: String) -> Self {
        Self {
            body,
            retention: None,
            written: None,
        }
    }
}

impl From<&str> for ControlReply {
    fn from(body: &str) -> Self {
        body.to_string().into()
    }
}

impl std::ops::Deref for ControlReply {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.body
    }
}

impl std::fmt::Debug for ControlReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlReply")
            .field("body", &self.body)
            .field("has_guard", &self.retention.is_some())
            .finish()
    }
}

impl PartialEq<&str> for ControlReply {
    fn eq(&self, other: &&str) -> bool {
        self.body == *other
    }
}

/// `image` reply payload: `(width, height, Some(png))` with `--bytes`,
/// `(width, height, None)` when written to the confined file, `(0, 0, None)`
/// with nothing to capture (no window is an `Err`).
pub(crate) type ImageReply = Result<Retained<(u32, u32, Option<Vec<u8>>)>, String>;
pub(crate) type WindowReply = Result<Retained<(u32, u32)>, String>;

/// One retained leaf contributing pixels to an `image --meta` capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageLeafFrameMetadata {
    pub(crate) kind: &'static str,
    pub(crate) view: u64,
    pub(crate) session: Option<u64>,
    pub(crate) focused: bool,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) snapshot_seq: Option<u64>,
    pub(crate) instance: Option<u64>,
    pub(crate) generation: Option<u64>,
    pub(crate) geometry: Option<u64>,
    pub(crate) config_revision: Option<u64>,
    pub(crate) update_revision: Option<u64>,
    pub(crate) document_seq: Option<u64>,
    pub(crate) presentation_revision: Option<u64>,
    pub(crate) paint_revision: Option<u64>,
    pub(crate) compiled_fingerprint: Option<u64>,
    pub(crate) raster_fingerprint: Option<u64>,
}

impl ImageLeafFrameMetadata {
    fn wire_token(&self) -> String {
        let optional =
            |value: Option<u64>| value.map_or_else(|| "-".to_string(), |value| value.to_string());
        let optional_hex = |value: Option<u64>| {
            value.map_or_else(|| "-".to_string(), |value| format!("{value:016x}"))
        };
        format!(
            "v{}:{}:s{}:f{}:{}x{}:seq{}:i{}:g{}:geom{}:cfg{}:upd{}:doc{}:pres{}:paint{}:compiled{}:raster{}",
            self.view,
            self.kind,
            optional(self.session),
            u8::from(self.focused),
            self.width,
            self.height,
            optional(self.snapshot_seq),
            optional(self.instance),
            optional(self.generation),
            optional_hex(self.geometry),
            optional(self.config_revision),
            optional(self.update_revision),
            optional(self.document_seq),
            optional(self.presentation_revision),
            optional_hex(self.paint_revision),
            optional_hex(self.compiled_fingerprint),
            optional_hex(self.raster_fingerprint),
        )
    }
}

/// `image --meta` identity of the encoded capture (version 2): the final RGBA
/// bytes, the retained composite and every leaf. A singular `view` is emitted
/// for a one-leaf frame (compiled stamp for native content only); composites use
/// `-` and list their leaves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageFrameMetadata {
    pub(crate) frame_kind: &'static str,
    pub(crate) phase: &'static str,
    pub(crate) window: u64,
    pub(crate) view: Option<u64>,
    pub(crate) generation: Option<u64>,
    pub(crate) config_revision: Option<u64>,
    pub(crate) update_revision: Option<u64>,
    pub(crate) document_seq: Option<u64>,
    pub(crate) presentation_revision: Option<u64>,
    pub(crate) paint_revision: Option<u64>,
    pub(crate) capture_serial: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixel_fingerprint: u64,
    pub(crate) compiled_fingerprint: Option<u64>,
    pub(crate) raster_fingerprint: u64,
    pub(crate) raster_model_fingerprint: u64,
    pub(crate) raster_geometry: u64,
    pub(crate) overlay_fingerprint: u64,
    pub(crate) theme_fingerprint: u64,
    pub(crate) leaves: Vec<ImageLeafFrameMetadata>,
}

impl ImageFrameMetadata {
    pub(crate) fn wire_fields(&self) -> String {
        let optional =
            |value: Option<u64>| value.map_or_else(|| "-".to_string(), |value| value.to_string());
        let optional_hex = |value: Option<u64>| {
            value.map_or_else(|| "-".to_string(), |value| format!("{value:016x}"))
        };
        let leaves = self
            .leaves
            .iter()
            .map(ImageLeafFrameMetadata::wire_token)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "image-meta-version=2 frame-kind={} frame-phase={} window={} view={} generation={} config-revision={} update-revision={} document-seq={} presentation-revision={} paint-revision={} capture-serial={} dimensions={}x{} pixel-fingerprint={:016x} compiled-fingerprint={} raster-fingerprint={:016x} raster-model-fingerprint={:016x} raster-geometry={:016x} overlay-fingerprint={:016x} theme-fingerprint={:016x} leaf-count={} leaves={}",
            self.frame_kind,
            self.phase,
            self.window,
            optional(self.view),
            optional(self.generation),
            optional(self.config_revision),
            optional(self.update_revision),
            optional(self.document_seq),
            optional(self.presentation_revision),
            optional_hex(self.paint_revision),
            self.capture_serial,
            self.width,
            self.height,
            self.pixel_fingerprint,
            optional_hex(self.compiled_fingerprint),
            self.raster_fingerprint,
            self.raster_model_fingerprint,
            self.raster_geometry,
            self.overlay_fingerprint,
            self.theme_fingerprint,
            self.leaves.len(),
            leaves,
        )
    }
}

/// Shared queue of pending [`ImageReq`]s, drained by the main thread.
pub(crate) type ImageQueue = Arc<Mutex<VecDeque<ImageReq>>>;

/// Fixed admission queue between the socket listener and the control workers.
/// The listener only does a bounded non-blocking enqueue; workers are created
/// once at startup, since spawning a thread per connection can stall for seconds
/// under macOS thread pressure.
pub(crate) struct BoundedConnectionQueue<T> {
    capacity: usize,
    items: Mutex<VecDeque<T>>,
    ready: Condvar,
}

/// Admission semaphore over queued plus running jobs, so a peer either owns a
/// runnable lane or immediately gets `ERR ... busy; retry` (bounding the queue
/// alone would let busy workers strand later peers). Capacity is the number of
/// workers that actually started.
///
/// A resumed item ([`Self::submit_resumed`], a parked connection turned
/// readable, `control_lanes.rs`) is never refused: it queues behind running
/// work, charged to the same counter, bounded by the open-connection bound.
pub(crate) struct BoundedDispatch<T> {
    inbox: BoundedConnectionQueue<T>,
    max_capacity: usize,
    capacity: AtomicUsize,
    outstanding: AtomicUsize,
}

struct DispatchCompletion<'a, T> {
    dispatch: &'a BoundedDispatch<T>,
}

impl<T> Drop for DispatchCompletion<'_, T> {
    fn drop(&mut self) {
        self.dispatch.complete();
    }
}

impl<T> BoundedDispatch<T> {
    #[must_use]
    pub(crate) fn new(max_capacity: usize) -> Self {
        Self::with_resume_room(max_capacity, 0)
    }

    /// [`Self::new`], with inbox room for `resume_room` resumed items beyond
    /// the `max_capacity` fresh admissions.
    #[must_use]
    pub(crate) fn with_resume_room(max_capacity: usize, resume_room: usize) -> Self {
        Self {
            inbox: BoundedConnectionQueue::new(max_capacity + resume_room),
            max_capacity,
            // Startup publishes the number of workers that actually started.
            // Until then admission fails closed.
            capacity: AtomicUsize::new(0),
            outstanding: AtomicUsize::new(0),
        }
    }

    /// Publish the number of reusable workers that actually exist. Called once,
    /// before the socket becomes externally discoverable.
    pub(crate) fn set_capacity(&self, capacity: usize) {
        assert!(
            capacity <= self.max_capacity,
            "dispatch capacity exceeds its fixed inbox"
        );
        assert_eq!(
            self.outstanding.load(Ordering::Acquire),
            0,
            "dispatch capacity changes only before admission"
        );
        self.capacity.store(capacity, Ordering::Release);
    }

    /// Reserve a lane and enqueue without blocking the listener. The reservation
    /// stays charged until [`Self::complete`].
    pub(crate) fn try_submit(&self, item: T) -> Result<(), T> {
        let mut current = self.outstanding.load(Ordering::Acquire);
        loop {
            let capacity = self.capacity.load(Ordering::Acquire);
            if current >= capacity {
                return Err(item);
            }
            match self.outstanding.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }

        // `outstanding <= capacity <= inbox.capacity`, so this can fail only if
        // those invariants regress. Preserve fail-safe ownership even then.
        self.inbox.try_push(item).inspect_err(|_| {
            let previous = self.outstanding.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0);
        })
    }

    /// Queue a resumed item behind running work, charged like a fresh admission.
    /// `Err` only if the inbox's resume room is sized below the caller's bound.
    #[cfg(unix)]
    pub(crate) fn submit_resumed(&self, item: T) -> Result<(), T> {
        self.outstanding.fetch_add(1, Ordering::AcqRel);
        self.inbox.try_push(item).inspect_err(|_| {
            let previous = self.outstanding.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0);
        })
    }

    pub(crate) fn pop(&self) -> T {
        self.inbox.pop()
    }

    fn completion_guard(&self) -> DispatchCompletion<'_, T> {
        DispatchCompletion { dispatch: self }
    }

    /// One turn of a worker lane: block for the next admission and serve it under
    /// its completion guard, which releases the lane even on panic. The panic is
    /// contained; returns `false` when `serve` panicked.
    pub(crate) fn serve_next(&self, serve: impl FnOnce(T)) -> bool {
        let item = self.pop();
        let _completion = self.completion_guard();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(item))).is_ok()
    }

    /// Release one queued-or-running admission after its worker has completely
    /// returned, including panic recovery.
    pub(crate) fn complete(&self) {
        let previous = self.outstanding.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "completed control work was never admitted");
    }

    /// Whether an admission would find a lane now.
    pub(crate) fn has_room(&self) -> bool {
        self.outstanding.load(Ordering::Acquire) < self.capacity.load(Ordering::Acquire)
    }

    /// Whether one more lane fits the fixed inbox.
    pub(crate) fn can_grow(&self) -> bool {
        self.capacity.load(Ordering::Acquire) < self.max_capacity
    }

    /// Publish one more lane that now exists (grown after admission began);
    /// `false` at the fixed inbox's bound, where nothing is published.
    pub(crate) fn add_capacity(&self) -> bool {
        self.capacity
            .try_update(Ordering::AcqRel, Ordering::Acquire, |c| {
                (c < self.max_capacity).then_some(c + 1)
            })
            .is_ok()
    }

    #[cfg(test)]
    pub(crate) fn outstanding(&self) -> usize {
        self.outstanding.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity.load(Ordering::Acquire)
    }
}

impl<T> BoundedConnectionQueue<T> {
    #[must_use]
    pub(crate) fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "a control connection queue needs capacity");
        Self {
            capacity,
            items: Mutex::new(VecDeque::with_capacity(capacity)),
            ready: Condvar::new(),
        }
    }

    /// Admit without blocking the listener; on saturation the item comes back so
    /// the caller can send a busy reply. Never exceeds `capacity`.
    pub(crate) fn try_push(&self, item: T) -> Result<(), T> {
        let mut items = self.items.lock().unwrap_or_else(|p| p.into_inner());
        if items.len() >= self.capacity {
            return Err(item);
        }
        items.push_back(item);
        self.ready.notify_one();
        Ok(())
    }

    /// Wait for one admitted connection. Only the fixed workers call this;
    /// waiting consumes no CPU and the listener never takes this path.
    pub(crate) fn pop(&self) -> T {
        let mut items = self.items.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(item) = items.pop_front() {
                return item;
            }
            items = self.ready.wait(items).unwrap_or_else(|p| p.into_inner());
        }
    }
}

/// The lanes every connection is served on: requests, waits, the parker
/// (`control_lanes.rs` carries the whole design).
#[path = "control_lanes.rs"]
mod control_lanes;

/// Request lanes. A lane holds a connection only while it authenticates or
/// serves a request: idle connections park and waits move to wait lanes
/// (`control_lanes.rs`). Push subscriptions get their own pool. A fresh peer
/// gets a busy reply when every lane is working or
/// [`control_connections_cap`] connections are open.
const CONTROL_WORKERS: usize = 8;
/// The push lanes started with the socket. Each long-lived subscription
/// holds one for its life (`subscribe` is push-only), so more are grown one
/// thread at a time while every started lane is taken, up to
/// [`control_subscriptions_cap`], and kept: a native harness keeps one
/// `subscribe @self` stream per agent tab (codex-alab's contract §2), and a
/// fixed four refused the fifth codex-alab of an instance its claim for good
/// (the end-to-end run of 2026-10-16, AD-2).
const CONTROL_SUBSCRIPTION_WORKERS: usize = 4;
/// The CEILING on push lanes at once. The bound in force is
/// [`control_subscriptions_cap`], which answers to the descriptor limit too.
const CONTROL_SUBSCRIPTION_MAX: usize = 64;
/// The WAIT lanes' cap (`await`, `ready`, `wait`, `turn`, a parking `post` or
/// `inbox get @…`, a relay). Started on demand, kept once started. Every
/// supervised agent tab spends most of its life in one 20 s `await`.
const CONTROL_WAIT_WORKERS: usize = 64;
/// The REQUEST lanes that may serve a wait at once: a wait no wait lane can
/// take (every one taken; a wait with requests pipelined behind it moves with
/// them) is served on its request lane only within this budget, and past it
/// gets the busy reply on its connection. Half the lanes, so the other half always
/// serve requests: before this bound, waits past the wait lanes took request
/// lanes without limit, and eight of them left a parked driver's next request
/// queued behind them for up to the waits' 600 s clamp (`control_lanes.rs`).
const CONTROL_INLINE_WAITS: usize = CONTROL_WORKERS / 2;
const _: () = assert!(
    CONTROL_INLINE_WAITS < CONTROL_WORKERS,
    "a request lane must always be left for requests"
);
/// The CEILING on control connections open at once. The bound in force is
/// [`control_connections_cap`], which also answers to the process's descriptor
/// limit and is usually lower.
const CONTROL_CONNECTIONS_MAX: usize = 1024;

/// Most control connections open at once in this process (push subscriptions
/// count against their own pool): the parker's capacity.
///
/// Every open connection holds a descriptor, and the window's descriptor budget
/// is small: an app started by launchd (the Dock, Finder, `open`) runs with a
/// SOFT `RLIMIT_NOFILE` of 256, and aterm does not raise it (except for an
/// update: a successor launched for one raises it for its descriptor grant
/// before it dials, `handoff_rendezvous::claim_incoming`, and an attempt that
/// takes the seamless lane toward a build no older than itself raises it for
/// its park, `handoff_rendezvous::raise_descriptor_limit_for_park`). A fixed
/// cap of 1024 ignored that (measured 2026-09-26 under `ulimit -n 256`): about 240 idle
/// connections used up the process, every fresh client then got a dropped
/// connection instead of the busy line, and a new tab or a file open had no
/// descriptor left. So the cap is a QUARTER of the soft limit read at startup —
/// 64 at 256 — leaving three quarters for tabs, files and everything else (half,
/// were every connection a relay holding a second descriptor to its far end);
/// never more than [`CONTROL_CONNECTIONS_MAX`], and never fewer than
/// [`CONTROL_WORKERS`], the connections the fixed lanes always held.
fn control_connections_cap() -> usize {
    connections_cap_for(soft_descriptor_limit())
}

/// [`control_connections_cap`] for a soft descriptor limit (`None`: unreadable,
/// which keeps the fixed lanes' bound).
fn connections_cap_for(soft_limit: Option<u64>) -> usize {
    soft_limit.map_or(CONTROL_WORKERS, |soft| {
        usize::try_from(soft / 4)
            .unwrap_or(usize::MAX)
            .clamp(CONTROL_WORKERS, CONTROL_CONNECTIONS_MAX)
    })
}

/// This process's soft `RLIMIT_NOFILE`.
#[cfg(unix)]
fn soft_descriptor_limit() -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource id and an exclusively borrowed, writable
    // out-parameter for the one call.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) };
    (read == 0).then_some(limit.rlim_cur)
}

/// Most push subscriptions at once in this process: an EIGHTH of the soft
/// descriptor limit read at startup (32 at launchd's 256, beside the
/// connections' quarter), never more than [`CONTROL_SUBSCRIPTION_MAX`] and
/// never fewer than the [`CONTROL_SUBSCRIPTION_WORKERS`] started with the
/// socket. Each holds a descriptor and a thread.
fn control_subscriptions_cap() -> usize {
    subscriptions_cap_for(soft_descriptor_limit())
}

/// [`control_subscriptions_cap`] for a soft descriptor limit (`None`:
/// unreadable, which keeps the started lanes' bound).
fn subscriptions_cap_for(soft_limit: Option<u64>) -> usize {
    soft_limit.map_or(CONTROL_SUBSCRIPTION_WORKERS, |soft| {
        usize::try_from(soft / 8)
            .unwrap_or(usize::MAX)
            .clamp(CONTROL_SUBSCRIPTION_WORKERS, CONTROL_SUBSCRIPTION_MAX)
    })
}

/// No parker runs off unix, so the cap bounds nothing there (the lanes do).
#[cfg(not(unix))]
fn soft_descriptor_limit() -> Option<u64> {
    None
}

struct SubscriptionJob {
    line: String,
    scope: Scope,
    stream: CtlStream,
}

/// Starts one more push lane serving the dispatch's jobs; whether it started.
type GrowLane = Box<dyn Fn() -> bool + Send + Sync>;

struct SubscriptionDispatch {
    jobs: Arc<BoundedDispatch<SubscriptionJob>>,
    /// How a lane is grown ([`spawn_subscription_workers`] sets it once the
    /// started lanes run); none, nothing grows.
    grow: std::sync::OnceLock<GrowLane>,
    /// One grower at a time, so two submitters at a full pool grow one lane
    /// each, never past the ceiling.
    growing: Mutex<()>,
}

impl SubscriptionDispatch {
    /// A dispatch of at most `ceiling` push lanes.
    fn new(ceiling: usize) -> Self {
        Self {
            jobs: Arc::new(BoundedDispatch::new(ceiling)),
            grow: std::sync::OnceLock::new(),
            growing: Mutex::new(()),
        }
    }

    /// Admit a long-lived subscription onto a free push lane, growing one
    /// when every started lane is taken and the ceiling allows; `Err` (the
    /// stream back, for its refusal) only at the ceiling, or when no lane
    /// could be started.
    fn try_submit(&self, line: String, scope: Scope, stream: CtlStream) -> Result<(), CtlStream> {
        let mut job = SubscriptionJob {
            line,
            scope,
            stream,
        };
        loop {
            match self.jobs.try_submit(job) {
                Ok(()) => return Ok(()),
                Err(back) => job = back,
            }
            if !self.grow_one() {
                return Err(job.stream);
            }
        }
    }

    /// One more lane (or one freed meanwhile): whether a submission may try again.
    fn grow_one(&self) -> bool {
        let _one = self
            .growing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.jobs.has_room() {
            return true;
        }
        let Some(grow) = self.grow.get() else {
            return false;
        };
        self.jobs.can_grow() && grow() && self.jobs.add_capacity()
    }
}

/// Drop guard for the bridge connection's end. When it ends (child exited or was
/// killed, fd closed, serving thread unwound), the instance reports
/// `fabric=disconnected` and every session the bridge delivered to or held gets
/// `hold_advisory=fabric-lost`, or under `[fabric] hold_on_bridge_lost = true`
/// is held `reason=fabric-lost origin=fleet` (`fabric::bridge_lost`).
///
/// A guard rather than a trailing call so the halt is fail-closed: an agent
/// sharing the bridge's uid can kill it, and that must never lift a hold.
#[cfg(any(unix, test))]
struct BridgeLostGuard {
    store: Store,
    /// The incarnation this guard speaks for. The hold sweep ignores it, but
    /// `fabric=disconnected` is stored only while this generation owns the link,
    /// so a stale guard cannot report its live replacement dead (see
    /// [`crate::fabric::BridgeGeneration`]).
    generation: crate::fabric::BridgeGeneration,
}

#[cfg(any(unix, test))]
impl Drop for BridgeLostGuard {
    fn drop(&mut self) {
        crate::fabric::bridge_lost(&self.store, self.generation);
    }
}

struct ControlWorkerContext {
    active: ActiveHandle,
    store: Store,
    subscribers: Subscribers,
    proxy: EventLoopProxy<Wake>,
    image_queue: ImageQueue,
    token: Arc<String>,
    sock_dir: std::path::PathBuf,
    subscriptions: Arc<SubscriptionDispatch>,
}

/// Local resource preparation, not Commit authority. Fixed-path candidates
/// reserve both service lanes before proving adoption; their private queues get
/// no work until the Commit gate permits binding.
#[derive(Clone, Default)]
pub(crate) struct ControlPreparation(Arc<std::sync::atomic::AtomicU8>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlPreparationState {
    Pending,
    Ready,
    Failed,
}

impl ControlPreparation {
    pub(crate) fn state(&self) -> ControlPreparationState {
        match self.0.load(Ordering::Acquire) {
            0 => ControlPreparationState::Pending,
            1 => ControlPreparationState::Ready,
            _ => ControlPreparationState::Failed,
        }
    }

    pub(crate) fn finish(&self, ready: bool) {
        // Preparation is one-shot. A post-Commit bind failure does not rewrite
        // the historical fact that the service resources were reserved.
        let _ = self.0.compare_exchange(
            0,
            if ready { 1 } else { 2 },
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

fn control_lanes_prepared(rpc: usize, subscriptions: usize) -> bool {
    rpc > 0 && subscriptions > 0
}

struct ControlPreparationGuard {
    preparation: ControlPreparation,
    proxy: EventLoopProxy<Wake>,
}

impl ControlPreparationGuard {
    fn ready(&self) {
        self.preparation.finish(true);
        let _ = self.proxy.send_event(Wake::ControlPrepared);
    }
}

impl Drop for ControlPreparationGuard {
    fn drop(&mut self) {
        if self.preparation.state() == ControlPreparationState::Pending {
            self.preparation.finish(false);
            let _ = self.proxy.send_event(Wake::ControlPrepared);
        }
    }
}

/// What every pooled lane serves with (`control_lanes.rs`): the handshake, one
/// request at a time, wait classification, the push flip and claim release.
impl control_lanes::LaneService for ControlWorkerContext {
    fn authenticate(
        &self,
        stream: &CtlStream,
        reader: &mut control_lanes::LaneReader<'_>,
    ) -> Option<(Scope, Principal, Option<String>)> {
        authenticate_connection(
            stream,
            reader,
            self.token.as_str(),
            &self.active,
            &control_denials::AUTH_DENIALS,
            std::time::Instant::now() + AUTH_READ_TIMEOUT,
        )
    }

    fn serve_line(
        &self,
        line: String,
        scope: Scope,
        stream: &CtlStream,
        reader: &mut control_lanes::LaneReader<'_>,
        out: &mut Vec<u8>,
    ) -> Option<ServeDisposition> {
        serve_request_line(
            line,
            scope,
            stream,
            reader,
            out,
            &self.active,
            &self.store,
            &self.subscribers,
            &self.proxy,
            &self.image_queue,
            &self.sock_dir,
        )
    }

    fn frame_header(&self, line: &str, scope: Scope, out: &mut Vec<u8>) -> bool {
        match binary_frame_verb(line) {
            Some(verb) => {
                let route = FeedBinRoute {
                    active: &self.active,
                    store: &self.store,
                    scope,
                    subscribers: None,
                };
                let mut clear_license =
                    |session| front_routed_license_clear(&self.proxy, session, false);
                feed_bin_header(line, verb, route, &mut clear_license, out)
            }
            // A `post`'s body has no header to act on.
            None => true,
        }
    }

    fn is_wait(&self, line: &str, scope: Scope) -> bool {
        is_wait_request(line, scope, &self.store)
    }

    fn subscribe(&self, line: String, scope: Scope, stream: CtlStream) {
        handoff_subscription(&self.subscriptions, line, scope, stream);
    }

    fn release(&self, id: u64) {
        release_connection_claims(&self.store, &self.subscribers, Some(&self.proxy), id);
    }
}

impl ControlWorkerContext {
    /// Serve one inherited bridge fd (scope from the fd, not a token), marking
    /// `fabric=connected` while served; [`BridgeLostGuard`] applies the
    /// fabric-lost halt however the connection ends.
    #[cfg(unix)]
    fn serve_bridge(&self, mut stream: CtlStream, generation: crate::fabric::BridgeGeneration) {
        // This thread is one lane of one incarnation; `link` reports are accepted
        // only for the generation that owns the link.
        crate::fabric::set_lane_generation(generation);
        crate::fabric::bridge_attached(generation);
        let _lost = BridgeLostGuard {
            store: self.store.clone(),
            generation,
        };
        match serve_preresolved(
            &stream,
            Scope::Bridge,
            &self.active,
            &self.store,
            &self.subscribers,
            &self.proxy,
            &self.image_queue,
            &self.sock_dir,
        ) {
            // Each half on its own ([`aterm_uds::hangup::hang_up`]): the lane
            // closes on the bridge's EOF, and Darwin refuses a both-ways
            // `shutdown` of a socket its peer half-closed, shutting nothing.
            ServeDisposition::Close => {
                aterm_uds::hangup::hang_up(&stream);
            }
            // The bridge's push subscription is served inline, not handed to the
            // subscription pool: the `BridgeLostGuard` fires when this function
            // returns, so handing off would apply the fabric-lost halt at the
            // flip. The bridge also uses none of the shared request or wait lanes.
            ServeDisposition::Subscribe { line, scope } => {
                // Hangs the lane up when its push stream ends.
                run_subscribe_socket(
                    &line,
                    &self.active,
                    &self.store,
                    &self.subscribers,
                    scope,
                    &mut stream,
                );
            }
        }
    }

    fn serve_subscription(&self, mut job: SubscriptionJob) {
        // This direct connection moved from a request lane to this push lane.
        // Re-establish its actual peer context here; the inherited bridge's
        // inline subscription deliberately never gains DirectPeer authority.
        let _serving = ServingFd::enter(&job.stream);
        #[cfg(unix)]
        let _direct = DirectPeer::enter();
        // Match the ordinary lane teardown: the push stream hangs its
        // connection up as it ends, waking a vanished peer out of the AF_UNIX
        // kernel path before Drop releases this reusable lane.
        run_subscribe_socket(
            &job.line,
            &self.active,
            &self.store,
            &self.subscribers,
            job.scope,
            &mut job.stream,
        );
    }
}

/// The control-worker context, published once the server runs, so the fabric
/// bridge is served against the same registry, active handle and proxy as every
/// other lane.
static BRIDGE_CONTEXT: std::sync::OnceLock<Arc<ControlWorkerContext>> = std::sync::OnceLock::new();

/// Serve one inherited fabric-bridge connection: the near end of a
/// `socketpair(AF_UNIX)` whose far end belongs to this instance's
/// `aterm-link serve` child. [`Scope::Bridge`] is pre-resolved (no `AUTH` line;
/// the fd is the authority), and the fabric-lost halt lands when it closes.
///
/// Returns `false` when the control server is not up yet or a thread could not
/// start; never a partial attach.
///
/// Called by [`crate::fabric_launch`] once per near end (fds 3 and 4 on the
/// child); either closing fires the halt. `generation` identifies the launch,
/// shared by both ends, so neither guard can clobber a later launch's
/// `fabric=connected`.
#[cfg(unix)]
pub(crate) fn attach_fabric_bridge(
    stream: CtlStream,
    generation: crate::fabric::BridgeGeneration,
) -> bool {
    let Some(context) = BRIDGE_CONTEXT.get().cloned() else {
        return false;
    };
    crate::qos::spawn_lane_thread(
        "aterm-fabric-bridge".to_string(),
        crate::qos::Role::Responsive,
        move || {
            // Same QoS floor as the pooled lanes: this thread serves the same verbs
            // and then stays for the push loop, holding the terminal mutex.
            context.serve_bridge(stream, generation);
        },
    )
    .is_ok()
}

/// Create the push lanes started with the socket, and install the grower that
/// starts more on demand ([`SubscriptionDispatch::try_submit`]). Returns how
/// many started so startup can fail closed if none did.
fn spawn_subscription_workers(
    dispatch: &Arc<SubscriptionDispatch>,
    context: &Arc<std::sync::OnceLock<Arc<ControlWorkerContext>>>,
) -> usize {
    let started = (0..CONTROL_SUBSCRIPTION_WORKERS)
        .filter(|&index| spawn_subscription_worker(index, &dispatch.jobs, context))
        .count();
    dispatch.jobs.set_capacity(started);
    if started > 0 {
        let (jobs, context) = (Arc::clone(&dispatch.jobs), Arc::clone(context));
        let next = AtomicUsize::new(started);
        let _ = dispatch.grow.set(Box::new(move || {
            let index = next.fetch_add(1, Ordering::Relaxed);
            spawn_subscription_worker(index, &jobs, &context)
        }));
    }
    started
}

/// One push lane, serving `jobs` for good; whether its thread started.
fn spawn_subscription_worker(
    index: usize,
    jobs: &Arc<BoundedDispatch<SubscriptionJob>>,
    context: &Arc<std::sync::OnceLock<Arc<ControlWorkerContext>>>,
) -> bool {
    let (jobs, context) = (Arc::clone(jobs), Arc::clone(context));
    let name = format!("aterm-subscribe-{index}");
    // Same QoS floor as the RPC lanes: the push loop takes `term_lock` per
    // target per tick.
    match crate::qos::spawn_lane_thread(name, crate::qos::Role::Responsive, move || {
        loop {
            if !jobs.serve_next(|job| {
                context
                    .get()
                    .expect("context published before dispatch")
                    .serve_subscription(job);
            }) {
                aterm_log::warn!("subscription worker recovered after a connection panic");
            }
        }
    }) {
        Ok(_) => true,
        Err(error) => {
            aterm_log::warn!("subscription worker {index} could not start: {error}");
            false
        }
    }
}

/// The same admitted Commit gate and process witness that protect PTY adoption.
/// No raw environment flag or PID can grant control-socket takeover authority.
#[cfg(unix)]
pub(crate) struct IncomingControlHandoff {
    pub(crate) gate: crate::spawn::DeferredReaderGate,
    pub(crate) parent: crate::seamless::AttestedParent,
    pub(crate) identity: crate::control_socket_identity::SocketIdentity,
}

#[cfg(not(unix))]
pub(crate) enum IncomingControlHandoff {}

impl IncomingControlHandoff {
    fn bind(&self, _plan: &control_auth::SocketPlan) -> Option<BoundControl> {
        #[cfg(unix)]
        {
            bind_control_listener(
                _plan,
                Some(&self.gate),
                || self.parent.still_alive(),
                |pid| self.parent.owns_retiring_socket(pid),
                Some(&self.identity),
            )
        }
        #[cfg(not(unix))]
        match *self {}
    }
}

/// The bound control socket and its discovery publication, in their own module
/// so privacy enforces the ordering: nothing else can take a [`BoundControl`]
/// apart.
mod bound_control {
    use std::sync::Arc;

    use aterm_session::{LaunchNonce, SessionId};
    use aterm_uds::CtlListener;

    use crate::control_auth;
    use crate::session_store::Store;

    /// A control socket this instance has bound: listener, per-launch token and
    /// path. Built only by `bind_control_listener` right after a successful bind,
    /// so holding one proves the path is listening.
    pub(super) struct BoundControl {
        listener: CtlListener,
        token: Arc<String>,
        sock_path: String,
    }

    impl BoundControl {
        /// `listener` must be the socket just bound at `sock_path`.
        pub(super) fn bound(listener: CtlListener, token: Arc<String>, sock_path: String) -> Self {
            Self {
                listener,
                token,
                sock_path,
            }
        }

        pub(super) fn listener(&self) -> &CtlListener {
            &self.listener
        }

        pub(super) fn token(&self) -> &Arc<String> {
            &self.token
        }

        /// The listener and token WITHOUT publishing — for the bind's own tests,
        /// which exercise the socket and never advertise it.
        #[cfg(test)]
        pub(super) fn into_parts(self) -> (CtlListener, Arc<String>) {
            (self.listener, self.token)
        }
    }

    /// Publish this instance's discovery entries (the root session's and every
    /// session's), record the bound socket so later sessions publish their own,
    /// and return the listener to serve on.
    ///
    /// Order is the contract (`publish_ordering_model`: published ⟹ bound):
    /// `proxy::sweep_stale_graph` deletes entries whose socket has no listener,
    /// so an entry written before the bind could be swept. Consuming the
    /// [`BoundControl`] enforces the order in the signature, and `spawn` reaches
    /// its accept loop only through the returned listener.
    ///
    /// Uses `publish_graph_entry` so an explicit `--control-sock` also registers
    /// in the default rendezvous dir. A concurrent registration either sees the
    /// recorded socket or is in the snapshot. A session deregistered in between
    /// may briefly reappear, naming our live socket; the self-dial guard answers
    /// `ERR no such session` and the next sweep removes it. Tier-1:
    /// `publish_ordering_conformance`.
    pub(super) fn publish_discovery(
        bound: BoundControl,
        root_identity: Option<&(SessionId, LaunchNonce)>,
        store: &Store,
    ) -> CtlListener {
        let BoundControl {
            listener,
            sock_path,
            ..
        } = bound;
        let sock_dir = control_auth::dir_of_socket(&sock_path);
        if let Some((sid, nonce)) = root_identity {
            crate::proxy::publish_graph_entry(&sock_dir, sid, &sock_path, nonce);
        }
        crate::proxy::set_self_sock(&sock_dir, &sock_path);
        // The snapshot first, then the writes. A guard taken in the `for`
        // header lives for the whole loop, so the session store stayed held
        // across every graph write; a volume-wide flush stretched those, and
        // the main thread's `store.write()` (a registration, the harness
        // posture) waited behind them: 3.4–4.8 ms launch turns (LAG-5).
        let handles = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
        for h in handles {
            crate::proxy::publish_graph_entry(&sock_dir, &h.sid, &sock_path, &h.nonce);
        }
        listener
    }

    /// Publish once this launch may (the `latest` alias, then every discovery
    /// entry via [`publish_discovery`]) and return the listener; `None` for a
    /// candidate whose attempt stood down.
    ///
    /// One server per address at every instant (`HandoffAddressOwner`,
    /// `CandidateNamedOnlyAfterCommit`). Only a per-process update candidate
    /// waits: it binds early but carries its parent's session ids, which the
    /// parent owns until Commit, so it publishes nothing before `gate` is
    /// released (otherwise two instances would answer one `@<sid>`). Tier-1:
    /// `publish_when_committed_conformance`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HandoffAddressOwner",
            action = "SuccessorPublishes",
            project = "aterm_gui::control::publish_when_committed_conformance::project"
        )
    )]
    pub(super) fn publish_when_committed(
        bound: BoundControl,
        gate: Option<&crate::spawn::DeferredReaderGate>,
        latest_link: Option<&std::path::Path>,
        root_identity: Option<&(SessionId, LaunchNonce)>,
        store: &Store,
    ) -> Option<CtlListener> {
        if !super::await_commit_to_publish(gate) {
            return None;
        }
        if let Some(link) = latest_link {
            control_auth::publish_latest_link(link, &bound.sock_path);
        }
        Some(publish_discovery(bound, root_identity, store))
    }
}
#[cfg(test)]
use bound_control::publish_discovery;
use bound_control::{BoundControl, publish_when_committed};

/// How long a committed handoff's successor waits, from the Commit, for its
/// predecessor to exit and stop listening before it stands down and leaves the
/// fixed path to it. The predecessor `_exit`s a few instructions after the
/// Commit, so this only names a predecessor that never left.
const HANDOFF_BIND_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// For an incoming handoff, bind only after the predecessor has committed and
/// stopped listening. The reader gate is shared with the control worker, so a
/// rejected candidate never touches the parent's socket, token or discovery.
/// Runs on the control worker, never the event loop.
fn bind_control_listener(
    plan: &control_auth::SocketPlan,
    handoff_gate: Option<&crate::spawn::DeferredReaderGate>,
    parent_alive: impl Fn() -> bool,
    parent_owns_peer: impl Fn(u32) -> bool,
    identity: Option<&crate::control_socket_identity::SocketIdentity>,
) -> Option<BoundControl> {
    bind_control_listener_within(
        plan,
        handoff_gate,
        parent_alive,
        parent_owns_peer,
        identity,
        HANDOFF_BIND_WAIT,
        control_auth::connect_socket_nonblocking,
    )
}

/// [`bind_control_listener`] with the handoff wait and the teardown probe's
/// dial named: production's are [`HANDOFF_BIND_WAIT`] and
/// [`control_auth::connect_socket_nonblocking`]. A test that walks the
/// successor through the Commit, the parent's exit and the listener's close
/// one FACT at a time names a hang detector instead, so a test thread
/// descheduled between two facts cannot run the production clock out and make
/// a correct worker stand down; and it holds a dial, to close the listener at
/// the one instant a scheduler otherwise picks only now and then.
fn bind_control_listener_within(
    plan: &control_auth::SocketPlan,
    handoff_gate: Option<&crate::spawn::DeferredReaderGate>,
    parent_alive: impl Fn() -> bool,
    parent_owns_peer: impl Fn(u32) -> bool,
    identity: Option<&crate::control_socket_identity::SocketIdentity>,
    handoff_wait: std::time::Duration,
    mut dial: impl FnMut(&str) -> std::io::Result<CtlStream>,
) -> Option<BoundControl> {
    let sock_path = plan.sock_path.clone();
    let sock_dir = control_auth::dir_of_socket(&sock_path);
    if let Some(gate) = handoff_gate {
        let identity = identity.filter(|identity| identity.matches_current(plan))?;
        if !gate.wait_until_released(&AtomicBool::new(false)) {
            return None;
        }
        // Commit's write precedes the parent's _exit by a few instructions.
        // Wait on the attested process, not repeated socket connects: a live
        // listener with a full backlog can block connect beyond any deadline.
        // The ordinary strict socket probe below still refuses unrelated owners.
        let deadline = std::time::Instant::now() + handoff_wait;
        while parent_alive() {
            if std::time::Instant::now() >= deadline {
                aterm_log::warn!(
                    "control socket {sock_path}: predecessor still alive after handoff Commit; \
                     preserving its socket and token"
                );
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Darwin can report process death before final socket teardown. A
        // successful connect must identify the attested predecessor before any
        // retry, so an unrelated backlog is never filled.
        loop {
            // ECONNREFUSED is not ownership evidence; revalidate the parent's
            // socket inode and token digest before every probe.
            if !identity.matches_current(plan) {
                aterm_log::warn!(
                    "control socket {sock_path}: endpoint identity no longer verifies during handoff; preserving it"
                );
                return None;
            }
            match dial(&sock_path) {
                Ok(stream) => match aterm_uds::fdpass::peer_pid(&stream) {
                    Ok(pid) if parent_owns_peer(pid) => {}
                    // The predecessor's teardown reached this dial between the
                    // connect and the question: closing its listener aborts
                    // every connection still queued on it, and its exit closes
                    // every connection it accepted. Either way this end is no
                    // longer connected, and XNU answers `LOCAL_PEERPID` only on
                    // a connected socket. That is the teardown this loop waits
                    // for, not an owner, and it left nothing of this dial in
                    // any backlog. Dial again, after the identity check above:
                    // it is refused, or reaches a listener that answers for
                    // itself.
                    Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {}
                    _ => {
                        aterm_log::warn!(
                            "control socket {sock_path}: listener ownership does not verify as the predecessor; preserving its socket and token"
                        );
                        return None;
                    }
                },
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    break;
                }
                Err(_) => {}
            }
            if std::time::Instant::now() >= deadline {
                aterm_log::warn!(
                    "control socket {sock_path}: endpoint did not become safely reusable after predecessor exit; preserving its socket and token"
                );
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    if control_auth::ensure_private_dir(&sock_dir).is_err() {
        // Also log every bind outcome to the file log: Finder/`open` launches
        // discard stderr.
        aterm_log::warn!(
            "control socket dir {} not creatable; socket disabled",
            sock_dir.display()
        );
        crate::logging::stderr_line!(
            "aterm-gui: control socket dir {} not creatable; socket disabled",
            sock_dir.display()
        );
        return None;
    }
    // Sweep dead pids' per-instance socket/token leftovers, only in the shared
    // default dir (an explicit path owns its directory).
    if plan.latest_link.is_some() {
        sweep_dead_instance_files(&sock_dir);
        // The age-and-liveness sweep also catches leftovers whose pid Windows
        // recycled and files in the older socket directory. Launch only; not
        // part of `sweep_dead_instance_files`, which the post-handoff sweep
        // reruns.
        #[cfg(windows)]
        control_auth::sweep_stale_socket_files_at_launch(&sock_dir);
    }
    // Never unlink a live socket: a nested aterm given an explicit socket path
    // must not hijack its parent's listener. A stale file (no listener) is
    // removed so bind() does not fail with EADDRINUSE.
    //
    // This runs before `provision_token`, which rewrites `plan.token_path`
    // unconditionally; with a shared explicit path that would clobber a live
    // instance's token. Per-instance default paths use the inverted probe (only
    // a listener that answers refuses the bind); explicit paths keep the strict
    // probe.
    let is_live = if plan.latest_link.is_some() {
        control_auth::socket_is_live_per_instance(&sock_path)
    } else {
        control_auth::socket_is_live(&sock_path)
    };
    if control_auth::decide_bind(is_live) == control_auth::BindAction::RefuseLiveSocket {
        aterm_log::warn!(
            "control socket {sock_path} already has a live listener; running without \
             a control socket rather than hijacking it"
        );
        crate::logging::stderr_line!(
            "aterm-gui: control socket {sock_path} already has a live listener; \
             running without a control socket rather than hijacking it"
        );
        return None;
    }
    if identity.is_some_and(|identity| !identity.matches_current(plan)) {
        return None;
    }
    // Provision the per-launch token; fail closed (no token, no socket).
    let token = match control_auth::provision_token(&plan.token_path) {
        Some(t) => Arc::new(t),
        None => {
            aterm_log::warn!("could not provision control-socket token; socket disabled");
            crate::logging::stderr_line!(
                "aterm-gui: could not provision control-socket token; socket disabled"
            );
            return None;
        }
    };
    let _ = std::fs::remove_file(&sock_path);
    let listener = bind_prepared_control_listener(plan, handoff_gate.is_some())?;
    Some(BoundControl::bound(listener, token, sock_path))
}

/// Remove dead instances' per-instance sockets, tokens and discovery entries
/// from the shared control directory. Default per-instance plans only.
fn sweep_dead_instance_files(sock_dir: &std::path::Path) {
    control_auth::sweep_stale_instances(sock_dir);
    crate::proxy::sweep_stale_graph(sock_dir);
}

/// How long after a committed update handoff [`sweep_after_handoff`] looks again.
/// The predecessor `_exit`s at Commit, so its pid is dead well before this.
#[cfg(unix)]
const HANDOFF_SWEEP_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// Sweep the control directory again [`HANDOFF_SWEEP_AFTER`] after taking over
/// from an update predecessor, on a short-lived thread: the committed
/// predecessor exits without graceful cleanup. Default per-instance plans only.
#[cfg(unix)]
pub(crate) fn sweep_after_handoff(plan: Option<&control_auth::SocketPlan>) {
    let Some(plan) = plan.filter(|plan| plan.latest_link.is_some()) else {
        return;
    };
    let sock_dir = control_auth::dir_of_socket(&plan.sock_path);
    let _ = crate::qos::spawn_lane_thread(
        "aterm-handoff-sweep",
        crate::qos::Role::Housekeeping,
        move || {
            // An opportunistic disk sweep: no deadline, no lock, nothing waits on it.
            std::thread::sleep(HANDOFF_SWEEP_AFTER);
            sweep_dead_instance_files(&sock_dir);
            crate::seamless::sweep_dead_handoff_leftovers();
        },
    );
}

const HANDOFF_BIND_ATTEMPTS: u32 = 3;

/// A consumed handoff witness never authorizes another unlink. A failed bind
/// may try again only while the path is demonstrably absent, within its budget.
fn handoff_bind_retry_allowed(failed_attempt: u32, path_missing: bool) -> bool {
    path_missing && failed_attempt < HANDOFF_BIND_ATTEMPTS
}

/// The handoff bind retry loop with its two OS effects passed in, so tests can
/// fail a bind or place a foreign listener during backoff deterministically.
fn bind_handoff_control_listener(
    plan: &control_auth::SocketPlan,
    mut bind: impl FnMut(&str) -> std::io::Result<CtlListener>,
    mut pause: impl FnMut(std::time::Duration),
) -> Option<CtlListener> {
    for attempt in 1..=HANDOFF_BIND_ATTEMPTS {
        match bind(&plan.sock_path) {
            Ok(listener) => return Some(listener),
            Err(error) => {
                aterm_log::warn!(
                    "control socket handoff bind failed at {} (attempt {attempt}/{HANDOFF_BIND_ATTEMPTS}): {error}",
                    plan.sock_path
                );
                // symlink_metadata treats a dangling symlink as occupied too.
                // No connect probe: a full foreign backlog can look stale.
                let missing = std::fs::symlink_metadata(&plan.sock_path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
                if !handoff_bind_retry_allowed(attempt, missing) {
                    crate::logging::stderr_line!(
                        "aterm-gui: control socket handoff bind could not complete at {}; preserving any replacement endpoint",
                        plan.sock_path
                    );
                    return None;
                }
                pause(std::time::Duration::from_millis(50 * u64::from(attempt)));
            }
        }
    }
    None
}

/// Bind after the caller's one authorized token rotation and path removal.
/// Handoff retries bind only, never startup's stale-path unlink policy.
fn bind_prepared_control_listener(
    plan: &control_auth::SocketPlan,
    handoff: bool,
) -> Option<CtlListener> {
    if handoff {
        return bind_handoff_control_listener(
            plan,
            |path| CtlListener::bind(path),
            std::thread::sleep,
        );
    }
    let sock_path = plan.sock_path.as_str();
    // Retry transient bind failures (AV scan, racing cleanup) with a short delay
    // rather than losing introspection for the process lifetime.
    let mut listener = None;
    let max_attempts = 3u32;
    for attempt in 1..=max_attempts {
        match CtlListener::bind(sock_path) {
            Ok(l) => {
                if attempt > 1 {
                    aterm_log::info!(
                        "control socket bind succeeded on retry {attempt} at {sock_path}"
                    );
                }
                listener = Some(l);
                break;
            }
            Err(e) => {
                aterm_log::warn!(
                    "control socket bind failed at {sock_path} (attempt {attempt}/{max_attempts}): {e}"
                );
                crate::logging::stderr_line!(
                    "aterm-gui: control socket bind failed at {sock_path} \
                     (attempt {attempt}/{max_attempts}): {e}"
                );
                if attempt < 3 {
                    std::thread::sleep(std::time::Duration::from_millis(750));
                    // Shared explicit paths: re-probe before unlinking, since a
                    // sibling may have bound while we slept. Per-instance paths
                    // cannot be owned by anyone else.
                    if plan.latest_link.is_none() && control_auth::socket_is_live(sock_path) {
                        aterm_log::warn!(
                            "control socket {sock_path} became live during retry; \
                             running socketless rather than hijacking it"
                        );
                        crate::logging::stderr_line!(
                            "aterm-gui: control socket {sock_path} became live during \
                             retry; running socketless rather than hijacking it"
                        );
                        return None;
                    }
                    let _ = std::fs::remove_file(sock_path);
                }
            }
        }
    }
    let Some(listener) = listener else {
        aterm_log::warn!("control socket disabled: bind failed after 3 attempts");
        return None;
    };
    Some(listener)
}

/// Spawn the control-listener thread: provision the token, bind the socket
/// (after sweeping stale files), lock it to `0600`, publish the `latest`
/// symlink, then serve each authenticated connection.
///
/// Access control is on by default ([`crate::control_auth`]): a per-user
/// `0700` directory, same-uid peers only, and the per-launch token before any
/// verb. Without a token the socket is never bound.
#[allow(
    clippy::too_many_arguments,
    reason = "the control server's full set of independent collaborators (active handle, store, subscribers, proxy, image queue, socket plan); a config struct would only move the list"
)]
pub(crate) fn spawn(
    active: ActiveHandle,
    store: Store,
    subscribers: Subscribers,
    proxy: EventLoopProxy<Wake>,
    queue: ImageQueue,
    plan: control_auth::SocketPlan,
    // The startup config generation already admitted; no second config read
    // before deciding on the optional fabric bridge.
    startup_config: crate::app_config::Config,
    // The root session's (id, nonce) to publish as the recursion discovery graph
    // entry, written AFTER bind so it never races the stale sweep. `None` = skip.
    root_identity: Option<(SessionId, aterm_session::LaunchNonce)>,
    // Set only after this instance's bind succeeds, so graceful-exit cleanup
    // unlinks the socket and token only when we own them.
    bound: Arc<AtomicBool>,
    // The handle the cold update lane retires the socket by before its `exec`
    // (`control_listener::Endpoint`); the supervised accept loop serves it.
    endpoint: crate::control_listener::Endpoint,
    // Only fixed-path incoming candidates wait: per-process sockets cannot collide.
    handoff: Option<IncomingControlHandoff>,
    // A per-process update candidate's Commit gate: bind at once, publish and
    // serve only after release. `None` for every other launch.
    publish_gate: Option<crate::spawn::DeferredReaderGate>,
) -> ControlPreparation {
    // Windows has no exec(2), so nothing retires its endpoint; nor a fabric
    // bridge, the startup config's one reader here.
    #[cfg(not(unix))]
    let _ = (endpoint, startup_config);
    let preparation = ControlPreparation::default();
    let preparation_guard = ControlPreparationGuard {
        preparation: preparation.clone(),
        proxy: proxy.clone(),
    };
    // The admission path in front of every lane also holds a lock the UI thread
    // contends (the startup graph-entry publish holds the session store's read
    // guard across its file writes), so it gets the same QoS floor; free while
    // idle in `accept`.
    let _ = crate::qos::spawn_lane_thread(
        "aterm-control-listener",
        crate::qos::Role::Responsive,
        move || {
            // This guard also publishes failure if thread creation fails (the
            // unstarted closure is dropped), or startup unwinds before preparation.
            let preparation_guard = preparation_guard;
            let sock_path = plan.sock_path.clone();
            // The token + image-confinement subdir live alongside the socket file.
            let sock_dir = control_auth::dir_of_socket(&sock_path);
            // Ordinary startup can refuse an occupied path without allocating service
            // threads; only the incoming fixed endpoint reserves before the Commit
            // gate permits a bind.
            let ordinary_binding = if handoff.is_none() {
                let Some(binding) = bind_control_listener(&plan, None, || false, |_| false, None)
                else {
                    return;
                };
                Some(binding)
            } else {
                None
            };
            let connections_cap = control_connections_cap();
            let lanes = control_lanes::Lanes::new(control_lanes::LaneLimits {
                rpc: CONTROL_WORKERS,
                wait: CONTROL_WAIT_WORKERS,
                inline: CONTROL_INLINE_WAITS,
                open: connections_cap,
            });
            let subscriptions_cap = control_subscriptions_cap();
            let subscription_dispatch = Arc::new(SubscriptionDispatch::new(subscriptions_cap));
            let context_slot = Arc::new(std::sync::OnceLock::new());
            let workers = lanes.start();
            let subscription_workers =
                spawn_subscription_workers(&subscription_dispatch, &context_slot);
            if !control_lanes_prepared(workers, subscription_workers) {
                aterm_log::warn!("control startup refused: no runnable RPC or subscription lane");
                crate::logging::stderr_line!(
                    "aterm-gui: control startup refused: no runnable RPC or subscription lane"
                );
                if let Some(socket) = ordinary_binding {
                    drop(socket);
                    let _ = std::fs::remove_file(&sock_path);
                    let _ = std::fs::remove_file(&plan.token_path);
                }
                return;
            }
            if handoff.is_some() {
                // The parent still owns the exact endpoint and credential here.
                // Empty queues park these already allocated workers through Commit.
                preparation_guard.ready();
            }
            let binding = match handoff.as_ref() {
                Some(handoff) => handoff.bind(&plan),
                None => ordinary_binding,
            };
            let Some(socket) = binding else {
                return;
            };
            let token = socket.token().clone();
            let worker_context = Arc::new(ControlWorkerContext {
                active: active.clone(),
                store: store.clone(),
                subscribers: subscribers.clone(),
                proxy: proxy.clone(),
                image_queue: queue.clone(),
                token: token.clone(),
                sock_dir: sock_dir.clone(),
                subscriptions: subscription_dispatch.clone(),
            });
            // No stream enters any queue before this context is available.
            let _ = context_slot.set(worker_context.clone());
            lanes.publish(worker_context.clone());
            // A bridge attached later must never find a half-built process.
            let _ = BRIDGE_CONTEXT.set(worker_context.clone());
            if workers != CONTROL_WORKERS {
                aterm_log::warn!(
                    "control socket started with {workers}/{CONTROL_WORKERS} connection workers"
                );
            }
            if subscription_workers != CONTROL_SUBSCRIPTION_WORKERS {
                aterm_log::warn!(
                    "control socket started with {subscription_workers}/{CONTROL_SUBSCRIPTION_WORKERS} subscription workers"
                );
            }
            // The service is actually runnable. Only now authorize exit cleanup,
            // harden the socket, and advertise it as the newest instance.
            if let Some(identity) = crate::control_socket_identity::SocketIdentity::capture(
                &plan,
                socket.listener(),
                token.as_str(),
            ) {
                let _ = crate::control_socket_identity::publish(identity);
            }
            bound.store(true, Ordering::SeqCst);
            control_auth::lock_socket_file(&sock_path);
            // Record what this instance owns: the loop's exit and a kill both
            // remove it by that record, and only while it is still ours.
            crate::owned_endpoint::publish(&plan);
            if handoff.is_none() {
                preparation_guard.ready();
            }
            // A per-process update candidate publishes at Commit, not at boot: before
            // Commit the parent still owns the carried sessions, and a candidate named
            // in `latest` or `graph/<sid>` would receive in-session `aterm ctl` calls
            // its main thread cannot answer and make the parent's ids ambiguous. On a
            // stand-down nothing is published. The wait and the publication are one
            // function, `publish_when_committed`.
            let Some(listener) = publish_when_committed(
                socket,
                publish_gate.as_ref(),
                plan.latest_link.as_deref(),
                root_identity.as_ref(),
                &store,
            ) else {
                return;
            };
            // The one line whose ABSENCE from the file log now positively means
            // "control setup failed before bind" (every failure branch warns above).
            aterm_log::info!(
                "control socket listening at {sock_path} ({workers} RPC lanes, up to {CONTROL_WAIT_WORKERS} wait lanes and {CONTROL_INLINE_WAITS} waits on RPC lanes, up to {connections_cap} connections, {subscription_workers} subscription lanes grown up to {subscriptions_cap}; excess peers and waits get retry)"
            );
            #[cfg(not(windows))]
            crate::logging::stderr_line!(
                "aterm-gui: control socket listening at {sock_path} (token-gated, same-uid only)"
            );
            // `listening at <PATH> (token-gated` shape, with
            // the HONEST posture parenthetical (never silently claim same-uid): a peer
            // running as another user is refused (SIO_AF_UNIX_GETPEERPID); where the
            // ioctl or the peer's token is unavailable, the control directory's
            // owner-only DACL + the per-launch token are the gates.
            #[cfg(windows)]
            crate::logging::stderr_line!(
                "aterm-gui: control socket listening at {sock_path} (token-gated, dir-ACL + peer pid)"
            );
            // Our entries are published, so take each claim transferred across a
            // handoff (the predecessor has exited) and retire the successor markers
            // it wrote at Commit (`identity_claim`). No-op without a handoff.
            crate::identity_claim::settle_transferred();
            // Every `aterm ctl` begins with `version`, whose reply carries the
            // executable's content signature (a hash of tens of MB). Compute it once
            // here, off every lane; a racing probe waits on the same `OnceLock`.
            let _ = crate::qos::spawn_lane_thread(
                "aterm-build-signature",
                crate::qos::Role::Responsive,
                || {
                    // A lane may be waiting on this result: never below the lanes.
                    let _ = crate::build_info::binary_signature();
                },
            );
            // The fabric bridge, off unless `[fabric] command` (or a development seam)
            // is set. Launched after the bind, the lanes and `BRIDGE_CONTEXT`, so its
            // inherited connection is served by a complete process; it uses none of
            // the lanes.
            #[cfg(unix)]
            crate::fabric_launch::spawn_supervisor(&startup_config);
            // Admission for one accepted peer: never parks the accept thread. It
            // owns the lanes (`'static`): a rebind hands it to a new accept thread.
            let admit = move |stream: CtlStream| {
                // Peer credential gate: refuse other uids before spending a lane (Unix:
                // unverifiable also refuses; Windows: the peer pid's token user where
                // readable, the token remaining the mandatory gate).
                if let Err(why) = control_auth::peer_check(&stream) {
                    log_denial(
                        AUDIT_SUBSYSTEM,
                        &why,
                        aterm_containment::mode_or_containment(),
                        "peer uid mismatch",
                    );
                    // Each half on its own: a peer that sent its request and its
                    // EOF before the accept has shut this end's read half, and
                    // Darwin refuses a both-ways `shutdown` of it, shutting nothing.
                    aterm_uds::hangup::hang_up(&stream);
                    return;
                }
                // Admission counts queued plus running requests, not open connections
                // (idle ones park, waits go to wait lanes, `control_lanes.rs`). Only a
                // socket saturated with work, or at `control_connections_cap()`, gets
                // the busy reply; the listener never blocks.
                if let Err(mut stream) = lanes.admit(stream) {
                    crate::control_listener::note_busy_reply();
                    let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(100)));
                    let _ =
                        stream.write_all(aterm_types::control_verbs::CONTROL_BUSY_LINE.as_bytes());
                    let _ = stream.flush();
                    // No `shutdown(Both)`: the request bytes are unread, and macOS may
                    // discard the queued reply on reset. Dropping after the bounded
                    // write keeps the busy frame, which `aterm-ctl` reads first.
                }
            };
            // Supervised accept loop (`control_listener`): an accept thread that
            // never blocks in `accept`, watched by a watchdog that never touches the
            // socket; a stuck thread or stalled queue is replaced at the same path
            // with the same token. The cold update lane retires it via `endpoint`
            // before `exec` and rebinds if `exec` returns.
            #[cfg(unix)]
            {
                let rebind_plan = plan.clone();
                let rebind_path = sock_path.clone();
                let mut published = crate::control_listener::socket_file_id(&sock_path);
                let rebind = move || {
                    let previous = published;
                    let fresh =
                        crate::control_listener::rebind_socket(&rebind_path, &mut published)?;
                    control_auth::lock_socket_file(&rebind_path);
                    if let Some(previous) = previous {
                        // A fixed endpoint's handoff witness names the socket vnode;
                        // move it with the socket or a later update fails closed.
                        let _ = crate::control_socket_identity::republish_rebound(
                            &rebind_plan,
                            previous,
                        );
                        // So does the exit's record of what it removes, or every
                        // exit after a rebind leaves the socket file behind.
                        if let Some(current) = published {
                            let _ = crate::owned_endpoint::rebound(previous, current);
                        }
                    }
                    Some(fresh)
                };
                crate::control_listener::Supervisor::new(listener, rebind, endpoint)
                    .run(admit, || false);
            }
            #[cfg(windows)]
            {
                let mut accept_backoff: Option<std::time::Duration> = None;
                for stream in listener.incoming() {
                    let stream = match stream {
                        Ok(s) => {
                            accept_backoff = None;
                            s
                        }
                        Err(e) => {
                            // Descriptor/buffer exhaustion recurs immediately; back off
                            // rather than spin (logged once per streak).
                            if let Some(wait) = accept_error_backoff(&e, accept_backoff) {
                                if accept_backoff.is_none() {
                                    crate::logging::stderr_line!(
                                        "aterm-gui: control socket accept failing ({e}); backing off"
                                    );
                                }
                                accept_backoff = Some(wait);
                                std::thread::sleep(wait);
                            }
                            continue;
                        }
                    };
                    admit(stream);
                }
            }
        },
    );
    preparation
}

/// Accept-loop backoff after `err`, given the previous wait in this streak:
/// `Some` for descriptor or buffer exhaustion (`EMFILE`, `ENFILE`, `ENOBUFS`,
/// `ENOMEM`, `WSAEMFILE`, `WSAENOBUFS`), 10 ms doubling to one second; `None`
/// for a per-connection error, retried at once. Also used by
/// `control_listener`.
pub(crate) fn accept_error_backoff(
    err: &std::io::Error,
    prev: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    #[cfg(unix)]
    const EXHAUSTED: [i32; 4] = [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM];
    #[cfg(windows)]
    const EXHAUSTED: [i32; 2] = [10024, 10055];
    #[cfg(not(any(unix, windows)))]
    const EXHAUSTED: [i32; 0] = [];
    let code = err.raw_os_error()?;
    if !EXHAUSTED.contains(&code) {
        return None;
    }
    const FIRST: std::time::Duration = std::time::Duration::from_millis(10);
    const CAP: std::time::Duration = std::time::Duration::from_secs(1);
    Some(prev.map_or(FIRST, |p| (p * 2).min(CAP)))
}

/// Resolve a stable-id selector to a child this aterm holds authority over and
/// its live socket path. Shared by `@`-first verbs and `subscribe`. Fail
/// closed: locally hosted sids are not hops, unspawned children have no entry,
/// and a nonce mismatch (relaunched child) is rejected.
fn resolve_proxy_child(
    sid: &SessionId,
    sock_dir: &std::path::Path,
) -> Option<(crate::proxy::ProxyEntry, String)> {
    let entry = crate::proxy::lookup_child(sid)?;
    let (sock_path, nonce) = crate::proxy::read_graph_entry(sock_dir, sid)?;
    if !entry.nonce.ct_eq(&nonce) {
        return None; // graph entry is for a DIFFERENT launch — fail closed
    }
    // Confine the discovered socket to our runtime dir before dialing with the
    // edge token: the graph entry is same-uid writable, so the nonce check alone
    // cannot stop a redirect to an attacker socket. Fail closed.
    let sock_path = crate::control_auth::confine_proxy_sock(sock_dir, &sock_path)?;
    Some((entry, sock_path))
}

/// Resolve a stable-id selector to a sibling instance's socket: a session hosted
/// by another same-uid aterm this process did not spawn, found through its
/// published graph entry.
///
/// Fail-closed guards, in order:
/// * a locally hosted sid is not a hop ([`proxy_forward_plan`] checks first);
/// * no graph entry means unknown sid;
/// * the socket path must confine to our runtime dir
///   ([`crate::control_auth::confine_proxy_sock`]);
/// * self-dial guard: an entry naming our own socket is stale and refused;
/// * the socket must have a live listener;
/// * the sibling's per-launch token must be readable
///   ([`crate::proxy::read_sibling_token`]), the credential any same-uid client
///   could use directly.
///
/// The token is read only to decide; the forward reads the token beside the
/// socket after connecting ([`crate::proxy::ForwardToken::BesideSocket`]), since
/// an update successor may take the socket in between.
fn resolve_sibling(sid: &SessionId, sock_dir: &std::path::Path) -> Option<String> {
    let (sock_path, _nonce) = crate::proxy::read_graph_entry(sock_dir, sid)?;
    let sock_path = crate::control_auth::confine_proxy_sock(sock_dir, &sock_path)?;
    if crate::proxy::self_sock_path().as_deref() == Some(sock_path.as_str()) {
        return None; // stale entry for a session we no longer host — never self-dial
    }
    if !crate::control_auth::socket_is_live(&sock_path) {
        return None; // dead instance leftovers — swept later, never dialed
    }
    crate::proxy::read_sibling_token(&sock_path)?;
    Some(sock_path)
}

/// The one session a `subscribe` names, read from its text alone (`rest` is the
/// line after the verb): `Some((sid, tail))` for a single `@<sid>`, `tail` being
/// the line after that selector; `None` for self (`@.`, `@`), a local id, a
/// list, `@*` (this instance's live target set, never one session) or no
/// selector. [`proxy_forward_plan`] relays only such a session and
/// [`is_wait_request`] classes the same text, so whether a `subscribe` is
/// relayed and which lane it takes cannot disagree.
fn subscribe_session(rest: &str) -> Option<(SessionId, &str)> {
    let rest = rest.trim_start();
    let (token, tail) = rest.split_at(rest.find(char::is_whitespace).unwrap_or(rest.len()));
    let body = token.strip_prefix('@')?;
    if body == "*" || body.contains(',') {
        return None;
    }
    match Selector::parse(body) {
        Selector::Sid(sid) => Some((sid, tail)),
        Selector::SelfTok | Selector::Local(_) => None,
    }
}

/// Decide whether `line` from `scope` must be forwarded to another instance,
/// returning `(target_sock_path, first_line)`, else `None` (handle locally).
/// Two hop kinds, in order:
///
/// 1. Spawned child ([`resolve_proxy_child`]): forwarded with the per-op edge
///    token minted at spawn, the selector rewritten to `@.` so the child runs
///    the verb on itself and cannot re-forward.
/// 2. Sibling instance ([`resolve_sibling`]): forwarded with the sibling's own
///    per-launch token (Owner there), keeping `@<sid>`. Only Owner-scope callers
///    reach this, and the credential is one any same-uid client could read. The
///    forward reads the token beside the socket after connecting
///    ([`crate::proxy::ForwardToken`]).
///
/// Pure, so the security decisions are unit-testable without a relay.
fn proxy_forward_plan(
    line: &str,
    scope: Scope,
    store: &Store,
    sock_dir: &std::path::Path,
) -> Option<(String, crate::proxy::ForwardLine)> {
    // Only Owner-class scopes forward; an Edge falls through to local
    // resolution, which denies the cross-process sid.
    if !scope.is_owner_class() {
        return None;
    }
    let line = line.strip_suffix('\r').unwrap_or(line);
    // A sid this instance hosts is a normal in-process target, never a hop on
    // either path — asked once per request, before both resolvers.
    let hosted = |sid: &SessionId| {
        store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(sid)
            .is_some()
    };

    // `subscribe` takes its selector second (`subscribe @<sel> <streams> …`), so
    // handle it here; only a single remote session forwards
    // ([`subscribe_session`]), a list or `@*` stays local.
    if let Some(rest) = line.strip_prefix("subscribe ") {
        let (sid, tail) = subscribe_session(rest)?;
        if hosted(&sid) {
            return None;
        }
        let op = required_op("subscribe")?; // ReadScreen
        if let Some((entry, sock_path)) = resolve_proxy_child(&sid, sock_dir) {
            let tok = entry.token_for(op)?;
            // Rewrite the child's selector to `@.` while preserving the streams + flags.
            let rewritten = format!("subscribe @.{tail}");
            return Some((
                sock_path,
                crate::proxy::ForwardLine::edge(tok.to_hex(), rewritten),
            ));
        }
        // Sibling hop: keep the selector (the sibling resolves the sid itself).
        let sock_path = resolve_sibling(&sid, sock_dir)?;
        return Some((
            sock_path,
            crate::proxy::ForwardLine::sibling(line.to_string()),
        ));
    }

    let (first, verb_line) = line.split_once(' ')?;
    let sel_body = first.strip_prefix('@')?; // no @selector → local self path
    let Selector::Sid(sid) = Selector::parse(sel_body) else {
        return None; // only stable-id selectors cross processes
    };
    if hosted(&sid) {
        return None;
    }
    let verb = verb_line.split_whitespace().next().unwrap_or("");
    // Only op-classed verbs (read/write/signal) cross processes — identity /
    // privilege verbs (`grant`/`whoami`/...) stay strictly local on BOTH hops.
    let op = required_op(verb)?;
    if let Some((entry, sock_path)) = resolve_proxy_child(&sid, sock_dir) {
        let tok = entry.token_for(op)?;
        // The child is the DIRECT target: rewrite its selector to `@.` (run on self).
        let rewritten = format!("@. {verb_line}");
        return Some((
            sock_path,
            crate::proxy::ForwardLine::edge(tok.to_hex(), rewritten),
        ));
    }
    // Sibling hop keeps `@<sid>` so the sibling resolves among its own tabs.
    //
    // A relaunched child (nonce mismatch above) falls through here on purpose:
    // it is reachable like any sibling, with its own token, never the stale
    // edge tokens.
    let sock_path = resolve_sibling(&sid, sock_dir)?;
    Some((
        sock_path,
        crate::proxy::ForwardLine::sibling(line.to_string()),
    ))
}

/// If `line` targets a session this process does not host, forward the whole
/// connection ([`proxy_forward_plan`]) and relay bytes, returning `true` (the
/// caller then ends the connection); `false` for a local request.
fn try_proxy_forward<R: Read>(
    line: &str,
    scope: Scope,
    store: &Store,
    sock_dir: &std::path::Path,
    client: &CtlStream,
    reader: &mut BufReader<R>,
) -> bool {
    let Some((sock_path, first_line)) = proxy_forward_plan(line, scope, store, sock_dir) else {
        return false;
    };
    // Relay takeover is intentionally long-lived. Keep the inherited socket
    // explicitly deadline-free even if handshake policy changes independently.
    if client.set_read_timeout(None).is_err() {
        let _ = (&*client).write_all(b"ERR forward timeout setup\n");
        let _ = (&*client).flush();
        return true;
    }
    // Forward anything the BufReader already read past the request line, then relay.
    let pre = crate::proxy::drain_buffered(reader);
    // A dial/handshake failure happens BEFORE any relay byte (the client stream is
    // untouched), so honor the contract and answer ERR rather than a silent EOF.
    if let Err(error) = crate::proxy::connect_and_relay(&sock_path, &first_line, client, &pre) {
        // A connect timeout is reported as such; other failures say the host
        // could not be reached.
        let reply = if error.kind() == std::io::ErrorKind::TimedOut {
            format!("ERR forward: {error}\n")
        } else {
            "ERR forward: could not reach the aterm that hosts that session\n".to_string()
        };
        let _ = (&*client).write_all(reply.as_bytes());
        let _ = (&*client).flush();
    }
    true
}

/// Parse the optional selector and verb without resolving a terminal target.
/// This is deliberately the first dispatch step for ordinary request lines.
fn request_head(line: &str) -> (Option<Selector>, &str, &str) {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let (selector, line) = match line.split_once(' ') {
        Some((first, tail)) if first.starts_with('@') => (Some(Selector::parse(&first[1..])), tail),
        None if line.starts_with('@') => (Some(Selector::parse(&line[1..])), ""),
        _ => (None, line),
    };
    match line.split_once(' ') {
        Some((verb, rest)) => (selector, verb, rest),
        None => (selector, line, ""),
    }
}

/// `Some(ERR usage)` when a zero-argument verb (`version`, `sessions`, `who`,
/// `whoami`) is given a tail, else `None`, so a flag like `sessions --json` is
/// not silently dropped.
fn refuse_stray_args(verb: &str, rest: &str) -> Option<String> {
    let stray = rest.trim();
    (!stray.is_empty()).then(|| format!("ERR usage: {verb}\n"))
}

#[cfg(test)]
mod stray_args_tests {
    use super::refuse_stray_args;

    /// A zero-argument verb refuses a tail (`sessions --json`) with the ordinary
    /// usage error; the bare form is unchanged.
    #[test]
    fn a_zero_argument_verb_refuses_its_tail_and_passes_bare() {
        for verb in ["version", "sessions", "who", "whoami"] {
            assert_eq!(
                refuse_stray_args(verb, " --json").as_deref(),
                Some(format!("ERR usage: {verb}\n").as_str()),
                "`{verb} --json` must be a usage error, not a silent drop"
            );
            assert_eq!(refuse_stray_args(verb, ""), None, "bare `{verb}` proceeds");
            assert_eq!(
                refuse_stray_args(verb, "   "),
                None,
                "whitespace tail is bare"
            );
        }
    }
}

fn dispatch_meta_verb(
    verb: &str,
    rest: &str,
    selector: Option<&Selector>,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
) -> Option<String> {
    match verb {
        "version" => {
            Some(refuse_stray_args(verb, rest).unwrap_or_else(crate::build_info::control_line))
        }
        "update" => Some(cmd_update(rest, scope, proxy)),
        "help" | "verbs" => Some(cmd_help(rest)),
        // `privacy` — the macOS consent posture. Instance-wide and self-scoped:
        // unlike the other meta verbs (whose answer is the same whoever is
        // named, so a selector is merely ignored), this one aggregates PER
        // SESSION, so a selector would read as "the posture of that session"
        // and be silently wrong. It is rejected, as the verb's help states.
        // The scope goes with it: an edge's report lists only the sessions it
        // may read, never the roster `sessions` withholds from it.
        "privacy" => Some(match selector {
            None | Some(Selector::SelfTok) => {
                crate::control_privacy::cmd_privacy(rest, scope, proxy)
            }
            Some(_) => crate::control_privacy::no_selector_error().to_string(),
        }),
        _ => None,
    }
}

/// Dispatch app-level verbs without a PTY mirror; an optional terminal only
/// fills the `metrics` rows/cols while the front is a terminal.
///
/// Only owner-class scopes reach this (`native_control_decision` denies edges
/// every App verb), so exit-ledger attribution for `tab close` / `invoke
/// CloseTab` is by kind, `ctl` or `bridge`
/// ([`control_session::caller_actor_of`] with `None`).
#[allow(clippy::too_many_arguments)]
fn dispatch_app_verb(
    verb: &str,
    rest: &str,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
    sock_dir: &std::path::Path,
    active_term: Option<&Arc<Mutex<Terminal>>>,
) -> Option<ControlReply> {
    let response = match verb {
        "window" => return Some(control_media::cmd_window(proxy, rest, sock_dir)),
        "video" => {
            return Some(control_media::cmd_video(
                proxy,
                rest,
                sock_dir,
                scope.is_owner_class(),
            ));
        }
        "chrome" => control_media::cmd_chrome(proxy),
        // `appearance`: the OS light/dark reading and what it drew; a dev
        // build can inject a flip.
        "appearance" => crate::os_appearance::cmd_appearance(proxy, rest),
        // `pressure`: the memory-pressure level and the crash-restore journal's shed; a
        // dev build on macOS can inject a level.
        "pressure" => crate::crash_restore::cmd_pressure(proxy, rest),
        // `appstatus`: what aterm is doing on its own (a toolchain install, an
        // update download): live status-bar rows plus finished jobs the ring
        // remembers. Instance state, so it answers with no terminal.
        "appstatus" => control_query::cmd_appstatus(proxy),
        // `messages`: the message log, one row per record (the band and
        // Settings ▸ Messages as text). Instance state.
        "messages" => control_query::cmd_messages(proxy, rest),
        "panes" => control_media::cmd_panes(proxy, None),
        "controls" => control_media::cmd_controls(proxy, rest),
        "inspect" => control_media::cmd_inspect(proxy, rest),
        "open" => control_media::cmd_open(proxy, rest),
        "act" => control_media::cmd_act(proxy, rest),
        "invoke" => {
            control_media::cmd_invoke(proxy, rest, control_session::caller_actor_of(scope, None))
        }
        "confirm" => control_media::cmd_confirm(proxy, rest),
        "rain" => control_media::cmd_rain(proxy, rest),
        "theme" => control_media::cmd_theme(proxy, rest, None),
        "fx" => control_media::cmd_fx(proxy, rest),
        "streak" => control_media::cmd_streak(proxy, rest),
        "tone" => control_media::cmd_tone(proxy, rest),
        "trail" => control_media::cmd_trail(proxy, rest),
        "kitty" => control_media::cmd_kitty(proxy, rest),
        // No selector on this path, so no `@<sid>` aim; `window=` still aims.
        "spawn" => control_media::cmd_spawn(proxy, rest, None),
        "settings" => control_media::cmd_settings_overlay(proxy, rest),
        "tab" => control_input::cmd_tab(proxy, rest, control_session::caller_actor_of(scope, None)),
        "hover" => control_input::cmd_hover(proxy, rest),
        // Window-surface drives target the front window and need no PTY, so they
        // are answered here as well as in the session router.
        "find" => control_input::cmd_find(proxy, rest),
        "pane" => control_input::cmd_pane(proxy, rest),
        "pointer" => control_input::cmd_pointer(proxy, rest),
        "metrics" => control_query::cmd_metrics(active_term, rest),
        _ => return None,
    };
    Some(response.into())
}

fn selector_is_live(store: &Store, selector: &Selector) -> bool {
    match selector {
        Selector::SelfTok => true,
        Selector::Local(id) => store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_local(*id)
            .is_some(),
        Selector::Sid(id) => store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(id)
            .is_some(),
    }
}

/// Whether `post` must refuse this connection. An `Edge` is refused outright:
/// a write edge over S authorizes typing into S, not speaking as S on the bus,
/// where the Owner attests `from=` as identity.
fn post_scope_denied(scope: Scope) -> bool {
    !scope.is_owner_class()
}

/// Whether a `lease` request from a non-bridge connection claims the bridge's
/// `holder=fabric:*` namespace. That sub-form of an ordinary Owner verb is
/// bridge-only (design §11.2) and no verb-table row can express it, so it is
/// checked here; otherwise any `lease` caller could attribute its own drive to
/// a human in `who` and `lease status`.
///
/// Acquire only. `lease release … force` still steals any hold, `fabric:` ones
/// included, as a deliberate operator override; a theft shows in `who` under
/// the thief's name.
fn lease_forges_fabric_holder(scope: Scope, rest: &str) -> bool {
    if scope.is_bridge() {
        return false;
    }
    let mut toks = rest.split_whitespace();
    if toks.next() != Some("acquire") {
        return false;
    }
    toks.filter_map(|t| t.strip_prefix("holder="))
        .any(|h| h.starts_with("fabric:"))
}

/// `fabric status` / `fabric attach [<command...>]`: the bridge supervisor of a
/// running instance, so an instance launched without `[fabric] command` can
/// attach one without a relaunch.
///
/// Requires `Scope::Owner` exactly (not [`Scope::is_owner_class`]): a selector,
/// edge token or the bridge itself is `ERR denied`. Whoever arms the supervisor
/// chooses which process holds [`Scope::Bridge`]; the bridge is that process,
/// minted only after the latch closed (`fabric_launch::launch_once`). Pure over
/// `(rest, selector, scope)` so tests drive the real decision.
fn dispatch_fabric_verb(
    rest: &str,
    selector: Option<&Selector>,
    scope: Scope,
    store: &Store,
) -> String {
    if !matches!(selector, None | Some(Selector::SelfTok)) || !matches!(scope, Scope::Owner) {
        return "ERR denied\n".to_string();
    }
    cmd_fabric(rest, store)
}

/// `fabric retire <sid>…`: ask this instance's bridge to retire presence rows
/// nobody hosts (the doctor's bridge-up path). The request goes CLI → here (a
/// hosted sid is refused) → events lane → bridge, which re-checks hosting and
/// publishes `exited` through its own sequence. Answers `OK requested=<n>`, or
/// `ERR hosted <sid>` with nothing requested.
fn cmd_fabric_retire(words: &[&str], store: &Store) -> String {
    if words.is_empty() {
        return FABRIC_USAGE.to_string();
    }
    for sid in words {
        if !sid.starts_with("s-") || !crate::fabric::valid_principal(sid) {
            return FABRIC_USAGE.to_string();
        }
    }
    let mut g = store.write().unwrap_or_else(|p| p.into_inner());
    for sid in words {
        if g.by_sid(&SessionId::new((*sid).to_string())).is_some() {
            return format!("ERR hosted {sid}\n");
        }
    }
    let mut n = 0;
    for sid in words {
        if g.request_retire(sid) {
            n += 1;
        }
    }
    format!("OK requested={n}\n")
}

/// `hold <sid> on|off [reason=<pct>] [origin=fleet|local]`: the drive halt.
///
/// `Access::OwnerOnly` in the table; what the table cannot express is decided
/// here and in the handler: `Scope::Bridge` issues the fleet hold,
/// `Scope::Owner` the local one (`origin=local` only), and an Owner act on a
/// standing fleet hold is refused, so a fleet halt cannot be lifted locally.
///
/// Every refusal is audited (edge tokens, selectors, Owner naming the fleet or
/// touching a fleet hold). Both dispatch entries route `hold` here before the
/// silent generic `OwnerOnly` gate. Pure over `(rest, selector, scope, store)`.
fn dispatch_hold_verb(
    rest: &str,
    selector: Option<&Selector>,
    scope: Scope,
    store: &Store,
) -> String {
    use crate::fabric::{HOLD_DENIED, HoldIssuer, cmd_hold};

    let issuer = match scope {
        #[cfg(any(unix, test))]
        Scope::Bridge => HoldIssuer::Bridge,
        Scope::Owner => HoldIssuer::Owner,
        Scope::Edge(_) => {
            log_denial(
                AUDIT_SUBSYSTEM,
                "hold",
                aterm_containment::mode_or_containment(),
                "hold is owner-class: an edge token may not halt or lift",
            );
            return "ERR denied\n".to_string();
        }
    };
    if !matches!(selector, None | Some(Selector::SelfTok)) {
        log_denial(
            AUDIT_SUBSYSTEM,
            "hold",
            aterm_containment::mode_or_containment(),
            "hold names its session as the argument: a selector is refused",
        );
        return "ERR denied\n".to_string();
    }
    let reply = cmd_hold(store, rest, issuer);
    if reply == HOLD_DENIED {
        log_denial(
            AUDIT_SUBSYSTEM,
            "hold",
            aterm_containment::mode_or_containment(),
            "the fleet hold is bridge-issued only: an Owner token may neither name \
             origin=fleet nor set, replace or lift a standing fleet hold",
        );
    }
    reply
}

const FABRIC_USAGE: &str =
    "ERR usage: fabric status | fabric attach [<command...>] | fabric retire <sid>...\n";

/// `story <verb> [<text>]`: the watcher's decision for a session, told to the
/// window. Owner-only (a child edge must not write `✓ approved` onto a band it
/// does not drive) and session-scoped by selector: `@<sid> story approved`, or
/// bare `story …` for the connection's own session, which falls through to
/// `handle`; the `None` arm is defensive. Grammar is checked in
/// [`control_query::cmd_story`].
fn dispatch_story_verb(
    rest: &str,
    selector: Option<&Selector>,
    scope: Scope,
    store: &Store,
    self_session: Option<u64>,
    proxy: &EventLoopProxy<Wake>,
) -> String {
    if !scope.is_owner_class() {
        log_denial(
            AUDIT_SUBSYSTEM,
            "story",
            aterm_containment::mode_or_containment(),
            "story is owner-class: an edge token may not write the band",
        );
        return "ERR denied\n".to_string();
    }
    let session = match selector {
        Some(Selector::Local(n)) => *n,
        Some(Selector::Sid(sid)) => {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            match g.by_sid(sid) {
                Some(h) => h.local_id,
                None => return "ERR no such session\n".to_string(),
            }
        }
        None | Some(Selector::SelfTok) => match self_session {
            Some(s) => s,
            None => return "ERR story: no session (name one: @<sid> story …)\n".to_string(),
        },
    };
    control_query::cmd_story(proxy, session, rest)
}

/// `@<sid> rekey shell=<pid>` / `@<sid> rekey withdraw` (`shell_rekey::typed`):
/// owner-class because it issues a key that admits a session's OSC 133/633
/// marks. The selector is required, since the tab it heals is never the
/// connection's own.
fn dispatch_rekey_verb(
    rest: &str,
    selector: Option<&Selector>,
    scope: Scope,
    store: &Store,
) -> String {
    if !scope.is_owner_class() {
        log_denial(
            AUDIT_SUBSYSTEM,
            "rekey",
            aterm_containment::mode_or_containment(),
            "rekey is owner-class: an edge token may not mint a session's mark authority",
        );
        return "ERR denied\n".to_string();
    }
    let (term, master, sid) = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let handle = match selector {
            Some(Selector::Local(n)) => g.by_local(*n),
            Some(Selector::Sid(sid)) => g.by_sid(sid),
            None | Some(Selector::SelfTok) => return REKEY_USAGE.to_string(),
        };
        match handle {
            Some(h) => (h.term.clone(), h.master, h.sid.clone()),
            None => return "ERR no such session\n".to_string(),
        }
    };
    cmd_rekey(&term, master, &sid, rest)
}

const REKEY_USAGE: &str = "ERR usage: @<sid> rekey shell=<pid> | @<sid> rekey withdraw\n";

/// The typed re-key against the resolved tab: `shell=<pid>` issues one only
/// while `<pid>` leads the tab's foreground process group (read from the master
/// as `signal pid=` does); `withdraw` settles the waiting one.
///
/// THE TYPED UPGRADE (2026-09-26, `shell_body`): for a tab whose shell does
/// not sign this build's integration revision at the prompt it holds — one
/// from before LOADERS, or a loader whose prompt hook never runs (2026-10-01:
/// a fish from before the prompt event under a prompt its config.fish
/// defines), which no body pointer reaches — the file also names this build's
/// script folder and the session's body pointer path, and the relaunch line
/// sources this build's loader, healthy or degraded (a healthy one keeps its
/// key: the file hands back the one it signs with). The ENGINE decides
/// (`shell_rekey::typed`), never the registry's loader flag: that says a
/// loader was spawned, not that it runs. A folder that cannot be made sure
/// of, or no pointer path, issues what it did before.
#[cfg(unix)]
fn cmd_rekey(
    term: &Arc<Mutex<Terminal>>,
    master: i32,
    sid: &aterm_session::SessionId,
    rest: &str,
) -> String {
    use crate::shell_rekey::typed;
    let mut words = rest.split_whitespace();
    match (words.next(), words.next()) {
        (Some("withdraw"), None) => format!("OK rekey {}\n", typed::settle(term, sid).word()),
        (Some(arg), None) => {
            let Some(pid) = arg
                .strip_prefix("shell=")
                .and_then(|p| p.parse::<i32>().ok())
                .filter(|&p| p > 1)
            else {
                return REKEY_USAGE.to_string();
            };
            // SAFETY: `tcgetpgrp` on this session's own PTY master fd, which the
            // registry holds open for the session's life; it takes no pointers.
            let pgrp = unsafe { libc::tcgetpgrp(master) };
            if pgrp <= 0 {
                return "ERR no foreground process group\n".to_string();
            }
            if pgrp != pid {
                return format!("ERR pid {pid} is not the foreground process group ({pgrp})\n");
            }
            let upgrade = || {
                Some(typed::Upgrade {
                    folder: aterm_core::shell_integration::ensure_script_set().ok()?,
                    pointer: crate::shell_body::path_for(sid)?,
                })
            };
            match typed::issue(term, sid, upgrade) {
                // The path is the rest of ONE reply line, so a path no line can
                // carry whole is taken back rather than sent in pieces.
                Ok(path) => match path.to_str().filter(|p| !p.chars().any(char::is_control)) {
                    Some(p) => format!("OK rekey ttl={} path={p}\n", typed::TTL.as_secs()),
                    None => {
                        typed::settle(term, sid);
                        "ERR rekey the control dir's path cannot be sent on one line\n".to_string()
                    }
                },
                Err(no) => no.reply(),
            }
        }
        _ => REKEY_USAGE.to_string(),
    }
}

/// Windows arm: no foreground process group to prove the shell by.
#[cfg(not(unix))]
fn cmd_rekey(
    _term: &Arc<Mutex<Terminal>>,
    _master: i32,
    _sid: &aterm_session::SessionId,
    _rest: &str,
) -> String {
    "ERR rekey unsupported on this platform\n".to_string()
}

/// The `rekey` gates before any key exists: owner-class, a named tab, grammar,
/// and the kernel confirming the pid leads the foreground group. Only then is
/// the tab's integration consulted.
#[cfg(all(test, unix))]
mod rekey_verb_tests {
    use super::*;

    /// A pty whose foreground process group is a `sleep` that leads its own
    /// session on it: the stand-in for a tab whose shell holds the terminal.
    struct Leader {
        master: i32,
        child: std::process::Child,
    }

    impl Drop for Leader {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            // SAFETY: the master fd this test opened and alone owns.
            unsafe { libc::close(self.master) };
        }
    }

    fn leader() -> Leader {
        use aterm_uds::spawnfd::InheritNothing as _;
        use std::os::fd::{FromRawFd as _, OwnedFd};
        use std::os::unix::process::CommandExt as _;
        // Both ends are close-on-exec from birth, so the `sleep` holds its
        // terminal and never a copy of its master.
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: `slave` is a fresh fd this function alone owns.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let stdio = || std::process::Stdio::from(slave.try_clone().expect("dup slave"));
        // std's Command on purpose: the PTY child needs `pre_exec` (setsid + TIOCSCTTY), which spawnfd lacks;
        // the strip first, so the `sleep` holds its terminal and nothing else of this process.
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.inherit_nothing()
            .arg("30")
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio());
        // SAFETY: in the forked child before exec: `setsid` and
        // `ioctl(TIOCSCTTY)` are async-signal-safe and touch nothing of the
        // parent's.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn sleep");
        drop(slave);
        let led = Leader { master, child };
        let pid = libc::pid_t::try_from(led.child.id()).expect("pid");
        // A child forked under load can sit seconds before its exec: a hang
        // detector, not a 10 s budget.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the child leads the pty's foreground",
            // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
            || (unsafe { libc::tcgetpgrp(led.master) } == pid).then_some(()),
        );
        led
    }

    #[test]
    fn rekey_is_owner_class_and_names_its_tab() {
        let store = crate::session_store::new_store();
        let tab = Some(Selector::Sid(aterm_session::SessionId::new(
            "s-00000000000000000001",
        )));
        assert_eq!(
            dispatch_rekey_verb(
                "shell=4242",
                tab.as_ref(),
                Scope::Edge(aterm_session::EdgeToken::generate()),
                &store
            ),
            "ERR denied\n"
        );
        assert_eq!(
            dispatch_rekey_verb("shell=4242", None, Scope::Owner, &store),
            REKEY_USAGE,
            "the bare form names no tab to heal"
        );
        assert_eq!(
            dispatch_rekey_verb("shell=4242", tab.as_ref(), Scope::Owner, &store),
            "ERR no such session\n"
        );
    }

    #[test]
    fn rekey_is_issued_only_while_the_named_shell_leads_the_foreground() {
        let led = leader();
        let pid = led.child.id();
        let sid = aterm_session::SessionId::new("s-00000000000000000002");
        let mut healthy = Terminal::new(24, 80);
        healthy.set_require_shell_integration_nonce(true);
        healthy.authorize_shell_integration([3; 32]);
        // Its shell signs THIS build's revision, so it is refused with nothing
        // built or written (one behind is handed the upgrade: `shell_rekey`).
        healthy.process(
            format!(
                "\x1b]633;P;AtermIntegration={};id={}\x07",
                crate::shell_body::own_rev(),
                aterm_core::shell_integration::hex_encode(&[3; 32])
            )
            .as_bytes(),
        );
        let healthy = Arc::new(Mutex::new(healthy));
        let off = Arc::new(Mutex::new(Terminal::new(24, 80)));
        for bad in [
            "",
            "shell=",
            "shell=abc",
            "shell=1",
            "shell=5 more",
            "withdraw now",
            "4242",
        ] {
            assert_eq!(
                cmd_rekey(&healthy, led.master, &sid, bad),
                REKEY_USAGE,
                "{bad:?}"
            );
        }
        let other = pid + 1;
        assert_eq!(
            cmd_rekey(&healthy, led.master, &sid, &format!("shell={other}")),
            format!("ERR pid {other} is not the foreground process group ({pid})\n"),
        );
        assert_eq!(
            cmd_rekey(&healthy, -1, &sid, &format!("shell={pid}")),
            "ERR no foreground process group\n",
        );
        // Past the kernel's proof, the tab is asked, and neither is lost.
        assert!(
            cmd_rekey(&healthy, led.master, &sid, &format!("shell={pid}"))
                .starts_with("ERR rekey integration=on"),
        );
        assert!(
            cmd_rekey(&off, led.master, &sid, &format!("shell={pid}"))
                .starts_with("ERR rekey integration=off"),
        );
        assert_eq!(
            cmd_rekey(&healthy, led.master, &sid, "withdraw"),
            "OK rekey none\n"
        );
    }
}

/// `topic`: the receiver-side broadcast opt-in, owner-class and selector-taking
/// (a manager arms a worker's topics from its own window). Owner-class because
/// adding a topic changes what lands in an agent's inbox. Refusals are audited
/// here rather than left to the generic silent `ERR denied`.
fn dispatch_topic_verb(
    rest: &str,
    selector: Option<&Selector>,
    scope: Scope,
    store: &Store,
    subscribers: &Subscribers,
    self_session: Option<u64>,
) -> String {
    if !scope.is_owner_class() {
        log_denial(
            AUDIT_SUBSYSTEM,
            "topic",
            aterm_containment::mode_or_containment(),
            "topic is owner-class: an edge token may not choose what reaches a mailbox",
        );
        return "ERR denied\n".to_string();
    }
    let (local, ctx) = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let handle = match selector {
            Some(Selector::Local(n)) => g.by_local(*n),
            Some(Selector::Sid(sid)) => g.by_sid(sid),
            None | Some(Selector::SelfTok) => match self_session {
                Some(s) => g.by_local(s),
                None => {
                    return "ERR topic: no session (name one: @<sid> topic ls)\n".to_string();
                }
            },
        };
        match handle {
            Some(h) => (h.local_id, h.ctx.clone()),
            None => return "ERR no such session\n".to_string(),
        }
    };
    let (reply, changed) = crate::fabric::cmd_topic(&ctx, rest);
    // The record drains NOW, not on the next 250 ms tick — `meta`'s rule.
    if changed {
        wake_watchers(subscribers, local);
    }
    reply
}

/// `fabric` sub-forms. `attach`'s argv is split on whitespace like
/// `fabric_launch::configured_command`, never via a shell.
#[cfg(unix)]
fn cmd_fabric(rest: &str, store: &Store) -> String {
    let mut words = rest.split_whitespace();
    match words.next() {
        Some("status") if words.next().is_none() => {
            with_unserved_mirror(fabric_status_line(), store)
        }
        Some("attach") => cmd_fabric_attach(words.map(str::to_string).collect()),
        Some("retire") => cmd_fabric_retire(&words.collect::<Vec<_>>(), store),
        _ => FABRIC_USAGE.to_string(),
    }
}

/// `fabric status` with the bus mirror's retained state no bridge serves
/// (`session_log_mirror::StoreMirror::unserved_facts`) before its newline:
/// nothing on an instance that holds none, read with the store's read guard
/// alone (no mirror lock, no link lock) on one that never mirrored.
fn with_unserved_mirror(line: String, store: &Store) -> String {
    let facts = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .mirror()
        .unserved_facts();
    if facts.is_empty() {
        return line;
    }
    match line.strip_suffix('\n') {
        Some(head) => format!("{head}{facts}\n"),
        None => format!("{line}{facts}"),
    }
}

/// `OK state=<absent|connected|stale|stalled|disconnected> supervised=<0|1>
/// command=<pct|-> reason=<token|-> rtt_ms=<n|-> link_age_ms=<n|->
/// build=<token> bridge_lane=<n|-> bridge_build=<token|-> lane_faults=<n>`.
///
/// `state=` is the link token `status` carries; `supervised=` is the latch
/// `post` reads; `command=` is the supervised argv (or the launch one when
/// unarmed). The next three are the bridge's broker-link report
/// (`fabric::link_report`). `build=` is this instance's own build and the
/// last three the bridge's `hello` and its lane (`fabric::cmd_hello`,
/// `fabric::note_lane_fault`): a `bridge_build=` other than `build=` is a
/// bridge of another build than its instance, `-` one from before `hello`.
fn fabric_status_line() -> String {
    #[cfg(unix)]
    let (supervised, command) = {
        let s = crate::fabric_launch::status();
        let command = s
            .armed
            .or(s.configured)
            .map_or_else(|| "-".to_string(), |argv| pct_encode(&argv.join(" ")));
        (crate::fabric::bridge_supervised(), command)
    };
    #[cfg(not(unix))]
    let (supervised, command) = (false, "-".to_string());
    let (reason, rtt, age) = crate::fabric::fabric_link_facts();
    let (lane, bridge_build, faults) = crate::fabric::fabric_bridge_facts();
    let n = |v: Option<u64>| v.map_or_else(|| "-".to_string(), |n| n.to_string());
    // The bus mirror's tail, empty (the line as it was) unless the bridge's
    // recorded `hello` asked for it (session log phase 4).
    let mirror = crate::fabric::fabric_mirror_facts();
    format!(
        "OK state={} supervised={} command={command} reason={reason} rtt_ms={} link_age_ms={} \
         build={} bridge_lane={} bridge_build={} lane_faults={faults}{mirror}\n",
        crate::fabric::fabric_state(),
        u8::from(supervised),
        n(rtt),
        n(age),
        aterm_types::version::build_id(),
        n(lane.map(u64::from)),
        bridge_build.as_deref().unwrap_or("-"),
    )
}

/// `OK attached command=<pct>` when the supervisor thread started; the bridge
/// connects asynchronously (see `fabric status`).
///
/// Once per process: a second attach is an `ERR` naming what runs, checked
/// before the argv is resolved (`arm` re-checks under its lock).
///
/// IT ARMS ONLY WHAT THE INSTANCE'S CONFIG NAMES. The process this arms holds
/// `Scope::Bridge` — `deliver`'s attested `from=`/`trust=` and the fleet halt
/// — and this verb's scope is `Owner`, which every in-session client holds; so
/// the argv is not the caller's to choose. A bare attach arms the `[fabric]
/// command` aterm.toml names now (`fabric_launch::command_on_disk`, read again,
/// so the command `aterm fabric on` just wrote counts), else the one the
/// instance was launched with; an explicit argv is armed only when it IS one
/// of those two (`fabric on` sends the one it wrote) and is otherwise `ERR
/// fabric not configured command=<pct>`, nothing armed and the latch open.
///
/// `ERR fabric not executable program=<pct> reason=<token>` is `arm`'s
/// pre-flight, refused before the latch closes so a corrected command may
/// follow (`fabric_launch::preflight`).
#[cfg(unix)]
fn cmd_fabric_attach(argv: Vec<String>) -> String {
    use crate::fabric_launch::ArmError;
    let already = |running: Vec<String>| {
        format!(
            "ERR fabric already supervised command={}\n",
            pct_encode(&running.join(" "))
        )
    };
    let snapshot = crate::fabric_launch::status();
    if let Some(running) = snapshot.armed {
        return already(running);
    }
    let on_disk = crate::fabric_launch::command_on_disk();
    let argv = if argv.is_empty() {
        match on_disk.or(snapshot.configured) {
            Some(configured) => configured,
            None => return "ERR fabric no command\n".to_string(),
        }
    } else if on_disk.as_ref() == Some(&argv) || snapshot.configured.as_ref() == Some(&argv) {
        argv
    } else {
        return format!(
            "ERR fabric not configured command={}\n",
            pct_encode(&argv.join(" "))
        );
    };
    match crate::fabric_launch::arm(argv.clone()) {
        Ok(()) => format!("OK attached command={}\n", pct_encode(&argv.join(" "))),
        Err(ArmError::AlreadySupervised(running)) => already(running),
        Err(ArmError::NoCommand) => "ERR fabric no command\n".to_string(),
        Err(ArmError::NotExecutable { program, reason }) => format!(
            "ERR fabric not executable program={} reason={reason}\n",
            pct_encode(&program)
        ),
        Err(ArmError::Thread(e)) => format!("ERR fabric attach {}\n", pct_encode(&e.to_string())),
    }
}

/// No `socketpair` inheritance on Windows (`fabric_launch` is unix-only):
/// `status` answers `absent`, and `attach` says why it cannot.
#[cfg(not(unix))]
fn cmd_fabric(rest: &str, store: &Store) -> String {
    let mut words = rest.split_whitespace();
    match words.next() {
        Some("status") if words.next().is_none() => {
            with_unserved_mirror(fabric_status_line(), store)
        }
        Some("attach") => "ERR fabric unavailable on this platform\n".to_string(),
        Some("retire") => cmd_fabric_retire(&words.collect::<Vec<_>>(), store),
        _ => FABRIC_USAGE.to_string(),
    }
}

/// The bridge plane: the `Access::BridgeOnly` verbs `deliver`, `link`, `hello`,
/// `outbox`, `outbox sent`, `log-take` and `log-landed`. `None` means not a
/// bridge verb.
///
/// A LINE OF THE PLANE THIS ENDPOINT CANNOT PARSE is counted for the lane
/// (`fabric::note_lane_fault`, `fabric status lane_faults=`): every one of
/// these verbs answers such a line `ERR usage`, and a bridge that shares this
/// endpoint's grammar writes none, so the count is the two not reading the lane
/// alike — the bridge holds what it could not deliver back, and this is where
/// the instance says so.
///
/// Answered before session resolution because each names its session as an
/// argument, and a delivery must land even on a windowless instance.
///
/// A selector is rejected (it could aim the plane elsewhere), and every scope
/// but `Bridge` is denied, Owner included: every in-session client holds Owner,
/// and these are what an injected agent would use to forge orders, read all
/// outbound traffic or fake a `post --wait` release. `hold` is Owner-class and
/// handled by [`dispatch_hold_verb`].
///
/// Pure over `(verb, rest, selector, scope, store)`.
fn dispatch_bridge_verb(
    verb: &str,
    rest: &str,
    selector: Option<&Selector>,
    scope: Scope,
    store: &Store,
) -> Option<String> {
    if !aterm_types::control_verbs::is_bridge_only(verb) {
        return None;
    }
    if !matches!(selector, None | Some(Selector::SelfTok)) || !scope.is_bridge() {
        log_denial(
            AUDIT_SUBSYSTEM,
            &format!("bridge-only {verb}"),
            aterm_containment::mode_or_containment(),
            &format!("only the inherited bridge connection may run {verb}"),
        );
        return Some("ERR denied\n".to_string());
    }
    let generation = crate::fabric::lane_generation();
    let reply = match verb {
        "deliver" => crate::fabric::cmd_deliver(store, rest),
        // The bridge's own broker link, gated to the lane's incarnation. A
        // replaced bus log (`forgot=1`) is another log's offsets to the bus
        // mirror too: its cursors start over.
        "link" => {
            let reply = crate::fabric::cmd_link(store, rest, generation);
            if reply.contains(" forgot=1") {
                crate::session_log_mirror::forget_landed(store);
            }
            reply
        }
        // THE BUS MIRROR (session log phase 4), gated the same way.
        "log-take" => crate::session_log_mirror::cmd_log_take(store, rest, generation),
        "log-landed" => crate::session_log_mirror::cmd_log_landed(store, rest, generation),
        // Its lane grammar and build, gated the same way. A RECORDED hello
        // (the owning lane's, answered `OK` and not `stale=1`) says whether
        // the node mirrors its session logs (`session_log_mirror::Shared`),
        // read off the link before the store's read guard is taken, so the
        // two locks never nest here.
        "hello" => {
            let reply = crate::fabric::cmd_hello(rest, generation);
            if crate::fabric::lane_owns(generation)
                && reply.starts_with("OK ")
                && !reply.starts_with("OK stale=1")
            {
                let served = crate::fabric::bridge_mirrors_logs();
                store
                    .read()
                    .unwrap_or_else(|p| p.into_inner())
                    .mirror()
                    .set_served(served);
            }
            reply
        }
        // `outbox sent` arrives as `verb == "outbox"`; the handler splits the
        // sub-form, like `inbox get`/`inbox seen`.
        "outbox" => crate::fabric::cmd_outbox_verb(store, rest),
        _ => no_handler(verb),
    };
    if reply.starts_with("ERR usage") {
        crate::fabric::note_lane_fault(generation);
    }
    Some(reply)
}

/// The reply for a table row its access class's dispatch has no arm for: refused,
/// never a panic, so a verb added to `VERBS` before its handler fails closed
/// instead of dropping the connection thread.
fn no_handler(verb: &str) -> String {
    format!("ERR {verb} has no handler\n")
}

/// Handle requests whose authority/target class does not depend on a session.
/// Returning `None` means the normal session/edge resolver must continue.
/// `(selector, verb, rest)` is the line's [`request_head`], parsed once by
/// [`dispatch_request`].
#[allow(clippy::too_many_arguments)]
fn dispatch_before_session(
    selector: &Option<Selector>,
    verb: &str,
    rest: &str,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    sock_dir: &std::path::Path,
) -> Option<ControlReply> {
    use aterm_types::control_verbs::{Access, Target};

    let Some(spec) = aterm_types::control_verbs::spec(verb) else {
        return Some("ERR unknown verb (try: help)\n".into());
    };

    if spec.access == Access::AnyScopeMeta {
        return dispatch_meta_verb(verb, rest, selector.as_ref(), scope, proxy).map(Into::into);
    }

    // Fleet reads and connection metadata work with no terminal; session
    // authority mutations (`grant`/`revoke`/`whoami`) fall through and report
    // the typed no-terminal error.
    if spec.access == Access::OwnerOnly {
        // `hold` first: it names its session as an argument, so a fleet halt must
        // land even on a windowless instance, and `dispatch_hold_verb` audits
        // each refusal where the generic gate would refuse silently.
        if verb == "hold" {
            return Some(dispatch_hold_verb(rest, selector.as_ref(), scope, store).into());
        }
        // `topic` resolves its selector's session with no terminal, like `story`.
        if verb == "topic" {
            return match selector.as_ref() {
                Some(sel @ (Selector::Local(_) | Selector::Sid(_))) => Some(
                    dispatch_topic_verb(rest, Some(sel), scope, store, subscribers, None).into(),
                ),
                None | Some(Selector::SelfTok) => None,
            };
        }
        // `story` resolves a selector-named session here, before the self-only
        // gate; the bare form is resolved by `handle`.
        if verb == "story" {
            return match selector.as_ref() {
                Some(sel @ (Selector::Local(_) | Selector::Sid(_))) => {
                    Some(dispatch_story_verb(rest, Some(sel), scope, store, None, proxy).into())
                }
                None | Some(Selector::SelfTok) => None,
            };
        }
        // `rekey` names its tab the same way, and needs no terminal of the
        // connection's own: the tab it heals is the one the selector names.
        if verb == "rekey" {
            return Some(dispatch_rekey_verb(rest, selector.as_ref(), scope, store).into());
        }
        if let Some(refusal) = owner_only_refusal(verb, selector.as_ref(), scope) {
            return Some(refusal.into());
        }
        return match verb {
            "sessions" => Some(match rest.trim() {
                "bridge" => control_session::cmd_sessions_bridge(store),
                "status" => control_session::cmd_sessions_status(proxy),
                _ => refuse_stray_args(verb, rest)
                    .unwrap_or_else(|| control_session::cmd_sessions_store(store, Some(proxy))),
            }),
            "who" => Some(
                refuse_stray_args(verb, rest)
                    .unwrap_or_else(|| control_session::cmd_who(store, subscribers)),
            ),
            // The exit ledger is a store read like `sessions` — the instance's
            // past stays answerable with no terminal at all.
            "exits" => Some(control_session::cmd_exits(store, rest)),
            // The identity roster is a directory plus store read, so identities
            // stay listable and forgettable with no terminal.
            "identities" => Some(crate::agent_identity::cmd_identities(store, rest)),
            // `appnotice <lane> <text>`: a pull-down row from outside the process;
            // app state, answered with no terminal.
            "appnotice" => Some(control_query::cmd_appnotice(proxy, rest)),
            // `notice <post|progress|done|ask|dismiss|act> …`: the message band's write
            // face; app state, answered with no terminal.
            "notice" => Some(control_query::cmd_notice(proxy, rest)),
            // `devtools hand-over [now]`: the window's Developer Tools hand-over; app
            // state, answered with no terminal (a headless instance says so).
            "devtools" => Some(control_query::cmd_devtools(proxy, rest)),
            // `fabric status|attach`: this process's bridge supervisor, answered
            // with no terminal (a windowless instance is the one that needs it).
            "fabric" => Some(dispatch_fabric_verb(rest, selector.as_ref(), scope, store)),
            _ => None,
        }
        .map(Into::into);
    }

    // `spec` is `verb`'s row already, so ask its class here rather than let the
    // callee scan the table again for every non-bridge verb.
    if spec.access == Access::BridgeOnly
        && let Some(response) = dispatch_bridge_verb(verb, rest, selector.as_ref(), scope, store)
    {
        return Some(response.into());
    }

    let principal = if scope.is_owner_class() {
        NativeControlPrincipal::Owner
    } else {
        NativeControlPrincipal::Edge
    };

    // A bare `paste` with a native Editor in front has no ActiveSession: post the
    // same InputEvent a human paste uses and let App::input enforce the native
    // boundary. With a terminal front or an explicit selector, return None.
    if verb == "paste"
        && let Some(route) = sessionless_front_paste_event(
            verb,
            || control_input::paste_text(rest),
            selector.as_ref(),
            resolve_active(active).is_some(),
            principal,
        )
    {
        return Some(
            match route {
                Ok(event) => {
                    control_input::input_reply_to_str(post_input_reply(proxy, vec![event]))
                }
                Err(error) => error.to_string(),
            }
            .into(),
        );
    }

    // A bare `key` with a native view in front reaches that view's keyboard
    // handling through the same input seam a person's keypress takes.
    if verb == "key"
        && let Some(route) = sessionless_front_key_event(
            rest,
            selector.as_ref(),
            resolve_active(active).is_some(),
            principal,
        )
    {
        return Some(
            match route {
                Ok(event) => {
                    control_input::input_reply_to_str(post_input_reply(proxy, vec![event]))
                }
                Err(error) => error.to_string(),
            }
            .into(),
        );
    }

    // `resize px` names the window; like the paste lane: bare/self Owner only,
    // App authority, the cell form untouched.
    if verb == "resize"
        && let Some(route) = sessionless_front_window_resize(
            verb,
            rest,
            selector.as_ref(),
            resolve_active(active).is_some(),
            principal,
        )
    {
        return Some(
            match route {
                Ok(event) => {
                    control_input::input_reply_to_str(post_input_reply(proxy, vec![event]))
                }
                Err(error) => error.to_string(),
            }
            .into(),
        );
    }

    // A native tab has a framebuffer but no terminal session, so a bare Owner
    // `image` targets the front native window. `image read` and explicit
    // selectors stay on the session path.
    if verb == "image"
        && resolve_active(active).is_none()
        && scope.is_owner_class()
        && matches!(selector, None | Some(Selector::SelfTok))
        && rest.split_whitespace().next() != Some("read")
    {
        return Some(control_media::cmd_image(proxy, queue, rest, sock_dir, None));
    }

    // An aimed `spawn`/`tab`: for an Owner the selector reaches the session
    // resolver; the App authority verdict still applies (see `aimed_app_lane`).
    match aimed_app_lane(verb, selector.as_ref(), principal) {
        Some(AimedAppLane::ResolveSession) => return None,
        Some(AimedAppLane::Denied) => return Some("ERR denied\n".into()),
        None => {}
    }

    if spec.target != Target::App {
        return None;
    }
    let explicit_live = selector
        .as_ref()
        .is_none_or(|selector| selector_is_live(store, selector));
    if !explicit_live {
        return Some("ERR no such session\n".into());
    }
    match native_control_decision(
        resolve_active(active).is_some(),
        explicit_live,
        principal,
        NativeControlTarget::App,
    ) {
        NativeControlDecision::WithoutSession => {}
        NativeControlDecision::Denied => return Some("ERR denied\n".into()),
        NativeControlDecision::NoSuchSession => {
            return Some("ERR no such session\n".into());
        }
        NativeControlDecision::ResolveSession | NativeControlDecision::NoActiveTerminal => {
            return None;
        }
    }
    // Fleet halt, App lane (design §5.3). These App-target verbs resolve no
    // session, so the session dispatch's halt gate never sees their bare forms;
    // `fabric::is_pty_reaching` decides generically. The fenced set:
    //
    // * `invoke`: `invoke Paste` writes the clipboard into the front PTY
    //   (`App::paste_clipboard`), and `invoke SelectAll` + `copy` picks those
    //   bytes from the session's screen.
    // * `hwkey`: posts a real NSEvent, indistinguishable from a keypress.
    // * `pointer`: moves aterm's pointer, reported to the program under DEC
    //   1000/1002/1003.
    // * `pane`: moves keyboard focus, and with it which session flagless verbs
    //   drive.
    // * `tab`: `tab close [N]` destroys a session (`App::close_tab_via_verb`).
    // * `confirm`: `confirm yes` runs a parked close or quit (`close_confirm`).
    //
    // Checked after authorization so an unauthorized caller learns nothing.
    // `app_halt_refusal` asks whether any session is held, not just the front
    // one. Aimed forms (`@<sid> tab …`) are routed by `aimed_app_lane` and gated
    // by the second `app_halt_refusal` call in the session dispatch.
    if let Some(refusal) = crate::fabric::app_halt_refusal(store, verb) {
        return Some(refusal.into());
    }
    let active = resolve_active(active);
    // Unread-input gate (`input_stall`) for `hwkey`, `pointer` and `invoke Paste`
    // against the session they write to; a leading `unread=ok` lifts it for one
    // gesture, as on `send`/`key`. A slow read warns in the `OK` rather than
    // refusing (owner, 2026-09-29).
    let (unread_ok, rest) = control_input::take_app_unread_ok(verb, rest);
    let admitted = active
        .as_ref()
        .map_or(crate::input_stall::Admission::Open, |(_, _, _, ctx)| {
            crate::input_stall::admission(ctx, verb, rest, unread_ok)
        });
    let warning = match admitted {
        crate::input_stall::Admission::Refuse(refusal) => return Some(refusal.into()),
        crate::input_stall::Admission::Warn(warning) => Some(warning),
        crate::input_stall::Admission::Open => None,
    };
    let active_term = active.map(|(term, _, _, _)| term);
    dispatch_app_verb(verb, rest, scope, proxy, sock_dir, active_term.as_ref())
        .map(|reply| reply.warned(warning.as_deref()))
}

/// Resolve only an explicit non-self selector, independent of front content,
/// and refuse an id that names two places.
///
/// `Ok(None)` is an ordinary miss; the caller words it. `Err(refusal)` is
/// [`ambiguous_sid_refusal`]'s line: the id resolves here and a live foreign
/// instance also publishes it.
///
/// The check lives in the resolver because several seams resolve selectors
/// without going through [`handle`] (binary `post`, `feed-bin`/`paste-bin` at
/// both fence and effect, `subscribe` targets), and those write input or
/// stream output. A `Result` cannot be ignored.
fn resolve_explicit(store: &Store, selector: &Selector) -> Result<Option<Target>, String> {
    /// The registry lookup without the ambiguity gate, nested so no seam can
    /// call it directly.
    fn look_up(store: &Store, selector: &Selector) -> Option<Target> {
        match selector {
            Selector::SelfTok => None,
            Selector::Local(id) => {
                let guard = store.read().unwrap_or_else(|p| p.into_inner());
                let handle = guard.by_local(*id)?;
                Some((
                    handle.term.clone(),
                    handle.master,
                    handle.local_id,
                    handle.ctx.clone(),
                ))
            }
            Selector::Sid(id) => {
                let guard = store.read().unwrap_or_else(|p| p.into_inner());
                let handle = guard.by_sid(id)?;
                Some((
                    handle.term.clone(),
                    handle.master,
                    handle.local_id,
                    handle.ctx.clone(),
                ))
            }
        }
    }

    let found = look_up(store, selector);
    // A miss that names the warm spare taints it (`crate::spare`); a hit on a
    // session before its start record is counted against its rc generation.
    match (&found, selector) {
        (None, Selector::Sid(id)) => crate::spare::control_refusal(Some(id), None),
        (None, Selector::Local(id)) => crate::spare::control_refusal(None, Some(*id)),
        (Some((_, _, _, ctx)), _) => crate::spare::note_control(&ctx.witness),
        (None, Selector::SelfTok) => {}
    }
    // Only a hosted id can be ambiguous, so probe only on a hit (a miss costs
    // nothing). `live_holder` reads `graph/<sid>` for a live foreign pid and is
    // gated on ids this process adopted; minted ids are answered from a
    // register.
    if found.is_some()
        && let Some(refusal) = ambiguous_sid_refusal(selector)
    {
        return Err(refusal);
    }
    Ok(found)
}

/// A session-class request's route: the front terminal and the classifier's
/// verdict for its selector, assembled in one place for the dispatcher and
/// `native_control_conformance`.
struct SessionRoute {
    active_target: Option<Target>,
    decision: NativeControlDecision,
}

fn session_route(
    selector: &Option<Selector>,
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
) -> SessionRoute {
    let active_target = resolve_active(active);
    let explicit = matches!(selector, Some(Selector::Local(_) | Selector::Sid(_)));
    let explicit_live = selector
        .as_ref()
        .is_some_and(|selector| selector_is_live(store, selector));
    let principal = if scope.is_owner_class() {
        NativeControlPrincipal::Owner
    } else {
        NativeControlPrincipal::Edge
    };
    let decision = native_control_decision(
        active_target.is_some(),
        explicit_live,
        principal,
        if explicit {
            NativeControlTarget::ExplicitSession
        } else {
            NativeControlTarget::BareSession
        },
    );
    SessionRoute {
        active_target,
        decision,
    }
}

/// The dispatcher's session-class verdict for `line`, for Tier-1 conformance.
#[cfg(test)]
pub(crate) fn session_route_decision(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
) -> NativeControlDecision {
    session_route(&request_head(line).0, active, store, scope).decision
}

/// Full request dispatch. Classification precedes terminal resolution, so
/// native-only windows stay controllable without a fabricated ActiveSession.
#[allow(clippy::too_many_arguments)]
fn dispatch_request(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    sock_dir: &std::path::Path,
) -> ControlReply {
    // Parsed ONCE; every stage below reads this same head.
    let (selector, verb, rest) = request_head(line);
    if let Some(response) = dispatch_before_session(
        &selector,
        verb,
        rest,
        active,
        store,
        subscribers,
        scope,
        proxy,
        queue,
        sock_dir,
    ) {
        return response;
    }

    let SessionRoute {
        active_target,
        decision,
    } = session_route(&selector, active, store, scope);
    match decision {
        NativeControlDecision::ResolveSession => {}
        NativeControlDecision::Denied => return "ERR denied\n".into(),
        NativeControlDecision::NoActiveTerminal => return NO_ACTIVE_TERMINAL.into(),
        NativeControlDecision::NoSuchSession => {
            return closed_session_reply(selector.as_ref(), verb, rest, store, scope).into();
        }
        NativeControlDecision::WithoutSession => {
            return "ERR invalid control target\n".into();
        }
    }
    // Capture the front tab's id before `active_target` is consumed: an explicit
    // `@<sid>` naming it goes through the App input seam (`front_routed_input`).
    let front_active_session = active_target.as_ref().map(|(_, _, session, _)| *session);
    let target = match selector.as_ref() {
        Some(selector @ (Selector::Local(_) | Selector::Sid(_))) => {
            match resolve_explicit(store, selector) {
                Ok(target) => target,
                // Two instances answer to this id: refuse, do not pick one.
                Err(refusal) => return refusal.into(),
            }
        }
        None | Some(Selector::SelfTok) => active_target,
    };
    let Some((term, master, session, ctx)) = target else {
        return if matches!(selector, Some(Selector::Local(_) | Selector::Sid(_))) {
            // It closed between the route and the resolve.
            closed_session_reply(selector.as_ref(), verb, rest, store, scope)
        } else {
            NO_ACTIVE_TERMINAL.to_string()
        }
        .into();
    };
    let mut tail = ReplyTail::default();
    let body = handle(
        &selector,
        verb,
        rest,
        &term,
        master,
        session,
        &ctx,
        store,
        scope,
        proxy,
        queue,
        sock_dir,
        subscribers,
        front_active_session,
        &mut tail,
    );
    ControlReply::with_tail(body, tail)
}

/// `log` and `brief` naming a session that has closed in this process: the
/// closed-session index answers (`session_log::ClosedLogs`), under the rule a
/// live read takes. Owner-class reads; an edge reads iff `decide_edge` permits
/// its `ReadScreen` against the session's shared authority table as it stands
/// now (a revoke since the close counts), under its launch nonce. A read the
/// index does not answer falls to the disk journal
/// ([`journal_session_reply_with`]), which answers Owner-class reads only, so
/// a revoked edge gets nothing there either. Everything else (another verb, a
/// session neither holds, an edge it did not grant) answers `ERR no such
/// session`, word for word what a closed session answered before, so an edge
/// learns nothing of a session it could not read. An id a live foreign
/// instance also serves is refused as a live one is
/// ([`ambiguous_sid_refusal`]), and `brief all` stays Owner-only
/// ([`escalated_op`]).
fn closed_session_reply(
    selector: Option<&Selector>,
    verb: &str,
    rest: &str,
    store: &Store,
    scope: Scope,
) -> String {
    closed_session_reply_with(
        crate::session_log_journal::read::reader(),
        selector,
        verb,
        rest,
        store,
        scope,
    )
}

/// [`closed_session_reply`] whose index misses fall to `reader` (a test's
/// own journal; production passes this process's, `None` while it keeps
/// none).
fn closed_session_reply_with(
    reader: Option<&crate::session_log_journal::read::Reader>,
    selector: Option<&Selector>,
    verb: &str,
    rest: &str,
    store: &Store,
    scope: Scope,
) -> String {
    const NO_SUCH: &str = "ERR no such session\n";
    if !matches!(verb, "log" | "brief") {
        return NO_SUCH.to_string();
    }
    // Taken out of the index under the store's read lock (`Arc` clones);
    // its grants are locked, and its log read back, after it.
    let found = {
        let guard = store.read().unwrap_or_else(|p| p.into_inner());
        let index = guard.closed_logs();
        match selector {
            Some(Selector::Local(id)) => index.by_local(*id),
            Some(Selector::Sid(sid)) => index.by_sid(sid),
            None | Some(Selector::SelfTok) => None,
        }
        .map(crate::session_log::ClosedLog::read)
    };
    let found = found.filter(|read| match scope {
        Scope::Edge(presented) => read.edge_may_read(&presented),
        _ => scope.is_owner_class(),
    });
    // Read back (a summary's text into its log) after the store's lock.
    let found = found.and_then(|read| {
        let sid = read.sid().as_str().to_string();
        read.source().into_log().map(|log| (sid, log))
    });
    let (Some(selector), Some((sid, log))) = (selector, found) else {
        return journal_session_reply_with(reader, selector, verb, rest, store, scope)
            .unwrap_or_else(|| NO_SUCH.to_string());
    };
    if let Some(refusal) = ambiguous_sid_refusal(selector) {
        return refusal;
    }
    if escalated_op(verb, rest) == Some(Escalation::OwnerOnly) && !scope.is_owner_class() {
        log_denial(
            AUDIT_SUBSYSTEM,
            &format!("closed {verb} -> {sid}"),
            aterm_containment::mode_or_containment(),
            "owner-only form of a closed session's read",
        );
        return "ERR denied\n".to_string();
    }
    match verb {
        "log" => control_session::cmd_log_closed(&log, rest),
        _ => control_session::cmd_brief_closed(&log, &sid, store, rest),
    }
}

/// `log @<sid>` and `brief @<sid>` of a session from an EARLIER process
/// (phase 2B), answered from the disk journal on this control worker
/// (`session_log_journal::read`): Owner-class reads of a sid only, and only
/// while this launch keeps a journal (off by default: nothing is read and
/// nothing created). `None` is the caller's `ERR no such session`, the same
/// bytes for an edge, a local id, a journal that is off, and a sid it does
/// not hold. An id a live foreign instance serves is refused as a live one
/// is, and `brief all` stays Owner-only. `reader` is this process's journal
/// (`session_log_journal::read::reader`) or a test's own.
fn journal_session_reply_with(
    reader: Option<&crate::session_log_journal::read::Reader>,
    selector: Option<&Selector>,
    verb: &str,
    rest: &str,
    store: &Store,
    scope: Scope,
) -> Option<String> {
    let Some(sel @ Selector::Sid(sid)) = selector else {
        return None;
    };
    if !scope.is_owner_class() {
        return None;
    }
    let reader = reader?;
    if let Some(refusal) = ambiguous_sid_refusal(sel) {
        return Some(refusal);
    }
    let short = aterm_slog::ShortId::new(sid.as_str())?;
    use crate::session_log_journal::read::{self as journal_read, JournalRead};
    let view = match reader.view(&short) {
        JournalRead::View(view) => view,
        JournalRead::Absent => return None,
        // The read could not reach the whole journal and found no line of
        // the session: said as such, never as a session that does not exist.
        JournalRead::Cut => return Some(journal_read::JOURNAL_CUT.to_string()),
    };
    Some(match verb {
        "log" => journal_read::cmd_log_journal(&view, rest),
        _ => {
            // A closed session's brief, its cursor placed and handed out as
            // `log`'s is.
            let mut query = match crate::session_log::BriefQuery::parse(rest) {
                Ok(q) => q,
                Err(usage) => return Some(usage.to_string()),
            };
            query.cursor = query.cursor.map(|c| journal_read::journal_cursor(&view, c));
            control_session::cmd_brief_closed_query(
                &view.log,
                sid.as_str(),
                store,
                &query,
                Some(view.reply_cursor()),
            )
        }
    })
}

/// How serving a request can end a connection's time on its lane. A connection
/// authenticates on its first line ([`authenticate_connection`]; the uid was
/// checked in [`spawn`]), then serves one response per request line until the
/// client disconnects or a write fails (`control_lanes.rs`).
///
/// PUSH-ONLY (P1.3): a `subscribe` verb FLIPS the connection to server-push by
/// handing it to the reserved subscription pool. [`run_subscribe_socket`] there
/// never reads another request line — the client thereafter only reads
/// `DELTA`/`EVENT`/`GAP` frames — while the ordinary lane returns at once.
enum ServeDisposition {
    Close,
    Subscribe { line: String, scope: Scope },
}

struct PendingArtifactAck {
    retention: ReplyRetention,
    nonce: String,
    deadline: std::time::Instant,
}

/// Time a guarded response gets to finish its client handoff. A valid nonce ACK
/// releases the guards at once; a timeout, early close, legacy client or wire
/// failure moves them to quarantine for this extra grace. Publication
/// capabilities stay live; closed frame identities are revalidated at the wire
/// but do not block a later same-name replacement.
const ARTIFACT_HANDOFF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const ARTIFACT_FAILURE_QUARANTINE: std::time::Duration = std::time::Duration::from_secs(30);
const MIN_ARTIFACT_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1);

struct QuarantinedArtifactReply {
    until: std::time::Instant,
    _retention: ReplyRetention,
    /// Which quarantine this is, so a test ends ITS OWN entry's grace
    /// (`control_tests::end_quarantine`) and leaves every other test's alone:
    /// the quarantine is process-global, and the lib tests share the process.
    #[cfg(test)]
    ticket: u64,
}

#[cfg(test)]
thread_local! {
    /// The ticket of the guard the calling thread put in quarantine last
    /// ([`QuarantinedArtifactReply::ticket`]): the thread that ran the failed
    /// handoff is the one that knows which entry is its own.
    static LAST_QUARANTINE_TICKET: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// A fresh quarantine ticket, noted as this thread's last
/// ([`LAST_QUARANTINE_TICKET`]).
#[cfg(test)]
fn next_quarantine_ticket() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let ticket = NEXT.fetch_add(1, Ordering::Relaxed);
    LAST_QUARANTINE_TICKET.with(|last| last.set(Some(ticket)));
    ticket
}

struct ArtifactReplyQuarantine {
    entries: std::sync::Mutex<Vec<QuarantinedArtifactReply>>,
    changed: std::sync::Condvar,
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReplyPublication",
        action = "AdvanceQuarantine",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn advance_artifact_quarantine_anchor() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReplyPublication",
        action = "ExpireQuarantine",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn expire_artifact_quarantine_anchor() {}

/// The process-global quarantine: a `&'static` to a leaked value rather than the
/// value itself, so the reaper thread spawned by the initializer can be HANDED
/// the quarantine instead of asking this cell for it. The lazy-init
/// reentrancy census (`aterm-census --lazy-init`, OB-19) refuses the shape
/// whose reaper calls `artifact_reply_quarantine()` first thing: a blocking
/// `get_or_init` on the very cell whose initializer had just spawned it. Nothing
/// inside the initializer touches the cell.
static ARTIFACT_REPLY_QUARANTINE: std::sync::OnceLock<&'static ArtifactReplyQuarantine> =
    std::sync::OnceLock::new();

impl ArtifactReplyQuarantine {
    fn leak() -> &'static Self {
        Box::leak(Box::new(Self {
            entries: std::sync::Mutex::new(Vec::new()),
            changed: std::sync::Condvar::new(),
        }))
    }

    /// Remove every entry whose quarantine has run out by `now`, firing the
    /// model's expiry anchors; one whose deadline is still ahead stays, so no
    /// caller can release a guard early (the reaper's one sweep). The caller
    /// drops what this returns once the queue mutex is released: filesystem
    /// publication guards are never released under it.
    fn take_expired(
        entries: &mut Vec<QuarantinedArtifactReply>,
        now: std::time::Instant,
    ) -> Vec<QuarantinedArtifactReply> {
        let mut expired = Vec::new();
        let mut index = 0;
        while index < entries.len() {
            if entries[index].until <= now {
                advance_artifact_quarantine_anchor();
                expire_artifact_quarantine_anchor();
                expired.push(entries.swap_remove(index));
            } else {
                index += 1;
            }
        }
        expired
    }
}

/// The quarantine reaper's scheduling role: `Background`, not `Housekeeping`
/// (2026-09-30). A quarantined reply holds a production handoff permit and its
/// capture lease, so later captures are refused, or wait out
/// `CAPTURE_NAMESPACE_WAIT` and fail busy, until the reaper drops it. That is
/// cleanup something else waits on, which `qos::Role::Housekeeping` rules out:
/// at QOS_CLASS_BACKGROUND the reaper did not run within 60 s in 8 of 20 runs
/// at load 71-115 on 18 cores.
const ARTIFACT_QUARANTINE_REAPER_ROLE: crate::qos::Role = crate::qos::Role::Background;

fn artifact_reply_quarantine() -> &'static ArtifactReplyQuarantine {
    ARTIFACT_REPLY_QUARANTINE.get_or_init(|| {
        let quarantine = ArtifactReplyQuarantine::leak();
        // One reaper handles every failed handoff. If the OS refuses the single
        // helper thread, queued guards intentionally remain retained forever:
        // fail-closed availability loss is safer than releasing an advertised
        // path before its bounded compatibility handoff.
        let _ = crate::qos::spawn_lane_thread(
            "aterm-artifact-quarantine",
            ARTIFACT_QUARANTINE_REAPER_ROLE,
            move || {
                artifact_reply_quarantine_reaper(quarantine);
            },
        );
        quarantine
    })
}

fn artifact_reply_quarantine_reaper(quarantine: &'static ArtifactReplyQuarantine) {
    loop {
        let mut entries = quarantine
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while entries.is_empty() {
            entries = quarantine
                .changed
                .wait(entries)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let now = std::time::Instant::now();
        let next = entries
            .iter()
            .map(|entry| entry.until)
            .min()
            .expect("non-empty quarantine has a deadline");
        if next > now {
            let (next_entries, _) = quarantine
                .changed
                .wait_timeout(entries, next.saturating_duration_since(now))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(next_entries);
            continue;
        }
        let expired = ArtifactReplyQuarantine::take_expired(&mut entries, now);
        drop(entries);
        drop(expired);
    }
}

/// The reaper's sweep, run on the CALLER's thread: a test that waits for an
/// expired guard to go drives the shipping sweep itself instead of waiting for
/// the reaper to be scheduled, which a loaded Mac can starve past any hang
/// detector (2026-09-30: three tests, 60 s each, at load 100).
#[cfg(test)]
fn sweep_artifact_quarantine_for_test() {
    let quarantine = artifact_reply_quarantine();
    let expired = {
        let mut entries = quarantine
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ArtifactReplyQuarantine::take_expired(&mut entries, std::time::Instant::now())
    };
    drop(expired);
}

fn quarantine_artifact_reply_until(retention: ReplyRetention, until: std::time::Instant) {
    let quarantine = artifact_reply_quarantine();
    let mut entries = quarantine
        .entries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    entries.push(QuarantinedArtifactReply {
        until,
        _retention: retention,
        #[cfg(test)]
        ticket: next_quarantine_ticket(),
    });
    quarantine.changed.notify_one();
}

#[cfg(test)]
fn write_control_reply(
    writer: &mut impl Write,
    reply: ControlReply,
) -> std::io::Result<Option<PendingArtifactAck>> {
    write_control_reply_with_timeout_arm(writer, reply, ARTIFACT_HANDOFF_TIMEOUT, |_| Ok(()))
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReplyPublication",
        action = "WriteWire",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn artifact_reply_write_wire_anchor() {}

/// Write `reply` where the connection's answers go: `writer`, a lane's outbox
/// (delivered as the peer takes it, never waited for on the lane) or the
/// bridge's socket. A GUARDED reply goes straight to `stream` instead, under its
/// handoff deadline, its ERR fallback included: it must be on the wire before
/// its ACK is awaited, and a lane's outbox holds nothing then (each answer is
/// delivered before the next request is read).
fn write_control_reply_to_stream(
    stream: &CtlStream,
    writer: &mut impl Write,
    reply: ControlReply,
) -> std::io::Result<Option<PendingArtifactAck>> {
    if reply.retention.is_none() {
        return write_control_reply_with_timeout_arm(
            writer,
            reply,
            ARTIFACT_HANDOFF_TIMEOUT,
            |_| Ok(()),
        );
    }
    let mut direct = stream;
    write_control_reply_with_timeout_arm(&mut direct, reply, ARTIFACT_HANDOFF_TIMEOUT, |timeout| {
        stream.set_write_timeout(timeout)
    })
}

fn write_control_reply_with_timeout_arm(
    writer: &mut impl Write,
    mut reply: ControlReply,
    handoff_timeout: std::time::Duration,
    mut set_write_timeout: impl FnMut(Option<std::time::Duration>) -> std::io::Result<()>,
) -> std::io::Result<Option<PendingArtifactAck>> {
    let handoff_deadline = reply
        .retention
        .is_some()
        .then(|| std::time::Instant::now() + handoff_timeout);
    let requires_ack = matches!(
        reply.retention.as_ref(),
        Some(ReplyHandoff::Acknowledged(_))
    );
    // Mint the challenge before fallible wire preparation: video preparation can
    // publish its durable marker, so entropy failure must stay an abortable ERR.
    let mut nonce = if requires_ack {
        match aterm_uds::rand::hex_token::<16>() {
            Ok(nonce) => Some(nonce),
            // The ERR keeps the handoff deadline: written straight to the
            // socket, it must not wait on its peer without one.
            Err(error) => {
                reply.body = format!("ERR artifact acknowledgement setup failed: {error}\n");
                reply.retention = None;
                None
            }
        }
    } else {
        None
    };
    if let Some(retention) = reply.retention.as_mut()
        && let Err(error) = retention.prepare_write()
    {
        // The OK's identity can no longer be certified: abort the publication
        // before writing a path-free ERR.
        retention.prepare_failed_anchor();
        reply.body = format!("ERR artifact reply validation failed: {error}\n");
        reply.retention = None;
        nonce = None;
    }
    // `reply` remains in scope through the complete write + flush; guarded
    // retention is then returned to the explicit ACK waiter.
    let mut write_timeout_armed = false;
    let write = if let Some(deadline) = handoff_deadline {
        let mut arm_write_timeout = |remaining| {
            set_write_timeout(Some(remaining))?;
            write_timeout_armed = true;
            Ok(())
        };
        (|| {
            write_all_until(
                writer,
                reply.body.as_bytes(),
                deadline,
                &mut arm_write_timeout,
            )?;
            if let Some(nonce) = nonce.as_deref() {
                write_all_until(
                    writer,
                    aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX.as_bytes(),
                    deadline,
                    &mut arm_write_timeout,
                )?;
                write_all_until(writer, nonce.as_bytes(), deadline, &mut arm_write_timeout)?;
                write_all_until(writer, b"\n", deadline, &mut arm_write_timeout)?;
            }
            flush_until(writer, deadline, &mut arm_write_timeout)
        })()
    } else {
        (|| {
            writer.write_all(reply.body.as_bytes())?;
            writer.flush()
        })()
    };
    // Free the (possibly large) inline body before releasing or quarantining any
    // handoff, so new requests never overlap unaccounted reply memory.
    drop(std::mem::take(&mut reply.body));
    // The reply is on the wire (or never will be): release waiters such as a
    // seamless update's Commit ([`ReplyWire`]), before the longer ACK wait.
    drop(reply.written.take());
    if let Err(error) = write {
        match reply.retention.take() {
            Some(ReplyHandoff::Acknowledged(mut retention)) => {
                retention.write_failed_anchor();
                quarantine_artifact_reply_until(
                    retention,
                    std::time::Instant::now() + ARTIFACT_FAILURE_QUARANTINE,
                );
            }
            Some(ReplyHandoff::WriteOnly(retention)) => drop(retention),
            None => {}
        }
        return Err(error);
    }
    let pending = match (reply.retention.take(), nonce) {
        (Some(ReplyHandoff::Acknowledged(retention)), Some(nonce)) => {
            // Guarded body and challenge are written and flushed; plain and
            // write-only replies skip the acknowledgement state machine.
            artifact_reply_write_wire_anchor();
            Some(PendingArtifactAck {
                retention,
                nonce,
                deadline: handoff_deadline.expect("guarded reply owns its handoff deadline"),
            })
        }
        (Some(ReplyHandoff::WriteOnly(retention)), None) => {
            drop(retention);
            None
        }
        (None, None) => None,
        _ => unreachable!("reply handoff and acknowledgement nonce stay paired"),
    };
    if pending.is_none() && write_timeout_armed {
        // Write-only replies keep the connection; clear their handoff deadline
        // for later responses.
        set_write_timeout(None)?;
    }
    Ok(pending)
}

fn remaining_artifact_io_budget(
    deadline: std::time::Instant,
) -> std::io::Result<std::time::Duration> {
    let now = std::time::Instant::now();
    if now >= deadline {
        return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "artifact handoff deadline expired",
        ));
    }
    Ok(deadline
        .saturating_duration_since(now)
        .max(MIN_ARTIFACT_IO_TIMEOUT))
}

fn write_all_until(
    writer: &mut impl Write,
    mut bytes: &[u8],
    deadline: std::time::Instant,
    arm_write_timeout: &mut impl FnMut(std::time::Duration) -> std::io::Result<()>,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        arm_write_timeout(remaining_artifact_io_budget(deadline)?)?;
        match writer.write(bytes) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "failed to write guarded artifact reply",
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn flush_until(
    writer: &mut impl Write,
    deadline: std::time::Instant,
    arm_write_timeout: &mut impl FnMut(std::time::Duration) -> std::io::Result<()>,
) -> std::io::Result<()> {
    loop {
        arm_write_timeout(remaining_artifact_io_budget(deadline)?)?;
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
    }
}

/// Guarded artifact replies are one-shot per connection. After the full
/// response frame the server appends an unpredictable `ACK-CHALLENGE <nonce>`;
/// the client echoes `ACK <nonce>` only after consuming both, so a
/// pre-pipelined ACK cannot guess it.
///
/// Any other line, EOF, timeout or I/O failure is a failed ACK moved to
/// quarantine, never a success or another verb. Request-half EOF is not an ACK
/// (a client may half-close while still reading). Local/TLS relays carry the
/// ACK as ordinary traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactAckOutcome {
    PeerAcknowledged,
    AcknowledgementQuarantined,
}

fn await_guarded_reply_close(
    stream: &CtlStream,
    reader: &mut impl BufRead,
    pending: PendingArtifactAck,
) -> ArtifactAckOutcome {
    await_guarded_reply_close_with_quarantine(stream, reader, pending, ARTIFACT_FAILURE_QUARANTINE)
}

fn await_guarded_reply_close_with_quarantine(
    stream: &CtlStream,
    reader: &mut impl BufRead,
    pending: PendingArtifactAck,
    failure_quarantine: std::time::Duration,
) -> ArtifactAckOutcome {
    let PendingArtifactAck {
        mut retention,
        nonce,
        deadline,
    } = pending;
    let expected = format!(
        "{}{nonce}",
        aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX
    );
    // EXACTLY one line decides the outcome — the challenge is one-shot, so a
    // line that is not the ACK is a failure and never a re-read: reading again
    // would let a peer keep guessing the nonce inside one deadline. The read
    // absolute deadline is recomputed before every byte read, and
    // `read_request_line_until` folds expiry, EOF, and I/O failure into `None`.
    match read_request_line_until(stream, reader, deadline) {
        Some(line) if line == expected => {
            retention.acknowledge_peer_anchor();
            let _ = stream.shutdown(std::net::Shutdown::Write);
            drop(retention);
            ArtifactAckOutcome::PeerAcknowledged
        }
        Some(_) | None => {
            retention.acknowledge_failed_anchor();
            quarantine_artifact_reply_until(
                retention,
                std::time::Instant::now() + failure_quarantine,
            );
            ArtifactAckOutcome::AcknowledgementQuarantined
        }
    }
}

thread_local! {
    /// The connection this worker thread is serving, to which connection-scoped
    /// claims (`meta set supervisor <holder>` without `ttl=`) bind. Set while a
    /// lane carries it (`control_lanes.rs`'s `Serving`) and during the bridge's
    /// [`serve_preresolved`]; a connection keeps its serial across lanes.
    static SERVING_CONNECTION: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// Whether the connection being served made a connection-scoped claim, so
    /// only such a connection pays for the release sweep when it ends.
    static CONNECTION_CLAIMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the verb this thread serves is finishing input it has begun
    /// ([`CommittedInput`]), so its main-thread hops are not refused for a
    /// hangup.
    static INPUT_COMMITTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the served connection (`aterm_control::hangup::serving_fd`) is a
    /// DIRECT local connection — one a pooled
    /// lane accepted on the instance's own listener ([`DirectPeer`]) — whose
    /// kernel peer pid may prove a self-claim ([`serving_peer_pid`]). Never
    /// the bridge's inherited `socketpair` ([`serve_preresolved`]).
    #[cfg(unix)]
    static SERVING_DIRECT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The `if-supervisor=<holder>` fence of the write this thread is making
    /// ([`CarriedFence`]): a main-thread input hop posted under it carries it
    /// ([`post_input_reply_to`]), and the main thread reads the fence again
    /// as it applies the batch.
    static CARRIED_FENCE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// Who presented the token of the connection this thread serves
    /// ([`Principal`]): set by the pooled lanes beside [`ServingFd`] for the
    /// connection's whole tenure, `Instance` everywhere else (the bridge's
    /// connection, an in-process call).
    static SERVING_PRINCIPAL: std::cell::Cell<Principal> =
        const { std::cell::Cell::new(Principal::Instance) };
}

/// The [`Principal`] of the connection this thread serves (`Instance` off a
/// lane): what the session log's `by=` names an Owner caller
/// ([`control_session::log_actor`]).
pub(crate) fn serving_principal() -> Principal {
    SERVING_PRINCIPAL.with(std::cell::Cell::get)
}

/// Marks the connection this thread serves as presented by `principal`
/// until dropped ([`SERVING_PRINCIPAL`]). Nests: a drop restores what it
/// found.
pub(crate) struct ServingPrincipal(Principal);

impl ServingPrincipal {
    pub(crate) fn enter(principal: Principal) -> Self {
        Self(SERVING_PRINCIPAL.with(|p| p.replace(principal)))
    }
}

impl Drop for ServingPrincipal {
    fn drop(&mut self) {
        SERVING_PRINCIPAL.with(|p| p.set(self.0));
    }
}

/// The fence a write on this thread is made under, until dropped
/// ([`CARRIED_FENCE`]). Nests: a drop restores what it found.
struct CarriedFence {
    outer: Option<String>,
}

impl CarriedFence {
    fn enter(holder: &str) -> Self {
        Self {
            outer: CARRIED_FENCE.with(|fence| fence.replace(Some(holder.to_string()))),
        }
    }
}

impl Drop for CarriedFence {
    fn drop(&mut self) {
        let outer = self.outer.take();
        CARRIED_FENCE.with(|fence| *fence.borrow_mut() = outer);
    }
}

/// The fence the write on this thread is made under, if any.
fn carried_fence() -> Option<String> {
    CARRIED_FENCE.with(|fence| fence.borrow().clone())
}

/// One write made under the `if-supervisor=<holder>` fence (the self-claim
/// contract, §2): counted as in flight from the fence's read to the write's
/// end ([`crate::session_timeline::FencedWrite`]), so a replacement waits
/// for it, and carried onto any main-thread input hop it posts. With no
/// fence the write runs as it is. `Err` is the fence's refusal; nothing was
/// written.
fn under_fence<T>(
    ctx: &SessionCtx,
    holder: Option<&str>,
    write: impl FnOnce() -> T,
) -> Result<T, String> {
    let Some(holder) = holder else {
        return Ok(write());
    };
    let _counted =
        crate::session_timeline::FencedWrite::enter(ctx, holder, crate::metrics::now_us())?;
    let _carried = CarriedFence::enter(holder);
    Ok(write())
}

/// Marks the connection this lane serves as a DIRECT local one until dropped
/// ([`SERVING_DIRECT`]): set by the pooled lanes beside [`ServingFd`].
#[cfg(unix)]
pub(crate) struct DirectPeer(bool);

#[cfg(unix)]
impl DirectPeer {
    pub(crate) fn enter() -> Self {
        Self(SERVING_DIRECT.with(|direct| direct.replace(true)))
    }
}

#[cfg(unix)]
impl Drop for DirectPeer {
    fn drop(&mut self) {
        SERVING_DIRECT.with(|direct| direct.set(self.0));
    }
}

/// A set's PROOF ([`crate::self_claim`]): the kernel's peer pid of the
/// direct connection this thread serves ([`serving_peer_pid`]), the live
/// foreground group of the session's PTY `master`, and the named process's
/// image ([`crate::self_claim::runs_alab_build`]), read only for a holder a
/// self-claim can name (`codex-alab@<pid>`).
pub(crate) fn prove_self_claim(holder: &str, master: i32) -> Option<crate::self_claim::Proven> {
    prove_self_claim_by(holder, master, &crate::self_claim::runs_alab_build)
}

/// [`prove_self_claim`] with the image fact read by `alab_image`, which is
/// asked only once the peer and the foreground facts hold.
pub(crate) fn prove_self_claim_by(
    holder: &str,
    master: i32,
    alab_image: &dyn Fn(u32) -> bool,
) -> Option<crate::self_claim::Proven> {
    let pid = crate::self_claim::holder_pid(holder)?;
    let peer = serving_peer_pid();
    let foreground = crate::quit_safety::foreground_pgrp(master);
    let named = peer == Some(pid) && i64::from(foreground) == i64::from(pid);
    crate::self_claim::prove(
        holder,
        crate::self_claim::Facts {
            peer,
            foreground,
            alab_image: named && alab_image(pid),
        },
    )
}

/// THE KERNEL'S PEER PID of the direct local connection this thread serves,
/// read now — right after its request line was read ([`aterm_uds::fdpass::
/// peer_pid`]; Darwin's `LOCAL_PEERPID` names the connection's last writer).
/// `None` off a direct connection (the bridge, no lane) or where it cannot be
/// read: no proof ([`crate::self_claim`]).
pub(crate) fn serving_peer_pid() -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd as _;
        if !SERVING_DIRECT.with(std::cell::Cell::get) {
            return None;
        }
        let fd = aterm_control::hangup::serving_fd()?;
        // SAFETY: `fd` is the connection this lane serves, open for as long
        // as its `ServingFd` guard stands (the whole serve); the borrowed
        // stream is never dropped, so it never closes the descriptor.
        let stream = std::mem::ManuallyDrop::new(unsafe { CtlStream::from_raw_fd(fd) });
        aterm_uds::fdpass::peer_pid(&stream).ok()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Marks the verb on this thread as COMMITTED to input it has begun, until
/// dropped: a `turn` from its first typed byte to its verified submit. A
/// main-thread hop it makes then is not refused because its caller hung up
/// ([`crate::control_media::main_thread_refusal`]), for the same reason its
/// parks do not end on one (`OnHangup::Finish` in `control_session.rs`):
/// abandoning the rest would leave the text typed so far sitting unsubmitted
/// in the target's composer, where the next turn's text is appended to it.
/// Every other refusal (a stalled main thread, a standing dialog) still
/// applies. Nests: a drop restores what it found.
pub(crate) struct CommittedInput {
    outer: bool,
}

impl CommittedInput {
    pub(crate) fn enter() -> Self {
        Self {
            outer: INPUT_COMMITTED.with(|committed| committed.replace(true)),
        }
    }
}

impl Drop for CommittedInput {
    fn drop(&mut self) {
        INPUT_COMMITTED.with(|committed| committed.set(self.outer));
    }
}

/// Whether the verb on this thread is inside a [`CommittedInput`].
pub(crate) fn input_committed() -> bool {
    INPUT_COMMITTED.with(std::cell::Cell::get)
}

/// Whether the caller is still there (`aterm_control::hangup`): `await`,
/// `ready`, `wait`, `turn`, `await inbox` and `post --wait` check
/// [`caller_hung_up`] on every wake and park at most [`hangup_park`] between
/// checks, so a killed client frees its lane within [`HANGUP_POLL`]; the
/// fabric endpoint's waits read the same mark.
pub(crate) use aterm_control::hangup::{HANGUP_POLL, HUNG_UP_REPLY, caller_hung_up, hangup_park};

/// Marks this thread as serving one connection for [`caller_hung_up`] until
/// dropped (set during `control_lanes.rs`'s `serve_here` and the bridge's
/// [`serve_preresolved`]); the stream must outlive the guard.
struct ServingFd {
    _mark: aterm_control::hangup::ServingFd,
}

impl ServingFd {
    fn enter(stream: &CtlStream) -> Self {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            Self {
                _mark: aterm_control::hangup::ServingFd::enter(stream.as_raw_fd()),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = stream;
            Self {
                _mark: aterm_control::hangup::ServingFd::enter(),
            }
        }
    }
}

/// Per-process connection serial: every served connection gets a fresh one, so
/// a claim can never be released by a later connection that reused its lane.
static NEXT_CONNECTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The connection this thread is serving, if any (see [`SERVING_CONNECTION`]).
pub(crate) fn serving_connection() -> Option<u64> {
    SERVING_CONNECTION.with(std::cell::Cell::get)
}

/// Record that the serving connection bound a claim to itself.
fn note_connection_claim() {
    CONNECTION_CLAIMED.with(|claimed| claimed.set(true));
}

/// Drop guard for a connection served on its own thread (the bridge): when it
/// stops serving (closed, peer gone, unwound, or turned push subscription),
/// every claim bound to it is released, so a dead supervisor never shows as
/// running. Pooled lanes do the same on the connection (`control_lanes.rs`'s
/// `ConnTenure`).
#[cfg(unix)]
struct ConnectionScope<'a> {
    id: u64,
    _serving: ServingFd,
    store: &'a Store,
    subscribers: &'a Subscribers,
    proxy: &'a EventLoopProxy<Wake>,
}

#[cfg(unix)]
impl<'a> ConnectionScope<'a> {
    fn enter(
        store: &'a Store,
        subscribers: &'a Subscribers,
        proxy: &'a EventLoopProxy<Wake>,
        stream: &CtlStream,
    ) -> Self {
        let id = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
        SERVING_CONNECTION.with(|serving| serving.set(Some(id)));
        CONNECTION_CLAIMED.with(|claimed| claimed.set(false));
        Self {
            id,
            _serving: ServingFd::enter(stream),
            store,
            subscribers,
            proxy,
        }
    }
}

#[cfg(unix)]
impl Drop for ConnectionScope<'_> {
    fn drop(&mut self) {
        SERVING_CONNECTION.with(|serving| serving.set(None));
        if !CONNECTION_CLAIMED.with(|claimed| claimed.replace(false)) {
            return;
        }
        release_connection_claims(self.store, self.subscribers, Some(self.proxy), self.id);
    }
}

/// Release every supervisor claim bound to connection `conn`, with `meta
/// unset`'s fan-out (tab chrome wake, `events` notify). The registry snapshot is
/// cloned first so no meta lock is taken under the registry lock.
fn release_connection_claims(
    store: &Store,
    subscribers: &Subscribers,
    proxy: Option<&EventLoopProxy<Wake>>,
    conn: u64,
) {
    let handles = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
    let now = crate::metrics::now_us();
    for handle in handles {
        if !crate::session_timeline::release_supervisor(&handle.ctx, Some(conn), now) {
            continue;
        }
        if let Some(proxy) = proxy {
            let _ = proxy.send_event(Wake::MetaChanged {
                session: handle.local_id,
            });
        }
        if subscribers.any() {
            subscribers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .notify(handle.local_id);
        }
    }
}

/// Serve an inherited `socketpair` end on its own thread. No handshake is read:
/// the fd is the authority, and a spoken credential is exactly what this design
/// avoids.
#[cfg(unix)]
#[allow(clippy::too_many_arguments)]
fn serve_preresolved(
    stream: &CtlStream,
    scope: Scope,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    sock_dir: &std::path::Path,
) -> ServeDisposition {
    // Claims bound to this connection end with it, however it ends.
    let _connection = ConnectionScope::enter(store, subscribers, proxy, stream);
    // The fd is the authority; no token was presented.
    let _principal = ServingPrincipal::enter(Principal::Instance);
    if arm_authenticated_read_poll(stream).is_err() {
        return ServeDisposition::Close;
    }
    let mut reader = control_lanes::lane_reader(stream, Vec::new());
    let mut writer = stream;
    while let Some(line) = read_authenticated_request_line(&mut reader) {
        // Written straight to the stream and flushed before this returns: owed
        // until then ([`FinalReplies`]).
        let _final_reply = FINAL_REPLIES.enter(&line);
        if let Some(disposition) = serve_request_line(
            line,
            scope,
            stream,
            &mut reader,
            &mut writer,
            active,
            store,
            subscribers,
            proxy,
            queue,
            sock_dir,
        ) {
            return disposition;
        }
    }
    ServeDisposition::Close
}

/// Handshake of an ordinary connection: the first line must be `AUTH <hex>` or
/// `TOKEN <hex> <verb…>`, matched against the instance token (`Owner`) or
/// resolved as an edge token (`Edge`). Anything else gets `ERR auth\n` and
/// `None`, closing before any verb runs. `Some` carries the scope and any
/// folded-in verb (empty for a bare `TOKEN`), which then runs through the same
/// handler as a request line.
fn authenticate_connection(
    stream: &CtlStream,
    reader: &mut control_lanes::LaneReader<'_>,
    token: &str,
    active: &ActiveHandle,
    denials: &control_denials::AuthDenials,
    deadline: std::time::Instant,
) -> Option<(Scope, Principal, Option<String>)> {
    // Bound the unauthenticated phase so a peer cannot hold the lane before
    // presenting a token: the first line must be whole by `deadline` (the lane's
    // pick-up plus [`AUTH_READ_TIMEOUT`]), re-armed before every refill so a
    // dribbled byte buys nothing; expiry drops the connection. While a parker
    // runs the line is whole before this is called (the parker holds the
    // connection until it is); this bound is a lane's without one. The reader
    // borrows this stream rather than duplicating the fd (a duplicated AF_UNIX
    // fd's close can block on macOS). Cleared after authentication.
    if stream.set_read_timeout(Some(AUTH_READ_TIMEOUT)).is_err() {
        return None; // cannot bound pre-auth: fail closed
    }
    // An explicit `read_line` loop (not `lines()`) so `feed-bin <n>` can
    // `read_exact` its raw bytes from the same buffered stream.
    let first = read_request_line_until(stream, reader, deadline)?; // `None`: hung up or too slow
    let active_ctx = || resolve_active(active).map(|(_, _, _, ctx)| ctx);
    let (scope, principal, inline_verb) = match first_line_scope(&first, token, active_ctx) {
        Ok(authenticated) => authenticated,
        Err(denial) => {
            // Every refusal is answered and counted; its warning is logged at the
            // 1st, 2nd, 4th … of a run, and a new run logs at once
            // (`control_denials`).
            let peer = control_denials::Peer::of(stream);
            if let Some(reason) = denials.record(denial, &peer, std::time::Instant::now()) {
                log_denial(
                    AUDIT_SUBSYSTEM,
                    "auth",
                    aterm_containment::mode_or_containment(),
                    &reason,
                );
            }
            let mut writer = stream;
            let _ = writer.write_all(b"ERR auth\n");
            let _ = writer.flush();
            return None;
        }
    };
    // Once authenticated, a connection may idle indefinitely and follows the
    // active tab per request. Idle connections park off their lane
    // (`control_lanes.rs`); otherwise on macOS the reader waits for readability
    // or hangup before each refill. The short recv timeout is a backstop, not
    // an idle deadline. Push subscriptions move to their own pool.
    arm_authenticated_read_poll(stream).ok()?;
    Some((scope, principal, inline_verb))
}

/// Whether a request belongs on a wait lane (`control_lanes.rs`): it mostly
/// waits for an event, or hands the connection to a relay. Only decides which
/// lane's time it takes; an unlisted waiting verb costs a request lane, and a
/// missed relay holds one for as long as the relay lives. So a line
/// [`proxy_forward_plan`] relays is a wait here: its scope and selector tests,
/// without its resolution (no graph read or connect probe per request).
fn is_wait_request(line: &str, scope: Scope, store: &Store) -> bool {
    let (selector, verb, rest) = request_head(line);
    // Only an Owner-class connection is relayed; an Edge's line naming another
    // instance's session is refused here, at once.
    let relayed = |sid: &SessionId| {
        scope.is_owner_class()
            && store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .by_sid(sid)
                .is_none()
    };
    // A session this instance does not host: `@<sid>` forwards the connection to
    // the child or sibling that does, and the relay then owns it.
    if let Some(Selector::Sid(sid)) = &selector
        && relayed(sid)
    {
        return true;
    }
    match verb {
        // `subscribe` carries its selector second: one session this instance
        // does not host is relayed for the whole subscription. Self, a local
        // id, a list or `@*` flips to push here at once: request-lane work.
        "subscribe" if selector.is_none() => {
            subscribe_session(rest).is_some_and(|(sid, _)| relayed(&sid))
        }
        // Parks on an event, a settle, a prompt, or the bridge's landing.
        "await" | "ready" | "wait" | "turn" | "post" => true,
        // Parks for the owner to answer the sudo password sheet (up to 300 s),
        // so it holds a wait lane, not one of the RPC lanes.
        "askpass" => true,
        // `inbox get @<off>` parks for the bridge's next drain; the listing and
        // the other forms answer at once.
        "inbox" => {
            let mut words = rest.split_whitespace();
            words.next() == Some("get") && words.next().is_some_and(|w| w.starts_with('@'))
        }
        // `cast drift` settles 60 ms, then replays the recording through fresh
        // engines: seconds of lane time on a long recording.
        "cast" => rest.split_whitespace().next() == Some("drift"),
        // A paste into a session not on screen waits, bounded, for its bytes to
        // reach the program (`background_input`); a request lane must never wait
        // on a program. A `paste` waits so only into another session: a flagless
        // one goes to the App (`front_routed_input`, `cmd_paste`). A `paste-bin`
        // takes the hidden arm whenever its target is not on screen, flagless too
        // (a scoped caller's own background tab, a tab switch that raced it). Its
        // target may be the tab on screen, whose paste answers at once; a wait
        // lane serves that as well.
        "paste" => selector
            .as_ref()
            .is_some_and(|s| !matches!(s, Selector::SelfTok)),
        "paste-bin" => true,
        _ => false,
    }
}

/// Handle one authenticated request, folded into the `TOKEN` line or on its own
/// line: proxy forward, subscribe flip, binary frame, then dispatch
/// and reply. `None` means keep polling; `Some(disposition)` ends this lane.
#[allow(clippy::too_many_arguments)]
fn serve_request_line(
    line: String,
    scope: Scope,
    stream: &CtlStream,
    reader: &mut control_lanes::LaneReader<'_>,
    writer: &mut impl Write,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    sock_dir: &std::path::Path,
) -> Option<ServeDisposition> {
    // The `sudo` askpass relay. Intercepted here, the one layer holding the
    // stream, so it reads the peer once, gates on `Scope::Owner` EXACTLY, and
    // returns the (secret-bearing) reply. The reply reaches the peer through
    // `writer` like any other — on a pooled lane that is the `Outbox` buffer
    // (`serve_line`'s `out`), which `Outbox::scrub` zeroes with `write_volatile`
    // once delivered and on drop, so the plaintext does not linger. Never
    // logged. A selector is refused: the request names no session (the server
    // attributes it by ancestry).
    {
        let (selector, verb, _rest) = request_head(&line);
        if verb == "askpass" {
            let response = if selector.is_some() {
                crate::control_askpass::public_reply("ERR askpass refused takes no @<sid>\n")
            } else {
                crate::control_askpass::serve(stream, scope, proxy, store)
            };
            if writer.write_all(response.as_bytes()).is_err() || writer.flush().is_err() {
                return Some(ServeDisposition::Close);
            }
            return None;
        }
    }
    // Tokenized once: the generic binary-frame lane below reuses it (`line` is
    // not changed in between).
    let bin_verb = binary_frame_verb(&line);
    // Cross-process forward (Item 5b): relay a `@<child-sid>` we spawned but
    // don't host to the child's socket; the relay then owns the connection.
    if try_proxy_forward(&line, scope, store, sock_dir, stream, reader) {
        return Some(ServeDisposition::Close);
    }
    // PUSH FLIP: `subscribe` authorizes its targets EXACTLY like a read verb,
    // then this connection becomes push-only (never reads another line). On an
    // auth/parse failure `run_subscribe_socket` writes a single `ERR ...` and returns,
    // and we close the connection (a half-subscribed connection is meaningless).
    // Transferred to the reserved push pool so a quiet subscriber never consumes
    // an ordinary RPC worker.
    if is_subscribe_line(&line) {
        return Some(ServeDisposition::Subscribe { line, scope });
    }
    // `post … len=<n>` reads the next N raw bytes as the message body. A valid
    // length is always consumed (even on refusal) to keep framing; a length the
    // server will not read closes the connection.
    if let Some(len) = post_frame_len(&line) {
        if !run_post_bin(
            &line,
            len,
            reader,
            active,
            store,
            subscribers,
            scope,
            writer,
        ) {
            return Some(ServeDisposition::Close);
        }
        return None;
    }
    // `feed-bin`/`paste-bin <n>` read N raw bytes from the same stream into the
    // target's PTY (`feed-bin` raw, `paste-bin` with paste semantics), authorized
    // like `feed` inside `run_feed_bin`.
    if let Some(bin_verb) = bin_verb {
        let mut dispatch_front_input =
            |event, session, completion: Option<crate::control_input_ack::InputCompletion>| {
                post_input_reply_to_observed(
                    proxy,
                    vec![event],
                    session,
                    completion.map(|c| c.into_mark()),
                )
            };
        let mut clear_license = |session| front_routed_license_clear(proxy, session, false);
        if !run_feed_bin_routed_observed(
            &line,
            bin_verb,
            reader,
            FeedBinRoute {
                active,
                store,
                scope,
                subscribers: Some(subscribers),
            },
            &mut dispatch_front_input,
            &mut clear_license,
            writer,
        ) {
            return Some(ServeDisposition::Close);
        }
        return None;
    }
    let resp = dispatch_request(
        &line,
        active,
        store,
        subscribers,
        scope,
        proxy,
        queue,
        sock_dir,
    );
    // A dead client (broken pipe) must not crash the app — just drop it.
    match write_control_reply_to_stream(stream, writer, resp) {
        Ok(Some(retention)) => {
            await_guarded_reply_close(stream, reader, retention);
            Some(ServeDisposition::Close)
        }
        Ok(None) => None,
        Err(_) => Some(ServeDisposition::Close),
    }
}

/// The replies this instance owes for requests whose own action can END the
/// process, each counted by a [`FinalReply`] and waited for at exit
/// ([`settle_final_replies`]).
///
/// WHY. Closing the last window's last tab is the app's exit: the main thread
/// answers `close` (`Wake::CloseSession`), stops the event loop, runs its teardown
/// and calls `process::exit`, while the lane that was handed the answer still has
/// to be scheduled to deliver it, and a peer slow to read leaves the rest of it
/// with the parker. Nothing ordered the two, so the `close` verb's promise that its
/// reply is the instance's final one held only while the lane won the race. The
/// verbs are those whose own action can end the process: `close`, `tab close`,
/// `invoke` (Quit, Close Tab, Close Window) and `confirm` (answering a quit
/// question). Any other reply in flight at exit is not waited for.
pub(super) struct FinalReplies {
    owed: Mutex<usize>,
    delivered: Condvar,
}

/// The instance's [`FinalReplies`].
pub(super) static FINAL_REPLIES: FinalReplies = FinalReplies::new();

impl FinalReplies {
    pub(super) const fn new() -> Self {
        Self {
            owed: Mutex::new(0),
            delivered: Condvar::new(),
        }
    }

    /// Count `line`'s reply as owed when its verb can end the process; `None`
    /// otherwise. Taken BEFORE the dispatch (the action may end the process),
    /// dropped once the reply is delivered or its connection is gone.
    pub(super) fn enter(&'static self, line: &str) -> Option<FinalReply> {
        let (_, verb, rest) = request_head(line);
        let may_end = match verb {
            "close" | "invoke" | "confirm" => true,
            "tab" => rest.split_whitespace().next() == Some("close"),
            _ => false,
        };
        may_end.then(|| {
            *self
                .owed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
            FinalReply { replies: self }
        })
    }

    /// Wait, at most `bound`, until no reply is owed: `true` when none is.
    fn settle(&self, bound: std::time::Duration) -> bool {
        let owed = self
            .owed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (owed, _) = self
            .delivered
            .wait_timeout_while(owed, bound, |owed| *owed > 0)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *owed == 0
    }

    #[cfg(test)]
    pub(super) fn owed(&self) -> usize {
        *self
            .owed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One owed reply ([`FinalReplies`]): held from before its request's dispatch
/// until the reply is delivered (the lane's `Outbox`, or a direct writer's flush).
pub(super) struct FinalReply {
    replies: &'static FinalReplies,
}

impl Drop for FinalReply {
    fn drop(&mut self) {
        let mut owed = self
            .replies
            .owed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *owed = owed.saturating_sub(1);
        if *owed == 0 {
            self.replies.delivered.notify_all();
        }
    }
}

/// Wait, at most `bound`, until every [`FinalReply`] is delivered: the exit seam's
/// call, `true` when none is owed any more. A lane still inside such a request
/// finishes quickly once the event loop is gone (a main-thread hop finds its reply
/// sender dropped), so the bound only guards a client that stopped reading.
pub(crate) fn settle_final_replies(bound: std::time::Duration) -> bool {
    FINAL_REPLIES.settle(bound)
}

#[cfg(test)]
mod final_reply_tests {
    use super::FinalReplies;
    use std::time::Duration;

    /// Only a request whose own action can end the process is owed at exit, and
    /// it stays owed exactly while its guard is held.
    #[test]
    fn only_a_request_that_can_end_the_process_is_owed() {
        static REPLIES: FinalReplies = FinalReplies::new();
        for line in [
            "close",
            "@s-1 close",
            "tab close",
            "tab close 0",
            "invoke Quit",
            "confirm yes",
        ] {
            let guard = REPLIES.enter(line);
            assert!(guard.is_some(), "{line:?} can end the process");
            assert_eq!(REPLIES.owed(), 1, "{line:?}");
            drop(guard);
            assert_eq!(REPLIES.owed(), 0, "{line:?} delivered");
        }
        for line in ["ls", "tab new", "tab 0", "text", "closed", "await seq 3"] {
            assert!(REPLIES.enter(line).is_none(), "{line:?}");
        }
        assert_eq!(REPLIES.owed(), 0);
    }

    /// The exit seam's wait returns once the owed reply is delivered, and gives up
    /// at its bound while one never is: an exit is delayed, never held.
    #[test]
    fn the_exit_waits_for_an_owed_reply_and_only_to_its_bound() {
        static REPLIES: FinalReplies = FinalReplies::new();
        assert!(REPLIES.settle(Duration::ZERO), "nothing owed: no wait");
        let guard = REPLIES.enter("close").expect("close is owed");
        assert!(
            !REPLIES.settle(Duration::from_millis(20)),
            "owed: the wait ends at its bound"
        );
        let writer = std::thread::spawn(move || drop(guard));
        // A hang detector, never a latency budget.
        assert!(
            REPLIES.settle(aterm_spec::wait::MUST_HAPPEN),
            "delivered: the wait ends"
        );
        writer.join().expect("writer thread");
    }
}

/// Read one request line, stripping `\n` (and `\r`). `None` on EOF, a read
/// error, or a line longer than [`MAX_REQUEST_LINE`]. Test-only: a socket's
/// first line is read under a deadline ([`read_request_line_until`]).
#[cfg(test)]
fn read_request_line(reader: &mut impl BufRead) -> Option<String> {
    read_request_line_with_policy(reader, false, || Ok(()))
}

/// Read one line under one monotonic deadline, re-armed before every buffer
/// refill, so a peer cannot hold a lane by dribbling bytes: the handshake's
/// first line, and the ACK that releases a guarded artifact.
///
/// A detached peer's queued bytes are still read: the one-shot `aterm ctl`
/// writes `ACK <nonce>` and exits, and on Darwin `SO_RCVTIMEO` then fails with
/// EINVAL while `recv` still returns the queued bytes (see
/// [`enter_detached_peer_drain`]).
fn read_request_line_until(
    stream: &CtlStream,
    reader: &mut impl BufRead,
    deadline: std::time::Instant,
) -> Option<String> {
    // Latched, not re-tested per refill: once Darwin has refused the bound for
    // a detached peer, another failing syscall buys nothing.
    let mut draining_detached_peer = false;
    read_request_line_with_policy(reader, false, || {
        // The absolute deadline still governs, drain or not: it is the one
        // bound a peer cannot influence.
        let budget = remaining_artifact_io_budget(deadline)?;
        if draining_detached_peer {
            return Ok(());
        }
        match stream.set_read_timeout(Some(budget)) {
            Ok(()) => Ok(()),
            Err(refused) => {
                enter_detached_peer_drain(stream, refused)?;
                draining_detached_peer = true;
                Ok(())
            }
        }
    })
}

/// Recover from Darwin's detached-peer `EINVAL` by switching to non-blocking
/// reads: the socket yields queued bytes then EOF, and an unexpectedly attached
/// peer surfaces as `WouldBlock` rather than parking a worker. Other errors are
/// returned unchanged.
#[cfg(target_os = "macos")]
fn enter_detached_peer_drain(stream: &CtlStream, refused: std::io::Error) -> std::io::Result<()> {
    if refused.raw_os_error() != Some(libc::EINVAL) {
        return Err(refused);
    }
    stream.set_nonblocking(true)
}

#[cfg(not(target_os = "macos"))]
fn enter_detached_peer_drain(_stream: &CtlStream, refused: std::io::Error) -> std::io::Result<()> {
    Err(refused)
}

/// Read one authenticated request without imposing an application idle deadline.
/// On macOS, a level-triggered socket wait parks the lane until data or EOF;
/// the existing recv timeout remains a backstop for a spurious readiness event.
/// A coalesced next line already in `BufReader` must bypass the socket wait:
/// its bytes are no longer visible to `poll` and waiting would deadlock.
fn read_authenticated_request_line(reader: &mut control_lanes::LaneReader<'_>) -> Option<String> {
    let stream = reader.get_ref().stream();
    let mut buffered_first_refill = !reader.buffer().is_empty();
    read_request_line_with_policy(reader, true, || {
        if std::mem::take(&mut buffered_first_refill) {
            return Ok(());
        }
        // After a refill without a newline, the parser consumes the entire
        // visible chunk. Its next iteration is therefore an underlying read.
        wait_authenticated_readable(stream)
    })
}

/// On Darwin `poll(POLLIN)` reports a write-half close as readable plus hangup
/// even when `recvfrom` would sleep; wait on it so idle reads do not wake every
/// 250 ms. Polls the borrowed fd (a cloned AF_UNIX fd's close can stall).
#[cfg(target_os = "macos")]
fn wait_authenticated_readable(stream: &CtlStream) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let mut fd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    loop {
        fd.revents = 0;
        // SAFETY: `stream` remains borrowed throughout this call and `fd` is
        // one initialized pollfd. -1 parks until data, peer hangup, or a signal.
        match unsafe { libc::poll(&mut fd, 1, -1) } {
            n if n > 0 => return authenticated_poll_event(fd.revents),
            0 => {
                return Err(std::io::Error::other(
                    "unbounded control socket poll timed out",
                ));
            }
            _ => {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn authenticated_poll_event(revents: libc::c_short) -> std::io::Result<()> {
    if revents & libc::POLLNVAL != 0 {
        return Err(std::io::Error::from_raw_os_error(libc::EBADF));
    }
    if revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
        // Read even on HUP/ERR: queued bytes (including a final unterminated
        // line) must be drained before EOF is observed.
        return Ok(());
    }
    Err(std::io::Error::other(
        "control socket poll returned no read event",
    ))
}

#[cfg(not(target_os = "macos"))]
fn wait_authenticated_readable(_stream: &CtlStream) -> std::io::Result<()> {
    Ok(())
}

fn read_request_line_with_policy(
    reader: &mut impl BufRead,
    retry_idle_timeout: bool,
    mut before_read: impl FnMut() -> std::io::Result<()>,
) -> Option<String> {
    let mut buf = Vec::with_capacity(64);
    loop {
        // Re-arm the absolute deadline once per `fill_buf` refill and consume the
        // whole chunk, rather than a syscall pair per byte.
        before_read().ok()?;
        match reader.fill_buf() {
            Ok([]) => {
                // EOF: yield a final unterminated line if any, else stop.
                return if buf.is_empty() {
                    None
                } else {
                    Some(decode_request_line(buf))
                };
            }
            Ok(available) => {
                let newline = available.iter().position(|byte| *byte == b'\n');
                let chunk_len = newline.unwrap_or(available.len());
                let remaining = MAX_REQUEST_LINE.saturating_sub(buf.len());
                if chunk_len > remaining {
                    return None; // runaway line: drop the connection
                }
                buf.extend_from_slice(&available[..chunk_len]);
                let consumed = chunk_len + usize::from(newline.is_some());
                reader.consume(consumed);
                if newline.is_some() {
                    return Some(decode_request_line(buf));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error)
                if retry_idle_timeout
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
            {
                continue;
            }
            Err(_) => return None,
        }
    }
}

/// Move an authenticated push subscription to its own bounded pool; saturation
/// returns an explicit error and the RPC worker is reusable at once.
fn handoff_subscription(
    subscriptions: &SubscriptionDispatch,
    line: String,
    scope: Scope,
    stream: CtlStream,
) {
    if let Err(mut stream) = subscriptions.try_submit(line, scope, stream) {
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(100)));
        let _ = stream.write_all(b"ERR subscription capacity busy; retry\n");
        let _ = stream.flush();
    }
}

/// Upper bound on one control request line (`feed-bin` payloads are bounded
/// separately); longer lines drop the connection.
const MAX_REQUEST_LINE: usize = 64 * 1024;

/// How long a fresh connection has to make its handshake line whole: one
/// deadline from admission, not a timeout per read, so a peer that sends a byte
/// now and then does not keep its connection for it. The parker holds the
/// connection meanwhile (`control_lanes.rs`); a lane without one reads the line
/// under it, from its pick-up.
const AUTH_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Receive timeout backstop for authenticated sockets (the liveness cadence
/// off macOS). Timed-out reads are retried forever.
const AUTHENTICATED_READ_POLL: std::time::Duration = std::time::Duration::from_millis(250);

fn arm_authenticated_read_poll(stream: &CtlStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(AUTHENTICATED_READ_POLL))
}

/// Turn a raw line into the dispatch `String`: strip a trailing `\r`, then
/// decode UTF-8 lossily so a bad byte does not kill the connection.
fn decode_request_line(mut buf: Vec<u8>) -> String {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    // Valid UTF-8 (every ASCII line) keeps `buf`'s allocation; only a malformed
    // line pays the lossy copy.
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Whether a request line's first token is `subscribe`, which [`serve`] diverts
/// to push mode before per-verb dispatch.
fn is_subscribe_line(line: &str) -> bool {
    let line = line.strip_suffix('\r').unwrap_or(line);
    matches!(line.split_whitespace().next(), Some("subscribe"))
}

/// Maximum `feed-bin` payload per frame (256 KiB).
const MAX_FEED_BIN: usize = 256 * 1024;

/// Maximum `@<sel>` targets in one `subscribe`. Each target adds a `ByteFanout`
/// slot the PTY reader tees on every burst, so the list is capped; selectors are
/// also de-duplicated by session.
pub(crate) const MAX_SUBSCRIBE_TARGETS: usize = 256;

/// Whether a request line is a binary-frame verb (optionally `@<sel>`-prefixed)
/// whose length-prefixed payload [`serve`] reads before dispatch: `feed-bin`
/// (raw) or `paste-bin` (paste seam). Named so the serve-loop sync test finds
/// each in the VERBS table.
fn binary_frame_verb(line: &str) -> Option<&'static str> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut it = line.split_whitespace();
    // An optional leading `@<selector>` (cross-session) precedes the verb.
    let tok = match it.next() {
        Some(t) if t.starts_with('@') => it.next(),
        other => other,
    };
    match tok {
        Some("feed-bin") => Some("feed-bin"),
        Some("paste-bin") => Some("paste-bin"),
        _ => None,
    }
}

/// Whether this line is a length-prefixed `post`, whose body (up to
/// [`crate::fabric::BODY_MAX`]) travels on the stream so model text never
/// passes through argv. `Some(Ok(n))` announces n bytes; `Some(Err(reply))` is
/// a `len=` the server will not read ([`crate::fabric::post_len`]), which
/// desyncs the stream and closes the connection.
///
/// The scan follows `cmd_post`'s own rule — OPTIONS LEAD, the first token that is
/// not one (or the first after `--`) begins the inline text — so a `post … hello len=3` whose `len=` sits in
/// the BODY is not a frame, and the two parsers cannot disagree about where the
/// text starts. That symmetry is load-bearing: a detector that saw a frame the
/// handler did not would leave n bytes of body to be read as control verbs.
fn post_frame_len(line: &str) -> Option<Result<usize, String>> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut it = line.split_whitespace();
    let tok = match it.next() {
        Some(t) if t.starts_with('@') => it.next(),
        other => other,
    };
    if tok != Some("post") {
        return None;
    }
    // The option vocabulary `cmd_post` accepts before the body begins, and its
    // `--` end of options — the SAME reading the handler uses, not a copy of it.
    crate::fabric::post_options(it)
        .find_map(|tok| tok.strip_prefix("len="))
        .map(crate::fabric::post_len)
}

/// The bytes of body a request `line` announces: what [`serve_request_line`]
/// reads after the line before it answers — a `post … len=<n>`, `feed-bin <n>`,
/// or `paste-bin <n>` with a length it will read.
/// Zero for every other line, and for a length it refuses unread (it answers
/// and closes instead). The lanes serve a request only once this much has
/// followed its line (`control_lanes::held`), so a lane never waits for a body.
fn announced_body(line: &str) -> usize {
    if let Some(len) = post_frame_len(line) {
        return len.unwrap_or(0);
    }
    binary_frame_verb(line).map_or(0, |verb| match parse_feed_bin(line, verb) {
        FeedBinFrame::Ok(_, n, _) => n,
        FeedBinFrame::Malformed | FeedBinFrame::TooLarge => 0,
    })
}

/// Resolve only the target-bearing prefix of a binary input header, independent
/// of the length parser: once a client names a write verb and target, even a
/// malformed or over-cap attempt is a new input boundary for that target's
/// cursor licence. Unknown verbs return `None`; authorization is checked
/// separately.
fn binary_frame_attempt_selector(line: &str, verb: &str) -> Option<Option<Selector>> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut it = line.split_whitespace();
    let first = it.next()?;
    if let Some(body) = first.strip_prefix('@') {
        (it.next()? == verb).then(|| Some(Selector::parse(body)))
    } else {
        (first == verb).then_some(None)
    }
}

/// Outcome of parsing `[@<sel>] feed-bin <n>`. Because the payload is pipelined
/// on the same stream, the failures need opposite handling:
/// - [`Malformed`](FeedBinFrame::Malformed): no length announced, so reply
///   `ERR` and stay framed.
/// - [`TooLarge`](FeedBinFrame::TooLarge): a length past [`MAX_FEED_BIN`]; the
///   client already sent bytes we will not read, so the caller must close the
///   connection rather than execute them as verbs.
enum FeedBinFrame {
    /// Not a well-formed feed-bin line (missing / non-numeric length).
    Malformed,
    /// Well-formed but the declared length exceeds [`MAX_FEED_BIN`].
    TooLarge,
    /// A valid frame: optional selector, length (`<= MAX_FEED_BIN`) and the
    /// optional idempotency key, carried in the header so a refused key still
    /// consumes the payload.
    Ok(Option<Selector>, usize, Option<String>),
}

/// Parse `[@<sel>] feed-bin <n>`. Pure; see [`FeedBinFrame`] for why oversize
/// and malformed differ.
fn parse_feed_bin(line: &str, verb: &str) -> FeedBinFrame {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let mut it = line.split_whitespace();
    let Some(first) = it.next() else {
        return FeedBinFrame::Malformed;
    };
    let (selector, len_tok) = if let Some(body) = first.strip_prefix('@') {
        // `@<sel> <verb> <n>`: next token must be the verb, then the length.
        match (it.next(), it.next()) {
            (Some(v), Some(tok)) if v == verb => (Some(Selector::parse(body)), tok),
            _ => return FeedBinFrame::Malformed,
        }
    } else {
        // `<verb> <n>`
        if first != verb {
            return FeedBinFrame::Malformed;
        }
        match it.next() {
            Some(tok) => (None, tok),
            None => return FeedBinFrame::Malformed,
        }
    };
    // One optional trailing token, `feed-bin` only: the idempotency key
    // `id=<epoch>:<producer>:<seq>`. `paste-bin` takes none.
    //
    // Any other trailing token is malformed (`feed-bin 1 junk` must not consume
    // a byte of the next request). A token shaped like a key but invalid as one
    // is not malformed here: the payload is read and the key parser refuses it.
    let idem = match it.next() {
        None => None,
        Some(tok) if verb == "feed-bin" => match tok.strip_prefix("id=") {
            Some(key) => Some(key.to_string()),
            None => return FeedBinFrame::Malformed,
        },
        Some(_) => return FeedBinFrame::Malformed,
    };
    if it.next().is_some() {
        return FeedBinFrame::Malformed;
    }
    let Ok(n) = len_tok.parse::<usize>() else {
        return FeedBinFrame::Malformed;
    };
    if n > MAX_FEED_BIN {
        return FeedBinFrame::TooLarge;
    }
    FeedBinFrame::Ok(selector, n, idem)
}

/// Handle `[@<sel>] feed-bin <n>\n<bytes>`: read the n raw bytes from the same
/// stream and route them to the target's PTY like `feed`. Returns `true` to
/// keep the connection, `false` to close it.
///
/// Authorized like `feed` (`WriteInput`, per target). A malformed line replies
/// `ERR usage` and stays framed; an over-cap length closes the connection. A
/// valid length is always consumed, even on denial, so framing holds.
#[cfg(all(test, unix))]
fn run_feed_bin<W: Write>(
    line: &str,
    verb: &str,
    reader: &mut impl BufRead,
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
    writer: &mut W,
) -> bool {
    let mut dispatch =
        |event, _session, completion: Option<crate::control_input_ack::InputCompletion>| {
            let Some((term, _, actual_session, ctx)) = resolve_active(active) else {
                return Err("ERR input dispatch unavailable\n".to_string());
            };
            let receipt = crate::app_input::tracked_egress(
                &term,
                &ctx.modes,
                &ctx.sink,
                &ctx.output_echo,
                &event,
                crate::input::EgressMode::Backpressured,
            );
            if let Some(completion) = completion.filter(|c| c.matches(actual_session, &ctx)) {
                completion.finish(receipt);
            }
            Ok(InputOutcome::Ok)
        };
    let mut clear_license = |_session| "OK\n".to_string();
    run_feed_bin_routed_observed(
        line,
        verb,
        reader,
        FeedBinRoute {
            active,
            store,
            scope,
            subscribers: None,
        },
        &mut dispatch,
        &mut clear_license,
        writer,
    )
}

/// THE `mark` ARM (design §7.4, phase 3A): a claim written to session `session`'s
/// log, attributed to the connection ([`control_session::log_actor`]), and the
/// session's watchers woken once it is in. Its whole job is these calls: a mark
/// is a record, and this path names no input, signal or session write — the
/// `slog-inert` census guard holds this function to exactly this list, and the
/// functional negative control (`a_mark_act_on_an_open_approval_writes_no_byte`)
/// proves no byte reaches the PTY.
fn dispatch_mark(
    ctx: &SessionCtx,
    scope: Scope,
    session: u64,
    rest: &str,
    subscribers: &Subscribers,
) -> String {
    let reads = scope.is_owner_class() || cross_session_authorized(scope, "text", ctx);
    let (reply, owed) =
        crate::session_log::cmd_mark(ctx, control_session::log_actor(scope, ctx), reads, rest);
    if owed {
        wake_watchers(subscribers, session);
    }
    reply
}

/// WAKE SESSION `local`'s SUBSCRIBERS so a timeline row just recorded drains
/// NOW, not on the push loop's 250 ms tick — `meta`'s rule. The fabric verbs
/// whose row the bridge acts on (`post`, `topic`, `inbox seen`, a parked
/// `inbox get @<off>`) all wake through this, because the bridge's push lane
/// is one of those subscribers and has no idle poll of its own.
fn wake_watchers(subscribers: &Subscribers, local: u64) {
    if subscribers.any() {
        subscribers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .notify(local);
    }
}

/// `post` (both forms) for session `local` via
/// [`crate::fabric::cmd_post_waking`], waking that session's `events`
/// subscribers so the `post` record drains now (the bridge drains the outbox on
/// `EVENT <sid> post`).
fn post_waking_watchers(
    ctx: &SessionCtx,
    local: u64,
    rest: &str,
    body: Option<Vec<u8>>,
    subscribers: &Subscribers,
) -> String {
    crate::fabric::cmd_post_waking(ctx, rest, body, &|| wake_watchers(subscribers, local))
}

/// Serve a length-prefixed `post`: read the body, then hand it to
/// [`post_waking_watchers`]. Returns `false` to close only when the stream is
/// desynced (a refused length or a short read).
///
/// An Edge connection is refused outright (see [`post_scope_denied`]); the
/// refusal still consumes the body.
#[allow(
    clippy::too_many_arguments,
    reason = "the framed post reads its body off the stream and wakes the target's subscribers"
)]
fn run_post_bin<W: Write>(
    line: &str,
    len: Result<usize, String>,
    reader: &mut impl BufRead,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    writer: &mut W,
) -> bool {
    let n = match len {
        Ok(n) => n,
        Err(reply) => {
            let _ = writer.write_all(reply.as_bytes());
            let _ = writer.flush();
            return false;
        }
    };
    let mut body = vec![0u8; n];
    if read_exact_authenticated(reader, &mut body).is_err() {
        return false;
    }
    let (selector, verb, rest) = request_head(line);
    debug_assert_eq!(verb, "post");
    let target = match selector.as_ref() {
        None | Some(Selector::SelfTok) => Ok(resolve_active(active)),
        Some(sel) => resolve_explicit(store, sel),
    };
    let target = match target {
        Ok(target) => target,
        // The body is already read, so the stream stays framed: answer the
        // refusal and keep the connection.
        Err(refusal) => {
            if writer.write_all(refusal.as_bytes()).is_err() {
                return false;
            }
            return writer.flush().is_ok();
        }
    };
    let response = match target {
        _ if post_scope_denied(scope) => "ERR denied\n".to_string(),
        Some((_, _, local, ctx)) => {
            post_waking_watchers(&ctx, local, rest, Some(body), subscribers)
        }
        None if matches!(selector, Some(Selector::Local(_) | Selector::Sid(_))) => {
            "ERR no such session\n".to_string()
        }
        None => NO_ACTIVE_TERMINAL.to_string(),
    };
    if writer.write_all(response.as_bytes()).is_err() {
        return false;
    }
    writer.flush().is_ok()
}

/// The header of a `[@<sel>] feed-bin|paste-bin <n>` frame: the input boundary
/// for its target, acted on before the payload is waited for. Resolve and
/// authorize the header prefix first, so a slowly trickled frame cannot let
/// unrelated child output borrow the old key-hint licence: an authorized
/// target's licence is fenced here. A lane runs it as soon as the header is
/// whole (`LaneService::frame_header`) and again when the whole frame is served
/// ([`run_feed_bin_routed`]): the second fence is one more of the kind the
/// payload gets anyway at its effect boundary, before egress.
/// `false`: refused before the payload (an ambiguous target, a failed fence),
/// the answer in `writer`; the stream is desynced, so the connection closes.
fn feed_bin_header<W: Write, C>(
    line: &str,
    verb: &str,
    route: FeedBinRoute<'_>,
    clear_license: &mut C,
    writer: &mut W,
) -> bool
where
    C: FnMut(u64) -> String,
{
    // Malformed and over-cap attempts follow the same rule; unknown or
    // unauthorized targets never mutate UI state.
    let attempt_selector = binary_frame_attempt_selector(line, verb);
    let header_active_target = resolve_active(route.active);
    // An ambiguous id must not clear another instance's licence either; refuse
    // before the payload.
    let mut header_refusal = None;
    let header_target = attempt_selector
        .as_ref()
        .and_then(|selector| match selector {
            None | Some(Selector::SelfTok) => header_active_target.clone(),
            Some(sel) => match resolve_explicit(route.store, sel) {
                Ok(target) => target,
                Err(refusal) => {
                    header_refusal = Some(refusal);
                    None
                }
            },
        });
    if let Some(refusal) = header_refusal {
        // The payload is unread, so the stream is desynced: answer and close.
        let _ = writer.write_all(refusal.as_bytes());
        let _ = writer.flush();
        return false;
    }
    let header_authorized = header_target.as_ref().is_some_and(|(_, _, _, ctx)| {
        route.scope.is_owner_class() || cross_session_authorized(route.scope, "feed", ctx)
    });
    if header_authorized && let Some((_, _, session, _)) = header_target.as_ref() {
        let response = clear_license(*session);
        if !response.starts_with("OK") {
            // The main-thread fence failed; do not wait for the payload.
            let _ = writer.write_all(response.as_bytes());
            let _ = writer.flush();
            return false;
        }
    }

    true
}

#[derive(Clone, Copy)]
struct FeedBinRoute<'a> {
    active: &'a ActiveHandle,
    store: &'a Store,
    scope: Scope,
    /// The watchers to wake once the frame's input is in the session log
    /// (phase 3A); `None` where none can be watching (the header's check,
    /// the tests' direct route).
    subscribers: Option<&'a Subscribers>,
}

#[cfg(test)]
fn run_feed_bin_routed<W: Write, F, C>(
    line: &str,
    verb: &str,
    reader: &mut impl BufRead,
    route: FeedBinRoute<'_>,
    dispatch_front_input: &mut F,
    clear_license: &mut C,
    writer: &mut W,
) -> bool
where
    F: FnMut(InputEvent, Option<u64>) -> Result<InputOutcome, String>,
    C: FnMut(u64) -> String,
{
    let mut observed = |event, target, _completion| dispatch_front_input(event, target);
    run_feed_bin_routed_observed(
        line,
        verb,
        reader,
        route,
        &mut observed,
        clear_license,
        writer,
    )
}

fn run_feed_bin_routed_observed<W: Write, F, C>(
    line: &str,
    verb: &str,
    reader: &mut impl BufRead,
    route: FeedBinRoute<'_>,
    dispatch_front_input: &mut F,
    clear_license: &mut C,
    writer: &mut W,
) -> bool
where
    F: FnMut(
        InputEvent,
        Option<u64>,
        Option<crate::control_input_ack::InputCompletion>,
    ) -> Result<InputOutcome, String>,
    C: FnMut(u64) -> String,
{
    // `paste-bin` goes through the paste seam (bracketing, sanitize, LF→CR under
    // the target's lock); `feed-bin` writes raw. Everything else is shared.
    let paste = verb == "paste-bin";
    if !feed_bin_header(line, verb, route, clear_license, writer) {
        return false;
    }
    let (selector, n, idem_key) = match parse_feed_bin(line, verb) {
        FeedBinFrame::Ok(selector, n, idem) => (selector, n, idem),
        FeedBinFrame::Malformed => {
            // No parseable length ⇒ no payload was announced; a client that merely
            // typo'd the verb stays framed. No payload read.
            let _ =
                writer.write_all(format!("ERR usage: {verb} <n> then <n> raw bytes\n").as_bytes());
            let _ = writer.flush();
            return true;
        }
        FeedBinFrame::TooLarge => {
            // Over-cap length: the client already sent bytes we refuse to read and
            // cannot skip, so close rather than dispatch them as verbs.
            let _ = writer.write_all(
                format!("ERR {verb} too large: the cap is {MAX_FEED_BIN} bytes\n").as_bytes(),
            );
            let _ = writer.flush();
            return false;
        }
    };
    // Read exactly n bytes; kernel timeout ticks retry with the filled prefix. A
    // real short read closes the connection.
    let mut payload = vec![0u8; n];
    if read_exact_authenticated(reader, &mut payload).is_err() {
        return false;
    }

    // The payload may arrive long after the header, so re-resolve target and
    // capability at the effect boundary (tab switch, replacement, revocation).
    let active_target = resolve_active(route.active);
    let front_terminal_session = active_target.as_ref().map(|(_, _, session, _)| *session);
    let target = match selector.as_ref() {
        None | Some(Selector::SelfTok) => Ok(active_target.clone()),
        Some(sel) => resolve_explicit(route.store, sel),
    };
    // Re-check ambiguity at the effect boundary too; the payload is consumed, so
    // reply and stay framed.
    let target = match target {
        Ok(target) => target,
        Err(refusal) => {
            if writer.write_all(refusal.as_bytes()).is_err() {
                return false;
            }
            return writer.flush().is_ok();
        }
    };
    let authorized = target.as_ref().is_some_and(|(_, _, _, ctx)| {
        route.scope.is_owner_class() || cross_session_authorized(route.scope, "feed", ctx)
    });

    // Like inline `paste`: explicit selectors stay terminal operations, a
    // bare/self paste with native front content goes to the main-thread input
    // seam. `feed-bin` never takes this branch.
    if paste {
        let principal = if route.scope.is_owner_class() {
            NativeControlPrincipal::Owner
        } else {
            NativeControlPrincipal::Edge
        };
        // Lazy: only the native-front arm reads the payload.
        if let Some(route) = sessionless_front_paste_event(
            "paste",
            || String::from_utf8_lossy(&payload).into_owned(),
            selector.as_ref(),
            active_target.is_some(),
            principal,
        ) {
            let response = match route {
                Ok(event) => match dispatch_front_input(event, None, None) {
                    Ok(InputOutcome::Ok) => format!("OK {n} bytes\n"),
                    Ok(InputOutcome::RangeRejected) => "ERR out of range\n".to_string(),
                    Ok(InputOutcome::WriteFailed) => "ERR write failed\n".to_string(),
                    Err(error) => error,
                },
                Err(error) => error.to_string(),
            };
            if writer.write_all(response.as_bytes()).is_err() {
                return false;
            }
            return writer.flush().is_ok();
        }
    }

    // Resolve the target and gate it like `feed`. The payload is consumed, so
    // every path replies and stays framed. Both verbs use the target terminal
    // tuple (`paste-bin` formats under its lock).
    let Some((term, _, target_session, ctx)) = target else {
        let error = if matches!(&selector, None | Some(Selector::SelfTok)) {
            NO_ACTIVE_TERMINAL
        } else {
            "ERR no such session\n"
        };
        let _ = writer.write_all(error.as_bytes());
        let _ = writer.flush();
        return true;
    };

    // `WriteInput` gate on one per-session predicate for self and cross: Owner
    // holds; an Edge needs a `decide_edge` token for the resolved target's table
    // and nonce, so a moved `@.` grants nothing.
    if !authorized {
        log_denial(
            AUDIT_SUBSYSTEM,
            &format!("feed-bin -> {}", ctx.self_id.as_str()),
            aterm_containment::mode_or_containment(),
            "no authorizing edge for feed-bin",
        );
        let _ = writer.write_all(b"ERR denied\n");
        let _ = writer.flush();
        return true;
    }

    // The self target can move while the payload is in flight, and new local
    // input can stamp a licence; fence the post-payload target before egress.
    let cleared = clear_license(target_session);
    if !cleared.starts_with("OK") {
        let _ = writer.write_all(cleared.as_bytes());
        let _ = writer.flush();
        return false;
    }

    // Fleet halt against the resolved target: binary frames bypass `handle()`'s
    // halt gate. The payload is consumed, so the stream stays framed.
    if let Some(refusal) = crate::fabric::halt_refusal(&ctx, verb) {
        let _ = writer.write_all(refusal.as_bytes());
        let _ = writer.flush();
        return true;
    }
    // …and the unread-input gate, for the same reason (`input_stall`): a
    // refusal only on an established stall or a stopped job; a slow read is
    // written and warned on the `OK <n> bytes`.
    let warning = match crate::input_stall::admission(&ctx, verb, "", false) {
        crate::input_stall::Admission::Refuse(refusal) => {
            let _ = writer.write_all(refusal.as_bytes());
            let _ = writer.flush();
            return true;
        }
        crate::input_stall::Admission::Warn(warning) => Some(warning),
        crate::input_stall::Admission::Open => None,
    };

    // Turn lease: binary frames bypass the verb-dispatch refusal of write verbs
    // while a turn or another connection's hard drive lease holds the session;
    // refuse identically so raw bytes cannot corrupt a turn's output.
    if let Some(held) = ctx
        .turn_lease
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(|l| l.write_block(crate::metrics::now_us(), serving_connection()))
    {
        let _ = writer.write_all(format!("ERR busy {held}\n").as_bytes());
        let _ = writer.flush();
        return true;
    }

    // Self-feed floor: binary frames bypass the verb-dispatch floor, so apply
    // the same per-session injection bucket on the self path (the cross path is
    // gated by the edge above).
    if matches!(&selector, None | Some(Selector::SelfTok))
        && !crate::inject_floor::allow(target_session, payload.len().max(1))
    {
        let _ = writer.write_all(RATE_REFUSAL.as_bytes());
        let _ = writer.flush();
        return true;
    }

    // A visible terminal tab gets the same App input side effects as a human
    // paste (cursor-movement provenance, viewport snap, selection clearing,
    // blink reset), including an explicit selector naming it. The exact session
    // is passed so a racing tab switch degrades to hidden-session delivery.
    // The idempotency gate wraps only the write, so every earlier refusal leaves
    // the key's sequence unconsumed; a frame without `id=` runs unguarded. A
    // keyed zero-length frame is refused before the claim.
    if let Some(refusal) =
        crate::pty_idem::empty_payload_refusal(idem_key.as_deref(), payload.is_empty())
    {
        let _ = writer.write_all(refusal.as_bytes());
        let _ = writer.flush();
        return true;
    }
    let mark = crate::session_log::InputVerb::of(verb).map(|input| {
        crate::control_input_ack::InputMark::at_epoch(
            &ctx,
            route.subscribers,
            target_session,
            control_session::log_actor(route.scope, &ctx),
            input,
            None,
            false,
        )
    });
    let completion = || mark.as_ref().map(|mark| mark.completion());
    let response = crate::pty_idem::guarded(&ctx, route.scope, verb, idem_key.as_deref(), || {
        if front_terminal_session == Some(target_session) {
            let event = if paste {
                InputEvent::Paste(
                    String::from_utf8_lossy(&payload).into_owned(),
                    PasteFraming::AtDrain,
                )
            } else {
                // Raw input is not the user's fingers, but it still goes through
                // App::input so older licences close first. The direct route
                // below fences the session explicitly, since another window may
                // present it.
                InputEvent::KeySequence(payload.clone())
            };
            return match dispatch_front_input(event, Some(target_session), completion()) {
                Ok(InputOutcome::Ok) => format!("OK {n} bytes\n"),
                Ok(InputOutcome::RangeRejected) => "ERR out of range\n".to_string(),
                Ok(InputOutcome::WriteFailed) => "ERR write failed\n".to_string(),
                Err(error) => error,
            };
        }

        crate::metrics::note_input();
        crate::note_unseamed_control_bytes(&payload);
        let event = if paste {
            // AtDrain preserves the old direct route's mode-at-write framing.
            InputEvent::Paste(
                String::from_utf8_lossy(&payload).into_owned(),
                PasteFraming::AtDrain,
            )
        } else {
            InputEvent::KeySequence(payload)
        };
        // The hidden arm answers from the same receipt as the front arm: a short
        // or failed write is `ERR write failed`, never a false `OK` (which under
        // `id=` would make a retry a `dup=1`). It never parks this lane on the
        // program, and a paste also waits, bounded, for its bytes to leave the
        // spill (see `background_input`).
        match background_input_observed(&term, &ctx, &event, PASTE_RECEIPT_WAIT, completion()) {
            Ok(InputOutcome::Ok) => format!("OK {n} bytes\n"),
            outcome => control_input::input_reply_to_str(outcome),
        }
    });
    let response = crate::input_stall::with_warning(response, warning.as_deref());
    if writer.write_all(response.as_bytes()).is_err() {
        return false;
    }
    writer.flush().is_ok()
}

/// Fill one bounded authenticated binary frame, treating the liveness-poll
/// timeout as a retry tick; the filled prefix survives each tick.
fn read_exact_authenticated(reader: &mut impl Read, payload: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < payload.len() {
        match reader.read(&mut payload[filled..]) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => filled += read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Write an error line and flush, swallowing the I/O result. Returns `()` so it
/// can be used as `return write_err(writer, b"ERR ...\n");` in a `-> ()` fn.
fn write_err<W: Write>(writer: &mut W, msg: &[u8]) {
    let _ = writer.write_all(msg);
    let _ = writer.flush();
}

/// TEST-ONLY, for in-memory writers: its peer never goes, so a push loop it
/// starts ends only on a failed write. A test with a real socket serves it with
/// [`run_subscribe_socket`], whose read-side probe sees the client hang up even
/// while a sibling test's fork holds a copy of the client's end — a write then
/// still succeeds, and a loop waiting for one to fail never ends.
#[cfg(test)]
fn run_subscribe<W: Write>(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    writer: &mut W,
) {
    run_subscribe_with_peer_probe(line, active, store, subscribers, scope, writer, || false);
}

/// Run a production socket subscription with read-side disconnect detection,
/// then HANG THE CONNECTION UP: a push stream is the last thing a connection
/// carries, so however it ended, its client reads end-of-file here.
///
/// The protocol becomes push-only after the subscribe acknowledgement. Its client
/// MUST keep the write/send half open and send no more bytes; EOF, an unexpected
/// byte, or a read error therefore all mean the peer is gone.
/// [`subscription_peer_gone`] asks that before EVERY pass, without waiting —
/// see it for why a bounded wait there was a cost on every pushed frame.
///
/// The probe gets its own handle only because the loop holds `writer`
/// mutably for its lifetime. Nothing is configured on it: a clone is the same
/// socket (a Unix `dup` shares the open file description, a Windows clone the
/// `Arc`), so a receive timeout or `O_NONBLOCK` set on it would be set on the
/// writer too. That is why the probe's no-wait is a PER-CALL flag and never a
/// non-blocking socket: a non-blocking writer would turn a slow reader's full
/// buffer into a write error, and a stream that ended on it.
///
/// A client that half-closed (`printf 'subscribe …\n' | nc -U`) is gone to the
/// protocol but still reading. The hang-up shuts each half on its own
/// ([`aterm_uds::hangup::hang_up`]): Darwin refuses a both-ways `shutdown` of a
/// socket whose read half that half-close shut, and shuts NOTHING, so such a
/// client read its end-of-file only when the last copy of this end closed — and
/// a child any thread forked holds one until it execs.
fn run_subscribe_socket(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    writer: &mut CtlStream,
) {
    if let Ok(probe) = writer.try_clone() {
        run_subscribe_with_peer_probe(line, active, store, subscribers, scope, writer, move || {
            subscription_peer_gone(&probe)
        });
    }
    aterm_uds::hangup::hang_up(writer);
}

/// Whether a push-only subscriber is gone, asked WITHOUT WAITING: any byte, EOF
/// or read error is gone (its client sends nothing after `subscribe`, so even a
/// write-half close is a disconnect), and a quiet open socket is there.
///
/// THE PROBE RUNS BEFORE EVERY PASS, the real wakes included, so a probe that
/// waits is a wait on every frame pushed. It used to be a `read` under a 1 ms
/// receive timeout, and a quiet subscriber — every live one — made it sleep the
/// whole timeout out: every `EVENT` and `DELTA` to every subscriber, the fabric
/// bridge's `EVENT post` on the post → visible path included, left about
/// 1.1 ms late. Measured 2026-10-16, interleaved, load 43–63: a notify until
/// the subscriber has read the whole DELTA, p50 1.15 ms → 0.020 ms (n = 800
/// each). The fabric bench's post → visible gained that one hop and no more,
/// on the tree this probe landed on: re-measured by review with arms that
/// differ only in this probe (f35f017e56 → df40e5055b), 13 runs of 100 posts a
/// side, the Strict-publish yardstick ≈ 4.2 ms in both, the median of the
/// runs' p50s went 6.79 → 5.64 ms. Its runs fall in two modes by post →
/// durable (≈ 5.1 or ≈ 4.2 ms), and an earlier 6.51 → 4.54 ms quoted here was
/// the faster mode's, not twice this hop. On main as of ffd191e439 the hop no
/// longer shows in that bench: with the sleep, without it, and with the push
/// loop's pass budget, 4 interleaved runs each (load 5–17), the medians of
/// the runs' p50s are 5.49, 5.27 and 5.02 ms, inside the same two-mode spread.
///
/// The answer the bound waited for is already in the socket — a queued byte
/// or EOF is there to see at once — but the wait did one other job: it was
/// the push loop's only coalescing window, so a flood of output wakes was
/// folded into at most one pass a millisecond. That job is the push loop's
/// own now, done without delaying a wake that has room
/// ([`crate::subscribe`]'s pass budget, `Pacer`, has the flood's numbers).
///
/// The no-wait is guarded twice: the push thread's tally judges this probe's
/// one read by its flags (`push_probe_tally`), and a test counts the
/// kernel's own syscalls to hold the whole probe to that one entry, so a wait
/// put beside the read is caught too.
///
/// Unix peeks with the per-call flag pair `MSG_PEEK | MSG_DONTWAIT`: nothing
/// consumed, no blocking mode set on a socket the writer shares
/// ([`run_subscribe_socket`] says why that matters). Windows has no per-call
/// flag and needs none — its socket is already non-blocking — so it takes one
/// `recv` without the park its `read` adds; that consumes a byte, which ends
/// this stream anyway.
fn subscription_peer_gone(stream: &CtlStream) -> bool {
    match peek_push_peer(stream) {
        Ok(_) => true,
        Err(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::Interrupted
        ),
    }
}

/// The flags of the push probe's one `recv`: look, never take, never wait.
#[cfg(unix)]
const PUSH_PROBE_FLAGS: libc::c_int = libc::MSG_PEEK | libc::MSG_DONTWAIT;

/// One non-blocking look at a push subscriber's socket: `Ok(n)` with a byte
/// queued (n = 1) or at EOF (n = 0), `WouldBlock` for a quiet open peer.
#[cfg(unix)]
fn peek_push_peer(stream: &CtlStream) -> std::io::Result<usize> {
    use std::os::fd::AsRawFd as _;

    let fd = stream.as_raw_fd();
    let mut byte = 0_u8;
    // SAFETY: `stream` is borrowed for the call, so its fd is open; the buffer
    // is one owned byte, and `MSG_PEEK | MSG_DONTWAIT` neither consumes nor
    // blocks.
    let n = unsafe { libc::recv(fd, (&raw mut byte).cast(), 1, PUSH_PROBE_FLAGS) };
    // Read errno before the tally's own syscall can overwrite it.
    let answer = usize::try_from(n).map_err(|_| std::io::Error::last_os_error());
    #[cfg(test)]
    push_probe_tally::record(fd, PUSH_PROBE_FLAGS);
    answer
}

#[cfg(not(unix))]
fn peek_push_peer(stream: &CtlStream) -> std::io::Result<usize> {
    let mut byte = [0_u8; 1];
    stream.read_nonblocking(&mut byte)
}

/// TEST-ONLY: what the push probe asked the kernel on THIS thread — a push
/// loop runs on the thread that serves its subscription, so a test reads its
/// own loop's tally whatever sibling tests' loops do. A probe read is PARKING
/// when the kernel may sleep in it: its flags lack `MSG_DONTWAIT` and the
/// descriptor is blocking (`O_NONBLOCK` clear), whatever receive timeout
/// bounds the sleep. Judged from the call's own flags and the descriptor's
/// state at the call, so a probe that dropped `MSG_DONTWAIT`, or ran on a
/// blocking socket under a timeout as it once did, counts.
///
/// IT SEES ONLY THE READ IT IS ATTACHED TO, and is told that read's flags by
/// the read's own constant: a wait added BESIDE the read — a bounded `poll`
/// before the peek, a timed `read` next to it — tallies nothing here. That
/// half is counted by the kernel, not by this: `the_push_probe_enters_the_kernel_once`
/// holds the whole probe to one kernel entry, the read this judges.
#[cfg(all(test, unix))]
pub(crate) mod push_probe_tally {
    use std::cell::Cell;

    /// Probe reads on this thread, and how many of them could park.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) struct Tally {
        pub(crate) reads: u64,
        pub(crate) parking: u64,
    }

    thread_local! {
        static TALLY: Cell<Tally> = const { Cell::new(Tally { reads: 0, parking: 0 }) };
    }

    /// Count one probe read made on `fd` with `flags`.
    ///
    /// A read flagged `MSG_DONTWAIT` cannot park whatever the descriptor's
    /// state, so only a read without it asks the descriptor — which keeps the
    /// production probe's kernel entries, which
    /// `the_push_probe_enters_the_kernel_once` counts, to its `recv` alone in a
    /// test build too.
    pub(crate) fn record(fd: std::os::fd::RawFd, flags: libc::c_int) {
        let parks = flags & libc::MSG_DONTWAIT == 0 && {
            // SAFETY: `F_GETFL` reads the descriptor's status flags and writes
            // nothing; the caller's descriptor is open for the call.
            let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            status >= 0 && status & libc::O_NONBLOCK == 0
        };
        TALLY.with(|tally| {
            let mut now = tally.get();
            now.reads += 1;
            now.parking += u64::from(parks);
            tally.set(now);
        });
    }

    /// This thread's tally since its last `take`, reset to zero.
    pub(crate) fn take() -> Tally {
        TALLY.with(|tally| tally.replace(Tally::default()))
    }
}

/// The `subscribe` usage refusal, built from
/// [`aterm_types::control_verbs::SUBSCRIBE_STREAMS`] so every refusal path lists
/// the same streams.
fn subscribe_usage_bytes() -> Vec<u8> {
    format!(
        "ERR usage: subscribe [@<sel>[,<sel>]|@*] <streams> [since=<seq>] [since-turn=<id>] \
         [since-block=<id>] [since-log=<seq> log-ep=<e>] [every-frame]\n  <streams>: a subset \
         of {} and must name at least one frame source; mail takes mail:kinds=<k,..>, \
         mail:from=<class|principal> and/or mail:topic=<t>; log takes \
         log:lvl=<session|run|turn|step> and/or log:class=turn\n",
        aterm_types::control_verbs::SUBSCRIBE_STREAMS.join(",")
    )
    .into_bytes()
}

/// Authorize a `subscribe` request and, on success, flip the connection to push
/// mode (the push loop never returns to request polling).
///
/// Grammar: `subscribe [@<sel>[,<sel>...]|@*] <streams> [since=<seq>]
/// [since-turn=<id>] [since-block=<id>] [every-frame]`, where `<streams>` is one
/// comma-separated token from `aterm_types::control_verbs::SUBSCRIBE_STREAMS`
/// plus the `timestamps`/`ts` and `trim` modifiers.
///
/// Per-target streams are gated per `@<sel>` like a read verb (`ReadScreen` via
/// [`resolve_explicit`] and [`cross_session_authorized`]); the instance-scoped
/// `sessions` stream and `@*` are Owner-only. Any failure writes one `ERR ...`
/// and closes without a partial subscription; on success the socket and the
/// `OK subscribe <n>` ack go to [`subscribe::push_loop_with_peer_probe`].
pub(crate) fn run_subscribe_with_peer_probe<W: Write, P: FnMut() -> bool>(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    writer: &mut W,
    peer_gone: P,
) {
    let line = line.strip_suffix('\r').unwrap_or(line);
    // One usage text for every refusal: the selector list (or `@*`) is optional
    // and the trailing args are exactly what the parser accepts.
    let usage = subscribe_usage_bytes();
    let usage = usage.as_slice();
    // Strip the verb; the remainder is `[@<sel>[,<sel>...]] <streams> [args]`.
    let rest = match line.split_once(' ') {
        Some(("subscribe", r)) => r.trim(),
        _ => {
            let _ = writer.write_all(usage);
            let _ = writer.flush();
            return;
        }
    };
    let mut it = rest.split_whitespace();
    let Some(first) = it.next() else {
        let _ = writer.write_all(usage);
        let _ = writer.flush();
        return;
    };
    // Default to self: a first token that is a valid stream list, not an `@<sel>`
    // list, means `@.`.
    let (sel_tok, stream_tok) = if first.starts_with('@') {
        match it.next() {
            Some(s) => (first, s),
            None => {
                let _ = writer.write_all(usage);
                let _ = writer.flush();
                return;
            }
        }
    } else if Requested::parse(first).is_some() {
        ("@.", first)
    } else {
        let _ = writer.write_all(usage);
        let _ = writer.flush();
        return;
    };
    // Trailing args: `since=<seq>` (content_seq resume), `since-turn=<id>` /
    // `since-block=<id>` (events resume), `every-frame` (re-emit `cells` every
    // wake).
    let mut since: Option<u64> = None;
    let mut since_turn: Option<u64> = None;
    let mut since_block: Option<u64> = None;
    // `since-log=<seq> log-ep=<e>` (phase 3A): the `log` stream's resume anchor,
    // a session-log cursor (a seq means nothing without its epoch), never
    // `since=`, which is the content sequence's.
    let mut since_log: Option<u64> = None;
    let mut log_ep: Option<u32> = None;
    let mut non_coalesced = false;
    for tok in it {
        let parse_u64 = |v: &str| v.parse::<u64>().ok();
        if let Some(v) = tok.strip_prefix("since-log=") {
            match parse_u64(v) {
                Some(n) => since_log = Some(n),
                None => return write_err(writer, b"ERR bad since-log\n"),
            }
        } else if let Some(v) = tok.strip_prefix("log-ep=") {
            match v.parse::<u32>().ok() {
                Some(n) => log_ep = Some(n),
                None => return write_err(writer, b"ERR bad log-ep\n"),
            }
        } else if let Some(v) = tok.strip_prefix("since=") {
            match parse_u64(v) {
                Some(n) => since = Some(n),
                None => return write_err(writer, b"ERR bad since\n"),
            }
        } else if let Some(v) = tok.strip_prefix("since-turn=") {
            match parse_u64(v) {
                Some(n) => since_turn = Some(n),
                None => return write_err(writer, b"ERR bad since-turn\n"),
            }
        } else if let Some(v) = tok.strip_prefix("since-block=") {
            match parse_u64(v) {
                Some(n) => since_block = Some(n),
                None => return write_err(writer, b"ERR bad since-block\n"),
            }
        } else if tok == "every-frame" {
            non_coalesced = true;
        } else {
            return write_err(writer, b"ERR unknown subscribe arg\n");
        }
    }

    let Some(req) = Requested::parse(stream_tok) else {
        let _ = writer.write_all(usage);
        let _ = writer.flush();
        return;
    };
    let log_since = match (since_log, log_ep) {
        (None, None) => None,
        (Some(seq), Some(ep)) => Some(aterm_slog::Cursor::new(ep, seq)),
        _ => {
            return write_err(writer, b"ERR usage: since-log= and log-ep= go together\n");
        }
    };

    // Instance-scoped authority, decided once: the `sessions` stream diffs the
    // whole roster, revealing siblings no per-target grant covers, so it is
    // Owner-only. Refused hard (a push connection that never pushes looks like a
    // hang). `InstanceStreams::authorize` enforces the same rule at the type
    // level.
    if req.instance.sessions && !scope.is_owner_class() {
        log_denial(
            AUDIT_SUBSYSTEM,
            "subscribe -> sessions",
            aterm_containment::mode_or_containment(),
            "the sessions stream is instance-wide; only Owner holds instance authority",
        );
        return write_err(writer, b"ERR denied\n");
    }
    let instance = InstanceStreams::authorize(req.instance, scope);

    // `@*`: the live instance target set, including sessions created later, which
    // the push loop adopts and acks with `sub <local> <sid>`. One connection
    // follows every tab instead of one subscription per tab, which would exhaust
    // `CONTROL_SUBSCRIPTION_WORKERS`.
    //
    // Owner-only like the `sessions` stream (it reveals siblings), refused hard.
    // `AdoptScope::authorize` enforces the same rule at the type level.
    let adopt_all = sel_tok == "@*";
    if adopt_all && !scope.is_owner_class() {
        log_denial(
            AUDIT_SUBSYSTEM,
            "subscribe -> @*",
            aterm_containment::mode_or_containment(),
            "the @* live target set is instance-wide; only Owner holds instance authority",
        );
        return write_err(writer, b"ERR denied\n");
    }
    // Resume anchors are per-session watermarks and a live target set is
    // multi-target, so refuse them with `@*` up front.
    if adopt_all
        && (since.is_some() || since_turn.is_some() || since_block.is_some() || log_since.is_some())
    {
        return write_err(
            writer,
            b"ERR resume anchors (since=/since-turn=/since-block=/since-log=) require a single \
              target\n",
        );
    }
    let adopt = subscribe::AdoptScope::authorize(adopt_all, scope);

    // Resolve and gate every `@<sel>` in the list (self follows the active tab).
    // The first bad selector aborts the whole subscribe, audited like a
    // cross-session read denial.
    let mut targets: Vec<subscribe::ResolvedTarget> = Vec::new();
    // Channel-id → stable-sid map for the ack, so clients can map compact
    // `<local>` frame tags back to sids.
    let mut sub_map: Vec<(u64, String)> = Vec::new();
    let mut parsed = 0usize;
    // `@*` seeds from the current roster (a snapshot, guard dropped at once) as
    // real `@<sid>` selectors, so they go through the same resolve, `ReadScreen`
    // gate, de-dup and cap loop as an explicit list.
    //
    // At most `MAX_SUBSCRIBE_TARGETS`: the cap defers adoption rather than
    // refusing (`pick_adoptions`). Leftover sessions are adopted and acked as
    // slots free (the adoption watermark starts at zero), and are not announced
    // because they are in the `sessions` baseline.
    //
    // The roster journal's seq is read under the same guard: a session it
    // records as Created after that seq was not in this snapshot, so it is news
    // to the subscription, and its `log` stream reads from its log's start
    // (`PushOptions::adopt_after`).
    let (expanded, adopt_after): (Vec<String>, Option<u64>) = if adopt_all {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        (
            g.live_handles()
                .take(MAX_SUBSCRIBE_TARGETS)
                .map(|h| format!("@{}", h.sid.as_str()))
                .collect(),
            Some(g.roster_seq()),
        )
    } else {
        (
            sel_tok
                .split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            None,
        )
    };
    for raw in expanded.iter().map(String::as_str) {
        // Bound the selector list so a duplicate-laden / oversized comma list cannot
        // fan out into thousands of ByteFanout slots (O(N) tee per output burst).
        parsed += 1;
        if parsed > MAX_SUBSCRIBE_TARGETS {
            let _ = writer.write_all(b"ERR too many subscribe targets\n");
            let _ = writer.flush();
            return;
        }
        let Some(body) = raw.strip_prefix('@') else {
            let _ = writer.write_all(b"ERR usage: targets are @<sel> (e.g. @., @1, @s-...)\n");
            let _ = writer.flush();
            return;
        };
        let sel = Selector::parse(body);
        let target = match &sel {
            Selector::SelfTok => Ok(resolve_active(active)),
            Selector::Local(_) | Selector::Sid(_) => resolve_explicit(store, &sel),
        };
        // A subscribe is a READ, and an ambiguous one streams a pane's output
        // to a caller who asked for a different instance's pane. Refuse.
        let target = match target {
            Ok(target) => target,
            Err(refusal) => {
                let _ = writer.write_all(refusal.as_bytes());
                let _ = writer.flush();
                return;
            }
        };
        let Some((term, _master, local_id, ctx)) = target else {
            let error = if matches!(sel, Selector::SelfTok) {
                NO_ACTIVE_TERMINAL
            } else {
                "ERR no such session\n"
            };
            let _ = writer.write_all(error.as_bytes());
            let _ = writer.flush();
            return;
        };
        // Authorize like a read (`ReadScreen`) with `cross_session_authorized`:
        // Owner holds; an Edge, for `@.` too, needs a `decide_edge` token for the
        // resolved target, so a moved `@.` grants nothing.
        if !scope.is_owner_class() && !cross_session_authorized(scope, "subscribe", &ctx) {
            log_denial(
                AUDIT_SUBSYSTEM,
                &format!("subscribe -> {}", ctx.self_id.as_str()),
                aterm_containment::mode_or_containment(),
                "no authorizing edge for cross-session subscribe",
            );
            let _ = writer.write_all(b"ERR denied\n");
            let _ = writer.flush();
            return;
        }
        // De-dup by session: a repeated selector (`@.,@.`) or two selectors that
        // resolve to the same session must not install a second fanout slot.
        if targets.iter().any(|(id, _, _, _, _, _)| *id == local_id) {
            continue;
        }
        sub_map.push((local_id, ctx.self_id.as_str().to_string()));
        targets.push((
            local_id,
            term,
            ctx.byte_fanout.clone(),
            ctx.turns.clone(),
            ctx.timeline.clone(),
            ctx.fabric.clone(),
        ));
    }
    // An empty set is an error for an explicit list but normal for `@*`.
    if targets.is_empty() && !adopt_all {
        let _ = writer.write_all(b"ERR usage: at least one @<sel> target\n");
        let _ = writer.flush();
        return;
    }

    // Resume anchors live in one session's monotonic space, so they are only
    // valid for a single-target subscription.
    if targets.len() > 1
        && (since.is_some() || since_turn.is_some() || since_block.is_some() || log_since.is_some())
    {
        return write_err(
            writer,
            b"ERR resume anchors (since=/since-turn=/since-block=/since-log=) require a single \
              target\n",
        );
    }

    // Authorized: ack with `OK subscribe <n>` plus one `sub <local> <sid>` line
    // per target, then flip to push-only. The push loop writes the ack after
    // seeding every watch it names (`subscribe::push_loop_with_peer_probe`).
    let mut ack = format!("OK subscribe {}\n", targets.len());
    for (local_id, sid) in &sub_map {
        ack.push_str(&format!("sub {local_id} {sid}\n"));
    }
    subscribe::push_loop_with_peer_probe(
        subscribers,
        store,
        &targets,
        PushScopes {
            streams: req.targets,
            instance,
            adopt,
        },
        PushOptions {
            since,
            since_turn,
            since_block,
            non_coalesced,
            timestamps: req.timestamps,
            mail: req.mail.clone(),
            log: req.log,
            log_since,
            adopt_after,
        },
        &ack,
        writer,
        peer_gone,
    );
}

/// A resolved cross-session target: the same `(term, master, id, ctx)` shape as
/// `resolve_active`, for an `@<selector>` session. Cloned out of the [`Store`]
/// before the guard drops, so the registry lock is never held across a
/// `Terminal` lock.
type Target = (Arc<Mutex<Terminal>>, i32, u64, Arc<SessionCtx>);

/// A parsed `@<selector>`. `SelfTok` (`@.` or bare `@`) names the connection's
/// own session; `Local`/`Sid` name a specific one. An unknown id resolves to
/// `None` (`ERR no such session`), never to a wrong session.
enum Selector {
    /// `@.` — the connection's own session (explicit self; degenerates to the
    /// verbatim self path).
    SelfTok,
    /// `@<u64>` — by the process-local `Session.id`.
    Local(u64),
    /// `@s-<hex>` / `@<sid>` — by stable `SessionId`.
    Sid(SessionId),
}

impl Selector {
    /// Parse the body after the leading `@`: `.` or empty is self, all digits is a
    /// local id, anything else is taken verbatim as a `SessionId` string (the `s-<hex>`
    /// wire id `whoami`/`sessions` report).
    fn parse(body: &str) -> Selector {
        if body.is_empty() || body == "." {
            Selector::SelfTok
        } else if let Ok(n) = body.parse::<u64>() {
            Selector::Local(n)
        } else {
            Selector::Sid(SessionId::new(body))
        }
    }
}

/// The refusal an `@<sid>` earns when this instance hosts it and another live instance
/// also answers to it: an ambiguous id is an error, never a delivery.
///
/// Two instances sharing one `s-…` share one `graph/<sid>` entry, and the id alone
/// cannot say which window a caller meant, so a keystroke could land in a stranger's
/// terminal. `identity_claim` prevents minting duplicates; this covers an existing
/// one (e.g. an instance from a build without that gate).
///
/// Deterministic and one-sided: the instance the entry names keeps serving, so an
/// address that resolves keeps resolving to exactly one place, and the answer names
/// the pid to address instead.
///
/// An entry naming this process's own seamless-update candidate
/// (`identity_claim::register_handoff_candidate`) is not a second holder. Successors
/// publish only at the Commit (`publish_when_committed`); this exemption is the second
/// layer for a successor that publishes early (see `identity_claim`'s module header,
/// "The other order"; the model is `HandoffAddressOwner`).
///
/// Cost: `@.` and `@<local>` never reach here (process-local), and an id this process
/// minted is answered from an in-memory register (one lock read and a compare). Only
/// an adopted id (from an outer aterm's premint or a handoff, a handful per instance)
/// reads `graph/<sid>` and calls `kill(pid, 0)`, on the order of the request's own
/// socket round trip.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "HandoffAddressOwner",
        action = "Request",
        project = "aterm_gui::identity_claim::project_handoff_address_owner"
    )
)]
fn ambiguous_sid_refusal(sel: &Selector) -> Option<String> {
    let Selector::Sid(sid) = sel else {
        return None;
    };
    let pid = crate::identity_claim::live_holder(sid)?;
    Some(format!(
        "ERR ambiguous session id {} also served by pid {pid}; use aterm ctl --pid {pid}\n",
        sid.as_str()
    ))
}

/// Whether a cross-session call (target != the connection's own session) is authorized
/// for `verb`. Default deny.
///
/// * `Owner` — the per-instance launcher god token (same-uid + per-launch token).
///   It is the process's root authority, so it may reach any SIBLING session in
///   the same process (the same trust domain). Still subject to `required_op`
///   being defined for the verb; privilege/identity verbs (`grant`/`revoke`/
///   `whoami`/`sessions`) are self-scoped and handled BEFORE this gate.
/// * `Edge(presented)` — a scoped connection must present a token that
///   `decide_edge` PERMITS against the TARGET's edge table, for the verb's exact
///   required op, bound to the TARGET's CURRENT launch nonce. A token authorizing
///   session B grants nothing toward session C (each hop is an independent
///   point-lookup); a target restarted under a reused id (nonce mismatch) fails
///   closed and is audited.
pub(crate) fn cross_session_authorized(scope: Scope, verb: &str, target_ctx: &SessionCtx) -> bool {
    let Some(need) = required_op(verb) else {
        // No op class (privilege/identity/unknown verb): never cross-session.
        return false;
    };
    // Owner reaches any sibling (same trust domain); a scoped Edge needs a
    // `decide_edge`-permitted token for `need` against the TARGET's table + nonce.
    scope_holds_op(scope, need, target_ctx)
}

/// The dispatch-gate authority for the whole `verb rest` line against `target_ctx`:
/// like [`cross_session_authorized`] but on the effective op, the base op escalated by
/// its argument for the indirect seams (`invoke <clipboard/config action>`,
/// `open prefs|settings`; see [`escalated_op`]). The escalated op replaces the base op,
/// so an edge granted the fine op (ClipboardWrite/ConfigWrite) passes while a plain
/// WriteInput edge cannot tunnel through. An [`Escalation`] may demand Owner-only (the
/// palette gateway, update-apply twins), which no Edge satisfies. Owner reaches self and
/// siblings; a `None` effective op (unknown verb) is denied.
fn dispatch_authorized(scope: Scope, verb: &str, rest: &str, target_ctx: &SessionCtx) -> bool {
    match escalated_op(verb, rest) {
        // OwnerOnly admits only the Owner class; a fine-op escalation is decided by
        // `scope_holds_op` like the base gate (Owner short-circuits there).
        Some(Escalation::OwnerOnly) => scope.is_owner_class(),
        Some(Escalation::Op(need)) => scope_holds_op(scope, need, target_ctx),
        // No escalation: fall back to the verb's base required op (None => unknown verb,
        // default-DENY).
        None => match required_op(verb) {
            Some(need) => scope_holds_op(scope, need, target_ctx),
            None => false,
        },
    }
}

/// The reply a scoped EDGE gets for the framebuffer `image` (every form but
/// `image read`). An edge authorizes ONE session, but the frame is a whole
/// window's: sibling split panes, every tab title in the strip, overlays and
/// the message band, none of which the edge covers. The bare form is worse:
/// it captures the window that is frontmost when the main thread renders, not
/// the session the gate checked. The renderer composes the window as a unit
/// (presented frames, deferred GPU captures, overlays spliced over the panes),
/// so there is no honest crop to the edge's pane. The edge still reads its
/// session through `image read`, `text` and `screen`, and Owner-class scopes
/// never reach this. `window`, `panes` and `video`, the App-target twins that
/// show the same window, are denied to an edge by `native_control_decision`.
fn image_frame_refusal(target_ctx: &SessionCtx) -> String {
    log_denial(
        AUDIT_SUBSYSTEM,
        &format!("image -> {}", target_ctx.self_id.as_str()),
        aterm_containment::mode_or_containment(),
        "the window frame exceeds a scoped edge's one session",
    );
    "ERR denied: image captures the whole window, beyond an edge token's one session; \
     use image read, text or screen\n"
        .to_string()
}

/// PART B — the input-injection verbs that reach the FRONT window through the proxy →
/// `App::input` seam and can therefore be SWALLOWED by (i.e. DRIVE) an open modal overlay
/// or native Settings tab, plus the direct-sink writers, fenced as a set for defense in
/// depth. `key`/`ctrl`/
/// `mouse`/`paste`/`focus` post a `Wake::Input` that `App::input` routes into the overlay
/// while one is modal; `send`/`feed`/`turn` write the PTY sink directly (they drive the
/// shell, not the overlay) but are refused too so a scoped edge cannot race input while a
/// human is mid-overlay. (`feed-bin` also writes the sink directly and is intercepted
/// before this dispatch WITHOUT a proxy, so it takes no overlay hop — it cannot drive the
/// overlay, only the shell.)
/// The session a `turn` is credited to ([`crate::Lease::Turn::driver`],
/// [`control_session::TurnIo::driver`]): for an edge-scoped connection, the source its
/// token was granted to in the resolved target's table (the lookup
/// [`control_session::caller_actor`] makes for the exits ledger); `None` for
/// Owner-class scopes, which name a connection, not a session.
fn turn_driver(scope: Scope, ctx: &SessionCtx) -> Option<SessionId> {
    match scope {
        Scope::Owner => None,
        #[cfg(any(unix, test))]
        Scope::Bridge => None,
        Scope::Edge(presented) => ctx
            .edges
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .src_of(&presented)
            .cloned(),
    }
}

/// Input verbs fenced while a modal overlay or native Settings tab is front:
/// `key`/`ctrl`/`mouse`/`paste`/`focus` reach it through `App::input`, and the
/// direct-sink writers `send`/`feed`/`turn` are fenced too so a scoped edge cannot race
/// input while a human is in the overlay. (`feed-bin` is intercepted earlier without a
/// proxy and can only drive the shell.)
fn is_front_driving_verb(verb: &str) -> bool {
    matches!(
        verb,
        "key" | "ctrl" | "mouse" | "paste" | "focus" | "send" | "feed" | "turn"
    )
}

/// What a scoped caller's front-surface observation hop came back with: the
/// surface, or the whole reply `handle` sends instead. A failed hop is an
/// authorization failure, never "no surface" (fail closed): it is audited
/// with `log_denial` and answered `ERR denied`. The one exception is a
/// stalled main thread's refusal (`ERR main thread stalled <N>s;
/// retry`). It was given before anything was posted, so nothing was observed
/// and nothing authorized either way, and it goes to the wire as it is, as it
/// does for every other main-thread verb. As `ERR denied` it would be a false
/// authority failure in the audit log, and the harness, which rides out the
/// stall refusal (aterm-agent's `CtlReply::lost`), would read it as the
/// server's final answer.
fn front_surface_authority(
    verb: &str,
    hop: Result<FrontControlSurface, control_media::MainHopError>,
) -> Result<FrontControlSurface, String> {
    match hop {
        Ok(surface) => Ok(surface),
        Err(control_media::MainHopError::Stalled(stall)) => Err(format!("ERR {stall}\n")),
        Err(error) => {
            log_denial(
                AUDIT_SUBSYSTEM,
                &format!("self {verb} front-surface authorization failed"),
                aterm_containment::mode_or_containment(),
                &format!("could not observe front input authority: {error}"),
            );
            Err("ERR denied\n".to_string())
        }
    }
}

/// Given the caller's `scope`, input `verb` and the front main-thread surface, the
/// escalation that replaces the verb's base WriteInput requirement: every overlay is
/// Owner-only (some bind clipboard/external-process actions), native Settings needs the
/// durable-config fine op, and Settings ▸ Harness is Owner-only (its session buttons
/// write Autopilot words). Owner skips the observation and satisfies either.
///
/// The Harness fence is the whole page, not the keys that would press a session
/// button: which key presses one depends on the page's focus and a click's hit,
/// state the observation does not replay — a Tab that only moves focus onto a
/// button is what makes the next Return a press.
///
/// Pure for unit testing; a failed hop is denied by the caller, never read as `None`.
fn front_drive_escalation(
    scope: Scope,
    verb: &str,
    surface: FrontControlSurface,
) -> Option<Escalation> {
    if scope.is_owner_class() || !is_front_driving_verb(verb) {
        return None;
    }
    match surface {
        FrontControlSurface::Overlay(_) | FrontControlSurface::HarnessSettings => {
            Some(Escalation::OwnerOnly)
        }
        FrontControlSurface::NativeSettings => Some(Escalation::Op(Op::ConfigWrite)),
        FrontControlSurface::None | FrontControlSurface::OtherNative => None,
    }
}

/// Post the connections repaint poke after a successful connection-authority act
/// (`connect`/`disconnect`/`grant`/`revoke`). The edge tables changed but no funnel
/// epoch moved, so the tab marks would otherwise lag until an unrelated strip refresh.
/// Keyed on the `OK` reply; fire-and-forget (a gone event loop only costs the repaint).
fn poke_connections_on_ok(proxy: &EventLoopProxy<Wake>, resp: String) -> String {
    if resp.starts_with("OK") {
        // Advance the freshness epoch before the wake: the refresh recomposes the tab
        // chrome through its cache gate, and `grant`/`revoke` change tables without
        // touching the record store, so this is their only bump. (`connect`/`disconnect`
        // already bumped; a double bump only re-misses the cache.)
        crate::connections::connections().bump_revision();
        let _ = proxy.send_event(Wake::ConnectionsChanged);
    }
    resp
}

fn front_drive_denial(surface: FrontControlSurface) -> String {
    match surface {
        FrontControlSurface::Overlay(kind) => format!(
            "ERR overlay {} open (owner-only while open)\n",
            kind.keyword()
        ),
        FrontControlSurface::NativeSettings => {
            "ERR native settings open (config-write authority required)\n".to_string()
        }
        FrontControlSurface::HarnessSettings => {
            "ERR native settings harness open (owner-only while open)\n".to_string()
        }
        FrontControlSurface::None | FrontControlSurface::OtherNative => "ERR denied\n".to_string(),
    }
}

// ── Cross-session input arms ───────────────────────────────────────────────
//
// These run on the control thread against a resolved `@<selector>` target, not the
// active tab, so there is no App UI/gesture/window state and no `Wake::Input`. They use
// the source-blind tracked seam and the engine's viewport/geometry APIs directly, with
// `(term, &ctx.sink)` resolved as `send`/`feed` do, after `cross_session_authorized`.

/// Cross-session `key`/`ctrl`/`paste`/`focus`: feed a pre-built [`InputEvent`] to the
/// source-blind seam on the target `(term, sink)`. The seam reads the target's modes
/// once and writes the encoded bytes to its sink (nothing under a suppressing mode, e.g.
/// focus reporting off), byte-identical to the active-tab seam for the same event.
/// `None` (a malformed line) maps to `err`.
///
/// The reply is [`background_input_observed`]'s: the write's receipt, like the front-session
/// arms and [`cross_mouse_apply`] (a short or failed write, a dead peer, is `ERR write
/// failed`), and a paste's `OK` also means every byte left the process-local spill for
/// the kernel.
fn cross_input(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: Option<InputEvent>,
    err: &str,
) -> String {
    cross_input_observed(term, ctx, ev, err, None)
}

fn cross_input_observed(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: Option<InputEvent>,
    err: &str,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> String {
    match ev {
        Some(ev) => {
            // This bypasses the App input seam, so an in-flight `video ... keys`
            // recording cannot log it; count the attempts so the recording can say so.
            // `focus` is not an attempt and contributes nothing.
            crate::note_unseamed_control_input(&ev);
            control_input::input_reply_to_str(background_input_observed(
                term,
                ctx,
                &ev,
                PASTE_RECEIPT_WAIT,
                completion,
            ))
        }
        None => err.to_string(),
    }
}

/// Raw `send`/`feed` bytes are terminal input: keep their unlogged-input/latency
/// accounting while routing the frame through the per-session echo receipt. Returns
/// [`background_input_observed`]'s verdict, so the reply can say `ERR write failed`.
#[cfg(test)]
fn cross_raw_input(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    bytes: Vec<u8>,
) -> Result<InputOutcome, String> {
    cross_raw_input_observed(term, ctx, bytes, None)
}

fn cross_raw_input_observed(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    bytes: Vec<u8>,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> Result<InputOutcome, String> {
    crate::metrics::note_input();
    crate::note_unseamed_control_bytes(&bytes);
    background_input_observed(
        term,
        ctx,
        &InputEvent::KeySequence(bytes),
        PASTE_RECEIPT_WAIT,
        completion,
    )
}

/// How long a background paste's reply waits for its bytes to reach the program
/// before answering [`PASTE_UNREAD`]: the wait family's default
/// (`aterm_control::wire::WAIT_DEFAULT_MS`).
const PASTE_RECEIPT_WAIT: std::time::Duration =
    std::time::Duration::from_millis(aterm_control::wire::WAIT_DEFAULT_MS);

/// A background write refused at the session's input cap (the sink's spill, or its
/// ordered writer's queue): nothing was written. `ERR busy`, so a driver's back-off
/// retries it and an `id=` mark is given back (`pty_idem`).
const INPUT_QUEUE_FULL: &str =
    "ERR busy input-queue-full: nothing written (the program has not read its queued input)\n";

/// A background paste whose bytes are still queued for a program that has not read
/// them when its reply's wait ends: in doubt, never `ERR busy` (a retry would type it
/// twice). The program gets them when it reads; `signal` drops them.
const PASTE_UNREAD: &str =
    "ERR in-doubt paste-unread: queued, the program has not read it yet (signal drops it)\n";

/// One control-lane write into a session that is not on screen, and its reply's
/// outcome (`Err` is the reply line itself).
///
/// The write takes the App's hidden-session route
/// ([`crate::app_input::paste_order::unparked_observed`]), so it never parks the lane on the
/// program. These arms used the blocking egress until 2026-10-14, and a lane that
/// wrote into a program that had stopped reading waited for it with no bound: eight
/// `@<sid> paste`s into one stuck background tab took every request lane, and every
/// other client — `@<sid> signal`, the remedy, included — was answered busy for as
/// long as the program did not read. Refused at the session's input cap:
/// [`INPUT_QUEUE_FULL`], nothing written.
///
/// A paste's `OK` stays a kernel receipt (see [`background_reply`]); a paste handed
/// to the session's ordered writer (behind input queued there, or on Windows)
/// answers on acceptance, as the front tab's paste does.
#[cfg(test)]
fn background_input(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: &InputEvent,
    limit: std::time::Duration,
) -> Result<InputOutcome, String> {
    background_input_observed(term, ctx, ev, limit, None)
}

fn background_input_observed(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: &InputEvent,
    limit: std::time::Duration,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> Result<InputOutcome, String> {
    let discards = ctx.sink.discards();
    let receipt = crate::app_input::paste_order::unparked_observed(
        term,
        &ctx.modes,
        &ctx.sink,
        &ctx.output_echo,
        ev,
        completion,
    );
    background_reply(
        &ctx.sink,
        receipt,
        matches!(ev, InputEvent::Paste(..)).then_some(discards),
        limit,
    )
}

/// Whether a background write was refused at the session's input cap with
/// nothing written: [`INPUT_QUEUE_FULL`]. A wheel writes a report per notch,
/// and one cut short at the cap after its first notches went is a failed
/// write, never "nothing written" (a retry would scroll those notches twice).
const fn refused_unwritten(receipt: crate::input::EgressReceipt) -> bool {
    receipt.input_queue_full() && receipt.accepted_order().is_none()
}

/// The reply verdict for one background (control-lane) write: the seam's receipt
/// through [`crate::app_input::egress_to_outcome`], so a short or failed write is
/// `WriteFailed`, and a refusal at the input cap with nothing written is
/// [`INPUT_QUEUE_FULL`]. With `kernel_receipt` (a `paste` / `paste-bin`, whose `OK`
/// is a delivered-turn receipt: the sink's discard count before the write), the
/// process-local spill must also drain to the kernel: a frame spilled behind a
/// wedged foreground reports `Delivery::Full`, and if the peer dies first the drainer
/// discards it; the sink's completion fence keeps that as a sticky failure, answered
/// `ERR write failed`. So is a spill `signal` (or `reset flush`) dropped meanwhile:
/// the discard count moved, so some of the paste may never reach the program, and
/// the fence, which reads an emptied spill as drained, cannot tell. Keystroke-shaped
/// input keeps the accepted-for-delivery contract.
///
/// That wait is the request's own, on a wait lane (`is_wait_request` classes a paste
/// into another session as one), and bounded: it ends when its caller hangs up
/// ([`HUNG_UP_REPLY`]) and after `limit` ([`PASTE_UNREAD`], the bytes still queued).
fn background_reply(
    sink: &aterm_session::sink::SinkWriter,
    receipt: crate::input::EgressReceipt,
    kernel_receipt: Option<u64>,
    limit: std::time::Duration,
) -> Result<InputOutcome, String> {
    if refused_unwritten(receipt) {
        return Err(INPUT_QUEUE_FULL.to_string());
    }
    let outcome = crate::app_input::egress_to_outcome(receipt.egress);
    let Some(discards) = kernel_receipt else {
        return Ok(outcome);
    };
    if outcome != InputOutcome::Ok || receipt.is_queued() {
        return Ok(outcome);
    }
    let now = std::time::Instant::now();
    let deadline = now.checked_add(limit).unwrap_or(now);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match sink.wait_egress_drained_to_kernel_for(hangup_park(left)) {
            Some(true) if sink.discards() == discards => return Ok(InputOutcome::Ok),
            Some(_) => return Ok(InputOutcome::WriteFailed),
            None if caller_hung_up() => return Err(HUNG_UP_REPLY.to_string()),
            None if left.is_zero() => return Err(PASTE_UNREAD.to_string()),
            None => {}
        }
    }
}

/// Should an input verb go through the App INPUT SEAM PINNED to the session it
/// was authorized against (`Wake::Input { session: Some(target) }`), rather than
/// the background egress or the flagless follow-the-front post?
///
/// An explicit selector (`is_cross`) is pinned exactly when it names the tab
/// currently on screen. A named session that is NOT on screen keeps the direct,
/// round-trip-free egress the background path exists for. The middle case —
/// naming the tab you are looking at, which is what the documented `@self`
/// selector expands to — is the one that was silently taking the background
/// path and losing every side effect the seam owns (blink reset, viewport snap,
/// selection clear, the typing-momentum hints every cursor effect reads, and
/// the `video ... keys` ledger).
///
/// A flagless request is already on the seam. An OWNER-class caller's stays
/// unpinned (`session: None`): its bare form follows the front, whatever tab or
/// overlay holds it when the event loop applies it. A SCOPED (Edge) caller's
/// is pinned: its token was checked against the session active on the CONTROL
/// thread, and an unpinned post would be re-resolved on the main thread to
/// whatever tab, window or overlay (the Owner-only palette) became front in
/// between — the edge's keystrokes landing in a session it holds no edge over.
/// Pinned, they reach the authorized session (on its hidden path if it left
/// the front) and never an overlay.
///
/// Pure for testing; the event loop re-checks before acting, so a stale `front_active`
/// can only cost a path, never a mis-delivery.
const fn front_routed(
    is_cross: bool,
    owner_class: bool,
    front_active: Option<u64>,
    target: u64,
) -> bool {
    if !is_cross {
        return !owner_class;
    }
    match front_active {
        Some(front) => front == target,
        None => false,
    }
}

/// Execution seam for the two input phases of the composite `turn` verb, one decision
/// so paste and Return cannot take different paths when a selector names the visible
/// tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnInputRoute {
    Front,
    Background,
    Local,
}

const fn turn_input_route(is_cross: bool, targets_front: bool) -> TurnInputRoute {
    if targets_front {
        TurnInputRoute::Front
    } else if is_cross {
        TurnInputRoute::Background
    } else {
        TurnInputRoute::Local
    }
}

/// The front-routed twin of [`cross_input`]: an explicit `@<sid>` resolved to the tab on
/// screen goes through the App input seam (`Wake::Input` carrying the session) rather
/// than straight to the sink, so it behaves like a flagless drive (blink reset,
/// viewport snap, selection clear, typing-momentum hints, `video ... keys` ledger).
/// Authority is unchanged (the cross-session gate already ran). The event loop
/// re-resolves the session, so a racing tab switch lands on the hidden path for the
/// named session rather than typing into whatever tab won.
fn front_routed_input(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    ev: Option<InputEvent>,
    err: &str,
) -> String {
    front_routed_input_observed(proxy, session, ev, err, None)
}

fn front_routed_input_observed(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    ev: Option<InputEvent>,
    err: &str,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> String {
    match ev {
        Some(ev) => control_input::input_reply_to_str(post_input_reply_to_observed(
            proxy,
            vec![ev],
            Some(session),
            completion.map(|c| c.into_mark()),
        )),
        None => err.to_string(),
    }
}

fn local_input_observed(
    proxy: &EventLoopProxy<Wake>,
    ev: Option<InputEvent>,
    err: &str,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> String {
    match ev {
        Some(ev) => control_input::input_reply_to_str(post_input_reply_to_observed(
            proxy,
            vec![ev],
            None,
            completion.map(|c| c.into_mark()),
        )),
        None => err.to_string(),
    }
}

/// Close the cursor-move licence before a visible-front control operation that does not
/// pass through `App::input`. The main-thread wake expires only the two key-hint stamps
/// (unlike an empty synthetic key, it cannot heat cadence/rain, stamp input latency or
/// write a PTY frame), and completes before the direct operation, so a signal cannot
/// trigger output that borrows the older licence.
fn front_routed_license_clear(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    keep_hand: bool,
) -> String {
    license_clear_reply(control_media::call_main(proxy, |reply| {
        Wake::CursorMoveLicenseClear {
            session,
            keep_hand,
            reply,
        }
    }))
}

fn license_clear_reply(result: Result<bool, control_media::MainHopError>) -> String {
    match result {
        // `false` is a safe tab-switch race: the target is no longer visible and holds no
        // licence, and the authorized direct operation still proceeds.
        Ok(_) => "OK\n".to_string(),
        Err(error) => format!("ERR input dispatch failed: {error}\n"),
    }
}

fn front_routed_resize(proxy: &EventLoopProxy<Wake>, session: u64, rest: &str) -> String {
    if let Some(event) = control_input::parse_resize_px(rest) {
        return match event {
            Err(usage) => usage,
            Ok(event) => front_routed_input(proxy, session, Some(event), "ERR\n"),
        };
    }
    let (rows, cols) = match control_input::parse_resize(rest) {
        Ok(size) => size,
        Err(error) => return error,
    };
    front_routed_input(
        proxy,
        session,
        Some(InputEvent::Resize {
            rows,
            cols,
            echo_to_window: true,
        }),
        "ERR\n",
    )
}

fn parse_scroll_intent(rest: &str) -> Result<ScrollIntent, String> {
    Ok(match rest.trim() {
        "" => ScrollIntent::By(0),
        "top" => ScrollIntent::Top,
        "bottom" => ScrollIntent::Bottom,
        "up" => ScrollIntent::Up,
        "down" => ScrollIntent::Down,
        "prev-prompt" => ScrollIntent::PrevPrompt,
        "next-prompt" => ScrollIntent::NextPrompt,
        n => ScrollIntent::By(n.parse::<i32>().map_err(|_| {
            "ERR usage: scroll <up|down|top|bottom|prev-prompt|next-prompt|N>\n".to_string()
        })?),
    })
}

fn front_routed_scroll(
    term: &Arc<Mutex<Terminal>>,
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    rest: &str,
) -> String {
    let intent = match parse_scroll_intent(rest) {
        Ok(intent) => intent,
        Err(error) => return error,
    };
    if let Err(error) =
        post_input_reply_to(proxy, vec![InputEvent::ScrollView(intent)], Some(session))
    {
        return error;
    }
    let t = term_lock(term);
    format!(
        "OK {} {}\n",
        t.grid().display_offset(),
        t.grid().scrollback_lines()
    )
}

/// Cross-session `mouse`: build the event via [`parse_mouse`] and feed it to the seam on
/// the target. When the target app is mouse-tracking, the seam writes the report to its
/// sink (`Egress::Reported`). When not (`Egress::TrackingOff`):
///   * a wheel (`wheel_lines > 0`) scrolls the target's viewport via `scroll_display`
///     (positive = toward history; `wheel_up` => +lines), like the active-tab fallback,
///     and asks the target tab to repaint with `Wake::redraw(session)`;
///   * a plain press/release/move is a no-op (a background session has no selection
///     UI to drive); the reply is still `OK`.
fn cross_mouse(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    session: u64,
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
) -> String {
    match cross_mouse_apply(term, ctx, rest) {
        // The viewport moved: ask the (possibly inactive) target tab to repaint, as
        // `cmd_select` does.
        Ok(true) => {
            let _ = proxy.send_event(Wake::redraw(session));
            "OK\n".to_string()
        }
        // Tracking-on report, or a no-op press/move: nothing to repaint.
        Ok(false) => "OK\n".to_string(),
        Err(e) => e,
    }
}

/// The proxy-independent core of [`cross_mouse`], testable without an
/// `EventLoopProxy`: parses and feeds the event to the target's seam and applies the
/// `TrackingOff` fallback. Returns `Ok(true)` when the target viewport moved (caller
/// repaints), `Ok(false)` for a reported event or a no-op, `Err(usage)` for a malformed
/// line.
fn cross_mouse_apply(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    rest: &str,
) -> Result<bool, String> {
    let ev = parse_mouse(rest)?;
    // The App's report route, which never parks this lane on the program
    // ([`background_input_observed`] has why).
    let receipt =
        crate::app_input::paste_order::report_ordered_or_inline(term, &ctx.modes, &ctx.sink, &ev);
    if refused_unwritten(receipt) {
        return Err(INPUT_QUEUE_FULL.to_string());
    }
    match receipt.egress {
        // Tracking ON but the PTY write failed: honest error, not a false OK.
        Egress::Reported(crate::input::Delivery::Failed) => Err("ERR write failed\n".to_string()),
        // Tracking ON: the seam already wrote the report to the target sink.
        Egress::Reported(_) => Ok(false),
        // Tracking off: only a wheel has a background fallback (move the viewport); a
        // plain press/release/move is a no-op.
        Egress::TrackingOff {
            wheel_lines,
            wheel_up,
            ..
        } if wheel_lines > 0 => {
            let delta = if wheel_up { wheel_lines } else { -wheel_lines };
            term_lock(term).scroll_display(delta);
            Ok(true)
        }
        Egress::TrackingOff { .. } => Ok(false),
    }
}

/// Cross-session `resize`: `Resize { echo_to_window: false }` confined to the target.
/// Not a full `apply_term_resize` (a window-level op over every session, the GPU
/// swapchain and `request_inner_size`); only the target's per-session artifacts:
/// `Terminal::resize`, `aterm_pty::resize` and the asciicast geometry record. The
/// active window and other sessions are untouched.
///
/// The `[t, "r", "<cols>x<rows>"]` record goes into the target's `ctx.cast` as a self
/// resize would, so `cast` shows no cross/self difference. `seam_egress` emits no bytes
/// for `Resize`, so the geometry effect is applied directly.
///
/// Reuses [`parse_resize`] for the `ERR out of range` / usage strings.
fn cross_resize(
    term: &Arc<Mutex<Terminal>>,
    master: i32,
    ctx: &SessionCtx,
    session: u64,
    proxy: Option<&EventLoopProxy<Wake>>,
    rest: &str,
) -> String {
    let (rows, cols) = match control_input::parse_resize(rest) {
        Ok(rc) => rc,
        Err(e) => return e,
    };
    // Rewrap scrollback off the `term` lock: the per-session mutex is contended by the
    // PTY reader and main thread, so the O(history) rewrap runs detach (brief lock) →
    // rewrap off-lock → re-attach (brief lock), inline since this is already off the
    // main thread. Seam policy as in the window path: under ConPTY conhost repaints the
    // target's viewport after this resize (`pty_resize_policy`), asked before
    // `term_lock` because it takes the PTY registry's lock.
    let policy = crate::app_render::pty_resize_policy(master);
    // The resize ledger: the engine's journal is copied inside the resize's hold and
    // booked below after every lock is released (`site=ctl-cross`).
    let (pending, journal, cast_stamp) = {
        let mut t = term_lock(term);
        // The target's rewrap gauge, booked inside the hold that detaches its history
        // as the window-resize hand-off books it, so search and scrollback into the
        // target meanwhile get the honest answer and the row: a `search` whose own
        // hold sees the history detached finds the job running or its count moved.
        let pending = t
            .resize_offloading_scrollback_with_policy(rows, cols, policy)
            .map(|pending| {
                let rewrap = ctx.rewrap_gauge.begin_job(pending.lines_total());
                (pending, rewrap)
            });
        // The crash-restore journal's resize word, in this hold, as the window path.
        ctx.crash_journal.host_resized(&t);
        (
            pending,
            crate::resize_ledger::JournalCopy::take(&t),
            crate::cast::ResizeStamp::take(&t),
        )
    };
    if let Some((pending, rewrap)) = pending {
        // The gauge's hold ends on every exit (a panic ends it `Failed` via `Drop`).
        // Stepped like the reflow worker (`drive_reflow_job_gauged`, content-identical
        // to `reflow()`) so the fill moves. No cancel: the rewrap runs to its end.
        let never = std::sync::atomic::AtomicBool::new(false);
        let mut ended = aterm_messages::WaitEnd::Done;
        // Guard the off-lock rewrap: a panic must not leave the target detached
        // (`scrollback_detached_for_reflow` stuck true leaks the lazy buffer and hides
        // tiered history), so recover to ring-only. A width change that raced the
        // rewrap left the store at the first width; keep rewrapping off-lock until the
        // settled width matches (at most one extra pass once widths settle).
        let mut next = Some(pending);
        let mut first = true;
        while let Some(active) = next.take() {
            let pass_lines = active.lines_total();
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::app_render::drive_reflow_job_gauged(
                    active,
                    &never,
                    crate::app_render::REFLOW_WORKER_STEP_LINES,
                    Some(rewrap.gauge()),
                )
            })) {
                Ok(Some(reflowed)) => {
                    next = term_lock(term).finish_resize_offload(reflowed);
                    rewrap
                        .gauge()
                        .pass_done(pass_lines, next.as_ref().map(|n| n.lines_total()));
                }
                // Unreachable (the cancel is never raised), but a dropped job
                // is recovered the one way every other arm recovers it.
                Ok(None) | Err(_) => {
                    ended = aterm_messages::WaitEnd::Failed;
                    if first {
                        aterm_log::error!(
                            "cross-session reflow panicked rewrapping session {session} \
                             scrollback; aborting the offload (tiered history lost, grid \
                             recovered to ring-only)"
                        );
                    } else {
                        aterm_log::error!(
                            "cross-session convergence rewrap panicked for session \
                             {session}; aborting the offload (grid recovered)"
                        );
                    }
                    term_lock(term).abort_resize_offload();
                }
            }
            first = false;
        }
        rewrap.finish(ended);
        // Repaint the target: it may be foreground in another window scrolled into
        // history, which would otherwise keep the pre-reflow view. Matches the self
        // path's post-reattach Wake::Output.
        if let Some(proxy) = proxy {
            let _ = proxy.send_event(Wake::redraw(session));
        }
    }
    // Winsize pixels come from the engine, which holds the session's real font box;
    // `host_cell_pixel_size` is `None` until a host measured one, so a headless
    // target reports zeros rather than a placeholder.
    let cell_px = term_lock(term).host_cell_pixel_size();
    aterm_pty::resize_with_cell_px(master, rows, cols, cell_px);
    // Mirror the window pass's asciicast record so the target's `screen.cast` shows the
    // geometry change, at the place the resize's hold stamped.
    ctx.cast
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .record_resize_stamped(cast_stamp, cols, rows);
    ctx.timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .ingest_resizes(
            &journal,
            Some(crate::resize_ledger::Attribution::CTL_CROSS),
            crate::turn_ledger::now_ms(),
        );
    "OK\n".to_string()
}

/// Cross-session `scroll`: apply the [`ScrollIntent`] to the target's viewport (the seam
/// emits no bytes for `ScrollView`) and report `OK <offset> <max>` like [`cmd_scroll`].
/// No repaint: a background tab reads the new offset when next shown (`select` repaints
/// only because a selection may be visible).
fn cross_scroll(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let intent = match parse_scroll_intent(rest) {
        Ok(intent) => intent,
        Err(error) => return error,
    };
    let mut t = term_lock(term);
    apply_scroll_intent(&mut t, intent);
    let offset = t.grid().display_offset();
    let max = t.grid().scrollback_lines();
    format!("OK {offset} {max}\n")
}

/// Apply a [`ScrollIntent`] to a locked [`Terminal`]'s viewport with the mapping of the
/// seam's `ScrollView` arm (`Up`/`Down` = one screen, `By(n)` = n lines toward history,
/// `Top`/`Bottom` jump), so cross-session `scroll` matches the self path.
fn apply_scroll_intent(t: &mut Terminal, intent: ScrollIntent) {
    let page = i32::from(t.rows()).max(1);
    match intent {
        ScrollIntent::Up => t.scroll_display(page),
        ScrollIntent::Down => t.scroll_display(-page),
        ScrollIntent::By(n) => t.scroll_display(n),
        ScrollIntent::Top => t.scroll_to_top(),
        ScrollIntent::Bottom => t.return_to_live(),
        ScrollIntent::PrevPrompt | ScrollIntent::NextPrompt => {
            if let Some(row) =
                crate::input::jump_prompt_target(t, matches!(intent, ScrollIntent::PrevPrompt))
            {
                t.scroll_to_absolute_row(row);
            }
        }
    }
}

/// Dispatch one request to its handler, returning the full response (including any
/// trailing data rows).
///
/// `(selector, verb, rest)` is the line's [`request_head`] (the optional leading
/// `@<selector>` parsed before the verb split), and [`dispatch_request`] has already
/// resolved the target into `self_*`: the connection's own session for a flagless or
/// `@.` line, else the named session, whose access is gated here via
/// `cross_session_authorized`.
#[allow(clippy::too_many_arguments)]
fn handle(
    selector: &Option<Selector>,
    verb: &str,
    rest: &str,
    self_term: &Arc<Mutex<Terminal>>,
    self_master: i32,
    self_session: u64,
    self_ctx: &Arc<SessionCtx>,
    store: &Store,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    sock_dir: &std::path::Path,
    subscribers: &Subscribers,
    // The session id of the tab on screen, if any (already resolved by the caller),
    // so an explicit `@<sid>` for that tab takes the App input seam
    // (see `front_routed_input`).
    front_active_session: Option<u64>,
    // Artifact verbs park their handoff here, since it must outlive this `String` body
    // through the bounded socket write. Published paths also wait for the client's
    // ACK; in-memory bytes release after flush.
    reply_retention: &mut ReplyTail,
) -> String {
    // Build and meta verbs (`version`/`update`/`help`/`verbs`): non-sensitive global
    // provenance answered for any authenticated scope before target resolution (a
    // selector is ignored). Membership is the table's `Access::AnyScopeMeta`
    // (`is_any_scope_meta`).
    if aterm_types::control_verbs::is_any_scope_meta(verb) {
        return match verb {
            // Global build provenance (version/commit/build-time/binary signature); see
            // crate::build_info (also shown in the macOS About panel).
            "version" => {
                refuse_stray_args(verb, rest).unwrap_or_else(crate::build_info::control_line)
            }
            // The in-app updater's state (enabled, running build, any staged build's
            // version and changes). `update check` forces one synchronous check, which
            // may block for tens of seconds.
            "update" => cmd_update(rest, scope, proxy),
            // The self-describing protocol catalog (`verbs` is an alias); `rest` picks the
            // short catalog, one verb's full entry, or the complete catalog.
            "help" | "verbs" => cmd_help(rest),
            // The macOS consent posture behind EPERM: Full Disk Access, the signing
            // identity a grant is keyed to, and one row per live session. Instance-wide,
            // so a selector is refused.
            "privacy" => match selector {
                None | Some(Selector::SelfTok) => {
                    crate::control_privacy::cmd_privacy(rest, scope, proxy)
                }
                Some(_) => crate::control_privacy::no_selector_error().to_string(),
            },
            _ => no_handler(verb),
        };
    }

    // Owner-only, self-scoped verbs (`sessions`/`who`/`whoami`/`grant`/`revoke`),
    // per the table's `Access::OwnerOnly` (`is_owner_only`); op-class and
    // scope gate are orthogonal (`who` is `Read`-op yet Owner-gated). They act on the
    // connection's own ctx/registry and reject a target selector, so they cannot be
    // redirected to another session's table.
    if aterm_types::control_verbs::is_owner_only(verb) {
        // `hold` is answered in `dispatch_before_session` in production; this covers a
        // caller reaching `handle` directly, through the same decision function and
        // before the generic gate, so refusals are the audited ones, not a silent
        // `ERR denied`.
        if verb == "hold" {
            return dispatch_hold_verb(rest, selector.as_ref(), scope, store);
        }
        // `topic` and `story` take a session selector like any session verb (see
        // `dispatch_before_session`); the bare form lands here with the connection's
        // own session.
        if verb == "topic" {
            return dispatch_topic_verb(
                rest,
                selector.as_ref(),
                scope,
                store,
                subscribers,
                Some(self_session),
            );
        }
        if verb == "story" {
            return dispatch_story_verb(
                rest,
                selector.as_ref(),
                scope,
                store,
                Some(self_session),
                proxy,
            );
        }
        if verb == "rekey" {
            return dispatch_rekey_verb(rest, selector.as_ref(), scope, store);
        }
        if let Some(refusal) = owner_only_refusal(verb, selector.as_ref(), scope) {
            return refusal;
        }
        return match verb {
            "sessions" => match rest.trim() {
                "bridge" => control_session::cmd_sessions_bridge(store),
                "status" => control_session::cmd_sessions_status(proxy),
                _ => refuse_stray_args(verb, rest)
                    .unwrap_or_else(|| control_session::cmd_sessions(self_ctx, store, Some(proxy))),
            },
            "who" => refuse_stray_args(verb, rest)
                .unwrap_or_else(|| control_session::cmd_who(store, subscribers)),
            // `exits`: the roster journal's `Exited` rows — `sessions`' past.
            "exits" => control_session::cmd_exits(store, rest),
            // `identities`: the on-disk identity roster and its `forget`.
            "identities" => crate::agent_identity::cmd_identities(store, rest),
            // The op-level authority primitives and their connection-grain twins share
            // the repaint poke: a successful act moves the tab marks, which no funnel
            // epoch would otherwise notice.
            "grant" => {
                poke_connections_on_ok(proxy, control_session::cmd_grant(self_ctx, scope, rest))
            }
            "revoke" => poke_connections_on_ok(
                proxy,
                control_session::cmd_revoke_with_closed(self_ctx, store, scope, rest),
            ),
            // `connect`/`disconnect`: the declarative session-connection verbs. Self-scoped
            // like `grant` (endpoints ride as `dst=`/`src=`); they take the store to
            // resolve both endpoints and act on the destination's table.
            "connect" => {
                poke_connections_on_ok(proxy, control_session::cmd_connect(store, scope, rest))
            }
            "disconnect" => {
                poke_connections_on_ok(proxy, control_session::cmd_disconnect(store, scope, rest))
            }
            // `flows`: the aggregated connection graph (pure read, no poke).
            "flows" => control_session::cmd_flows(store, scope, rest),
            // `raise <sid>`: raise the hosting window + select the tab (a
            // main-thread hop; window/tab state is App-owned).
            "raise" => control_session::cmd_raise(proxy, store, scope, rest),
            "whoami" => refuse_stray_args(verb, rest)
                .unwrap_or_else(|| control_session::cmd_whoami(self_ctx, scope)),
            "appnotice" => control_query::cmd_appnotice(proxy, rest),
            "notice" => control_query::cmd_notice(proxy, rest),
            "devtools" => control_query::cmd_devtools(proxy, rest),
            "fabric" => dispatch_fabric_verb(rest, selector.as_ref(), scope, store),
            _ => no_handler(verb),
        };
    }

    // The bridge plane. Production answers `deliver`/`hold` in `dispatch_before_session`
    // (no session needed, and the bridge may be the only connection); this covers a
    // caller reaching `handle` directly, via the same decision function.
    if let Some(response) = dispatch_bridge_verb(verb, rest, selector.as_ref(), scope, store) {
        return response;
    }

    // The dispatch target as the caller resolved it: no selector (or `@.`) is the self
    // tuple; otherwise the named session, with its miss and ambiguity already answered.
    let is_cross = !matches!(selector, None | Some(Selector::SelfTok));
    let (term, master, session) = (self_term, self_master, self_session);
    let ctx: &SessionCtx = self_ctx;
    // Is this request's input PINNED to `session` on the App seam? An explicit
    // selector is when it names the tab ON SCREEN (the caller already resolved
    // the front tab, so this is a plain id compare — no extra round trip); a
    // flagless one is when the caller is scoped, so the input its edge authorized
    // here cannot be re-resolved to another front on the main thread. The event
    // loop re-checks before applying, so a stale answer here is harmless.
    let targets_front = front_routed(
        is_cross,
        scope.is_owner_class(),
        front_active_session,
        session,
    );

    // Observe a scoped caller's front input consumer before the op gate. A native
    // Settings tab has no modal `OverlayKind`, yet its reducer consumes key/mouse/paste
    // events and can persist default-off security knobs, so the main-thread
    // observation adds an escalation: Settings -> ConfigWrite, Settings ▸ Harness and
    // any transient overlay -> Owner-only. Defence in depth: a scoped caller's
    // flagless input is pinned to its session (`front_routed`), which never reaches a
    // native reducer. Owner and cross-session requests skip this hop. A failed hop is
    // an authorization failure (fail closed), except a stalled main thread's refusal
    // (`front_surface_authority`).
    let front_surface = if !is_cross && !scope.is_owner_class() && is_front_driving_verb(verb) {
        match front_surface_authority(
            verb,
            control_media::call_main(proxy, |reply| Wake::FrontControlSurface { reply }),
        ) {
            Ok(surface) => surface,
            Err(reply) => return reply,
        }
    } else {
        FrontControlSurface::None
    };
    let front_escalation = front_drive_escalation(scope, verb, front_surface);

    // Op-scope gate, exhaustive and fail-closed, on the effective op
    // (`dispatch_authorized`): the base op escalated by the argument for the indirect
    // seams (`invoke <clipboard/config action>`, `open prefs|settings`), so a plain
    // WriteInput edge cannot tunnel to ClipboardWrite/ConfigWrite; the escalated op
    // replaces the base op, so a fine-op edge for exactly that action still passes.
    // `rest` is the raw argument here (json-flag stripping comes later).
    //
    // Self path (no `@`/`@.`): Owner passes before any lookup; an Edge runs a verb only
    // if its token authorizes the effective op against the now-active session.
    //
    // Cross path (`@other`): additionally, an Owner reaches siblings (same trust
    // domain) and a scoped Edge needs a `decide_edge`-permitted token against the
    // target's table (default deny).
    if is_cross {
        if !dispatch_authorized(scope, verb, rest, ctx) {
            log_denial(
                AUDIT_SUBSYSTEM,
                &format!("cross-session {verb} -> {}", ctx.self_id.as_str()),
                aterm_containment::mode_or_containment(),
                "no authorizing edge for cross-session access",
            );
            return "ERR denied\n".to_string();
        }
    } else if !scope.is_owner_class()
        && !front_escalation.map_or_else(
            || dispatch_authorized(scope, verb, rest, ctx),
            |need| scope_holds_escalation(scope, need, ctx),
        )
    {
        // Self path, Edge scope: re-verify the token against the session active now,
        // not by op alone. The socket's one global ActiveHandle is retargeted by
        // `sync_active_session` on every tab/window switch, and an edge token is a single
        // (src, dst, op) grant against one session's table, so op-only matching would let
        // an edge for session B drive or read whatever session came to the front. Self
        // and cross share the per-session `decide_edge` predicate; they differ only in
        // whether UI side effects run.
        log_denial(
            AUDIT_SUBSYSTEM,
            &format!("self {verb} -> {}", ctx.self_id.as_str()),
            aterm_containment::mode_or_containment(),
            "edge not authorized against the now-active session",
        );
        return front_escalation.map_or_else(
            || "ERR denied\n".to_string(),
            |_| front_drive_denial(front_surface),
        );
    }
    // The `aterm-control` verbs take a host, not a term, built here from the resolved
    // tuple (borrowed, zero-cost). The fleet handles are this dispatcher's registry and
    // the target's own sink, so roster/resolve/write_input answer for the real process;
    // `session` binds the host, so a sid from elsewhere is refused.
    let host = GuiHost::with_fleet(session, term, Some(proxy), subscribers, store, ctx);

    // `--json` read mode: the flag is parsed off `rest` here and routed to the matching
    // `*_json` emitter, then stripped so the text fall-through never sees it. A read
    // verb with no json form answers an ERR (see [`json_unsupported`] for the
    // allowlist); other verbs, and json-capable verbs without the flag, are untouched.
    // The op-scope gate above already authorized `verb`.
    if rest.contains("json")
        && let (true, body) = take_json_flag(rest)
    {
        let json = match verb {
            // `text --json [trim] [tail=<n>|rows=<a>-<b>]`: the tail is parsed; an unknown
            // token is the `TEXT_USAGE` line (a span naming no row, `ERR bad rows`), as in
            // the text form.
            "text" => Some(match control_query::text_args(&body) {
                Ok(args) => control_query::cmd_text_json_looked(term, ctx, args),
                Err(err) => err,
            }),
            // `screen` is ALWAYS styled JSON; accept `screen --json` for symmetry.
            "screen" => Some(control_query::cmd_screen_styled_json(term, &body)),
            "cursor" => Some(control_query::cmd_cursor_json(term)),
            "dims" => Some(control_query::cmd_dims_json(term, session, proxy)),
            "metrics" => Some(control_query::cmd_metrics_json(Some(term), &body)),
            "blocks" => Some(control_selection::cmd_blocks_json(&host, session, &body)),
            "edges" | "grants" => Some(control_session::cmd_edges_json(ctx)),
            // `log --json [<node>] [since= ep=] …`: one object per record.
            "log" => Some(control_session::cmd_log_json(ctx, &body)),
            _ => None,
        };
        if let Some(out) = json {
            return out;
        }
        if let Some(err) = json_unsupported(verb) {
            return err;
        }
    }

    // Take a leading `id=<key>` off the verbs that accept one
    // (`crate::pty_idem::KEYED_VERBS`) and a leading `if=<re>` guard off those that
    // accept one (`control_input::GUARDED_VERBS`), in either order, so every arm parses
    // its usual tail. Done after the op-scope gate (which reads the raw `rest` for
    // `invoke`/`open` escalation) and before the halt gate, self-feed floor and turn
    // arbitration, which must refuse without consuming the key's sequence or
    // evaluating the guard. A malformed option is carried (`leading.refusal`) and
    // answered below the halt gate.
    let (leading, rest) = control_input::take_leading_options(verb, rest);
    let idem_key = leading.idem;
    let rest: &str = &rest;
    // `if-supervisor=<holder>` (the self-claim contract, §2): a verb's whole
    // write is made under it, and each phase of a `turn` ([`under_fence`]).
    let if_supervisor = leading.if_supervisor.as_deref();
    let fenced_phase =
        |write: &dyn Fn() -> bool| under_fence(ctx, if_supervisor, write).unwrap_or(false);

    // Every authorized attempt that can inject bytes, signal the child or mutate its
    // cursor coordinate space is a newer input boundary, even if its arguments are
    // malformed. Fence before parsing so a swallowed key's licence cannot survive an
    // ignored attempt and be borrowed by later PTY output. Denied callers never get
    // here. What the fence does (nothing for a key that arms its own licence at the
    // seam, a release or a bare modifier; keeping the glow on its row for a line edit
    // or paste) is `control_input::control_fence`, the seam's own `press_fence`, so a
    // driven key is fenced like the same physical key.
    let fenced_write = leading.guard.is_some()
        || leading.if_gen.is_some()
        || leading.if_fp.is_some()
        || leading.if_human.is_some();
    if let Some(keep_hand) = control_input::control_fence(verb, rest, fenced_write) {
        let cleared = front_routed_license_clear(proxy, session, keep_hand);
        if !cleared.starts_with("OK") {
            return cleared;
        }
    }

    // The fleet halt: every PTY-reaching verb resolving to a held session answers
    // `ERR halted <reason>`, a transient class beside `ERR busy`, so drivers back off.
    //
    // It applies to every scope, Owner and Bridge included: the fleet `hold` sits
    // behind `Scope::Bridge` so the halted party cannot lift it (the local hold is the
    // owner's own). Placed after authorization, so an unauthorized caller learns
    // nothing of the target's state, beside the self-feed floor as another per-session
    // refusal every socket-driven PTY write passes. `feed-bin`/`paste-bin` repeat it in
    // `run_feed_bin_routed`. The verb list is `fabric::is_pty_reaching`: `post`,
    // `inbox seen`, `meta set`, `lease`
    // and every read verb stay answerable, so a halted agent can ask why.
    if let Some(refusal) = crate::fabric::halt_refusal(ctx, verb) {
        return refusal;
    }

    // The App-lane halt again, for an aimed App verb (`@<sid> tab …`). [`aimed_app_lane`]
    // routes those past `dispatch_before_session`'s App-lane gate so the selector
    // reaches the resolver, but they drive the whole hosting window
    // (`cmd_tab_aimed` → `hosting_window(session)`, `App::apply_tab_cmd_in`), so
    // `tab close N` retires whatever session tab N hosts. The gate above only asks
    // about the aimed session; under a partial halt (`fabric::bridge_lost` holds only
    // `LINK.touched` sids) that would let `@<unheld> tab close N` retire a held
    // window-mate.
    //
    // So re-ask the App-lane question here, keyed on [`is_aimed_app_verb`] and on
    // `is_pty_reaching` inside `app_halt_refusal`: an aimed verb outside the halt set
    // (`spawn`) is untouched, and one added to the lane later is covered.
    if is_aimed_app_verb(verb)
        && let Some(refusal) = crate::fabric::app_halt_refusal(store, verb)
    {
        return refusal;
    }

    // The self-feed floor: every self-targeted input-injection verb passes a
    // per-session token bucket first, so a raw client cannot drive an
    // output→observe→write storm by looping `feed @.` (`SelfGovernor` binds only
    // drivers linking `aterm-agent`; this binds everyone). The cap is generous. Cross
    // writes cannot self-loop and are gated by the target's edge token. `feed-bin` is
    // intercepted earlier and passes the same floor in `run_feed_bin`.
    if !is_cross
        && matches!(
            verb,
            "send" | "key" | "ctrl" | "feed" | "mouse" | "paste" | "turn"
        )
    {
        let nbytes = rest.len().max(1);
        if !crate::inject_floor::allow(self_session, nbytes) {
            return RATE_REFUSAL.to_string();
        }
    }

    // Turn arbitration: while a `turn` holds this session's lease, or another
    // connection holds a hard drive lease (`lease acquire … hard`), other connections'
    // write verbs are refused naming the holder, so an orchestrator can wait and retry.
    // `cmd_turn` re-checks under the lease lock (this is the fast fail). `signal` stays
    // exempt, and keyboard input never passes this seam.
    if matches!(
        verb,
        "send" | "key" | "ctrl" | "feed" | "mouse" | "paste" | "turn"
    ) {
        let held = ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|l| l.write_block(crate::metrics::now_us(), serving_connection()));
        if let Some(held) = held {
            return format!("ERR busy {held}\n");
        }
    }

    // A malformed leading option, then the `if=<re>` guard itself, are answered here:
    // below the halt gate (a halted session says `ERR halted`, never `ERR badregex` or
    // `OK skipped`) and above the idempotency claim, so a bad pattern never consumes an
    // `id=` sequence. The regex is compiled outside the terminal lock, bounded like
    // `await match`'s.
    if let Some(refusal) = leading.refusal {
        return refusal;
    }
    // `cause=@<off>|<seq>` on `send`/`turn` (phase 3A, rulings R6 and R7): what this
    // input says it acted on, checked against what THIS session holds before any byte
    // moves or any `id=` is consumed. An edge also needs read authority on the target:
    // the check's answer would otherwise be an oracle on its mail offsets. A `turn`
    // takes one among its own options too (`cmd_turn`, which refuses a second).
    let input_log_epoch = crate::session_log::InputVerb::of(verb).map(|_| {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog()
            .log()
            .ep()
    });
    let cause = match leading.cause {
        Some(raw) => {
            if !scope.is_owner_class() && !cross_session_authorized(scope, "text", ctx) {
                log_denial(
                    AUDIT_SUBSYSTEM,
                    &format!("{verb} cause= -> {}", ctx.self_id.as_str()),
                    aterm_containment::mode_or_containment(),
                    "cause= reads what the target holds; edge lacks read",
                );
                return "ERR denied\n".to_string();
            }
            match crate::session_log::validate_cause(ctx, raw) {
                Ok(id) => Some(id),
                Err(refusal) => return refusal,
            }
        }
        None => None,
    };
    // `turn`'s OWN WORDS, refused here, before the exactly-once claim below: its
    // whole option grammar, its patterns, and a `cause=` among its options
    // (ruling R6's any-position form), checked as a leading one is — authority,
    // then what this session holds — and taken once (both is the usage). A
    // refusal decided inside the turn would come after the claim and burn the
    // `id=`: the corrected retry would answer `ERR in-doubt` though nothing
    // was typed (`control_session::turn_preflight`).
    let cause = if verb == "turn" {
        match control_session::turn_preflight(rest) {
            Err(refusal) => return refusal,
            Ok(None) => cause,
            Ok(Some(_)) if cause.is_some() => {
                return crate::session_log::CAUSE_USAGE.to_string();
            }
            Ok(Some(raw)) => {
                if !scope.is_owner_class() && !cross_session_authorized(scope, "text", ctx) {
                    log_denial(
                        AUDIT_SUBSYSTEM,
                        &format!("turn cause= -> {}", ctx.self_id.as_str()),
                        aterm_containment::mode_or_containment(),
                        "cause= reads what the target holds; edge lacks read",
                    );
                    return "ERR denied\n".to_string();
                }
                match crate::session_log::validate_cause(ctx, raw) {
                    Ok(id) => Some(id),
                    Err(refusal) => return refusal,
                }
            }
        }
    } else {
        cause
    };
    // The unread-input gate (`input_stall`): a program published as not reading (or a
    // stopped job) has earlier input unread, so a key written now would be read later
    // against a screen the program never drew. Refused only then; a slow read is
    // written and warned on the reply. Below the halt and usage refusals, above the
    // guard and the idempotency claim, so a refusal consumes no `id=` sequence
    // (owner, 2026-09-29).
    let warning = match crate::input_stall::admission(ctx, verb, rest, leading.unread_ok) {
        crate::input_stall::Admission::Refuse(refusal) => return refusal,
        crate::input_stall::Admission::Warn(warning) => Some(warning),
        crate::input_stall::Admission::Open => None,
    };
    let guard = match control_input::compile_guard(leading.guard.as_deref()) {
        Ok(guard) => guard,
        Err(reply) => return reply,
    };
    let fence = control_input::InputFence {
        guard: guard.as_deref(),
        generation: leading.if_gen,
        fp: leading.if_fp,
        human: leading.if_human,
        guard_at_cursor: false,
    };

    // The PTY seam's exactly-once gate, placed after every refusal decided before a
    // byte can move (authority, halt, self-feed floor, turn arbitration) and around the
    // arm that writes, so only an attempt that reached the seam consumes a sequence. A
    // request with no `id=` runs unguarded. A keyed `send` with nothing to type is
    // refused first, so it cannot settle a claim with no bytes moved.
    if verb == "send"
        && let Some(refusal) = crate::pty_idem::empty_payload_refusal(
            idem_key.as_deref(),
            control_input::send_bytes(rest).is_empty(),
        )
    {
        return refusal;
    }
    // THE SUPERVISOR FENCE, last of the refusals before a byte can move and
    // above the idempotency claim (a refusal consumes no `id=` sequence). A
    // `send`/`key`/`paste` is admitted here and stays counted until its reply
    // ([`under_fence`]); a `turn` is checked here and fenced again per phase,
    // so its waits never hold a replacement back.
    let _fenced = match if_supervisor {
        Some(holder) if verb == "turn" => {
            if let Some(refusal) =
                crate::session_timeline::supervisor_fence(ctx, holder, crate::metrics::now_us())
            {
                return refusal;
            }
            None
        }
        Some(holder) => {
            match crate::session_timeline::FencedWrite::enter(ctx, holder, crate::metrics::now_us())
            {
                Ok(counted) => Some((counted, CarriedFence::enter(holder))),
                Err(refusal) => return refusal,
            }
        }
        None => None,
    };
    let input_mark = crate::session_log::InputVerb::of(verb)
        .filter(|v| *v != crate::session_log::InputVerb::Turn)
        .map(|input| {
            crate::control_input_ack::InputMark::at_epoch(
                ctx,
                Some(subscribers),
                session,
                control_session::log_actor(scope, ctx),
                input,
                cause,
                false,
            )
            .within_epoch(input_log_epoch)
        });
    let completion = || input_mark.as_ref().map(|mark| mark.completion());
    let resp = crate::pty_idem::guarded(ctx, scope, verb, idem_key.as_deref(), || match verb {
        // `text [trim] [tail=<n>|rows=<a>-<b>]`: the arm parses its tail, so an unknown
        // modifier is `ERR usage` rather than a silent full grid.
        "text" => match control_query::text_args(rest) {
            Ok(args) => control_query::cmd_text_opt(term, args),
            Err(err) => err,
        },
        // The LOSSLESS styled-screen read (keystone): full per-cell colour +
        // resolved decorations + cursor + dims + seq as one JSON frame. Always
        // styled-JSON (no plaintext variant) — `--json` is implied.
        "screen" => control_query::cmd_screen_styled_json(term, rest),
        "cursor" => control_query::cmd_cursor(term),
        "cell" => control_query::cmd_cell(term, rest),
        "search" => control_query::cmd_search(term, rest, &ctx.rewrap_gauge),
        // `edges`/`grants`: this session's inbound capability edges (EdgeTable rows). An
        // observer of the authority surface, gated `ReadScreen` like other reads;
        // cross-session reads a sibling's table through the same resolution and gate.
        "edges" | "grants" => control_session::cmd_edges(ctx),
        // `inbox`: the session's fabric mailbox: the bare listing (`Lines`),
        // `inbox get <id>` (a byte body) and `inbox seen <id>` (a status line). `Read`
        // class, with `seen` escalated to `WriteInput` by `escalated_op`, so a read-only
        // edge can read mail but not mark it handled. `seen` and a parked `get @<off>`
        // record a row the bridge acts on, so they wake `session`'s watchers.
        "inbox" => {
            crate::fabric::cmd_inbox_verb(ctx, rest, &|| wake_watchers(subscribers, session))
        }
        // `post`: queue one outbound fabric message. The handler refuses edge-token
        // connections (a write-input edge over S must not speak as S on the bus; a
        // sender is attested by the Owner). Exempt from `hold`, so a halted agent can
        // `post kind=ask "why am I halted?"`. The length-prefixed body form is
        // intercepted in the serve loop; this arm serves the inline text form.
        "post" => {
            if post_scope_denied(scope) {
                log_denial(
                    AUDIT_SUBSYSTEM,
                    &format!("post -> {}", ctx.self_id.as_str()),
                    aterm_containment::mode_or_containment(),
                    "post is attested by the instance owner; an edge cannot speak as a session",
                );
                return "ERR denied\n".to_string();
            }
            post_waking_watchers(ctx, session, rest, None, subscribers)
        }
        // `lease`: the explicit cooperative drive lease for raw (non-`turn`) drivers.
        // Write-class (cross-session needs a write edge) but outside the
        // write-arbitration seam above, since it manages the lease rather than injecting
        // input; `lease status`/`release` answer mid-turn.
        //
        // The `fabric:` namespace is the bridge's, gated here because this seam knows
        // the connection's scope. See [`lease_forges_fabric_holder`].
        "lease" => {
            if lease_forges_fabric_holder(scope, rest) {
                log_denial(
                    AUDIT_SUBSYSTEM,
                    &format!("lease acquire holder=fabric:* -> {}", ctx.self_id.as_str()),
                    aterm_containment::mode_or_containment(),
                    "the `fabric:` lease namespace is the bridge connection's;                      authority is a connection, never a name",
                );
                return "ERR denied\n".to_string();
            }
            control_session::cmd_lease(ctx, rest)
        }
        // `family [<sid>]`: the session hierarchy (parent and children) from the
        // registry's parent links. The no-arg form starts at the resolved session; an
        // explicit `<sid>` walks an arbitrary node, so it is Owner-only.
        "family" => control_session::cmd_family(ctx, store, scope, rest),
        // `ready [timeout_ms]`: block until the target is alive and idle (at an OSC-133
        // prompt, or after the kernel idle-settle window), so an agent can chain
        // sessions without polling.
        "ready" => control_session::cmd_ready(term, store, session, ctx, rest, subscribers),
        // `await <idle|seq|match|gone|block>`: block until the Observation Kernel
        // latches the predicate. Unlike `wait` (OSC-133-only), `idle`/`match`/`gone`/`seq`
        // work for alt-screen agent TUIs. It registers a subscriber, so it wakes on
        // output and at the idle deadline without polling.
        // `await consent` reads the instance's consent posture, not this terminal, so it
        // takes the main-thread posture hop instead of a `WatcherSpec`; split here so
        // the kernel path keeps its grammar and `term`-only inputs.
        "await" if rest.split_whitespace().next() == Some("consent") => {
            crate::control_privacy::cmd_await_consent(proxy, scope, session, rest)
        }
        // `await momentum <floor>` reads the typing momentum of the window hosting the
        // session (main-thread App state, like `trail status`), then parks on the
        // kernel's `MomentumBelow` timer.
        "await" if rest.split_whitespace().next() == Some("momentum") => {
            let read = || typing_momentum_reading(proxy, session);
            control_session::cmd_await_momentum(term, store, session, rest, subscribers, &read)
        }
        "await" => control_session::cmd_await(term, store, session, ctx, rest, subscribers),
        // `turn`: one complete human turn: type text (paste semantics), verified
        // submit (content_seq must advance, else re-press), settle, and return the
        // settled screen. It writes input and reads the screen, so a scoped caller
        // needs both authorities; non-Owner scopes past the WriteInput gate are denied
        // here unless read is also authorized. Input delivery is resolved per path:
        // cross = the source-blind seam on the target, self = the event-loop seam, as
        // for plain `paste`/`key`.
        "turn" => {
            if !scope.is_owner_class() && !cross_session_authorized(scope, "text", ctx) {
                log_denial(
                    AUDIT_SUBSYSTEM,
                    &format!("turn -> {}", ctx.self_id.as_str()),
                    aterm_containment::mode_or_containment(),
                    "turn needs read+write; edge lacks read",
                );
                return "ERR denied\n".to_string();
            }
            // The even hand's two extra routes: `key` presses one pre-built event
            // (`turn typed=1`) through the same seam as the path's `press`, and
            // `momentum` reads the target's typing momentum on the main thread
            // (`turn yield=`, as `trail status momentum=` reports).
            let momentum = || typing_momentum_reading(proxy, session);
            // Who drives, for the lease and presence band: an edge-scoped caller is the
            // session its token was granted to (from the target's table that just
            // authorized it); Owner-class callers are credited to no session.
            let driver = turn_driver(scope, ctx);
            // Who the session log names for the turn: the connection.
            let actor = control_session::log_actor(scope, ctx);
            let mark = crate::control_input_ack::InputMark::at_epoch(
                ctx,
                Some(subscribers),
                session,
                actor,
                crate::session_log::InputVerb::Turn,
                cause,
                true,
            )
            .within_epoch(input_log_epoch);
            if turn_input_route(is_cross, targets_front) == TurnInputRoute::Front {
                // An explicit selector naming the visible tab is a visible App drive:
                // both phases go through the targeted event-loop seam like plain
                // paste/key, keeping movement provenance, ordering and host side effects.
                let paste = |text: &str| {
                    fenced_phase(&|| {
                        front_routed_input_observed(
                            proxy,
                            session,
                            Some(InputEvent::Paste(
                                control_input::paste_text(text),
                                PasteFraming::AtDrain,
                            )),
                            "ERR\n",
                            Some(mark.completion()),
                        )
                        .starts_with("OK")
                    })
                };
                let press = |name: &str| {
                    fenced_phase(&|| {
                        front_routed_input_observed(
                            proxy,
                            session,
                            parse_key(name),
                            "ERR\n",
                            Some(mark.completion()),
                        )
                        .starts_with("OK")
                    })
                };
                let key = |ev: InputEvent| {
                    let ev = std::cell::Cell::new(Some(ev));
                    fenced_phase(&|| {
                        front_routed_input_observed(
                            proxy,
                            session,
                            ev.take(),
                            "ERR\n",
                            Some(mark.completion()),
                        )
                        .starts_with("OK")
                    })
                };
                control_session::cmd_turn(
                    term,
                    store,
                    session,
                    rest,
                    subscribers,
                    ctx,
                    &control_session::TurnIo {
                        paste: &paste,
                        press: &press,
                        key: &key,
                        momentum: &momentum,
                        driver: driver.clone(),
                        actor,
                        cause,
                        mark: Some(&mark),
                    },
                )
            } else if turn_input_route(is_cross, targets_front) == TurnInputRoute::Background {
                let paste = |text: &str| {
                    fenced_phase(&|| {
                        cross_input_observed(
                            term,
                            ctx,
                            Some(InputEvent::Paste(
                                control_input::paste_text(text),
                                PasteFraming::AtDrain,
                            )),
                            "ERR\n",
                            Some(mark.completion()),
                        )
                        .starts_with("OK")
                    })
                };
                let press = |name: &str| {
                    fenced_phase(&|| {
                        cross_input_observed(
                            term,
                            ctx,
                            parse_key(name),
                            "ERR\n",
                            Some(mark.completion()),
                        )
                        .starts_with("OK")
                    })
                };
                let key = |ev: InputEvent| {
                    let ev = std::cell::Cell::new(Some(ev));
                    fenced_phase(&|| {
                        cross_input_observed(term, ctx, ev.take(), "ERR\n", Some(mark.completion()))
                            .starts_with("OK")
                    })
                };
                control_session::cmd_turn(
                    term,
                    store,
                    session,
                    rest,
                    subscribers,
                    ctx,
                    &control_session::TurnIo {
                        paste: &paste,
                        press: &press,
                        key: &key,
                        momentum: &momentum,
                        driver: driver.clone(),
                        actor,
                        cause,
                        mark: Some(&mark),
                    },
                )
            } else {
                let paste = |text: &str| {
                    fenced_phase(&|| {
                        local_input_observed(
                            proxy,
                            Some(control_input::paste_event(text)),
                            "ERR\n",
                            Some(mark.completion()),
                        )
                        .starts_with("OK")
                    })
                };
                let press = |name: &str| {
                    fenced_phase(&|| {
                        local_input_observed(
                            proxy,
                            parse_key(name),
                            control_input::KEY_USAGE,
                            Some(mark.completion()),
                        )
                        .starts_with("OK")
                    })
                };
                let key = |ev: InputEvent| {
                    let ev = std::cell::Cell::new(Some(ev));
                    fenced_phase(&|| {
                        ev.take().is_some_and(|ev| {
                            control_input::input_reply_to_str(post_input_reply_to_observed(
                                proxy,
                                vec![ev],
                                None,
                                Some(mark.clone()),
                            ))
                            .starts_with("OK")
                        })
                    })
                };
                control_session::cmd_turn(
                    term,
                    store,
                    session,
                    rest,
                    subscribers,
                    ctx,
                    &control_session::TurnIo {
                        paste: &paste,
                        press: &press,
                        key: &key,
                        momentum: &momentum,
                        driver: driver.clone(),
                        actor,
                        cause,
                        mark: Some(&mark),
                    },
                )
            }
        }
        // `key if=<re> <name>` / `send if=<re> <text>`: atomic check-and-press. The guard
        // is tested against the visible rows and the bytes written under one hold of
        // the target's terminal lock, so no output batch can land between check and
        // press (e.g. a permission prompt resolving between a read and `key 1`). One arm
        // for self, front and background targets: it runs on the control thread against
        // the resolved `(term, sink)`, since the App seam cannot hold the terminal lock
        // across its write. A miss is `OK skipped` (exit 0) and writes nothing.
        //
        // `if-gen=<epoch>.<seq>` / `if-fp=<hex16>` (`InputFence`) compare the caller's
        // screen generation or hash under that hold; a moved screen answers
        // `OK skipped reason=changed`. `if-human=<n>` compares the person count under
        // the same hold, ahead of the screen fences, answering
        // `OK skipped reason=person` (not atomic with the person's own write, which
        // takes no terminal lock; see `crate::human_input`).
        "send" | "key" if fence.is_armed() => {
            if verb == "key" {
                control_input::cmd_key_fenced_observed(term, ctx, &fence, rest, completion())
            } else {
                control_input::cmd_send_fenced_observed(term, ctx, &fence, rest, completion())
            }
        }
        "send" if !is_cross || targets_front => front_routed_input_observed(
            proxy,
            session,
            Some(InputEvent::KeySequence(control_input::send_bytes(rest))),
            "ERR\n",
            completion(),
        ),
        "send" => control_input::input_reply_to_str(cross_raw_input_observed(
            term,
            ctx,
            control_input::send_bytes(rest),
            completion(),
        )),
        // The self (active-tab) path sends `key`/`ctrl`/`mouse`/`paste`/`focus`/`resize`/
        // `scroll` through the source-blind `App::input` seam on the event loop
        // (`Wake::Input` / a reply-bearing resize), so bytes match human input and the
        // renderer/window/gesture side effects (snap-to-bottom, selection gesture,
        // `request_inner_size`) run for the visible tab. Only these arms touch the
        // active UI.
        //
        // P1.2 follow-up: the CROSS-session (`@other`) path no longer fails closed. A
        // background target is NOT the active tab, so there is no App UI/gesture/window
        // state to touch — and the tracked egress seam is source-blind and session-
        // agnostic (it reads the modes from the GIVEN term and writes the GIVEN sink).
        // So a cross arm resolves `(target_term, target_sink) = (term, &ctx.sink)` the
        // SAME way `send`/`feed` already do, builds the IDENTICAL `InputEvent` via the
        // shared `parse_*` helpers, and calls tracked egress DIRECTLY on the control
        // thread (no `Wake::Input`). Op-scope is already `WriteInput`-gated above
        // (`cross_session_authorized`), so these run only after the edge/owner check.
        // An explicit `@<sid>` that resolved to the tab ON SCREEN is not a
        // background target: route it through the App seam so a named-session
        // drive is indistinguishable from a flagless one (see
        // [`front_routed_input`]). Genuinely background targets keep the direct,
        // round-trip-free egress below. A SCOPED caller's flagless drive takes
        // the same pinned arm (`targets_front`, see [`front_routed`]): only the
        // Owner's bare form follows the front.
        // A malformed name answers the SAME usage line the flagless and the
        // guarded arms do: a cross-session drive is not a second grammar.
        "key" if targets_front => front_routed_input_observed(
            proxy,
            session,
            parse_key(rest),
            control_input::KEY_USAGE,
            completion(),
        ),
        "key" if is_cross => cross_input_observed(
            term,
            ctx,
            parse_key(rest),
            control_input::KEY_USAGE,
            completion(),
        ),
        "key" => local_input_observed(
            proxy,
            parse_key(rest),
            control_input::KEY_USAGE,
            completion(),
        ),
        "hwkey" => control_input::cmd_hwkey(proxy, rest),
        "ctrl" if targets_front => front_routed_input_observed(
            proxy,
            session,
            parse_ctrl(rest),
            "ERR usage: ctrl <single-letter>\n",
            completion(),
        ),
        "ctrl" if is_cross => cross_input_observed(
            term,
            ctx,
            parse_ctrl(rest),
            "ERR usage: ctrl <single-letter>\n",
            completion(),
        ),
        "ctrl" => local_input_observed(
            proxy,
            parse_ctrl(rest),
            "ERR usage: ctrl <single-letter>\n",
            completion(),
        ),
        "feed" if !is_cross || targets_front => match control_input::feed_bytes(rest) {
            Ok(bytes) => {
                let n = bytes.len();
                let response = front_routed_input_observed(
                    proxy,
                    session,
                    Some(InputEvent::KeySequence(bytes)),
                    "ERR\n",
                    completion(),
                );
                if response.starts_with("OK") {
                    format!("OK {n} bytes\n")
                } else {
                    response
                }
            }
            Err(error) => error.to_string(),
        },
        "feed" => match control_input::feed_bytes(rest) {
            Ok(bytes) => {
                let n = bytes.len();
                match cross_raw_input_observed(term, ctx, bytes, completion()) {
                    Ok(InputOutcome::Ok) => format!("OK {n} bytes\n"),
                    outcome => control_input::input_reply_to_str(outcome),
                }
            }
            Err(error) => error.to_string(),
        },
        "signal" => control_input::signal_verb(master, rest, ctx),
        // The explicit restart (`crate::restart_act`): its stamp before its
        // signal, its escalation its own thread, so the lane is not held.
        "restart" => control_input::restart_verb(master, rest, self_ctx),
        // The manual reset (`crate::manual_reset`), for a terminal the foreground
        // handback cannot reach. It runs on the target's PTY reader, touching no App UI
        // and writing no input, so self, front and background share one arm.
        "reset" => crate::manual_reset::cmd_reset(session, term, ctx, rest),
        // The parser's refusal is the reply, as it is for the self verb
        // (`cmd_mouse`) and the background cross arm (`cross_mouse`): a bad
        // modifier answers `ERR bad modifier …` naming what a report can carry,
        // not the generic usage line. Until 2026-09-21 this arm `.ok()`'d the
        // Result and handed `front_routed_input` the usage line for every
        // refusal, so the one drive whose target is the on-screen tab was the
        // one that could not say why it refused — a divergence the `mods=`
        // refusal of 367d85b9e made visible and the key arms above never had.
        //
        // A pinned gesture keeps the self verb's copy-on-select fence: a scoped
        // caller's release never auto-copies (`apply_copy_on_select_policy`, a
        // no-op for Owner).
        "mouse" if targets_front => match control_input::parse_mouse(rest) {
            Ok(ev) => front_routed_input(
                proxy,
                session,
                Some(control_input::apply_copy_on_select_policy(scope, ev)),
                control_input::MOUSE_USAGE,
            ),
            Err(refusal) => refusal,
        },
        "mouse" if is_cross => cross_mouse(term, ctx, session, proxy, rest),
        // Self mouse: `scope` is passed so a non-Owner gesture has its copy-on-select
        // clipboard effect suppressed (the exfil fence), stamped on the injected event
        // rather than branching on `Source` in the byte seam.
        "mouse" => control_input::cmd_mouse(proxy, scope, rest),
        "paste" if targets_front => front_routed_input_observed(
            proxy,
            session,
            Some(InputEvent::Paste(
                control_input::paste_text(rest),
                PasteFraming::AtDrain,
            )),
            "ERR\n",
            completion(),
        ),
        "paste" if is_cross => cross_input_observed(
            term,
            ctx,
            Some(InputEvent::Paste(
                control_input::paste_text(rest),
                PasteFraming::AtDrain,
            )),
            "ERR\n",
            completion(),
        ),
        "paste" => local_input_observed(
            proxy,
            Some(control_input::paste_event(rest)),
            "ERR\n",
            completion(),
        ),
        "focus" if targets_front => match control_input::parse_focus(rest) {
            Some(focused) => front_routed_input(
                proxy,
                session,
                Some(InputEvent::Focus(focused)),
                "ERR usage: focus <in|out>\n",
            ),
            None => "ERR usage: focus <in|out>\n".to_string(),
        },
        "focus" if is_cross => match control_input::parse_focus(rest) {
            Some(focused) => cross_input(term, ctx, Some(InputEvent::Focus(focused)), "ERR\n"),
            None => "ERR usage: focus <in|out>\n".to_string(),
        },
        "focus" => control_input::cmd_focus(proxy, rest),
        // `image read [...]` reads the structured inline-image payloads from the target
        // terminal model (headless-safe, cross-session correct), so it is matched before
        // the framebuffer arm; `term` is already the resolved target.
        "image" if rest.split_whitespace().next() == Some("read") => control_media::cmd_image_read(
            term,
            rest.strip_prefix("read").unwrap_or(rest).trim_start(),
        ),
        // Framebuffer capture. Cross-session (`@<sel>`): capture the window whose
        // ACTIVE tab displays the resolved session through the same app-present
        // path as self, including decorations, tab strip, and overlays. A session
        // no window shows (background tab) fails honestly instead of silently
        // capturing the WRONG (front) session. This is client-destination truth,
        // not platform-compositor visibility or scanout. That frame is more than
        // the resolved session, so a scoped edge is refused it
        // ([`image_frame_refusal`]); `image read` above is its pixel verb.
        "image" if !scope.is_owner_class() => image_frame_refusal(ctx),
        "image" => {
            control_media::cmd_image(proxy, queue, rest, sock_dir, is_cross.then_some(session))
                .into_body_retaining(reply_retention)
        }
        // `window` captures a full front-window artifact (platform chrome around the
        // submitted client destination) to a PNG; no compositor visibility or scanout
        // claim. App-level verbs: the selector chooses where it runs (`@<sid>` routes to
        // the instance, cross-instance via the client relay), and the verb acts on that
        // instance's front window, so `@peer window` screenshots the peer's window
        // (`image` is the per-session pixel verb). Auth is enforced upstream.
        "window" => {
            control_media::cmd_window(proxy, rest, sock_dir).into_body_retaining(reply_retention)
        }
        // `video <seconds> [full] [keys] [pace]`: record the front window's submitted
        // swapchain-destination frames to a PNG sequence + index.json, so an AI can
        // inspect smoothness/flashes and correlate input attempts with frames. It does
        // not observe compositor selection or scanout. `keys` is owner-only (enforced
        // inside); its samples are not PTY-delivery or glyph receipts.
        "video" => control_media::cmd_video(proxy, rest, sock_dir, scope.is_owner_class())
            .into_body_retaining(reply_retention),
        // `chrome`: the resolved instance's front window native UI (app-level; `@<sid>`
        // routes to the instance, per the rule above — `@peer chrome` reads the peer's).
        "chrome" => control_media::cmd_chrome(proxy),
        // `panes`: the active tab's split layout (cell rects, focus, zoom). Cross-session
        // uses `image`'s routing (the window whose active tab shows the session), so
        // `@<sid> panes` and `@<sid> image` describe the same window.
        "panes" => control_media::cmd_panes(proxy, is_cross.then_some(session)),
        // `controls <target>` dumps an own-rendered app surface's controls as text (like
        // `chrome`), built on the main thread from the pure surface model, so it works
        // headless. App-level; `@<sid>` routes to the instance.
        "controls" => control_media::cmd_controls(proxy, rest),
        // `open <target>`: app-level UI on the resolved instance (`@<sid>` routes).
        "open" => control_media::cmd_open(proxy, rest),
        // `invoke <action>`: fire a menu action by name (enabled-gated, single sink).
        // App-level, so `@peer invoke NewTab` opens a tab in the peer. The caller is the
        // ledger's `by=` for a session the action retires (`invoke CloseTab`), as for
        // `close` and `tab`.
        "invoke" => {
            control_media::cmd_invoke(proxy, rest, control_session::caller_actor(scope, ctx))
        }
        // `confirm yes|no`: answer the close/quit question a control client raised
        // (`close_confirm`, Windows). App-level: one question per instance.
        "confirm" => control_media::cmd_confirm(proxy, rest),
        // `rain [status|on|off|toggle]`: the per-session matrix-rain override on the
        // focused window's front session. App-level.
        "rain" => control_media::cmd_rain(proxy, rest),
        // `theme [status|list|preview|revert|commit|set]`: the live theme and
        // its transient preview lane. App-level; the class rises by sub-form
        // (`escalated_op`). The session-scoped forms (`theme session …`,
        // `theme palette session`) act on the target: this tab, or `@<sid>`.
        "theme" => control_media::cmd_theme(proxy, rest, Some(session)),
        // `fx [status|celebrate …]`: arm a one-shot rainbow-kitty celebration on the
        // focused window's front session (or read the arm). App-level; write-class
        // because the session's next keyed edge spends it.
        "fx" => control_media::cmd_fx(proxy, rest),
        // `tone [status]`: the front window's tone-of-typing mood and every classifier
        // gate. Read-only and App-level.
        "tone" => control_media::cmd_tone(proxy, rest),
        // `streak [status]`: the front window's prism wake state, every gate between
        // program output and a comet. Read-only and App-level.
        "streak" => control_media::cmd_streak(proxy, rest),
        // `trail [status|<n>]`: the focused window's cursor-trail diagnostics. `<n>`
        // prints the last n spawn-seam verdicts from the engine's ring (licensed or
        // declined + reason: no-fresh-hint / no-credits / off-shape / program-row, plus
        // origin/target); `status` prints one line of standing engine state (style,
        // every gate, the licensed/declined tally, live ribbon). Read-only, App-level;
        // the command face of the ATERM_TRACE_SPAWN sensor.
        "trail" => control_media::cmd_trail(proxy, rest),
        "kitty" => control_media::cmd_kitty(proxy, rest),
        // `spawn`: mint one new tab session and reply `OK <sid>`, addressable at once
        // (`@<sid> turn …`). Cross-session (`@<sid> spawn`) places it in the window
        // hosting the resolved session on any tab (`App::window_of_session`), wider than
        // `@<sid> image`, which needs the session on an active tab; where both answer
        // they name the same window. An aimed spawn does not raise the window by default
        // (`raise=` wins). Only an Owner reaches this arm with a selector
        // (`aimed_app_lane`).
        "spawn" => control_media::cmd_spawn(proxy, rest, is_cross.then_some(session)),
        // `@<sid> close`: retire the resolved session (self with no selector). The caller
        // is the exit ledger's `by=`: an edge-scoped connection is the session its token
        // was granted to; an owner-token connection is named by kind (`by=ctl`,
        // `by=bridge`). Never `self_ctx` (already resolved to the target) or the front
        // tab (after `spawn`, `@<new> close` would name the closed session as closer).
        // `close` takes no argument and says so before retiring anything (see
        // [`control_media::close_no_arg`]).
        "close" => match control_media::close_no_arg(rest) {
            Ok(()) => control_media::cmd_close(
                proxy,
                session,
                ctx.self_id.as_str(),
                control_session::caller_actor(scope, ctx),
            ),
            Err(usage) => usage,
        },
        // `settings [open|close|toggle]` drives the native Settings tab (`open settings`
        // is an alias), so a driver can open it and read its route with
        // `controls settings` or capture it with `image` on any platform. App-level UI
        // on the main thread (write-class); `@<sid>` routes to the instance.
        "settings" => control_media::cmd_settings_overlay(proxy, rest),
        // `tab`: cross-session (`@<sid> tab …`) drives the tabs of the window hosting the
        // resolved session, the same aim as `@<sid> spawn`, so an agent in a background
        // window can walk its own tabs; flagless stays the front window. The caller is
        // the ledger's `by=` for a `tab close`, as for `close`.
        "tab" if is_cross => control_media::cmd_tab_aimed(
            proxy,
            rest,
            session,
            control_session::caller_actor(scope, ctx),
        ),
        "tab" => control_input::cmd_tab(proxy, rest, control_session::caller_actor(scope, ctx)),
        // Toggle the drop-target highlight on the front window (automation of the
        // drag-and-drop affordance; a real drag drives the same flag). A `@<sel>` is
        // meaningless here.
        "hover" => control_input::cmd_hover(proxy, rest),
        // `find`: drive the find bar (type a query, step matches, toggle case/regex,
        // accept/cancel). Find mode diverts keystrokes from the PTY and moves only
        // viewport and highlight, like `scroll`/`select`; `search` answers the same
        // question without a bar. Front window, like `hover`: `App::find_cmd` resolves
        // `frontmost_window`, so `@<sel>` routes to the instance only.
        //
        // The mutating forms require read and write together (the `turn` conjunction):
        // escalating would replace the base read gate, and those forms still answer
        // with the match position. The bare and `status` forms only report, so a
        // watcher can use them; every other form routes later keystrokes into the bar
        // and moves selection and viewport, which is a write.
        "find"
            if !matches!(rest.split_whitespace().next().unwrap_or(""), "" | "status")
                && !matches!(scope, Scope::Owner)
                && !cross_session_authorized(scope, "key", ctx) =>
        {
            log_denial(
                AUDIT_SUBSYSTEM,
                &format!("find -> {}", ctx.self_id.as_str()),
                aterm_containment::mode_or_containment(),
                "find's mutating forms need read+write; edge lacks write",
            );
            "ERR denied\n".to_string()
        }
        "find" => control_input::cmd_find(proxy, rest),
        // `pane <dir>`: move keyboard focus between the active tab's split panes (made by
        // `spawn split=v|h`), via the key binding's `focus_pane_in`. Write-class like
        // `tab`, since it chooses where keystrokes go. Front window, like `hover`.
        "pane" => control_input::cmd_pane(proxy, rest),
        // `pointer`: put the window's pointer on a cell through `on_cursor_moved` (what
        // winit's `CursorMoved` calls), so link hover, divider cursors and strip
        // highlight resolve for real. Write-class: under DEC 1000/1002/1003 the motion
        // is reported to the program. Front window.
        "pointer" => control_input::cmd_pointer(proxy, rest),
        // Cross-session `resize` does NOT go through the seam: `seam_egress` emits no
        // bytes for `Resize`, and `App::input`'s Resize arm resizes the WINDOW (every
        // tab + the GPU swapchain). A background target has no window to echo to, so we
        // replicate ONLY the term+PTY pair (`echo_to_window: false` semantics) on the
        // TARGET, never the active window/framebuffer.
        "resize" if targets_front => front_routed_resize(proxy, session, rest),
        // The px form names the WINDOW and a driven px resize only ever reaches
        // the FRONT one, so a background target is refused by name
        // ([`control_input::RESIZE_PX_FRONT_ONLY`]) instead of falling into the
        // cell parser's `ERR bad args`; the cell form goes on to the engine + PTY.
        "resize" if is_cross => match control_input::cross_resize_px_refusal(rest) {
            Some(refusal) => refusal,
            None => cross_resize(term, master, ctx, session, Some(proxy), rest),
        },
        "resize" => control_input::cmd_resize(proxy, rest),
        // Cross-session `scroll` also bypasses the seam (`ScrollView` emits no bytes;
        // the viewport move lives in `App::input`). It applies the `ScrollIntent`
        // DIRECTLY to the TARGET term's viewport and reports `OK <offset> <max>` — the
        // SAME wire shape as the self path's `cmd_scroll`. `select` is already
        // cross-correct (mutates the target term + fires a repaint keyed by target id).
        "scroll" if targets_front => front_routed_scroll(term, proxy, session, rest),
        "scroll" if is_cross => cross_scroll(term, rest),
        "scroll" => control_input::cmd_scroll(term, proxy, rest),
        "dims" => control_query::cmd_dims(term, session, proxy),
        // `metrics`: live render/latency counters (process-global; the active tab's grid
        // supplies rows/cols), so a driver can measure responsiveness directly.
        "metrics" => control_query::cmd_metrics(Some(term), rest),
        "lines" => control_query::cmd_lines(term),
        // `offscreen`: rows an alt-screen app scrolled off its top (Claude Code repaints
        // in place, so `lines` stays 0). Reads the target's own archive, so it is
        // cross-session correct.
        "offscreen" => control_query::cmd_offscreen(term, rest),
        // `mainscreen`: force the target's primary screen when a killed app left the
        // alternate one up (its last frame goes to `offscreen` first). Host-based on the
        // resolved sid like `select`. It takes no argument; a tail is not consent to
        // move a running app's screen.
        "mainscreen" => refuse_stray_args(verb, rest)
            .unwrap_or_else(|| control_query::cmd_mainscreen(&host, session)),
        "line" => control_query::cmd_line(term, rest),
        "modes" => control_query::cmd_modes(term),
        // `custody` -> WHO last took the reading position or the highlight, by name.
        // Read-side: it reports the engine's own custody record and no screen content.
        "custody" => control_query::cmd_custody(term),
        // `appstatus`: what aterm is doing on its own initiative (a toolchain install,
        // an update download): live status-bar rows plus finished ones still in the
        // ring. Session-independent App state.
        "appstatus" => control_query::cmd_appstatus(proxy),
        // `messages` -> the message log (see `dispatch_app_verb`); App state.
        "messages" => control_query::cmd_messages(proxy, rest),
        "title" => control_query::cmd_title(term),
        "cwd" => control_query::cmd_cwd(term),
        "blocks" => control_selection::cmd_blocks(&host, session, rest),
        // `blocktext <id> [trim]`: the arm's helper checks the grammar and passes only
        // `<id>` to the shared emitter; `trim` drops trailing blank rows as `text trim`
        // does.
        "blocktext" => control_query::cmd_blocktext_args(rest, |id| {
            control_selection::cmd_blocktext(&host, session, id)
        }),
        "wait" => control_selection::cmd_wait(&host, session, rest),
        "colors" => control_query::cmd_colors(term),
        "select" => control_selection::cmd_select(&host, session, rest),
        "selection" => control_selection::cmd_selection(&host, session),
        "copy" => control_selection::cmd_copy(&host, session),
        // `cast` reads the target session's own asciicast recorder, not the shared
        // renderer, so it is correct cross-session (no `is_cross` guard). `cast frames
        // [count=N]` expands the recording into keyframe screens (line-framed like
        // `text`); bare `cast` is the byte-framed asciicast.
        "cast" if rest.split_whitespace().next() == Some("frames") => {
            control_session::cmd_cast_frames(ctx, rest.strip_prefix("frames").unwrap_or("").trim())
        }
        // `cast drift [max_runs=<k>] [rows=<n>] [seed=…]` replays the target's recording
        // against the target's engine (line-framed); both locks are the target's, so it
        // is cross-session correct. The seam policy is asked before either lock. A
        // wait-lane verb (`is_wait_request`): it settles 60 ms and folds the recording.
        "cast" if rest.split_whitespace().next() == Some("drift") => {
            let policy = crate::app_render::pty_resize_policy(master);
            control_session::cmd_cast_drift(
                term,
                &ctx.cast,
                policy,
                rest.trim_start().strip_prefix("drift").unwrap_or("").trim(),
            )
        }
        "cast" => control_session::cmd_cast(ctx),
        // `temporal [tick]` reconstructs the target's screen at a past instant from its
        // own temporal recorder, cross-session correct like `cast`. It also reads the
        // target's engine (its own lock, never nested with the recorder's) for resizes
        // not yet on the spine (`pending_resizes=`).
        "temporal" => control_session::cmd_temporal(ctx, term, rest),
        // `history [<n>] [since=<id>]`: the session's turn ledger (what was driven and
        // what settled). Named `history` so it never collides with the `turn` driver.
        "history" => control_session::cmd_history(ctx, rest),
        // `meta`: read/write the target session's user metadata (title, description,
        // icon). Write sub-forms were escalated to WriteInput by `escalated_op`. On an
        // actual change the arm (not the headless-testable handler) posts
        // `Wake::MetaChanged` so tab labels repaint, and notifies subscribers so an
        // `events` watcher drains `EVENT <sid> meta …` at once.
        // `meta set|unset supervisor` is answered first: it is Owner-writable and may
        // bind to this connection, which only this seam knows. A non-Owner `questions`
        // or `autopilot` write is refused next (each decides whether the supervisor
        // answers for the person); an Owner's falls through to `cmd_meta`'s closed set.
        // An `autopilot` word that moved rings the window's host from the store write
        // itself (`session_timeline::apply_meta_value`), and the `Wake::MetaChanged`
        // below repaints the session's footer (its chip) with the rest.
        "meta" => {
            let prove = |holder: &str| prove_self_claim(holder, master);
            let (resp, changed) = control_session::cmd_meta_supervisor(
                ctx,
                scope,
                serving_connection(),
                &prove,
                rest,
            )
            .or_else(|| {
                control_session::cmd_meta_harness_act(ctx, serving_connection(), &prove, rest)
            })
            .or_else(|| control_session::meta_questions_denied(scope, rest))
            .unwrap_or_else(|| control_session::cmd_meta(term, store, session, ctx, rest));
            if changed {
                // A proven claim's record is read again at each meta-change
                // while its identity is live (`self_claim`, the contract §4).
                crate::self_claim::meta_changed(ctx);
                let _ = proxy.send_event(Wake::MetaChanged { session });
                if subscribers.any() {
                    subscribers
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .notify(session);
                }
            }
            resp
        }
        // `timeline`: the target session's event timeline (the lifecycle twin of
        // `history`), a pure observer of the ctx ring, cross-session correct.
        "timeline" => control_session::cmd_timeline(ctx, rest),
        // `log`: the target session's SESSION LOG (session > run > turn > step),
        // a pure reader of the ctx's timeline lock like `timeline`.
        "log" => control_session::cmd_log(ctx, rest),
        // `brief`: the target session's token-budgeted rollup; `brief all` (the
        // instance's) was escalated to Owner-only by `escalated_op`.
        "brief" => control_session::cmd_brief(ctx, store, rest),
        // `mark`: an attributed, rate-limited CLAIM in the target session's log
        // (design §7.4). Write-class, so an edge needs `WriteInput`; `by=` is the
        // connection's; it never acts ([`dispatch_mark`]).
        "mark" => dispatch_mark(ctx, scope, session, rest, subscribers),
        // `resizes`: the target session's resize ledger (what re-gridded it, the runs,
        // and whether the app drew over displaced content), from its own engine and
        // timeline, cross-session correct.
        "resizes" => crate::resize_ledger::cmd_resizes(ctx, term, rest),
        // `status`: the target session's Subject + classified Status. A main-thread hop,
        // since the classifier is `App` state owned by the event loop.
        "status" => control_media::cmd_session_status(proxy, session, rest),
        // `harness policy`: the target session's `[harness]` policy, the facts a
        // main-thread hop (the policy cell, the host, the session's words), the
        // rules file read and the line made here. An edge reads only `effective=`.
        "harness" => crate::harness_policy_verb::cmd(rest, scope.is_owner_class(), || {
            control_media::call_main(proxy, |tx| Wake::ReadHarnessPolicy { session, reply: tx })
                .map_err(|e| e.to_string())
                .and_then(|facts| facts)
        }),
        // `sessions`/`grant`/`revoke`/`whoami` are handled self-scoped above.
        _ => "ERR unknown verb (try: help)\n".to_string(),
    });
    // Input observation is completed by the exact event's nonempty accepted
    // receipt, including ordered-writer completion; a reply is not byte evidence.
    // Stamp the `content_seq` baseline on a successful input-write reply so a driver
    // can read a seq, write, then `await seq <that+1>` race-free (` seq=<n>` before the
    // trailing newline; ERR untouched). A warned write then adds
    // ` warn=input-unread input_bytes=<n> input_wait_ms=<ms>` on the first line
    // (`input_stall::with_warning`).
    crate::input_stall::with_warning(stamp_input_seq(verb, resp, term), warning.as_deref())
}

/// Append ` seq=<content_seq>` to a successful input-write reply. The seq is the target
/// grid's content baseline right after the write is dispatched, so `await seq <n+1>`
/// waits for the output it causes. Only input-write verbs with an `OK` reply are
/// stamped, before the trailing newline so the reply stays one line.
fn stamp_input_seq(verb: &str, resp: String, term: &Arc<Mutex<Terminal>>) -> String {
    if !matches!(verb, "send" | "feed" | "key" | "ctrl" | "mouse" | "paste")
        || !resp.starts_with("OK")
    {
        return resp;
    }
    let seq = term_lock(term).content_seq();
    match resp.strip_suffix('\n') {
        Some(head) => format!("{head} seq={seq}\n"),
        None => format!("{resp} seq={seq}"),
    }
}

/// The wire name of a [`CursorStyle`]: its variant in lowercase snake_case.
pub(crate) fn cursor_style_name(style: CursorStyle) -> &'static str {
    match style {
        CursorStyle::BlinkingBlock => "blinking_block",
        CursorStyle::SteadyBlock => "steady_block",
        CursorStyle::BlinkingUnderline => "blinking_underline",
        CursorStyle::SteadyUnderline => "steady_underline",
        CursorStyle::BlinkingBar => "blinking_bar",
        CursorStyle::SteadyBar => "steady_bar",
        CursorStyle::Hidden => "hidden",
        CursorStyle::HollowBlock => "hollow_block",
        CursorStyle::Bolt => "bolt",
        // the enum is non-exhaustive; name future variants when they exist
        _ => "unknown",
    }
}

/// Whether `rest` carries the `--json` / `json` flag, and `rest` without that token,
/// so each verb's positional parse runs unchanged. Without the flag returns
/// `(false, rest.to_string())`.
pub(crate) fn take_json_flag(rest: &str) -> (bool, String) {
    let mut json = false;
    let mut kept: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if tok == "--json" || tok == "json" {
            json = true;
        } else {
            kept.push(tok);
        }
    }
    (json, kept.join(" "))
}

/// The reply for an explicit `--json`/`json` on a verb with no structured emitter:
/// `Some("ERR json: not supported for <verb>\n")` for the allowlisted verbs below,
/// `None` to fall through to the text emitters with the original `rest`.
///
/// Allowlist, fail-open: only verbs whose grammar can never carry a literal
/// `json`/`--json` token as data are refused. Payload-bearing verbs (`send --json`,
/// `turn … json`, `search json`, `await match json`) must receive the token verbatim.
fn json_unsupported(verb: &str) -> Option<String> {
    matches!(
        verb,
        "modes"
            | "custody"
            | "selection"
            | "colors"
            | "cell"
            | "line"
            | "lines"
            | "offscreen"
            | "title"
            | "cwd"
            | "history"
            | "copy"
            | "blocktext"
            | "wait"
            | "ready"
    )
    .then(|| format!("ERR json: not supported for {verb}\n"))
}

/// Post a reply-bearing [`InputEvent`] and block on the seam's [`InputOutcome`] (an
/// `mpsc` round-trip like `cmd_image`'s). Used by `resize` and the input verbs; the
/// caller maps the outcome to its reply. A control connection is always a
/// [`Source::Controller`].
fn post_input_reply(
    proxy: &EventLoopProxy<Wake>,
    batch: Vec<InputEvent>,
) -> Result<InputOutcome, String> {
    post_input_reply_to(proxy, batch, None)
}

/// [`post_input_reply`] with an explicit session target, for a verb whose `@<sid>`
/// resolved to the tab on screen. `None` targets the front tab; `Some` is re-resolved
/// on the event loop (see [`Wake::Input::session`]), so a racing tab switch falls back
/// to the hidden-session path instead of typing into the wrong tab.
fn post_input_reply_to(
    proxy: &EventLoopProxy<Wake>,
    batch: Vec<InputEvent>,
    session: Option<u64>,
) -> Result<InputOutcome, String> {
    post_input_reply_to_observed(proxy, batch, session, None)
}

fn post_input_reply_to_observed(
    proxy: &EventLoopProxy<Wake>,
    batch: Vec<InputEvent>,
    session: Option<u64>,
    mark: Option<crate::control_input_ack::InputMark>,
) -> Result<InputOutcome, String> {
    let fence = carried_fence();
    control_media::call_main(proxy, |tx| Wake::Input {
        batch,
        mark,
        src: Source::Controller,
        reply: Some(tx),
        session,
        fence,
    })
    .map_err(|error| format!("ERR input dispatch failed: {error}\n"))
}

/// One reading of the typing-momentum metric of the window hosting `session`
/// (`Wake::TypingMomentum`, answered by [`crate::App::typing_momentum_of`]), stamped
/// with the main thread's read instant: what `turn yield=` and `await momentum` solve
/// the release law's crossing from. `Err` names why there is no reading (no event
/// loop, a dropped or late reply) so the caller refuses the yield.
fn typing_momentum_reading(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
) -> Result<(f32, std::time::Instant), String> {
    control_media::call_main(proxy, |tx| Wake::TypingMomentum { session, reply: tx })
        .map_err(|error| error.to_string())
        .and_then(|r| r)
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "control_hangup_tests.rs"]
mod control_hangup_tests;
#[cfg(all(test, unix))]
#[path = "control_lease_pid_tests.rs"]
mod control_lease_pid_tests;
#[cfg(all(test, unix))]
#[path = "control_retire_tests.rs"]
mod control_retire_tests;
#[cfg(all(test, unix))]
#[path = "control_self_claim_tests.rs"]
mod control_self_claim_tests;
#[cfg(all(test, unix))]
#[path = "control_socket_handoff_tests.rs"]
mod control_socket_handoff_tests;
#[cfg(all(test, unix))]
#[path = "control_stall_tests.rs"]
mod control_stall_tests;
#[cfg(all(test, unix))]
#[path = "publish_ordering_conformance.rs"]
mod publish_ordering_conformance;
#[cfg(all(test, unix))]
#[path = "publish_when_committed_conformance.rs"]
mod publish_when_committed_conformance;

/// One `hold` request as a client's line carries it (`hold <sid> off`,
/// `@<sid> hold off`), split by [`request_head`] and answered by
/// [`dispatch_hold_verb`] from `scope`, as [`dispatch_before_session`]
/// answers it: the seam another module's test runs a command it read off a
/// record through. A line whose verb is not `hold` is answered `ERR`.
#[cfg(test)]
pub(crate) fn hold_line_for_test(store: &Store, line: &str, scope: Scope) -> String {
    let (selector, verb, rest) = request_head(line);
    if verb != "hold" {
        return format!("ERR not a hold line: {line:?}\n");
    }
    dispatch_hold_verb(rest, selector.as_ref(), scope, store)
}

/// `update check` under a dialog or a stalled main thread answers the main thread's
/// typed refusal, never an `OK` line whose empty status hides a staged build.
#[cfg(test)]
mod update_check_refusal_tests {
    use super::control_media::MainHopError;

    /// The refusal in its own words, behind the verb's prefix, which is what
    /// the harness's turned-away rule reads (`ERR <what>: main thread stalled`).
    #[test]
    fn a_refused_settings_read_is_answered_err_in_its_own_words() {
        let dialog = MainHopError::Reason("aterm is showing a dialog; answer it, then retry");
        assert_eq!(
            super::update_check_refused(&dialog),
            "ERR update check: aterm is showing a dialog; answer it, then retry\n"
        );
    }

    /// A per-process update candidate publishes nothing (no `latest` alias, no
    /// `graph/<sid>` entry, no served request) until the Commit releases its gate; the
    /// control server waits before either publication, and every other launch passes
    /// at once. `spawn` needs an event loop, so the source pin is only that it
    /// publishes through `publish_when_committed`, whose order
    /// `publish_when_committed_conformance` drives for real.
    #[test]
    fn a_per_process_candidate_publishes_only_after_the_commit() {
        assert!(super::await_commit_to_publish(None), "an ordinary launch");
        let gate = crate::spawn::DeferredReaderGate::closed();
        let waiter = {
            let gate = gate.clone();
            std::thread::spawn(move || super::await_commit_to_publish(Some(&gate)))
        };
        // The waiter PARKED on the closed gate — a fact, where 100 ms of
        // sleep passed vacuously whenever the waiter was slow to start. Parked,
        // it can leave only by the release; ended first is the bug.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the candidate's publication parks on the closed gate",
            || (gate.parked() == 1 || waiter.is_finished()).then_some(()),
        );
        assert!(
            !waiter.is_finished(),
            "nothing is published before the Commit"
        );
        gate.release();
        assert!(
            waiter.join().unwrap(),
            "the Commit releases the publication"
        );
    }
}
