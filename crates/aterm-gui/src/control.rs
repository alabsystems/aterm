// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live introspection control socket (aterm introspection control protocol v1).
//!
//! Serves newline-delimited text requests on a Unix domain socket so an
//! out-of-process client can read the live screen, drive the shell, snapshot
//! pixels, drive selection and resize the engine and PTY of the running terminal.
//!
//! Threading: text/cursor/cell/search read the shared [`Terminal`] directly;
//! send/key/resize write the PTY master fd. `image` needs the renderer on the
//! main thread, so this thread posts an [`ImageReq`] as [`Wake::Image`] through
//! the main-thread RPC (`control_media::post_main`) and blocks on the reply;
//! the main thread renders, and the encode worker writes the PNG and replies
//! with the frame dimensions.
//!
//! In a window, `image` (and the SIGUSR1 snapshot) re-renders the current
//! terminal through the same renderer and splice/composite sequence as the
//! window present, including the live cursor blink phase and the hollow
//! unfocused cursor, so a focused blinking session may capture no cursor
//! pixels. It does not observe compositor visibility or scanout. Headless
//! sessions render a semantic frame with the blink phase pinned on. For
//! deterministic cursor state use the `cursor` verb.

use std::collections::VecDeque;
use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, LazyLock, Mutex};

use aterm_uds::{CtlListener, CtlStream};

use aterm_containment::log_denial;
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_session::{EdgeToken, Op, SessionId, decide_edge};
use aterm_types::control_request::{self, Head, Selector, Sid};
use aterm_types::control_verbs::{Body, Input, Resolved, input_of};
use aterm_window::event_loop::EventLoopProxy;

use crate::budget::{BudgetEvent, Slot, Slots, UnitBudget, UnitPermit};
use crate::control_auth::{self, AuthOutcome};
use crate::input::{Egress, InputEvent, InputOutcome, PasteFraming, ScrollIntent, Source};
use crate::session_store::Store;
use crate::subscribe::{self, InstanceStreams, PushOptions, PushScopes, Requested};
use crate::{SessionCtx, Wake, term_lock};

/// Read-only screen introspection serializers, dispatched as
/// `control_query::cmd_*` from [`dispatch_request`]'s class handlers. The file is
/// `src/control_query.rs`.
#[path = "control_query.rs"]
mod control_query;
// Query serializers reached as `crate::control::NAME` (`crate::subscribe`,
// `crate::frame_key`).
pub(crate) use control_query::{
    ScreenGen, StyledFrameSnapshot, gather_styled_frame, line_size_name, row_line_size, screen_gen,
    screen_text, serialize_styled_frame, trimmed_len, visible_row,
};
// The ground truths the published text is held to.
#[cfg(test)]
pub(crate) use control_query::rows_hash;
#[cfg(all(test, unix))]
pub(crate) use control_query::screen_stamp;
// A test reads a generation the way a fenced driver does, through the verb's
// own function (`control_tests.rs`); the subscribe tests read `screen` beside
// a `cells` watch the same way.
#[cfg(test)]
pub(crate) use control_query::{cmd_screen_styled_json, cmd_text_json_looked, text_args};
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

/// Input-injection verbs and their parsers
/// (key/ctrl/send/feed/signal/mouse/paste/focus/resize/scroll/tab), dispatched
/// as `control_input::cmd_*` from [`dispatch_request`]'s class handlers. The file
/// is `src/control_input.rs`.
#[path = "control_input.rs"]
mod control_input;
// Parsers reached as `crate::control::NAME` (`crate::input`, `input_stall`), and
// `no_such_tab`, the refusal `App::apply_tab_cmd_in` formats.
pub(crate) use control_input::{
    feed_bytes, no_such_tab, parse_ctrl, parse_key, parse_mouse, send_bytes,
};
// The worker's initial command uses the same final check-and-press law.
use control_input::INPUT_QUEUE_FULL;
pub(crate) use control_input::{GuardedInput, InputFence, input_if_fenced_observed};
// A footer fact's slash press guards its writes on the caret row as it read
// it (`crate::claude_slash`).
pub(crate) use control_input::compile_guard;
// `input_stall`'s restart test sends `signal term` through the verb itself,
// and the harness's restart tests drive `signal` and `restart` as the
// dispatch runs them (`signal_verb`, `restart_verb`).
#[cfg(all(test, target_os = "macos"))]
pub(crate) use control_input::{cmd_signal, restart_verb};
// The window's Stop Worker (Overview ⌫, the tab menu's sheet) signals the
// worker's agent through the verb's own body, in process, and the needs
// pill's Resume sends `signal cont` as the dispatch runs it
// (`app_presence::resume_if_stopped`).
pub(crate) use control_input::signal_verb;
// The press path's wiring test drives a `key`/`ctrl`/`paste` body through the
// control fence and then the verb's own post, as `serve_session_verb` does.
#[cfg(all(test, unix))]
pub(crate) use control_input::{control_fence, paste_event};

/// Selection/copy/block verbs (`select`/`selection`/`copy`/`blocks`/
/// `blocktext`/`wait`), implemented in the winit-free `aterm-control` crate over
/// a [`SessionHost`](aterm_control::SessionHost); aliased for [`serve_session_verb`].
use aterm_control::selection as control_selection;
// Smart-selection gesture helpers for `app_mouse.rs`'s double/triple click;
// they take a bare `&mut Terminal`.
pub(crate) use aterm_control::selection::{select_line, select_word, word_span};
// Response-framing primitives, re-exported so `crate::control::NAME` and
// `super::` callers resolve.
#[cfg(test)]
pub(crate) use aterm_control::wire::json_escape;
pub(crate) use aterm_control::wire::{json_escape_into, pct_encode, visible_char};
use aterm_control::wire::{json_ok, json_str_field};

/// This crate's [`SessionHost`](aterm_control::SessionHost): a borrowed adapter
/// over the target [`dispatch_session`] resolved. The file is `src/control_host.rs`.
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
/// `control_media::cmd_*` from [`dispatch_request`]'s class handlers. The file is
/// `src/control_media.rs`.
#[path = "control_media.rs"]
// `pub(crate)` so sibling verb modules (`control_privacy`) can reach
// `call_main`.
pub(crate) mod control_media;
/// The aimed-spawn target: built in `control_media::cmd_spawn`, carried in
/// `Wake::SpawnSession`.
pub(crate) use control_media::SpawnAim;

/// The handshake's `ERR auth` audit line, thinned per refusal kind; and the
/// fleet's refusals (`crate::fleet_listen`, the fleet gate), thinned by the
/// same rule.
#[path = "control_denials.rs"]
pub(crate) mod control_denials;
/// `await sys`: an agent waits for free space on its volume
/// (`control_disk::cmd_await_sys`). The file is `src/control_disk.rs`.
#[path = "control_disk.rs"]
pub(crate) mod control_disk;
/// Session-graph and capability verbs (`sessions`/`family`/`ready`/`cast`/
/// `edges`/`grant`/`revoke`/`whoami`), dispatched as `control_session::cmd_*`
/// from [`dispatch_request`]'s class handlers. The file is `src/control_session.rs`.
#[path = "control_session.rs"]
mod control_session;
#[cfg(test)]
pub(crate) use control_session::cmd_sessions_store;
// The turn-id counter a self-update handoff carries (`crate::seamless`).
#[cfg(any(unix, test))]
pub(crate) use control_session::turn_ids_for_handoff;
#[cfg(unix)]
pub(crate) use control_session::turn_ids_past_handoff_ceiling;
pub(crate) use control_session::{TURN_ID_RESERVE, raise_turn_ids, turn_ids_minted};
// A turn id minted as `turn` mints one, for the crash-restore tests.
#[cfg(all(test, unix))]
pub(crate) use control_session::mint_turn_id_for_test;
// A test that reads the turn-id count twice counts on its own.
#[cfg(test)]
pub(crate) use control_session::own_turn_ids;
// The two reads a handoff's end-to-end test drives the harness's report reader
// (`Session::report`) through (the handoff is unix-only, and so is that test).
#[cfg(all(test, unix))]
pub(crate) use control_query::cmd_offscreen;
/// `meta`'s who-set-it fields, for the unix restore test that carries a
/// setter (design §11.7.5).
#[cfg(all(test, unix))]
pub(crate) use control_session::autopilot_by_fields;
/// The `cast` read, for the recorder's serializer tests.
#[cfg(test)]
pub(crate) use control_session::cmd_cast;
#[cfg(all(test, unix))]
pub(crate) use control_session::cmd_history;
/// The `meta` verb, for the need publication's tests (what a write answers
/// is what the dispatch arm wakes the main thread on) and Autopilot's timer
/// tests (`crate::autopilot_timers`), which drive the wire's `until=` against
/// the App's index.
#[cfg(test)]
pub(crate) use control_session::cmd_meta;
#[cfg(test)]
pub(crate) use control_session::harness_act_face;
/// The `turn` composite and its io, for the tests that drive a REAL turn
/// through the lease seam (the session log's and the input stall's).
#[cfg(test)]
pub(crate) use control_session::{TurnIo, cmd_turn};
/// The `log` and `brief` reads, for the session log's own tests.
#[cfg(test)]
pub(crate) use control_session::{cmd_brief, cmd_log, cmd_log_json};
/// The same store write by a named writer and the Owner rule's refusal,
/// for the tests of who set a session's Autopilot (`crate::autopilot`'s
/// `set_by_tests`, design §11.7.5).
#[cfg(test)]
pub(crate) use control_session::{cmd_meta_by, meta_questions_denied};

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
    /// The stats Deck's rows in the frame (`App::deck_rows_px`): its top and
    /// height in device px, both 0 while the window draws none. Part of
    /// `head`: under macOS's titlebar its foot, over a Linux or Windows strip
    /// band its top.
    pub(crate) deck_y: u32,
    pub(crate) deck_h: u32,
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

/// `log n` for `scope`: the instance's node log for an Owner-class
/// connection; any other is refused (`ERR denied`, audited), as is a selector.
fn node_log_reply(rest: &str, selector: Option<&Selector>, scope: Scope) -> String {
    if let Some(refusal) = owner_only_refusal("log n", selector, scope) {
        log_denial(
            AUDIT_SUBSYSTEM,
            "log n",
            aterm_containment::mode_or_containment(),
            "the node log is owner-only",
        );
        return refusal;
    }
    crate::session_log_node::cmd_node_log(crate::session_log_node::node_log(), rest)
}

/// A FLEET peer's `survey` ([`Scope::Fleet`]): the owner-only verb whose row
/// the fleet gate admits ([`fleet_refusal`], which has already refused a
/// selector), answered at the level the peer's listener proved — `[fleet]
/// allow` — and never more, within the reply a peer reads
/// (`aterm_net::fleet::REPLY_MAX`). `None` for any other verb or scope, which
/// the owner-only gate then decides.
fn fleet_survey(verb: &str, rest: &str, scope: Scope, store: &Store) -> Option<String> {
    #[cfg(any(unix, test))]
    if let Scope::Fleet { content, drive } = scope
        && verb == "survey"
    {
        let most = if content {
            crate::survey::SurveyLevel::Content
        } else {
            crate::survey::SurveyLevel::Facts
        };
        return Some(crate::survey::cmd_survey(
            store,
            rest,
            most,
            aterm_net::fleet::REPLY_MAX,
            &crate::survey::Here {
                drive,
                ..crate::survey::Here::now()
            },
        ));
    }
    let _ = (verb, rest, scope, store);
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

/// The window an App-target verb acts on ([`app_route`]).
enum AppAim {
    /// No selector or `@.`: the front window. A bare `@` never gets here
    /// ([`empty_selector_refusal`]).
    Front,
    /// An Owner's `@<sid>` naming a session this instance hosts. `spawn`, `tab`
    /// and `panes` act on the window hosting it, `theme`'s session forms on the
    /// session itself and `edit` beside its tab; every other App verb acts on
    /// the front window, the selector having only routed the request here.
    Aimed(Target),
}

impl AppAim {
    /// The aimed session's local id, `None` for the front window: what
    /// `cmd_spawn`, `cmd_tab_aimed`, `cmd_panes`, `cmd_theme` and `cmd_edit`
    /// take as their aim.
    fn session(&self) -> Option<u64> {
        match self {
            AppAim::Front => None,
            AppAim::Aimed((_, _, session, _)) => Some(*session),
        }
    }
}

/// An App-target request's aim, or the line refusing it.
///
/// The authority verdict is [`native_control_decision`]'s App case (Tier-1 pins
/// `edge_app_allowed = 0`), asked before `resolve` runs: an Edge is denied every
/// App verb before its selector is looked up, so it learns nothing about which
/// sessions exist, and a write-input edge on one session cannot spawn or close
/// tabs in that session's window. An Owner's explicit selector is then resolved
/// once: a session here is the aim, a dead one is `ERR no such session`, and an
/// id two instances answer to is refused by name. Non-App verdicts fail closed.
fn app_route(
    principal: NativeControlPrincipal,
    selector: Option<&Selector<'_>>,
    resolve: impl FnOnce(&Selector<'_>) -> Resolution,
) -> Result<AppAim, String> {
    match native_control_decision(false, true, principal, NativeControlTarget::App) {
        NativeControlDecision::WithoutSession => {}
        NativeControlDecision::Denied => return Err("ERR denied\n".to_string()),
        NativeControlDecision::ResolveSession
        | NativeControlDecision::NoActiveTerminal
        | NativeControlDecision::NoSuchSession => {
            return Err("ERR invalid control target\n".to_string());
        }
    }
    let Some(selector) = selector.filter(|s| !s.is_self()) else {
        return Ok(AppAim::Front);
    };
    match resolve(selector) {
        Resolution::Hosted(target) => Ok(AppAim::Aimed(target)),
        Resolution::SelfTok => Ok(AppAim::Front),
        Resolution::Missing => Err("ERR no such session\n".to_string()),
        Resolution::Ambiguous(refusal) => Err(refusal),
    }
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
    /// A FLEET peer (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5): a
    /// connection the fleet listener ([`crate::fleet_listen`]) admitted on a proof
    /// of the fleet secret, handed over as the near end of a `socketpair` and
    /// pre-resolved like [`Self::Bridge`]. No token crosses the wire, and it is
    /// never owner-class and holds no op: [`fleet_refusal`], the first step of
    /// [`serve_request_line`], refuses it every verb whose row is not marked
    /// `fleet`, every selector and every frame. Only a drive connection's
    /// admitted lines go further: [`drive_admission`], before that gate, serves
    /// each line of its allowlist as a [`Self::Owner`] line credited to its
    /// node, and refuses the rest. `content` is the level the
    /// listener proved: `[fleet] allow = "content"` (or `"drive"`), else facts
    /// only.
    #[cfg(any(unix, test))]
    Fleet {
        /// Whether this peer is served content as well as facts.
        content: bool,
        /// Whether this peer is a node this Mac lets drive it (the step-in,
        /// `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1): its
        /// survey says `drive=1`, and on the connection its drive op opened,
        /// whose thread names the node ([`crate::fabric::DrivingAs`]), the
        /// drive gate admits its allowlist ([`drive_admission`]).
        drive: bool,
    },
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
            #[cfg(any(unix, test))]
            Scope::Fleet { .. } => false,
        }
    }

    /// The principal [`native_control_decision`] judges: Owner-class or Edge.
    fn principal(self) -> NativeControlPrincipal {
        if self.is_owner_class() {
            NativeControlPrincipal::Owner
        } else {
            NativeControlPrincipal::Edge
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

/// The fine [`Op`] an op class needs. `None` for [`OpClass::Owner`]: the
/// privilege/identity and build-meta rows, which their [`Access`] gate answers
/// before any op check, so no op gate admits them.
///
/// [`OpClass::Owner`]: aterm_types::control_verbs::OpClass::Owner
/// [`Access`]: aterm_types::control_verbs::Access
fn op_of(class: aterm_types::control_verbs::OpClass) -> Option<Op> {
    use aterm_types::control_verbs::OpClass;
    match class {
        OpClass::Read => Some(Op::ReadScreen),
        OpClass::Write => Some(Op::WriteInput),
        OpClass::Signal => Some(Op::Signal),
        // `settings` and `copy` have their own fine ops, so inherited base
        // edges cannot reach them; only Owner can.
        OpClass::ConfigWrite => Some(Op::ConfigWrite),
        OpClass::ClipboardWrite => Some(Op::ClipboardWrite),
        OpClass::Owner => None,
    }
}

/// The op-class a verb's ROW needs, from [`VERBS`] (the one verb table, from which
/// the help catalog is generated and against which the dispatch router is
/// checked), whatever its arguments: what the relay and
/// [`cross_session_authorized`] gate on. `ReadScreen` observes; `WriteInput`
/// injects human input and drives the app; `Signal` sends signals; `ConfigWrite`
/// (settings) and `ClipboardWrite` (copy) are separate fine ops an inherited base
/// edge cannot reach. `None` for Owner-only or meta verbs (gated before the op
/// check) or an unknown verb (`ERR unknown verb`). The dispatch gate reads the
/// whole request instead ([`need_of`]). `turn` also requires read authority for
/// scoped callers, enforced in its arm.
///
/// [`VERBS`]: aterm_types::control_verbs::VERBS
fn required_op(verb: &str) -> Option<Op> {
    aterm_types::control_verbs::spec(verb).and_then(|s| op_of(s.op))
}

/// What a request needs at the dispatch's op gate: a fine [`Op`] a granted edge
/// can satisfy, or Owner-only where no op expresses it. Owner-class scopes satisfy
/// both ([`scope_holds_need`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Need {
    /// Requires the fine op — `scope_holds_op` decides (an edge granted exactly this op
    /// passes; Owner always passes).
    Op(Op),
    /// Requires the per-instance Owner token — no fine op is sufficient (a scoped Edge,
    /// however privileged, is DENIED).
    OwnerOnly,
}

/// What the request `verb rest` needs at the dispatch's op gate, `form` being its
/// resolution in the verb table ([`aterm_types::control_verbs::spec_for`]): the
/// [`argument_escalation`] the table cannot state, else the form's owner-only
/// flag, else its op. The form replaces its row's facts, so `meta set`, `theme
/// set|commit|<any other word>`, `theme session set|clear`, `inbox seen`, `family
/// <sid>`, `pointer click|mods`, `task mark` and `window|controls connections`
/// need what their table form says, and a plain WriteInput edge cannot tunnel to
/// a fine op through any of them. `None` for an [`OpClass::Owner`] row, which no
/// op gate admits.
///
/// [`OpClass::Owner`]: aterm_types::control_verbs::OpClass::Owner
fn need_of(form: &aterm_types::control_verbs::Resolved, verb: &str, rest: &str) -> Option<Need> {
    if let Some(need) = argument_escalation(verb, rest) {
        return Some(need);
    }
    if form.owner_only {
        return Some(Need::OwnerOnly);
    }
    op_of(form.op).map(Need::Op)
}

#[cfg(test)]
#[test]
fn system_state_open_requires_the_config_authority() {
    assert_eq!(
        argument_escalation("open", "app system-state"),
        Some(Need::Op(Op::ConfigWrite))
    );
    assert_eq!(
        argument_escalation("open", "app settings"),
        Some(Need::Op(Op::ConfigWrite))
    );
}

/// Authority that depends on an argument's VALUE, which no table form can match:
/// seams where a plain `WriteInput` edge could otherwise reach a fenced op.
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
///     verb, which [`dispatch_app`] refuses every Edge before this is asked
///     ([`app_route`], by `native_control_decision`).
///   * `open menu` / `open palette` and `open update` are Owner-only, like
///     their `invoke` twins, and `open overview` (its old `connections`
///     keyword too) lists every session, Owner-only like `flows`.
///   * `spawn connected=…`, `brief all`, `log n` (the instance's
///     node log), `sys`'s wide forms and `await sys … scope=aterm` are
///     Owner-only.
///
/// Returns the escalated requirement, else `None` (the table form decides).
fn argument_escalation(verb: &str, rest: &str) -> Option<Need> {
    use crate::menu::{InvokeAuthority, MenuAction};
    // Classify what the verb will run, not its raw spelling: `hwkey`, `pointer` and
    // `invoke` take a leading `unread=ok` off before they parse (`take_app_unread_ok`),
    // so `pointer unread=ok click` or `invoke unread=ok OpenPalette` must escalate
    // exactly as the bare forms do.
    let (_, rest) = control_input::take_app_unread_ok(input_of(verb), rest);
    match verb {
        "invoke" => {
            let action = MenuAction::from_invoke_name(rest.trim())?;
            match action.invoke_authority() {
                // Benign: the base `WriteInput` gate already covers it.
                InvokeAuthority::WriteInput => None,
                InvokeAuthority::ConfigWrite => Some(Need::Op(Op::ConfigWrite)),
                InvokeAuthority::ClipboardWrite => Some(Need::Op(Op::ClipboardWrite)),
                // The palette (a gateway to EVERY action) and the update-apply twins:
                // no single fine op expresses "only the god token", so require Owner.
                InvokeAuthority::OwnerOnly => Some(Need::OwnerOnly),
                // The person's own: at least Owner here, so an edge is refused
                // at the gate like every Owner-only seam, and `cmd_invoke`
                // refuses the Owner token itself (`ERR denied`).
                InvokeAuthority::PersonOnly => Some(Need::OwnerOnly),
            }
        }
        // Classify by the target token only: `cmd_open` treats a trailing
        // `close` as the dismiss flag, and `open <target> close` must escalate
        // like `open <target>`.
        "open"
            if matches!(
                aterm_types::app_inspection::parse_open_app(rest),
                Ok(aterm_types::app_inspection::OpenAppRequest::Settings { .. }
                    | aterm_types::app_inspection::OpenAppRequest::SystemState)
            ) =>
        {
            // The native setup/config apps are the same capability as `open prefs`;
            // classify it here, before the main-thread hop.
            Some(Need::Op(Op::ConfigWrite))
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
            Some(Need::OwnerOnly)
        }
        // `act app/v1 ...` resolves its view kind only on the main thread, so the
        // gateway requires ConfigWrite rather than trusting the action spelling.
        "act" if aterm_types::app_inspection::parse_act(rest).is_ok() => {
            Some(Need::Op(Op::ConfigWrite))
        }
        // A connected spawn (`spawn connected=… of=<sid>`) mints session-connection
        // authority, and a worker (`agent=`, with its `model=`, its
        // `from=` and its task frame's `len=`) starts an agent in a folder
        // trusted for it: all Owner-only. A plain `spawn [cwd=…]` keeps
        // WriteInput.
        // Tokenized with `split_quoted_tokens`, like `parse_spawn_args`, so no
        // quoting walks past the fence; unparseable input escalates too.
        "spawn"
            if control_media::split_quoted_tokens(rest).is_none_or(|tokens| {
                tokens.iter().any(|t| {
                    ["connected=", "agent=", "model=", "from=", "len="]
                        .iter()
                        .any(|key| t.starts_with(key))
                })
            }) =>
        {
            Some(Need::OwnerOnly)
        }
        // The Overview (`open overview`, and the map's old `connections`
        // keyword) lists every session of the instance: Owner-only like
        // `flows` (its readout and capture, `controls overview|connections`
        // and `window overview|connections`, are table forms). Classified by
        // the target token, so the `close` variant escalates too.
        "open"
            if matches!(
                rest.split_whitespace().next(),
                Some("overview" | "connections")
            ) =>
        {
            Some(Need::OwnerOnly)
        }
        "open" => match crate::app_introspect::AuxTarget::parse(
            rest.split_whitespace().next().unwrap_or(""),
        ) {
            // The Settings tab flips default-OFF security knobs => ConfigWrite.
            Some(crate::app_introspect::AuxTarget::Prefs) => Some(Need::Op(Op::ConfigWrite)),
            // The palette, software-update overlay and in-grid tab menu are
            // gateways, Owner-only like their `invoke` twins.
            Some(
                crate::app_introspect::AuxTarget::Menu
                | crate::app_introspect::AuxTarget::TabMenu
                | crate::app_introspect::AuxTarget::AutopilotMenu
                | crate::app_introspect::AuxTarget::FactMenu
                | crate::app_introspect::AuxTarget::Update,
            ) => Some(Need::OwnerOnly),
            _ => None,
        },
        // `brief all` reads every session's log, the whole roster: Owner-only
        // like `sessions`. `brief [<node>]` reads only its target. Classified by
        // the verb's own parser (which takes `all` anywhere among the
        // arguments), fail-closed: only a request it reads as a target's brief
        // stays a read, so no spelling of the instance-wide form passes as one.
        "brief" if !crate::session_log::BriefQuery::parse(rest).is_ok_and(|q| !q.all) => {
            Some(Need::OwnerOnly)
        }
        // `sys` reads other sessions' processes, folders or the instance's
        // parts only in these forms (the `aterm` view, `procs`/`jobs` beyond
        // the target's tab, `disk` of aterm or the machine, `instances=all`,
        // `name`): Owner-only like `sessions` and `brief all`. Classified by
        // the verb's own parser, fail-closed: only a request it reads as the
        // target's aggregates or own rows stays a Scoped read.
        "sys" if !aterm_vitals::sys::SysQuery::parse(rest).is_ok_and(|q| q.target_rows_only()) => {
            Some(Need::OwnerOnly)
        }
        // `await sys … scope=aterm` counts and names every tab's jobs:
        // Owner-only like `sys jobs scope=aterm`, by the wait's own parser,
        // which the verb runs first (a wait it cannot read reads nothing).
        "await"
            if rest.split_whitespace().next() == Some("sys")
                && crate::sys_host::await_reads_every_tab(rest) =>
        {
            Some(Need::OwnerOnly)
        }
        // `log n` reads the INSTANCE's node log (session log 5J-3): Owner-only
        // like `brief all`. Any bare `n` word names it (`n=<count>` is a count),
        // so no spelling of it passes as a session's read.
        "log" if crate::session_log_node::names_node_log(rest) => Some(Need::OwnerOnly),
        _ => None,
    }
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
/// base op gate and the [`need_of`] fence. Owner always holds; an `Edge`
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
        // A fleet peer holds no op on any session: the gate admits no verb that
        // asks one, and this answer keeps it so if one ever got past it.
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => false,
    }
}

/// Whether `scope` meets `need` against `ctx`'s session: a fine op as
/// [`scope_holds_op`] decides it, Owner-only for the Owner class alone.
fn scope_holds_need(scope: Scope, need: Need, ctx: &SessionCtx) -> bool {
    match need {
        Need::Op(op) => scope_holds_op(scope, op, ctx),
        Need::OwnerOnly => scope.is_owner_class(),
    }
}

/// `help [<verb> | --full | --json]` (alias `verbs`): the catalog of every verb
/// this build supports, generated from the `VERBS` table in aterm-types.
/// Answered for any authenticated scope before target resolution.
///
/// Bare: the short catalog (a three-line header and one summary row per verb,
/// under `SHORT_CATALOG_MAX_BYTES`). `help <verb>`: that verb's full entry,
/// wrapped. `help --full`: the complete catalog under the protocol header.
/// Anything else is `ERR usage`.
///
/// The three whole-catalog answers are pure functions of the compile-time
/// table, so each is built on its first ask ([`HELP_SHORT`], [`HELP_FULL`],
/// [`HELP_JSON`]) and copied out after; `help <verb>` wraps its one entry per
/// ask.
fn cmd_help(rest: &str) -> String {
    use aterm_types::control_verbs::spec;
    let tail = rest.trim();
    match tail {
        "" => return HELP_SHORT.to_string(),
        "--full" => return HELP_FULL.to_string(),
        "--json" => return HELP_JSON.to_string(),
        _ => {}
    }
    let usage = || "ERR usage: help [<verb> | --full | --json]\n".to_string();
    if tail.starts_with('-') {
        return usage();
    }
    // A verb, or the two words of a listed sub-form row (`inbox get`).
    let words: Vec<&str> = tail.split_whitespace().collect();
    match spec(&words.join(" ")) {
        Some(s) => {
            let lines = s.entry_lines();
            let mut body = format!("OK {}\n", lines.len());
            for line in lines {
                body.push_str(&line);
                body.push('\n');
            }
            body
        }
        None if words.len() > 1 => usage(),
        None => format!("ERR unknown verb '{tail}' (help lists them)\n"),
    }
}

/// Bare `help`, built once by [`help_short_catalog`].
static HELP_SHORT: LazyLock<Box<str>> = LazyLock::new(|| help_short_catalog().into_boxed_str());
/// `help --full`, built once by [`help_full_catalog`].
static HELP_FULL: LazyLock<Box<str>> = LazyLock::new(|| help_full_catalog().into_boxed_str());
/// `help --json`, built once by [`help_json_catalog`].
static HELP_JSON: LazyLock<Box<str>> = LazyLock::new(|| help_json_catalog().into_boxed_str());

/// Bare `help`: framing, the generated `--json` set, pointers to the other
/// forms, then one summary row per verb.
fn help_short_catalog() -> String {
    use aterm_types::control_verbs::{catalog_lines, json_verbs};
    let json_verbs = json_verbs().collect::<Vec<_>>().join("/");
    let mut body = format!(
        "# Framing: one \"OK …\"/\"ERR …\" status line, or \"OK <count>\" + <count> content lines; \
         EMPTY content = no data, never an error\n\
         # --json on {json_verbs} = structured output · \"@<sid> <verb>\" = run it on that session \
         (a bare @, what @$W is when $W is empty, is refused)\n\
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
fn help_json_catalog() -> String {
    use aterm_control::wire::json_escape;
    let rows: Vec<String> = aterm_types::control_verbs::VERBS
        .iter()
        .map(|s| {
            format!(
                "{{\"name\":\"{}\",\"op\":\"{}\",\"framing\":\"{}\",\"target\":\"{}\",\
                 \"access\":\"{}\",\"fleet\":{},\"summary\":\"{}\"}}",
                json_escape(s.name),
                s.op.wire_name(),
                s.framing.wire_name(),
                s.target.wire_name(),
                s.access.wire_name(),
                s.fleet,
                json_escape(s.summary),
            )
        })
        .collect();
    format!("OK 1\n{{\"verbs\":[{}]}}\n", rows.join(","))
}

/// `help --full`: the complete catalog under the full protocol header.
fn help_full_catalog() -> String {
    // "OK <count> …\n" plus <count> body lines so aterm-ctl streams it like other
    // list verbs. A fixed header (framing, cross-session, targeting, seeing
    // modes), then the catalog generated from `VERBS`; the `--json` list is the
    // table's JSON-emitting rows.
    let json_verbs = aterm_types::control_verbs::json_verbs()
        .collect::<Vec<_>>()
        .join("/");
    let header = &format!(
        r#"# Framing: every reply is a STATUS line ("OK …" / "ERR …") OR raw CONTENT lines. Content
# verbs prefix "OK <count>" then <count> lines — except the Bytes-framed verbs (cast,
# temporal, inbox get, outbox), which prefix "OK <nbytes>" then exactly that many raw bytes;
# aterm-ctl prints the content and consumes the OK line, so an EMPTY content reply means NO
# DATA — it is never an error (errors are always "ERR …"). Add "--json" to {json_verbs} for
# structured output; a verb without a json form answers "ERR json: not supported for <verb>"
# or, outside that allowlist, reads "--json" as an ordinary argument — do not probe with it.
# Cross-session: prefix a verb with "@<selector>" (e.g. "@s-ab12 text"); an "@" that names
# nothing (an empty $W in "@$W") is refused, never the active tab. Same-user only;
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

    /// An unknown verb is named back with the way to the list; a listed sub-form
    /// row is looked up by its two words; any other flag or extra token is a usage
    /// error, never the whole catalog.
    #[test]
    fn help_rejects_unknown_verbs_and_bad_tails() {
        assert_eq!(
            super::cmd_help("nosuch"),
            "ERR unknown verb 'nosuch' (help lists them)\n"
        );
        let inbox_get = super::cmd_help("inbox get");
        assert!(inbox_get.starts_with("OK "), "{inbox_get}");
        assert!(
            inbox_get
                .lines()
                .nth(1)
                .is_some_and(|row| row.starts_with("inbox get ")),
            "{inbox_get}"
        );
        assert_eq!(
            super::cmd_help(" inbox \t get "),
            inbox_get,
            "any separator"
        );
        for tail in [
            "-h",
            "--jsn",
            "image --full",
            "--full image",
            "--json image",
            "a b",
            "inbox nope",
            "inbox get extra",
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
    // `check` (stages a build) and `apply` (hands off or replaces the bundle on
    // disk) are Owner-only, like `invoke ApplyUpdate` / `open update`: the table's
    // `update` forms. Bare and `status` answer any scope. The in-session AI drives
    // with the Owner token.
    if aterm_types::control_verbs::spec_for("update", rest).is_some_and(|form| form.owner_only)
        && !scope.is_owner_class()
    {
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
                let activation = crate::update_control::published_activation();
                let staged = aterm_update::status(build).and_then(|mut st| {
                    fold_installed_activation(&mut st, activation.as_ref());
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
    let apply_snapshot = crate::control::control_media::call_main_within(
        proxy,
        std::time::Duration::from_secs(2),
        |reply| Wake::ReadUpdateControl { reply },
    )
    .ok();
    // The published activation is read (a lock and a clone) only when the event
    // loop cannot answer; its own stage outranks it.
    let published = apply_snapshot
        .is_none()
        .then(crate::update_control::published_activation)
        .flatten();
    fold_installed_activation(
        &mut st,
        crate::update_control::status_activation(apply_snapshot.as_ref(), published.as_ref()),
    );
    let staged_build = st
        .staged_build
        .map(|b| b.to_string())
        .unwrap_or_else(|| "-".to_string());
    let staged_version = st.staged_version.as_deref().unwrap_or("-");
    let staged_commit = st.staged_commit.as_deref().unwrap_or("-");
    // Historical wire key: a newer stage EXISTS. Apply permission and scheduling
    // are separate facts; a stage can be blocked or require manual action.
    let relaunch_ready = st.staged_build.is_some_and(|b| b > st.current_build);
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
/// bundle newer than this process, which the updater reducer stages in memory
/// (no `ready.toml`) and which outranks any download on disk. `stage` is the
/// reducer's own, so the line and `update apply` report what the reducer acts
/// on, and no verb probes the bundle (codesign and spctl are the facts
/// worker's). A download stage folds nothing: the ledger already names it.
fn fold_installed_activation(
    st: &mut aterm_update::UpdateStatus,
    stage: Option<&crate::native_updater_service::StagedUpdate>,
) {
    let Some(activation) = stage.filter(|stage| {
        st.enabled && stage.build > st.current_build && stage.is_installed_activation()
    }) else {
        return;
    };
    st.staged_build = Some(activation.build);
    st.staged_version = Some(activation.version.clone());
    st.staged_commit = activation.commit.clone();
    st.staged_dmg_sha256 = None;
    st.changelog = None;
    // The reducer names a bundle whose plist carries no version `build <N>`; the
    // sentence then goes without one.
    let version = Some(activation.version.as_str())
        .filter(|version| *version != format!("build {}", activation.build));
    // The reducer's sentence for the same state, word for word, so this verb and
    // Software Update never describe it twice.
    st.outcome = format!(
        "{} (ledger: {})",
        crate::native_updater_service::NativeUpdaterService::installed_activation_outcome(version),
        st.outcome
    );
}

#[cfg(test)]
mod installed_activation_fold_tests {
    use super::fold_installed_activation;
    use crate::native_updater_service::{StagedUpdate, installed_activation_digest};

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn ledger() -> aterm_update::UpdateStatus {
        aterm_update::UpdateStatus {
            enabled: true,
            installable: true,
            current_build: 10,
            staged_build: None,
            staged_version: None,
            staged_commit: None,
            staged_dmg_sha256: None,
            changelog: None,
            outcome: "up to date".into(),
            updated_at: String::new(),
            failing_checks: 0,
            failing_kind: String::new(),
            failing_applies: 0,
            failing_since: String::new(),
            failing_persistent: false,
            failing_checks_kind: String::new(),
            channel_unreadable: false,
            linux: None,
        }
    }

    const DOWNLOAD_OUTCOME: &str = "staged 0.39.5 (build 11) — verified and ready to apply";

    /// The ledger holding a downloaded build 11, which an activation of build 12
    /// outranks.
    fn ledger_with_download() -> aterm_update::UpdateStatus {
        aterm_update::UpdateStatus {
            staged_build: Some(11),
            staged_version: Some("0.39.5".into()),
            staged_commit: Some("f".repeat(40)),
            staged_dmg_sha256: Some("c".repeat(64)),
            changelog: Some("notes".into()),
            outcome: DOWNLOAD_OUTCOME.into(),
            ..ledger()
        }
    }

    fn activation(build: u64, version: &str) -> StagedUpdate {
        StagedUpdate {
            build,
            version: version.into(),
            commit: Some(COMMIT.into()),
            dmg_sha256: installed_activation_digest(build, COMMIT),
            changelog: None,
            generation: 1,
        }
    }

    /// The reducer's activation is the line's stage, in the reducer's words, over
    /// a download the ledger holds: the download's digest and changelog go with
    /// it (left, the posture would weigh the activation as that download and say
    /// `unreconciled`, and the line would print the download's notes). A
    /// version-less bundle (`build <N>`, as the reducer names it) is said
    /// without one.
    #[test]
    fn the_reducers_activation_is_the_stage_in_its_own_words() {
        for (version, said) in [("0.40.0", Some("0.40.0")), ("build 12", None)] {
            let mut st = ledger_with_download();
            fold_installed_activation(&mut st, Some(&activation(12, version)));
            assert_eq!(st.staged_build, Some(12), "{version}");
            assert_eq!(st.staged_version.as_deref(), Some(version));
            assert_eq!(st.staged_commit.as_deref(), Some(COMMIT), "{version}");
            assert_eq!(st.staged_dmg_sha256, None, "an activation has no download");
            assert_eq!(st.changelog, None, "nor the download's notes");
            assert_eq!(
                st.outcome,
                format!(
                    "{} (ledger: {DOWNLOAD_OUTCOME})",
                    crate::native_updater_service::NativeUpdaterService::installed_activation_outcome(
                        said
                    )
                )
            );
        }
    }

    /// Only an activation newer than this process, on an enabled updater,
    /// folds; a download stage, one not newer, a disabled updater and no stage
    /// leave the ledger's line as it was (each the negative control of the
    /// test above).
    #[test]
    fn nothing_but_a_newer_activation_folds() {
        let mut download = activation(12, "0.40.0");
        download.dmg_sha256 = "b".repeat(64);
        let mut disabled = ledger();
        disabled.enabled = false;
        for (mut st, stage) in [
            (ledger(), Some(download)),
            (ledger(), Some(activation(10, "0.39.0"))),
            (disabled, Some(activation(12, "0.40.0"))),
            (ledger(), None),
        ] {
            let before = format!("{st:?}");
            fold_installed_activation(&mut st, stage.as_ref());
            assert_eq!(format!("{st:?}"), before, "{stage:?}");
        }
    }
}

/// The scope a connection's first line authenticates to (with any folded-in
/// verb), or the denial `serve` logs before `ERR auth`.
///
/// Tier 1: the per-instance token is `Scope::Owner`, which is also what a
/// forward presents (the token beside the host's socket). Tier 2: otherwise it
/// is tried as an edge token against the active session's table
/// (`Scope::Edge`), and an edge-scoped connection never forwards on
/// (`proxy_forward_plan` forwards Owner-class only), so an edge cannot reach
/// past its grant. A line with no token is its own refusal kind
/// (`control_denials`).
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
            // A blind connection (no token) is its own refusal kind.
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
/// filename from `confine_image_path`), posts it as [`Wake::Image`] and blocks on
/// `reply` for `(width, height)`. Passing dir plus filename lets the writer
/// `openat` under a dir fd, so no path component can be swapped after the check.
#[derive(Debug)]
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

/// Captures in flight at once, path and in-memory alike. Each holds a request
/// lane while it captures, so this cap is what keeps captures off every lane
/// (the const assert beside [`CONTROL_INLINE_WAITS`]); it is also the count cap
/// on path artifacts, quarantined ones included ([`ArtifactBudgets`]).
const ARTIFACT_HANDOFF_LIMIT: usize = 4;
const ARTIFACT_HANDOFF_SUFFIX_UNITS: usize =
    crate::pinned_dir::PINNED_DIR_OPERATION_DESCRIPTOR_UNITS;
const ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT: usize =
    crate::pinned_dir::PINNED_DIR_OPEN_COMPONENT_LIMIT + ARTIFACT_HANDOFF_SUFFIX_UNITS;
/// Directory chains the best-effort cleanup worker may hold past their reply
/// permits: one, so a sweep spanning batches holds the only one.
const ARTIFACT_CLEANUP_LIMIT: usize = 1;
/// Cleanup tasks reserved and not yet run.
pub(crate) const ARTIFACT_CLEANUP_QUEUE_LIMIT: usize = 32;
pub(crate) const ARTIFACT_HANDOFF_BUSY: &str = "other captures are still pending; retry";

/// The capacity artifact verbs admit against. The shipping verbs use
/// [`Self::process`]; a test builds its own with [`Self::shipping`], at the same
/// caps, so no sibling test competes for its slots. Every permit keeps its
/// budget alive and gives back to it on whatever thread drops it.
pub(crate) struct ArtifactBudgets {
    /// Captures in flight ([`ARTIFACT_HANDOFF_LIMIT`]): taken at admission by
    /// both kinds, given back once an in-memory body is flushed or a path
    /// reply's acknowledgement is judged. A quarantine never holds one, so
    /// failed path handoffs cannot refuse an in-memory capture.
    in_flight: Arc<Slots>,
    /// Path artifacts and the directory chains they retain: at most
    /// [`ARTIFACT_HANDOFF_LIMIT`] of them charging at most
    /// [`ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT`] units, each its path's depth, so a
    /// few deep paths cannot defeat a count-only limit. Held through the
    /// acknowledgement and a failed one's quarantine (fail-closed). Its moves
    /// are the `ArtifactHandoffCapacity` machine's.
    descriptors: Arc<UnitBudget>,
    /// Root capacity reserved while one video artifact is live; its final lease
    /// hands it, with the pinned root, to the cleanup worker. Separate, so
    /// best-effort GC never starves an armed video's sweep.
    video: Arc<UnitBudget>,
    /// The directory chains the best-effort cleanup worker holds past their
    /// reply permits ([`ARTIFACT_CLEANUP_LIMIT`]).
    cleanup: Arc<Slots>,
    /// Places in the cleanup queue, reserved before the work becomes visible
    /// so a final drop's enqueue never blocks or fails.
    cleanup_queue: Arc<Slots>,
}

impl ArtifactBudgets {
    /// A fresh set at the shipping caps.
    pub(crate) fn shipping() -> Self {
        Self {
            in_flight: Arc::new(Slots::new(ARTIFACT_HANDOFF_LIMIT)),
            descriptors: Arc::new(UnitBudget::new(
                ARTIFACT_HANDOFF_LIMIT,
                ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT,
                observe_artifact_handoff,
            )),
            video: Arc::new(UnitBudget::new(
                ARTIFACT_HANDOFF_LIMIT,
                ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT,
                |_| {},
            )),
            cleanup: Arc::new(Slots::new(ARTIFACT_CLEANUP_LIMIT)),
            cleanup_queue: Arc::new(Slots::new(ARTIFACT_CLEANUP_QUEUE_LIMIT)),
        }
    }

    /// The process's own set, which every shipping artifact verb shares. In a
    /// test build it is the CALLING TEST THREAD's own set, made once per thread
    /// and leaked (a few bytes per test): the lib-test process runs up to 54
    /// tests at once over one cleanup worker, so one shared set let a sibling's
    /// backlog refuse this test's admission (`AdmissionRefused`, then `None`, at
    /// load 20-30). A permit keeps the budget it was charged to, so whichever
    /// thread drops it (the worker's, mostly) gives back to the right one. The
    /// caps are the shipping ones either way.
    pub(crate) fn process() -> &'static Self {
        #[cfg(test)]
        {
            thread_local! {
                static OWN: &'static ArtifactBudgets = Box::leak(Box::new(ArtifactBudgets::shipping()));
            }
            OWN.with(|own| *own)
        }
        #[cfg(not(test))]
        {
            static PROCESS: std::sync::LazyLock<ArtifactBudgets> =
                std::sync::LazyLock::new(ArtifactBudgets::shipping);
            &PROCESS
        }
    }

    /// Admission for a target-free body: an in-flight slot, and no directory
    /// chain because none is owned.
    pub(crate) fn try_reserve_in_memory(&self) -> Option<ReplyRetentionPermit> {
        Some(ReplyRetentionPermit {
            in_flight: self.in_flight.try_take()?,
            descriptors: None,
        })
    }

    /// Admission for a path artifact: an in-flight slot plus a lexical
    /// descriptor estimate before the path resolves; `PinnedDir` reconciles the
    /// exact depth before opening. A saturated request does no filesystem I/O.
    pub(crate) fn try_reserve_for_path(
        &self,
        path: &std::path::Path,
    ) -> Option<ReplyRetentionPermit> {
        let in_flight = self.in_flight.try_take()?;
        let descriptors = self.descriptors.try_take(artifact_descriptor_units(path))?;
        Some(ReplyRetentionPermit {
            in_flight,
            descriptors: Some(descriptors),
        })
    }

    /// Transfer one open directory chain to the cleanup worker. Paths outside
    /// the pinned-depth contract are refused.
    pub(crate) fn try_reserve_cleanup_for_path(
        &self,
        path: &std::path::Path,
    ) -> Option<ArtifactCleanupPermit> {
        if artifact_descriptor_units(path) > ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT {
            return None;
        }
        Some(ArtifactCleanupPermit {
            _slot: self.cleanup.try_take()?,
        })
    }

    /// One place in the cleanup queue.
    pub(crate) fn try_reserve_cleanup_queue(&self) -> Option<Slot> {
        self.cleanup_queue.try_take()
    }

    /// Pre-reserve the root capacity an armed video sweep will hand the
    /// cleanup worker, independent of best-effort cleanup admission.
    pub(crate) fn try_reserve_video_retention_for_path(
        &self,
        path: &std::path::Path,
    ) -> Option<VideoRetentionPermit> {
        Some(VideoRetentionPermit {
            _root: self.video.try_take(artifact_descriptor_units(path))?,
        })
    }
}

/// Capacity reserved before an artifact job starts: its in-flight slot and,
/// for a path artifact, its descriptor chain. Binding it to a guard carries
/// both through reply and ACK; a failed ACK's quarantine keeps only the chain.
#[derive(Debug)]
pub(crate) struct ReplyRetentionPermit {
    in_flight: Slot,
    descriptors: Option<UnitPermit>,
}

/// Admission for an in-memory artifact body: no path to preserve after the
/// bounded flush, so it is outside the acknowledgement/quarantine machine.
pub(crate) struct WriteOnlyRetention {
    _in_flight: Slot,
}

/// One directory chain held by the asynchronous retention worker.
pub(crate) struct ArtifactCleanupPermit {
    _slot: Slot,
}

/// A slot reserved while one video artifact is live. The final lease hands it,
/// with its pinned root, to the cleanup worker, so the final Drop's enqueue
/// cannot block or fail.
#[derive(Debug)]
pub(crate) struct VideoRetentionPermit {
    _root: UnitPermit,
}

/// The descriptor budget's moves, told to the `ArtifactHandoffCapacity`
/// machine.
fn observe_artifact_handoff(event: BudgetEvent) {
    match event {
        BudgetEvent::Acquire => artifact_handoff_acquire_anchor(),
        BudgetEvent::RefuseAtCap => artifact_handoff_refuse_anchor(),
        BudgetEvent::Release => artifact_handoff_release_anchor(),
        BudgetEvent::ReconcileGrow => artifact_handoff_reconcile_grow_anchor(),
        BudgetEvent::ReconcileShrink => artifact_handoff_reconcile_shrink_anchor(),
        BudgetEvent::RefuseReconcile => artifact_handoff_refuse_reconcile_anchor(),
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

/// Type-erased ownership that stays live until a reply is consumed and
/// acknowledged; concrete guards revalidate filesystem identities at the wire.
pub(crate) struct ReplyRetention {
    guard: Option<Box<dyn WireRetention>>,
    /// How far the guard got, read only by the drop to tell the model an
    /// unprepared guard was aborted (`AbortQueued`): kept where the anchors are
    /// (test and debug builds).
    #[cfg(any(test, debug_assertions))]
    phase: ReplyRetentionPhase,
    /// A path artifact's directory chain, which a quarantine keeps.
    _descriptors: Option<UnitPermit>,
    /// Given back when the handoff's outcome is judged: a quarantine keeps the
    /// guard and its chain, not a capture's place in flight.
    in_flight: Option<Slot>,
}

#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplyRetentionPhase {
    Queued,
    Prepared,
    Classified,
}

impl ReplyRetention {
    #[cfg(test)]
    fn new(guard: impl WireRetention + 'static) -> Self {
        Self {
            guard: Some(Box::new(guard)),
            phase: ReplyRetentionPhase::Queued,
            _descriptors: None,
            in_flight: None,
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
        #[cfg(any(test, debug_assertions))]
        {
            if result.is_ok() {
                self.phase = ReplyRetentionPhase::Prepared;
            }
        }
        result
    }

    /// Note that the guard's outcome is decided, so its drop aborts nothing.
    fn classified(&mut self) {
        #[cfg(any(test, debug_assertions))]
        {
            self.phase = ReplyRetentionPhase::Classified;
        }
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
        self.classified();
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
        self.classified();
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
        self.classified();
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
        self.classified();
    }

    #[cfg(any(test, debug_assertions))]
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
    /// ancestor chain opens (called by `PinnedDir`). A permit with no chain
    /// has no path to admit.
    pub(crate) fn try_reconcile_for_resolved_path(&mut self, path: &std::path::Path) -> bool {
        self.descriptors
            .as_mut()
            .is_some_and(|chain| chain.reconcile(artifact_descriptor_units(path)))
    }

    pub(crate) fn retain(self, guard: impl WireRetention + 'static) -> ReplyRetention {
        ReplyRetention {
            guard: Some(Box::new(guard)),
            #[cfg(any(test, debug_assertions))]
            phase: ReplyRetentionPhase::Queued,
            _descriptors: self.descriptors,
            in_flight: Some(self.in_flight),
        }
    }

    /// Close `path_capability`, then give back its descriptor chain; the
    /// in-flight slot stays with the in-memory body until it is flushed.
    pub(crate) fn release_path_and_retain_in_memory<T>(
        self,
        path_capability: T,
    ) -> WriteOnlyRetention {
        drop(path_capability);
        drop(self.descriptors);
        WriteOnlyRetention {
            _in_flight: self.in_flight,
        }
    }
}

#[cfg(test)]
impl ReplyRetentionPermit {
    /// An in-memory permit from a fresh budget at the shipping caps: real
    /// admission that no sibling test competes for.
    pub(crate) fn from_own_budget() -> Self {
        ArtifactBudgets::shipping()
            .try_reserve_in_memory()
            .expect("a fresh budget has room")
    }
}

fn artifact_descriptor_units(path: &std::path::Path) -> usize {
    path.components()
        .count()
        .saturating_add(ARTIFACT_HANDOFF_SUFFIX_UNITS)
}

impl Drop for ReplyRetention {
    fn drop(&mut self) {
        #[cfg(any(test, debug_assertions))]
        {
            if self.phase == ReplyRetentionPhase::Queued {
                self.abort_queued_anchor();
            }
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

#[cfg(test)]
impl ReplyHandoff {
    pub(crate) fn prepare_write(&mut self) -> Result<(), String> {
        match self {
            Self::Acknowledged(retention) => retention.prepare_write(),
            Self::WriteOnly(_) => Ok(()),
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
pub(crate) struct ReplyWire(Arc<(Mutex<WireLatch>, std::sync::Condvar)>);

/// What a [`ReplyWire`]'s lock holds.
#[derive(Debug, Default)]
struct WireLatch {
    /// The reply is written (or never will be).
    released: bool,
    /// TESTS ONLY: a [`ReplyWire::wait_released`] began on it
    /// ([`ReplyWritten::awaited_within`]).
    #[cfg(test)]
    awaited: bool,
}

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
        self.0
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .released
    }

    /// Wait until the latch is released or `deadline` passes; whether it was
    /// released.
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) fn wait_released(&self, deadline: std::time::Instant) -> bool {
        let (latch, changed) = &*self.0;
        let mut latch = latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(test)]
        {
            latch.awaited = true;
            changed.notify_all();
        }
        while !latch.released {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            latch = changed
                .wait_timeout(latch, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        true
    }
}

#[cfg(all(test, unix))]
impl ReplyWritten {
    /// TESTS ONLY: wait up to `within` for a [`ReplyWire::wait_released`] to
    /// begin on this latch; whether one did. A stand-in connection writes its
    /// reply only then, so a waiter that went on without waiting finds it
    /// unwritten, every run.
    #[must_use]
    pub(crate) fn awaited_within(&self, within: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        let (latch, changed) = &*(self.0).0;
        let mut latch = latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !latch.awaited {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            latch = changed
                .wait_timeout(latch, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        true
    }
}

impl Drop for ReplyWritten {
    fn drop(&mut self) {
        let (latch, changed) = &*(self.0).0;
        latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .released = true;
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
            "image-meta-version=3 frame-kind={} frame-phase={} window={} view={} generation={} config-revision={} update-revision={} document-seq={} presentation-revision={} paint-revision={} capture-serial={} dimensions={}x{} pixel-fingerprint={:016x} compiled-fingerprint={} raster-fingerprint={:016x} raster-model-fingerprint={:016x} raster-geometry={:016x} overlay-fingerprint={:016x} theme-fingerprint={:016x} leaf-count={} leaves={}",
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
    #[cfg(test)]
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

    #[cfg(test)]
    pub(crate) fn outstanding(&self) -> usize {
        self.outstanding.load(Ordering::Acquire)
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
/// serves a request: idle connections park, waits move to wait lanes and push
/// streams to subscription lanes (`control_lanes.rs`). A fresh peer gets a busy
/// reply when every lane is working, or when [`control_connections_cap`]
/// connections are open and no parked one gives way to it (`control_lanes.rs`,
/// Admission).
const CONTROL_WORKERS: usize = 8;
/// The WAIT lanes' cap (`await`, `ready`, `wait`, `turn`, a parking `post` or
/// `inbox get @…`, a relay). Started on demand, kept once started. Every
/// supervised agent tab spends most of its life in one 20 s `await`.
const CONTROL_WAIT_WORKERS: usize = 64;
/// The REQUEST lanes that may serve a wait at once: a wait no wait lane can
/// take (every one taken; a wait with requests pipelined behind it moves with
/// them) is served on its request lane only within this budget, and past it
/// gets the busy reply on its connection. Before this bound, waits past the
/// wait lanes took request lanes without limit, and eight of them left a parked
/// driver's next request queued behind them for up to the waits' 600 s clamp
/// (`control_lanes.rs`).
///
/// The other long holders of a request lane are artifact captures: an `image`,
/// `window` or `video` holds its lane while it captures (up to minutes), and
/// at most `ARTIFACT_HANDOFF_LIMIT` are in flight at once
/// ([`ArtifactBudgets::try_reserve_in_memory`], [`ArtifactBudgets::try_reserve_for_path`]).
/// What both can hold together leaves one request lane always free for requests.
const CONTROL_INLINE_WAITS: usize = CONTROL_WORKERS - ARTIFACT_HANDOFF_LIMIT - 1;
const _: () = assert!(
    ARTIFACT_HANDOFF_LIMIT + CONTROL_INLINE_WAITS < CONTROL_WORKERS && CONTROL_INLINE_WAITS > 0,
    "captures and inline waits together must always leave a request lane for requests"
);
/// The CEILING on control connections open at once. The bound in force is
/// [`control_connections_cap`], which also answers to the process's descriptor
/// limit and is usually lower.
const CONTROL_CONNECTIONS_MAX: usize = 1024;

/// Most control connections open at once in this process (push subscriptions
/// have a cap of their own, [`control_subscriptions_cap`]): the parker's
/// capacity.
///
/// Every open connection holds a descriptor, and the window's descriptor budget
/// is small: an app started by launchd (the Dock, Finder, `open`) runs with a
/// SOFT `RLIMIT_NOFILE` of 256, and aterm does not raise it (except for an
/// update: a successor launched for one raises it for its descriptor grant
/// before it dials, `handoff_rendezvous::claim_incoming`, and an attempt that
/// takes the seamless lane toward a build no older than itself raises it for
/// its park, `descriptor_limit::raise_descriptor_limit_for_park`). A fixed
/// cap of 1024 ignored that (measured 2026-09-26 under `ulimit -n 256`): about 240 idle
/// connections used up the process, every fresh client then got a dropped
/// connection instead of the busy line, and a new tab or a file open had no
/// descriptor left. So the cap is a QUARTER of the soft limit read at startup —
/// 64 at 256 — leaving three quarters for tabs, files and everything else (half,
/// were every connection a relay holding a second descriptor to its far end);
/// never more than [`CONTROL_CONNECTIONS_MAX`], and never fewer than
/// [`CONTROL_WORKERS`], the connections the fixed lanes always held.
pub(crate) fn control_connections_cap() -> usize {
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
pub(crate) fn soft_descriptor_limit() -> Option<u64> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource id and an exclusively borrowed, writable
    // out-parameter for the one call.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) };
    (read == 0).then_some(limit.rlim_cur)
}

/// No parker runs off unix, so the cap bounds nothing there (the lanes do).
#[cfg(not(unix))]
pub(crate) fn soft_descriptor_limit() -> Option<u64> {
    None
}

/// The FLOOR on push subscriptions served at once: the fixed subscription pool
/// this cap replaced held four.
const CONTROL_SUBSCRIPTIONS_MIN: usize = 4;
/// The CEILING on push subscriptions served at once.
const CONTROL_SUBSCRIPTIONS_MAX: usize = 256;

/// Most push subscriptions served at once in this process: the subscription
/// lanes' cap (`control_lanes.rs`).
///
/// A subscription holds its stream's descriptor and one thread parked on its
/// sessions' wakes for its whole life; on unix nothing more, since the peer
/// watch polls that same descriptor ([`crate::peer_watch`]). Idle, it costs a
/// parked thread, so what bounds it is the descriptor budget: a SIXTEENTH of
/// the soft `RLIMIT_NOFILE` — 16 at launchd's 256 — on top of the open
/// connections' quarter ([`control_connections_cap`]); never fewer than
/// [`CONTROL_SUBSCRIPTIONS_MIN`], never more than [`CONTROL_SUBSCRIPTIONS_MAX`].
/// A subscription past it is answered `ERR subscription capacity busy; retry`.
fn control_subscriptions_cap() -> usize {
    subscriptions_cap_for(soft_descriptor_limit())
}

/// [`control_subscriptions_cap`] for a soft descriptor limit (`None`:
/// unreadable, and off unix, which keeps the floor).
fn subscriptions_cap_for(soft_limit: Option<u64>) -> usize {
    soft_limit.map_or(CONTROL_SUBSCRIPTIONS_MIN, |soft| {
        usize::try_from(soft / 16)
            .unwrap_or(usize::MAX)
            .clamp(CONTROL_SUBSCRIPTIONS_MIN, CONTROL_SUBSCRIPTIONS_MAX)
    })
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
    /// The registry whose sessions the halt holds; its `hold` marks ring each
    /// session's watchers ([`wake_sid_watchers`]).
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
        crate::fabric::bridge_lost_waking(&self.store, self.generation, &|sid| {
            wake_sid_watchers(&self.store, sid);
        });
    }
}

struct ControlWorkerContext {
    active: ActiveHandle,
    store: Store,
    proxy: EventLoopProxy<Wake>,
    token: Arc<String>,
    sock_dir: std::path::PathBuf,
    recovery_count_admission: Arc<crate::keeper_link::RecoveryCountAdmission>,
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
    fn owed_input_reply(&self, request: &Request) -> Option<FinalReply> {
        owed_input_reply(&self.store, request)
    }

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
        request: Request,
        scope: Scope,
        stream: &CtlStream,
        reader: &mut control_lanes::LaneReader<'_>,
        out: &mut control_lanes::Outbox,
    ) -> Option<ServeDisposition> {
        serve_request_line(
            request,
            scope,
            stream,
            reader,
            out,
            &self.active,
            &self.store,
            &self.proxy,
            &self.sock_dir,
            &self.recovery_count_admission,
        )
    }

    fn frame_header(
        &self,
        request: &Request,
        scope: Scope,
        out: &mut control_lanes::Outbox,
    ) -> bool {
        if let Some(refusal) = recovery_count_refusal(&self.recovery_count_admission) {
            out.extend_from_slice(refusal.as_bytes());
            return false;
        }
        // The header of `@ feed-bin <n>` or `@ spawn … len=<n>` is refused as
        // its whole line would be ([`empty_selector_refusal`]): no `GO`, no
        // input boundary, and the connection closes.
        if let Some(refusal) = empty_selector_refusal(request) {
            out.extend_from_slice(refusal.as_bytes());
            return false;
        }
        let head = request.head();
        if request.frame().is_some() {
            let route = FeedBinRoute {
                active: &self.active,
                store: &self.store,
                scope,
            };
            return feed_bin_header(head.selector.as_ref(), route, out);
        }
        // A worker's spawn: its task is sent only once this answer says the
        // server reads it (`task_frame_go`) — unless it is aimed where it would
        // be relayed, which is refused unsent.
        match spawn_frame_len(&head) {
            Some(Ok(n)) => match worker_frame_refusal(&head, &self.store) {
                Some(refusal) => {
                    out.extend_from_slice(refusal.as_bytes());
                    false
                }
                None => {
                    out.extend_from_slice(aterm_types::control_verbs::task_frame_go(n).as_bytes());
                    true
                }
            },
            // A `post`'s body has no header to act on.
            _ => true,
        }
    }

    fn is_wait(&self, request: &Request, scope: Scope) -> bool {
        self.recovery_count_admission.ready() && is_wait_request(request, scope, &self.store)
    }

    fn subscribe(&self, request: Request, scope: Scope, mut stream: CtlStream) {
        // This direct connection moved from a request lane to this push lane:
        // its peer context is set again here. The inherited bridge's inline
        // subscription ([`Self::serve_bridge`]) never gains DirectPeer authority.
        let _serving = ServingFd::enter(&stream);
        #[cfg(unix)]
        let _direct = DirectPeer::enter();
        // Hangs the connection up as the push stream ends, as an ordinary lane
        // does, waking a vanished peer out of the AF_UNIX kernel path.
        run_subscribe_socket(
            request.head(),
            &self.active,
            &self.store,
            scope,
            &mut stream,
        );
    }

    fn release(&self, id: u64) {
        release_connection_claims(&self.store, Some(&self.proxy), id);
    }

    #[cfg(unix)]
    fn has_live_claim(&self, id: u64) -> bool {
        connection_has_live_claim(&self.store, id, crate::metrics::now_us())
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
            &self.proxy,
            &self.sock_dir,
            &self.recovery_count_admission,
        ) {
            // Each half on its own ([`aterm_uds::hangup::hang_up`]): the lane
            // closes on the bridge's EOF, and Darwin refuses a both-ways
            // `shutdown` of a socket its peer half-closed, shutting nothing.
            ServeDisposition::Close => {
                aterm_uds::hangup::hang_up(&stream);
            }
            // The bridge's push subscription is served inline, not handed to a
            // subscription lane: the `BridgeLostGuard` fires when this function
            // returns, so handing off would apply the fabric-lost halt at the
            // flip. The bridge also uses none of the shared lanes.
            ServeDisposition::Subscribe { request, scope } => {
                // Hangs the lane up when its push stream ends.
                run_subscribe_socket(
                    request.head(),
                    &self.active,
                    &self.store,
                    scope,
                    &mut stream,
                );
            }
        }
    }

    /// Serve one fleet connection (scope from the listener's proof, not a token)
    /// until it closes. The gate closes it at the first refused line, which it
    /// audits under `peer`'s address; a push flip never happens, `subscribe`
    /// being one. With a `driver`, the connection a node's drive op opened:
    /// served as that node ([`crate::fabric::DrivingAs`]) through the drive
    /// gate ([`drive_admission`]).
    #[cfg(unix)]
    fn serve_fleet(
        &self,
        stream: CtlStream,
        (content, drive): (bool, bool),
        peer: Option<std::net::SocketAddr>,
        driver: Option<crate::fabric::Driver>,
    ) {
        SERVING_FLEET_PEER.with(|serving| serving.set(peer));
        let drive = drive || driver.is_some();
        let content = content || driver.is_some();
        let _driving = driver.map(crate::fabric::DrivingAs::enter);
        let _ = serve_preresolved(
            &stream,
            Scope::Fleet { content, drive },
            &self.active,
            &self.store,
            &self.proxy,
            &self.sock_dir,
            &self.recovery_count_admission,
        );
        SERVING_FLEET_PEER.with(|serving| serving.set(None));
        aterm_uds::hangup::hang_up(&stream);
    }
}

/// The control-worker context, published once the server runs, so the fabric
/// bridge and every fleet connection are served against the same registry,
/// active handle and proxy as every other lane.
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

/// Serve one fleet connection the fleet listener admitted
/// ([`crate::fleet_listen`]): the near end of a `socketpair` whose far end that
/// listener relays to the peer's TLS stream. [`Scope::Fleet`] is pre-resolved,
/// as the bridge's scope is: no `AUTH` line and no token, the fleet proof having
/// been made. On a thread of its own, using none of the lanes; the listener's
/// slots bound how many there are, since a relayed connection keeps its slot
/// until this thread has answered the line it is serving or hung up, however
/// the peer ends it (`aterm_net::tls::relay_answers`). `peer`, the dialer's
/// address, names it in
/// the audit trail of what the fleet gate refused.
///
/// `drive` says the peer is a node this Mac lets drive it, which its survey
/// says (`drive=1`).
///
/// Returns `false` when the control server is not up yet or the thread could
/// not start; the caller then drops the connection.
#[cfg(unix)]
pub(crate) fn attach_fleet_connection(
    stream: CtlStream,
    (content, drive): (bool, bool),
    peer: Option<std::net::SocketAddr>,
) -> bool {
    let Some(context) = BRIDGE_CONTEXT.get().cloned() else {
        return false;
    };
    crate::qos::spawn_lane_thread(
        "aterm-fleet-serve".to_string(),
        // The lanes' floor: it serves their verbs under the same locks.
        crate::qos::Role::Responsive,
        move || context.serve_fleet(stream, (content, drive), peer, None),
    )
    .is_ok()
}

/// Serve one DRIVE connection the fleet listener admitted (the step-in,
/// `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1): as
/// [`attach_fleet_connection`], from the node `driver` names — proved by its
/// node key and on this Mac's `[fleet] drive_from` — whose lines the drive
/// gate admits ([`drive_admission`]) and credits to it.
///
/// Returns `false` when the control server is not up yet or the thread could
/// not start; the caller then drops the connection.
#[cfg(unix)]
pub(crate) fn attach_fleet_drive(
    stream: CtlStream,
    peer: Option<std::net::SocketAddr>,
    driver: crate::fabric::Driver,
) -> bool {
    let Some(context) = BRIDGE_CONTEXT.get().cloned() else {
        return false;
    };
    crate::qos::spawn_lane_thread(
        "aterm-fleet-drive".to_string(),
        crate::qos::Role::Responsive,
        move || context.serve_fleet(stream, (true, true), peer, Some(driver)),
    )
    .is_ok()
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
                |deadline| self.parent.exited_by(deadline),
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
        #[cfg(all(test, unix))]
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
#[cfg(all(test, unix))]
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
///
/// `parent_exited(deadline)` waits for the attested predecessor's exit: `true`
/// once it has exited, `false` at `deadline` with it still running
/// ([`crate::seamless::AttestedParent::exited_by`]). It and `parent_owns_peer`
/// are asked only with a `handoff_gate`.
fn bind_control_listener(
    plan: &control_auth::SocketPlan,
    handoff_gate: Option<&crate::spawn::DeferredReaderGate>,
    parent_exited: impl FnMut(std::time::Instant) -> bool,
    parent_owns_peer: impl Fn(u32) -> bool,
    identity: Option<&crate::control_socket_identity::SocketIdentity>,
) -> Option<BoundControl> {
    bind_control_listener_within(
        plan,
        handoff_gate,
        parent_exited,
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
    mut parent_exited: impl FnMut(std::time::Instant) -> bool,
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
        // Wait for the attested process's exit, not on repeated socket
        // connects: a live listener with a full backlog can block connect
        // beyond any deadline. The ordinary strict socket probe below still
        // refuses unrelated owners.
        let deadline = std::time::Instant::now() + handoff_wait;
        if !parent_exited(deadline) {
            aterm_log::warn!(
                "control socket {sock_path}: predecessor still alive after handoff Commit; \
                 preserving its socket and token"
            );
            return None;
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
    if control_auth::ensure_control_dir(&sock_dir).is_err() {
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
    let policy = if handoff_gate.is_some() {
        BindRetry::HANDOFF
    } else {
        BindRetry::STARTUP
    };
    let listener = bind_with_retry(
        plan,
        policy,
        |path| CtlListener::bind(path),
        std::thread::sleep,
    )?;
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

/// What a failed control-socket bind may do to its path before binding again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnlinkRule {
    /// Nothing. A consumed handoff witness never authorizes another unlink, so
    /// a failed bind is tried again only while the path is demonstrably absent.
    Never,
    /// Remove the stale file after the pause. A shared explicit path (no
    /// `latest` link) is probed first, since a sibling may have bound it while
    /// this one slept; a per-instance path cannot be anyone else's.
    Stale,
}

/// How a control-socket bind recovers from a transient failure (an AV scan, a
/// racing cleanup) rather than losing introspection for the process lifetime.
#[derive(Clone, Copy, Debug)]
struct BindRetry {
    /// Binds in all, the first included.
    attempts: u32,
    /// The pause after failed bind `n` (from 1).
    backoff: fn(u32) -> std::time::Duration,
    unlink: UnlinkRule,
}

impl BindRetry {
    /// Startup's, after its own live-owner refusal and unlink.
    const STARTUP: Self = Self {
        attempts: 3,
        backoff: |_| std::time::Duration::from_millis(750),
        unlink: UnlinkRule::Stale,
    };

    /// A committed handoff's, after its one authorized token rotation and
    /// unlink. Never startup's stale-path policy, so an endpoint that arrives
    /// during backoff is preserved.
    const HANDOFF: Self = Self {
        attempts: 3,
        backoff: |attempt| std::time::Duration::from_millis(50 * u64::from(attempt)),
        unlink: UnlinkRule::Never,
    };

    /// May bind `failed_attempt`'s failure be followed by another, within the
    /// budget? Judged before the pause, so a refusal pays none. `path_missing`:
    /// nothing is at the path, not even a dangling symlink.
    fn retry_allowed(self, failed_attempt: u32, path_missing: bool) -> bool {
        failed_attempt < self.attempts
            && match self.unlink {
                UnlinkRule::Never => path_missing,
                UnlinkRule::Stale => true,
            }
    }
}

/// Bind `plan`'s path under `policy`, once the caller has provisioned the
/// token and removed the path. `bind` and `pause` are its two OS effects,
/// passed in so tests can fail a bind or place a foreign listener during
/// backoff deterministically.
fn bind_with_retry(
    plan: &control_auth::SocketPlan,
    policy: BindRetry,
    mut bind: impl FnMut(&str) -> std::io::Result<CtlListener>,
    mut pause: impl FnMut(std::time::Duration),
) -> Option<CtlListener> {
    let sock_path = plan.sock_path.as_str();
    let attempts = policy.attempts;
    for attempt in 1..=attempts {
        let error = match bind(sock_path) {
            Ok(listener) => {
                if attempt > 1 {
                    aterm_log::info!(
                        "control socket bind succeeded on retry {attempt} at {sock_path}"
                    );
                }
                return Some(listener);
            }
            Err(error) => error,
        };
        aterm_log::warn!(
            "control socket bind failed at {sock_path} (attempt {attempt}/{attempts}): {error}"
        );
        crate::logging::stderr_line!(
            "aterm-gui: control socket bind failed at {sock_path} \
             (attempt {attempt}/{attempts}): {error}"
        );
        // symlink_metadata treats a dangling symlink as occupied too. No
        // connect probe: a full foreign backlog can look stale.
        let missing = std::fs::symlink_metadata(sock_path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound);
        if !policy.retry_allowed(attempt, missing) {
            let why = if attempt < attempts {
                "its path is occupied; preserving the endpoint there"
            } else {
                "out of bind attempts"
            };
            aterm_log::warn!("control socket {sock_path} disabled: {why}");
            crate::logging::stderr_line!("aterm-gui: control socket {sock_path} disabled: {why}");
            return None;
        }
        pause((policy.backoff)(attempt));
        if policy.unlink == UnlinkRule::Stale {
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
    None
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
    reason = "the control server's full set of independent collaborators (active handle, store, proxy, socket plan); a config struct would only move the list"
)]
pub(crate) fn spawn(
    active: ActiveHandle,
    store: Store,
    proxy: EventLoopProxy<Wake>,
    plan: control_auth::SocketPlan,
    // The startup config generation already admitted; no second config read
    // before deciding on the optional fabric bridge and fleet listener.
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
    recovery_count_admission: Arc<crate::keeper_link::RecoveryCountAdmission>,
    // Only fixed-path incoming candidates wait: per-process sockets cannot collide.
    handoff: Option<IncomingControlHandoff>,
    // A per-process update candidate's Commit gate: bind at once, publish and
    // serve only after release. `None` for every other launch.
    publish_gate: Option<crate::spawn::DeferredReaderGate>,
) -> ControlPreparation {
    // Windows has no exec(2), so nothing retires its endpoint.
    #[cfg(not(unix))]
    let _ = endpoint;
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
                let Some(binding) = bind_control_listener(&plan, None, |_| true, |_| false, None)
                else {
                    return;
                };
                Some(binding)
            } else {
                None
            };
            let connections_cap = control_connections_cap();
            let subscriptions_cap = control_subscriptions_cap();
            let lanes = control_lanes::Lanes::new(control_lanes::LaneLimits {
                rpc: CONTROL_WORKERS,
                wait: CONTROL_WAIT_WORKERS,
                inline: CONTROL_INLINE_WAITS,
                open: connections_cap,
                subscriptions: subscriptions_cap,
            });
            let started = lanes.start();
            let workers = started.rpc;
            if !control_lanes_prepared(workers, started.subscriptions) {
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
                proxy: proxy.clone(),
                token: token.clone(),
                sock_dir: sock_dir.clone(),
                recovery_count_admission,
            });
            // No stream enters any queue before this context is available.
            lanes.publish(worker_context.clone());
            // A bridge attached later must never find a half-built process.
            let _ = BRIDGE_CONTEXT.set(worker_context.clone());
            if workers != CONTROL_WORKERS {
                aterm_log::warn!(
                    "control socket started with {workers}/{CONTROL_WORKERS} connection workers"
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
                "control socket listening at {sock_path} ({workers} RPC lanes, up to {CONTROL_WAIT_WORKERS} wait lanes and {CONTROL_INLINE_WAITS} waits on RPC lanes, up to {connections_cap} connections, up to {subscriptions_cap} subscription lanes; excess peers, waits and subscriptions get retry)"
            );
            #[cfg(not(windows))]
            crate::logging::stderr_line!(
                "aterm-gui: control socket listening at {sock_path} (token-gated, same-uid only)"
            );
            // `listening at <PATH> (token-gated` shape, with
            // the HONEST posture parenthetical (never silently claim same-uid): a peer
            // running as another user is refused (SIO_AF_UNIX_GETPEERPID); where the
            // ioctl or the peer's token is unavailable, the control directory's
            // owner-only DACL + the per-launch token are the gates. An elevated
            // instance's directory and socket admit Administrators and SYSTEM
            // only (the elevation law, `control_auth_win.rs`).
            #[cfg(windows)]
            crate::logging::stderr_line!(
                "aterm-gui: control socket listening at {sock_path} (token-gated, dir-ACL + peer pid{})",
                if aterm_uds::elevation::is_elevated() {
                    "; elevated: Administrators only"
                } else {
                    ""
                }
            );
            // Our entries are published, so take each claim transferred across a
            // handoff (the predecessor has exited) and retire the successor markers
            // it wrote at Commit (`identity_claim`). No-op without a handoff.
            crate::identity_claim::settle_transferred();
            // Every `aterm ctl` begins with `version`, whose reply carries the
            // executable's content signature. Where that is a hash of tens of MB (off
            // macOS, and an unsigned x86_64 macOS dev build), compute it once here, off
            // every lane; a racing probe waits on the same `OnceLock`. A signed macOS
            // build's is the kernel's code directory hash, one syscall, so no thread.
            if crate::build_info::binary_signature_reads_the_file() {
                let _ = crate::qos::spawn_lane_thread(
                    "aterm-build-signature",
                    crate::qos::Role::Responsive,
                    || {
                        // A lane may be waiting on this result: never below the lanes.
                        let _ = crate::build_info::binary_signature();
                    },
                );
            }
            // The fabric bridge, off unless `[fabric] command` (or a development seam)
            // is set. Launched after the bind, the lanes and `BRIDGE_CONTEXT`, so its
            // inherited connection is served by a complete process; it uses none of
            // the lanes.
            #[cfg(unix)]
            crate::fabric_launch::spawn_supervisor(&startup_config);
            // The fleet listener, off unless `[fleet] listen` is set: a TLS port whose
            // proved connections this server serves as `Scope::Fleet`. Started after
            // `BRIDGE_CONTEXT`, which serves them, and followed by every config reload.
            crate::fleet_listen::start_fleet_listener(startup_config.fleet.as_ref());
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
                // socket saturated with work, or at `control_connections_cap()` with no
                // parked connection to give way, gets the busy reply (here, or from the
                // parker); the listener never blocks.
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

/// Resolve a stable-id selector to the socket of the instance that hosts it: a
/// session another same-uid aterm hosts (one this process spawned, one that
/// one spawned, or an instance opened separately), found through the graph
/// entry that instance published.
///
/// Fail-closed guards, in order:
/// * a locally hosted sid is not a hop ([`proxy_forward_plan`] checks first);
/// * no graph entry means unknown sid;
/// * the socket path must confine to our runtime dir
///   ([`crate::control_auth::confine_proxy_sock`]): the entry is same-uid
///   writable, and the forward presents the token beside the socket it dials;
/// * self-dial guard: an entry naming our own socket is stale and refused.
///
/// Whether anything listens there, and the token to present, are the forward's
/// own connect's to find ([`crate::proxy::connect_hop`]): no probe connection
/// is made here, and the token is read once the connection exists, since an
/// update successor may take the socket in between.
fn resolve_sibling(sid: Sid<'_>, sock_dir: &std::path::Path) -> Option<String> {
    let (sock_path, _nonce) = crate::proxy::read_graph_entry(sock_dir, sid)?;
    let sock_path = crate::control_auth::confine_proxy_sock(sock_dir, &sock_path)?;
    if crate::proxy::self_sock_path().as_deref() == Some(sock_path.as_str()) {
        return None; // stale entry for a session we no longer host — never self-dial
    }
    Some(sock_path)
}

/// The session `request` from `scope` would be relayed to, if it names one on
/// another instance: an Owner-class connection's request whose verb acts on a
/// session or an instance (its row's target is [`Target::Session`] or
/// [`Target::App`]) and names one stable id, an explicit `@<sid>` or the one
/// session a selectorless `subscribe` names second
/// ([`control_request::subscribe_target`]). Pure: whether the store hosts it
/// is the caller's question.
///
/// An Edge never forwards (it falls through to local resolution, which denies
/// the cross-process sid), and a [`Target::Meta`] verb answers here whatever
/// it names, as the far instance would. [`proxy_forward_plan`] and
/// [`is_wait_request`] both ask this, so whether a line is relayed and which
/// lane it takes cannot disagree.
///
/// [`Target::Session`]: aterm_types::control_verbs::Target::Session
/// [`Target::App`]: aterm_types::control_verbs::Target::App
/// [`Target::Meta`]: aterm_types::control_verbs::Target::Meta
fn relay_candidate(request: &Request, scope: Scope) -> Option<Sid<'_>> {
    use aterm_types::control_verbs::Target;
    if !scope.is_owner_class() {
        return None;
    }
    match request.form()?.row.target {
        Target::Session | Target::App => {}
        Target::Meta => return None,
    }
    let head = request.head();
    match head.selector {
        Some(Selector::Sid(sid)) => Some(sid),
        None if head.verb == "subscribe" => {
            control_request::subscribe_target(head.rest).map(|(sid, _)| sid)
        }
        _ => None,
    }
}

/// The socket of the instance `request` from `scope` must be forwarded to, or
/// `None` to handle it locally: a [`relay_candidate`] this instance does not
/// host whose graph entry [`resolve_sibling`] accepts. The line goes as it
/// came, selector kept, presented with the host's own per-launch token (Owner
/// there), so the host resolves the session among its own tabs; the credential
/// is one any same-uid client could read.
///
/// Pure (no connection), so the security decisions are unit-testable without a
/// relay.
fn proxy_forward_plan(
    request: &Request,
    scope: Scope,
    store: &Store,
    sock_dir: &std::path::Path,
) -> Option<String> {
    let sid = relay_candidate(request, scope)?;
    // A sid this instance hosts is a normal in-process target, never a hop.
    let hosted = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_selector(&Selector::Sid(sid))
        .is_some();
    if hosted {
        return None;
    }
    resolve_sibling(sid, sock_dir)
}

/// If `request` targets a session this process does not host, forward the whole
/// connection ([`proxy_forward_plan`]) and relay bytes, returning `true` (the
/// caller then ends the connection); `false` for a local request.
///
/// The forward's connect is its liveness test, made before the connection is
/// touched: a stale entry ([`crate::proxy::ForwardError::NotAHop`]: nothing
/// listening, no token beside the socket) returns `false`, leaving the read
/// timeout and the bytes `reader` already holds to the local dispatch, which
/// answers `ERR no such session`.
fn try_proxy_forward(
    request: &Request,
    scope: Scope,
    store: &Store,
    sock_dir: &std::path::Path,
    client: &CtlStream,
    reader: &mut control_lanes::LaneReader<'_>,
) -> bool {
    let Some(sock_path) = proxy_forward_plan(request, scope, store, sock_dir) else {
        return false;
    };
    let reply = |line: &str| {
        let _ = (&*client).write_all(line.as_bytes());
        let _ = (&*client).flush();
    };
    // A failure before any byte reaches the host leaves the client stream
    // untouched, so honor the contract and answer ERR rather than a silent EOF.
    // A connect timeout is reported as such; other failures say the host could
    // not be reached.
    let forward_error = |error: &std::io::Error| {
        if error.kind() == std::io::ErrorKind::TimedOut {
            reply(&format!("ERR forward: {error}\n"));
        } else {
            reply("ERR forward: could not reach the aterm that hosts that session\n");
        }
    };
    let hop = match crate::proxy::connect_hop(&sock_path) {
        Ok(hop) => hop,
        Err(crate::proxy::ForwardError::NotAHop) => return false,
        Err(crate::proxy::ForwardError::Unreachable(error)) => {
            forward_error(&error);
            return true;
        }
    };
    // Relay takeover is intentionally long-lived. Keep the inherited socket
    // explicitly deadline-free even if handshake policy changes independently.
    if client.set_read_timeout(None).is_err() {
        reply("ERR forward timeout setup\n");
        return true;
    }
    // Forward anything already read past the request line, then relay.
    let pre = reader.take_buffered();
    if let Err(error) = crate::proxy::relay_hop(hop, request.line(), client, &pre) {
        forward_error(&error);
    }
    true
}

/// One request line, read once where its lane first holds it whole: the
/// request grammar's spans ([`control_request::Spans`]), its verb's table form
/// ([`aterm_types::control_verbs::spec_for`]) and the body it announces. The
/// lane's wait test, a frame's header, the relay, serve and dispatch all read
/// this one parse, so no two stages split or classify a line two ways.
struct Request {
    line: String,
    spans: control_request::Spans,
    form: Option<Resolved>,
    body: usize,
}

impl Request {
    /// Read a request line, decoded ([`decode_request_line`]).
    fn parse(line: String) -> Self {
        let spans = control_request::Spans::of(&line);
        let head = spans.head(&line);
        let form = aterm_types::control_verbs::spec_for(head.verb, head.rest);
        let body = announced_body(&head, form);
        Self {
            line,
            spans,
            form,
            body,
        }
    }

    /// The line as the client sent it.
    fn line(&self) -> &str {
        &self.line
    }

    /// Its selector, verb and rest.
    fn head(&self) -> Head<'_> {
        self.spans.head(&self.line)
    }

    /// Its verb's table form; `None` for an unknown verb.
    fn form(&self) -> Option<Resolved> {
        self.form
    }

    /// The bytes of body that follow its line ([`announced_body`]).
    fn body_len(&self) -> usize {
        self.body
    }

    /// A counted frame's header (`feed-bin`, `paste-bin`: its row's
    /// [`Body::Counted`]); `None` for any other request.
    fn frame(&self) -> Option<FeedBinFrame<'_>> {
        match self.form?.row.body {
            Body::Counted { keyed } => Some(parse_frame_header(self.head().rest, keyed)),
            Body::None | Body::PostLen => None,
        }
    }

    /// A length-prefixed `post`'s announced body ([`post_frame_len`]); `None` for
    /// an inline `post` and any other request.
    fn post_frame(&self) -> Option<Result<usize, String>> {
        match self.form?.row.body {
            Body::PostLen => post_frame_len(self.head().rest),
            Body::None | Body::Counted { .. } => None,
        }
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

/// Build and meta verbs (`Access::AnyScopeMeta`: `version`, `update`, `help`,
/// `verbs`, `privacy`), answered for any authenticated scope with no session.
fn dispatch_meta_verb(
    form: &Resolved,
    head: &Head<'_>,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
) -> String {
    let Head {
        selector,
        verb,
        rest,
    } = *head;
    // `privacy` — the macOS consent posture. Instance-wide and self-scoped:
    // unlike the other meta verbs (whose answer is the same whoever is
    // named, so a selector is merely ignored), this one aggregates PER
    // SESSION, so a selector would read as "the posture of that session"
    // and be silently wrong. It is rejected, as the verb's help states.
    // The scope goes with it: an edge's report lists only the sessions it
    // may read, never the roster `sessions` withholds from it.
    let privacy = |json: bool, rest: &str| match selector {
        None | Some(Selector::SelfTok) => {
            crate::control_privacy::cmd_privacy(json, rest, scope, proxy)
        }
        Some(_) => crate::control_privacy::no_selector_error().to_string(),
    };
    match json_ask(form, verb, rest) {
        JsonAsk::No => {}
        JsonAsk::Refuse(refusal) => return refusal,
        JsonAsk::Emit(body) => {
            return match verb {
                "privacy" => privacy(true, &body),
                // A JSON-emitting row with no emitter here: one `ERR` line.
                _ => no_handler(verb),
            };
        }
    }
    match verb {
        "version" => refuse_stray_args(verb, rest).unwrap_or_else(crate::build_info::control_line),
        "update" => cmd_update(rest, scope, proxy),
        "help" | "verbs" => cmd_help(rest),
        "privacy" => privacy(false, rest),
        _ => no_handler(verb),
    }
}

/// The App-target verbs' arms, past [`dispatch_app`]'s route and gates. `aim` is
/// the window an aimed `spawn`, `tab` or `panes` acts on, the session an aimed
/// `theme session` or `theme palette session` colours, and the session whose tab
/// an `edit` opens its view beside; every other arm acts on the front window.
/// `active_term` (the front terminal, if any) only fills the `metrics` rows/cols.
///
/// Only owner-class scopes reach this (`native_control_decision` denies edges
/// every App verb), so exit-ledger attribution for `tab close` / `invoke
/// CloseTab` is by kind, `ctl` or `bridge`
/// ([`control_session::caller_actor_of`] with `None`).
fn dispatch_app_verb(
    verb: &str,
    rest: &str,
    form: &Resolved,
    aim: &AppAim,
    cx: &DispatchCx<'_>,
    active_term: Option<&Arc<Mutex<Terminal>>>,
    typist: Option<Arc<crate::typed_by::Caller>>,
) -> ControlReply {
    let DispatchCx {
        scope,
        proxy,
        sock_dir,
        ..
    } = *cx;
    match json_ask(form, verb, rest) {
        JsonAsk::No => {}
        JsonAsk::Refuse(refusal) => return refusal.into(),
        JsonAsk::Emit(body) => {
            return match verb {
                "metrics" => control_query::cmd_metrics_json(active_term, &body),
                // A JSON-emitting row with no emitter here: one `ERR` line.
                _ => no_handler(verb),
            }
            .into();
        }
    }
    let response = match verb {
        // `window` and `video` capture the front window: their artifact replies
        // carry their own retention.
        "window" => return control_media::cmd_window(proxy, rest, sock_dir),
        "video" => return control_media::cmd_video(proxy, rest, sock_dir, scope.is_owner_class()),
        "chrome" => control_media::cmd_chrome(proxy),
        // `appearance`: the OS light/dark reading and what it drew; a dev
        // build can inject a flip.
        "appearance" => crate::os_appearance::cmd_appearance(proxy, rest),
        // `text-scale`: the text scale native pages lay out at; a dev build
        // can inject one (macOS never reports anything but 1.0).
        "text-scale" => crate::native_appearance::cmd_text_scale(proxy, rest),
        // `pressure`: the memory-pressure level and the crash-restore journal's shed; a
        // dev build on macOS can inject a level.
        "pressure" => crate::crash_restore::cmd_pressure(proxy, rest),
        // `program-check`: the stuck check's board; a dev build can hold its
        // readings at the kernel's limit.
        "program-check" => crate::program_check_watch::cmd(rest),
        // `login-notice`: the harness's login notices, listed; a development
        // build raises one for the front tab through the host's own path.
        "login-notice" => crate::login_notice_verb::cmd(proxy, rest),
        // `card`: the harness card's constellation pin; a debug build pins
        // it, so two builds' cards can be compared pixel for pixel.
        "card" => crate::card_verb::cmd(proxy, rest),
        // `aos`: the front window's momentum glow as an Aos task (attach, pause,
        // repair, resume, detach, status, trace); a debug build only.
        "aos" => crate::aos_verb::cmd(proxy, rest),
        // `security-posture`: where Settings ▸ Security's MACOS ACCESS block's
        // posture comes from; a development build's headless instance can stage
        // a named fixture.
        "security-posture" => crate::security_posture::cmd(proxy, rest),
        // `appstatus`: what aterm is doing on its own (a toolchain install, an
        // update download): live status-bar rows plus finished jobs the ring
        // remembers. Instance state, so it answers with no terminal.
        "appstatus" => control_query::cmd_appstatus(proxy),
        // `messages`: the message log, one row per record (the band and
        // Settings ▸ Messages as text). Instance state.
        "messages" => control_query::cmd_messages(proxy, rest),
        // `panes`: the split layout of the window whose active tab shows the
        // aimed session (`OK 0` with `panes=0` when no window shows it), else
        // the front window's; `@<sid> image` names the same window.
        "panes" => control_media::cmd_panes(proxy, aim.session()),
        "controls" => control_media::cmd_controls(proxy, rest),
        "inspect" => control_media::cmd_inspect(proxy, rest),
        "open" => control_media::cmd_open(proxy, rest),
        // `@<sid> edit <file-uri>`: a fresh Editor view beside the aimed
        // session's tab, answered once it is installed; unaimed, its usage.
        "edit" => crate::control_edit::cmd_edit(proxy, scope, aim.session(), rest),
        "doc" => crate::control_doc::cmd_doc(proxy, rest),
        "act" => control_media::cmd_act(proxy, rest),
        "invoke" => control_media::cmd_invoke(
            proxy,
            rest,
            control_session::caller_actor_of(scope, None),
            typist,
        ),
        "confirm" => control_media::cmd_confirm(proxy, rest),
        "rain" => control_media::cmd_rain(proxy, rest),
        // `theme [status|list|palette|preview|revert|commit|set]`: the live theme
        // and its transient preview lane. The session-scoped forms (`theme
        // session …`, `theme palette session`) act on the aimed session, else on
        // the front terminal.
        "theme" => control_media::cmd_theme(proxy, rest, aim.session()),
        "fx" => control_media::cmd_fx(proxy, rest),
        "streak" => control_media::cmd_streak(proxy, rest),
        "tone" => control_media::cmd_tone(proxy, rest),
        "trail" => control_media::cmd_trail(proxy, rest),
        "kitty" => control_media::cmd_kitty(proxy, rest),
        // `spawn`: mint one new tab session and reply `OK <sid>`, addressable at
        // once. Aimed, it lands in the window hosting the session on any tab
        // (`App::window_of_session`), wider than `@<sid> image`, which needs the
        // session on an active tab; where both answer they name the same window.
        // `window=` still aims, and wins.
        "spawn" => control_media::cmd_spawn(proxy, rest, aim.session()),
        "settings" => control_media::cmd_settings_overlay(proxy, rest),
        // `tab`: aimed, the tabs of the window hosting the session, so an agent in
        // a background window can walk its own tabs; flagless, the front window's.
        "tab" => {
            let by = control_session::caller_actor_of(scope, None);
            match aim.session() {
                Some(session) => control_media::cmd_tab_aimed(proxy, rest, session, by),
                None => control_input::cmd_tab(proxy, rest, by),
            }
        }
        // `hwkey`: a real NSEvent posted through the OS event queue to the front
        // window, the path a physical keypress takes, whatever it shows — a
        // terminal or a native view such as Settings; it needs no session.
        "hwkey" => control_input::cmd_hwkey(proxy, rest),
        "hover" => control_input::cmd_hover(proxy, rest),
        // Window-surface drives target the front window and need no PTY, so they
        // are answered here alone. `find` drives the find bar (`App::find_cmd`
        // resolves `frontmost_window`); `pane <dir>` moves keyboard focus between
        // the active tab's split panes; `pointer` puts the window's pointer through
        // `on_cursor_moved`, as winit's `CursorMoved` does (under DEC
        // 1000/1002/1003 the motion is reported to the program).
        "find" => control_input::cmd_find(proxy, rest),
        "pane" => control_input::cmd_pane(proxy, rest),
        "pointer" => control_input::cmd_pointer(proxy, rest),
        "metrics" => control_query::cmd_metrics(active_term, rest),
        _ => no_handler(verb),
    };
    response.into()
}

/// Tombstones for removed verbs: not advertised and without an authority
/// class, but an old client gets an actionable error — the embedded
/// operator's, which a manager session replaced, and the network drive's,
/// which the fleet replaced (design §7).
fn retired_verb_error(verb: &str) -> Option<String> {
    match verb {
        "operator" | "operator-propose-bin" => Some(format!(
            "ERR verb {verb} was removed with the embedded operator: a manager session \
             replaced it (`aterm ctl survey` reads every session, `aterm ctl @self spawn \
             agent=claude -` starts a worker)\n"
        )),
        "dial" | "dial-list" | "dial-token" => Some(format!(
            "ERR verb {verb} was removed with the network drive: a fleet reads other machines \
             (`aterm fleet init` on one machine, `aterm fleet join` on each other, then \
             `aterm ctl survey --fleet`)\n"
        )),
        _ => None,
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
/// touching a fleet hold). [`dispatch_owner`] routes `hold` here before the
/// silent generic `OwnerOnly` gate. Pure over `(rest, selector, scope, store)`.
fn dispatch_hold_verb(
    rest: &str,
    selector: Option<&Selector>,
    scope: Scope,
    store: &Store,
) -> String {
    use crate::fabric::{HOLD_DENIED, HoldIssuer, cmd_hold_waking};

    let issuer = match scope {
        #[cfg(any(unix, test))]
        Scope::Bridge => HoldIssuer::Bridge,
        // A drive connection's admitted `hold`: the remote slot, as its node.
        Scope::Owner if crate::fabric::serving_driver().is_some() => HoldIssuer::Remote,
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
        // The fleet gate refuses `hold` before it can reach here.
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => return "ERR denied\n".to_string(),
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
    // The `hold` mark wakes the session log's readers.
    let reply = cmd_hold_waking(store, rest, issuer, &|sid| {
        wake_sid_watchers(store, sid);
    });
    if reply == HOLD_DENIED {
        log_denial(
            AUDIT_SUBSYSTEM,
            "hold",
            aterm_containment::mode_or_containment(),
            "the fleet hold is bridge-issued only: an Owner token may neither name \
             origin=fleet nor set, replace or lift a standing fleet hold",
        );
    }
    // THE OWNER'S SLOT WAITS ON THE JOURNAL; THE FLEET'S IS ANSWERED AT ONCE
    // (the review's KR-J-1 of ATERM-KI-1016-1). The fleet's halt has a
    // durable home of its own, the bus's standing halt row that every attach
    // of a bridge applies again, and the bridge reads its `OK` as "applied"
    // and nothing more (the fleet's ack, §5.3). It writes its holds one
    // session at a time on one blocking connection, so a wait per session
    // held back the halt of every later one — the whole bound each where
    // the lane could not capture (before the first frame, a cold restore's
    // drain, a successor before its Commit) — while those sessions were
    // still being driven, and an attach's lift of every session, which
    // changes nothing, waited too. Answered at once, the pass holds every
    // session, and the one capture the holds' own wakes owe
    // (`App::journal_owner_words`) carries them all to disk: sooner than a
    // pass that waited per session. The owner's slot, which only the journal
    // keeps, waits even where the act changed nothing: a retry of `hold on`
    // after `journaled=0` stands for a hold no image on disk carries yet.
    if hold_names_local_slot(rest, issuer) {
        word_journaled(store, reply)
    } else {
        reply
    }
}

/// Whether a `hold` line acts on the owner's slot (`origin=local`): every
/// Owner act, and a bridge's that names it ([`dispatch_hold_verb`]). A
/// bridge's act is the fleet's unless the line says otherwise, as
/// [`crate::fabric::cmd_hold`] reads it; the last `origin=` wins there too.
fn hold_names_local_slot(rest: &str, issuer: crate::fabric::HoldIssuer) -> bool {
    use crate::fabric::HoldIssuer;
    issuer == HoldIssuer::Owner
        || rest
            .split_whitespace()
            .skip(2)
            .filter_map(|tok| tok.strip_prefix("origin="))
            .last()
            == Some("local")
}

/// A VERB THAT MOVED A WORD ABOUT TYPING ANSWERS ONCE THE CRASH JOURNAL'S
/// IMAGE THAT CARRIES IT IS DURABLE (ATERM-KI-1016-1;
/// [`crate::crash_journal::Durable::settle`]): `hold`'s `OK hold=…` on the
/// owner's slot ([`hold_names_local_slot`]) and an `OK` of `meta set|unset
/// autopilot`. Their `OK` came as the word was
/// stored, before that image was renamed into place, and a kill sent at the
/// reply lost the hold. Past [`crate::crash_journal::WORD_DURABLE_WAIT`] the
/// `OK` says `journaled=0`; a refusal, and a registry no journaling lane runs
/// (`restore_session` off, headless, off unix), answer at once, as before.
fn word_journaled(store: &Store, reply: String) -> String {
    if !reply.starts_with("OK") {
        return reply;
    }
    let durable = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .journal_durable();
    let bound = durable.word_bound();
    durable.settle(reply, bound)
}

/// THE `meta` VERB'S ANSWER ([`serve_session_verb`]'s arm, all of it but the
/// proxy): `meta set|unset supervisor` first, then `harness_act`, then a
/// non-Owner `questions` or `autopilot` write refused, then
/// [`control_session::cmd_meta`]'s closed set. On an actual change `on_change`
/// runs the arm's wake (`Wake::MetaChanged`) BEFORE an Autopilot word's answer
/// waits on the journal ([`word_journaled`]): the window repaints and the
/// loop captures the word while the verb waits. One door for the dispatch
/// and a test (`meta_line_for_test`), so the wait the answer takes is the
/// wait the wire's answer takes (the review's KR-J-2 of ATERM-KI-1016-1:
/// the arm's wait could be taken out with every test green). `prove` is the
/// kernel's look at a self-claim's holder (the arm's [`prove_self_claim`] on
/// its PTY master).
#[allow(clippy::too_many_arguments)]
fn serve_meta(
    rest: &str,
    term: &Arc<Mutex<Terminal>>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    session: u64,
    ctx: &SessionCtx,
    store: &Store,
    scope: Scope,
    on_change: impl FnOnce(),
) -> String {
    // A PROVEN SELF-CLAIM'S LANDING, and a point it left to a person
    // (`harness_task::holds_until_another_turn`), is marked with the session's
    // counts of turns begun (`session_timeline::LandingMark`) before its face
    // is posted, so the window reopens it at a turn begun after it
    // (`App::reopen_self_claim_landing`). The engine's count is read FIRST,
    // before the act is admitted (the claim read, a kernel look for a
    // connection the claim is not bound to, the commit under the session's
    // locks): a turn whose report the PTY reader parses while the act is
    // admitted began after the holder decided its landing, and must stay past
    // the mark. Never from the status sweep's last reading, which may not yet
    // hold the landing turn's own reports. The screen's count is read with it,
    // so a busy a sweep's read began meanwhile stays past it too. Leaf locks,
    // each let go before the next: the terminal, then the timeline.
    let snapshot = control_session::harness_act_face(rest)
        .is_some_and(crate::harness_task::holds_until_another_turn)
        .then(|| {
            use aterm_core::terminal::program_state::ProgramStateWord as W;
            let report = term_lock(term).program_state();
            let left_at_rest = report
                .state
                .is_some_and(|state| matches!(state.state, W::Idle | W::Done | W::Error));
            let busy_entries = ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .agent()
                .busy_entries;
            control_session::HarnessActSnapshot {
                mark: crate::session_timeline::LandingMark {
                    program_turns: report.turns,
                    busy_entries,
                },
                left_at_rest,
            }
        });
    let (resp, changed) =
        control_session::cmd_meta_supervisor(ctx, scope, serving_connection(), prove, rest)
            .or_else(|| {
                let by = control_session::log_actor(scope, ctx);
                control_session::cmd_meta_harness_act(
                    ctx,
                    snapshot,
                    serving_connection(),
                    by,
                    prove,
                    rest,
                )
            })
            .or_else(|| control_session::meta_questions_denied(scope, rest))
            .unwrap_or_else(|| {
                let by = control_session::log_actor(scope, ctx);
                control_session::cmd_meta_by(term, store, session, ctx, rest, by)
            });
    if changed {
        // A proven claim's record is read again at each meta-change while its
        // identity is live (`self_claim`, the contract §4).
        crate::self_claim::meta_changed(ctx);
        on_change();
    }
    // The Autopilot word is answered once the journal carries it.
    if meta_moves_autopilot(rest) {
        word_journaled(store, resp)
    } else {
        resp
    }
}

/// THE `meta` ARM, WIRED ([`serve_session_verb`]'s arm, all of it but its
/// sink): [`serve_meta`], and on an actual change each wake
/// [`meta_change_wakes`] lists handed to `post`, in order. The arm passes the
/// event loop's proxy; the wire test (`meta_wakes_for_test`) a list. The loop
/// over the wakes is this function's, so a wake the arm stops posting fails
/// that test. An `events` watcher is woken by the change's `meta-change`
/// record, not here.
#[allow(clippy::too_many_arguments)]
fn serve_meta_wired(
    rest: &str,
    term: &Arc<Mutex<Terminal>>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
    session: u64,
    ctx: &SessionCtx,
    store: &Store,
    scope: Scope,
    post: &mut dyn FnMut(Wake),
) -> String {
    serve_meta(rest, term, prove, session, ctx, store, scope, || {
        for wake in meta_change_wakes(session, ctx.self_id.as_str(), rest) {
            post(wake);
        }
    })
}

/// THE WAKES THE `meta` ARM POSTS on an actual change of session `session`
/// (sid `sid`, line `rest`), in the order it posts them
/// ([`serve_meta_wired`]): `Wake::MetaChanged` (the tab's repaint), then, for
/// a proven self-claim's recorded act, the face that act tells
/// (`Wake::HarnessFact`, FOREVER LAND §10.4: `landed@v1` lands the task,
/// `left-to-person@v1` leaves it to a person, any other act reopens it).
fn meta_change_wakes(session: u64, sid: &str, rest: &str) -> Vec<Wake> {
    let mut wakes = vec![Wake::MetaChanged { session }];
    if let Some(face) = control_session::harness_act_face(rest) {
        wakes.push(Wake::HarnessFact {
            sid: sid.to_string(),
            fact: crate::HarnessFact::SelfTask(face),
        });
    }
    wakes
}

/// Whether a `meta` line writes the Autopilot word (`set autopilot …`,
/// `unset autopilot`): a word about typing ([`word_journaled`]).
fn meta_moves_autopilot(rest: &str) -> bool {
    let mut words = rest.split_whitespace();
    matches!(words.next(), Some("set" | "unset")) && words.next() == Some("autopilot")
}

const FABRIC_USAGE: &str =
    "ERR usage: fabric status | fabric attach [<command...>] | fabric retire <sid>...\n";

/// `story chose [<text>]`: the supervisor's question choice for a session, a
/// RECORD (`status story=`). Owner-only (a child edge must not write a choice
/// for a session it does not drive) and session-scoped by selector: `@<sid>
/// story chose`, or bare `story …` for the connection's own session, which
/// [`dispatch_owner`] takes from the front terminal; the `None` arm is
/// defensive. Grammar is checked in [`control_query::cmd_story`].
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
            "story is owner-class: an edge token may not write the story",
        );
        return "ERR denied\n".to_string();
    }
    let session = match selector {
        Some(Selector::Local(n)) => *n,
        Some(sel) if !sel.is_self() => {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            match g.by_selector(sel) {
                Some(h) => h.local_id,
                None => return "ERR no such session\n".to_string(),
            }
        }
        _ => match self_session {
            Some(s) => s,
            None => return "ERR story: no session (name one: @<sid> story …)\n".to_string(),
        },
    };
    // The teller, for the session log's `story` mark (addendum A15).
    let by = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_local(session)
        .map_or(aterm_slog::Actor::Nobody, |h| {
            control_session::log_actor(scope, &h.ctx)
        });
    control_query::cmd_story(proxy, session, rest, by)
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
            Some(sel) if !sel.is_self() => g.by_selector(sel),
            _ => return REKEY_USAGE.to_string(),
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
        let tab = Some(Selector::parse("s-00000000000000000001"));
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
    let ctx = {
        let g = store.read().unwrap_or_else(|p| p.into_inner());
        let handle = match selector {
            Some(sel) if !sel.is_self() => g.by_selector(sel),
            _ => match self_session {
                Some(s) => g.by_local(s),
                None => {
                    return "ERR topic: no session (name one: @<sid> topic ls)\n".to_string();
                }
            },
        };
        match handle {
            Some(h) => h.ctx.clone(),
            None => return "ERR no such session\n".to_string(),
        }
    };
    // A change records its `topic` row, which wakes the session's watchers.
    crate::fabric::cmd_topic(&ctx, rest)
}

/// `fabric` sub-forms. `attach`'s argv is the line's whitespace words, never
/// via a shell, read TWO WAYS ([`cmd_fabric_attach`]): as written — what every
/// earlier `aterm fabric on` sent — and each pct-decoded
/// (`aterm_codec::pct::split_words`), so a word with a space in it (an
/// argv-form `[fabric] command`'s) arrives as `%20` and is one word.
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

/// An argv as the `command=` token every build has written:
/// `pct_encode(argv.join(" "))`, which a reader splits on whitespace. A word
/// with a space in it does not survive that split; [`argv_token`] does.
#[cfg(unix)]
fn command_token(argv: &[String]) -> String {
    pct_encode(&argv.join(" "))
}

/// An argv as ONE `argv=` token: its line of words
/// (`aterm_codec::pct::join_words` — a word's own whitespace or `%` escaped,
/// so it stays one word) pct-encoded, which `aterm fabric` decodes back into
/// the argv whole (`aterm_link::fabric::status_argv`). Beside `command=`, so a
/// reader from before it reads `command=` exactly as it always did, and a
/// reader that knows it never guesses which encoding an instance used.
#[cfg(unix)]
fn argv_token(argv: &[String]) -> String {
    pct_encode(&aterm_codec::pct::join_words(argv))
}

/// `OK state=<absent|connected|stale|stalled|disconnected> supervised=<0|1>
/// command=<pct|-> argv=<pct|-> reason=<token|-> rtt_ms=<n|-> link_age_ms=<n|->
/// build=<token> bridge_lane=<n|-> bridge_build=<token|-> lane_faults=<n>`.
///
/// `state=` is the link token `status` carries; `supervised=` is the latch
/// `post` reads; `command=` is the supervised argv (or the launch one when
/// unarmed) joined by spaces and pct-encoded, as every build has written it
/// ([`command_token`]), and `argv=` the same argv as a line of words
/// (`aterm_codec::pct::join_words`, so a word with a space in it stays one),
/// pct-encoded ([`argv_token`]). The next three are the bridge's broker-link report
/// (`fabric::link_report`). `build=` is this instance's own build and the
/// last three the bridge's `hello` and its lane (`fabric::cmd_hello`,
/// `fabric::note_lane_fault`): a `bridge_build=` other than `build=` is a
/// bridge of another build than its instance, `-` one from before `hello`.
fn fabric_status_line() -> String {
    #[cfg(unix)]
    let (supervised, command) = {
        let s = crate::fabric_launch::status();
        let command = s.armed.or(s.configured).map_or_else(
            || "- argv=-".to_string(),
            |argv| format!("{} argv={}", command_token(&argv), argv_token(&argv)),
        );
        (crate::fabric::bridge_supervised(), command)
    };
    #[cfg(not(unix))]
    let (supervised, command) = (false, "- argv=-".to_string());
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
            command_token(&running)
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
    } else {
        // The words AS WRITTEN (every earlier `on` sent the plain join, a `%`
        // in a word included) or EACH PCT-DECODED (a word with a space in it
        // arrives as `%20`): whichever IS a configured command. Either reading
        // must equal one, so reading two ways arms nothing the config does
        // not name.
        let decoded: Vec<String> = argv.iter().map(|w| aterm_codec::pct::decode(w)).collect();
        let refused = format!(
            "ERR fabric not configured command={}\n",
            command_token(&argv)
        );
        let named = |c: &Vec<String>| {
            on_disk.as_ref() == Some(c) || snapshot.configured.as_ref() == Some(c)
        };
        match [argv, decoded].into_iter().find(named) {
            Some(configured) => configured,
            None => return refused,
        }
    };
    match crate::fabric_launch::arm(argv.clone()) {
        Ok(()) => format!("OK attached command={}\n", command_token(&argv)),
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
/// `outbox`, `outbox sent`, `log-take` and `log-landed`.
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
) -> String {
    if !matches!(selector, None | Some(Selector::SelfTok)) || !scope.is_bridge() {
        log_denial(
            AUDIT_SUBSYSTEM,
            &format!("bridge-only {verb}"),
            aterm_containment::mode_or_containment(),
            &format!("only the inherited bridge connection may run {verb}"),
        );
        return "ERR denied\n".to_string();
    }
    let generation = crate::fabric::lane_generation();
    // A `mail` or `mail-landed` mark wakes the session log's readers.
    let log_wake = |sid: &aterm_session::SessionId| wake_sid_watchers(store, sid);
    let reply = match verb {
        "deliver" => crate::fabric::cmd_deliver_waking(store, rest, &log_wake),
        // The bridge's own broker link, gated to the lane's incarnation. A
        // replaced bus log (`forgot=1`) is another log's offsets to the bus
        // mirror too: its cursors start over.
        // `link mirror unretained` (session log Phase 6 step 6E-3) is an
        // `auto` bridge's word that the node does not mirror: the store hears
        // it as it hears a hello ([`note_mirror_word`]).
        "link" => {
            let reply = crate::fabric::cmd_link_after_log_switch(store, rest, generation, || {
                crate::session_log_mirror::forget_landed(store);
            });
            if reply == "OK\n" && rest.split_whitespace().next() == Some("mirror") {
                note_mirror_word(store, generation);
            }
            reply
        }
        // THE BUS MIRROR (session log phase 4), gated the same way.
        "log-take" => crate::session_log_mirror::cmd_log_take(store, rest, generation),
        "log-landed" => crate::session_log_mirror::cmd_log_landed(store, rest, generation),
        // Its lane grammar and build, gated the same way. A RECORDED hello
        // (the owning lane's, answered `OK` and not `stale=1`) says whether
        // the node mirrors its session logs ([`note_mirror_word`]).
        "hello" => {
            let reply = crate::fabric::cmd_hello(rest, generation);
            if reply.starts_with("OK ") && !reply.starts_with("OK stale=1") {
                note_mirror_word(store, generation);
            }
            reply
        }
        // `outbox sent` arrives as `verb == "outbox"`; the handler splits the
        // sub-form, like `inbox get`/`inbox seen`.
        "outbox" => crate::fabric::cmd_outbox_verb_waking(store, rest, &log_wake),
        _ => no_handler(verb),
    };
    if reply.starts_with("ERR usage") {
        crate::fabric::note_lane_fault(generation);
    }
    reply
}

/// The words after `task` when `await`'s argument is the LIST form (`task`
/// then an `@<sel>[,…]` among its words), `None` for every other `await`.
fn await_task_list(rest: &str) -> Option<Vec<&str>> {
    let mut words = rest.split_whitespace();
    if words.next() != Some("task") {
        return None;
    }
    let words: Vec<&str> = words.collect();
    words.iter().any(|w| w.starts_with('@')).then_some(words)
}

/// `await task @<sel>[:<rev>][,…] [any|all] [since=<rev>] [timeout=<ms>]`:
/// resolve every target in THIS instance as `subscribe` does — the first one
/// that does not resolve, or that a scoped caller may not read, fails the
/// whole request (a wait that silently dropped a worker would never answer
/// for it) — then park on them all ([`control_session::await_task`]), which
/// answers ONE ROW PER TARGET in request order: two selectors naming one
/// session (`@3,@s-<its sid>`) are two targets, each with its own since, so a
/// client reads row `i` as target `i`'s. The ctl client groups targets by the
/// instance that hosts them, so a sibling's sid is the CLIENT's to route.
fn dispatch_await_task(
    words: &[&str],
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
) -> String {
    let args = match crate::task_record::parse_await_task(words, true) {
        Ok(args) => args,
        Err(usage) => return usage,
    };
    let mut held: Vec<(u64, Arc<SessionCtx>, Option<u64>)> = Vec::new();
    for target in &args.targets {
        // A list element that names nothing is refused like a bare `@`
        // ([`empty_selector_refusal`]), never read as the active tab.
        if !aterm_types::control_verbs::selector_body_names_something(&target.selector) {
            return format!("{}\n", aterm_types::control_verbs::EMPTY_SELECTOR);
        }
        let sel = Selector::parse(&target.selector);
        let resolved = match resolve(store, &sel) {
            Resolution::SelfTok => resolve_active(active),
            Resolution::Hosted(target) => Some(target),
            Resolution::Missing => None,
            Resolution::Ambiguous(refusal) => return refusal,
        };
        let Some((_, _, local_id, ctx)) = resolved else {
            return if sel.is_self() {
                NO_ACTIVE_TERMINAL.to_string()
            } else {
                format!("ERR no such session @{}\n", target.selector)
            };
        };
        if !scope.is_owner_class() && !cross_session_authorized(scope, "await", &ctx) {
            log_denial(
                AUDIT_SUBSYSTEM,
                &format!("await task -> {}", ctx.self_id.as_str()),
                aterm_containment::mode_or_containment(),
                "no authorizing edge for a cross-session task wait",
            );
            return "ERR denied\n".to_string();
        }
        held.push((local_id, ctx, target.since.or(args.since)));
    }
    let targets: Vec<control_session::TaskTarget<'_>> = held
        .iter()
        .map(|(local_id, ctx, since)| control_session::TaskTarget {
            local_id: *local_id,
            ctx,
            since: *since,
        })
        .collect();
    control_session::await_task(
        store,
        &targets,
        args.combine,
        args.timeout_ms
            .unwrap_or(aterm_control::wire::WAIT_DEFAULT_MS),
    )
}

/// THE STORE HEARS WHETHER THE NODE MIRRORS (`session_log_mirror::Shared`)
/// from the owning bridge's word on it (`fabric::bridge_mirror_word`): a
/// recorded `hello`, and its `link mirror` report — an `auto` bridge's
/// `unretained` (session log Phase 6 step 6E-3). An `auto` hello not yet
/// judged leaves the word as it was. Read off the link before the store's
/// read guard is taken, so the two locks never nest here.
fn note_mirror_word(store: &Store, generation: Option<crate::fabric::BridgeGeneration>) {
    let _ = crate::fabric::with_current_bridge(generation, || {
        if let Some(served) = crate::fabric::bridge_mirror_word() {
            store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .mirror()
                .set_served(served);
        }
    });
}

/// The reply for a table row its access class's dispatch has no arm for: refused,
/// never a panic, so a verb added to `VERBS` before its handler fails closed
/// instead of dropping the connection thread.
fn no_handler(verb: &str) -> String {
    format!("ERR {verb} has no handler\n")
}

/// The class handler a request goes to ([`route`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Route {
    /// `Access::AnyScopeMeta`: [`dispatch_meta_verb`].
    Meta,
    /// `Access::BridgeOnly`: [`dispatch_bridge_verb`].
    Bridge,
    /// `Access::OwnerOnly`, whatever the target: [`dispatch_owner`].
    Owner,
    /// A scoped `Target::App` verb: [`dispatch_app`].
    App,
    /// A scoped session verb: [`dispatch_session`].
    Session,
}

/// A request's route and table form, or the line that refuses it before any
/// handler: a removed verb's tombstone ([`retired_verb_error`]), or `ERR unknown
/// verb`. The route is the row's `(access, target)`, read once off the form the
/// request was parsed with (a listed sub-form row shares its verb's access and
/// target, which aterm-types asserts at compile time), so no handler
/// re-classifies the verb and no arm can serve a verb the table does not list.
fn route(verb: &str, form: Option<Resolved>) -> Result<(Route, Resolved), String> {
    use aterm_types::control_verbs::{Access, Target as RowTarget};
    if let Some(error) = retired_verb_error(verb) {
        return Err(error);
    }
    let Some(form) = form else {
        return Err("ERR unknown verb (try: help)\n".to_string());
    };
    let route = match (form.row.access, form.row.target) {
        (Access::AnyScopeMeta, _) => Route::Meta,
        (Access::BridgeOnly, _) => Route::Bridge,
        (Access::OwnerOnly, _) => Route::Owner,
        (Access::Scoped, RowTarget::App) => Route::App,
        // A scoped row never targets `Meta` (asserted in aterm-types).
        (Access::Scoped, RowTarget::Session | RowTarget::Meta) => Route::Session,
    };
    Ok((route, form))
}

/// What a request is dispatched with: its connection's scope and the instance
/// state the class handlers read.
struct DispatchCx<'a> {
    active: &'a ActiveHandle,
    store: &'a Store,
    scope: Scope,
    proxy: &'a EventLoopProxy<Wake>,
    sock_dir: &'a std::path::Path,
}

/// Full request dispatch: [`route`] once, then the class handler. Classification
/// precedes terminal resolution, so native-only windows stay controllable
/// without a fabricated ActiveSession, and only the session and App handlers
/// look a selector up.
fn dispatch_request(request: &Request, cx: &DispatchCx<'_>) -> ControlReply {
    let head = request.head();
    let (route, form) = match route(head.verb, request.form()) {
        Ok(routed) => routed,
        Err(refusal) => return refusal.into(),
    };
    match route {
        Route::Meta => dispatch_meta_verb(&form, &head, cx.scope, cx.proxy).into(),
        Route::Bridge => dispatch_bridge_verb(
            head.verb,
            head.rest,
            head.selector.as_ref(),
            cx.scope,
            cx.store,
        )
        .into(),
        Route::Owner => dispatch_owner(&head, cx, || {
            resolve_active(cx.active).ok_or(NO_ACTIVE_TERMINAL)
        })
        .into(),
        Route::App => dispatch_app(&head, &form, cx),
        Route::Session => dispatch_session(&head, &form, cx),
    }
}

/// Which session an owner-only request acts on ([`owner_aim`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OwnerAim {
    /// Instance state, or a session named by argument or selector: answered with
    /// no terminal.
    Instance,
    /// The connection's own session: the front terminal, else `ERR no active
    /// terminal`.
    OwnSession,
}

/// What an owner-only `verb` acts on: the connection's own session for `grant`,
/// `revoke` and `whoami` (the authority of the session asking), and for `topic`
/// and `story` with no selector or `@.`; everything else is instance state, so a
/// windowless instance or a native tab in front still answers it.
fn owner_aim(verb: &str, selector: Option<&Selector>) -> OwnerAim {
    match verb {
        "grant" | "revoke" | "whoami" => OwnerAim::OwnSession,
        "topic" | "story" if selector.is_none_or(Selector::is_self) => OwnerAim::OwnSession,
        _ => OwnerAim::Instance,
    }
}

/// Owner-only verbs (`Access::OwnerOnly`): only an owner-class connection may run
/// them, and a selector is refused ([`owner_only_refusal`]). `hold`, `rekey`,
/// `topic` and `story` name their session by argument or selector and decide their
/// own refusals, each audited, instead of that silent generic gate (`hold` first,
/// so a fleet halt lands on a windowless instance). Then [`owner_aim`] decides
/// once whether the verb needs `own`, which yields the front terminal or the line
/// refusing its absence; no other owner verb asks for a terminal.
fn dispatch_owner(
    head: &Head<'_>,
    cx: &DispatchCx<'_>,
    own: impl FnOnce() -> Result<Target, &'static str>,
) -> String {
    let Head {
        selector,
        verb,
        rest,
    } = *head;
    let selector = selector.as_ref();
    let DispatchCx {
        store,
        scope,
        proxy,
        ..
    } = *cx;
    match verb {
        "hold" => return dispatch_hold_verb(rest, selector, scope, store),
        // `rekey` heals the tab its selector names, never the connection's own.
        "rekey" => return dispatch_rekey_verb(rest, selector, scope, store),
        "topic" | "story" => {}
        _ => {
            // A fleet peer's `survey`, at the level its listener proved.
            if let Some(reply) = fleet_survey(verb, rest, scope, store) {
                return reply;
            }
            if let Some(refusal) = owner_only_refusal(verb, selector, scope) {
                return refusal;
            }
        }
    }
    let own_target = match owner_aim(verb, selector) {
        OwnerAim::Instance => None,
        OwnerAim::OwnSession => match own() {
            Ok((_, _, session, ctx)) => Some((session, ctx)),
            Err(refusal) => return refusal.to_string(),
        },
    };
    let own_session = own_target.as_ref().map(|(session, _)| *session);
    let own_ctx = own_target.as_ref().map(|(_, ctx)| ctx.as_ref());
    match verb {
        "topic" => dispatch_topic_verb(rest, selector, scope, store, own_session),
        "story" => dispatch_story_verb(rest, selector, scope, store, own_session, proxy),
        "sessions" => match rest.trim() {
            "bridge" => control_session::cmd_sessions_bridge(store),
            "status" => control_session::cmd_sessions_status(proxy),
            _ => refuse_stray_args(verb, rest)
                .unwrap_or_else(|| control_session::cmd_sessions_store(store, Some(proxy))),
        },
        // The survey reads only what the sessions publish: answerable with no
        // terminal, like `sessions`.
        "survey" => crate::survey::cmd_survey(
            store,
            rest,
            crate::survey::SurveyLevel::Content,
            usize::MAX,
            &crate::survey::Here::now(),
        ),
        "who" => refuse_stray_args(verb, rest).unwrap_or_else(|| control_session::cmd_who(store)),
        // The exit ledger is a store read like `sessions`: the instance's past
        // stays answerable with no terminal at all.
        "exits" => control_session::cmd_exits(store, rest),
        // The op-level authority primitives and their connection-grain twins
        // share the repaint poke: a successful act moves the tab marks, which no
        // funnel epoch would otherwise notice.
        "grant" => on_own_session(own_ctx, verb, |ctx| {
            poke_connections_on_ok(proxy, control_session::cmd_grant(ctx, scope, rest))
        }),
        // A revoke also reaches a closed session's still-revocable grants.
        "revoke" => on_own_session(own_ctx, verb, |ctx| {
            poke_connections_on_ok(
                proxy,
                control_session::cmd_revoke_with_closed(ctx, store, scope, rest),
            )
        }),
        "whoami" => on_own_session(own_ctx, verb, |ctx| {
            refuse_stray_args(verb, rest).unwrap_or_else(|| control_session::cmd_whoami(ctx, scope))
        }),
        // `connect`/`disconnect`: the declarative session-connection verbs. Their
        // endpoints ride as `dst=`/`src=`, so they resolve both from the store and
        // act on the destination's table.
        "connect" => {
            poke_connections_on_ok(proxy, control_session::cmd_connect(store, scope, rest))
        }
        "disconnect" => {
            poke_connections_on_ok(proxy, control_session::cmd_disconnect(store, scope, rest))
        }
        // `flows`: the aggregated connection graph (pure read, no poke).
        "flows" => control_session::cmd_flows(store, scope, rest),
        // `raise <sid>`: raise the hosting window and select the tab (a
        // main-thread hop; window and tab state is App-owned).
        "raise" => control_session::cmd_raise(proxy, store, scope, rest),
        // `appnotice <lane> <text>`: a pull-down row from outside the process.
        "appnotice" => control_query::cmd_appnotice(proxy, rest),
        // `notice <post|progress|done|ask|later|dismiss|act> …`: the message band's
        // write face.
        "notice" => control_query::cmd_notice(proxy, rest),
        // `devtools hand-over [now]`: the window's Developer Tools hand-over (a
        // headless instance says so).
        "devtools" => control_query::cmd_devtools(proxy, rest),
        // `person look <window>|off` | `person menubar press <at>`: a debug
        // build's stand-in for the person at the window, armed at launch.
        "person" => control_media::cmd_person(proxy, rest),
        // `fabric status|attach`: this process's bridge supervisor (a windowless
        // instance is the one that needs it).
        "fabric" => dispatch_fabric_verb(rest, selector, scope, store),
        _ => no_handler(verb),
    }
}

/// `act` on the connection's own session, which [`dispatch_owner`] resolved
/// because [`owner_aim`] named it; an arm that [`owner_aim`] does not name fails
/// closed with [`no_handler`].
fn on_own_session(
    own: Option<&SessionCtx>,
    verb: &str,
    act: impl FnOnce(&SessionCtx) -> String,
) -> String {
    own.map_or_else(|| no_handler(verb), act)
}

/// App-target verbs (scoped `Target::App`): the front window, or the window
/// hosting the session an Owner's `@<sid>` names ([`app_route`]). Gated once,
/// before the arm and after authorization, so an unauthorized caller learns
/// nothing:
///
/// * the fleet halt (design §5.3), first on the aimed session, whose own
///   `reason=`/`origin=` an aimed `tab close` answers, then on the App lane
///   ([`crate::fabric::app_halt_refusal`]): these verbs drive whatever window
///   they reach, so the question is whether any session here is held, and the
///   row's input class (`Input::PTY`) decides which verbs it binds (`invoke`,
///   `hwkey`, `pointer`, `pane`, `tab`, `confirm`), then the gateway cards an
///   `open` may raise ([`app_verb_halt`]);
/// * the edit hold (amacs §6.7): the input-writing App verbs (`hwkey`,
///   `pointer` but its `status` read, `invoke Paste`) answer `ERR busy editing`
///   while the terminal on screen is a session an `edit` ticket holds, and go
///   on while a native view (the edit's own) is in front
///   ([`crate::control_edit::hold::refusal`]);
/// * the unread-input gate (`input_stall`) against the front session, the one
///   the input-writing App verbs (`hwkey`, `pointer`, `invoke Paste`) write to
///   whatever the aim; a leading `unread=ok` lifts it for one gesture, as on
///   `send`/`key`, and a slow read warns in the `OK` rather than refusing
///   (owner, 2026-09-29).
fn dispatch_app(head: &Head<'_>, form: &Resolved, cx: &DispatchCx<'_>) -> ControlReply {
    let Head {
        selector,
        verb,
        rest,
    } = *head;
    let store = cx.store;
    let aim = match app_route(cx.scope.principal(), selector.as_ref(), |selector| {
        resolve(store, selector)
    }) {
        Ok(aim) => aim,
        Err(refusal) => return refusal.into(),
    };
    let class = form.row.input_class();
    if let Some(refusal) = app_verb_halt(&aim, store, class, verb, rest) {
        return refusal.into();
    }
    let active = resolve_active(cx.active);
    let (unread_ok, rest) = control_input::take_app_unread_ok(class, rest);
    // The edit hold (amacs §6.7) for `hwkey`, `pointer` and `invoke Paste`, which
    // type into the terminal on screen.
    let front = active.as_ref().map(|(_, _, front, _)| *front);
    if let Some(refusal) = crate::control_edit::hold::refusal(verb, rest, None, front) {
        return refusal.into();
    }
    let admitted = active
        .as_ref()
        .map_or(crate::input_stall::Admission::Open, |(_, _, _, ctx)| {
            crate::input_stall::gate(ctx, class, verb, rest.as_bytes(), unread_ok)
        });
    let warning = match admitted {
        crate::input_stall::Admission::Refuse(refusal) => return refusal.into(),
        crate::input_stall::Admission::Warn(warning) => Some(warning),
        crate::input_stall::Admission::Open => None,
    };
    // `invoke Paste` types the clipboard into the front tab: its caller rides
    // the hop and is credited there if the paste lands at once (S9,
    // `App::invoke_typist`), read here, on the lane that serves it.
    let typist = (verb == "invoke"
        && crate::input_stall::writes_input(class, verb, rest.as_bytes()))
    .then(|| {
        active
            .as_ref()
            .and_then(|(_, _, _, ctx)| serving_caller(cx.scope, ctx))
    })
    .flatten();
    let active_term = active.map(|(term, _, _, _)| term);
    dispatch_app_verb(verb, rest, form, &aim, cx, active_term.as_ref(), typist)
        .warned(warning.as_deref())
}

/// The fleet halt's refusal of an App-target verb, or `None`. A verb whose input
/// `class` the halt binds (`Input::PTY`) asks the aimed session's own hold first,
/// so a held one answers with its own `reason=`/`origin=`, then the App lane's
/// question, whether any session here is held ([`crate::fabric::app_halt_refusal`]).
/// An aimed `tab close N` retires whatever session tab N hosts, so an unheld aim
/// does not clear a held window-mate. The person's own acts, where a development
/// build stands a test in the person's place ([`control_media::person_invoke_act`]),
/// are theirs under any hold. `open` of a gateway card (the palette, a tab's menu,
/// the Overview) is the same question ([`crate::fabric::open_halt_refusal`]): each
/// card is `invoke`'s rows behind a key.
fn app_verb_halt(
    aim: &AppAim,
    store: &Store,
    class: Input,
    verb: &str,
    rest: &str,
) -> Option<String> {
    let person = verb == "invoke" && control_media::person_invoke_act(rest);
    if class.contains(Input::PTY) && !person {
        if let AppAim::Aimed((_, _, _, ctx)) = aim
            && let Some(refusal) = crate::fabric::hold_refusal(ctx)
        {
            return Some(refusal);
        }
        if let Some(refusal) = crate::fabric::any_hold_refusal(store) {
            return Some(refusal);
        }
    }
    crate::fabric::open_halt_refusal(store, verb, rest)
}

/// Session-target verbs: [`session_route`] picks the front terminal or the named
/// session, resolved once; [`serve_session_verb`] gates and serves the verb on it.
/// With no terminal in front, a bare request may still reach a native front
/// ([`sessionless_front`]).
///
/// Two forms name no session of their own and are answered first, whatever
/// terminal there is or is not: `log n`, the instance's node log (session log
/// 5J-3, Owner-only like `brief all`, [`node_log_reply`]), and `await task`'s
/// list form, which names its targets as an argument like `subscribe` and
/// resolves them itself ([`dispatch_await_task`]; a leading selector keeps the
/// single form, answered by the session's `cmd_await`).
fn dispatch_session(head: &Head<'_>, form: &Resolved, cx: &DispatchCx<'_>) -> ControlReply {
    let selector = head.selector.as_ref();
    if head.verb == "log" && crate::session_log_node::names_node_log(head.rest) {
        return node_log_reply(head.rest, selector, cx.scope).into();
    }
    if head.verb == "await"
        && selector.is_none()
        && let Some(words) = await_task_list(head.rest)
    {
        return dispatch_await_task(&words, cx.active, cx.store, cx.scope).into();
    }
    let resolution = selector.map_or(Resolution::SelfTok, |s| resolve(cx.store, s));
    let SessionRoute {
        active_target,
        decision,
    } = session_route(selector, &resolution, cx.active, cx.scope);
    match decision {
        NativeControlDecision::ResolveSession => {}
        NativeControlDecision::Denied => return "ERR denied\n".into(),
        NativeControlDecision::NoActiveTerminal => {
            return sessionless_front(head, cx).unwrap_or_else(|| NO_ACTIVE_TERMINAL.into());
        }
        // A session that closed in this process may still answer `log` and
        // `brief` from the closed-session index; anything else is no such
        // session.
        NativeControlDecision::NoSuchSession => {
            return closed_session_reply(selector, head.verb, head.rest, cx.store, cx.scope).into();
        }
        NativeControlDecision::WithoutSession => {
            return "ERR invalid control target\n".into();
        }
    }
    // Capture the front tab's id before `active_target` is consumed: an explicit
    // `@<sid>` naming it goes through the App input seam (`InputRoute`).
    let front_active_session = active_target.as_ref().map(|(_, _, session, _)| *session);
    let target = match resolution {
        Resolution::SelfTok => active_target,
        Resolution::Hosted(target) => Some(target),
        Resolution::Missing => None,
        // Two instances answer to this id: refuse, do not pick one.
        Resolution::Ambiguous(refusal) => return refusal.into(),
    };
    let Some(target) = target else {
        return if selector.is_some_and(|s| !s.is_self()) {
            // It closed between the route and the resolve.
            closed_session_reply(selector, head.verb, head.rest, cx.store, cx.scope)
        } else {
            NO_ACTIVE_TERMINAL.to_string()
        }
        .into();
    };
    let mut tail = ReplyTail::default();
    let body = serve_session_verb(head, form, &target, front_active_session, cx, &mut tail);
    ControlReply::with_tail(body, tail)
}

/// A bare or `@.` session verb with no terminal in front (a native Settings or
/// Editor tab): `paste`, `key` and `resize px` post the input event a person's
/// own paste, keypress or window drag makes, which `App::input` delivers to the
/// native view under its own boundary, and an Owner's framebuffer `image` (any
/// form but `image read`) captures the front native window. Each decision is its
/// `sessionless_front_*` classifier's, by [`native_control_decision`]'s App case
/// (an Edge is denied). `None` for every other request, which the caller refuses
/// with [`NO_ACTIVE_TERMINAL`].
fn sessionless_front(head: &Head<'_>, cx: &DispatchCx<'_>) -> Option<ControlReply> {
    let Head {
        selector,
        verb,
        rest,
    } = *head;
    let selector = selector.as_ref();
    let principal = cx.scope.principal();
    let route = match verb {
        "paste" => sessionless_front_paste_event(
            verb,
            || control_input::paste_text(rest),
            selector,
            false,
            principal,
        )
        .map(|route| route.map_err(String::from)),
        "key" => sessionless_front_key_event(rest, selector, false, principal)
            .map(|route| route.map_err(String::from)),
        "resize" => sessionless_front_window_resize(verb, rest, selector, false, principal),
        "image" if cx.scope.is_owner_class() && rest.split_whitespace().next() != Some("read") => {
            return Some(control_media::cmd_image(cx.proxy, rest, cx.sock_dir, None));
        }
        _ => None,
    }?;
    Some(
        match route {
            Ok(event) => control_input::input_reply_to_str(post_input_reply(cx.proxy, vec![event])),
            Err(error) => error,
        }
        .into(),
    )
}

/// What a request's selector names, looked up once per request
/// ([`resolve`]).
enum Resolution {
    /// No selector or `@.`: the connection's own session, which the caller
    /// takes from the front.
    SelfTok,
    /// A session this instance hosts.
    Hosted(Target),
    /// No session here answers to it: an unknown number or id, or a spelling
    /// that names no session.
    Missing,
    /// The id resolves here and a live foreign instance also publishes it:
    /// [`ambiguous_sid_refusal`]'s line is the answer.
    Ambiguous(String),
}

/// Resolve a selector, independent of front content, and refuse an id that
/// names two places.
///
/// The ambiguity check lives in the resolver because several seams resolve
/// selectors without going through [`dispatch_request`] (binary `post`,
/// `feed-bin`/`paste-bin` at both fence and effect, `subscribe` targets), and
/// those write input or stream output; none can reach a hosted target without
/// passing it.
fn resolve(store: &Store, selector: &Selector<'_>) -> Resolution {
    if selector.is_self() {
        return Resolution::SelfTok;
    }
    // Cloned out before the guard drops, so the registry lock is never held
    // across a `Terminal` lock or the ambiguity probe's read.
    let found = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_selector(selector)
        .map(|h| (h.term.clone(), h.master, h.local_id, h.ctx.clone()));
    // A miss that names the warm spare taints it (`crate::spare`); a hit on a
    // session before its start record is counted against its rc generation.
    match (&found, selector) {
        (None, Selector::Sid(id)) => {
            crate::spare::control_refusal(Some(&SessionId::new(id.as_str())), None);
        }
        (None, Selector::Local(id)) => crate::spare::control_refusal(None, Some(*id)),
        (Some((_, _, _, ctx)), _) => crate::spare::note_control(&ctx.witness),
        (None, Selector::SelfTok | Selector::Invalid(_)) => {}
    }
    let Some(target) = found else {
        return Resolution::Missing;
    };
    // Only a hosted id can be ambiguous, so probe only on a hit (a miss costs
    // nothing). `live_holder` reads `graph/<sid>` for a live foreign pid and is
    // gated on ids this process adopted; minted ids are answered from a
    // register.
    match ambiguous_sid_refusal(selector) {
        Some(refusal) => Resolution::Ambiguous(refusal),
        None => Resolution::Hosted(target),
    }
}

/// A session-class request's route: the front terminal and the classifier's
/// verdict for its selector, assembled in one place for the dispatcher and
/// `native_control_conformance`.
struct SessionRoute {
    active_target: Option<Target>,
    decision: NativeControlDecision,
}

fn session_route(
    selector: Option<&Selector<'_>>,
    resolution: &Resolution,
    active: &ActiveHandle,
    scope: Scope,
) -> SessionRoute {
    let active_target = resolve_active(active);
    let explicit = selector.is_some_and(|selector| !selector.is_self());
    let explicit_live = matches!(resolution, Resolution::Hosted(_) | Resolution::Ambiguous(_));
    let decision = native_control_decision(
        active_target.is_some(),
        explicit_live,
        scope.principal(),
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
    let selector = control_request::head(line).selector;
    let resolution = selector.map_or(Resolution::SelfTok, |s| resolve(store, &s));
    session_route(selector.as_ref(), &resolution, active, scope).decision
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
/// ([`argument_escalation`]).
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
            Some(Selector::Sid(sid)) => index.by_sid(&SessionId::new(sid.as_str())),
            None | Some(Selector::SelfTok | Selector::Invalid(_)) => None,
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
    if argument_escalation(verb, rest) == Some(Need::OwnerOnly) && !scope.is_owner_class() {
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
/// while this launch keeps a journal (on by default since D3; with
/// `journal = false`, or on a desk that keeps none, nothing is read and
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
/// handing it to a subscription lane. [`run_subscribe_socket`] there
/// never reads another request line — the client thereafter only reads
/// `DELTA`/`EVENT`/`GAP` frames — while the ordinary lane returns at once.
enum ServeDisposition {
    Close,
    /// The request, parsed, goes with the connection: the push handler reads
    /// its head ([`run_subscribe_with_peer_probe`]), never the line again.
    Subscribe {
        request: Request,
        scope: Scope,
    },
}

/// An artifact reply's handoff, carried by its connection's outbox
/// (`control_lanes.rs`) from the moment the answer is queued until it is over:
/// a write-only permit until the last byte is out, or a guard until one line
/// after the challenge decides. Whatever ends the connection first drops it —
/// the deadline, a failed write, the peer's hang-up, a lane's unwind, the
/// parker's stop — and the drop fails closed: a guard not yet classified goes
/// to quarantine ([`AwaitedAck`]), never released.
enum PendingHandoff {
    WriteOnly {
        deadline: std::time::Instant,
        _permit: WriteOnlyRetention,
    },
    Acknowledged(AwaitedAck),
}

impl PendingHandoff {
    /// When the handoff fails if it has not finished: the write, and for a
    /// guarded reply its acknowledgement too.
    fn deadline(&self) -> std::time::Instant {
        match self {
            Self::WriteOnly { deadline, .. } => *deadline,
            Self::Acknowledged(ack) => ack.deadline,
        }
    }
}

/// A guarded reply's guard, and the challenge its peer must echo. Dropped
/// before [`Self::acknowledged`], it quarantines the guard: a write that did
/// not finish is `WriteFailed` (path bytes may be visible), a frame that is
/// out but was not acknowledged is `AcknowledgeFailed`.
struct AwaitedAck {
    /// `None` once classified.
    retention: Option<ReplyRetention>,
    nonce: String,
    deadline: std::time::Instant,
    /// The whole frame, challenge included, is on the wire: one line decides.
    on_wire: bool,
}

impl AwaitedAck {
    /// The body and its challenge are written and flushed.
    fn mark_on_wire(&mut self) {
        artifact_reply_write_wire_anchor();
        self.on_wire = true;
    }

    /// The peer echoed the challenge: the guard is released at once, after the
    /// write half is shut (nothing more is sent on this connection).
    fn acknowledged(mut self, stream: &CtlStream) {
        if let Some(mut retention) = self.retention.take() {
            retention.acknowledge_peer_anchor();
            let _ = stream.shutdown(std::net::Shutdown::Write);
            drop(retention);
        }
    }
}

impl Drop for AwaitedAck {
    fn drop(&mut self) {
        let Some(mut retention) = self.retention.take() else {
            return;
        };
        if self.on_wire {
            retention.acknowledge_failed_anchor();
        } else {
            retention.write_failed_anchor();
        }
        quarantine_artifact_reply_until(
            retention,
            std::time::Instant::now() + ARTIFACT_FAILURE_QUARANTINE,
        );
    }
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
/// (2026-09-30). A quarantined reply holds its descriptor chain and its
/// capture lease, so later path captures are refused, or wait out
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

fn quarantine_artifact_reply_until(mut retention: ReplyRetention, until: std::time::Instant) {
    // The handoff's outcome is judged: its place in flight goes back now, and
    // the quarantine keeps the guard and its descriptor chain.
    drop(retention.in_flight.take());
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

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "ArtifactReplyPublication",
        action = "WriteWire",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn artifact_reply_write_wire_anchor() {}

/// The bytes of an artifact reply's acknowledgement nonce (`hex_token`), so
/// its challenge and echo carry twice as many hex digits.
const ARTIFACT_ACK_NONCE_BYTES: usize = 16;

/// The most a peer may send after an artifact's challenge before its first
/// line ends: `ACK <nonce>`, the `\r` every request line may carry, and the
/// line feed. One line decides, so nothing past it is ever judged.
const ACK_LINE_MAX: usize = aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX.len()
    + 2 * ARTIFACT_ACK_NONCE_BYTES
    + "\r\n".len();

impl ControlReply {
    /// This reply as the bytes its connection sends, the handoff that spans
    /// them, and the latch their last byte releases. An artifact's guard is
    /// revalidated (`prepare_write`) before any byte exists, so no OK byte can
    /// certify an identity the barrier then refuses: a failure answers a
    /// path-free ERR instead and the guard is aborted. A guarded body is
    /// followed, in the same buffer, by an unpredictable
    /// `ACK-CHALLENGE <nonce>`. A write-only body carries its permit. Both
    /// handoffs fail at `now + handoff_timeout` if not over by then.
    fn into_wire(
        mut self,
        handoff_timeout: std::time::Duration,
    ) -> (Vec<u8>, Option<PendingHandoff>, Option<ReplyWritten>) {
        let deadline = std::time::Instant::now() + handoff_timeout;
        let handoff = match self.retention.take() {
            None => None,
            Some(ReplyHandoff::WriteOnly(permit)) => Some(PendingHandoff::WriteOnly {
                deadline,
                _permit: permit,
            }),
            // Mint the challenge before fallible wire preparation: video
            // preparation can publish its durable marker, so entropy failure
            // must stay an abortable ERR.
            Some(ReplyHandoff::Acknowledged(mut retention)) => {
                match aterm_uds::rand::hex_token::<ARTIFACT_ACK_NONCE_BYTES>() {
                    Err(error) => {
                        self.body = format!("ERR artifact acknowledgement setup failed: {error}\n");
                        None
                    }
                    Ok(nonce) => match retention.prepare_write() {
                        Err(error) => {
                            retention.prepare_failed_anchor();
                            self.body = format!("ERR artifact reply validation failed: {error}\n");
                            None
                        }
                        Ok(()) => {
                            self.body.push_str(
                                aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX,
                            );
                            self.body.push_str(&nonce);
                            self.body.push('\n');
                            Some(PendingHandoff::Acknowledged(AwaitedAck {
                                retention: Some(retention),
                                nonce,
                                deadline,
                                on_wire: false,
                            }))
                        }
                    },
                }
            }
        };
        (self.body.into_bytes(), handoff, self.written)
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

/// What the bytes a peer `sent` after an artifact's challenge say, once they
/// say anything: `Some(true)` when its first line is exactly `ACK <nonce>` (a
/// trailing `\r` is no part of a line), `Some(false)` for any other first line
/// or for [`ACK_LINE_MAX`] bytes with no line end, `None` while neither. EXACTLY
/// one line decides — the challenge is one-shot, so a line that is not the ACK
/// is a failure and never a re-read: reading on would let a peer keep guessing
/// the nonce inside one deadline. Once the peer has `ended` (EOF), what it sent
/// is its line, as the request grammar reads a last line with no newline.
fn ack_verdict(sent: &[u8], nonce: &str, ended: bool) -> Option<bool> {
    let window = &sent[..sent.len().min(ACK_LINE_MAX)];
    let line = match window.iter().position(|byte| *byte == b'\n') {
        Some(end) => &window[..end],
        None if sent.len() >= ACK_LINE_MAX => return Some(false),
        None if ended => window,
        None => return None,
    };
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    Some(
        line.strip_prefix(aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX.as_bytes())
            == Some(nonce.as_bytes()),
    )
}

/// The acknowledgement line after an artifact's challenge, read waiting —
/// where no parker reads it (`control_lanes.rs`'s fallback, the bridge) —
/// under the handoff's absolute deadline, re-armed before every refill
/// ([`DeadlineRead`]), so a dribbling peer buys nothing. Expiry, EOF and I/O
/// failure are a failed acknowledgement ([`ack_verdict`]).
fn read_ack_until(
    stream: &CtlStream,
    reader: &mut impl BufRead,
    nonce: &str,
    deadline: std::time::Instant,
) -> bool {
    let mut bound = DeadlineRead::new(stream, deadline);
    let mut sent = Vec::with_capacity(ACK_LINE_MAX);
    loop {
        if bound.arm().is_err() {
            return false;
        }
        match reader.fill_buf() {
            Ok([]) => return ack_verdict(&sent, nonce, true) == Some(true),
            Ok(available) => {
                let taken = available.len().min(ACK_LINE_MAX - sent.len());
                sent.extend_from_slice(&available[..taken]);
                reader.consume(taken);
                if let Some(verdict) = ack_verdict(&sent, nonce, false) {
                    return verdict;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

thread_local! {
    /// The connection this worker thread is serving, to which connection-scoped
    /// claims (`meta set supervisor <holder>` without `ttl=`) bind. Set while a
    /// lane carries it (`control_lanes.rs`'s `Serving`) and during the bridge's
    /// [`serve_preresolved`]; a connection keeps its serial across lanes.
    static SERVING_CONNECTION: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// Whether the connection being served made a connection-scoped claim (or
    /// took a drive lease `here`), so only such a connection pays for the
    /// release sweep when it ends.
    static CONNECTION_CLAIMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the verb this thread serves is finishing input it has begun
    /// ([`CommittedInput`]), so its main-thread hops are not refused for a
    /// hangup.
    static INPUT_COMMITTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The address of the fleet peer this thread serves (`serve_fleet`), for
    /// the audit line of a fleet gate refusal ([`audit_fleet_refusal`]).
    #[cfg(any(unix, test))]
    static SERVING_FLEET_PEER: std::cell::Cell<Option<std::net::SocketAddr>> =
        const { std::cell::Cell::new(None) };
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
    /// Who the connection this lane serves is, for the typed-by stamp
    /// ([`serving_caller`]): `Some` while a lane carries the connection
    /// ([`ServingCaller`]), holding its memo once its first typing verb has
    /// read it; `None` off a lane, where each stamp reads it afresh.
    static SERVING_CALLER: std::cell::RefCell<Option<Option<Arc<crate::typed_by::Caller>>>> =
        const { std::cell::RefCell::new(None) };
}

/// The connection's typed-by memo, lent to this lane while it serves the
/// connection ([`SERVING_CALLER`]) and handed back when it lets go.
pub(crate) struct ServingCaller<'a>(&'a mut Option<Arc<crate::typed_by::Caller>>);

impl<'a> ServingCaller<'a> {
    pub(crate) fn enter(memo: &'a mut Option<Arc<crate::typed_by::Caller>>) -> Self {
        SERVING_CALLER.with(|serving| *serving.borrow_mut() = Some(memo.take()));
        Self(memo)
    }
}

impl Drop for ServingCaller<'_> {
    fn drop(&mut self) {
        *self.0 = SERVING_CALLER.with(|serving| serving.borrow_mut().take().flatten());
    }
}

/// Who the connection this thread serves is, as its typing is credited
/// (S9, [`crate::typed_by::Who`]): the bridge; the harness's token; any other
/// owner-token client by its kernel peer pid, read with its lineage; an edge
/// by the client that presented its token, read the same way, on the session
/// the target's table says the edge was minted for (its `src`, which whoever
/// granted it declared, so never the typist on its own word). A connection
/// is read at its first typing verb and kept ([`SERVING_CALLER`]). A fleet
/// drive's admitted line is `<host> (node <8 hex>) over the fleet`
/// ([`crate::typed_by::fleet_credit`]): the host this Mac's book names the
/// driving node, and the node it proved. `None` for a fleet peer, which types
/// nothing, and an edge the target's table no longer names.
fn serving_caller(scope: Scope, ctx: &SessionCtx) -> Option<Arc<crate::typed_by::Caller>> {
    use crate::typed_by::{Caller, Who};
    match scope {
        Scope::Owner => {
            // A drive connection's admitted line: the node it serves for, by
            // the host this Mac's book names it and its id. Never the lane's
            // memo: the drive thread carries no pooled connection's caller.
            if let Some(driver) = crate::fabric::serving_driver() {
                return Some(Caller::new(Who::Word(crate::typed_by::fleet_credit(
                    &driver.host,
                ))));
            }
            Some(kept_caller(
                |kept| !matches!(kept, Who::Edge { .. }),
                || {
                    Caller::new(match serving_principal() {
                        Principal::Harness => Who::Aterm,
                        Principal::Instance => Who::client(serving_peer_pid()),
                    })
                },
            ))
        }
        #[cfg(any(unix, test))]
        Scope::Bridge => Some(Caller::new(Who::Bridge)),
        Scope::Edge(presented) => {
            let src = ctx
                .edges
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .src_of(&presented)?
                .clone();
            let on = src.clone();
            Some(kept_caller(
                move |kept| matches!(kept, Who::Edge { src, .. } if *src == on),
                || {
                    Caller::new(Who::Edge {
                        src,
                        client: Box::new(Who::client(serving_peer_pid())),
                    })
                },
            ))
        }
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => None,
    }
}

/// The connection's caller memo where it `fits`, else `read` once and kept
/// as the memo; off a lane, `read` afresh ([`SERVING_CALLER`]).
fn kept_caller(
    fits: impl Fn(&crate::typed_by::Who) -> bool,
    read: impl FnOnce() -> Arc<crate::typed_by::Caller>,
) -> Arc<crate::typed_by::Caller> {
    SERVING_CALLER.with(|serving| match serving.borrow_mut().as_mut() {
        Some(memo) => match memo {
            Some(kept) if fits(&kept.who) => Arc::clone(kept),
            _ => Arc::clone(memo.insert(read())),
        },
        None => read(),
    })
}

/// A drive record's reply-level description of a typing act. This is not
/// byte evidence and never credits the session's typed-by stamp: the exact
/// event's nonempty accepted completion does that (`control_input_ack`).
#[cfg(any(unix, test))]
fn typed_into(reply: &str) -> bool {
    let Some(first) = reply.lines().next().filter(|first| first.starts_with("OK")) else {
        return false;
    };
    if first.split_whitespace().any(|word| word == "dup=1") {
        return false;
    }
    !first.contains(" skipped")
        || first
            .split(' ')
            .filter_map(|word| word.split_once('='))
            .any(|(key, value)| match key {
                "reason" => value == "guard",
                "typed" | "pasted" => value != "0",
                _ => false,
            })
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
        if !SERVING_DIRECT.with(std::cell::Cell::get) {
            return None;
        }
        // The connection this lane serves, open for as long as its
        // `ServingFd` guard stands (the whole serve).
        let fd = aterm_control::hangup::serving_fd()?;
        aterm_uds::fdpass::peer_pid_of_fd(fd).ok()
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
    proxy: &'a EventLoopProxy<Wake>,
}

#[cfg(unix)]
impl<'a> ConnectionScope<'a> {
    fn enter(store: &'a Store, proxy: &'a EventLoopProxy<Wake>, stream: &CtlStream) -> Self {
        let id = NEXT_CONNECTION.fetch_add(1, Ordering::Relaxed);
        SERVING_CONNECTION.with(|serving| serving.set(Some(id)));
        CONNECTION_CLAIMED.with(|claimed| claimed.set(false));
        Self {
            id,
            _serving: ServingFd::enter(stream),
            store,
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
        release_connection_claims(self.store, Some(self.proxy), self.id);
    }
}

/// Whether ending `conn` would release a live supervisor claim or `here` drive
/// lease. Clone the roster before taking any session lock, as the release sweep
/// does; the parker holds no lane-queue lock while querying it.
#[cfg(unix)]
fn connection_has_live_claim(store: &Store, conn: u64, now: u64) -> bool {
    let handles = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
    handles.into_iter().any(|handle| {
        let supervised = handle
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .live_claim(now)
            .is_some_and(|claim| claim.conn == Some(conn));
        if supervised {
            return true;
        }
        let lease = handle
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        lease.as_ref().is_some_and(|lease| {
            matches!(lease, crate::Lease::Drive { conn: Some(owner), here: true, .. }
                if *owner == conn)
                && lease.is_live(now)
        })
    })
}

#[cfg(all(test, unix))]
mod connection_claim_tests {
    use super::*;

    /// The parker reads the actual session authority, including releases,
    /// renewal to another connection, detached TTLs, and exact expiry.
    #[test]
    fn only_live_connection_bound_claims_spare_their_connection() {
        let store = crate::session_store::new_store();
        let supervisor = crate::session_store::test_handle(1);
        let driver = crate::session_store::test_handle(2);
        store.write().unwrap().register(supervisor.clone());
        store.write().unwrap().register(driver.clone());
        let claimed = |conn, now| connection_has_live_claim(&store, conn, now);
        assert!(!claimed(7, 100));
        assert_eq!(
            crate::session_timeline::claim_supervisor(&supervisor.ctx, "sup", Some(7), None, 100),
            Ok(true)
        );
        assert!(claimed(7, 100));
        assert!(!claimed(8, 100), "another connection owns no claim");
        assert!(crate::session_timeline::release_supervisor(
            &supervisor.ctx,
            Some(7),
            100,
            aterm_slog::Actor::Ctl
        ));
        assert!(!claimed(7, 100), "release makes the connection evictable");
        assert_eq!(
            crate::session_timeline::claim_supervisor(&supervisor.ctx, "sup", None, Some(500), 100),
            Ok(true)
        );
        assert!(
            supervisor
                .ctx
                .meta
                .lock()
                .unwrap()
                .live_claim(499)
                .is_some()
        );
        assert!(
            !claimed(7, 499),
            "a detached live TTL outlives the connection"
        );
        assert!(
            supervisor
                .ctx
                .meta
                .lock()
                .unwrap()
                .live_claim(500)
                .is_none()
        );
        assert!(
            !claimed(7, 500),
            "expired supervisor authority spares no connection"
        );

        // Drive the real acquire/release parser under a connection, retaining
        // the previous thread-local context even if an assertion fails.
        struct Restore(Option<u64>, bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                SERVING_CONNECTION.with(|s| s.set(self.0));
                CONNECTION_CLAIMED.with(|c| c.set(self.1));
            }
        }
        let _restore = Restore(
            SERVING_CONNECTION.with(|s| s.replace(Some(7))),
            CONNECTION_CLAIMED.with(|c| c.replace(false)),
        );
        let lease = |rest| control_session::cmd_lease(&driver.ctx, rest);
        assert!(lease("acquire holder=drive ttl=60000 hard here").starts_with("OK lease acquired"));
        let expiry = match driver.ctx.turn_lease.lock().unwrap().as_ref().unwrap() {
            crate::Lease::Drive { expires_us, .. } => *expires_us,
            other => panic!("expected the acquired drive lease, got {other:?}"),
        };
        assert!(claimed(7, expiry - 1));
        assert!(
            !claimed(8, expiry - 1),
            "another owner does not protect this connection"
        );
        assert!(
            !claimed(7, expiry),
            "the drive lease's expiry is authoritative"
        );
        assert_eq!(lease("release holder=drive here"), "OK lease released\n");
        assert!(!claimed(7, crate::metrics::now_us()));
        assert!(
            CONNECTION_CLAIMED.with(std::cell::Cell::get),
            "Drop still owes cleanup"
        );
        assert!(lease("acquire holder=drive ttl=60000 hard here").starts_with("OK lease acquired"));
        SERVING_CONNECTION.with(|s| s.set(Some(8)));
        assert!(lease("acquire holder=drive ttl=60000 hard").starts_with("OK lease acquired"));
        assert!(!claimed(7, crate::metrics::now_us()));
        assert!(
            !claimed(8, crate::metrics::now_us()),
            "a detached renewal outlives either connection"
        );
    }
}

/// Release every supervisor claim bound to connection `conn`, with `meta
/// unset`'s fan-out (the tab chrome's wake; the release's `meta-change` record
/// wakes the session's watchers), and every drive lease it took `here`
/// ([`control_session::release_connection_lease`], with `lease release`'s). The
/// registry snapshot is cloned first so no meta lock is taken under the
/// registry lock.
fn release_connection_claims(store: &Store, proxy: Option<&EventLoopProxy<Wake>>, conn: u64) {
    let handles = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
    let now = crate::metrics::now_us();
    for handle in handles {
        control_session::release_connection_lease(&handle.ctx, conn);
        // The connection's end lets its claim go: aterm's.
        let by = aterm_slog::Actor::Aterm;
        if !crate::session_timeline::release_supervisor(&handle.ctx, Some(conn), now, by) {
            continue;
        }
        if let Some(proxy) = proxy {
            let _ = proxy.send_event(Wake::MetaChanged {
                session: handle.local_id,
            });
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
    proxy: &EventLoopProxy<Wake>,
    sock_dir: &std::path::Path,
    recovery_count_admission: &crate::keeper_link::RecoveryCountAdmission,
) -> ServeDisposition {
    // Claims bound to this connection end with it, however it ends.
    let _connection = ConnectionScope::enter(store, proxy, stream);
    // The fd is the authority; no token was presented.
    let _principal = ServingPrincipal::enter(Principal::Instance);
    if arm_authenticated_read_poll(stream).is_err() {
        return ServeDisposition::Close;
    }
    let mut unread = control_lanes::Unread::default();
    let mut reader = control_lanes::LaneReader::new(stream, &mut unread);
    let mut out = control_lanes::Outbox::default();
    let mut served = 0usize;
    loop {
        // A line too long for any request is refused, its answer delivered,
        // and the connection closed: nothing past it is read.
        let Some(line) = read_authenticated_request_line(&mut reader, &mut out) else {
            let _ = out.finish_waiting(stream, &mut reader);
            return ServeDisposition::Close;
        };
        let request = Request::parse(line);
        // Delivered by `finish_waiting` below before the next line is read:
        // owed until then ([`FinalReplies`]).
        let _final_reply = FINAL_REPLIES
            .enter(&request.head())
            .or_else(|| owed_input_reply(store, &request));
        let disposition = serve_request_line(
            request,
            scope,
            stream,
            &mut reader,
            &mut out,
            active,
            store,
            proxy,
            sock_dir,
            recovery_count_admission,
        );
        // This thread is the bridge's or a fleet peer's alone: it delivers each
        // answer, and sees a guarded one's acknowledgement through, waiting.
        if out.finish_waiting(stream, &mut reader).is_err() || out.closes() {
            return ServeDisposition::Close;
        }
        if let Some(disposition) = disposition {
            return disposition;
        }
        served += 1;
        if matches!(scope, Scope::Fleet { .. }) && served >= FLEET_REQUESTS {
            return ServeDisposition::Close;
        }
    }
}

/// The most requests one fleet connection is served before it is closed: a
/// fleet read is one `survey` (and a `version`) per connection, and each
/// survey reads every session's locks, so a member that pipelines lines on
/// one slot gets a few answers, then must dial — and prove — again.
#[cfg(unix)]
const FLEET_REQUESTS: usize = 4;

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
    // `read_exact` its raw bytes from the same buffered stream. `None`: hung
    // up, too slow, or a line too long (answered why, as a refused token is).
    let mut writer = stream;
    let first = read_request_line_until(stream, reader, &mut writer, deadline)?;
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
            let _ = writer.write_all(b"ERR auth\n");
            let _ = writer.flush();
            return None;
        }
    };
    // Once authenticated, a connection may idle indefinitely and follows the
    // active tab per request. Idle connections park off their lane
    // (`control_lanes.rs`); otherwise on unix the reader waits for readability
    // or hangup before each refill. The short recv timeout is a backstop, not
    // an idle deadline. Push subscriptions move to their own pool.
    arm_authenticated_read_poll(stream).ok()?;
    Some((scope, principal, inline_verb))
}

/// Whether a request belongs on a wait lane (`control_lanes.rs`): it mostly
/// waits for an event, or hands the connection to a relay. Only decides which
/// lane's time it takes; a waiting form the table leaves on `Lane::Rpc` costs a
/// request lane, and a missed relay holds one for as long as the relay lives. So a
/// line [`proxy_forward_plan`] relays is a wait here: its [`relay_candidate`]
/// test, without its resolution (no graph read or connect per request).
fn is_wait_request(request: &Request, scope: Scope, store: &Store) -> bool {
    use aterm_types::control_verbs::Lane;
    let head = request.head();
    // A session this instance does not host: `@<sid>` (or a `subscribe`'s one
    // session) forwards the connection to the instance that does, and the
    // relay then owns it — except a worker's spawn frame, refused where it is
    // ([`worker_frame_refusal`]). Only an Owner-class connection is relayed;
    // an Edge's line naming another instance's session is refused here, at
    // once.
    if let Some(sid) = relay_candidate(request, scope)
        && spawn_frame_len(&head).is_none()
        && store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_selector(&Selector::Sid(sid))
            .is_none()
    {
        return true;
    }
    match head.verb {
        // `subscribe` carries its selector second. Self, a local id, a list, `@*`
        // or a hosted session flips to push here at once: request-lane work.
        "subscribe" if head.selector.is_none() => false,
        // Every other line costs the lane its table form names (`Lane`): a wait
        // for what parks on an event, a settle, a prompt, the owner, the
        // network or the bridge's landing; a `paste` only when an explicit
        // selector aims it at another session, whose program it waits on.
        _ => match request.form().map(|form| form.lane) {
            Some(Lane::Wait) => true,
            Some(Lane::WaitWhenAimed) => head.selector.is_some_and(|s| !s.is_self()),
            Some(Lane::Rpc) | None => false,
        },
    }
}

/// THE FLEET GATE (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5): the
/// refusal a [`Scope::Fleet`] connection gets for `line`, decided before any
/// other path of [`serve_request_line`] reads it — the sibling
/// proxy, the push flip, every frame. A fleet peer runs only a
/// verb whose row in the one verb table is marked `fleet`
/// (`aterm_types::control_verbs::VerbSpec::fleet`), and with no selector:
/// nothing is forwarded over the sibling hop, so a fleet read reaches this
/// instance and no other. No frame is served, whatever its verb. `None` for every
/// other scope, and for a line the fleet may run.
fn fleet_refusal(request: &Request, scope: Scope) -> Option<String> {
    #[cfg(any(unix, test))]
    if matches!(scope, Scope::Fleet { .. }) {
        let head = request.head();
        let (selector, verb) = (head.selector, head.verb);
        let (refused, reply) = if selector.is_some() {
            (
                "an @ selector",
                "ERR denied: the fleet serves no selector; nothing is forwarded\n".to_string(),
            )
        } else if request.frame().is_some()
            || request.post_frame().is_some()
            || spawn_frame_len(&head).is_some()
        {
            (
                "a frame",
                "ERR denied: the fleet serves no frame\n".to_string(),
            )
        } else if aterm_types::control_verbs::spec(verb).is_some_and(|spec| spec.fleet) {
            return None;
        } else {
            let served: Vec<&str> = aterm_types::control_verbs::VERBS
                .iter()
                .filter(|spec| spec.fleet)
                .map(|spec| spec.name)
                .collect();
            (
                "no fleet verb",
                format!("ERR denied: the fleet serves only {}\n", served.join(", ")),
            )
        };
        audit_fleet_refusal(verb, refused);
        return Some(reply);
    }
    let _ = (request, scope);
    None
}

/// The audit line of one fleet gate refusal ([`fleet_refusal`]): the peer's
/// address and the verb it sent (escaped, its first 32 characters), written
/// through `log_denial` as every other scope's denial is, and thinned by the
/// network run rule ([`control_denials::FLEET_GATE_DENIALS`]): the peer proved
/// the fleet secret, but a member probing verbs it may not run can do so as
/// fast as it can handshake.
#[cfg(any(unix, test))]
fn audit_fleet_refusal(verb: &str, refused: &str) {
    let peer = SERVING_FLEET_PEER
        .with(std::cell::Cell::get)
        .map_or_else(|| "a fleet peer".to_string(), |peer| peer.to_string());
    let verb: String = verb.chars().take(32).collect();
    let line = format!(
        "{peer} sent `{}`: {refused}",
        aterm_log::sanitize_record(&verb)
    );
    if let Some(line) =
        control_denials::FLEET_GATE_DENIALS.record_network(&line, std::time::Instant::now())
    {
        log_denial(
            AUDIT_SUBSYSTEM,
            "fleet",
            aterm_containment::mode_or_containment(),
            &line,
        );
    }
}

/// One write a drive connection's line makes, kept for its record once it
/// is answered ([`note_drive_act`]): the verb, the session it acts on
/// (`None` for a spawn, whose reply names its worker), and a hold's ask.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DriveAct {
    verb: &'static str,
    sid: Option<SessionId>,
    hold: Option<HoldAsk>,
}

/// What a drive's `hold <sid> on|off` asked, and where the session's remote
/// slot stood for the driving node when the line was admitted
/// ([`drive_admission`]): its record names the direction asked, and only a
/// slot that moved that way ([`drive_record`]). The hold verb's `OK hold=1`
/// says only that some slot stands — the person's, the owner's or the
/// bridge's — so it never tells a node's hold from its hand-back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HoldAsk {
    /// `on`, else `off`.
    on: bool,
    /// Whether the driving node held the session's remote slot before.
    held: bool,
}

/// What the drive gate made of one admitted line ([`drive_gate`]).
#[cfg(any(unix, test))]
#[derive(Debug, PartialEq, Eq)]
enum DriveLine {
    /// A fleet verb (`survey`, `version`), served as any fleet peer's line:
    /// the fleet gate judges it.
    Fleet,
    /// An act of the allowlist, served as an Owner line credited to the
    /// node: `line` as it goes on (a spawn's with `raise=false`), and the
    /// write it makes, if it makes one.
    Act {
        line: String,
        write: Option<DriveAct>,
    },
}

/// THE DRIVE GATE OF A DRIVE CONNECTION'S LINE (the step-in,
/// `docs/DESIGN-sessions-as-subagents-2026-10-15.md` §5.1), applied first
/// after the recovery-count gate ([`recovery_count_refusal`]) on that
/// connection: a fleet verb goes on to the fleet gate as any fleet
/// peer's; an act of the allowlist goes on as an Owner line — `scope`
/// becomes [`Scope::Owner`] for it alone, the node named on the thread
/// ([`crate::fabric::DrivingAs`]) for the hold slot and the typed-by stamp —
/// with the write it makes, for its record; anything else is one `ERR
/// denied:` line, audited, and the connection closes. Every other
/// connection's line passes untouched. A drive connection is a
/// [`Scope::Fleet`] whose `drive` is set on a thread that names its node:
/// either alone is a fleet peer.
fn drive_admission(
    request: Request,
    scope: Scope,
    store: &Store,
) -> Result<(Request, Scope, Option<DriveAct>), String> {
    #[cfg(any(unix, test))]
    if matches!(scope, Scope::Fleet { drive: true, .. })
        && let Some(driver) = crate::fabric::serving_driver()
    {
        let hosts = |sid: &SessionId| {
            store
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .by_sid(sid)
                .is_some()
        };
        let spawned = |sid: &SessionId| drive_spawned_by(sid).as_deref() == Some(&*driver.node);
        return match drive_gate(request.line(), &hosts, &spawned) {
            Ok(DriveLine::Fleet) => Ok((request, scope, None)),
            Ok(DriveLine::Act { line, mut write }) => {
                if let Some(DriveAct {
                    sid: Some(sid),
                    hold: Some(ask),
                    ..
                }) = &mut write
                {
                    ask.held = remote_held_by(store, sid, &driver.node);
                }
                // A spawn's line gains `raise=false`: read again as it goes on.
                let request = if line == request.line() {
                    request
                } else {
                    Request::parse(line)
                };
                Ok((request, Scope::Owner, write))
            }
            Err(refused) => {
                audit_fleet_refusal(
                    request.head().verb,
                    &format!("{} driving: {refused}", driver.host),
                );
                Err(format!("ERR denied: {refused}\n"))
            }
        };
    }
    let _ = store;
    Ok((request, scope, None))
}

/// A drive's close of a session this aterm does not know the node spawned:
/// one it did not, or one spawned before this aterm started — a restart or a
/// seamless update forgets which node spawned which worker
/// ([`DRIVE_SPAWNED`]).
#[cfg(any(unix, test))]
const DRIVE_CLOSE_REFUSED: &str = "a drive closes only a worker it spawned since this aterm \
                                   started (an update or a restart forgets which; the person \
                                   here closes it)";

/// [`drive_admission`]'s rule over one line, pure: `hosts` says whether this
/// instance hosts a session, `spawned` whether the driving node spawned it
/// here. `Err` is the refusal's words. Admitted:
/// * `survey`, `version` ([`DriveLine::Fleet`]);
/// * `@<sid> status|text|screen|result`, reads;
/// * `@<sid> turn|key|send`, writes;
/// * `hold <sid> on|off [reason=<pct>]`, with no `origin=`;
/// * `spawn agent=<claude|gemini|agy|muse> [model=<m>] [cwd=<absolute>]
///   [from=<sid>] [raise=false] len=<n>`, a worker's frame, `raise=false` added where it
///   is absent, `from=` naming no session of this instance;
/// * `await task @<sid>[:<rev>][,…] …`, the list form;
/// * `@<sid> close` of a worker this node spawned.
///
/// Every `@<sid>` names a session this instance hosts by its sid: no bare
/// form, `@.`, local id or list, and nothing is forwarded to a sibling. No
/// other frame is served.
#[cfg(any(unix, test))]
fn drive_gate(
    line: &str,
    hosts: &dyn Fn(&SessionId) -> bool,
    spawned: &dyn Fn(&SessionId) -> bool,
) -> Result<DriveLine, String> {
    const SID: &str = "a drive names a session this Mac hosts by its sid (@s-…)";
    let request = Request::parse(line.to_owned());
    if request.frame().is_some() || request.post_frame().is_some() {
        return Err("a drive serves no frame but a worker's task".to_owned());
    }
    let Head {
        selector,
        verb,
        rest,
    } = request.head();
    let hosted = |sid: &str| {
        let sid = SessionId::new(sid);
        (sid.as_str().starts_with("s-") && hosts(&sid)).then_some(sid)
    };
    let act = |verb: &'static str, sid: Option<SessionId>| DriveLine::Act {
        line: line.to_owned(),
        write: Some(DriveAct {
            verb,
            sid,
            hold: None,
        }),
    };
    let read = || DriveLine::Act {
        line: line.to_owned(),
        write: None,
    };
    match selector {
        None => match verb {
            "survey" | "version" => Ok(DriveLine::Fleet),
            "hold" => {
                let mut words = rest.split_whitespace();
                let (Some(sid), Some(way @ ("on" | "off"))) = (words.next(), words.next()) else {
                    return Err("hold <sid> on|off [reason=<pct>]".to_owned());
                };
                if !words.all(|word| word.starts_with("reason=")) {
                    return Err("a drive's hold names no origin: hold <sid> on|off \
                                [reason=<pct>]"
                        .to_owned());
                }
                let sid = hosted(sid).ok_or_else(|| SID.to_owned())?;
                Ok(DriveLine::Act {
                    line: line.to_owned(),
                    write: Some(DriveAct {
                        verb: "hold",
                        sid: Some(sid),
                        hold: Some(HoldAsk {
                            on: way == "on",
                            held: false,
                        }),
                    }),
                })
            }
            "spawn" => drive_spawn(line, rest, &hosts).map(|line| DriveLine::Act {
                line,
                write: Some(DriveAct {
                    verb: "spawn",
                    sid: None,
                    hold: None,
                }),
            }),
            "await" => {
                let Some(words) = await_task_list(rest) else {
                    return Err("a drive awaits only `await task @<sid>,…`".to_owned());
                };
                let args = crate::task_record::parse_await_task(&words, true)
                    .map_err(|usage| usage.trim_end().trim_start_matches("ERR ").to_owned())?;
                for target in &args.targets {
                    if hosted(&target.selector).is_none() {
                        return Err(SID.to_owned());
                    }
                }
                Ok(read())
            }
            _ => Err(format!(
                "a drive serves survey, version, hold, spawn agent=<word> and await task, \
                 and @<sid> status|text|screen|result|turn|key|send|close; not `{}`",
                verb.chars().take(32).collect::<String>()
            )),
        },
        Some(Selector::Sid(sid)) => {
            let Some(sid) = hosted(sid.as_str()) else {
                return Err(SID.to_owned());
            };
            match verb {
                "status" | "text" | "screen" | "result" => Ok(read()),
                "turn" => drive_turn(rest).map(|()| act("turn", Some(sid))),
                "key" => Ok(act("key", Some(sid))),
                "send" => Ok(act("send", Some(sid))),
                "close" if rest.trim().is_empty() && spawned(&sid) => Ok(act("close", Some(sid))),
                "close" => Err(DRIVE_CLOSE_REFUSED.to_owned()),
                _ => Err(format!(
                    "a drive serves @<sid> status|text|screen|result|turn|key|send|close; not \
                     `{}`",
                    verb.chars().take(32).collect::<String>()
                )),
            }
        }
        Some(_) => Err(SID.to_owned()),
    }
}

/// Whether a drive's `turn` goes on ([`drive_gate`]): one whose budget —
/// `timeout=` as the turn's own grammar reads it past the dispatch's leading
/// options, its minutes-long default where it names none — is at most
/// [`aterm_types::control_verbs::DRIVE_TURN_MAX_MS`]. The drive connection
/// is cut at 30 s, and a turn types on past that for nobody, holding the
/// tab's lease. A line the grammar refuses goes on: the dispatch refuses it
/// before any byte.
#[cfg(any(unix, test))]
fn drive_turn(rest: &str) -> Result<(), String> {
    use aterm_types::control_verbs::DRIVE_TURN_MAX_MS;
    let (_, rest) = control_input::take_leading_options(input_of("turn"), rest);
    match control_session::turn_options(rest) {
        Ok(turn) if turn.timeout_ms > DRIVE_TURN_MAX_MS => Err(format!(
            "a drive's turn names a timeout= of at most {DRIVE_TURN_MAX_MS} ms (its \
             connection lasts 30 s): say settle=submitted and wait with `await task \
             @<w>:<task_rev>`"
        )),
        _ => Ok(()),
    }
}

/// A drive's `spawn` line as it goes on, or why it is refused
/// ([`drive_gate`]): a worker's frame alone — `agent=<word>` of a worker
/// (`aterm_agent::harness::worker::WORKERS`), `len=<n>`, and
/// of the rest only `model=`, an absolute `cwd=`, a `from=` naming no session
/// of this instance (`hosts`) and `raise=false`, which is added where it is
/// absent: another Mac's worker never takes this Mac's front window.
#[cfg(any(unix, test))]
fn drive_spawn(
    line: &str,
    rest: &str,
    hosts: &dyn Fn(&SessionId) -> bool,
) -> Result<String, String> {
    const USAGE: &str = "a drive spawns only a worker: spawn agent=<claude|gemini|agy|muse> \
                         [model=<m>] [cwd=<absolute>] [from=<sid>] [raise=false] len=<n>";
    let tokens = control_media::split_quoted_tokens(rest).ok_or(USAGE)?;
    let (mut agent, mut framed, mut raise) = (false, false, false);
    for token in &tokens {
        let (key, value) = token.split_once('=').ok_or(USAGE)?;
        match key {
            "agent"
                if aterm_agent::harness::worker::WORKERS
                    .iter()
                    .any(|w| w.word == value) =>
            {
                agent = true;
            }
            "len" => framed = true,
            "model" => {}
            "cwd" if std::path::Path::new(value).is_absolute() => {}
            "cwd" => return Err("a drive's spawn names an absolute cwd=".to_owned()),
            "from" if hosts(&SessionId::new(value)) => {
                return Err(
                    "a drive's from= is its own session, never one of this Mac's".to_owned(),
                );
            }
            "from" => {}
            "raise" if value == "false" => raise = true,
            "raise" => return Err("a drive's worker opens raise=false".to_owned()),
            _ => return Err(USAGE.to_owned()),
        }
    }
    if !(agent && framed) {
        return Err(USAGE.to_owned());
    }
    Ok(if raise {
        line.to_owned()
    } else {
        format!("{} raise=false", line.trim_end_matches('\r'))
    })
}

/// Whether the node `node` holds the remote slot of `sid`, a session this
/// instance hosts.
#[cfg(any(unix, test))]
fn remote_held_by(store: &Store, sid: &SessionId, node: &str) -> bool {
    store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_sid(sid)
        .is_some_and(|handle| {
            handle
                .ctx
                .fabric
                .holds()
                .remote()
                .is_some_and(|hold| hold.node == node)
        })
}

/// The workers a drive spawned here since this aterm started, newest last,
/// each with the node that spawned it: what `@<sid> close` over a drive may
/// close. At most [`DRIVE_SPAWNED_MAX`]; an older one, and every one after a
/// restart, is no longer the node's to close. THE LIMIT: it lives in this
/// process alone — no task record, session meta or handoff carries it — so a
/// seamless update, which keeps the workers and their sids, starts the
/// successor with none, and from then on the node that spawned them cannot
/// close them over the fleet (the refusal says so); the person closes them
/// here.
#[cfg(any(unix, test))]
static DRIVE_SPAWNED: Mutex<VecDeque<(SessionId, String)>> = Mutex::new(VecDeque::new());

/// The most workers [`DRIVE_SPAWNED`] keeps.
#[cfg(any(unix, test))]
const DRIVE_SPAWNED_MAX: usize = 256;

/// The node that spawned `sid` over a drive, if one did ([`DRIVE_SPAWNED`]).
#[cfg(any(unix, test))]
fn drive_spawned_by(sid: &SessionId) -> Option<String> {
    DRIVE_SPAWNED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .find(|(spawned, _)| spawned == sid)
        .map(|(_, node)| node.clone())
}

/// THE WORDS OF A DRIVE'S RECORD, once its line is answered `reply`, or
/// `None` for no record — pure: a typing verb that typed ([`typed_into`]); a
/// hold whose remote slot moved the way it asked (`holds_now`: whether the
/// driving node holds it after the line), named by that direction — never by
/// the reply's `hold=`, which counts every slot; a spawn or a close that
/// answered `OK`. A refusal records nothing.
#[cfg(any(unix, test))]
fn drive_record(act: &DriveAct, reply: &str, holds_now: bool) -> Option<&'static str> {
    let ok = reply.starts_with("OK");
    match (act.verb, act.hold) {
        ("turn", _) if typed_into(reply) => Some("turn"),
        ("key", _) if typed_into(reply) => Some("key"),
        ("send", _) if typed_into(reply) => Some("send"),
        ("hold", Some(HoldAsk { on: true, held })) if ok && !held && holds_now => {
            Some("took (hold on)")
        }
        ("hold", Some(HoldAsk { on: false, held })) if ok && held && !holds_now => {
            Some("handed back (hold off)")
        }
        ("spawn", _) if ok => Some("started a worker in"),
        ("close", _) if ok => Some("closed"),
        _ => None,
    }
}

/// THE RECORD OF A DRIVE'S WRITE, once it is answered `reply`
/// ([`drive_record`]): a spawn's worker is the node's to close from now
/// ([`DRIVE_SPAWNED`]) and a close takes it off; each record is a row in this
/// Mac's Messages log naming the host, the act and the tab
/// (`Wake::Driven`). `store` says where a hold's remote slot stands after
/// the line.
#[cfg(any(unix, test))]
fn note_drive_act(act: &DriveAct, reply: &str, store: &Store) {
    let Some(driver) = crate::fabric::serving_driver() else {
        return;
    };
    let holds_now = act
        .sid
        .as_ref()
        .filter(|_| act.hold.is_some())
        .is_some_and(|sid| remote_held_by(store, sid, &driver.node));
    let Some(words) = drive_record(act, reply, holds_now) else {
        return;
    };
    let sid = match act.verb {
        "spawn" => {
            let worker = reply
                .split_whitespace()
                .nth(1)
                .filter(|sid| sid.starts_with("s-"))
                .map(SessionId::new);
            if let Some(worker) = &worker {
                let mut spawned = DRIVE_SPAWNED.lock().unwrap_or_else(|p| p.into_inner());
                if spawned.len() >= DRIVE_SPAWNED_MAX {
                    spawned.pop_front();
                }
                spawned.push_back((worker.clone(), driver.node.clone()));
            }
            worker
        }
        "close" => {
            if let Some(sid) = &act.sid {
                DRIVE_SPAWNED
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .retain(|(spawned, _)| spawned != sid);
            }
            act.sid.clone()
        }
        _ => act.sid.clone(),
    };
    if let Some(sid) = sid {
        crate::presence::post_driven(&sid, &driver.host, words);
    }
}

/// The one line every request, and every frame's header, is refused with
/// while an old journal the keeper may still offer has no accepted successor
/// ([`crate::keeper_link::RecoveryCountAdmission`]); `None` once it has.
fn recovery_count_refusal(
    admission: &crate::keeper_link::RecoveryCountAdmission,
) -> Option<&'static str> {
    (!admission.ready()).then_some("ERR recovery-count-custody unconfirmed\n")
}

/// Handle one authenticated request, folded into the `TOKEN` line or on its own
/// line: the recovery-count gate ([`recovery_count_refusal`]), the drive and
/// fleet gates, the empty-selector gate, the askpass relay,
/// proxy forward, subscribe flip, binary frame, then dispatch, its answer queued
/// in `out` for the connection to deliver. `None` means keep polling;
/// `Some(disposition)` ends this lane once `out` is delivered.
#[allow(clippy::too_many_arguments)]
fn serve_request_line(
    request: Request,
    scope: Scope,
    stream: &CtlStream,
    reader: &mut control_lanes::LaneReader<'_>,
    out: &mut control_lanes::Outbox,
    active: &ActiveHandle,
    store: &Store,
    proxy: &EventLoopProxy<Wake>,
    sock_dir: &std::path::Path,
    recovery_count_admission: &crate::keeper_link::RecoveryCountAdmission,
) -> Option<ServeDisposition> {
    // Before drive attribution, forwarding, subscribe, askpass, frames or dispatch:
    // no ID or history/since anchor can escape while an old journal is offerable.
    if let Some(refusal) = recovery_count_refusal(recovery_count_admission) {
        out.extend_from_slice(refusal.as_bytes());
        return Some(ServeDisposition::Close);
    }
    // THE DRIVE GATE, first after the recovery-count gate on a drive
    // connection: an admitted act goes on as an Owner line; a refused line
    // reaches no path below. One ERR line, then the connection closes.
    let (request, scope, driven) = match drive_admission(request, scope, store) {
        Ok(admitted) => admitted,
        Err(refusal) => {
            out.push_owned(refusal.into_bytes());
            return Some(ServeDisposition::Close);
        }
    };
    // THE FLEET GATE: a fleet peer's refused line reaches no path below.
    // One ERR line, then the connection closes.
    if let Some(refusal) = fleet_refusal(&request, scope) {
        out.push_owned(refusal.into_bytes());
        return Some(ServeDisposition::Close);
    }
    // THE EMPTY-SELECTOR GATE: a leading `@` that names nothing reaches no
    // path below. The connection closes after the one ERR line, as a frame's
    // body (`@ feed-bin <n>`, `@ spawn … len=<n>`) may follow it unread.
    if let Some(refusal) = empty_selector_refusal(&request) {
        out.push_owned(refusal.into_bytes());
        return Some(ServeDisposition::Close);
    }
    let head = request.head();
    // The `sudo` askpass relay. Intercepted here, the one layer holding the
    // stream, so it reads the peer once and gates on `Scope::Owner` EXACTLY.
    // Its (secret-bearing) reply goes to the outbox in its own guarded buffer,
    // moved, never copied, scrubbed as it drops, and never logged. A selector
    // is refused: the request names no session (the server attributes it by
    // ancestry).
    if head.verb == "askpass" {
        let response = if head.selector.is_some() {
            crate::control_askpass::public_reply("ERR askpass refused takes no @<sid>\n")
        } else {
            crate::control_askpass::serve(stream, scope, proxy, store)
        };
        out.push_secret(response);
        return None;
    }
    // A worker's spawn frame aimed at a session another process hosts is
    // never relayed ([`worker_frame_refusal`]); its task is unsent.
    if let Some(refusal) = worker_frame_refusal(&head, store) {
        out.push_owned(refusal.into_bytes());
        return Some(ServeDisposition::Close);
    }
    // Cross-process forward: relay a `@<sid>` another instance hosts to that
    // instance's socket; the relay then owns the connection.
    if try_proxy_forward(&request, scope, store, sock_dir, stream, reader) {
        return Some(ServeDisposition::Close);
    }
    // PUSH FLIP: `subscribe` authorizes its targets EXACTLY like a read verb,
    // then this connection becomes push-only (never reads another line). On an
    // auth/parse failure `run_subscribe_socket` writes a single `ERR ...` and returns,
    // and we close the connection (a half-subscribed connection is meaningless).
    // Transferred to the reserved push pool so a quiet subscriber never consumes
    // an ordinary RPC worker.
    if head.verb == "subscribe" && head.selector.is_none() {
        return Some(ServeDisposition::Subscribe { request, scope });
    }
    // `post … len=<n>` reads the next N raw bytes as the message body. A valid
    // length is always consumed (even on refusal) to keep framing; a length the
    // server will not read closes the connection.
    if let Some(len) = request.post_frame() {
        if !run_post_bin(&request, len, reader, active, store, scope, out) {
            return Some(ServeDisposition::Close);
        }
        return None;
    }
    // `feed-bin`/`paste-bin <n>` read N raw bytes from the same stream into the
    // target's PTY (`feed-bin` raw, `paste-bin` with paste semantics), authorized
    // like `feed` inside `run_feed_bin`.
    if let Some(frame) = request.frame() {
        let mut dispatch_front_input =
            |event, session, completion: Option<crate::control_input_ack::InputCompletion>| {
                post_input_reply_to_observed(
                    proxy,
                    vec![event],
                    session,
                    completion.map(|c| c.into_mark()),
                )
            };
        if !run_feed_bin_routed_observed(
            &request,
            frame,
            reader,
            FeedBinRoute {
                active,
                store,
                scope,
            },
            &mut dispatch_front_input,
            out,
        ) {
            return Some(ServeDisposition::Close);
        }
        return None;
    }
    // A worker's `spawn … len=<n>` reads the next N raw bytes as its task,
    // then is dispatched as every `spawn` is, the task carried beside the line
    // on this thread for that one dispatch (`task_launch::CarriedTask`). As for
    // `post`, a valid length is always consumed (even on refusal) to keep the
    // framing, and one the server will not read is answered and closes. The
    // client sends the task only once its line is answered `GO <n>`
    // ([`read_worker_task`]).
    let task = match spawn_frame_len(&head) {
        None => None,
        Some(Err(reply)) => {
            out.push_owned(reply.into_bytes());
            return Some(ServeDisposition::Close);
        }
        Some(Ok(n)) => match read_worker_task(stream, reader, n) {
            Ok(body) => Some(body),
            Err(_) => return Some(ServeDisposition::Close),
        },
    };
    let _task = crate::task_launch::CarriedTask::enter(task);
    let resp = dispatch_request(
        &request,
        &DispatchCx {
            active,
            store,
            scope,
            proxy,
            sock_dir,
        },
    );
    #[cfg(any(unix, test))]
    if let Some(act) = &driven {
        note_drive_act(act, &resp.body, store);
    }
    #[cfg(not(any(unix, test)))]
    let _ = driven;
    // The outbox carries the answer and any artifact handoff it holds: the
    // lane delivers it, never waiting on the peer, and the parker takes the
    // rest — the guarded reply's acknowledgement included.
    out.attach(resp);
    None
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
/// question).
///
/// [`InputReplies`] counts the other replies an end must not cut: a verb that
/// writes into a session's input. Its bytes reach the program before its reply
/// is delivered, so an end in between leaves the caller an EOF for input that
/// landed, and a retry would type it twice. The exit waits for those too, and
/// an update's Commit, whose `_exit` is this process's end, writes only while
/// none is owed (`app_update_handoff::fence_handoff_egress`). Any other reply in
/// flight at an end is not waited for.
pub(super) struct FinalReplies {
    owed: Mutex<usize>,
    delivered: Condvar,
    /// Which requests this count owes: `(verb, rest)` of the request's head.
    owes: fn(&str, &str) -> bool,
}

/// The instance's [`FinalReplies`]: the requests whose own action can end it.
pub(super) static FINAL_REPLIES: FinalReplies = FinalReplies::new();

/// Whether a request's own action can end the process.
fn may_end(verb: &str, rest: &str) -> bool {
    match verb {
        "close" | "invoke" | "confirm" => true,
        "tab" => rest.split_whitespace().next() == Some("close"),
        _ => false,
    }
}

/// Whether a request writes into a session's input and answers once it has: not
/// `turn`, whose answer waits for the program's turn to end.
fn writes_input(verb: &str, _rest: &str) -> bool {
    matches!(
        verb,
        "send" | "key" | "hwkey" | "ctrl" | "paste" | "paste-bin" | "feed" | "feed-bin" | "mouse"
    )
}

/// The replies owed for input written into one registry's sessions
/// ([`FinalReplies`] has why): the count lives in the registry
/// ([`crate::session_store::SessionStore::input_replies`]), so the App that
/// commits an update or exits reads its own control server's requests, and a
/// test's App never reads another's.
#[derive(Clone)]
pub(crate) struct InputReplies(Arc<FinalReplies>);

impl Default for InputReplies {
    fn default() -> Self {
        Self(Arc::new(FinalReplies::counting(writes_input)))
    }
}

/// Exclusive admission at a handoff's final Commit. A new input request cannot
/// enter its dispatch while this guard holds the existing reply-count mutex.
/// Dropping it after a failed Commit reopens admission.
#[cfg(any(unix, test))]
pub(crate) struct InputRepliesFence<'a> {
    _owed: std::sync::MutexGuard<'a, usize>,
}

impl InputReplies {
    /// [`FinalReplies::enter`] on this count, for a request read by the request
    /// grammar ([`Request::head`]). A final Commit holds admission closed through
    /// its write, so a later request waits before dispatch.
    pub(crate) fn enter_head(&self, head: &Head<'_>) -> Option<FinalReply> {
        self.0.admit(head).then(|| FinalReply {
            replies: Counted::Registry(Arc::clone(&self.0)),
        })
    }

    /// [`Self::enter_head`] of a request line: the tests' door.
    #[cfg(test)]
    pub(crate) fn enter(&self, line: &str) -> Option<FinalReply> {
        self.enter_head(&control_request::head(line))
    }

    /// Whether a reply is still owed: an update's Commit does not `_exit` while
    /// one is.
    #[cfg(any(unix, test))]
    pub(crate) fn owed(&self) -> bool {
        self.0.owed_now() > 0
    }

    /// Close admission only while no input reply is owed. Unlike the drain
    /// gate's snapshot (`owed`), this guard keeps new requests before dispatch
    /// until the Commit fails or the process exits. Never parks the App.
    #[cfg(any(unix, test))]
    pub(crate) fn try_fence(&self) -> Option<InputRepliesFence<'_>> {
        let owed = match self.0.owed.try_lock() {
            Ok(owed) => owed,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        if *owed == 0 {
            Some(InputRepliesFence { _owed: owed })
        } else {
            None
        }
    }

    /// [`FinalReplies::settle`] on this count.
    pub(crate) fn settle(&self, bound: std::time::Duration) -> bool {
        self.0.settle(bound)
    }
}

/// The reply `request` owes for input it writes into one of `store`'s sessions
/// ([`InputReplies`]), counted from before its dispatch. Classified before any
/// lock, so a request that writes no input (`version`, the forward's connect
/// probe, say) never waits on the store. Not counted when its `@<sid>` names a
/// session this process does not host: that line is relayed to the process that
/// does ([`proxy_forward_plan`]), which counts its own reply, or refused here,
/// and a relay keeps its connection for the connection's whole life, so a count
/// held across it would stand every update Commit down until the client hangs
/// up.
fn owed_input_reply(store: &Store, request: &Request) -> Option<FinalReply> {
    let head = request.head();
    if !writes_input(head.verb, head.rest) {
        return None;
    }
    // The final Commit may hold admission closed. Never wait on that mutex
    // while holding the store: the Commit's remaining work may use it too.
    let replies = {
        let store = store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(selector @ Selector::Sid(_)) = head.selector
            && store.by_selector(&selector).is_none()
        {
            return None;
        }
        store.input_replies()
    };
    replies.enter_head(&head)
}

/// `store`'s [`InputReplies`].
pub(crate) fn input_replies_of(store: &Store) -> InputReplies {
    store
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .input_replies()
}

impl FinalReplies {
    pub(super) const fn new() -> Self {
        Self::counting(may_end)
    }

    const fn counting(owes: fn(&str, &str) -> bool) -> Self {
        Self {
            owed: Mutex::new(0),
            delivered: Condvar::new(),
            owes,
        }
    }

    /// Count a request's reply as owed when this count owes its verb (`head`,
    /// the request grammar's reading of it); `None` otherwise. Taken BEFORE the
    /// dispatch (the action may end the process), dropped once the reply is
    /// delivered or its connection is gone.
    pub(super) fn enter(&'static self, head: &Head<'_>) -> Option<FinalReply> {
        self.admit(head).then(|| FinalReply {
            replies: Counted::Instance(self),
        })
    }

    /// Count a request's reply when this count owes its verb: whether it did.
    fn admit(&self, head: &Head<'_>) -> bool {
        let owes = (self.owes)(head.verb, head.rest);
        if owes {
            *self
                .owed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        }
        owes
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

    /// How many replies are owed now.
    #[cfg(any(unix, test))]
    fn owed_now(&self) -> usize {
        *self
            .owed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    pub(super) fn owed(&self) -> usize {
        self.owed_now()
    }
}

/// One owed reply ([`FinalReplies`]): held from before its request's dispatch
/// until the reply is delivered (the lane's `Outbox`, or a direct writer's flush).
pub(crate) struct FinalReply {
    replies: Counted,
}

/// Where a [`FinalReply`] is counted.
enum Counted {
    /// The instance's [`FINAL_REPLIES`] (or a test's own static).
    Instance(&'static FinalReplies),
    /// A registry's [`InputReplies`].
    Registry(Arc<FinalReplies>),
}

impl Drop for FinalReply {
    fn drop(&mut self) {
        let replies = match &self.replies {
            Counted::Instance(replies) => replies,
            Counted::Registry(replies) => &**replies,
        };
        let mut owed = replies
            .owed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *owed = owed.saturating_sub(1);
        if *owed == 0 {
            replies.delivered.notify_all();
        }
    }
}

/// Wait, at most `bound`, until every [`FinalReply`] is delivered: the exit seam's
/// call, `true` when none is owed any more. A lane still inside such a request
/// finishes quickly once the event loop is gone (a main-thread hop finds its reply
/// sender dropped), so the bound only guards a client that stopped reading.
pub(crate) fn settle_final_replies(bound: std::time::Duration, input: &InputReplies) -> bool {
    let started = std::time::Instant::now();
    FINAL_REPLIES.settle(bound) && input.settle(bound.saturating_sub(started.elapsed()))
}

#[cfg(test)]
mod final_reply_tests {
    use super::FinalReplies;
    use aterm_types::control_request::head;
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
            let guard = REPLIES.enter(&head(line));
            assert!(guard.is_some(), "{line:?} can end the process");
            assert_eq!(REPLIES.owed(), 1, "{line:?}");
            drop(guard);
            assert_eq!(REPLIES.owed(), 0, "{line:?} delivered");
        }
        let close = REPLIES.enter(&head("close")).expect("close is owed");
        assert_eq!(REPLIES.owed(), 1);
        for line in [
            "version",
            "ls",
            "tab new",
            "tab 0",
            "text",
            "closed",
            "await seq 3",
        ] {
            assert!(REPLIES.enter(&head(line)).is_none(), "{line:?}");
            assert_eq!(REPLIES.owed(), 1, "{line:?} cannot deliver close's reply");
        }
        drop(close);
        assert_eq!(REPLIES.owed(), 0, "only close's guard delivers its reply");
    }

    /// The exit seam's wait returns once the owed reply is delivered, and gives up
    /// at its bound while one never is: an exit is delayed, never held.
    #[test]
    fn the_exit_waits_for_an_owed_reply_and_only_to_its_bound() {
        static REPLIES: FinalReplies = FinalReplies::new();
        assert!(REPLIES.settle(Duration::ZERO), "nothing owed: no wait");
        let guard = REPLIES.enter(&head("close")).expect("close is owed");
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

    /// A verb that writes into a session's input owes its reply until it is
    /// delivered, in its own registry's count only (one App's Commit never reads
    /// another's requests). NEGATIVE CONTROL: a read, `turn` (whose answer waits
    /// for the program's turn) and the verbs that can end the process
    /// ([`super::FINAL_REPLIES`]'s) are not counted here.
    #[test]
    fn an_input_verbs_reply_is_owed_until_delivered_in_its_registry() {
        let replies = super::InputReplies::default();
        let other = super::InputReplies::default();
        for line in [
            "send ls",
            "@s-1 key enter",
            "hwkey a",
            "ctrl c",
            "paste hi",
            "paste-bin 3",
            "feed 6869",
            "@s-1 feed-bin 3",
            "mouse click left 1 1",
        ] {
            let guard = replies.enter(line);
            assert!(guard.is_some(), "{line:?} writes input");
            assert!(replies.owed(), "{line:?}");
            assert!(
                replies.try_fence().is_none(),
                "{line:?}: its reply precedes Commit"
            );
            assert!(!other.owed(), "{line:?}: another registry owes nothing");
            drop(guard);
            assert!(!replies.owed(), "{line:?} delivered");
        }
        for line in ["text", "turn hi", "close", "tab close", "ls", "keys"] {
            assert!(replies.enter(line).is_none(), "{line:?}");
        }
        assert!(!replies.owed());

        let fence = replies.try_fence().expect("nothing owed: admission closes");
        assert!(
            replies.try_fence().is_none(),
            "one exclusive Commit admission"
        );
        assert!(
            replies.enter("text").is_none(),
            "a read takes no input admission"
        );
        let elsewhere = other.enter("send hi").expect("another registry stays open");
        drop(elsewhere);
        drop(fence); // A failed Commit lets a later request start.
        let later = replies.enter("send hi").expect("admission reopened");
        assert!(
            replies.try_fence().is_none(),
            "the later request now precedes Commit"
        );
        drop(later);
        assert!(
            replies.try_fence().is_some(),
            "delivered: Commit may fence again"
        );
    }

    /// THE LANE OWES AN INPUT VERB'S REPLY FROM BEFORE ITS DISPATCH UNTIL IT IS
    /// DELIVERED (`LaneService::owed_input_reply`): the dispatch of `send` reads
    /// its reply owed, and once the client has it nothing is. NEGATIVE CONTROL: a
    /// read (`text`) is never owed.
    #[test]
    #[cfg(unix)]
    fn a_lane_owes_an_input_reply_until_it_is_delivered() {
        use super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes, Outbox};
        use std::io::{BufRead as _, Write as _};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct Service {
            replies: super::InputReplies,
            seen: Mutex<Vec<(String, bool)>>,
        }
        impl LaneService for Service {
            fn owed_input_reply(&self, request: &super::Request) -> Option<super::FinalReply> {
                self.replies.enter_head(&request.head())
            }
            fn authenticate(
                &self,
                _stream: &aterm_uds::CtlStream,
                reader: &mut LaneReader<'_>,
            ) -> Option<(super::Scope, super::Principal, Option<String>)> {
                (super::read_request_line(reader)? == "AUTH ok").then_some((
                    super::Scope::Owner,
                    super::Principal::Instance,
                    None,
                ))
            }
            fn serve_line(
                &self,
                request: super::Request,
                _scope: super::Scope,
                _stream: &aterm_uds::CtlStream,
                _reader: &mut LaneReader<'_>,
                out: &mut Outbox,
            ) -> Option<super::ServeDisposition> {
                let line = request.line().to_string();
                out.extend_from_slice(format!("OK {line}\n").as_bytes());
                let owed = self.replies.owed();
                self.seen
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((line, owed));
                None
            }
            fn is_wait(&self, _request: &super::Request, _scope: super::Scope) -> bool {
                false
            }
            fn subscribe(
                &self,
                _request: super::Request,
                _scope: super::Scope,
                _stream: aterm_uds::CtlStream,
            ) {
            }
            fn release(&self, _id: u64) {}
            fn has_live_claim(&self, _id: u64) -> bool {
                false
            }
        }

        let lanes = Lanes::new(LaneLimits {
            rpc: 1,
            wait: 1,
            inline: 1,
            open: 4,
            subscriptions: 1,
        });
        let _ = lanes.start();
        let service = Arc::new(Service::default());
        lanes.publish(service.clone());
        let (client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
        assert!(lanes.admit(server).is_ok(), "admitted");
        client
            .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("hang detector");
        (&client)
            .write_all(b"AUTH ok\nsend ls\ntext\n")
            .expect("the requests");
        let mut reader = std::io::BufReader::new(&client);
        for expected in ["OK send ls\n", "OK text\n"] {
            let mut reply = String::new();
            reader.read_line(&mut reply).expect("a reply");
            assert_eq!(reply, expected);
        }
        assert!(
            service.replies.settle(aterm_spec::wait::MUST_HAPPEN),
            "delivered: nothing is owed"
        );
        assert_eq!(
            *service
                .seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            [("send ls".to_string(), true), ("text".to_string(), false)]
        );
    }

    /// A RELAYED INPUT REQUEST OWES NO REPLY HERE, AND A REQUEST THAT WRITES NO
    /// INPUT TAKES NO LOCK (`owed_input_reply`). A line whose `@<sid>` names a
    /// session this process does not host is relayed for its connection's whole
    /// life (`try_proxy_forward`), so a count held across it would stand every
    /// update Commit here down until its client hung up; the host counts it.
    /// CONTROL: the same verbs with no selector, `@.` or a hosted sid are
    /// counted. RED with the hosted check removed (the relayed lines are
    /// counted), and with the store locked before the verb is classified (the
    /// `version` probe waits behind the held write lock).
    #[test]
    fn a_relayed_input_request_owes_no_reply_here_and_a_read_takes_no_lock() {
        use std::sync::{Arc, mpsc};
        let store = crate::session_store::new_store();
        let hosted = crate::session_store::test_handle(5);
        let hosted_sid = hosted.sid.clone();
        store
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register(hosted);
        let replies = super::input_replies_of(&store);
        let elsewhere = aterm_session::SessionId::generate();
        for (line, counted) in [
            ("send ls".to_string(), true),
            ("@. key enter".to_string(), true),
            (format!("@{} send ls", hosted_sid.as_str()), true),
            (format!("@{} send ls", elsewhere.as_str()), false),
            (format!("@{} paste hi", elsewhere.as_str()), false),
        ] {
            let guard = super::owed_input_reply(&store, &super::Request::parse(line.clone()));
            assert_eq!(guard.is_some(), counted, "{line:?}");
            assert_eq!(replies.owed(), counted, "{line:?}");
            drop(guard);
            assert!(!replies.owed(), "{line:?} delivered");
        }

        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let holder = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let _write = store
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let _ = held_tx.send(());
                let _ = release_rx.recv();
            })
        };
        held_rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the store's write lock is held");
        let (answer_tx, answer_rx) = mpsc::channel();
        let asker = {
            let store = Arc::clone(&store);
            std::thread::spawn(move || {
                let version = super::Request::parse("version".to_string());
                let _ = answer_tx.send(super::owed_input_reply(&store, &version).is_none());
            })
        };
        let answered = answer_rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
        let _ = release_tx.send(());
        holder.join().expect("holder thread");
        asker.join().expect("asker thread");
        assert_eq!(
            answered,
            Ok(true),
            "`version` was answered while the store's write lock was held"
        );
    }
}

/// Read one request line, stripping `\n` (and `\r`). `None` on EOF, a read
/// error, or a line longer than [`MAX_REQUEST_LINE`] (its refusal discarded).
/// Test-only: a socket's first line is read under a deadline
/// ([`read_request_line_until`]).
#[cfg(test)]
fn read_request_line(reader: &mut impl BufRead) -> Option<String> {
    read_request_line_with_policy(reader, &mut std::io::sink(), false, || Ok(()))
}

/// Read one line under one monotonic deadline, re-armed before every buffer
/// refill ([`DeadlineRead`]), so a peer cannot hold a lane by dribbling bytes:
/// the handshake's first line. An over-long line's refusal goes to `refusal`.
fn read_request_line_until(
    stream: &CtlStream,
    reader: &mut impl BufRead,
    refusal: &mut impl Write,
    deadline: std::time::Instant,
) -> Option<String> {
    let mut bound = DeadlineRead::new(stream, deadline);
    read_request_line_with_policy(reader, refusal, false, || bound.arm())
}

/// The time left until `deadline` for one blocking socket call (at least a
/// millisecond: a zero timeout means none), or `TimedOut` once it has passed.
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

/// The bound a deadline-bound read re-arms before every refill: the time left
/// until one absolute deadline, as `SO_RCVTIMEO`.
///
/// A detached peer's queued bytes are still read: the one-shot `aterm ctl`
/// writes `ACK <nonce>` and exits, and on Darwin `SO_RCVTIMEO` then fails with
/// EINVAL while `recv` still returns the queued bytes (see
/// [`enter_detached_peer_drain`]).
struct DeadlineRead<'a> {
    stream: &'a CtlStream,
    deadline: std::time::Instant,
    /// Latched, not re-tested per refill: once Darwin has refused the bound
    /// for a detached peer, another failing syscall buys nothing.
    draining_detached_peer: bool,
}

impl<'a> DeadlineRead<'a> {
    fn new(stream: &'a CtlStream, deadline: std::time::Instant) -> Self {
        Self {
            stream,
            deadline,
            draining_detached_peer: false,
        }
    }

    fn arm(&mut self) -> std::io::Result<()> {
        // The absolute deadline still governs, drain or not: it is the one
        // bound a peer cannot influence.
        let budget = remaining_artifact_io_budget(self.deadline)?;
        if self.draining_detached_peer {
            return Ok(());
        }
        match self.stream.set_read_timeout(Some(budget)) {
            Ok(()) => Ok(()),
            Err(refused) => {
                enter_detached_peer_drain(self.stream, refused)?;
                self.draining_detached_peer = true;
                Ok(())
            }
        }
    }
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
/// On unix, a level-triggered socket wait parks the lane until data or EOF; the
/// recv timeout remains a backstop for a spurious readiness event.
/// A coalesced next line already in the reader's buffer must bypass the socket
/// wait: its bytes are no longer visible to `poll` and waiting would deadlock.
/// An over-long line's refusal goes to `refusal`.
fn read_authenticated_request_line(
    reader: &mut control_lanes::LaneReader<'_>,
    refusal: &mut impl Write,
) -> Option<String> {
    let stream = reader.stream();
    let mut buffered_first_refill = !reader.buffer().is_empty();
    read_request_line_with_policy(reader, refusal, true, || {
        if std::mem::take(&mut buffered_first_refill) {
            return Ok(());
        }
        // After a refill without a newline, the parser consumes the entire
        // visible chunk. Its next iteration is therefore an underlying read.
        wait_authenticated_readable(stream)
    })
}

/// Park until the socket is readable or hung up, so an idle authenticated
/// connection (the fabric bridge, a lane with no parker) does not wake on every
/// [`AUTHENTICATED_READ_POLL`] tick. On Darwin `poll(POLLIN)` also reports a
/// write-half close as readable plus hangup even when `recvfrom` would sleep.
/// Polls the borrowed fd (a cloned AF_UNIX fd's close can stall).
#[cfg(unix)]
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

#[cfg(unix)]
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

/// No readiness wait: the [`AUTHENTICATED_READ_POLL`] receive timeout is the
/// wait.
#[cfg(not(unix))]
fn wait_authenticated_readable(_stream: &CtlStream) -> std::io::Result<()> {
    Ok(())
}

/// Read one request line through `reader`, calling `before_read` before every
/// buffer refill. `None` on EOF, a read error, or a line longer than
/// [`MAX_REQUEST_LINE`] — refused first into `refusal` (the connection's
/// answers), so its peer reads why it is closed rather than a bare EOF.
fn read_request_line_with_policy(
    reader: &mut impl BufRead,
    refusal: &mut impl Write,
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
                    // A runaway line: nothing past it is read, so the connection
                    // closes once the refusal is out.
                    let reply = format!(
                        "ERR {}\n",
                        aterm_types::control_verbs::request_line_too_long()
                    );
                    let _ = refusal.write_all(reply.as_bytes());
                    let _ = refusal.flush();
                    return None;
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

/// Upper bound on one control request line (`feed-bin` payloads are bounded
/// separately); a longer line is refused and the connection closed. The
/// client refuses one before sending it, from the same constant.
const MAX_REQUEST_LINE: usize = aterm_types::control_verbs::MAX_REQUEST_LINE;

/// How long a fresh connection has to make its handshake line whole: one
/// deadline from admission, not a timeout per read, so a peer that sends a byte
/// now and then does not keep its connection for it. The parker holds the
/// connection meanwhile (`control_lanes.rs`); a lane without one reads the line
/// under it, from its pick-up.
const AUTH_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Receive timeout for authenticated sockets, armed on every platform. On unix
/// a request line's read follows a readiness wait, so it fires there only on a
/// spurious readiness event; elsewhere it is that wait. A frame body is read
/// without one, so a stalled body wakes on it and
/// [`read_exact_authenticated_until`] checks its deadline. A timed-out read is
/// retried, never taken for EOF.
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

/// [`decode_request_line`] of a line still in its buffer (its `\n` not
/// included), borrowed: a valid UTF-8 line costs no copy.
fn request_line_text(line: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line))
}

/// Maximum `feed-bin` payload per frame (256 KiB).
const MAX_FEED_BIN: usize = 256 * 1024;

/// Maximum `@<sel>` targets in one `subscribe`. Each target adds a `ByteFanout`
/// slot the PTY reader tees on every burst, so the list is capped; selectors are
/// also de-duplicated by session.
pub(crate) const MAX_SUBSCRIBE_TARGETS: usize = 256;

/// A length-prefixed `post`'s body (up to [`crate::fabric::BODY_MAX`]) travels
/// on the stream so model text never passes through argv; `rest` is the line
/// after the verb. `Some(Ok(n))` announces n bytes; `Some(Err(reply))` is a
/// `len=` the server will not read ([`crate::fabric::post_len`]), which desyncs
/// the stream and closes the connection; `None` is an inline `post`.
///
/// The scan follows `cmd_post`'s own rule — OPTIONS LEAD, the first token that is
/// not one (or the first after `--`) begins the inline text — so a `post … hello len=3` whose `len=` sits in
/// the BODY is not a frame, and the two parsers cannot disagree about where the
/// text starts. That symmetry is load-bearing: a detector that saw a frame the
/// handler did not would leave n bytes of body to be read as control verbs.
fn post_frame_len(rest: &str) -> Option<Result<usize, String>> {
    // The option vocabulary `cmd_post` accepts before the body begins, and its
    // `--` end of options — the SAME reading the handler uses, not a copy of it.
    crate::fabric::post_options(rest.split_whitespace())
        .find_map(|tok| tok.strip_prefix("len="))
        .map(crate::fabric::post_len)
}

/// Whether this request is a WORKER's spawn (`spawn … agent=<word> len=<n>`),
/// whose task — at most [`crate::task_launch::TASK_MAX`] raw bytes — travels
/// on the stream after the line (`task_launch`). `Some(Ok(n))` announces n
/// bytes; `Some(Err(reply))` is a `len=` the server will not read
/// ([`crate::task_launch::frame_len`]), which desyncs the stream and closes
/// the connection.
///
/// Read with the tokenizer `parse_spawn_args` uses (`split_quoted_tokens`), so
/// a quoted `cwd="… len=9"` is one token to both. A line that tokenizer
/// refuses (an unterminated quote) is read word by word instead, and any
/// `len=` there is a frame: its bytes are consumed and the handler answers
/// the usage line, so a task — text a model wrote — is never read as verbs.
fn spawn_frame_len(head: &Head<'_>) -> Option<Result<usize, String>> {
    if head.verb != "spawn" {
        return None;
    }
    let rest = head.rest;
    let tokens = control_media::split_quoted_tokens(rest)
        .unwrap_or_else(|| rest.split_whitespace().map(str::to_string).collect());
    let agent =
        crate::task_launch::worker_word(tokens.iter().find_map(|tok| tok.strip_prefix("agent=")));
    tokens
        .iter()
        .find_map(|tok| tok.strip_prefix("len="))
        .map(|len| crate::task_launch::frame_len(len, agent))
}

/// A worker's task: the `n` bytes after its spawn line ([`spawn_frame_len`]),
/// read from `reader` — answered `GO <n>` on `stream` first
/// ([`aterm_types::control_verbs::task_frame_go`]), as the client waits for,
/// unless they are here already: a parked connection's line had its `GO` from
/// `ControlWorkerContext::frame_header` and is served once its task is whole.
fn read_worker_task(
    stream: &CtlStream,
    reader: &mut control_lanes::LaneReader<'_>,
    n: usize,
) -> std::io::Result<Vec<u8>> {
    if reader.buffer().len() < n {
        let go = aterm_types::control_verbs::task_frame_go(n);
        let mut out = stream;
        out.write_all(go.as_bytes())?;
        out.flush()?;
    }
    let mut body = vec![0u8; n];
    read_exact_authenticated(reader, &mut body)?;
    Ok(body)
}

/// The refusal of a worker's spawn frame (`spawn … len=<n>`, [`spawn_frame_len`])
/// aimed at `@<sid>` this instance does not host. Every other request so aimed
/// is relayed to the instance that hosts it, but a frame is held here until its
/// task is whole — and the client sends the task only once its head is answered
/// `GO` ([`aterm_types::control_verbs::task_frame_go`]), which this instance
/// cannot answer for another (one that predates the frame would serve the task
/// as requests). So it is refused, the task unsent, and the connection closes:
/// the client asks the instance its own session lives on.
fn worker_frame_refusal(head: &Head<'_>, store: &Store) -> Option<String> {
    spawn_frame_len(head)?.ok()?;
    let agent = crate::task_launch::worker_word(
        head.rest
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("agent=")),
    );
    let Some(selector @ Selector::Sid(sid)) = head.selector else {
        return None;
    };
    let hosted = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_selector(&selector)
        .is_some();
    (!hosted).then(|| {
        format!(
            "ERR spawn agent={agent}: @{} is not a session of this instance; ask the one that \
             hosts it\n",
            sid.as_str()
        )
    })
}

/// The bytes of body a request announces: what [`serve_request_line`] reads
/// after the line before it answers — a `post … len=<n>`, `feed-bin <n>` or
/// `paste-bin <n>` (by its row's [`Body`]), or a worker's `spawn … len=<n>`
/// ([`spawn_frame_len`]), with a length it will read. Zero for every other
/// line, and for a length it refuses unread (it answers and closes instead).
/// The lanes serve a request only once this much has followed its line
/// (`control_lanes::held`), so a lane never waits for a body.
fn announced_body(head: &Head<'_>, form: Option<Resolved>) -> usize {
    if let Some(len) = spawn_frame_len(head) {
        return len.unwrap_or(0);
    }
    match form.map_or(Body::None, |form| form.row.body) {
        Body::None => 0,
        Body::PostLen => post_frame_len(head.rest).map_or(0, |len| len.unwrap_or(0)),
        Body::Counted { keyed } => match parse_frame_header(head.rest, keyed) {
            FeedBinFrame::Ok(n, _) => n,
            FeedBinFrame::Malformed | FeedBinFrame::TooLarge => 0,
        },
    }
}

/// [`announced_body`] of a line still in a connection's buffer, before it is a
/// [`Request`]: the lanes' test of whether a request is whole
/// (`control_lanes::held`), which reads it in place.
fn announced_body_of(line: &str) -> usize {
    let head = control_request::head(line);
    announced_body(
        &head,
        aterm_types::control_verbs::spec_for(head.verb, head.rest),
    )
}

/// Outcome of parsing a counted frame's header, `[@<sel>] feed-bin <n>`. Because
/// the payload is pipelined on the same stream, the failures need opposite
/// handling:
/// - [`Malformed`](FeedBinFrame::Malformed): no length announced, so reply
///   `ERR` and stay framed.
/// - [`TooLarge`](FeedBinFrame::TooLarge): a length past [`MAX_FEED_BIN`]; the
///   client already sent bytes we will not read, so the caller must close the
///   connection rather than execute them as verbs.
#[derive(Debug, PartialEq, Eq)]
enum FeedBinFrame<'a> {
    /// Not a well-formed header (missing / non-numeric length, a stray token).
    Malformed,
    /// Well-formed but the declared length exceeds [`MAX_FEED_BIN`].
    TooLarge,
    /// A valid frame: the length (`<= MAX_FEED_BIN`) and the optional
    /// idempotency key, carried in the header so a refused key still consumes
    /// the payload.
    Ok(usize, Option<&'a str>),
}

/// Parse a counted frame's header after its verb (`rest`): `<n>`, then an
/// `id=<key>` when the row is `keyed` (`feed-bin`). Pure; see [`FeedBinFrame`]
/// for why oversize and malformed differ.
fn parse_frame_header(rest: &str, keyed: bool) -> FeedBinFrame<'_> {
    let mut it = rest.split_whitespace();
    let Some(len_tok) = it.next() else {
        return FeedBinFrame::Malformed;
    };
    // One optional trailing token, `feed-bin` only: the idempotency key
    // `id=<epoch>:<producer>:<seq>`. `paste-bin` takes none.
    //
    // Any other trailing token is malformed (`feed-bin 1 junk` must not consume
    // a byte of the next request). A token shaped like a key but invalid as one
    // is not malformed here: the payload is read and the key parser refuses it.
    let idem = match it.next() {
        None => None,
        Some(tok) if keyed => match tok.strip_prefix("id=") {
            Some(key) => Some(key),
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
    FeedBinFrame::Ok(n, idem)
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
    let request = Request::parse(line.to_string());
    let frame = request.frame().expect("a counted frame's header");
    run_feed_bin_routed_observed(
        &request,
        frame,
        reader,
        FeedBinRoute {
            active,
            store,
            scope,
        },
        &mut dispatch,
        writer,
    )
}

/// THE `mark` ARM (design §7.4, phase 3A): a claim written to session `ctx`'s
/// log, attributed to the connection ([`control_session::log_actor`]), and the
/// session's watchers woken once it is in. Its whole job is these calls: a mark
/// is a record, and this path names no input, signal or session write — the
/// `slog-inert` census guard holds this function to exactly this list, and the
/// functional negative control (`a_mark_act_on_an_open_approval_writes_no_byte`)
/// proves no byte reaches the PTY.
fn dispatch_mark(ctx: &SessionCtx, scope: Scope, rest: &str) -> String {
    let reads = scope.is_owner_class() || cross_session_authorized(scope, "text", ctx);
    let (reply, owed) =
        crate::session_log::cmd_mark(ctx, control_session::log_actor(scope, ctx), reads, rest);
    if owed {
        ctx.watchers.ring_watchers();
    }
    reply
}

/// RING THE WATCHERS of the session whose stable id is `sid`, as a fabric
/// verb names it once its fabric lock is released (the session log's `mail`,
/// `mail-landed` and `hold` marks): its watchers are the registry's, read
/// with no other lock held.
pub(crate) fn wake_sid_watchers(store: &Store, sid: &aterm_session::SessionId) {
    let watchers = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_sid(sid)
        .map(|h| Arc::clone(&h.ctx.watchers));
    if let Some(watchers) = watchers {
        watchers.ring_watchers();
    }
}

/// Serve a length-prefixed `post`: read the body, then hand it to
/// [`crate::fabric::cmd_post`]. Returns `false` to close only when the stream
/// is desynced (a refused length or a short read).
///
/// An Edge connection is refused outright (see [`post_scope_denied`]); the
/// refusal still consumes the body.
fn run_post_bin<W: Write>(
    request: &Request,
    len: Result<usize, String>,
    reader: &mut impl BufRead,
    active: &ActiveHandle,
    store: &Store,
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
    let Head {
        selector,
        verb,
        rest,
    } = request.head();
    debug_assert_eq!(verb, "post");
    let target = match selector.map_or(Resolution::SelfTok, |s| resolve(store, &s)) {
        Resolution::SelfTok => resolve_active(active),
        Resolution::Hosted(target) => Some(target),
        Resolution::Missing => None,
        // The body is already read, so the stream stays framed: answer the
        // refusal and keep the connection.
        Resolution::Ambiguous(refusal) => {
            if writer.write_all(refusal.as_bytes()).is_err() {
                return false;
            }
            return writer.flush().is_ok();
        }
    };
    let response = match target {
        _ if post_scope_denied(scope) => "ERR denied\n".to_string(),
        Some((_, _, _, ctx)) => crate::fabric::cmd_post(&ctx, rest, Some(body)),
        None if selector.is_some_and(|s| !s.is_self()) => "ERR no such session\n".to_string(),
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
/// target's licence is fenced here ([`crate::licence_fence`]). A lane runs it as
/// soon as the header is whole (`LaneService::frame_header`) and again when the
/// whole frame is served ([`run_feed_bin_routed`]): the second fence is one more
/// of the kind the payload gets anyway at its effect boundary, before egress.
/// `false`: refused before the payload (an ambiguous target), the answer in
/// `writer`; the stream is desynced, so the connection closes.
fn feed_bin_header<W: Write>(
    selector: Option<&Selector<'_>>,
    route: FeedBinRoute<'_>,
    writer: &mut W,
) -> bool {
    // Malformed and over-cap attempts follow the same rule (the target is the
    // header's, whatever its length says); unknown or unauthorized targets never
    // mutate UI state.
    let header_target = match selector.map_or(Resolution::SelfTok, |s| resolve(route.store, s)) {
        Resolution::SelfTok => resolve_active(route.active),
        Resolution::Hosted(target) => Some(target),
        Resolution::Missing => None,
        // An ambiguous id must not clear another instance's licence either:
        // refuse before the payload. It is unread, so the stream is desynced:
        // answer and close.
        Resolution::Ambiguous(refusal) => {
            let _ = writer.write_all(refusal.as_bytes());
            let _ = writer.flush();
            return false;
        }
    };
    if let Some((_, _, _, ctx)) = header_target.as_ref()
        && (route.scope.is_owner_class() || cross_session_authorized(route.scope, "feed", ctx))
    {
        ctx.licence.fence(false);
    }
    true
}

#[derive(Clone, Copy)]
struct FeedBinRoute<'a> {
    active: &'a ActiveHandle,
    store: &'a Store,
    scope: Scope,
}

/// [`run_feed_bin_routed_observed`] for a front dispatch that takes no
/// completion: the tests' stand-in seam.
#[cfg(all(test, unix))]
fn run_feed_bin_routed<W: Write, F>(
    request: &Request,
    frame: FeedBinFrame<'_>,
    reader: &mut impl BufRead,
    route: FeedBinRoute<'_>,
    dispatch_front_input: &mut F,
    writer: &mut W,
) -> bool
where
    F: FnMut(InputEvent, Option<u64>) -> Result<InputOutcome, String>,
{
    let mut observed = |event, target, _completion| dispatch_front_input(event, target);
    run_feed_bin_routed_observed(request, frame, reader, route, &mut observed, writer)
}

/// Serve a `[@<sel>] feed-bin|paste-bin <n>` frame whose header `frame` was
/// read: its payload, its admission and its write, the answer in `writer`.
/// `false` closes the connection (the stream is desynced). An accepted write
/// is noted in the session log by its author once the seam's receipt says it
/// landed (`control_input_ack`): the front dispatch carries the completion.
fn run_feed_bin_routed_observed<W: Write, F>(
    request: &Request,
    frame: FeedBinFrame<'_>,
    reader: &mut impl BufRead,
    route: FeedBinRoute<'_>,
    dispatch_front_input: &mut F,
    writer: &mut W,
) -> bool
where
    F: FnMut(
        InputEvent,
        Option<u64>,
        Option<crate::control_input_ack::InputCompletion>,
    ) -> Result<InputOutcome, String>,
{
    let Head { selector, verb, .. } = request.head();
    let class = request
        .form()
        .map_or(Input::NONE, |form| form.row.input_class());
    // `paste-bin` goes through the paste seam (bracketing, sanitize, LF→CR under
    // the target's lock); `feed-bin` writes raw. Everything else is shared.
    let paste = verb == "paste-bin";
    if !feed_bin_header(selector.as_ref(), route, writer) {
        return false;
    }
    let (n, idem_key) = match frame {
        FeedBinFrame::Ok(n, idem) => (n, idem),
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
    let target = match selector.map_or(Resolution::SelfTok, |s| resolve(route.store, &s)) {
        Resolution::SelfTok => active_target.clone(),
        Resolution::Hosted(target) => Some(target),
        Resolution::Missing => None,
        // Re-check ambiguity at the effect boundary too; the payload is
        // consumed, so reply and stay framed.
        Resolution::Ambiguous(refusal) => {
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
                Ok(event) => control_input::bytes_reply(dispatch_front_input(event, None, None), n),
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
        let error = if selector.is_none_or(|s| s.is_self()) {
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
    ctx.licence.fence(false);

    // The input admission every input request passes (`admit`), in its one order:
    // the halt, the self-feed floor (a self-targeted frame), turn arbitration, the edit
    // hold, then the unread-input gate, which judges the body itself (a lone `^C` into
    // a cbreak program is a signal, not input behind the unread). A frame's malformed
    // header was answered before its body. The payload is consumed, so a refusal keeps
    // the stream framed.
    let is_cross = selector.is_some_and(|s| !s.is_self());
    let warning = match admit(InputAdmission {
        class,
        verb,
        ctx: &ctx,
        session: target_session,
        is_cross,
        args: &payload,
        unread_ok: false,
        usage: None,
    }) {
        Ok(warning) => warning,
        Err(refusal) => {
            let _ = writer.write_all(refusal.as_bytes());
            let _ = writer.flush();
            return true;
        }
    };

    // The idempotency gate wraps only the write, so every earlier refusal leaves
    // the key's sequence unconsumed; a frame without `id=` runs unguarded. A
    // keyed zero-length frame is refused before the claim.
    if let Some(refusal) = crate::pty_idem::empty_payload_refusal(idem_key, payload.is_empty()) {
        let _ = writer.write_all(refusal.as_bytes());
        let _ = writer.flush();
        return true;
    }
    // A frame delivers by the route a `send` takes (`Input::PINNED`): the App seam
    // pinned to its session when that session is on screen, an explicit selector
    // naming it included, so a visible tab gets a human paste's side effects; the
    // background route otherwise, which answers from the write's receipt (a short or
    // failed write is `ERR write failed`, never a false `OK` an `id=` retry would
    // read as `dup=1`) and never parks this lane on the program.
    let input_route = InputRoute::of(
        class,
        is_cross,
        front_terminal_session == Some(target_session),
        target_session,
    );
    // An accepted write is noted in the session log by its author
    // (`control_input_ack`); an `in-doubt` mark wakes the log's readers.
    let mark = crate::session_log::InputVerb::of(verb).map(|input| {
        crate::control_input_ack::InputMark::at_epoch(
            &ctx,
            target_session,
            control_session::log_actor(route.scope, &ctx),
            input,
            None,
            false,
        )
        .typed_by(&ctx, serving_caller(route.scope, &ctx))
    });
    let completion = || {
        mark.as_ref()
            .map(crate::control_input_ack::InputMark::completion)
    };
    let wake = || ctx.watchers.ring_watchers();
    let response =
        crate::pty_idem::guarded_waking(&ctx, route.scope, verb, idem_key, &wake, || {
            let event = if paste {
                // AtDrain frames the paste by the mode at its write, as `paste` does.
                InputEvent::Paste(
                    String::from_utf8_lossy(&payload).into_owned(),
                    PasteFraming::AtDrain,
                )
            } else {
                InputEvent::KeySequence(payload)
            };
            let front = |ev, pin, completion| dispatch_front_input(ev, pin, completion);
            control_input::bytes_reply(
                deliver(input_route, class, front, &term, &ctx, event, completion()),
                n,
            )
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
/// [`run_subscribe_socket`], whose read side sees the client hang up even
/// while a sibling test's fork holds a copy of the client's end — a write then
/// still succeeds, and a loop waiting for one to fail never ends.
#[cfg(test)]
fn run_subscribe<W: Write>(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
    writer: &mut W,
) {
    let head = control_request::head(line);
    run_subscribe_with_peer_probe(head, active, store, scope, writer, || false);
}

/// Run a production socket subscription with read-side disconnect detection,
/// then HANG THE CONNECTION UP: a push stream is the last thing a connection
/// carries, so however it ended, its client reads end-of-file here.
///
/// The protocol becomes push-only after the subscribe acknowledgement. Its client
/// MUST keep the write/send half open and send no more bytes; EOF, an unexpected
/// byte, or a read error therefore all mean the peer is gone. On unix the peer
/// watch ([`crate::peer_watch`]) polls the stream for exactly that and wakes the
/// push loop, which then needs no timer to notice; where it does not run, the
/// loop asks [`subscription_peer_gone`] at least every 250 ms.
///
/// A client that half-closed (`printf 'subscribe …\n' | nc -U`) is gone to the
/// protocol but still reading. The hang-up shuts each half on its own
/// ([`aterm_uds::hangup::hang_up`]): Darwin refuses a both-ways `shutdown` of a
/// socket whose read half that half-close shut, and shuts NOTHING, so such a
/// client read its end-of-file only when the last copy of this end closed — and
/// a child any thread forked holds one until it execs.
fn run_subscribe_socket(
    head: Head<'_>,
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
    writer: &mut CtlStream,
) {
    // The push loop writes through a shared borrow, which the peer watch's
    // guard also holds: the guard has let go of the descriptor before the
    // caller can drop the stream.
    let stream: &CtlStream = writer;
    let mut out = PushWriter(stream);
    #[cfg(unix)]
    match crate::peer_watch::watch_push(stream) {
        Some(guard) => run_subscribe_with_peer_probe(
            head,
            active,
            store,
            scope,
            &mut out,
            WatchedPeer { guard, stream },
        ),
        None => run_subscribe_with_peer_probe(head, active, store, scope, &mut out, || {
            subscription_peer_gone(stream)
        }),
    }
    // No peer watch off unix: the push loop asks the stream itself, without
    // waiting ([`subscription_peer_gone`]).
    #[cfg(not(unix))]
    run_subscribe_with_peer_probe(head, active, store, scope, &mut out, || {
        subscription_peer_gone(stream)
    });
    aterm_uds::hangup::hang_up(stream);
}

/// The push stream's writer. Its gather is [`aterm_uds::fdpass::send_vectored`],
/// not std's `write_vectored`, a bare `writev` that raises SIGPIPE at a client
/// that hung up whenever SIGPIPE is not ignored, as between
/// `Command::exec`'s reset and the `execve` of a cold-lane update: one push
/// thread meeting a gone client there would end every tab. Its `write` is the
/// stream's own, which sends with `MSG_NOSIGNAL`.
struct PushWriter<'a>(&'a CtlStream);

impl Write for PushWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[std::io::IoSlice<'_>]) -> std::io::Result<usize> {
        aterm_uds::fdpass::send_vectored(self.0, bufs)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A push stream's liveness from the peer watch, and from the read-side probe
/// once the watch has handed the stream back ([`crate::peer_watch::Peer::Unwatched`]).
#[cfg(unix)]
struct WatchedPeer<'a> {
    guard: crate::peer_watch::PeerGuard<'a>,
    stream: &'a CtlStream,
}

#[cfg(unix)]
impl subscribe::PeerLiveness for WatchedPeer<'_> {
    fn gone(&mut self) -> bool {
        match self.guard.peer() {
            crate::peer_watch::Peer::Watching => false,
            crate::peer_watch::Peer::Gone => true,
            crate::peer_watch::Peer::Unwatched => subscription_peer_gone(self.stream),
        }
    }

    fn rings(&self) -> Option<&Arc<crate::session_watch::Watchers>> {
        (self.guard.peer() == crate::peer_watch::Peer::Watching).then(|| self.guard.wakes())
    }
}

/// Whether a push-only subscriber is gone, asked of the stream itself and
/// WITHOUT WAITING: any byte, EOF or read error is gone (its client sends
/// nothing after `subscribe`, so even a write-half close is a disconnect), and a
/// quiet open socket is there. Asked where the peer watch does not run, or once
/// it has handed the stream back ([`WatchedPeer`]).
///
/// A probe that waits is a wait on every pass it is asked before. It used to be
/// a `read` under a 1 ms receive timeout, and a quiet subscriber — every live
/// one — made it sleep the whole timeout out: every `EVENT` and `DELTA` to every
/// subscriber, the fabric bridge's `EVENT post` on the post → visible path
/// included, left about 1.1 ms late. Measured 2026-10-16, interleaved, load
/// 43–63: a notify until the subscriber has read the whole DELTA, p50 1.15 ms →
/// 0.020 ms (n = 800 each). The answer the bound waited for is already in the
/// socket — a queued byte or EOF is there to see at once.
///
/// The no-wait is guarded twice: a quiet-peer probe on a socket with no
/// receive timeout answers under a hang detector, and a test counts the
/// kernel's own syscalls to hold the whole probe to its one `recv`, so a wait
/// put beside the read is caught too.
///
/// Unix peeks on the borrowed descriptor with the per-call flag pair
/// `MSG_PEEK | MSG_DONTWAIT`: nothing consumed, no copy of the descriptor, and
/// no blocking mode set on a socket the writer shares (a non-blocking writer
/// would turn a slow reader's full buffer into a write error, and a stream that
/// ended on it). Windows has no per-call flag and needs none — its socket is
/// already non-blocking — so it takes one `recv` without the park its `read`
/// adds; that consumes a byte, which ends this stream anyway.
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
    usize::try_from(n).map_err(|_| std::io::Error::last_os_error())
}

#[cfg(not(unix))]
fn peek_push_peer(stream: &CtlStream) -> std::io::Result<usize> {
    let mut byte = [0_u8; 1];
    stream.read_nonblocking(&mut byte)
}

/// `subscribe`'s usage as one line: the reply on a connection that stays open
/// (`@<sid> subscribe …` through dispatch), where a failure is one `ERR` line and
/// a second would be read as the next request's reply.
const SUBSCRIBE_USAGE: &str = "ERR usage: subscribe [@<sel>[,<sel>]|@*] <streams> \
                               [since=<seq>] [since-turn=<id>] [since-block=<id>] \
                               [since-log=<seq> log-ep=<e>] [every-frame]\n";

/// The serve loop's `subscribe` refusal, which closes the connection after it:
/// [`SUBSCRIBE_USAGE`] and the streams, built from
/// [`aterm_types::control_verbs::SUBSCRIBE_STREAMS`] so every refusal path lists
/// the same streams.
fn subscribe_usage_bytes() -> Vec<u8> {
    format!(
        "{SUBSCRIBE_USAGE}  <streams>: a subset of {} and must name at least one frame \
         source; mail takes mail:kinds=<k,..>, mail:from=<class|principal> and/or \
         mail:topic=<t>; log takes log:lvl=<session|run|turn|step> and/or log:class=turn\n",
        aterm_types::control_verbs::SUBSCRIBE_STREAMS.join(",")
    )
    .into_bytes()
}

/// Authorize a `subscribe` request and, on success, flip the connection to push
/// mode (the push loop never returns to request polling).
///
/// Grammar: `subscribe [@<sel>[,<sel>...]|@*] <streams> [since=<seq>]
/// [since-turn=<id>] [since-block=<id>] [since-log=<seq> log-ep=<e>]
/// [every-frame]`, where `<streams>` is one
/// comma-separated token from `aterm_types::control_verbs::SUBSCRIBE_STREAMS`
/// plus the `timestamps`/`ts` and `trim` modifiers. `head` is the request
/// grammar's reading of the line ([`control_request::head`]), the one its lane,
/// relay and push flip read, and the words of its rest split where theirs do
/// ([`control_request::words`]), so a spelling they class a subscribe is served
/// as one and the `@<sid>` the relay reads first is the one resolved here.
///
/// Per-target streams are gated per `@<sel>` like a read verb (`ReadScreen` via
/// [`resolve`] and [`cross_session_authorized`]); the instance-scoped
/// `sessions` stream and `@*` are Owner-only. Any failure writes one `ERR ...`
/// and closes without a partial subscription; on success the socket and the
/// `OK subscribe <n>` ack go to [`subscribe::push_loop_with_peer_probe`].
pub(crate) fn run_subscribe_with_peer_probe<W: Write, L: subscribe::PeerLiveness>(
    head: Head<'_>,
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
    writer: &mut W,
    liveness: L,
) {
    // One usage text for every refusal: the selector list (or `@*`) is optional
    // and the trailing args are exactly what the parser accepts.
    let usage = subscribe_usage_bytes();
    let usage = usage.as_slice();
    // A selectorless `subscribe`, as the push flip admits; its rest is
    // `[@<sel>[,<sel>...]] <streams> [args]`.
    if head.verb != "subscribe" || head.selector.is_some() {
        let _ = writer.write_all(usage);
        let _ = writer.flush();
        return;
    }
    let mut it = control_request::words(head.rest);
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
    // `since-block=<id>` (events resume), `every-frame` (re-read `cells` every
    // wake, sending a changed body).
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
    // follows every tab instead of one subscription per tab, each holding a
    // subscription lane (`control_subscriptions_cap`).
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
        // A list element that names nothing is refused like a bare `@`
        // ([`empty_selector_refusal`]), never read as the active tab.
        if !aterm_types::control_verbs::selector_body_names_something(body) {
            let refusal = format!("{}\n", aterm_types::control_verbs::EMPTY_SELECTOR);
            let _ = writer.write_all(refusal.as_bytes());
            let _ = writer.flush();
            return;
        }
        let sel = Selector::parse(body);
        let target = match resolve(store, &sel) {
            Resolution::SelfTok => resolve_active(active),
            Resolution::Hosted(target) => Some(target),
            Resolution::Missing => None,
            // A subscribe is a READ, and an ambiguous one streams a pane's
            // output to a caller who asked for a different instance's pane.
            // Refuse.
            Resolution::Ambiguous(refusal) => {
                let _ = writer.write_all(refusal.as_bytes());
                let _ = writer.flush();
                return;
            }
        };
        let Some((term, _master, local_id, ctx)) = target else {
            let error = if sel.is_self() {
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
        if targets.iter().any(|t| t.local_id == local_id) {
            continue;
        }
        sub_map.push((local_id, ctx.self_id.as_str().to_string()));
        // The leaves only: `ctx` drops at the end of this iteration, so the
        // subscription holds nothing that keeps the session's master open
        // (`subscribe::WatchSource`).
        targets.push(subscribe::ResolvedTarget {
            local_id,
            term,
            src: subscribe::WatchSource::of(&ctx),
        });
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
        liveness,
    );
}

/// A resolved cross-session target: the same `(term, master, id, ctx)` shape as
/// `resolve_active`, for an `@<selector>` session. Cloned out of the [`Store`]
/// before the guard drops, so the registry lock is never held across a
/// `Terminal` lock.
type Target = (Arc<Mutex<Terminal>>, i32, u64, Arc<SessionCtx>);

/// THE EMPTY-SELECTOR GATE: the refusal a request whose leading `@` names
/// nothing earns ([`aterm_types::control_verbs::EMPTY_SELECTOR`], its newline
/// added), decided before any other path of [`serve_request_line`] but the
/// recovery-count gate ([`recovery_count_refusal`], which a frame's header
/// meets first too) and the drive and fleet gates reads it — the askpass
/// relay, the proxy, the push flip, every frame — so a client that sends
/// `@ turn …` over the wire is refused like `aterm ctl` refuses it before
/// sending. A bare `@` is what `@$W`
/// becomes when `$W` is empty, and read as self it would act on the front tab.
/// `subscribe`'s selector-second list and `await task`'s list refuse their own
/// empty elements where they parse them. The line's first word is read, not
/// its parsed selector: the grammar reads a bare `@` as the verb.
fn empty_selector_refusal(request: &Request) -> Option<String> {
    let body = control_request::words(request.line().trim_end_matches('\r'))
        .next()?
        .strip_prefix('@')?;
    (!aterm_types::control_verbs::selector_body_names_something(body))
        .then(|| format!("{}\n", aterm_types::control_verbs::EMPTY_SELECTOR))
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
fn ambiguous_sid_refusal(sel: &Selector<'_>) -> Option<String> {
    let Selector::Sid(sid) = sel else {
        return None;
    };
    let pid = crate::identity_claim::live_holder(&SessionId::new(sid.as_str()))?;
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
/// like [`cross_session_authorized`] but on what the request needs ([`need_of`]): its
/// table form's op, or the [`argument_escalation`] of an indirect seam (`invoke
/// <clipboard/config action>`, `open prefs|settings`). That need replaces the verb's
/// base op, so an edge granted the fine op (ClipboardWrite/ConfigWrite) passes while a
/// plain WriteInput edge cannot tunnel through. A [`Need::OwnerOnly`] (the palette
/// gateway, update-apply twins) admits no Edge. Owner reaches self and siblings; a
/// row no op gate admits is denied. `form` is the request's table form.
fn dispatch_authorized(
    scope: Scope,
    form: &Resolved,
    verb: &str,
    rest: &str,
    target_ctx: &SessionCtx,
) -> bool {
    need_of(form, verb, rest).is_some_and(|need| scope_holds_need(scope, need, target_ctx))
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
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => None,
    }
}

/// What a scoped caller's front-surface observation hop came back with: the
/// surface, or the whole reply `serve_session_verb` sends instead. A failed hop is an
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

/// Given the caller's `scope`, the verb's input `class` and the front main-thread
/// surface, the escalation that replaces the verb's base WriteInput requirement for
/// a verb that can drive the front (`Input::FRONT_DRIVING`): every overlay is
/// Owner-only (some bind clipboard/external-process actions), native Settings needs
/// the durable-config fine op, and Settings ▸ Harness is Owner-only (its session
/// buttons write Autopilot words). Owner skips the observation and satisfies either.
///
/// The Harness fence is the whole page, not the keys that would press a session
/// button: which key presses one depends on the page's focus and a click's hit,
/// state the observation does not replay — a Tab that only moves focus onto a
/// button is what makes the next Return a press.
///
/// Pure for unit testing; a failed hop is denied by the caller, never read as `None`.
fn front_drive_escalation(
    scope: Scope,
    class: Input,
    surface: FrontControlSurface,
) -> Option<Need> {
    if scope.is_owner_class() || !class.contains(Input::FRONT_DRIVING) {
        return None;
    }
    match surface {
        FrontControlSurface::Overlay(_) | FrontControlSurface::HarnessSettings => {
            Some(Need::OwnerOnly)
        }
        FrontControlSurface::NativeSettings => Some(Need::Op(Op::ConfigWrite)),
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

// ── Background input ────────────────────────────────────────────────────────
//
// These run on the control thread against a resolved `@<selector>` target not on
// screen (`InputRoute::Background`), so there is no App UI/gesture/window state and no
// `Wake::Input`. They use the source-blind tracked seam and the engine's
// viewport/geometry APIs directly on the resolved `(term, &ctx.sink)`, after
// authorization.

/// How long a background paste's reply waits for its bytes to reach the program
/// before answering [`PASTE_UNREAD`]: the wait family's default
/// (`aterm_control::wire::WAIT_DEFAULT_MS`).
const PASTE_RECEIPT_WAIT: std::time::Duration =
    std::time::Duration::from_millis(aterm_control::wire::WAIT_DEFAULT_MS);

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
/// A paste's `OK` stays a kernel receipt (see [`background_reply`]). An event handed
/// to the session's ordered writer (behind input queued there, and on Windows always)
/// is answered from the write itself, not its admission ([`await_written`]): a write
/// into a dead peer is `ERR write failed` on every platform. Until 2026-10-19 it was
/// answered on admission, so on Windows every background key, `send` and paste into a
/// dead peer answered `OK`.
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
    let paste = matches!(ev, InputEvent::Paste(..));
    let started = std::time::Instant::now();
    let (receipt, written) = crate::app_input::paste_order::unparked_observed(
        term,
        &ctx.modes,
        &ctx.sink,
        &ctx.output_echo,
        ev,
        completion,
    );
    let receipt = match written {
        None => receipt,
        // A paste waits its whole bound, on its wait lane; anything else a moment,
        // so a request lane is never held by a program that stopped reading.
        Some(written) => match await_written(
            &written,
            if paste {
                limit
            } else {
                limit.min(queued_input_wait())
            },
        ) {
            Waited::Wrote(receipt) => receipt,
            Waited::Unrun => return Ok(InputOutcome::WriteFailed),
            Waited::HungUp => return Err(HUNG_UP_REPLY.to_string()),
            // Still queued for a program that has not read it: a paste is in doubt,
            // anything else accepted for delivery, as the spill accepts it on unix.
            Waited::Queued if paste => return Err(PASTE_UNREAD.to_string()),
            Waited::Queued => return Ok(InputOutcome::Ok),
        },
    };
    background_reply(
        &ctx.sink,
        receipt,
        paste.then_some(discards),
        limit.saturating_sub(started.elapsed()),
    )
}

/// How long a background key, `send`, `feed` or mouse report handed to the session's
/// ordered writer waits for the writer to run it ([`await_written`]): the writer's
/// wake and one write, or a dead peer's failure, take far less; past it the write is
/// queued behind a program that has not read, and the event is answered accepted for
/// delivery. Short, because these verbs run on request lanes.
const QUEUED_INPUT_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// [`QUEUED_INPUT_WAIT`], or a test fixture's own wait for this thread.
fn queued_input_wait() -> std::time::Duration {
    #[cfg(test)]
    if let Some(wait) = QUEUED_INPUT_WAIT_FOR_TEST.with(std::cell::Cell::get) {
        return wait;
    }
    QUEUED_INPUT_WAIT
}

#[cfg(test)]
thread_local! {
    static QUEUED_INPUT_WAIT_FOR_TEST: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

/// Test-only: this thread's lanes wait `wait`, not [`QUEUED_INPUT_WAIT`], for a write
/// they handed to the session's ordered writer, until the guard drops. A test that
/// judges the write's own answer waits `aterm_spec::wait::MUST_HAPPEN`, so a slow
/// writer is a hang detector, never an `OK` read as "queued".
#[cfg(test)]
fn queued_input_waits_for_test(wait: std::time::Duration) -> QueuedInputWait {
    QUEUED_INPUT_WAIT_FOR_TEST.with(|cell| cell.set(Some(wait)));
    QueuedInputWait(())
}

#[cfg(test)]
struct QueuedInputWait(());

#[cfg(test)]
impl Drop for QueuedInputWait {
    fn drop(&mut self) {
        QUEUED_INPUT_WAIT_FOR_TEST.with(|cell| cell.set(None));
    }
}

/// What a lane learned of a write it handed to the session's ordered writer.
enum Waited {
    /// The writer ran it: its own receipt.
    Wrote(crate::input::EgressReceipt),
    /// It was dropped unrun (a stop, or its session severed): nothing written.
    Unrun,
    /// Still queued or writing when the wait ended.
    Queued,
    /// The caller hung up first.
    HungUp,
}

/// Wait, at most `bound` and only while the caller is there, for the write a lane
/// handed to the session's ordered writer
/// ([`crate::app_input::paste_order::Written`]), in the one park every blocking
/// control verb waits in ([`crate::park::park`]), as [`background_reply`] waits
/// for the same write's kernel receipt.
fn await_written(
    written: &crate::app_input::paste_order::Written,
    bound: std::time::Duration,
) -> Waited {
    let now = std::time::Instant::now();
    let deadline = now.checked_add(bound).unwrap_or(now);
    let mut got = None;
    let woke = crate::park::park(
        |slice| {
            got = match written.wait(slice) {
                Ok(receipt) => Some(Waited::Wrote(receipt)),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Some(Waited::Unrun),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            };
            got.is_some()
        },
        deadline,
        crate::park::Lane::SERVED,
        &|| false,
    );
    match got {
        Some(waited) => waited,
        None if woke == crate::park::Woke::HungUp => Waited::HungUp,
        None => Waited::Queued,
    }
}

/// The reply verdict for one background (control-lane) write: the seam's receipt
/// through [`crate::app_input::receipt_outcome`], so a short or failed write is
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
    let outcome = crate::app_input::receipt_outcome(receipt);
    let Some(discards) = kernel_receipt else {
        return Ok(outcome);
    };
    if outcome != InputOutcome::Ok || receipt.is_queued() {
        return Ok(outcome);
    }
    let now = std::time::Instant::now();
    let deadline = now.checked_add(limit).unwrap_or(now);
    let mut drained = None;
    let woke = crate::park::park(
        |bound| {
            drained = sink.wait_egress_drained_to_kernel_for(bound);
            drained.is_some()
        },
        deadline,
        crate::park::Lane::SERVED,
        &|| false,
    );
    match drained {
        Some(true) if sink.discards() == discards => Ok(InputOutcome::Ok),
        Some(_) => Ok(InputOutcome::WriteFailed),
        None if woke == crate::park::Woke::HungUp => Err(HUNG_UP_REPLY.to_string()),
        None => Err(PASTE_UNREAD.to_string()),
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

/// Where an input verb's events go, chosen once per request ([`InputRoute::of`]):
/// every event of a `turn`, and the one of every other input verb, takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputRoute {
    /// The App input seam on the event loop (`Wake::Input`), with its side effects
    /// (blink reset, viewport snap, selection clear, the typing-momentum hints, the
    /// `video ... keys` ledger): pinned to the session the request was authorized
    /// against, or (`None`) to whatever the event loop finds in front.
    Seam { pin: Option<u64> },
    /// A session not on screen: the control thread writes it directly, round-trip
    /// free ([`background_input_observed`]).
    Background,
}

impl InputRoute {
    /// The route for a `class` request on `session`. A request pinned on screen
    /// ([`front_routed`]: an explicit selector naming the front tab, or a scoped
    /// caller's flagless one) takes the seam pinned; any other explicit selector
    /// writes in the background. An Owner's flagless request takes the seam: pinned
    /// for a write of the program's own bytes (`Input::PINNED`), following the front
    /// for a gesture, which an overlay in front may take instead.
    const fn of(class: Input, is_cross: bool, targets_front: bool, session: u64) -> Self {
        if targets_front || (!is_cross && class.contains(Input::PINNED)) {
            Self::Seam { pin: Some(session) }
        } else if is_cross {
            Self::Background
        } else {
            Self::Seam { pin: None }
        }
    }
}

/// Deliver one input event of a `class` verb by `route`: through `front` (the App
/// seam: `Wake::Input`, or a test's stand-in) or straight to the background
/// session, whose write is counted for the `video ... keys` ledger the seam would
/// have kept, and, for the program's own bytes (`Input::PINNED`), stamps the
/// input-arrival instant as a raw write does. `completion` notes the write in the
/// session log once its receipt says it was accepted (`control_input_ack`),
/// whichever route takes it.
fn deliver(
    route: InputRoute,
    class: Input,
    front: impl FnOnce(
        InputEvent,
        Option<u64>,
        Option<crate::control_input_ack::InputCompletion>,
    ) -> Result<InputOutcome, String>,
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: InputEvent,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> Result<InputOutcome, String> {
    match route {
        InputRoute::Seam { pin } => front(ev, pin, completion),
        InputRoute::Background => {
            if class.contains(Input::PINNED) {
                crate::metrics::note_input();
                crate::input_hint::arm();
            }
            crate::note_unseamed_control_input(&ev);
            background_input_observed(term, ctx, &ev, PASTE_RECEIPT_WAIT, completion)
        }
    }
}

/// A pinned `resize` (the tab on screen, `InputRoute::Seam` with a pin): the px or
/// cell form posted through the App seam for `session`, its outcome the reply.
fn front_routed_resize(proxy: &EventLoopProxy<Wake>, session: u64, rest: &str) -> String {
    let event = match control_input::parse_resize_px(rest) {
        Some(Err(usage)) => return usage,
        Some(Ok(event)) => event,
        None => match control_input::parse_resize(rest) {
            Ok((rows, cols)) => InputEvent::Resize {
                rows,
                cols,
                echo_to_window: true,
            },
            Err(error) => return error,
        },
    };
    control_input::input_reply_to_str(post_input_reply_to(proxy, vec![event], Some(session)))
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
    // The App's report route, which never parks this lane on the program, answered
    // from the write itself when the session's ordered writer takes it
    // ([`background_input_observed`] has why).
    let (receipt, written) =
        crate::app_input::paste_order::report_unparked(term, &ctx.modes, &ctx.sink, &ev);
    if crate::app_input::receipt_outcome(receipt) == InputOutcome::QueueFull {
        return Err(INPUT_QUEUE_FULL.to_string());
    }
    let receipt = match written.map(|written| await_written(&written, queued_input_wait())) {
        None => receipt,
        Some(Waited::Wrote(written)) => written,
        Some(Waited::Unrun) => return Err("ERR write failed\n".to_string()),
        Some(Waited::HungUp) => return Err(HUNG_UP_REPLY.to_string()),
        Some(Waited::Queued) => return Ok(false),
    };
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

/// One input request as the admission stage reads it ([`admit`]).
struct InputAdmission<'a> {
    /// The verb's input class: which gates ask it.
    class: Input,
    verb: &'a str,
    ctx: &'a SessionCtx,
    /// The session's local id, the self-feed floor's key.
    session: u64,
    /// The request names its session: a cross write cannot self-loop, so the
    /// self-feed floor does not ask it.
    is_cross: bool,
    /// What the request writes: a text verb's argument tail after its leading
    /// options, a frame's body.
    args: &'a [u8],
    /// `unread=ok`: write behind unread input anyway.
    unread_ok: bool,
    /// A malformed leading option, answered here so a halted session says
    /// `ERR halted` whatever the rest of the line looks like.
    usage: Option<String>,
}

/// [`admit_with`] for a request with no words of its own past its leading
/// options (a key, a `feed-bin`/`paste-bin` frame): `Ok` carries the unread-input
/// gate's warning for the reply, `Err` the refusal that is the whole reply.
fn admit(a: InputAdmission<'_>) -> Result<Option<String>, String> {
    admit_with(a, || Ok(())).map(|((), warning)| warning)
}

/// THE INPUT ADMISSION, one order for every input request — a text verb or a
/// `feed-bin`/`paste-bin` frame — each gate asked as the verb's class says, the
/// request's own `words` (`send`'s and `turn`'s [`input_words`]) at the usage
/// step. `Ok` carries what `words` answered and the unread-input gate's warning
/// for the reply, `Err` the refusal that is the whole reply.
///
/// 1. The fleet halt (`Input::PTY`): `ERR halted <reason>`, a transient class
///    beside `ERR busy`, from every scope, Owner and Bridge included (the fleet
///    hold sits behind `Scope::Bridge` so the halted party cannot lift it). `post`,
///    `inbox seen`, `meta set`, `lease` and every read stay answerable.
/// 2. The self-feed floor (`Input::FLOORED`, self-targeted only): a per-session
///    token bucket, so a raw client cannot drive an output→observe→write storm on
///    its own session (`SelfGovernor` binds only drivers linking `aterm-agent`;
///    this binds everyone). A cross write cannot self-loop.
/// 3. Turn arbitration (`Input::LEASED`): while a `turn` or another connection's
///    hard drive lease holds the session, `ERR busy <holder>`; `cmd_turn` re-checks
///    under the lease lock, so this is the fast fail. `signal` is exempt.
/// 4. The edit hold (amacs §6.7): while the session's program waits on an `edit`
///    ticket, the verbs that type into it ([`crate::control_edit::hold::holds_verb`])
///    answer `ERR busy editing ticket=<t> view=<v>`, for every principal. `signal`
///    and every read stay answerable; the App verbs that type are asked against the
///    terminal on screen ([`dispatch_app`]).
/// 5. The request's own usage refusal (a malformed leading option), then its own
///    `words` (a `cause=`'s authority and what it names, a `turn`'s options),
///    after the halt: a halted session says `ERR halted` whatever the rest of the
///    line is, and `words` is not asked.
/// 6. The unread-input gate (`input_stall`, `Input::WRITES`): refused only where a
///    write is known to be harmful (a published stall, a stopped job); a slow read
///    is written and warned on the reply (owner, 2026-09-29). Below the usage
///    step, so a malformed request into a program that is not reading is told
///    what is wrong with it.
///
/// After authorization, so an unauthorized caller learns nothing of the target's
/// state, and after the licence fence, so a refused attempt is still a newer
/// input boundary. Before the `if=` guard and the idempotency claim, so a refusal
/// evaluates no guard and consumes no `id=` sequence. The keyboard never passes
/// this seam: a halt and a stall refuse drivers, never the person at the glass.
fn admit_with<T>(
    a: InputAdmission<'_>,
    words: impl FnOnce() -> Result<T, String>,
) -> Result<(T, Option<String>), String> {
    if a.class.contains(Input::PTY)
        && let Some(refusal) = crate::fabric::hold_refusal(a.ctx)
    {
        return Err(refusal);
    }
    if !a.is_cross
        && a.class.contains(Input::FLOORED)
        && !crate::inject_floor::allow(a.session, a.args.len().max(1))
    {
        return Err(RATE_REFUSAL.to_string());
    }
    if a.class.contains(Input::LEASED)
        && let Some(held) = a
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(|l| l.write_block(crate::metrics::now_us(), serving_connection()))
    {
        return Err(format!("ERR busy {held}\n"));
    }
    if let Some(refusal) = crate::control_edit::hold::refusal(a.verb, "", Some(a.session), None) {
        return Err(refusal);
    }
    if let Some(usage) = a.usage {
        return Err(usage);
    }
    let words = words()?;
    match crate::input_stall::gate(a.ctx, a.class, a.verb, a.args, a.unread_ok) {
        crate::input_stall::Admission::Refuse(refusal) => Err(refusal),
        crate::input_stall::Admission::Warn(warning) => Ok((words, Some(warning))),
        crate::input_stall::Admission::Open => Ok((words, None)),
    }
}

/// What a `send`'s or `turn`'s own words say, past its leading options
/// ([`input_words`]).
struct InputWords {
    /// The session log's epoch, read before the cause was validated
    /// ([`crate::control_input_ack::InputMark::within_epoch`]); `None` for a verb
    /// the log does not record as input.
    epoch: Option<u32>,
    /// The record or mail the input says it acted on.
    cause: Option<aterm_slog::ShortId>,
}

/// THE REQUEST'S OWN WORDS, asked at the input admission's usage step
/// ([`admit_with`]), before the exactly-once claim, so a refusal consumes no
/// `id=`: a leading `cause=@<off>|<seq>` (phase 3A, rulings R6 and R7), checked
/// against what THIS session holds ([`cause_of`]), and `turn`'s own option
/// grammar and patterns with a `cause=` among its options (R6's any-position
/// form), checked as a leading one is and taken once (both is
/// [`crate::session_log::CAUSE_USAGE`]). A refusal decided inside the turn would
/// come after the claim and burn the `id=`: the corrected retry would answer `ERR
/// in-doubt` though nothing was typed (`control_session::turn_preflight`).
fn input_words(
    scope: Scope,
    verb: &str,
    rest: &str,
    leading: Option<crate::session_log::CauseRef>,
    ctx: &SessionCtx,
) -> Result<InputWords, String> {
    let epoch = crate::session_log::InputVerb::of(verb).map(|_| {
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slog()
            .log()
            .ep()
    });
    let cause = leading
        .map(|raw| cause_of(scope, verb, raw, ctx))
        .transpose()?;
    if verb != "turn" {
        return Ok(InputWords { epoch, cause });
    }
    let cause = match control_session::turn_preflight(rest)? {
        None => cause,
        Some(_) if cause.is_some() => return Err(crate::session_log::CAUSE_USAGE.to_string()),
        Some(raw) => Some(cause_of(scope, verb, raw, ctx)?),
    };
    Ok(InputWords { epoch, cause })
}

/// What a `cause=` names, checked: an edge needs read authority on the target
/// first, since the check's answer would otherwise be an oracle on its mail
/// offsets, then the cause must name what this session holds
/// ([`crate::session_log::validate_cause`]).
fn cause_of(
    scope: Scope,
    verb: &str,
    raw: crate::session_log::CauseRef,
    ctx: &SessionCtx,
) -> Result<aterm_slog::ShortId, String> {
    if !scope.is_owner_class() && !cross_session_authorized(scope, "text", ctx) {
        log_denial(
            AUDIT_SUBSYSTEM,
            &format!("{verb} cause= -> {}", ctx.self_id.as_str()),
            aterm_containment::mode_or_containment(),
            "cause= reads what the target holds; edge lacks read",
        );
        return Err("ERR denied\n".to_string());
    }
    crate::session_log::validate_cause(ctx, raw)
}

/// Serve one session verb on its resolved target, returning the full response
/// (including any trailing data rows): the op-scope gate, then the input gates,
/// then the verb's arm.
///
/// [`dispatch_session`] has resolved `target`: the connection's own session for a
/// flagless or `@.` line, else the named session, whose access is gated here via
/// `cross_session_authorized`.
fn serve_session_verb(
    head: &Head<'_>,
    form: &Resolved,
    target: &Target,
    // The session id of the tab on screen, if any (already resolved by the caller),
    // so an explicit `@<sid>` for that tab takes the App input seam
    // (see `InputRoute`).
    front_active_session: Option<u64>,
    cx: &DispatchCx<'_>,
    // Artifact verbs park their handoff here, since it must outlive this `String` body
    // through the bounded socket write. Published paths also wait for the client's
    // ACK; in-memory bytes release after flush.
    reply_retention: &mut ReplyTail,
) -> String {
    let Head {
        selector,
        verb,
        rest,
    } = *head;
    let DispatchCx {
        store,
        scope,
        proxy,
        sock_dir,
        ..
    } = *cx;
    // The dispatch target as the caller resolved it: no selector (or `@.`) is the self
    // tuple; otherwise the named session, with its miss and ambiguity already answered.
    let is_cross = !matches!(selector, None | Some(Selector::SelfTok));
    let (term, master, session, self_ctx) = (&target.0, target.1, target.2, &target.3);
    let ctx: &SessionCtx = self_ctx;
    // What the verb does to the session's input, which decides the input gates below.
    let class = form.row.input_class();
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
    // Where this request's input goes, chosen once: every event it delivers takes it.
    let route = InputRoute::of(class, is_cross, targets_front, session);
    // The App seam, for a route that takes it, carrying the write's completion
    // for the session log (`control_input_ack`).
    let front = |ev, pin, completion: Option<crate::control_input_ack::InputCompletion>| {
        post_input_reply_to_observed(proxy, vec![ev], pin, completion.map(|c| c.into_mark()))
    };

    // Observe a scoped caller's front input consumer before the op gate. A native
    // Settings tab has no modal `OverlayKind`, yet its reducer consumes key/mouse/paste
    // events and can persist default-off security knobs, so the main-thread
    // observation adds an escalation: Settings -> ConfigWrite, Settings ▸ Harness and
    // any transient overlay -> Owner-only. Defence in depth: a scoped caller's
    // flagless input is pinned to its session (`front_routed`), which never reaches a
    // native reducer. Owner and cross-session requests skip this hop. A failed hop is
    // an authorization failure (fail closed), except a stalled main thread's refusal
    // (`front_surface_authority`).
    let front_surface =
        if !is_cross && !scope.is_owner_class() && class.contains(Input::FRONT_DRIVING) {
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
    let front_escalation = front_drive_escalation(scope, class, front_surface);

    // Op-scope gate, exhaustive and fail-closed, on the effective op
    // (`dispatch_authorized`): the base op escalated by the argument for the indirect
    // seams (`invoke <clipboard/config action>`, `open prefs|settings`), so a plain
    // WriteInput edge cannot tunnel to ClipboardWrite/ConfigWrite; the escalated op
    // replaces the base op, so a fine-op edge for exactly that action still passes.
    // `rest` is the raw argument here (json-flag stripping comes later).
    //
    // Self path (no selector, or `@.`): Owner passes before any lookup; an Edge runs a verb only
    // if its token authorizes the effective op against the now-active session.
    //
    // Cross path (`@other`): additionally, an Owner reaches siblings (same trust
    // domain) and a scoped Edge needs a `decide_edge`-permitted token against the
    // target's table (default deny).
    if is_cross {
        if !dispatch_authorized(scope, form, verb, rest, ctx) {
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
            || dispatch_authorized(scope, form, verb, rest, ctx),
            |need| scope_holds_need(scope, need, ctx),
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
    // the target's own context, so roster and resolve answer for the real process and
    // `session_ended` for the target; `session` binds the host, so a sid from elsewhere
    // is refused.
    let host = GuiHost::with_fleet(session, term, Some(proxy), store, ctx);

    // `--json` read mode (`json_ask`, from the table's `json` column): a JSON-emitting
    // row's flag is taken off `rest` and the request goes to its `*_json` emitter, so
    // the text fall-through never sees it; a row with no JSON form refuses the flag;
    // every other row reads the token as an ordinary argument. The op-scope gate above
    // already authorized `verb`.
    match json_ask(form, verb, rest) {
        JsonAsk::No => {}
        JsonAsk::Refuse(refusal) => return refusal,
        JsonAsk::Emit(body) => {
            return match verb {
                // `text --json [trim] [tail=<n>|rows=<a>-<b>]`: the tail is parsed; an
                // unknown token is the `TEXT_USAGE` line (a span naming no row, `ERR bad
                // rows`), as in the text form.
                "text" => match control_query::text_args(&body) {
                    Ok(args) => control_query::cmd_text_json_looked(term, ctx, args),
                    Err(err) => err,
                },
                // `screen` is ALWAYS styled JSON; accept `screen --json` for symmetry.
                "screen" => control_query::cmd_screen_styled_json(term, &ctx.frame, &body),
                "cursor" => control_query::cmd_cursor_json(term),
                "dims" => control_query::cmd_dims_json(term, session, proxy),
                "blocks" => control_selection::cmd_blocks_json(&host, session, &body),
                "edges" | "grants" => control_session::cmd_edges_json(ctx),
                // `log --json [<node>] [since= ep=] …`: one object per record.
                "log" => control_session::cmd_log_json(ctx, &body),
                // `sys … --json`: schema 1, `kind: sys.<view>`, one shape per figure.
                // It reads its own flag, so it takes the request whole.
                "sys" => crate::sys_host::cmd_sys(
                    store,
                    session,
                    term,
                    master,
                    scope.is_owner_class(),
                    rest,
                ),
                // A JSON-emitting row this dispatch has no emitter for answers one
                // `ERR` line, which reads the same under any framing.
                _ => no_handler(verb),
            };
        }
    }

    // Take a leading `id=<key>` off the verbs that accept one (`Input::KEYED`) and a
    // leading `if=<re>` guard off those that accept one (`Input::GUARDED`), in either
    // order, so every arm parses its usual tail. Done after the op-scope gate (which
    // reads the raw `rest` for `invoke`/`open` escalation) and before the halt gate,
    // self-feed floor and turn arbitration, which must refuse without consuming the
    // key's sequence or evaluating the guard. A malformed option is carried
    // (`leading.refusal`) and answered below the halt gate.
    let (leading, rest) = control_input::take_leading_options(class, rest);
    let idem_key = leading.idem;
    let as_person = leading.as_person;
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
    // driven key is fenced like the same physical key. The fence is recorded on the
    // session, not hopped to the main thread: every window presenting it applies it
    // before it judges the output this write causes (`crate::licence_fence`), so a
    // write to a background session, or under a dialog, is never refused for it.
    let fenced_write = leading.guard.is_some()
        || leading.if_gen.is_some()
        || leading.if_fp.is_some()
        || leading.if_human.is_some();
    if let Some(keep_hand) = control_input::control_fence(class, verb, rest, fenced_write) {
        ctx.licence.fence(keep_hand);
    }

    // The input admission (`admit_with`): the fleet halt, the self-feed floor, turn
    // arbitration, the edit hold, a malformed leading option, the request's own
    // words (a `cause=` on `send`/`turn`, a `turn`'s options: `input_words`), then
    // the unread-input gate, each as the verb's class asks, in the one order every
    // input request passes. The `if=<re>` guard is compiled next, outside the
    // terminal lock and bounded like `await match`'s: below the halt (a halted
    // session says `ERR halted`, never `ERR badregex` or `OK skipped`) and above the
    // idempotency claim, so neither a refusal nor a bad pattern consumes an `id=`
    // sequence.
    let (words, warning) = match admit_with(
        InputAdmission {
            class,
            verb,
            ctx,
            session,
            is_cross,
            args: rest.as_bytes(),
            unread_ok: leading.unread_ok,
            usage: leading.refusal,
        },
        || input_words(scope, verb, rest, leading.cause, ctx),
    ) {
        Ok(admitted) => admitted,
        Err(refusal) => return refusal,
    };
    let InputWords {
        epoch: input_log_epoch,
        cause,
    } = words;
    let guard = match control_input::compile_guard(leading.guard.as_deref()) {
        Ok(guard) => guard,
        Err(reply) => return reply,
    };
    let fence = control_input::InputFence {
        guard: guard.as_deref(),
        generation: leading.if_gen,
        fp: leading.if_fp,
        human: leading.if_human,
        foreground: None,
        guard_at_cursor: false,
        entering_command: false,
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
                session,
                control_session::log_actor(scope, ctx),
                input,
                cause,
                false,
            )
            .within_epoch(input_log_epoch)
            .acting(leading.act)
            .typed_by(ctx, serving_caller(scope, ctx))
        });
    let completion = || input_mark.as_ref().map(|mark| mark.completion());
    // An `in-doubt` mark wakes the session log's readers.
    let log_wake = || ctx.watchers.ring_watchers();
    let idem = idem_key.as_deref();
    let resp = crate::pty_idem::guarded_waking(ctx, scope, verb, idem, &log_wake, || match verb {
        // `text [trim] [tail=<n>|rows=<a>-<b>]`: the arm parses its tail, so an unknown
        // modifier is `ERR usage` rather than a silent full grid.
        "text" => match control_query::text_args(rest) {
            Ok(args) => control_query::cmd_text_opt(term, &ctx.frame, args),
            Err(err) => err,
        },
        // The LOSSLESS styled-screen read (keystone): full per-cell colour +
        // resolved decorations + cursor + dims + seq as one JSON frame. Always
        // styled-JSON (no plaintext variant) — `--json` is implied.
        "screen" => control_query::cmd_screen_styled_json(term, &ctx.frame, rest),
        "cursor" => control_query::cmd_cursor(term),
        "cell" => control_query::cmd_cell(term, rest),
        "search" => control_query::cmd_search(term, rest, &ctx.rewrap_gauge),
        // `edges`/`grants`: this session's inbound capability edges (EdgeTable rows). An
        // observer of the authority surface, gated `ReadScreen` like other reads;
        // cross-session reads a sibling's table through the same resolution and gate.
        "edges" | "grants" => control_session::cmd_edges(ctx),
        // `inbox`: the session's fabric mailbox: the bare listing (`Lines`),
        // `inbox get <id>` (a byte body) and `inbox seen <id>` (a status line). `Read`
        // class, with `seen` needing `WriteInput` (its own table row), so a read-only
        // edge can read mail but not mark it handled. `seen` and a parked `get @<off>`
        // record a row the bridge acts on, and the record wakes the session's watchers.
        "inbox" => crate::fabric::cmd_inbox_verb(ctx, rest),
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
            crate::fabric::cmd_post(ctx, rest, None)
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
        "ready" => control_session::cmd_ready(term, store, session, ctx, rest),
        // `await <idle|seq|match|gone|block>`: block until the Observation Kernel
        // latches the predicate. Unlike `wait` (OSC-133-only), `idle`/`match`/`gone`/`seq`
        // work for alt-screen agent TUIs. It registers a subscriber, so it wakes on
        // output and at the idle deadline without polling.
        // `await consent` reads the instance's consent posture, not this terminal, so it
        // parks on the consent state's publications instead of a `WatcherSpec`; split
        // here so the kernel path keeps its grammar and `term`-only inputs. It ends with
        // the session as every other `await` does.
        "await" if rest.split_whitespace().next() == Some("consent") => {
            let exited = control_session::exit_probe(store, session);
            let exited = || exited(store);
            crate::control_privacy::cmd_await_consent(proxy, scope, session, ctx, &exited, rest)
        }
        // `await sys <predicate>` reads the MACHINE, not this terminal: the
        // machine terms (cpu, memory, pressure, swap, the tab's load…) park on
        // the vitals snapshot (`sys_host`); `free…`/`disk=…` alone on the disk
        // board the disk watch publishes, a latched wait that may register a
        // demand for free space (`control_disk`).
        "await" if rest.split_whitespace().next() == Some("sys") => {
            crate::sys_host::cmd_await_sys(term, store, session, scope, rest)
        }
        // `await momentum <floor>` reads the typing momentum of the window hosting the
        // session (main-thread App state, like `trail status`), then parks on the
        // kernel's `MomentumBelow` timer.
        "await" if rest.split_whitespace().next() == Some("momentum") => {
            let read = || typing_momentum_reading(proxy, session);
            control_session::cmd_await_momentum(term, store, session, ctx, rest, &read)
        }
        "await" if rest.split_whitespace().next() == Some("edit") => {
            crate::control_edit::await_edit(proxy, scope, rest)
        }
        "await" => control_session::cmd_await(term, store, session, ctx, rest),
        // `task` / `task mark …`: the session's task record (a worker's), read
        // off the timeline leaf; the mark is Owner-only (its table form).
        "task" => crate::task_record::cmd_task(ctx, session, scope, rest),
        // `result [full]`: the worker's result, the snapshot its record names
        // (`task_record::cmd_result`).
        "result" => crate::task_record::cmd_result(ctx, session, rest),
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
            // authorized it); Owner-class callers are credited to no session. And who
            // calls, for the ledger's `by=`: the connection as the exits ledger names it.
            let driver = turn_driver(scope, ctx);
            let by = control_session::turn_by(scope, driver.as_ref());
            // Who the session log names for the turn: the connection.
            let actor = control_session::log_actor(scope, ctx);
            let mark = crate::control_input_ack::InputMark::at_epoch(
                ctx,
                session,
                actor,
                crate::session_log::InputVerb::Turn,
                cause,
                true,
            )
            .within_epoch(input_log_epoch)
            .typed_by(ctx, serving_caller(scope, ctx));
            // Both phases and every typed key take the request's one route, so paste and
            // Return cannot take different paths when a selector names the visible tab.
            // Each is written under the supervisor fence ([`under_fence`]) and noted in
            // the session log once its receipt says it was accepted.
            let applied = |ev: InputEvent| {
                let ev = std::cell::Cell::new(Some(ev));
                fenced_phase(&|| {
                    ev.take().is_some_and(|ev| {
                        matches!(
                            deliver(route, class, front, term, ctx, ev, Some(mark.completion())),
                            Ok(InputOutcome::Ok)
                        )
                    })
                })
            };
            let paste = |text: &str| {
                applied(InputEvent::Paste(
                    control_input::paste_text(text),
                    PasteFraming::AtDrain,
                ))
            };
            let press = |name: &str| parse_key(name).is_some_and(applied);
            control_session::cmd_turn(
                term,
                store,
                session,
                rest,
                ctx,
                &control_session::TurnIo {
                    paste: &paste,
                    press: &press,
                    key: &applied,
                    momentum: &momentum,
                    driver,
                    by,
                    actor,
                    cause,
                    mark: Some(&mark),
                },
            )
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
        // `as=person` (a development build's seam, `control_input::PERSON_SEAM`):
        // the write goes in as a person's gesture, admitted as every input above.
        "send" | "key" if as_person => control_input::person_input(proxy, session, verb, rest),
        "send" | "key" if fence.is_armed() => {
            if verb == "key" {
                control_input::cmd_key_fenced_observed(term, ctx, &fence, rest, completion())
            } else {
                control_input::cmd_send_fenced_observed(term, ctx, &fence, rest, completion())
            }
        }
        // The input verbs build their event with the verb's own parser and deliver it by
        // the request's route (`InputRoute`): the source-blind `App::input` seam on the
        // event loop for the tab on screen or the Owner's flagless request, so bytes
        // match human input and the seam's side effects (blink reset, viewport snap,
        // selection clear, the typing-momentum hints, the `video ... keys` ledger) run
        // for it; or, for a session not on screen, the tracked egress straight to the
        // resolved `(term, &ctx.sink)` on the control thread, which has no App UI to
        // touch. One arm per verb, so a malformed body answers one usage line whatever
        // the route: a cross-session drive is not a second grammar.
        // Each write carries its completion, so the session log notes it by its
        // author once its receipt says it was accepted (`control_input_ack`).
        "send" => control_input::input_reply_to_str(deliver(
            route,
            class,
            front,
            term,
            ctx,
            InputEvent::KeySequence(control_input::send_bytes(rest)),
            completion(),
        )),
        "key" => parse_key(rest).map_or_else(
            || control_input::KEY_USAGE.to_string(),
            |ev| {
                control_input::input_reply_to_str(deliver(
                    route,
                    class,
                    front,
                    term,
                    ctx,
                    ev,
                    completion(),
                ))
            },
        ),
        "ctrl" => parse_ctrl(rest).map_or_else(
            || control_input::CTRL_USAGE.to_string(),
            |ev| {
                control_input::input_reply_to_str(deliver(
                    route,
                    class,
                    front,
                    term,
                    ctx,
                    ev,
                    completion(),
                ))
            },
        ),
        "feed" => match control_input::feed_bytes(rest) {
            Ok(bytes) => {
                let n = bytes.len();
                let ev = InputEvent::KeySequence(bytes);
                control_input::bytes_reply(
                    deliver(route, class, front, term, ctx, ev, completion()),
                    n,
                )
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
        "reset" => {
            let reply = crate::manual_reset::cmd_reset(session, term, ctx, rest);
            // The reset's `modes` mark, when it ran before the reply (a queued
            // one the reader runs later rides that reader's output wake).
            if ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slog_take_wake()
            {
                ctx.watchers.ring_watchers();
            }
            reply
        }
        // The parser's refusal is the reply on every route: a bad modifier answers
        // `ERR bad modifier …` naming what a report can carry, not the generic usage
        // line. A background target with tracking off has no selection UI, so its
        // wheel moves the target's viewport instead (`cross_mouse`). On the seam a
        // gesture keeps the copy-on-select fence: a scoped caller's release never
        // auto-copies (`apply_copy_on_select_policy`, a no-op for Owner).
        "mouse" if route == InputRoute::Background => cross_mouse(term, ctx, session, proxy, rest),
        "mouse" => match control_input::parse_mouse(rest) {
            Ok(ev) => {
                let ev = control_input::apply_copy_on_select_policy(scope, ev);
                control_input::input_reply_to_str(deliver(
                    route,
                    class,
                    front,
                    term,
                    ctx,
                    ev,
                    completion(),
                ))
            }
            Err(refusal) => refusal,
        },
        "paste" => control_input::input_reply_to_str(deliver(
            route,
            class,
            front,
            term,
            ctx,
            control_input::paste_event(rest),
            completion(),
        )),
        "focus" => match control_input::parse_focus(rest) {
            Some(focused) => control_input::input_reply_to_str(deliver(
                route,
                class,
                front,
                term,
                ctx,
                InputEvent::Focus(focused),
                completion(),
            )),
            None => "ERR usage: focus <in|out>\n".to_string(),
        },
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
        "image" => control_media::cmd_image(proxy, rest, sock_dir, is_cross.then_some(session))
            .into_body_retaining(reply_retention),
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
        // A background `resize` does NOT go through the seam: `seam_egress` emits no
        // bytes for `Resize`, and `App::input`'s Resize arm resizes the WINDOW (every
        // tab + the GPU swapchain). A background target has no window to echo to, so we
        // replicate ONLY the term+PTY pair (`echo_to_window: false` semantics) on the
        // TARGET, never the active window/framebuffer. The px form names the WINDOW and
        // a driven px resize only ever reaches the FRONT one, so a background target is
        // refused by name ([`control_input::RESIZE_PX_FRONT_ONLY`]) instead of falling
        // into the cell parser's `ERR bad args`.
        "resize" => match route {
            InputRoute::Seam { pin: Some(_) } => front_routed_resize(proxy, session, rest),
            InputRoute::Seam { pin: None } => control_input::cmd_resize(proxy, rest),
            InputRoute::Background => match control_input::cross_resize_px_refusal(rest) {
                Some(refusal) => refusal,
                None => cross_resize(term, master, ctx, session, Some(proxy), rest),
            },
        },
        // A background `scroll` also bypasses the seam (`ScrollView` emits no bytes; the
        // viewport move lives in `App::input`). It applies the `ScrollIntent` DIRECTLY to
        // the TARGET term's viewport and reports `OK <offset> <max>` — the SAME wire
        // shape as the seam's. `select` is already cross-correct (mutates the target
        // term + fires a repaint keyed by target id).
        "scroll" => match route {
            InputRoute::Seam { pin: Some(_) } => front_routed_scroll(term, proxy, session, rest),
            InputRoute::Seam { pin: None } => control_input::cmd_scroll(term, proxy, rest),
            InputRoute::Background => cross_scroll(term, rest),
        },
        "dims" => control_query::cmd_dims(term, session, proxy),
        "lines" => control_query::cmd_lines(term),
        // `offscreen`: rows an alt-screen app scrolled off its top (Claude Code repaints
        // in place, so `lines` stays 0). Reads the target's own archive, so it is
        // cross-session correct.
        "offscreen" => control_query::cmd_offscreen(term, &ctx.frame, rest),
        // `mainscreen`: force the target's primary screen when a killed app left the
        // alternate one up (its last frame goes to `offscreen` first). Host-based on the
        // resolved sid like `select`. It takes no argument; a tail is not consent to
        // move a running app's screen. Under OpenConsole the host leaves first and the
        // engine from its forwarded switch (`alt_screen_resync`).
        "mainscreen" => refuse_stray_args(verb, rest).unwrap_or_else(|| {
            crate::alt_screen_resync::mainscreen_via_host(term, master)
                .unwrap_or_else(|| control_query::cmd_mainscreen(&host, session))
        }),
        "line" => control_query::cmd_line(term, rest),
        "modes" => control_query::cmd_modes(term),
        // `custody` -> WHO last took the reading position or the highlight, by name.
        // Read-side: it reports the engine's own custody record and no screen content.
        "custody" => control_query::cmd_custody(term),
        "title" => control_query::cmd_title(term),
        "cwd" => control_query::cmd_cwd(term),
        "blocks" => control_selection::cmd_blocks(&host, session, rest),
        // `blocktext <id> [trim]`: the arm's helper checks the grammar and passes the
        // id and `trim` to the shared emitter, which frames the reply after the
        // terminal is released; `trim` drops trailing blank rows as `text trim` does.
        "blocktext" => control_query::cmd_blocktext_args(rest, |id, trim| {
            control_selection::blocktext(&host, session, id, trim)
        }),
        "wait" => control_selection::cmd_wait(&host, session, rest),
        "colors" => control_query::cmd_colors(term),
        "select" => control_selection::cmd_select(&host, session, rest),
        "selection" => control_selection::cmd_selection(&host, session),
        "copy" => control_selection::cmd_copy(&host, session),
        // `cast` reads the target session's own asciicast recorder, not the shared
        // renderer, so it is correct cross-session (no `is_cross` guard). `cast frames
        // [count=N]` expands the recording into keyframe screens (line-framed like
        // `text`), replayed under the target's seam policy; bare `cast` is the
        // byte-framed asciicast.
        "cast" if rest.split_whitespace().next() == Some("frames") => {
            let policy = crate::app_render::pty_resize_policy(master);
            control_session::cmd_cast_frames(
                ctx,
                policy,
                rest.strip_prefix("frames").unwrap_or("").trim(),
            )
        }
        // `cast drift [max_runs=<k>] [rows=<n>] [seed=…]` replays the target's recording
        // against the target's engine (line-framed); both locks are the target's, so it
        // is cross-session correct. The host posture (its seam policy, and whether
        // its console host forwards an alternate-screen leave) is asked before either
        // lock. A wait-lane verb (its table form): it settles 60 ms and folds the
        // recording.
        "cast" if rest.split_whitespace().next() == Some("drift") => {
            let posture = aterm_control::cast_drift::Posture {
                policy: crate::app_render::pty_resize_policy(master),
                host_resyncs_alt_screen: crate::alt_screen_resync::host_resyncs(
                    aterm_pty::conpty_host(master),
                ),
            };
            control_session::cmd_cast_drift(
                term,
                &ctx.frame,
                &ctx.cast,
                posture,
                rest.trim_start().strip_prefix("drift").unwrap_or("").trim(),
            )
        }
        "cast" => control_session::cmd_cast(&ctx.cast),
        // `temporal [tick]` reconstructs the target's screen at a past instant from its
        // own temporal recorder, cross-session correct like `cast`. It also reads the
        // target's engine (its own lock, never nested with the recorder's) for resizes
        // not yet on the spine (`pending_resizes=`).
        "temporal" => control_session::cmd_temporal(ctx, term, rest),
        // `history [<n>] [since=<id>]`: the session's turn ledger (what was driven and
        // what settled). Named `history` so it never collides with the `turn` driver.
        "history" => control_session::cmd_history(ctx, rest),
        // `meta`: read/write the target session's user metadata (title, description,
        // icon). The write sub-forms need WriteInput (the table's `meta` form). On an
        // actual change the arm (not the headless-testable handler) posts
        // [`meta_change_wakes`] through [`serve_meta_wired`], the code the wire test
        // runs: `Wake::MetaChanged` so tab labels repaint, then the task face a proven
        // self-claim's recorded act tells (`Wake::HarnessFact`); the change's
        // `meta-change` record wakes an `events` watcher, which drains `EVENT <sid>
        // meta …` at once.
        // `meta set|unset supervisor` is answered first: it is Owner-writable and may
        // bind to this connection, which only this seam knows. A non-Owner `questions`
        // or `autopilot` write is refused next (each decides whether the supervisor
        // answers for the person); an Owner's falls through to `cmd_meta`'s closed set
        // ([`serve_meta`]). An `autopilot` word that moved rings the window's host from
        // the store write itself (`session_timeline::apply_meta_value`), and the
        // `Wake::MetaChanged` below repaints the session's footer (its chip) with the
        // rest.
        "meta" => serve_meta_wired(
            rest,
            term,
            &|holder: &str| prove_self_claim(holder, master),
            session,
            ctx,
            store,
            scope,
            &mut |wake| {
                let _ = proxy.send_event(wake);
            },
        ),
        // `timeline`: the target session's event timeline (the lifecycle twin of
        // `history`), a pure observer of the ctx ring, cross-session correct.
        "timeline" => control_session::cmd_timeline(ctx, rest),
        // `log`: the target session's SESSION LOG (session > run > turn > step),
        // a pure reader of the ctx's timeline lock like `timeline`.
        "log" => control_session::cmd_log(ctx, rest),
        // `brief`: the target session's token-budgeted rollup; `brief all` (the
        // instance's) was escalated to Owner-only by `argument_escalation`.
        "brief" => control_session::cmd_brief_with_jobs(
            ctx,
            store,
            rest,
            crate::sys_host::brief_jobs_line(session),
        ),
        // `sys [<view>] …`: what the target's tab, aterm and the machine use
        // now, from the workers' copy of the vitals snapshot (a pull when it is
        // too old). The Owner-only forms were escalated by `argument_escalation`;
        // machine-wide rows print for an owner-class caller only.
        "sys" => {
            crate::sys_host::cmd_sys(store, session, term, master, scope.is_owner_class(), rest)
        }
        // `mark`: an attributed, rate-limited CLAIM in the target session's log
        // (design §7.4). Write-class, so an edge needs `WriteInput`; `by=` is the
        // connection's; it never acts ([`dispatch_mark`]).
        "mark" => dispatch_mark(ctx, scope, rest),
        // `resizes`: the target session's resize ledger (what re-gridded it, the runs,
        // and whether the app drew over displaced content), from its own engine and
        // timeline, cross-session correct.
        "resizes" => crate::resize_ledger::cmd_resizes(ctx, term, rest),
        // `status`: the target session's Subject + classified Status. A main-thread hop,
        // since the classifier is `App` state owned by the event loop.
        "status" => control_media::cmd_session_status(proxy, session, rest),
        "resources" => {
            let reply = crate::resource_jobs::command(ctx, scope.is_owner_class(), rest);
            if reply.starts_with("OK") {
                let _ = proxy.send_event(Wake::Vitals);
            }
            reply
        }
        // `harness policy`: the target session's `[harness]` policy, the facts a
        // main-thread hop (the policy cell, the host, the session's words), the
        // rules file read and the line made here. An edge reads only `effective=`.
        "harness" => crate::harness_policy_verb::cmd(rest, scope.is_owner_class(), || {
            control_media::call_main(proxy, |tx| Wake::ReadHarnessPolicy { session, reply: tx })
                .map_err(|e| e.to_string())
                .and_then(|facts| facts)
        }),
        // `subscribe` names its sessions after the verb, and the lane takes a
        // selectorless one over before dispatch: a leading selector is its usage,
        // one line, since this connection stays open.
        "subscribe" => SUBSCRIBE_USAGE.to_string(),
        // A session row with no arm here fails closed.
        _ => no_handler(verb),
    });
    // A text the window harness typed with `send` and submitted with `key
    // enter` is a turn of the harness's in the ledger, as its `turn` is.
    control_session::ledger_typed_input(term, ctx, scope, verb, rest, &resp);
    // Input observation is completed by the exact event's nonempty accepted
    // receipt, including ordered-writer completion; a reply is not byte evidence.
    // Stamp the `content_seq` baseline on a successful input-write reply so a driver
    // can read a seq, write, then `await seq <that+1>` race-free (` seq=<n>` before the
    // trailing newline; ERR untouched). A warned write then adds
    // ` warn=input-unread input_bytes=<n> input_wait_ms=<ms>` on the first line
    // (`input_stall::with_warning`).
    crate::input_stall::with_warning(stamp_input_seq(class, resp, term), warning.as_deref())
}

/// Append ` seq=<content_seq>` to a successful input-write reply. The seq is the target
/// grid's content baseline right after the write is dispatched, so `await seq <n+1>`
/// waits for the output it causes. Only a verb whose `class` has `Input::STAMPS_SEQ`
/// with an `OK` reply is stamped, before the trailing newline so the reply stays
/// one line.
fn stamp_input_seq(class: Input, resp: String, term: &Arc<Mutex<Terminal>>) -> String {
    if !class.contains(Input::STAMPS_SEQ) || !resp.starts_with("OK") {
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

/// What a request does with a `--json`/`json` token ([`json_ask`]).
#[derive(Debug, PartialEq, Eq)]
enum JsonAsk {
    /// No JSON form is asked for: the token is absent, or the verb reads it as an
    /// ordinary argument (`send --json`, `search json`, `meta set title json`).
    No,
    /// The verb has no JSON form: this `ERR` line is the whole reply.
    Refuse(String),
    /// The verb answers JSON: its arguments without the token, so each emitter's
    /// positional parse runs as the text form's does.
    Emit(String),
}

/// What `verb rest` does with a `--json`/`json` token, read off the
/// [`Json`](aterm_types::control_verbs::Json) column of its form's row — the column
/// the client frames the reply by ([`aterm_types::control_verbs::framing_of`]), so
/// the two cannot disagree. Only a `Refused` or `Emits` row reads the token as the
/// flag: a payload verb's `json` is data and reaches its handler verbatim.
fn json_ask(form: &Resolved, verb: &str, rest: &str) -> JsonAsk {
    use aterm_types::control_verbs::{Json, json_flag};
    if !json_flag(rest) {
        return JsonAsk::No;
    }
    match form.row.json {
        Json::None => JsonAsk::No,
        Json::Refused => JsonAsk::Refuse(format!("ERR json: not supported for {verb}\n")),
        Json::Emits => JsonAsk::Emit(take_json_flag(rest).1),
    }
}

/// Whether `rest` carries the `--json` / `json` flag
/// ([`aterm_types::control_verbs::json_flag`], the rule [`json_ask`] reads), and
/// `rest` without that token, so each positional parse runs unchanged. Also for a
/// reader handed a line the dispatch's [`json_ask`] did not see: a closed
/// session's `log`, read from the closed-session index or the disk journal.
pub(crate) fn take_json_flag(rest: &str) -> (bool, String) {
    use aterm_types::control_verbs::json_flag;
    // `json_flag` of one token is whether that token is the flag.
    let kept = rest
        .split_whitespace()
        .filter(|token| !json_flag(token))
        .collect::<Vec<_>>()
        .join(" ");
    (json_flag(rest), kept)
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
#[path = "control_handoff_tests.rs"]
mod control_handoff_tests;
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
/// `@<sid> hold off`), read by the request grammar ([`control_request::head`])
/// and answered by [`dispatch_hold_verb`] from `scope`, as [`dispatch_owner`]
/// answers it: the seam another module's test runs a command it read off a
/// record through. A line whose verb is not `hold` is answered `ERR`.
#[cfg(test)]
pub(crate) fn hold_line_for_test(store: &Store, line: &str, scope: Scope) -> String {
    let Head {
        selector,
        verb,
        rest,
    } = control_request::head(line);
    if verb != "hold" {
        return format!("ERR not a hold line: {line:?}\n");
    }
    dispatch_hold_verb(rest, selector.as_ref(), scope, store)
}

/// One `meta <rest>` line as the dispatch answers it from the Owner token
/// ([`serve_meta`], with no proxy to wake): the session's terminal, ctx and
/// pool id, as [`dispatch_session`] resolved them; no wake is sent. Gated on `test`
/// alone: [`serve_meta`] is built on every platform, so a test of the `meta`
/// arm builds wherever the arm does.
#[cfg(test)]
pub(crate) fn meta_line_for_test(
    store: &Store,
    term: &Arc<Mutex<Terminal>>,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
) -> String {
    serve_meta(
        rest,
        term,
        &|holder: &str| prove_self_claim(holder, -1),
        session,
        ctx,
        store,
        Scope::Owner,
        || {},
    )
}

/// [`meta_line_for_test`] served on connection `conn`, as the wire serves it
/// (a claim bound to that connection records its acts without a kernel look),
/// with the wakes the arm posts on a change, in order: the arm's own code
/// ([`serve_meta_wired`]), its proxy a list.
#[cfg(test)]
pub(crate) fn meta_wakes_for_test(
    store: &Store,
    term: &Arc<Mutex<Terminal>>,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
    conn: Option<u64>,
) -> (String, Vec<Wake>) {
    meta_wakes_proving_for_test(store, term, session, ctx, rest, conn, &|holder: &str| {
        prove_self_claim(holder, -1)
    })
}

/// [`meta_wakes_for_test`] with the kernel's look at a self-claim's holder
/// answered by `prove` — what a request on a connection the claim is not
/// bound to is admitted by ([`control_session::cmd_meta_harness_act`]).
#[cfg(test)]
pub(crate) fn meta_wakes_proving_for_test(
    store: &Store,
    term: &Arc<Mutex<Terminal>>,
    session: u64,
    ctx: &SessionCtx,
    rest: &str,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<crate::self_claim::Proven>,
) -> (String, Vec<Wake>) {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERVING_CONNECTION.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(SERVING_CONNECTION.with(|s| s.replace(conn)));
    let mut wakes = Vec::new();
    let reply = serve_meta_wired(
        rest,
        term,
        prove,
        session,
        ctx,
        store,
        Scope::Owner,
        &mut |wake| wakes.push(wake),
    );
    (reply, wakes)
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

#[cfg(test)]
#[path = "control_wire_goldens_tests.rs"]
pub(crate) mod wire_goldens;
