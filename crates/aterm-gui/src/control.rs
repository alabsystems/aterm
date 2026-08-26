// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live introspection CONTROL SOCKET (aterm introspection control protocol v1).
//!
//! A background thread binds a Unix domain socket and serves newline-delimited
//! text requests so an out-of-process intelligence can read the live screen
//! (text/cursor/cell/search), drive the shell (send/key), snapshot the pixels
//! (image), drive text selection (select — plain ranges plus the gesture
//! forms `word`/`line`/`block`/`extend` — and selection/copy), and resize the
//! engine + PTY — all against the SAME running terminal the window presents.
//! This is aterm introspecting itself: no OS screen recording, just the
//! engine's own grid and renderer.
//!
//! Threading: text/cursor/cell/search read the shared [`Terminal`] directly;
//! send/key/resize poke the PTY master fd. The `image` verb needs the renderer,
//! which lives on the MAIN thread, so this thread cannot render. Instead it
//! pushes an [`ImageReq`] onto a shared queue, wakes the event loop with
//! [`Wake::Control`], and blocks on the reply channel; the main thread drains
//! the queue, renders, writes the PNG, and replies with the frame dimensions.
//!
//! In a window, `image` (and the SIGUSR1 snapshot) renders the current terminal
//! through the SAME renderer and the SAME splice/composite sequence the window
//! present uses — a re-render from the retained application input, not a copy of
//! a presented destination. It captures the live cursor state, INCLUDING
//! the current cursor blink phase and the hollow unfocused-cursor override — so
//! a focused blinking session may legitimately capture a frame with no cursor
//! pixels. This boundary does not observe compositor visibility or scanout.
//! Headless sessions instead render a semantic frame and pin the blink phase on
//! (always deterministic). For deterministic cursor state regardless of phase,
//! use the `cursor` verb (row, col, visible, style).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

use aterm_uds::{CtlListener, CtlStream};

use aterm_containment::log_denial;
use aterm_core::terminal::{CursorStyle, Terminal};
use aterm_digest::Sha256;
use aterm_session::sink::InputEpoch;
use aterm_session::{EdgeToken, Op, SessionId, decide_edge};
use serde::Deserialize;
use winit::event_loop::EventLoopProxy;

use crate::control_auth::{self, AuthOutcome};
use crate::input::{
    Delivery, Egress, EgressMode, InputEvent, InputOutcome, ScrollIntent, Source, seam_egress,
};
use crate::session_store::Store;
use crate::subscribe::{self, InstanceStreams, PushOptions, PushScopes, Requested, Subscribers};
use crate::{SessionCtx, Wake, term_lock};

/// Read-only screen introspection serializers (the SACRED AI-reads-the-screen
/// path). Child module of `control`; verbs are dispatched as
/// `control_query::cmd_*` from [`handle`]. The file lives flat at
/// `src/control_query.rs` (sibling of `control.rs`), so `#[path]` points at it.
#[path = "control_query.rs"]
mod control_query;
// Re-export the two query serializers that out-of-module callers reach through
// the stable `crate::control::NAME` path (`crate::subscribe`), so the path keeps
// resolving after the move.
pub(crate) use control_query::{gather_styled_frame, serialize_styled_frame, visible_row};
// The shared full-history search (the GUI ⌘F find + the `search` verb both call it) and
// its config-driven index depth cap, reached through the stable `crate::control::NAME`
// path from `app_search`/`app_config`/`main`.
#[cfg(test)]
pub(crate) use control_query::search_full_history;
pub(crate) use control_query::{
    search_full_history_direction, search_full_history_point, set_search_max_lines,
};
// The combined single-lock wrapper is test-only at the crate level now: prod
// callers split gather/serialize so JSON rendering runs off the mutex.
#[cfg(test)]
pub(crate) use control_query::styled_frame_payload;
// Test-only serialization of the process-global search depth cap, so the cap-mutation
// test and any scrollback-searching test (in other modules) never interleave.
#[cfg(test)]
pub(crate) use control_query::search_cap_test_guard;

/// Input-injection verbs + their parsers (key/ctrl/send/feed/signal/mouse/paste/
/// focus/resize/scroll/tab). Child module of `control`; dispatched as
/// `control_input::cmd_*` from [`handle`]. The file lives flat at
/// `src/control_input.rs` (sibling of `control.rs`), so `#[path]` points at it.
#[path = "control_input.rs"]
mod control_input;
// Re-export the parsers that out-of-module callers reach through the stable
// `crate::control::NAME` path (`crate::input`), so the path keeps resolving.
pub(crate) use control_input::{parse_ctrl, parse_key, parse_mouse};

/// Selection / copy / block verbs (`select`/`selection`/`copy`/`blocks`/
/// `blocktext`/`wait`). They now live in the winit-free `aterm-control` crate,
/// typed over a [`SessionHost`](aterm_control::SessionHost); the alias keeps the
/// `control_selection::cmd_*` spelling [`handle`] dispatches by.
use aterm_control::selection as control_selection;
// Re-export the smart-selection gesture helpers (`app_mouse.rs`'s double/triple-
// click), which reach through the stable `crate::control::NAME` path. They take a
// bare `&mut Terminal` off the GUI's own lock guard — no host — so the crate move
// left their signatures alone.
pub(crate) use aterm_control::selection::{select_line, select_word, word_cols};
// The response-framing primitives moved WITH the verbs (which reached them via
// `super::`, so a duplicate here would be a second escape to drift from). Bound
// at their pre-move visibilities, so every `crate::control::NAME` and `super::`
// caller in this crate resolves unchanged.
pub(crate) use aterm_control::wire::{json_escape, json_escape_into, pct_encode, visible_char};
use aterm_control::wire::{json_ok, json_str_field};

/// This crate's [`SessionHost`](aterm_control::SessionHost): a borrowed adapter
/// over the target [`handle`] has already resolved. Child module of `control`;
/// the file lives flat at `src/control_host.rs` (sibling of `control.rs`), so
/// `#[path]` points at it.
#[path = "control_host.rs"]
mod control_host;
use control_host::GuiHost;

/// The MAIN-THREAD conformance gate: the verb matrix against a [`GuiHost`] holding
/// a real `EventLoopProxy`, which no `#[test]` can build (see the module). A child
/// module of `control` because `control_host` is private to it — putting the gate
/// anywhere else would mean widening the shipped API for a test. Behind the
/// off-by-default `control-conformance` feature, so the shipped GUI never carries
/// the suite. The file lives flat at `src/control_redraw_conformance.rs`, so
/// `#[path]` points at it.
#[cfg(feature = "control-conformance")]
#[path = "control_redraw_conformance.rs"]
pub(crate) mod control_redraw_conformance;

/// The platform CLIPBOARD layer (`pbcopy`/`pbpaste`, X11 PRIMARY). It could NOT
/// follow the selection verbs into a winit-free crate — it is AppKit/x11rb/Win32
/// code with callers all over the GUI — so it kept its own file under the name
/// that describes it. The file lives flat at `src/clipboard.rs` (sibling of
/// `control.rs`), so `#[path]` points at it.
#[path = "clipboard.rs"]
mod clipboard;
// Re-export `pbcopy`/`pbpaste` (GUI OSC-52 path, `main.rs`, menu Paste), which
// reach through the stable `crate::control::NAME` path, so those paths keep resolving.
pub(crate) use clipboard::{pbcopy, pbpaste};
// The test-only clipboard stub (see its doc): reached as
// `crate::control::PBPASTE_STUB` by tests that must make "the clipboard says X"
// deterministic without touching the real system clipboard.
#[cfg(test)]
pub(crate) use clipboard::PBPASTE_STUB;
// Its PRIMARY twin, for tests driving the Linux middle-click paste path.
#[cfg(all(test, target_os = "linux"))]
pub(crate) use clipboard::PRIMARY_STUB;
// `pbpaste_owned` (the X11 non-blocking own-selection read, for the OSC-52
// query arm — which must never block inside the terminal lock on a
// foreign-owned selection), `primary_get`/`primary_get_owned` (PRIMARY-selection
// paste: the blocking foreign read for the middle-click worker + its instant
// own-slot fast path) and `primary_set` (PRIMARY own on selection-release /
// OSC 52 'p'). ONE grouped re-export: a second standalone `pbpaste_owned`
// line is E0252 (duplicate import) on the Linux build. Linux-only — on macOS
// all four would be unused imports.
#[cfg(target_os = "linux")]
pub(crate) use clipboard::{pbpaste_owned, primary_get, primary_get_owned, primary_set};

/// Media-capture verbs (`image`/`image read`/`window`/`chrome`). Child module of
/// `control`; dispatched as `control_media::cmd_*` from [`handle`]. The file
/// lives flat at `src/control_media.rs` (sibling of `control.rs`), so `#[path]`
/// points at it.
#[path = "control_media.rs"]
mod control_media;
// Re-export `image_payload`: `control_query::styled_image_json` reaches it through
// `super::image_payload`, which now resolves to this sibling module's serializer.
pub(crate) use control_media::image_payload;

/// Session-graph + capability-authority verbs (`sessions`/`family`/`ready`/
/// `cast`/`edges`/`grant`/`revoke`/`whoami`). Child module of `control`;
/// dispatched as `control_session::cmd_*` from [`handle`]. The file lives flat at
/// `src/control_session.rs` (sibling of `control.rs`), so `#[path]` points at it.
#[path = "control_session.rs"]
mod control_session;

/// The containment subsystem name used in audit denials from this socket.
const AUDIT_SUBSYSTEM: &str = "control_socket";

/// The currently-ACTIVE tab's engine + PTY master, shared with the GUI so the
/// control socket's verbs follow tab switches instead of being pinned to the
/// session that happened to exist at startup. The GUI updates this on every tab
/// switch / open / close (`App::sync_active_session`); each request resolves the
/// current target from it ([`resolve_active`]). This changes ONLY which session a
/// verb targets — the auth gates (peer-uid + per-launch token) are untouched.
pub struct ActiveSession {
    pub term: Arc<Mutex<Terminal>>,
    pub master: i32,
    /// The active session's stable id, so a control verb that DIRECTLY mutates the
    /// engine (scroll/select) can request a repaint of the right tab.
    pub id: u64,
    /// The active session's fabric context (sink + edge table + identity), so the
    /// op-scope gate and the writer verbs resolve the live tab's sink/table.
    pub ctx: Arc<SessionCtx>,
}

/// Shared optional front-terminal handle, cloned into the control thread.
///
/// A native tab is a real front content target but has no PTY, `Terminal`, or
/// `SessionCtx`.  `None` represents that state honestly. Owner app/meta requests
/// are classified before consulting this handle; session requests return the
/// typed no-terminal error unless they carry an explicit live session selector.
pub type ActiveHandle = Arc<Mutex<Option<ActiveSession>>>;

/// One coherent, live `dims` observation assembled on the main thread.
///
/// The terminal grid (`rows`/`cols`) belongs to the selected session. Everything
/// else belongs to the deterministic window selected for that session: its own
/// font metrics, composed renderer frame, and raw presentation surface. Keeping
/// all three extents in one snapshot makes the resize/zoom remainder visible
/// instead of hiding it behind the historical startup-only cell-size estimate.
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
    /// THE DPI SCALE the selected window's geometry was derived from
    /// (`WindowState::scale`: its `scale_factor()`, or a `--scale` /
    /// `$ATERM_FORCE_SCALE` pin). Every other field here is downstream of it —
    /// `font_px` is `round(FONT_PX·scale)` under the auto-font, `pad`/`pad_top`
    /// are `round(logical·scale)`, and `head` is the synthetic band's remainder
    /// at that scale — so without it a reader can only INFER the DPI by dividing
    /// back out of the font, which the explicit-font and clamped paths make
    /// wrong. A headless boot is NOT a missing window: it still selects one and
    /// reports that window's own record (`geometry` = `headless`).
    ///
    /// When NO window holds the session — `geometry` = `detached`, the branch
    /// that reads cell size, pad, `pad_top`, head and `font_px` off the live
    /// SHARED backend — this is `App::detached_scale` instead: a `--scale` /
    /// `$ATERM_FORCE_SCALE` pin if one is set (the only scale a headless boot
    /// ever has), else the scale of the window that backend is currently tuned
    /// to, else the front window's, else the lowest stable window id's, and a
    /// literal `1.0` only when no window exists at all. So a detached record
    /// still names the DPI the pad and font beside it were derived from, rather
    /// than asserting 1.0 over them.
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
    /// The GPU swapchain layer's live presentation state, where the platform has
    /// such a layer: `(contentsGravity, contentsScale, contentsAreFlipped)`.
    ///
    /// The live-resize ANCHOR is the one thing in the resize path that no
    /// in-process instrument can score — the artifact it prevents is a compositor
    /// rescale that happens after aterm's frame reaches the WSI. This reports
    /// whether the anchor is in effect, which is both the check and the regression
    /// guard: the layer is owned by `raw-window-metal`, whose docs reserve the right
    /// to overwrite common `CALayer` properties.
    pub(crate) layer_presentation: Option<(String, f64, bool)>,
}

/// Snapshot the current active engine + PTY master + id for one request.
/// Poison-recovery (matches `term_lock`): a panicked GUI thread must not wedge
/// introspection.
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

/// Principal/target projection consumed by the drift-free native-control model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NativeControlPrincipal {
    Owner,
    Edge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NativeControlTarget {
    App,
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "privileged native meta routes are modeled and Tier-1 bound before their first shipping verb"
        )
    )]
    Meta,
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
        NativeControlTarget::App | NativeControlTarget::Meta => match principal {
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

/// Resolve the one session-class input whose unqualified form is also a real
/// front-content gesture. A human paste is delivered to whatever owns the
/// focused front view; the control twin must do the same when that owner is a
/// native Editor and there is deliberately no `ActiveSession` mirror.
///
/// `None` means retain the established terminal/session resolver. Only an
/// owner's bare/self paste may take the app-authority lane: explicit selectors
/// continue to mean an exact terminal session, and an Edge remains bound to the
/// session capability that minted it. The authority verdict is the shipping
/// [`native_control_decision`] classifier's existing App case, not a parallel
/// control-only allowlist.
///
/// `text` is a THUNK, not a `String`: the guard below reads only verb/selector/
/// front_has_terminal, so on the dominant terminal-front path the whole payload —
/// up to `MAX_FEED_BIN` (256 KiB) for `paste-bin` — used to be UTF-8-scanned,
/// heap-copied, and dropped unread before the caller re-derived it as a borrowed
/// `Cow`. Deferring the materialization into the one arm that consumes it keeps
/// the single guard as the only source of truth (no duplicated authority
/// predicate at the two call sites) and is otherwise byte-for-byte identical.
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
            NativeControlDecision::WithoutSession => Ok(InputEvent::Paste(text())),
            NativeControlDecision::Denied => Err("ERR denied\n"),
            NativeControlDecision::ResolveSession
            | NativeControlDecision::NoActiveTerminal
            | NativeControlDecision::NoSuchSession => Err("ERR invalid control target\n"),
        },
    )
}

/// What a connection is authorized to do, resolved at handshake.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The per-instance god token: full control of the active session (all verbs,
    /// incl. grant/revoke). Same-uid clients with the instance token keep zero-friction
    /// full power (no regression to aterm-ctl). Because the instance token is the
    /// launcher's per-process authority, an Owner connection may ALSO reach SIBLING
    /// sessions in the same process (the same-uid / same-trust-domain god token);
    /// a scoped `Edge` connection needs an explicit edge per target (see
    /// `resolve_target`).
    Owner,
    /// An edge token scoped to exactly one op against the connection's HANDSHAKE
    /// target (`decide_edge` semantics). ONLY the presented [`EdgeToken`] is carried —
    /// authority is ALWAYS re-derived from it per request (every verb runs
    /// `decide_edge`/`authorize` against the RESOLVED target's table+nonce), so a
    /// token authorizing session B says nothing about session C, and the global
    /// ActiveHandle swinging `@.` to another session cannot grant stale power. The
    /// connect-time op is deliberately NOT stored: caching it invited a confused
    /// deputy (whoami over-reporting / audit mis-attribution) where the cached op
    /// drifted from what the token actually authorizes against the now-active session.
    Edge(EdgeToken),
}

/// The verb table: one row per control verb, the single source of truth for its
/// op-class. `required_op` is a lookup into this. Two tests bind the other
/// representations to it. `catalog_and_verb_table_agree` binds the help catalog.
/// `every_dispatched_verb_is_in_the_table` binds the dispatch router. So a verb
/// cannot be added to the router without being classified and documented here (CI
/// fails otherwise). Op-class meanings. `ReadScreen` is a pure observer or
/// view-state control, and subscribe is its push face. `WriteInput` injects the
/// human input vocabulary the driven program observes, and turn/spawn/close/tab/
/// invoke are the app-drive members. `Signal` is the out-of-band signal class.
/// `ConfigWrite` (settings) rewrites durable on-disk config and `ClipboardWrite`
/// (copy) exfiltrates the selection to the OS pasteboard — each a distinct fine op,
/// so an inherited read/write edge (only the three base ops) cannot reach them.
/// `None` marks an Owner-only privilege verb (sessions/grant/revoke/whoami/dial)
/// or a build/meta verb (version/update/help), all gated before the op check.
/// The op-class a verb needs, from [`VERBS`]. `None` for an Owner-only/meta verb
/// (gated before the op check) OR an UNKNOWN verb (absent from the table → the
/// dispatch then returns `ERR unknown verb`). The `turn` write additionally
/// demands read authority for scoped callers, enforced in its dispatch arm.
fn required_op(verb: &str) -> Option<Op> {
    use aterm_types::control_verbs::OpClass;
    aterm_types::control_verbs::spec(verb).and_then(|s| match s.op {
        OpClass::Read => Some(Op::ReadScreen),
        OpClass::Write => Some(Op::WriteInput),
        OpClass::Signal => Some(Op::Signal),
        // `settings` (durable config) and `copy` (clipboard exfil) each map to their
        // OWN fine op — split out of Write/Read so an inherited write/read edge (which
        // carries only the three base ops) cannot reach them; only Owner can.
        OpClass::ConfigWrite => Some(Op::ConfigWrite),
        OpClass::ClipboardWrite => Some(Op::ClipboardWrite),
        OpClass::Owner => None,
    })
}

/// A few verbs' EFFECTIVE authority depends on their ARGUMENT, not just the verb
/// keyword — an INDIRECT seam by which a plain `WriteInput` edge could otherwise reach
/// a fenced fine op that the verb-only [`required_op`] gate cannot see:
///
///   * `invoke <action>` is verb-level `WriteInput`, but each menu action is classified
///     EXHAUSTIVELY by [`menu::MenuAction::invoke_authority`] (the compiler forces a
///     decision per variant, so a new privileged action cannot fall through to the benign
///     base gate). The CLIPBOARD actions (`Copy`/`Paste`/`SelectAll`) move the selection
///     onto / off the OS pasteboard — the SAME exfil/inject boundary as the `copy` verb —
///     so they demand `ClipboardWrite`; CONFIG actions that raise the durable-config
///     surface demand `ConfigWrite` — `Preferences` opens Settings ▸ Manual,
///     `ToggleSettings` raises the native security-settings tab). `OpenPalette` (a GATEWAY that
///     can dispatch EVERY action) and the update RE-EXEC twins (`SoftwareUpdate`/
///     `ApplyUpdate`, the MenuAction faces of the already-Owner-only `update` verb) demand
///     OWNER-ONLY — no fine op suffices. Font zoom / splits / fullscreen / tab & window
///     navigation are runtime-only view state (no persist), so they stay the benign
///     `WriteInput`. The wire name is the exact `MenuAction` Debug token (case-sensitive;
///     `controls menu` prints them, `action_by_name` matches them).
///   * `open prefs` / `open settings` (`AuxTarget::Prefs`) and the versioned
///     `open app settings /route` face raise the native Settings tab whose
///     keystrokes flip default-OFF security knobs, so they demand `ConfigWrite`.
///   * `act app/v1 ...` names a stable view, but that view's kind is intentionally
///     resolved on the main thread after socket authorization. Treat the generic
///     semantic-action gateway as `ConfigWrite` rather than trusting a caller-authored
///     action prefix to claim it is not a Settings action.
///     `open menu`/`open palette` (`AuxTarget::Menu`, the palette gateway) and `open
///     update` (`AuxTarget::Update`, the software-update surface) demand OWNER-ONLY, the
///     same as their `invoke` twins (about/front stay the benign `WriteInput`).
///
/// Returns the ESCALATED requirement when the argument reaches a fenced capability, else
/// `None` (the verb's base `required_op` already gates the argument). The dispatch
/// re-checks this against the SAME per-session predicate as the base gate, so an
/// inherited write edge cannot tunnel to ClipboardWrite/ConfigWrite — or, for the palette
/// GATEWAY and the staged-update RE-EXEC twins, to Owner-only power — through these seams.
///
/// The classification of `invoke <action>` is COMPILER-EXHAUSTIVE: it decodes the wire
/// name to a [`menu::MenuAction`] and reads its [`menu::MenuAction::invoke_authority`], so
/// a newly-added menu action cannot silently fall through to the benign base gate — it
/// must be classified in `menu.rs` or the workspace fails to build.
fn escalated_op(verb: &str, rest: &str) -> Option<Escalation> {
    use crate::menu::{InvokeAuthority, MenuAction};
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
        // Classify by the TARGET TOKEN ONLY, never the whole tail: `cmd_open` acts on
        // the first token and treats a trailing `close` as the dismiss flag, so
        // `open prefs close` DOES close the Settings tab. Parsing `rest.trim()`
        // here made `AuxTarget::parse("prefs close")` (an exact match) return `None`,
        // collapsing the escalation to the base `WriteInput` — a scoped edge could then
        // dismiss a privileged overlay through the `close` variant it is fenced from
        // opening. Escalate on `open <target> [close]` identically to `open <target>`.
        "open"
            if matches!(
                aterm_types::app_inspection::parse_open_app(rest),
                Ok(aterm_types::app_inspection::OpenAppRequest::Settings { .. })
            ) =>
        {
            // The versioned native Settings app is the same durable-config
            // capability as the compatibility `open prefs` face.  Classify the
            // full frozen grammar here, before the main-thread open hop, so a
            // WriteInput edge cannot reach `/security` (or any other route) via
            // `open app settings ...`.
            Some(Escalation::Op(Op::ConfigWrite))
        }
        // `act app/v1 ...` is a gateway into a stable native view.  The view kind
        // is deliberately resolved only on the main thread, after this socket
        // policy runs, so the control thread cannot safely distinguish a Settings
        // action from an Editor/Markdown action by trusting caller-authored action
        // spelling.  Require ConfigWrite for the gateway.  Owner behavior and the
        // v1 wire grammar/replies stay unchanged; a plain WriteInput edge can no
        // longer dispatch `settings/control/...` through the semantic seam.
        "act" if aterm_types::app_inspection::parse_act(rest).is_ok() => {
            Some(Escalation::Op(Op::ConfigWrite))
        }
        // A CONNECTED spawn (`spawn connected=… of=<sid>`) MINTS session-connection
        // authority over an arbitrary origin — Owner-only, the belt-and-suspenders
        // fence beside the App-target gate (design §5.3/§6): no fine op expresses
        // "may create standing authority between sessions". A plain `spawn [cwd=…]`
        // mints nothing and keeps its base WriteInput class.
        // Classified with `split_quoted_tokens` -- the SAME tokenizer
        // `parse_spawn_args` uses -- not `split_whitespace`. A fence that splits
        // differently than the parser it fences has a spelling that walks past it:
        // `spawn "connected=controller" of=<sid>` parses as a connected spawn while
        // a whitespace split sees a token beginning with `"`. Unparseable input
        // escalates too, so the fence is never weaker than the parser.
        "spawn"
            if control_media::split_quoted_tokens(rest)
                .is_none_or(|tokens| tokens.iter().any(|t| t.starts_with("connected="))) =>
        {
            Some(Escalation::OwnerOnly)
        }
        // Every surface rendering the AGGREGATED connection graph carries the same
        // Owner gate as `flows` (design §5.3): `open connections` raises the map,
        // `controls connections` reads it as text, `window connections` captures it
        // — all disclose the whole fabric to a scoped edge otherwise. Classified by
        // the TARGET TOKEN only (the `open prefs close` rule above), so the
        // `close`-variant dismiss escalates identically.
        "open" | "controls" | "window"
            if rest.split_whitespace().next() == Some("connections") =>
        {
            Some(Escalation::OwnerOnly)
        }
        "open" => match crate::app_introspect::AuxTarget::parse(
            rest.split_whitespace().next().unwrap_or(""),
        ) {
            // The Settings tab flips default-OFF security knobs => ConfigWrite.
            Some(crate::app_introspect::AuxTarget::Prefs) => Some(Escalation::Op(Op::ConfigWrite)),
            // Raising the PALETTE (a gateway to every action), the SOFTWARE-UPDATE
            // overlay (the re-exec surface), or the in-grid TAB MENU (a gateway to
            // the session context actions, connection rows included) matches the
            // OwnerOnly `invoke` twins, so a scoped edge cannot open any of them
            // through the `open` seam.
            Some(
                crate::app_introspect::AuxTarget::Menu
                | crate::app_introspect::AuxTarget::TabMenu
                | crate::app_introspect::AuxTarget::Update,
            ) => Some(Escalation::OwnerOnly),
            _ => None,
        },
        // `meta` is verb-level `Read` (the bare form is a pure metadata readout),
        // but `meta set`/`meta unset` WRITE the session's user metadata — a
        // session-scoped driving assertion like `lease`, so the sub-forms demand
        // `WriteInput` (NOT ConfigWrite: nothing durable on disk is rewritten). A
        // read-only edge can inspect a session's metadata but never rename it.
        "meta" => match rest.split_whitespace().next() {
            Some("set" | "unset") => Some(Escalation::Op(Op::WriteInput)),
            _ => None,
        },
        _ => None,
    }
}

/// The escalated authority requirement an argument-bearing seam demands — EITHER a fine
/// [`Op`] an explicitly-granted edge can satisfy, OR OWNER-ONLY where no single op
/// expresses the requirement (a gateway to every action, or a process re-exec). Returned
/// by [`escalated_op`] and resolved by [`dispatch_authorized`]; Owner satisfies both arms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Escalation {
    /// Requires the fine op — `scope_holds_op` decides (an edge granted exactly this op
    /// passes; Owner always passes).
    Op(Op),
    /// Requires the per-instance Owner token — no fine op is sufficient (a scoped Edge,
    /// however privileged, is DENIED).
    OwnerOnly,
}

/// What currently owns controller-directed input in the front window.
///
/// This is an internal main-thread observation, not a wire type.  It extends the
/// former overlay-only query with the native Settings tab so the socket policy
/// cannot confuse "no modal overlay" with "ordinary terminal input" while the
/// process-singleton Settings app is focused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FrontControlSurface {
    /// No app/overlay consumes front input.
    None,
    /// A transient own-rendered overlay consumes input.
    Overlay(crate::overlay::OverlayKind),
    /// The process-singleton native Settings tab consumes input.
    NativeSettings,
    /// A native app other than Settings consumes input.
    OtherNative,
}

/// Whether `scope` may exercise `need` against `ctx`'s session — the op-level core
/// shared by the base op-scope gate and the indirect-seam [`escalated_op`] fence.
/// `Owner` (the per-instance process god token) always holds; an `Edge` must present a
/// [`decide_edge`]-permitted token for EXACTLY `need`, bound to THIS session's
/// table + current launch nonce (default-DENY). Checking against `ctx` (the RESOLVED
/// target — self or sibling) keeps the self and cross paths on one predicate.
fn scope_holds_op(scope: Scope, need: Op, ctx: &SessionCtx) -> bool {
    match scope {
        Scope::Owner => true,
        Scope::Edge(presented) => {
            let table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            decide_edge(&table, &presented, &ctx.self_id, need, &ctx.nonce).is_permitted()
        }
    }
}

fn scope_holds_escalation(scope: Scope, need: Escalation, ctx: &SessionCtx) -> bool {
    match need {
        Escalation::Op(op) => scope_holds_op(scope, op, ctx),
        Escalation::OwnerOnly => matches!(scope, Scope::Owner),
    }
}

/// `update [status|check]` handler — the control-socket face of the in-app updater
/// (mirrors the "Check for Updates" menu). `status` (default) reads the durable
/// markers only (instant, no I/O); `check` runs ONE synchronous check+stage (may block
/// for tens of seconds on network + disk). Cross-platform: off macOS the updater API is
/// inert, so this reports `enabled=false`. The source is `Source::resolve(None, None)`
/// (env + compiled default); a GUI `[update]` owner/repo override is not applied to a
/// manual check.
/// `help` / `verbs` — the self-describing protocol catalog: a server-authoritative
/// list of every verb this BUILD supports, its args, and the reply framing, so an AI
/// (or human) can discover the whole introspection interface from a running instance
/// without external docs. Non-sensitive global provenance like `version`, answered for
/// any authenticated scope before target resolution. Keep in sync with the dispatch
/// match + `aterm-ctl --help`.
fn cmd_help() -> String {
    // Emitted as "OK <count> …\n" + <count> body lines so aterm-ctl STREAMS the whole
    // catalog like other list verbs (controls/edges/sessions), not just the header.
    // Raw string: contains no `"#` sequence. One entry per line so it is greppable.
    // Fixed protocol HEADER (framing / cross-session / targeting rule / seeing
    // modes), then the per-verb catalog GENERATED from the one VERBS table in
    // aterm-types — so the catalog can never drift from the table (it IS the table).
    // The `--json` verb list is GENERATED from `JSON_CAPABLE_VERBS`, not retyped.
    // It was a FIFTH hand-maintained replica of that set (beside `framing_of`, the
    // emitter match, the `json_unsupported` allowlist, and a test's private copy),
    // and it had already drifted: `metrics` gained a `--json` form and this
    // sentence never learned about it. A generated sentence cannot drift.
    let json_verbs = aterm_types::control_verbs::JSON_CAPABLE_VERBS.join("/");
    let header = &format!(
        r#"# Framing: every reply is a STATUS line ("OK …" / "ERR …") OR raw CONTENT lines. Content
# verbs prefix "OK <count>" then <count> lines; aterm-ctl prints the content and consumes
# the OK line, so an EMPTY content reply means NO DATA — it is never an error (errors are
# always "ERR …"). Add "--json" to {json_verbs} for
# structured output; a read verb without a json form answers "ERR json: not supported".
# Cross-session: prefix a verb with "@<selector>" (e.g. "@s-ab12 text"). Same-user only;
# the per-launch token is auto-discovered by aterm-ctl. A sid hosted by ANOTHER same-user
# aterm instance is relayed to it transparently (owner connections): every session of every
# instance is addressable here. Discover peers with `sessions` (this instance) or the
# client-side `aterm-ctl ls` (all instances).
# TARGETING RULE: the @<sid> selects WHERE the verb runs. SESSION verbs (text/screen/
# image/turn/send/…) act on that session. APP-LEVEL verbs (window/chrome/controls/open/
# settings/tab/invoke/spawn) act on the resolved instance's FRONT window — the selector
# routes to the instance (cross-instance via relay), e.g. `@peer window` screenshots the
# peer's window, `@peer invoke NewTab` opens a tab in the peer.
#
# SEEING a terminal (yours or a peer's) — five modes, pick by need:
#   text / screen        what the PROGRAM wrote (plain rows / lossless styled grid)
#   image                what ATERM submitted (client frame: decorations, tab strip,
#                        overlays; `image plain` strips host bling for bare pixels;
#                        compositor visibility/scanout is outside this boundary)
#   subscribe            the LIVE movie (pushed frames; add every-frame for animations)
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
    for line in aterm_types::control_verbs::catalog_lines() {
        body.push_str(&line);
        body.push('\n');
    }
    let n = body.lines().count();
    format!("OK {n} aterm introspection protocol v1\n{body}")
}

#[cfg(test)]
mod help_tests {
    #[test]
    fn help_catalog_is_well_formed_and_lists_core_verbs() {
        let h = super::cmd_help();
        assert!(h.starts_with("OK "), "help must be an OK status reply");
        for v in [
            "version", "update", "help", "text", "screen", "cell", "cursor", "image", "window",
            "video", "chrome", "controls", "sessions", "whoami", "edges", "grants", "send", "key",
            "turn",
        ] {
            assert!(h.contains(v), "help catalog is missing verb {v:?}");
        }
        // Documents the framing so a consumer knows empty content = no data, not error.
        assert!(
            h.contains("ERR") && h.contains("EMPTY"),
            "help must document the OK/ERR/empty framing"
        );
        // The self-describing catalog is marketed as external-doc-free, so its axis
        // orders must match the impl: resize/cell/dims are all ROWS-first (r, c).
        assert!(
            h.contains("resize <r> <c>") && !h.contains("resize <c> <r>"),
            "resize catalog axis order must be rows-first (match parse_resize + dims + cell)"
        );
        assert!(
            h.contains("open app settings /about")
                && h.contains("open app settings /updates")
                && h.contains("open app markdown file:///abs/doc.md")
                && h.contains("open app editor file:///abs/doc.md")
                && h.contains("inspect app/v1 tabs")
                && h.contains("act app/v1 view <id> <key> <action> [value]"),
            "help must make every native tab-app entry point discoverable"
        );
    }
}

/// `update` / `update status` is a pure read of the updater's state, so it stays
/// AnyScopeMeta (answerable to any authenticated scope). `check` (stages a build over
/// network + disk) and `apply` (re-execs the process) MUTATE, so they are Owner-only —
/// the direct-verb twin of the OwnerOnly fence on `invoke ApplyUpdate`/`open update`.
/// Extracted as a pure predicate so the gate is unit-testable without an event loop.
fn update_is_owner_only_subcmd(rest: &str) -> bool {
    matches!(rest.trim(), "check" | "apply")
}

/// `aterm_update::installed_update_facts()` for the control verb, cached for
/// [`INSTALLED_FACTS_TTL`]: the probe spawns codesign/spctl/PlistBuddy, and the
/// status verb is AnyScopeMeta and polled — uncached it would spawn a helper
/// chain per poll and could pin every control lane behind a slow Gatekeeper
/// lookup. The cache is a pure observation (no authority rides on it).
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

fn cmd_update(rest: &str, scope: Scope, proxy: &EventLoopProxy<Wake>) -> String {
    let build = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
    // `update` is AnyScopeMeta so `""`/`status` (a pure read of updater state) answers
    // for any authenticated scope — but the MUTATING sub-commands must be Owner-only:
    // `check` stages a build (network + disk) and `apply` re-execs the process. Without
    // this gate a scoped edge holding only ReadScreen could `update check` then `update
    // apply` to force a re-exec — the direct-verb twin of the Owner-only `invoke
    // ApplyUpdate` / `open update` faces the menu path already fences. The in-session AI
    // drives with the OWNER token (flagless self-location), so "SEE via status + PRESS
    // via apply" is unaffected; only a non-owner peer/child edge is refused.
    if update_is_owner_only_subcmd(rest) && !matches!(scope, Scope::Owner) {
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
        "check" => Some(aterm_update::check_now(
            build,
            &aterm_update::Source::resolve(None, None),
        )),
        // PROOF-CARRYING DSU (RFC Rung 1): PRESS the button — apply a staged update
        // now. Introspectable by design, so an AI running IN the session (Claude Code)
        // can `update status` to SEE a staged build and `update apply` to apply it.
        // The reducer validates durable facts + dirty native state asynchronously.
        // This socket acknowledges only REQUEST delivery; claiming "applying" here
        // would be false when facts collection saturates or preflight rejects.
        "apply" => {
            if proxy.send_event(Wake::ApplyStagedUpdate).is_err() {
                return "ERR event loop gone\n".to_string();
            }
            return "OK apply requested; updater reducer will validate stage and preflight\n"
                .to_string();
        }
        other => {
            return format!("ERR usage: update [status|check|apply] (got {other:?})\n");
        }
    };
    let Some(mut st) = st else {
        return "OK enabled=false outcome=\"no updater on this platform\"\n".to_string();
    };
    // THE ACTIVATION LANE, AS THE LEDGER CANNOT SEE IT. A bundle newer than this
    // process under its own executable is staged IN MEMORY by the GUI reducer (it
    // writes no `ready.toml`), so a status read from disk alone answered
    // `staged_build=- … "up to date"` while Settings said "Update ready" and
    // `update apply` would act on it (2026-08-19 round-2 audit). Derive the same
    // fact from the same source the reducer uses — the verified installed bundle —
    // so the line says what the process is about to do.
    // The reducer's rule, exactly: an installed bundle newer than the process with a
    // usable sealed commit is the activation and it OUTRANKS any download on disk
    // (the reducer retires the download for it). The probe runs codesign, so it is
    // cached for a short while — a controller polling every second must not spawn
    // helpers every second.
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
        st.outcome = format!(
            "build {} is already installed on disk; activating it in place (ledger: {})",
            installed.build_number, st.outcome
        );
    }
    let staged_build = st
        .staged_build
        .map(|b| b.to_string())
        .unwrap_or_else(|| "-".to_string());
    let staged_version = st.staged_version.as_deref().unwrap_or("-");
    let staged_commit = st.staged_commit.as_deref().unwrap_or("-");
    // relaunch_ready: a strictly-newer build is staged and will apply on the next
    // launch (or immediately via `update apply`). The one-glance SEE for a controller.
    let relaunch_ready = st.staged_build.is_some_and(|b| b > st.current_build);
    // Self-healing ledger fields: failing=<consecutive>:<kind> (0 when healthy) and
    // the lifetime rescue-path count — so a driver can see a broken pipeline (or a
    // limping primary path) from one line, without parsing health.toml.
    let failing = if st.failing_checks == 0 {
        "0".to_string()
    } else {
        // The class of the STANDING acquisition streak — the streak this count
        // counts — never `failing_kind`, the most recent failure of ANY class:
        // an apply failure landing after two network failures rendered
        // `failing=2:apply`, splicing an acquisition count with an apply label
        // and sending the reader down the wrong lane (round-11 audit; the
        // apply lane has its own failing_applies=/apply-class tokens).
        let kind = if st.failing_checks_kind.is_empty() {
            st.failing_kind.as_str()
        } else {
            st.failing_checks_kind.as_str()
        };
        format!("{}:{kind}", st.failing_checks)
    };
    // A frozen ledger must not read as a live one: flag a last-completed-check
    // stamp older than any lane's worst legitimate quiet period (the anonymous
    // 30-min base × the 4-interval backoff ceiling × jitter ≈ 2.4 h; 4 h clears
    // it with margin). Healthy lines stay byte-identical — the token appears
    // only in the stale state, per the installable=/apply_refusal= precedent.
    const STALE_CHECK_AFTER_SECS: u64 = 4 * 3600;
    let stale_check = if aterm_update::rfc3339_older_than(&st.updated_at, STALE_CHECK_AFTER_SECS) {
        format!(" stale_check={}", st.updated_at)
    } else {
        String::new()
    };
    // commit= is the RUNNING binary's source commit (compile-time stamp);
    // staged_commit= is the staged build's (from its release manifest). Together a
    // controller can bind both sides of an update to exact repo commits. The two use
    // DIFFERENT widths (short-12 stamp vs full-40 manifest) and the stamp may carry a
    // `-dirty` suffix, so `staged_is_same_commit=` is the canonical comparison —
    // computed here via `commit_matches` so no consumer re-implements prefix logic. It
    // reads true only when the staged build was built from the SAME source commit as the
    // running one (a churn/no-op relaunch rather than a real source change).
    let staged_is_same_commit =
        aterm_update::commit_matches(crate::build_info::GIT_COMMIT, staged_commit);
    let mut out = format!(
        "OK enabled={} current_build={} commit={} staged_build={} staged_version={} \
         staged_commit={} staged_is_same_commit={} relaunch_ready={} failing={} \
         failing_applies={} rescues={} persistent={}{stale_check} outcome={:?}\n",
        st.enabled,
        st.current_build,
        crate::build_info::GIT_COMMIT,
        staged_build,
        staged_version,
        staged_commit,
        staged_is_same_commit,
        relaunch_ready,
        failing,
        // Broken out of `failing=` on purpose: the acquisition streaks and the
        // apply streak answer different questions, and a line reading
        // `failing=0 failing_applies=7` is the exact state that used to be
        // indistinguishable from a healthy updater.
        st.failing_applies,
        st.rescues,
        st.is_failing_persistently(),
        st.outcome
    );
    // STRUCTURALLY UNABLE, in the same one-glance line. A copy run from the mounted
    // DMG, a Gatekeeper-translocated download, or a dev-marked build has no bundle
    // to replace: no check thread ever starts, so every field above stays at the
    // pristine default of a machine that will never update, and a controller reads
    // `enabled=true failing=0 persistent=false` as health. Emitted ONLY when false,
    // so a normal install's line is byte-identical to before and the abnormal state
    // is the loud one (2026-08-19 round-6 audit; Settings gained the same guard).
    if !st.installable {
        let line = out.trim_end_matches('\n');
        out = format!("{line} installable=false\n");
    }
    // THE APPLY LANE, in the same one-glance line. `failing_applies=` counts hard
    // failures only, and by design a REFUSAL (blocked/deferred/held) is not one —
    // which is how a machine could sit on `staged_build=<new> relaunch_ready=true
    // failing_applies=0` for hours while every single `update apply` was turned
    // away. `apply_refusal=` is that missing answer: the reason the apply lane
    // last declined, pct-encoded because it is prose on a single-line reply.
    // `apply_failure=` does the same for the streak `failing_applies=` counts but
    // never explained. Every token is emitted only when its value is non-empty, so
    // a healthy updater's line is byte-identical to before.
    if let Some(apply) = aterm_update::apply_lane_report(st.current_build) {
        // Every value here is prose read back from a ledger, so every value is
        // pct-encoded: an embedded space or newline would split this single-line
        // Status reply and desync every following read on a pipelined connection
        // (the same hazard `changelog=` below is encoded for).
        for (name, value) in [
            ("apply_refusal", apply.last_refusal.as_str()),
            ("apply_refusal_at", apply.last_refusal_at.as_str()),
            ("apply_failure", apply.last_failure.as_str()),
        ] {
            if !value.is_empty() {
                let line = out.trim_end_matches('\n');
                out = format!("{line} {name}={}\n", pct_encode(value));
            }
        }
    }
    // Fold the staged build's "what changed" notes into the SAME status line as a
    // pct-encoded `changelog=` token. `update` is Status-framed (the client reads
    // exactly ONE line), so the old trailing `changelog:\n<multi-line notes>` block
    // was silently dropped by the reader AND — on a persistent/pipelined connection —
    // read as the reply to the NEXT verb, desyncing every subsequent reply. Encoding
    // keeps the (possibly multi-line) notes on the single status line. Empty/absent
    // notes append nothing.
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

/// Interpret the handshake's hex as an edge token against the active session's
/// table. Returns the authorized op, the PRESENTED token (so a cross-session call
/// can re-check it against the TARGET's table via `decide_edge`), and any
/// folded-in inline verb, else None.
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

/// A request for the MAIN thread to render the live screen to a PNG.
///
/// The control thread fills the CONFINED target (a canonical dir + single
/// filename, already validated by `confine_image_path`), sends [`Wake::Control`],
/// then blocks on `reply`; the main thread renders and sends back
/// `(width, height)`. TOCTOU-1: passing the dir + filename (not a re-resolvable
/// path string) lets the writer `openat` the final component under a dir fd, so
/// no intermediate path component can be symlink-swapped after the check.
pub struct ImageReq {
    /// The confined image target (canonical `images/` dir + validated filename).
    pub target: control_auth::ConfinedImage,
    /// CLEAN capture: suppress ALL host-owned visual bling (cursor trail + LUMEN glow +
    /// sparkle-word decorations + the animated Scene) so the AI reads the bare terminal
    /// pixels. The bling lives in separate `RenderInput` layers, so a clean render is just
    /// those layers emptied — `image plain <name>`.
    pub clean: bool,
    /// Cross-session capture: render the window DISPLAYING this local session id
    /// (its active tab shows the session), not the frontmost window. `None` = the
    /// frontmost window, byte-identical to the pre-cross behavior. This is what
    /// lets one terminal SEE another's real rendered frame (`@<sid> image`) —
    /// decorations, tab strip, overlays and all — instead of failing closed.
    pub session: Option<u64>,
    /// `image --bytes`: return the PNG bytes OVER THE WIRE instead of writing the
    /// confined file — the only capture form a REMOTE driver (dial/TLS) can use,
    /// since a file path names the SERVER's filesystem. The encode worker then
    /// sends `Some(png)` in the reply and writes no file.
    pub want_bytes: bool,
    /// `image --meta`: compute and return the exact encoded-frame identity. Kept
    /// separate from `frame_metadata` so legacy captures pay no full-frame hash
    /// or retained-leaf inventory cost and preserve their historical wire reply.
    pub want_metadata: bool,
    /// Out-of-band identity of the exact encoded terminal/native/composite frame. The
    /// main thread initializes it only after the final RGBA pixels and every
    /// retained leaf/raster identity are known; the control worker reads it only
    /// after the image reply, so existing image reply payloads remain unchanged.
    pub frame_metadata: Arc<std::sync::OnceLock<ImageFrameMetadata>>,
    /// Linearizes a control-thread timeout against the final artifact-name
    /// publication. If timeout wins, the encode worker may finish its private
    /// temporary bytes but cannot make the requested path observable.
    pub cancel: CaptureCancellation,
    /// Channel the main thread (via the encode worker) replies on: the rendered
    /// `(width, height, png?)` — `png` is `Some` only in `--bytes` mode (else the
    /// PNG is ON DISK at the confined path), `Ok((0, 0, None))` when no window
    /// displays the target, or `Err` when the encode/write failed.
    pub reply: Sender<ImageReply>,
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
        test,
        aterm_spec::refines(
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
        test,
        aterm_spec::refines(
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

/// Behavior implemented by an exact artifact guard carried to the socket edge.
/// `prepare_write` revalidates the names an OK response is about to certify;
/// it also crosses irrevocable publication ownership before any OK bytes can
/// become visible. The guard itself remains live through write, flush, and the
/// client's explicit post-response acknowledgement.
pub(crate) trait WireRetention: Send {
    fn prepare_write(&mut self) -> Result<(), String>;
}

/// Type-erased ownership that remains live until a control reply is completely
/// consumed and explicitly acknowledged. Concrete guards retain and revalidate
/// exact file/directory handles.
pub(crate) struct ReplyRetention {
    guard: Option<Box<dyn WireRetention>>,
    phase: ReplyRetentionPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplyRetentionPhase {
    Queued,
    Prepared,
    Classified,
}

impl ReplyRetention {
    pub(crate) fn new(guard: impl WireRetention + 'static) -> Self {
        Self {
            guard: Some(Box::new(guard)),
            phase: ReplyRetentionPhase::Queued,
        }
    }

    #[cfg_attr(
        test,
        aterm_spec::refines(
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
        test,
        aterm_spec::refines(
            machine = "ArtifactReplyPublication",
            action = "AcknowledgePeer",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn acknowledge_peer_anchor(&mut self) {
        self.phase = ReplyRetentionPhase::Classified;
    }

    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ArtifactReplyPublication",
            action = "AcknowledgeFailed",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn acknowledge_failed_anchor(&mut self) {
        self.phase = ReplyRetentionPhase::Classified;
    }

    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ArtifactReplyPublication",
            action = "PrepareFailed",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn prepare_failed_anchor(&mut self) {
        self.phase = ReplyRetentionPhase::Classified;
    }

    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ArtifactReplyPublication",
            action = "WriteFailed",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn write_failed_anchor(&mut self) {
        self.phase = ReplyRetentionPhase::Classified;
    }

    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ArtifactReplyPublication",
            action = "AbortQueued",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn abort_queued_anchor(&self) {}

    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ArtifactReplyPublication",
            action = "ReleaseGuard",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    fn release_guard_anchor(&self) {}
}

impl Drop for ReplyRetention {
    fn drop(&mut self) {
        if self.phase == ReplyRetentionPhase::Queued {
            self.abort_queued_anchor();
            self.phase = ReplyRetentionPhase::Classified;
        }
        // Fire the refinement anchor only after the concrete handle/name guard
        // has actually dropped. A Drop body normally runs before its fields,
        // which would attach ReleaseGuard to a no-op and leave the real release
        // invisible to Tier-1 conformance.
        drop(self.guard.take());
        self.release_guard_anchor();
    }
}

/// A value coupled to exact artifact handles that certify any paths it names.
pub(crate) struct Retained<T> {
    pub(crate) value: T,
    pub(crate) retention: Option<ReplyRetention>,
}

impl<T> Retained<T> {
    pub(crate) fn plain(value: T) -> Self {
        Self {
            value,
            retention: None,
        }
    }

    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ArtifactReplyPublication",
            action = "QueueGuard",
            project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
        )
    )]
    pub(crate) fn guarded(value: T, retention: ReplyRetention) -> Self {
        Self {
            value,
            retention: Some(retention),
        }
    }

    pub(crate) fn into_parts(self) -> (T, Option<ReplyRetention>) {
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

/// One ordinary RPC response plus any artifact handles that must span its
/// explicit consume acknowledgement. Most replies are plain strings; capture
/// replies attach retention.
pub(crate) struct ControlReply {
    body: String,
    retention: Option<ReplyRetention>,
}

impl ControlReply {
    pub(crate) fn guarded(body: String, retention: Option<ReplyRetention>) -> Self {
        Self { body, retention }
    }

    fn into_body_retaining(self, slot: &mut Option<ReplyRetention>) -> String {
        let Self { body, retention } = self;
        *slot = retention;
        body
    }

    #[cfg(test)]
    pub(crate) fn prepare_retention_for_test(&mut self) -> Result<(), String> {
        self.retention
            .as_mut()
            .map_or_else(|| Ok(()), ReplyRetention::prepare_write)
    }
}

impl From<String> for ControlReply {
    fn from(body: String) -> Self {
        Self {
            body,
            retention: None,
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

/// The `image` reply payload: `(width, height, Some(png-bytes))` in `--bytes` mode,
/// `(width, height, None)` when the PNG was written to the confined file, or
/// `(0, 0, None)` when no window displays the target.
pub type ImageReply = Result<Retained<(u32, u32, Option<Vec<u8>>)>, String>;
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

/// Additive `image --meta` identity for the exact encoded capture.
///
/// Version 2 identifies the final RGBA bytes, the complete retained composite,
/// and every contributing leaf. A singular `view` is emitted for a one-leaf
/// terminal or native frame; the compiled stamp exists only for native content.
/// Composite captures use `-` for singular identity and enumerate their leaves
/// instead of making a false focused-leaf claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageFrameMetadata {
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
pub type ImageQueue = Arc<Mutex<VecDeque<ImageReq>>>;

/// Fixed admission queue between the socket listener and the control workers.
///
/// A connection used to create a fresh OS thread directly in the `accept` loop.
/// Under thread-pressure macOS can leave that new pthread parked at
/// `thread_start` (or stall `_pthread_create`) for seconds, so even a cheap
/// `version`/`metrics` request looked like a wedged main-thread RPC.  The listener
/// now performs only a bounded, non-blocking enqueue; a fixed set of workers is
/// created once at server startup and reused for every connection.
pub(crate) struct BoundedConnectionQueue<T> {
    capacity: usize,
    items: Mutex<VecDeque<T>>,
    ready: Condvar,
}

/// Bound admitted work across BOTH the queue and workers currently serving it.
///
/// Bounding only `inbox.len()` is insufficient for long-lived control sockets:
/// workers can pop every item and then all remain occupied, causing later peers
/// to sit in an apparently empty queue with nobody available to answer.  This
/// counter is an admission semaphore over queued + running jobs. A peer either
/// owns one runnable lane or gets its socket back immediately for an explicit
/// `ERR ... busy; retry` response. Before publication, capacity is set to the
/// number of workers that actually started; because admission cannot exceed that
/// number, no accepted excess waits behind a fully occupied worker set.
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
        Self {
            inbox: BoundedConnectionQueue::new(max_capacity),
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

    /// Reserve a runnable lane and enqueue without parking the listener. The
    /// reservation stays charged after a worker pops the job and is released
    /// only by [`Self::complete`].
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

    pub(crate) fn pop(&self) -> T {
        self.inbox.pop()
    }

    fn completion_guard(&self) -> DispatchCompletion<'_, T> {
        DispatchCompletion { dispatch: self }
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

    /// Admit without ever parking the listener. On saturation ownership is
    /// returned to the caller so it can emit a bounded busy reply and drop the
    /// connection; the queue can never grow beyond `capacity`.
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

/// Ordinary polling/RPC workers. Push subscriptions are handed to their own
/// bounded pool after authentication, so a quiet subscriber cannot consume an
/// inspection lane. Admission is lane-exact: accepted connections may remain
/// persistent indefinitely, while excess peers receive a prompt busy reply.
const CONTROL_WORKERS: usize = 8;
const CONTROL_SUBSCRIPTION_WORKERS: usize = 4;

struct SubscriptionJob {
    line: String,
    scope: Scope,
    stream: CtlStream,
}

struct SubscriptionDispatch {
    jobs: Arc<BoundedDispatch<SubscriptionJob>>,
}

impl SubscriptionDispatch {
    fn new() -> Self {
        Self {
            jobs: Arc::new(BoundedDispatch::new(CONTROL_SUBSCRIPTION_WORKERS)),
        }
    }

    fn try_submit(&self, line: String, scope: Scope, stream: CtlStream) -> Result<(), CtlStream> {
        self.jobs
            .try_submit(SubscriptionJob {
                line,
                scope,
                stream,
            })
            .map_err(|job| job.stream)
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
    operator: Option<crate::operator_host::ControlHandle>,
}

impl ControlWorkerContext {
    fn serve(&self, stream: CtlStream) {
        serve(
            stream,
            &self.active,
            &self.store,
            &self.subscribers,
            &self.proxy,
            &self.image_queue,
            self.token.as_str(),
            &self.sock_dir,
            &self.subscriptions,
            self.operator.as_ref(),
        );
    }

    fn serve_subscription(&self, mut job: SubscriptionJob) {
        run_subscribe_socket(
            &job.line,
            &self.active,
            &self.store,
            &self.subscribers,
            job.scope,
            &mut job.stream,
        );
        // Match the ordinary lane teardown: explicitly wake a vanished peer out
        // of the AF_UNIX kernel path before Drop releases this reusable lane.
        let _ = job.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Create the process-lifetime worker set once. Returns the number successfully
/// started so startup can fail closed if the OS cannot provide even one lane.
fn spawn_control_workers(
    dispatch: &Arc<BoundedDispatch<CtlStream>>,
    context: &Arc<ControlWorkerContext>,
) -> usize {
    let mut started = 0;
    for index in 0..CONTROL_WORKERS {
        let dispatch = dispatch.clone();
        let context = context.clone();
        let name = format!("aterm-control-{index}");
        match std::thread::Builder::new().name(name).spawn(move || {
            loop {
                let stream = dispatch.pop();
                let _completion = dispatch.completion_guard();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    context.serve(stream);
                }))
                .is_err()
                {
                    aterm_log::warn!("control worker recovered after a connection panic");
                }
            }
        }) {
            Ok(_) => started += 1,
            Err(error) => {
                aterm_log::warn!("control worker {index} could not start: {error}");
            }
        }
    }
    started
}

fn spawn_subscription_workers(
    dispatch: &Arc<SubscriptionDispatch>,
    context: &Arc<ControlWorkerContext>,
) -> usize {
    let mut started = 0;
    for index in 0..CONTROL_SUBSCRIPTION_WORKERS {
        let dispatch = dispatch.clone();
        let context = context.clone();
        let name = format!("aterm-subscribe-{index}");
        match std::thread::Builder::new().name(name).spawn(move || {
            loop {
                let job = dispatch.jobs.pop();
                let _completion = dispatch.jobs.completion_guard();
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    context.serve_subscription(job);
                }))
                .is_err()
                {
                    aterm_log::warn!("subscription worker recovered after a connection panic");
                }
            }
        }) {
            Ok(_) => started += 1,
            Err(error) => {
                aterm_log::warn!("subscription worker {index} could not start: {error}");
            }
        }
    }
    dispatch.jobs.set_capacity(started);
    started
}

/// Spawn the control-listener thread: provision the capability token, bind
/// the plan's socket (sweeping crashed instances' stale files first), lock it
/// to `0600`, publish the `latest` symlink, then accept connections and serve
/// the protocol on each AUTHENTICATED connection.
///
/// Access control is DEFAULT-ON (see [`crate::control_auth`]):
/// the socket lives in a per-user `0700` directory, every accepted peer must be
/// the same uid, and every connection must present the per-launch token before
/// any verb runs. If the token cannot be provisioned we FAIL CLOSED — the
/// socket is never bound, so it cannot be driven without auth.
#[allow(
    clippy::too_many_arguments,
    reason = "the control server's full set of independent collaborators (active handle, store, subscribers, proxy, image queue, socket plan); a config struct would only move the list"
)]
pub fn spawn(
    active: ActiveHandle,
    store: Store,
    subscribers: Subscribers,
    proxy: EventLoopProxy<Wake>,
    queue: ImageQueue,
    plan: control_auth::SocketPlan,
    // The exact startup config generation already admitted by the process-wide
    // service. The control thread must not perform a second config-file read
    // before deciding whether to expose the optional TLS listener.
    network_config: crate::app_config::Config,
    // The root session's (id, nonce) to publish as the recursion discovery graph
    // entry, written AFTER bind so it never races the stale sweep. `None` = skip.
    root_identity: Option<(SessionId, aterm_session::LaunchNonce)>,
    // Set to `true` ONLY after THIS instance's `CtlListener::bind` succeeds, so the
    // launcher's graceful-exit cleanup unlinks the socket + token ONLY when we
    // actually own them. Without this an instance that refused a live socket (or
    // failed to bind) would still delete another live instance's shared files on exit.
    bound: Arc<AtomicBool>,
    operator: Option<crate::operator_host::ControlHandle>,
) {
    std::thread::spawn(move || {
        let sock_path = plan.sock_path.clone();
        // The token + image-confinement subdir live alongside the socket file.
        let sock_dir = control_auth::dir_of_socket(&sock_path);
        if control_auth::ensure_private_dir(&sock_dir).is_err() {
            // Every bind outcome ALSO goes to the file log: a Finder/`open` launch
            // discards stderr, and a silently dead control socket takes the whole
            // introspection plane down with it (2026-07-05 incident).
            aterm_log::warn!(
                "control socket dir {} not creatable; socket disabled",
                sock_dir.display()
            );
            eprintln!(
                "aterm-gui: control socket dir {} not creatable; socket disabled",
                sock_dir.display()
            );
            return;
        }
        // Crashed instances cannot clean up after themselves: sweep dead pids'
        // per-instance socket/token leftovers. Only in the shared default dir —
        // an explicit override path owns its directory.
        if plan.latest_link.is_some() {
            control_auth::sweep_stale_instances(&sock_dir);
            // Also sweep dead recursion discovery entries (Item 5b) left by crashed
            // sessions that never ran their graceful `remove_graph_entry`.
            crate::proxy::sweep_stale_graph(&sock_dir);
        }
        // Never unlink a LIVE socket: a nested aterm that still saw an explicit
        // socket path must not unlink+rebind (and thus HIJACK) its parent's live
        // listener (Item 5 GAP-5, the belt to the env deny-list's suspenders). A
        // stale file from a crashed prior run (no live listener) is removed so
        // bind() does not fail with EADDRINUSE.
        //
        // This runs BEFORE `provision_token` on purpose: with an explicit (shared)
        // `ATERM_CONTROL_SOCK`, `plan.token_path` is a sibling shared with any live
        // instance, and `provision_token` unlinks+rewrites it unconditionally. If we
        // provisioned first and then refused a live socket, we would have clobbered
        // the live instance's token file — bricking its auth channel (every later
        // `aterm-ctl` would read our token and get `ERR auth`) without ever taking
        // over its listener. Detect+refuse the live listener first; touch nothing.
        // Per-instance default paths (`aterm-<ourpid>.sock`, latest_link present)
        // use the inverted probe: only a listener that actually ANSWERS refuses
        // the bind — an odd connect errno there is stale junk, and refusing on it
        // would strand this instance socketless for its lifetime (2026-07-05).
        // Explicit `$ATERM_CONTROL_SOCK` paths keep the strict never-hijack probe.
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
            eprintln!(
                "aterm-gui: control socket {sock_path} already has a live listener; \
                 running without a control socket rather than hijacking it"
            );
            return;
        }
        // Provision the per-launch capability token. FAIL CLOSED: no token =>
        // no socket (better to lose introspection than serve it unauthed).
        let token = match control_auth::provision_token(&plan.token_path) {
            Some(t) => Arc::new(t),
            None => {
                aterm_log::warn!("could not provision control-socket token; socket disabled");
                eprintln!("aterm-gui: could not provision control-socket token; socket disabled");
                return;
            }
        };
        let _ = std::fs::remove_file(&sock_path);
        // Bounded retry: a transient fs/errno hiccup at startup (AV scan, racing
        // cleanup of a predecessor) must degrade to a short delay, not a
        // process-lifetime loss of the whole introspection plane.
        let mut listener = None;
        for attempt in 1..=3u32 {
            match CtlListener::bind(&sock_path) {
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
                        "control socket bind failed at {sock_path} (attempt {attempt}/3): {e}"
                    );
                    eprintln!(
                        "aterm-gui: control socket bind failed at {sock_path} \
                         (attempt {attempt}/3): {e}"
                    );
                    if attempt < 3 {
                        std::thread::sleep(std::time::Duration::from_millis(750));
                        // Shared explicit paths: RE-PROBE before unlinking — a
                        // sibling may have bound while we slept, and unlinking its
                        // LIVE socket would be exactly the hijack RefuseLiveSocket
                        // exists to prevent. Per-instance paths stay retry-happy
                        // (nobody else can own our pid's name).
                        if plan.latest_link.is_none() && control_auth::socket_is_live(&sock_path) {
                            aterm_log::warn!(
                                "control socket {sock_path} became live during retry; \
                                 running socketless rather than hijacking it"
                            );
                            eprintln!(
                                "aterm-gui: control socket {sock_path} became live during \
                                 retry; running socketless rather than hijacking it"
                            );
                            return;
                        }
                        let _ = std::fs::remove_file(&sock_path);
                    }
                }
            }
        }
        let Some(listener) = listener else {
            aterm_log::warn!("control socket disabled: bind failed after 3 attempts");
            return;
        };
        // Start fixed reusable lanes BEFORE setting the externally observed
        // `bound` flag or publishing the latest-instance pointer. The accept loop
        // never calls pthread_create, so connection churn cannot stall admission.
        // At least one worker is required; a partial pool remains useful and is
        // reported honestly in the log.
        let connection_dispatch = Arc::new(BoundedDispatch::new(CONTROL_WORKERS));
        let subscription_dispatch = Arc::new(SubscriptionDispatch::new());
        let worker_context = Arc::new(ControlWorkerContext {
            active: active.clone(),
            store: store.clone(),
            subscribers: subscribers.clone(),
            proxy: proxy.clone(),
            image_queue: queue.clone(),
            token: token.clone(),
            sock_dir: sock_dir.clone(),
            subscriptions: subscription_dispatch.clone(),
            operator: operator.clone(),
        });
        let workers = spawn_control_workers(&connection_dispatch, &worker_context);
        connection_dispatch.set_capacity(workers);
        if workers == 0 {
            aterm_log::warn!("control socket disabled: no connection worker could start");
            eprintln!("aterm-gui: control socket disabled: no connection worker could start");
            drop(listener);
            let _ = std::fs::remove_file(&sock_path);
            let _ = std::fs::remove_file(&plan.token_path);
            return;
        }
        let subscription_workers =
            spawn_subscription_workers(&subscription_dispatch, &worker_context);
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
        bound.store(true, Ordering::SeqCst);
        control_auth::lock_socket_file(&sock_path);
        if let Some(link) = &plan.latest_link {
            control_auth::publish_latest_link(link, &sock_path);
        }
        // The one line whose ABSENCE from the file log now positively means
        // "control setup failed before bind" (every failure branch warns above).
        aterm_log::info!(
            "control socket listening at {sock_path} ({workers} RPC lanes, {subscription_workers} subscription lanes; excess peers get retry)"
        );
        #[cfg(not(windows))]
        eprintln!(
            "aterm-gui: control socket listening at {sock_path} (token-gated, same-uid only)"
        );
        #[cfg(windows)]
        {
            // Same `listening at <PATH> (token-gated` shape aterm-nest parses,
            // with the HONEST posture parenthetical, plus the one-line
            // peer-uid-unavailable notice (never silently claim same-uid).
            eprintln!(
                "aterm-gui: control socket listening at {sock_path} (token-gated, dir-ACL only)"
            );
            eprintln!(
                "aterm-gui: control socket peer-uid check NOT available on Windows (AF_UNIX \
                 has no SO_PEERCRED); relying on the %LOCALAPPDATA% directory ACL (owner \
                 verified + hardened to an owner-only DACL) + the per-launch token"
            );
        }
        // Recursion discovery (Item 5b): publish the root session's graph entry
        // ONLY NOW — AFTER bind succeeded — so a concurrent `sweep_stale_graph`
        // can never observe our entry pointing at a not-yet-bound socket and
        // delete it as stale (the sibling-respawn race). `None` skips it.
        if let Some((sid, nonce)) = &root_identity {
            // `publish_graph_entry` (not the single-dir `write_graph_entry`) so an
            // instance on an explicit `$ATERM_CONTROL_SOCK` ALSO registers in the
            // default rendezvous dir the flagless `aterm-ctl` client reads.
            crate::proxy::publish_graph_entry(&sock_dir, sid, &sock_path, nonce);
        }
        // Sibling discovery: record our bound socket so every session registered
        // from now on publishes its own graph entry (the register seam calls
        // `proxy::publish_session`), then publish the sessions ALREADY in the
        // store (registered before the bind). Any registration concurrent with
        // this window lands in one of the two — it either sees the recorded
        // socket (publishes itself) or is present in the snapshot below. (A
        // session DEREGISTERED between snapshot and write could get its entry
        // briefly resurrected; that stale entry names OUR live socket, so the
        // self-dial guard degrades it to `ERR no such session` and the next
        // instance's sweep removes it — never a wrong target.)
        crate::proxy::set_self_sock(&sock_dir, &sock_path);
        for h in store.read().unwrap_or_else(|p| p.into_inner()).snapshot() {
            // Mirror into the default rendezvous dir too (explicit-socket case);
            // see `publish_graph_entry`.
            crate::proxy::publish_graph_entry(&sock_dir, &h.sid, &sock_path, &h.nonce);
        }
        // Secure-default-OFF network drive: only when the operator configures it
        // (env `ATERM_NET_LISTEN/_CERT/_KEY` or the `[net]` table) does this open a
        // TLS port that relays a channel-bound remote driver into THIS control
        // socket. `maybe_spawn` itself enforces ROOT-ONLY (an explicit
        // ATERM_PARENT_SESSION_ID / TERM_PROGRAM check) so a nested aterm never binds
        // a second surface — the env deny-list covers only the env path, not the
        // shared config file. The same per-launch token gates network and local hop.
        crate::net_listen::maybe_spawn(token.as_str(), &sock_path, &network_config);
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            // Peer credential gate: refuse any connection NOT from our own uid
            // before spending a thread on it (Unix: `None`/cannot-verify also
            // refuses — fail closed; Windows has no peer-uid primitive, so the
            // gate always passes there and the token remains the mandatory
            // gate — the startup notice above discloses the reduction).
            match control_auth::peer_check(&stream) {
                Ok(()) => {}
                Err(why) => {
                    log_denial(
                        AUDIT_SUBSYSTEM,
                        &why,
                        aterm_containment::mode_or_containment(),
                        "peer uid mismatch",
                    );
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
            }
            // Admission counts queued PLUS running work, not merely inbox depth.
            // Thus every accepted connection owns a runnable worker lane even if
            // all existing lanes are slow-cadence persistent drivers. A same-uid
            // overload gets a short explicit retry response; the listener remains
            // in `accept` and no unbounded queue or pthread churn is created.
            if let Err(mut stream) = connection_dispatch.try_submit(stream) {
                let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(100)));
                let _ = stream.write_all(b"ERR control server busy; retry\n");
                let _ = stream.flush();
                // Do not `shutdown(Both)` here: this lane rejected the socket
                // before reading its AUTH/request bytes, and macOS may discard
                // the queued reply when a socket with unread input is reset.
                // Dropping after the bounded write preserves the explicit busy
                // frame; `aterm-ctl` sends AUTH + request atomically so it can
                // always consume this early status.
            }
        }
    });
}

/// Resolve a stable-id selector to a child this aterm holds authority over plus
/// its live socket path, or `None`. Shared by the `@`-first verbs and the
/// selector-SECOND `subscribe` path. Fail-closed: a locally-hosted sid is NOT a
/// proxy hop, an unspawned child has no entry, and a graph entry whose nonce
/// mismatches the retained one (a relaunched child) is rejected.
fn resolve_proxy_child(
    sid: &SessionId,
    store: &Store,
    sock_dir: &std::path::Path,
) -> Option<(crate::proxy::ProxyEntry, String)> {
    if store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_sid(sid)
        .is_some()
    {
        return None; // hosted locally → a normal in-process target
    }
    let entry = crate::proxy::lookup_child(sid)?;
    let (sock_path, nonce) = crate::proxy::read_graph_entry(sock_dir, sid)?;
    if !entry.nonce.ct_eq(&nonce) {
        return None; // graph entry is for a DIFFERENT launch — fail closed
    }
    // CONFINE the discovered socket path to our own runtime dir before the forward
    // dials it and presents the edge token. The graph entry is same-uid writable and
    // its nonce is readable, so the nonce check above does NOT stop a hostile same-uid
    // process from redirecting `sock <path>` to an attacker socket to capture the
    // token (the same threat `confine_image_path` closes for image writes). Fail closed.
    let sock_path = crate::control_auth::confine_proxy_sock(sock_dir, &sock_path)?;
    Some((entry, sock_path))
}

/// Resolve a stable-id selector to a SIBLING instance's socket: a session hosted
/// by another same-uid aterm this process did NOT spawn (two terminals the user
/// opened separately), discovered through the per-session graph entry the hosting
/// instance published. Returns `(sibling_sock_path, sibling_auth_token)`.
///
/// Fail-closed guards, in order:
/// * a locally-hosted sid is not a hop (the in-process store owns it);
/// * no graph entry → unknown sid;
/// * the entry's socket path must confine to OUR OWN runtime dir
///   ([`crate::control_auth::confine_proxy_sock`] — same posture as the child
///   hop: a same-uid-writable entry must never redirect a dial outside the dir);
/// * the SELF-DIAL guard: an entry pointing at our own socket is a stale record
///   of a session we no longer host — dialing it would loop the request back to
///   this very server, so it is refused (`ERR no such session` downstream);
/// * the socket must have a LIVE listener (a dead instance's leftovers resolve
///   to nothing rather than a hung dial);
/// * the sibling's own per-launch AUTH token must be readable
///   ([`crate::proxy::read_sibling_token`]) — the same same-uid 0600 credential
///   a direct `aterm-ctl --pid` client presents, so this forward grants nothing
///   the caller could not already take by dialing the sibling itself.
fn resolve_sibling(
    sid: &SessionId,
    store: &Store,
    sock_dir: &std::path::Path,
) -> Option<(String, String)> {
    if store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_sid(sid)
        .is_some()
    {
        return None; // hosted locally → a normal in-process target
    }
    let (sock_path, _nonce) = crate::proxy::read_graph_entry(sock_dir, sid)?;
    let sock_path = crate::control_auth::confine_proxy_sock(sock_dir, &sock_path)?;
    if crate::proxy::self_sock_path().as_deref() == Some(sock_path.as_str()) {
        return None; // stale entry for a session we no longer host — never self-dial
    }
    if !crate::control_auth::socket_is_live(&sock_path) {
        return None; // dead instance leftovers — swept later, never dialed
    }
    let token = crate::proxy::read_sibling_token(&sock_path)?;
    Some((sock_path, token))
}

/// PURE decision: given a request `line` + the connection `scope`, decide whether
/// it must be forwarded to another instance's socket and return
/// `(target_sock_path, first_line)` to present, else `None` (handle locally).
/// Two hop kinds, tried in order of authority specificity:
///
/// 1. **Spawned child** ([`resolve_proxy_child`]) — forwarded with the per-op
///    edge token this aterm minted at spawn, the child's own selector rewritten
///    to `@.` (the child runs the verb on itself; it can never re-forward).
/// 2. **Sibling instance** ([`resolve_sibling`]) — a same-uid aterm the user
///    opened separately; forwarded with the sibling's OWN per-launch token
///    (Owner there), the `@<sid>` selector KEPT so the sibling resolves the
///    session among its own tabs. Same-trust-domain by construction: only an
///    Owner-scope caller reaches this, and the presented credential is one any
///    same-uid client could read directly.
///
/// Split out so the security-critical decision — Owner-only, local-hosted
/// bypass, nonce-guarded child discovery, self-dial guard, op→token — is
/// unit-testable without a live relay.
fn proxy_forward_plan(
    line: &str,
    scope: Scope,
    store: &Store,
    sock_dir: &std::path::Path,
) -> Option<(String, String)> {
    // Only the OWNER of this aterm may forward — with its retained child tokens
    // (hop 1) or its same-trust-domain sibling reach (hop 2). A scoped Edge
    // connection cannot escalate to a session it was never granted (it falls
    // through to local resolution, which denies the cross-process sid).
    if !matches!(scope, Scope::Owner) {
        return None;
    }
    let line = line.strip_suffix('\r').unwrap_or(line);

    // `subscribe` is selector-SECOND (`subscribe @<sel> <streams> [since=] [every-frame]`),
    // so the generic `@`-first parse below cannot see its target. Handle it here so a
    // live `subscribe @<child> cells,bytes` reaches the inner session (a single remote
    // child only; a mixed local/remote comma list stays local).
    if let Some(rest) = line.strip_prefix("subscribe ") {
        let sel_tok = rest.split_whitespace().next()?;
        let sel_body = sel_tok.strip_prefix('@')?;
        if sel_body.contains(',') {
            return None;
        }
        let Selector::Sid(sid) = Selector::parse(sel_body) else {
            return None;
        };
        let op = required_op("subscribe")?; // ReadScreen
        if let Some((entry, sock_path)) = resolve_proxy_child(&sid, store, sock_dir) {
            let tok = entry.token_for(op)?;
            // Rewrite the child's selector to `@.` while preserving the streams + flags.
            let rewritten = format!("subscribe @.{}", &rest[sel_tok.len()..]);
            return Some((
                sock_path,
                crate::proxy::forward_first_line(&tok.to_hex(), &rewritten),
            ));
        }
        // Sibling hop: keep the selector (the sibling resolves the sid itself).
        let (sock_path, token) = resolve_sibling(&sid, store, sock_dir)?;
        return Some((sock_path, crate::proxy::forward_first_line(&token, line)));
    }

    let (first, verb_line) = line.split_once(' ')?;
    let sel_body = first.strip_prefix('@')?; // no @selector → local self path
    let Selector::Sid(sid) = Selector::parse(sel_body) else {
        return None; // only stable-id selectors cross processes
    };
    let verb = verb_line.split_whitespace().next().unwrap_or("");
    // Only op-classed verbs (read/write/signal) cross processes — identity /
    // privilege verbs (`grant`/`whoami`/...) stay strictly local on BOTH hops.
    let op = required_op(verb)?;
    if let Some((entry, sock_path)) = resolve_proxy_child(&sid, store, sock_dir) {
        let tok = entry.token_for(op)?;
        // The child is the DIRECT target: rewrite its selector to `@.` (run on self).
        let rewritten = format!("@. {verb_line}");
        return Some((
            sock_path,
            crate::proxy::forward_first_line(&tok.to_hex(), &rewritten),
        ));
    }
    // Sibling hop: keep the `@<sid>` selector so the sibling instance resolves
    // the session among its own tabs (which may not be its root session).
    //
    // DELIBERATE fallthrough for a RELAUNCHED child (nonce mismatch above): the
    // relaunch is no longer the child we provisioned, but it IS a live same-uid
    // aterm, so it is reachable like any sibling — with ITS OWN token, never the
    // stale edge tokens the nonce guard protects (those still fail closed).
    let (sock_path, token) = resolve_sibling(&sid, store, sock_dir)?;
    Some((sock_path, crate::proxy::forward_first_line(&token, line)))
}

/// If `line` targets a session this process does NOT host but IS a child this
/// aterm spawned (Item 5b), forward the whole connection to the child's control
/// socket and RELAY bytes transparently, returning `true` (the caller then ends
/// the connection). Returns `false` for a normal local request. The forward
/// presents the per-op edge token this aterm minted for the child (so the child
/// authorizes the EXACT op the verb needs) and rewrites the child's own selector
/// to `@.` so it runs the verb on itself.
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
    if crate::proxy::connect_and_relay(&sock_path, &first_line, client, &pre).is_err() {
        let _ = (&*client).write_all(b"ERR forward\n");
        let _ = (&*client).flush();
    }
    true
}

/// `dial-list` — the saved connection names (Owner-only), space-separated. An
/// agent runs this to discover what it can `dial`.
fn cmd_dial_list() -> String {
    let names = crate::net_connections::names();
    if names.is_empty() {
        "OK (no connections — add [[net.connections]] to aterm.toml)\n".to_string()
    } else {
        format!("OK {}\n", names.join(" "))
    }
}

/// `dial-token <name> <hex>` — provision a connection's drive token (Owner-only)
/// into the macOS Keychain (else a 0600 file); it never touches `aterm.toml`. This
/// is the one-time setup so `dial <name>` can authenticate to the remote.
fn cmd_dial_token(rest: &str) -> String {
    let mut it = rest.split_whitespace();
    let (Some(name), Some(hex), None) = (it.next(), it.next(), it.next()) else {
        return "ERR usage: dial-token <name> <token-hex>\n".to_string();
    };
    match crate::net_connections::store_token(name, hex) {
        Ok(msg) => format!("OK {msg}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// If `line` is `dial <name>` from an OWNER-scoped connection, dial the saved
/// remote endpoint (`[[net.connections]]`) over TLS, present the channel-bound
/// capability, and RELAY this connection to the remote's control socket — so the
/// client then drives the remote transparently (it speaks ordinary control verbs
/// and they execute there). Returns `true` once it has handled the line (the
/// caller ends this connection); `false` for any non-`dial <name>` line.
///
/// Bare `dial`, `dial-list`, and `dial-token` are NORMAL-response verbs handled in
/// [`handle`]; only `dial <name>` takes over the connection (like a proxy forward,
/// but across the network). A trailing verb is a convenient one-shot form used
/// by `aterm-ctl`; bare dial remains the persistent transport used by
/// `aterm-agent`/`aterm-drive`. The remote's listener authenticates its own local
/// control socket, so the raw drive token never crosses the wire. Artifact path
/// replies name the REMOTE host; use `image --bytes` when pixels must cross back.
fn try_net_dial<R: Read>(
    line: &str,
    scope: Scope,
    client: &CtlStream,
    reader: &mut BufReader<R>,
) -> bool {
    let Some(rest) = line.strip_prefix("dial ") else {
        return false; // not `dial <...>` (dial-list / dial-token go through handle)
    };
    // `dial <name> [verb...]`: the name is the first token; an OPTIONAL trailing verb
    // is a ONE-SHOT remote command. Without it a one-shot client deadlocks — it sends
    // `dial <name>`, then blocks reading while the remote blocks waiting for a verb.
    // A verb tail is prepended to the relay's prebuffer so the remote receives it
    // immediately, answers, and the relay pumps that reply back to the client.
    let mut parts = rest.trim().splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or("").trim();
    let verb_tail = parts.next().map(str::trim).filter(|s| !s.is_empty());
    let reply = |msg: &str| {
        let _ = (&*client).write_all(msg.as_bytes());
        let _ = (&*client).flush();
    };
    if name.is_empty() {
        reply("ERR dial expects a connection name (dial <name> [verb...])\n");
        return true;
    }
    // Dialing OUT wields the saved connection's full remote-drive authority, so it
    // is OWNER-only: a connection that itself arrived over an edge cannot dial out.
    if !matches!(scope, Scope::Owner) {
        reply("ERR dial requires owner scope (an edge token cannot dial out)\n");
        return true;
    }
    // The authenticated request loop uses a short liveness-poll timeout. The relay
    // owns both directions, so restore ordinary blocking I/O.
    if client.set_read_timeout(None).is_err() {
        reply("ERR dial timeout setup\n");
        return true;
    }
    // Prebuffer = the one-shot verb line (if any) FOLLOWED by any bytes the client
    // already pipelined after the dial line, so ordering on the remote is preserved.
    let mut pre: Vec<u8> = Vec::new();
    if let Some(verb_tail) = verb_tail {
        pre.extend_from_slice(verb_tail.as_bytes());
        pre.push(b'\n');
    }
    // Pipelined bytes also carry binary bodies and a very fast post-response
    // artifact ACK, so the relay must forward whatever BufReader already ate.
    pre.extend_from_slice(&crate::proxy::drain_buffered(reader));
    if let Err(e) = crate::net_connections::dial_relay(name, client, &pre) {
        reply(&format!("ERR dial {e}\n"));
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

fn dispatch_meta_verb(
    verb: &str,
    rest: &str,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
) -> Option<String> {
    match verb {
        "version" => Some(crate::build_info::control_line()),
        "update" => Some(cmd_update(rest, scope, proxy)),
        "help" | "verbs" => Some(cmd_help()),
        _ => None,
    }
}

/// Dispatch the established app-level verbs without requiring a PTY mirror.
/// An optional terminal is consulted only to preserve the legacy `metrics`
/// rows/cols fields while the front content is actually terminal.
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
                matches!(scope, Scope::Owner),
            ));
        }
        "chrome" => control_media::cmd_chrome(proxy),
        "panes" => control_media::cmd_panes(proxy, None),
        "controls" => control_media::cmd_controls(proxy, rest),
        "inspect" => control_media::cmd_inspect(proxy, rest),
        "open" => control_media::cmd_open(proxy, rest),
        "act" => control_media::cmd_act(proxy, rest),
        "invoke" => control_media::cmd_invoke(proxy, rest),
        "rain" => control_media::cmd_rain(proxy, rest),
        "tone" => control_media::cmd_tone(proxy, rest),
        "trail" => control_media::cmd_trail(proxy, rest),
        "spawn" => control_media::cmd_spawn(proxy, rest),
        "settings" => control_media::cmd_settings_overlay(proxy, rest),
        "tab" => control_input::cmd_tab(proxy, rest),
        "hover" => control_input::cmd_hover(proxy, rest),
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

/// Stable protocol tombstones for verbs removed with the bottom HUD. They stay
/// out of the advertised verb catalog and carry no authority class, but an old
/// client gets an actionable failure instead of accidentally targeting a future
/// verb with the same spelling.
fn retired_bottom_hud_verb_error(verb: &str) -> Option<&'static str> {
    match verb {
        "widgets" => Some("ERR verb widgets was removed with the bottom HUD\n"),
        "metric" => Some("ERR verb metric was removed with the bottom HUD\n"),
        _ => None,
    }
}

/// Handle requests whose authority/target class does not depend on a session.
/// Returning `None` means the normal session/edge resolver must continue.
#[allow(clippy::too_many_arguments)]
fn dispatch_before_session(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    sock_dir: &std::path::Path,
) -> Option<ControlReply> {
    use aterm_types::control_verbs::{Access, Target};

    let (selector, verb, rest) = request_head(line);
    if let Some(error) = retired_bottom_hud_verb_error(verb) {
        return Some(error.into());
    }
    let Some(spec) = aterm_types::control_verbs::spec(verb) else {
        return Some("ERR unknown verb (try: help)\n".into());
    };

    if spec.access == Access::AnyScopeMeta {
        return dispatch_meta_verb(verb, rest, scope, proxy).map(Into::into);
    }

    // Fleet reads and connection-management metadata remain useful when the
    // process currently has no terminal at all. Session-authority mutation
    // (`grant`/`revoke`/`whoami`) intentionally falls through and reports the
    // typed no-terminal error when no live context exists.
    if spec.access == Access::OwnerOnly {
        if !matches!(selector, None | Some(Selector::SelfTok)) || !matches!(scope, Scope::Owner) {
            return Some("ERR denied\n".into());
        }
        return match verb {
            "sessions" => Some(control_session::cmd_sessions_store(store)),
            "who" => Some(control_session::cmd_who(store, subscribers)),
            "dial-list" => Some(cmd_dial_list()),
            "dial-token" => Some(cmd_dial_token(rest)),
            _ => None,
        }
        .map(Into::into);
    }

    let principal = if matches!(scope, Scope::Owner) {
        NativeControlPrincipal::Owner
    } else {
        NativeControlPrincipal::Edge
    };

    // `paste` is session-class for explicit/background targets, but its bare
    // form is a front-input gesture. When a native Editor owns the front there
    // is intentionally no ActiveSession to resolve; post the same InputEvent a
    // human paste uses and let App::input enforce the native input boundary.
    // With a terminal front (or an explicit selector), return None and preserve
    // the established terminal routing byte-for-byte.
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
                Ok(event) => control_input::input_reply_to_str(post_input_reply(
                    proxy,
                    Op::WriteInput,
                    vec![event],
                )),
                Err(error) => error.to_string(),
            }
            .into(),
        );
    }

    // A native tab app owns a real framebuffer but deliberately has no active
    // terminal session. Bare owner `image` therefore targets the front native
    // window directly. Keep `image read` and explicit session selectors on the
    // session path: they describe terminal payloads, not app pixels.
    if verb == "image"
        && resolve_active(active).is_none()
        && matches!(scope, Scope::Owner)
        && matches!(selector, None | Some(Selector::SelfTok))
        && rest.split_whitespace().next() != Some("read")
    {
        return Some(control_media::cmd_image(proxy, queue, rest, sock_dir, None));
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
    let active_term = resolve_active(active).map(|(term, _, _, _)| term);
    dispatch_app_verb(verb, rest, scope, proxy, sock_dir, active_term.as_ref())
}

/// Resolve only an explicit non-self selector, independent of front content.
fn resolve_explicit(store: &Store, selector: &Selector) -> Option<Target> {
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

/// Full polling-request dispatch. Classification precedes terminal resolution,
/// so native-only Settings/about/update windows remain controllable without a
/// fabricated ActiveSession.
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
    if let Some(response) = dispatch_before_session(
        line,
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

    let (selector, _, _) = request_head(line);
    let active_target = resolve_active(active);
    let explicit = matches!(selector, Some(Selector::Local(_) | Selector::Sid(_)));
    let explicit_live = selector
        .as_ref()
        .is_some_and(|selector| selector_is_live(store, selector));
    let principal = if matches!(scope, Scope::Owner) {
        NativeControlPrincipal::Owner
    } else {
        NativeControlPrincipal::Edge
    };
    match native_control_decision(
        active_target.is_some(),
        explicit_live,
        principal,
        if explicit {
            NativeControlTarget::ExplicitSession
        } else {
            NativeControlTarget::BareSession
        },
    ) {
        NativeControlDecision::ResolveSession => {}
        NativeControlDecision::Denied => return "ERR denied\n".into(),
        NativeControlDecision::NoActiveTerminal => return NO_ACTIVE_TERMINAL.into(),
        NativeControlDecision::NoSuchSession => return "ERR no such session\n".into(),
        NativeControlDecision::WithoutSession => {
            return "ERR invalid control target\n".into();
        }
    }
    // Capture the FRONT tab's id before `active_target` is consumed: an explicit
    // `@<sid>` naming this very tab is routed through the App input seam rather
    // than the background egress (see `front_routed_input`).
    let front_active_session = active_target.as_ref().map(|(_, _, session, _)| *session);
    let target = match selector.as_ref() {
        Some(selector @ (Selector::Local(_) | Selector::Sid(_))) => {
            resolve_explicit(store, selector)
        }
        None | Some(Selector::SelfTok) => active_target,
    };
    let Some((term, master, session, ctx)) = target else {
        return if matches!(selector, Some(Selector::Local(_) | Selector::Sid(_))) {
            "ERR no such session\n".to_string()
        } else {
            NO_ACTIVE_TERMINAL.to_string()
        }
        .into();
    };
    let mut retention = None;
    let body = handle(
        line,
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
        &mut retention,
    );
    ControlReply::guarded(body, retention)
}

/// Serve one connection: AUTHENTICATE the first line against the capability
/// token, then read newline-delimited requests and write one response each,
/// until the client disconnects or a write fails (dead client).
///
/// The peer's uid was already verified in [`spawn`]; here we require the token.
/// The first line MUST be `AUTH <hex>` or `TOKEN <hex> <verb...>`; anything else
/// gets `ERR auth\n` and the connection is closed BEFORE any verb executes.
///
/// PUSH-ONLY (P1.3): a `subscribe` verb FLIPS this connection to server-push by
/// handing it to the reserved subscription pool. [`run_subscribe`] there never
/// reads another request line — the client thereafter only reads
/// `DELTA`/`EVENT`/`GAP` frames — while this ordinary RPC worker returns at once.
enum ServeDisposition {
    Close,
    Subscribe { line: String, scope: Scope },
}

struct PendingArtifactAck {
    retention: ReplyRetention,
    nonce: String,
}

/// A complete guarded response gets this long to finish its client handoff.
/// A valid nonce acknowledgement releases the exact handles immediately. Any
/// timeout, early request-half close, legacy client, or partial wire failure
/// transfers them to the process-global quarantine for this *additional* grace
/// interval, so a failed ACK can never become an immediate same-name race.
const ARTIFACT_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const ARTIFACT_FAILURE_QUARANTINE: std::time::Duration = std::time::Duration::from_secs(30);

struct QuarantinedArtifactReply {
    until: std::time::Instant,
    _retention: ReplyRetention,
}

struct ArtifactReplyQuarantine {
    entries: std::sync::Mutex<Vec<QuarantinedArtifactReply>>,
    changed: std::sync::Condvar,
}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReplyPublication",
        action = "AdvanceQuarantine",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn advance_artifact_quarantine_anchor() {}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReplyPublication",
        action = "ExpireQuarantine",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn expire_artifact_quarantine_anchor() {}

fn artifact_reply_quarantine() -> &'static ArtifactReplyQuarantine {
    static QUARANTINE: std::sync::OnceLock<ArtifactReplyQuarantine> = std::sync::OnceLock::new();
    QUARANTINE.get_or_init(|| {
        let quarantine = ArtifactReplyQuarantine {
            entries: std::sync::Mutex::new(Vec::new()),
            changed: std::sync::Condvar::new(),
        };
        // One reaper handles every failed handoff. If the OS refuses the single
        // helper thread, queued guards intentionally remain retained forever:
        // fail-closed availability loss is safer than releasing an advertised
        // path before its bounded compatibility handoff.
        let _ = std::thread::Builder::new()
            .name("aterm-artifact-quarantine".into())
            .spawn(artifact_reply_quarantine_reaper);
        quarantine
    })
}

fn artifact_reply_quarantine_reaper() {
    let quarantine = artifact_reply_quarantine();
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
        drop(entries);
        // Exact filesystem/OS locks are released outside the queue mutex.
        drop(expired);
    }
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
    });
    quarantine.changed.notify_one();
}

#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "ArtifactReplyPublication",
        action = "WriteWire",
        project = "aterm_gui::artifact_transaction_conformance::project_artifact_reply"
    )
)]
fn write_control_reply(
    writer: &mut impl Write,
    mut reply: ControlReply,
) -> std::io::Result<Option<PendingArtifactAck>> {
    // Mint the causal challenge BEFORE fallible wire preparation. Video
    // preparation can publish its durable marker; entropy failure must therefore
    // remain an invisible, abortable ERR rather than occur after that boundary.
    let mut nonce = if reply.retention.is_some() {
        match aterm_uds::rand::hex_token::<16>() {
            Ok(nonce) => Some(nonce),
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
        // The original OK names an identity the retained handles can no longer
        // certify. Drop/abort that publication before writing a path-free ERR.
        retention.prepare_failed_anchor();
        reply.body = format!("ERR artifact reply validation failed: {error}\n");
        reply.retention = None;
        nonce = None;
    }
    // `reply` remains in scope through the complete write + flush; guarded
    // retention is then returned to the explicit ACK waiter.
    let write = (|| {
        writer.write_all(reply.body.as_bytes())?;
        if let Some(nonce) = nonce.as_deref() {
            writer.write_all(
                aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX.as_bytes(),
            )?;
            writer.write_all(nonce.as_bytes())?;
            writer.write_all(b"\n")?;
        }
        writer.flush()
    })();
    if let Err(error) = write {
        if let Some(mut retention) = reply.retention.take() {
            retention.write_failed_anchor();
            quarantine_artifact_reply_until(
                retention,
                std::time::Instant::now() + ARTIFACT_FAILURE_QUARANTINE,
            );
        }
        return Err(error);
    }
    Ok(reply
        .retention
        .take()
        .zip(nonce)
        .map(|(retention, nonce)| PendingArtifactAck { retention, nonce }))
}

/// Guarded artifact replies are one-shot at the connection level. After writing
/// the complete ordinary response frame, retain every exact identity until the
/// server then appends an unpredictable `ACK-CHALLENGE <nonce>` trailer. The
/// shipping client can echo `ACK <nonce>` only after consuming the full framed
/// response and trailer; a pre-pipelined ACK cannot guess the challenge.
///
/// Any non-matching line, EOF, timeout, or I/O failure is an explicit failed-ACK
/// outcome transferred to quarantine, never a successful consume
/// acknowledgement or another dispatched verb. In particular, request-half EOF
/// cannot masquerade as ACK: a client may half-close immediately after its
/// request while still waiting to read the response. Transparent local/TLS
/// relays carry the ACK as ordinary reverse-direction traffic.
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
    } = pending;
    let expected = format!(
        "{}{nonce}",
        aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX
    );
    let deadline = std::time::Instant::now() + ARTIFACT_ACK_TIMEOUT;
    let now = std::time::Instant::now();
    if now >= deadline
        || stream
            .set_read_timeout(Some(deadline.saturating_duration_since(now)))
            .is_err()
    {
        retention.acknowledge_failed_anchor();
        quarantine_artifact_reply_until(retention, std::time::Instant::now() + failure_quarantine);
        return ArtifactAckOutcome::AcknowledgementQuarantined;
    }
    // EXACTLY one line decides the outcome — the challenge is one-shot, so a
    // line that is not the ACK is a failure and never a re-read: reading again
    // would let a peer keep guessing the nonce inside one deadline. The read
    // timeout set above bounds this single read, and `read_request_line` folds
    // its expiry, EOF, and I/O failure alike into `None`.
    match read_request_line(reader) {
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

#[allow(clippy::too_many_arguments)]
fn serve(
    stream: CtlStream,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    token: &str,
    sock_dir: &std::path::Path,
    subscriptions: &SubscriptionDispatch,
    operator: Option<&crate::operator_host::ControlHandle>,
) {
    match serve_borrowed(
        &stream,
        active,
        store,
        subscribers,
        proxy,
        queue,
        token,
        sock_dir,
        operator,
    ) {
        ServeDisposition::Close => {
            // On macOS a peer-vanished AF_UNIX close can linger in the kernel.
            // An explicit shutdown makes teardown prompt before the worker's
            // completion guard releases this lane.
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        ServeDisposition::Subscribe { line, scope } => {
            handoff_subscription(subscriptions, line, scope, stream);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn serve_borrowed(
    stream: &CtlStream,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    token: &str,
    sock_dir: &std::path::Path,
    operator: Option<&crate::operator_host::ControlHandle>,
) -> ServeDisposition {
    // Bound the UNAUTHENTICATED phase: `read_request_line` has no deadline of its
    // own, so a same-uid peer that connects and then goes silent would park this
    // thread forever before ever presenting a token. The timeout surfaces as a
    // read error -> `None` -> the connection is dropped and this fixed worker is
    // reused. The reader borrows this one stream rather than duplicating its fd:
    // macOS can block a duplicated AF_UNIX fd's close under connection churn.
    // Successful authentication clears it to preserve persistent polling.
    if stream.set_read_timeout(Some(AUTH_READ_TIMEOUT)).is_err() {
        return ServeDisposition::Close; // cannot bound pre-auth: fail closed
    }
    let mut reader = BufReader::new(stream);
    let mut writer = stream;

    // First line is the auth handshake. A `TOKEN <hex> <verb...>` form folds the
    // first verb in, so we may have a verb to dispatch immediately.
    //
    // We drive the BufReader with an explicit `read_line` loop (rather than the
    // `lines()` iterator) so the `feed-bin <n>` verb can `read_exact` the N raw
    // bytes that FOLLOW its request line from the SAME buffered stream — the
    // length-prefixed binary frame. Every other path is byte-identical: we strip
    // the trailing newline ourselves, exactly as `lines()` did.
    let first = match read_request_line(&mut reader) {
        Some(l) => l,
        None => return ServeDisposition::Close, // client hung up before auth
    };
    let (scope, inline_verb) = match control_auth::check_auth_line(&first, token) {
        // Tier 1: the per-instance god token => Owner.
        AuthOutcome::Ok(verb) => (Scope::Owner, verb),
        // Tier 2: not the instance token — try the same hex as an EDGE token.
        AuthOutcome::Denied => {
            let Some((_, _, _, ctx)) = resolve_active(active) else {
                log_denial(
                    AUDIT_SUBSYSTEM,
                    "auth",
                    aterm_containment::mode_or_containment(),
                    "edge token presented with no active terminal context",
                );
                let _ = writer.write_all(b"ERR auth\n");
                let _ = writer.flush();
                return ServeDisposition::Close;
            };
            match edge_scope_from_first_line(&first, &ctx) {
                // The connect-time op proves the token is a LIVE edge (else None ->
                // `ERR auth`); it is not stored — per-request `decide_edge` re-derives it.
                Some((_op, tok, verb)) => (Scope::Edge(tok), verb),
                None => {
                    log_denial(
                        AUDIT_SUBSYSTEM,
                        "auth",
                        aterm_containment::mode_or_containment(),
                        "missing or invalid capability/edge token",
                    );
                    let _ = writer.write_all(b"ERR auth\n");
                    let _ = writer.flush();
                    return ServeDisposition::Close;
                }
            }
        }
    };

    // Preserve the established slow-cadence persistent-driver contract: once
    // authenticated, this connection may idle indefinitely and still follows
    // active-tab changes per request. A short kernel wake-up tick is retried by
    // `read_authenticated_request_line`, so it is NOT an application idle
    // deadline. It avoids a macOS AF_UNIX edge where a blocking `recvfrom` can
    // remain asleep after the peer has closed. Availability comes from lane-exact
    // admission at accept time (excess peers get an immediate retry response),
    // not a surprise idle EOF. Push subscriptions move to their own pool below.
    if arm_authenticated_read_poll(stream).is_err() {
        return ServeDisposition::Close;
    }

    // A folded-in verb runs first (empty tail = bare TOKEN line, just an ack), and
    // runs through the SAME handler as a request line — a folded-in verb that
    // behaved differently from the identical verb sent on its own line would be a
    // protocol wart nobody could see from either call site.
    if let Some(verb) = inline_verb
        && !verb.is_empty()
        && let Some(disposition) = serve_request_line(
            verb,
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
            operator,
        )
    {
        return disposition;
    }

    while let Some(line) = read_authenticated_request_line(&mut reader) {
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
            operator,
        ) {
            return disposition;
        }
    }
    ServeDisposition::Close
}

/// Handle ONE authenticated request, whether it arrived folded into the `TOKEN`
/// handshake line or on its own line in the polling loop.
///
/// `None` = handled, keep polling. `Some(disposition)` ends this lane exactly as
/// the caller's own `return` did. Both callers ran this identical sequence —
/// proxy forward, TLS dial, subscribe flip, binary frame, dispatch+reply — and
/// keeping two copies meant a new early-exit verb had to be added to both, with
/// nothing to catch it if only one was.
#[allow(clippy::too_many_arguments)]
fn serve_request_line(
    line: String,
    scope: Scope,
    stream: &CtlStream,
    reader: &mut BufReader<&CtlStream>,
    writer: &mut &CtlStream,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    proxy: &EventLoopProxy<Wake>,
    queue: &ImageQueue,
    sock_dir: &std::path::Path,
    operator: Option<&crate::operator_host::ControlHandle>,
) -> Option<ServeDisposition> {
    // Embedded operator verbs are self-scoped Owner operations. Intercept them
    // before cross-process proxying so `@other operator...` cannot redirect the
    // instance's queue authority to a child socket.
    if let Some((selector, rest)) = operator_command_request(&line) {
        let response = if !matches!(scope, Scope::Owner)
            || !matches!(selector, None | Some(Selector::SelfTok))
        {
            "ERR denied\n".to_string()
        } else if let Some(operator) = operator {
            operator.command(store, rest)
        } else {
            "ERR operator unavailable\n".to_string()
        };
        if writer.write_all(response.as_bytes()).is_err() || writer.flush().is_err() {
            return Some(ServeDisposition::Close);
        }
        return None;
    }
    if binary_frame_verb(&line) == Some("operator-propose-bin") {
        if !run_operator_proposal_bin(&line, reader, scope, operator, store, subscribers, writer) {
            return Some(ServeDisposition::Close);
        }
        return None;
    }
    // Cross-process forward (Item 5b): relay a `@<child-sid>` we spawned but
    // don't host to the child's socket; the relay then owns the connection.
    if try_proxy_forward(&line, scope, store, sock_dir, stream, reader) {
        return Some(ServeDisposition::Close);
    }
    // Network drive: `dial <name>` relays this connection over TLS to a saved
    // remote aterm; the relay then owns the connection.
    if try_net_dial(&line, scope, stream, reader) {
        return Some(ServeDisposition::Close);
    }
    // PUSH FLIP: `subscribe` authorizes its targets EXACTLY like a read verb,
    // then this connection becomes push-only (never reads another line). On an
    // auth/parse failure `run_subscribe` writes a single `ERR ...` and returns,
    // and we close the connection (a half-subscribed connection is meaningless).
    // Transferred to the reserved push pool so a quiet subscriber never consumes
    // an ordinary RPC worker.
    if is_subscribe_line(&line) {
        return Some(ServeDisposition::Subscribe { line, scope });
    }
    // BINARY FRAME: `feed-bin`/`paste-bin <n>` consumes the following N raw bytes
    // from the SAME buffered stream and feeds them to the resolved target's PTY —
    // the length-prefixed (vs hex) wire form. `feed-bin` writes raw; `paste-bin`
    // applies paste semantics. Both authorize EXACTLY like `feed` (WriteInput) via
    // the normal `@<selector>` + op gate inside `run_feed_bin`.
    if let Some(bin_verb) = binary_frame_verb(&line) {
        // The operator's own binary frame never reaches this generic path: it
        // is dispatched by the operator host, which owns the guarded actuator.
        debug_assert_ne!(bin_verb, "operator-propose-bin");
        let mut dispatch_front_input =
            |event, session| post_input_reply_to(proxy, Op::WriteInput, vec![event], session);
        let mut clear_license = |session| front_routed_license_clear(proxy, session);
        if !run_feed_bin_routed(
            &line,
            bin_verb,
            reader,
            FeedBinRoute {
                active,
                store,
                scope,
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
    match write_control_reply(writer, resp) {
        Ok(Some(retention)) => {
            await_guarded_reply_close(stream, reader, retention);
            Some(ServeDisposition::Close)
        }
        Ok(None) => None,
        Err(_) => Some(ServeDisposition::Close),
    }
}

/// Read one newline-delimited request line from the buffered control stream,
/// stripping the trailing `\n` (and a `\r` so CRLF clients still work) — exactly
/// the line shape the `lines()` iterator used to yield. `None` on EOF or a read
/// error (the client hung up) OR on a line longer than [`MAX_REQUEST_LINE`] (a
/// runaway/abusive client is dropped rather than buffered unboundedly).
fn read_request_line(reader: &mut impl BufRead) -> Option<String> {
    read_request_line_with_idle_retry(reader, false)
}

/// Read one authenticated polling request without imposing an application idle
/// deadline. Kernel timeout ticks are retried indefinitely, retaining any
/// partially received line, while EOF and real I/O errors still end the lane.
fn read_authenticated_request_line(reader: &mut impl BufRead) -> Option<String> {
    read_request_line_with_idle_retry(reader, true)
}

fn read_request_line_with_idle_retry(
    reader: &mut impl BufRead,
    retry_idle_timeout: bool,
) -> Option<String> {
    let mut buf = Vec::with_capacity(64);
    loop {
        let mut byte = [0u8; 1];
        match reader.read(&mut byte) {
            Ok(0) => {
                // EOF: yield a final unterminated line if any, else stop.
                return if buf.is_empty() {
                    None
                } else {
                    Some(decode_request_line(buf))
                };
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Some(decode_request_line(buf));
                }
                if buf.len() >= MAX_REQUEST_LINE {
                    return None; // runaway line: drop the connection
                }
                buf.push(byte[0]);
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

/// Move an authenticated push subscription out of the ordinary RPC pool. The
/// push queue is independently bounded; saturation returns a prompt, explicit
/// error and the RPC worker is immediately reusable.
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

/// The upper bound on a single control REQUEST line (not a `feed-bin` payload,
/// which is length-prefixed and bounded separately). Generous for any real verb;
/// a line past it is treated as an abusive client and the connection is dropped.
const MAX_REQUEST_LINE: usize = 64 * 1024;

/// Read deadline for the UNAUTHENTICATED phase of a served control connection
/// (up to and including the `AUTH`/`TOKEN` first line). Generous for any real
/// client — aterm-ctl authenticates immediately on connect — while bounding how
/// long a silent same-uid peer can retain its already-reserved lane before token
/// presentation. Excess connections still receive a prompt busy/retry response.
const AUTH_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Kernel wake-up cadence for authenticated polling sockets. This is not an
/// idle timeout: [`read_authenticated_request_line`] retries every tick forever.
const AUTHENTICATED_READ_POLL: std::time::Duration = std::time::Duration::from_millis(250);

fn arm_authenticated_read_poll(stream: &CtlStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(AUTHENTICATED_READ_POLL))
}

/// Turn a raw line's bytes (newline already consumed) into the `String` the
/// dispatch expects: strip a trailing `\r`, then UTF-8 lossily (a control line is
/// ASCII; lossy keeps a malformed byte from killing the whole connection).
fn decode_request_line(mut buf: Vec<u8>) -> String {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Whether a request line is the `subscribe` verb (its first whitespace-delimited
/// token). Used by [`serve`] to FLIP the connection to push mode before the normal
/// per-verb dispatch — so the ~29 polling verbs are reached byte-identically and
/// only `subscribe` diverts into the push path.
fn is_subscribe_line(line: &str) -> bool {
    let line = line.strip_suffix('\r').unwrap_or(line);
    matches!(line.split_whitespace().next(), Some("subscribe"))
}

fn operator_command_request(line: &str) -> Option<(Option<Selector>, &str)> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let (selector, command) = match line.split_once(char::is_whitespace) {
        Some((first, tail)) if first.starts_with('@') => {
            (Some(Selector::parse(&first[1..])), tail.trim_start())
        }
        None if line.starts_with('@') => (Some(Selector::parse(&line[1..])), ""),
        _ => (None, line),
    };
    let (verb, rest) = command
        .split_once(char::is_whitespace)
        .map_or((command, ""), |(verb, rest)| (verb, rest.trim_start()));
    (verb == "operator").then_some((selector, rest))
}

/// The maximum `feed-bin` payload accepted in one frame (256 KiB) — large enough
/// for a bracketed-paste or a control burst, bounded so a hostile/garbled length
/// cannot make the server `read_exact` an unbounded payload.
const MAX_FEED_BIN: usize = 256 * 1024;
/// A schema-1 proposal contains bounded terminal text, not an arbitrary paste.
const MAX_OPERATOR_PROPOSAL: usize = aterm_types::control_verbs::MAX_OPERATOR_PROPOSAL_BYTES;

/// Maximum number of `@<sel>` targets accepted in one `subscribe` request. Each
/// accepted target installs a `ByteFanout` slot that the producer's PTY reader
/// tees on EVERY output burst, so an unbounded comma list — e.g. `@.,@.,…` — would
/// make that per-burst tee O(N) on the hot reader thread and reserve N queues. A
/// legit client subscribes to at most a handful of sessions; 256 is generous
/// headroom. Selectors are also de-duplicated by session, so repeats collapse.
pub(crate) const MAX_SUBSCRIBE_TARGETS: usize = 256;

/// Whether a request line is the `feed-bin` verb (optionally `@<sel>`-prefixed),
/// so [`serve`] reads its length-prefixed payload from the SAME stream BEFORE the
/// normal per-line dispatch (which only sees one line and cannot reach the bytes).
/// The binary-input frame verbs, both intercepted in `serve` BEFORE dispatch (they
/// consume N raw payload bytes off the same stream). `feed-bin` writes the bytes RAW;
/// `paste-bin` routes them through the PASTE seam (bracketing + sanitize). Named
/// here so the serve-loop-verb sync test scrapes BOTH literals and requires each in
/// the VERBS table.
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
        Some("operator-propose-bin") => Some("operator-propose-bin"),
        _ => None,
    }
}

/// Resolve only the target-bearing prefix of a binary input header.  This is
/// deliberately independent of the length/trailing-argument parser: once an
/// authenticated client has named a real write verb and target, even a malformed
/// or over-cap attempt is a newer input boundary for that target's cursor licence.
/// Unknown verbs return `None`, and authorization is still checked separately
/// before the boundary is allowed to mutate visible state.
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

/// Outcome of parsing a `[@<sel>] feed-bin <n>` request line. The two failure
/// modes demand OPPOSITE stream handling because a `feed-bin <n>\n<bytes>` frame
/// pipelines its payload on the SAME stream:
/// - [`Malformed`](FeedBinFrame::Malformed): no parseable length, so no payload
///   was announced — safe to reply `ERR` and keep the connection framed.
/// - [`TooLarge`](FeedBinFrame::TooLarge): a valid length past [`MAX_FEED_BIN`].
///   Per the wire form the client has ALREADY pipelined N bytes we refuse to read
///   (reading N — attacker-controlled, unbounded — would defeat the cap and is
///   itself a DoS), so the stream is unrecoverably desynced past this frame and
///   the caller MUST close the connection rather than let the payload fall
///   through and execute as control verbs.
enum FeedBinFrame {
    /// Not a well-formed feed-bin line (missing / non-numeric length).
    Malformed,
    /// Well-formed but the declared length exceeds [`MAX_FEED_BIN`].
    TooLarge,
    /// A valid frame: optional selector + payload length (`<= MAX_FEED_BIN`).
    Ok(Option<Selector>, usize),
}

/// Parse a `[@<sel>] feed-bin <n>` request line. Pure, so the framing parse is
/// unit-testable. See [`FeedBinFrame`] for why a valid-but-oversize length is
/// distinguished from a malformed line (they require different stream handling).
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
    // Reject trailing tokens: the canonical form is exactly `[@sel] feed-bin <n>`.
    // `feed-bin 1 junk` must NOT parse as a valid 1-byte frame — that would consume
    // a byte of the following pipelined request line and desync the stream. A
    // trailing token means the line is malformed (no announced payload), so keep
    // the connection and consume nothing. (Trailing WHITESPACE is already dropped
    // by split_whitespace, so only a real extra token trips this.)
    if it.next().is_some() {
        return FeedBinFrame::Malformed;
    }
    let Ok(n) = len_tok.parse::<usize>() else {
        return FeedBinFrame::Malformed;
    };
    if n > MAX_FEED_BIN {
        return FeedBinFrame::TooLarge;
    }
    FeedBinFrame::Ok(selector, n)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorProposalWire {
    schema: u64,
    event_id: u64,
    claim_token: String,
    sid: String,
    generation: OperatorGenerationWire,
    action: OperatorActionWire,
    expectation: OperatorExpectationWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorGenerationWire {
    lifecycle_epoch: u64,
    alternate_screen: bool,
    content_seq: u64,
    fingerprint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorActionWire {
    kind: String,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorExpectationWire {
    kind: String,
    deadline_ms: u64,
}

struct OperatorProposal {
    event_id: aterm_agent::operator::EventId,
    token: aterm_agent::operator::ClaimToken,
    sid: String,
    generation: aterm_agent::operator::EventGeneration,
    text: String,
    action_hash: String,
}

fn decode_operator_proposal(payload: &[u8]) -> Result<OperatorProposal, String> {
    let wire: OperatorProposalWire = serde_json::from_slice(payload)
        .map_err(|error| format!("invalid proposal JSON: {error}"))?;
    if wire.schema != 1 {
        return Err("unsupported proposal schema".to_string());
    }
    if wire.action.kind != "turn" || wire.expectation.kind != "busy_then_attention" {
        return Err("unsupported operator action or expectation".to_string());
    }
    if wire.action.text.is_empty() || wire.action.text.len() > 16 * 1024 {
        return Err("turn text must contain 1..=16384 bytes".to_string());
    }
    if wire.action.text.chars().any(char::is_control) {
        return Err("turn text contains unsupported control bytes".to_string());
    }
    if wire.action.text.trim() != wire.action.text {
        return Err("turn text must not have leading or trailing whitespace".to_string());
    }
    if looks_like_operator_approval_response(&wire.action.text) {
        return Err("approval and permission responses are human-only".to_string());
    }
    if !(1..=600_000).contains(&wire.expectation.deadline_ms) {
        return Err("deadline_ms must be in 1..=600000".to_string());
    }
    let fingerprint = decode_hex_32(&wire.generation.fingerprint)
        .ok_or_else(|| "generation fingerprint must be 64 lowercase hex characters".to_string())?;
    let event_id = aterm_agent::operator::EventId::from_wire(wire.event_id)
        .map_err(|error| error.to_string())?;
    let token = aterm_agent::operator::ClaimToken::from_wire(&wire.claim_token)
        .map_err(|error| error.to_string())?;
    let generation = aterm_agent::operator::EventGeneration::new(
        wire.generation.lifecycle_epoch,
        wire.generation.alternate_screen,
        wire.generation.content_seq,
        fingerprint,
    );
    let mut digest = Sha256::new();
    digest.update(b"aterm.operator.turn.v1\0");
    digest.update((wire.action.text.len() as u64).to_le_bytes());
    digest.update(wire.action.text.as_bytes());
    digest.update(wire.expectation.deadline_ms.to_le_bytes());
    let action_hash = hex_lower(&digest.finalize());
    Ok(OperatorProposal {
        event_id,
        token,
        sid: wire.sid,
        generation,
        text: wire.action.text,
        action_hash,
    })
}

fn looks_like_operator_approval_response(text: &str) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    text.lines().any(|line| approval_answer(line.trim()))
}

fn approval_answer(answer: &str) -> bool {
    let answer = answer.to_ascii_lowercase();
    answer.parse::<u32>().is_ok()
        || matches!(
            answer.as_str(),
            "y" | "yes"
                | "n"
                | "no"
                | "allow"
                | "approve"
                | "approved"
                | "deny"
                | "denied"
                | "reject"
                | "rejected"
                | "enter"
                | "escape"
                | "esc"
                | "ctrl-c"
        )
}

fn decode_hex_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || value
            .as_bytes()
            .iter()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return None;
    }
    let mut out = [0_u8; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        out[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Some(out)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn validate_operator_claim(
    queue: &aterm_agent::operator::DurableQueue,
    proposal: &OperatorProposal,
) -> Result<(), String> {
    use aterm_agent::operator::{AttentionCondition, EventStatus};

    let snapshot = queue
        .snapshot(proposal.event_id)
        .map_err(|error| error.to_string())?;
    if snapshot.sid != proposal.sid || snapshot.generation != proposal.generation {
        return Err("proposal does not match the claimed event generation".to_string());
    }
    if snapshot.condition != AttentionCondition::Ready || snapshot.escalated {
        return Err("only a non-escalated ready event may actuate".to_string());
    }
    match snapshot.status {
        EventStatus::Delivered { token, .. } if token == proposal.token => Ok(()),
        _ => Err("proposal claim is stale or no longer delivered".to_string()),
    }
}

/// Validate the same claimed event on either side of `begin_action`. Before the
/// durable append it must still be Delivered; after the append the exact same
/// token/hash must be ActionInFlight. No unrelated in-flight intent is admitted.
fn validate_operator_action_state(
    queue: &aterm_agent::operator::DurableQueue,
    proposal: &OperatorProposal,
    require_intent: bool,
) -> Result<(), String> {
    use aterm_agent::operator::{AttentionCondition, EventStatus};

    let snapshot = queue
        .snapshot(proposal.event_id)
        .map_err(|error| error.to_string())?;
    if snapshot.sid != proposal.sid || snapshot.generation != proposal.generation {
        return Err("proposal does not match the claimed event generation".to_string());
    }
    if snapshot.condition != AttentionCondition::Ready || snapshot.escalated {
        return Err("only a non-escalated ready event may actuate".to_string());
    }
    match snapshot.status {
        EventStatus::Delivered { token, .. } if !require_intent && token == proposal.token => {
            Ok(())
        }
        EventStatus::ActionInFlight {
            token,
            action_class,
            action_hash,
            ..
        } if token == proposal.token
            && action_class == "turn"
            && action_hash == proposal.action_hash =>
        {
            Ok(())
        }
        EventStatus::Delivered { .. } if require_intent => {
            Err("operator action intent is not durable".to_string())
        }
        _ => Err("proposal claim or durable action intent is stale".to_string()),
    }
}

/// Revalidate every live fact the actuator relies on, bracketing the screen read
/// with the sink's attempted-input epoch. The second epoch read catches input
/// that arrived while the snapshot was being classified; the conditional sink
/// write catches anything that arrives after this function returns.
fn validate_operator_live(
    operator: &crate::operator_host::ControlHandle,
    queue: &aterm_agent::operator::DurableQueue,
    proposal: &OperatorProposal,
    store: &Store,
    sink: &aterm_session::sink::SinkWriter,
    expected_epoch: InputEpoch,
) -> Result<(), String> {
    use crate::session_store::SessionState;

    operator.ensure_accepting_new_work()?;
    validate_operator_action_state(queue, proposal, false)?;
    if sink.input_epoch() != expected_epoch {
        return Err("target input interjected".to_string());
    }
    let live = operator.current_snapshot(store, &proposal.sid)?;
    if live.state != SessionState::Alive || live.generation != proposal.generation {
        return Err("target generation changed before input".to_string());
    }
    // Defense in depth: a delivered Ready may have been strengthened by a
    // later coalesced approval observation. The live presentation always wins.
    if crate::operator_host::looks_like_approval(&live.evidence) {
        return Err("approval-shaped screens are human-only".to_string());
    }
    if sink.input_epoch() != expected_epoch {
        return Err("target input interjected".to_string());
    }
    Ok(())
}

/// Capture the post-paste screen identity after echo settle. Unlike the initial
/// preflight, this deliberately does not compare content_seq/fingerprint with
/// the claimed Ready event: the operator's own paste is expected to change both.
/// The returned identity is compared again while holding the terminal lock across
/// the Enter syscall.
fn capture_operator_submit_generation(
    operator: &crate::operator_host::ControlHandle,
    queue: &aterm_agent::operator::DurableQueue,
    proposal: &OperatorProposal,
    store: &Store,
    sink: &aterm_session::sink::SinkWriter,
    expected_epoch: InputEpoch,
) -> Result<aterm_agent::operator::EventGeneration, String> {
    use crate::session_store::SessionState;

    operator.ensure_accepting_new_work()?;
    validate_operator_action_state(queue, proposal, true)?;
    if sink.input_epoch() != expected_epoch {
        return Err("target input interjected".to_string());
    }
    let live = operator.current_snapshot(store, &proposal.sid)?;
    if live.state != SessionState::Alive
        || live.generation.lifecycle_epoch != proposal.generation.lifecycle_epoch
        || live.generation.alternate_screen != proposal.generation.alternate_screen
    {
        return Err("target lifecycle changed before submit".to_string());
    }
    if crate::operator_host::looks_like_approval(&live.evidence) {
        return Err("approval-shaped screens are human-only".to_string());
    }
    if sink.input_epoch() != expected_epoch {
        return Err("target input interjected".to_string());
    }
    Ok(live.generation)
}

fn run_operator_proposal(
    payload: &[u8],
    operator: &crate::operator_host::ControlHandle,
    store: &Store,
    subscribers: &Subscribers,
) -> String {
    if let Err(error) = operator.ensure_accepting_new_work() {
        return format!("ERR {error}\n");
    }
    let proposal = match decode_operator_proposal(payload) {
        Ok(proposal) => proposal,
        Err(error) => return format!("ERR {error}\n"),
    };
    // Keep process replacement from crossing the proposal's durable-intent /
    // PTY-egress / durable-result transaction. The reversible update fence
    // refuses while this token exists; every return path drops it.
    let _action_activity = match operator.begin_action_activity() {
        Ok(activity) => activity,
        Err(error) => return format!("ERR {error}\n"),
    };
    let queue = match operator.queue() {
        Ok(queue) => queue,
        Err(error) => return format!("ERR {error}\n"),
    };
    if let Err(error) = validate_operator_claim(&queue, &proposal) {
        return format!("ERR {error}\n");
    }
    let target = {
        let guard = store
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.by_sid(&SessionId::new(&proposal.sid)).cloned()
    };
    let Some(target) =
        target.filter(|target| target.state == crate::session_store::SessionState::Alive)
    else {
        return "ERR target session is not alive\n".to_string();
    };
    // Capture before the first live screen read. An input attempt after this
    // point can never be silently adopted as part of the operator's baseline.
    let input_epoch = std::cell::Cell::new(target.ctx.sink.input_epoch());
    let submit_generation = std::cell::Cell::new(None);
    if let Err(error) = validate_operator_live(
        operator,
        &queue,
        &proposal,
        store,
        &target.ctx.sink,
        input_epoch.get(),
    ) {
        return format!("ERR {error}\n");
    }

    // Operator actuation deliberately uses only paste semantics plus one named
    // Enter press. It never exposes the raw send/key/control vocabulary.
    let input_failure = std::cell::Cell::new(None::<OperatorInputFailure>);
    let paste = |text: &str| {
        let delivery = operator
            .with_actuation_permit(
                &queue,
                proposal.event_id,
                &proposal.token,
                &proposal.sid,
                &proposal.action_hash,
                || {
                    operator_input_if_epoch(
                        &target.term,
                        &target.ctx,
                        Some(operator_paste_event(text)),
                        input_epoch.get(),
                        OperatorTerminalFence::Exact(proposal.generation),
                    )
                },
            )
            .unwrap_or(Delivery::ConflictZero);
        match delivery {
            Delivery::FullAt { epoch } => {
                input_epoch.set(epoch);
                true
            }
            delivery => {
                input_failure.set(Some(OperatorInputFailure {
                    stage: OperatorInputStage::Paste,
                    delivery,
                }));
                false
            }
        }
    };
    let press = |name: &str| {
        if name != "enter" {
            return false;
        }
        let Some(generation) = submit_generation.get() else {
            return false;
        };
        let delivery = operator
            .with_actuation_permit(
                &queue,
                proposal.event_id,
                &proposal.token,
                &proposal.sid,
                &proposal.action_hash,
                || {
                    operator_input_if_epoch(
                        &target.term,
                        &target.ctx,
                        parse_key(name),
                        input_epoch.get(),
                        OperatorTerminalFence::Exact(generation),
                    )
                },
            )
            .unwrap_or(Delivery::ConflictZero);
        match delivery {
            Delivery::FullAt { epoch } => {
                input_epoch.set(epoch);
                true
            }
            delivery => {
                input_failure.set(Some(OperatorInputFailure {
                    stage: OperatorInputStage::Submit,
                    delivery,
                }));
                false
            }
        }
    };
    let validate = || -> Result<(), String> {
        validate_operator_live(
            operator,
            &queue,
            &proposal,
            store,
            &target.ctx.sink,
            input_epoch.get(),
        )
    };
    // `expectation.deadline_ms` is client-advisory metadata describing when that
    // client plans to inspect downstream state. The resident service does not
    // schedule or wait to that deadline, and it never expands the foreground
    // actuator budget. One proposal occupies an ordinary control lane for at
    // most nine seconds of turn orchestration.
    let turn_request = format!(
        "idle=750 timeout=9000 submit_window=1500 presses=1 submit_verify=seq -- {}",
        proposal.text
    );
    let response = operator_action_transaction(
        &queue,
        proposal.event_id,
        &proposal.token,
        &proposal.action_hash,
        validate,
        |preflight| {
            let mut pre_submit = || {
                let generation = capture_operator_submit_generation(
                    operator,
                    &queue,
                    &proposal,
                    store,
                    &target.ctx.sink,
                    input_epoch.get(),
                )?;
                submit_generation.set(Some(generation));
                Ok(())
            };
            let response = control_session::cmd_turn_guarded(
                &target.term,
                store,
                target.local_id,
                &turn_request,
                subscribers,
                &target.ctx,
                &control_session::TurnIo {
                    paste: &paste,
                    press: &press,
                },
                preflight,
                &mut pre_submit,
                Some("[operator action]"),
            );
            input_failure
                .get()
                .map_or(response, OperatorInputFailure::response)
        },
    );
    operator.notify();
    response
}

/// Proposal text is JSON data, not `operator` command-line syntax. In
/// particular a trailing two-byte `\n` remains two literal bytes; only the
/// interactive text `paste` verb applies its convenience expansion.
fn operator_paste_event(text: &str) -> InputEvent {
    InputEvent::Paste(text.to_string())
}

/// Execute one proposed side effect behind its durable intent transaction.
///
/// The executor receives the exact preflight it must call at its own final
/// pre-mutation seam (`cmd_turn_guarded` does so after taking the turn lease).
/// Preflight validates both before and after the durable intent append/fsync:
/// input or state that changes while durability is established therefore stops
/// before paste. Tests inject a counting executor through this same production
/// helper to bind WAL-before-mutation, exactly-once, and no-retry behavior.
fn operator_action_transaction<V, E>(
    queue: &aterm_agent::operator::DurableQueue,
    event_id: aterm_agent::operator::EventId,
    token: &aterm_agent::operator::ClaimToken,
    action_hash: &str,
    mut validate: V,
    execute_once: E,
) -> String
where
    V: FnMut() -> Result<(), String>,
    E: FnOnce(&mut dyn FnMut() -> Result<(), String>) -> String,
{
    use aterm_agent::operator::Resolution;

    let mut intent_started = false;
    let mut preflight = || -> Result<(), String> {
        validate()?;
        queue
            .begin_action(event_id, token, "turn", action_hash)
            .map_err(|error| error.to_string())?;
        intent_started = true;
        // The WAL fsync is intentionally inside the validation sandwich. A
        // human/controller may type, or an approval prompt may appear, while the
        // durable write is in flight; never paste from the pre-fsync snapshot.
        validate()?;
        Ok(())
    };
    let response = execute_once(&mut preflight);
    if !intent_started {
        return response;
    }
    let summary = operator_result_summary(&response);
    let submitted = summary
        .split_whitespace()
        .any(|field| field == "submitted=1");
    if !submitted {
        let reason = format!("turn submission/result ambiguous: {summary}");
        let _ = queue.mark_action_in_doubt(event_id, token, &reason);
        return format!("ERR operator action in doubt: {summary}\n");
    }
    match queue.finish_action(event_id, token, action_hash, &summary, Resolution::Acted) {
        // Never return cmd_turn's screen body. The durable summary admits only a
        // fixed set of scalar fields, and the wire reply is rebuilt from it.
        Ok(_) => compact_operator_success(&summary),
        Err(error) => {
            // Error strings are generated by the queue and contain identifiers,
            // never terminal rows or proposal text.
            let reason = format!("could not persist turn result: {error}");
            let _ = queue.mark_action_in_doubt(event_id, token, &reason);
            format!("ERR operator action in doubt: {error}\n")
        }
    }
}

fn compact_operator_success(summary: &str) -> String {
    let metadata = summary
        .strip_prefix("turn ")
        .unwrap_or("result=unavailable");
    format!("OK operator action=turn outcome=acted {metadata}\n")
}

fn operator_result_summary(response: &str) -> String {
    let status = response.lines().next().unwrap_or("ERR missing turn result");
    if let Some(summary) = operator_input_error_summary(status) {
        return summary;
    }
    let mut fields = status.split_whitespace();
    if fields.next() != Some("OK")
        || fields
            .next()
            .is_none_or(|rows| rows.parse::<usize>().is_err())
        || fields.next() != Some("turn")
    {
        return "turn result unavailable".to_string();
    }
    let admitted = fields
        .filter(|field| {
            ["submitted=", "status=", "seq=", "id=", "dur_ms=", "hash="]
                .iter()
                .any(|prefix| field.starts_with(prefix))
                && field
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'=' | b'-' | b'_'))
        })
        .collect::<Vec<_>>();
    format!("turn {}", admitted.join(" "))
}

/// Admit only the service-generated operator-input error grammar into the WAL
/// summary.  This retains the safety-critical zero-vs-partial distinction while
/// excluding proposal text and terminal evidence from the durable reason.
fn operator_input_error_summary(status: &str) -> Option<String> {
    let mut fields = status.split_whitespace();
    if fields.next() != Some("ERR")
        || fields.next() != Some("operator")
        || fields.next() != Some("input")
    {
        return None;
    }
    let stage = match fields.next()? {
        "paste" => "paste",
        "submit" => "submit",
        _ => return None,
    };
    match fields.next()? {
        "busy-zero" if fields.next().is_none() => {
            Some(format!("turn input={stage} outcome=busy-zero"))
        }
        "conflict-zero" if fields.next().is_none() => {
            Some(format!("turn input={stage} outcome=conflict-zero"))
        }
        "partial" => {
            let accepted = fields.next()?.strip_prefix("accepted=")?;
            let accepted = accepted.parse::<usize>().ok()?;
            if accepted == 0 || fields.next().is_some() {
                return None;
            }
            Some(format!(
                "turn input={stage} outcome=partial accepted={accepted}"
            ))
        }
        _ => None,
    }
}

fn run_operator_proposal_bin<W: Write>(
    line: &str,
    reader: &mut impl BufRead,
    scope: Scope,
    operator: Option<&crate::operator_host::ControlHandle>,
    store: &Store,
    subscribers: &Subscribers,
    writer: &mut W,
) -> bool {
    let (selector, n) = match parse_feed_bin(line, "operator-propose-bin") {
        FeedBinFrame::Ok(selector, n) => (selector, n),
        FeedBinFrame::Malformed => {
            let _ = writer.write_all(b"ERR usage: operator-propose-bin <n> then <n> JSON bytes\n");
            let _ = writer.flush();
            return true;
        }
        FeedBinFrame::TooLarge => {
            let _ = writer.write_all(b"ERR operator-propose-bin too large\n");
            let _ = writer.flush();
            return false;
        }
    };
    // These failures make the announced frame unusable. Refuse and CLOSE before
    // touching its body: reading attacker-chosen bytes after an authority,
    // routing, availability, or operator-specific size failure needlessly parks
    // a scarce control lane and contradicts the fail-closed framing contract.
    let pre_body_error = if !matches!(scope, Scope::Owner) || selector.is_some() {
        Some("ERR denied\n")
    } else if operator.is_none() {
        Some("ERR operator unavailable\n")
    } else if n > MAX_OPERATOR_PROPOSAL {
        Some("ERR operator proposal too large\n")
    } else {
        None
    };
    if let Some(error) = pre_body_error {
        let _ = writer.write_all(error.as_bytes());
        let _ = writer.flush();
        return false;
    }
    let mut payload = vec![0_u8; n];
    let body_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    if read_exact_authenticated_until(reader, &mut payload, body_deadline).is_err() {
        let _ = writer.write_all(b"ERR operator proposal body timeout\n");
        let _ = writer.flush();
        return false;
    }
    let response = run_operator_proposal(
        &payload,
        operator.expect("availability checked before body read"),
        store,
        subscribers,
    );
    writer.write_all(response.as_bytes()).is_ok() && writer.flush().is_ok()
}

/// Handle a `feed-bin <n>\n<bytes>` (or `@<sel> feed-bin <n>\n<bytes>`) frame: read
/// the declared `n` RAW bytes that FOLLOW the request line from the SAME buffered
/// stream, then route them to the resolved target's PTY EXACTLY as `feed` does — a
/// length-prefixed binary feed that halves an agent's wire cost vs hex. Returns
/// `true` to keep the connection (the reply was written), `false` to close it (a
/// dead writer or an unrecoverable framing error after the payload was consumed).
///
/// AUTH: identical to `feed` — `WriteInput`, per-target. Framing handling depends on
/// the parse outcome ([`FeedBinFrame`]): a MALFORMED line (no parseable length)
/// announced no payload, so it replies `ERR usage` and keeps the connection framed;
/// a valid-but-OVER-CAP length means the client already pipelined N bytes we refuse
/// to read (reading unbounded N would defeat [`MAX_FEED_BIN`]), so the stream is
/// desynced past this frame and the connection is CLOSED (`return false`) rather than
/// reinterpreting the payload as control verbs. Once a VALID (<= cap) length is parsed
/// the N bytes are ALWAYS consumed (even on an auth denial), so the next request line
/// is correctly framed — a denial reads-and-discards the payload, then replies
/// `ERR denied`.
#[cfg(test)]
fn run_feed_bin<W: Write>(
    line: &str,
    verb: &str,
    reader: &mut impl BufRead,
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
    writer: &mut W,
) -> bool {
    let mut dispatch = |event, _session| {
        let Some((term, _, _, ctx)) = resolve_active(active) else {
            return Err("ERR input dispatch unavailable\n".to_string());
        };
        seam_egress(&term, &ctx.sink, &event, EgressMode::Backpressured);
        Ok(InputOutcome::Ok)
    };
    let mut clear_license = |_session| "OK\n".to_string();
    run_feed_bin_routed(
        line,
        verb,
        reader,
        FeedBinRoute {
            active,
            store,
            scope,
        },
        &mut dispatch,
        &mut clear_license,
        writer,
    )
}

#[derive(Clone, Copy)]
struct FeedBinRoute<'a> {
    active: &'a ActiveHandle,
    store: &'a Store,
    scope: Scope,
}

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
    // `paste-bin` routes the payload through the PASTE seam (bracketing + sanitize +
    // LF->CR under the target's lock), `feed-bin` writes it RAW — otherwise the two
    // share the entire framing/auth/lease/floor path below.
    let paste = verb == "paste-bin";
    // Resolve and authorize the target from the header PREFIX before waiting for
    // its payload.  A client may legally trickle a bounded binary frame for much
    // longer than the key-hint freshness window; leaving the old licence
    // stamped until `read_exact` completed let unrelated child output borrow it.
    // Malformed and over-cap attempts use this same prefix rule: a resolvable,
    // authorized target is fenced; unknown/unauthorized targets never mutate UI
    // state.  Framing behavior below remains unchanged.
    let attempt_selector = binary_frame_attempt_selector(line, verb);
    let header_active_target = resolve_active(route.active);
    let header_target = attempt_selector.as_ref().and_then(|selector| match selector {
        None | Some(Selector::SelfTok) => header_active_target.clone(),
        Some(sel) => resolve_explicit(route.store, sel),
    });
    let header_authorized = header_target.as_ref().is_some_and(|(_, _, _, ctx)| {
        matches!(route.scope, Scope::Owner)
            || cross_session_authorized(route.scope, "feed", ctx)
    });
    if header_authorized
        && let Some((_, _, session, _)) = header_target.as_ref()
    {
        let response = clear_license(*session);
        if !response.starts_with("OK") {
            // Do not wait for a slow payload while the header target's old
            // movement licence is still live. The main-thread fence failed, so
            // this framed connection is no longer safe to continue.
            let _ = writer.write_all(response.as_bytes());
            let _ = writer.flush();
            return false;
        }
    }

    let (selector, n) = match parse_feed_bin(line, verb) {
        FeedBinFrame::Ok(selector, n) => (selector, n),
        FeedBinFrame::Malformed => {
            // No parseable length ⇒ no payload was announced; a client that merely
            // typo'd the verb stays framed. No payload read.
            let _ =
                writer.write_all(format!("ERR usage: {verb} <n> then <n> raw bytes\n").as_bytes());
            let _ = writer.flush();
            return true;
        }
        FeedBinFrame::TooLarge => {
            // Valid length past MAX_FEED_BIN: the client has (per the wire form)
            // already pipelined N bytes on this stream. We refuse to read them
            // (unbounded N is the DoS the cap exists to stop) and cannot skip past
            // them, so the stream is unrecoverably desynced — CLOSE the connection
            // instead of letting the payload bytes fall through to the next
            // read_request_line and dispatch as control verbs.
            let _ = writer.write_all(format!("ERR {verb} too large\n").as_bytes());
            let _ = writer.flush();
            return false;
        }
    };
    // Read EXACTLY n raw payload bytes from the buffered stream. Authenticated
    // kernel timeout ticks are not an application deadline: retain the filled
    // prefix and retry. A real short read (the client hung up mid-frame) closes
    // the connection — the stream is desynced.
    let mut payload = vec![0u8; n];
    if read_exact_authenticated(reader, &mut payload).is_err() {
        return false;
    }

    // The payload may have arrived long after the header. Re-resolve both the
    // active target and its capability at the effect boundary: a tab switch,
    // session replacement, or edge revocation during `read_exact` must not be
    // authorized by the stale header snapshot. The early fence above exists
    // only to retire the header target while the read can block.
    let active_target = resolve_active(route.active);
    let front_terminal_session = active_target.as_ref().map(|(_, _, session, _)| *session);
    let target = match selector.as_ref() {
        None | Some(Selector::SelfTok) => active_target.clone(),
        Some(sel) => resolve_explicit(route.store, sel),
    };
    let authorized = target.as_ref().is_some_and(|(_, _, _, ctx)| {
        matches!(route.scope, Scope::Owner)
            || cross_session_authorized(route.scope, "feed", ctx)
    });

    // The binary paste twin has the same hybrid target contract as inline
    // `paste`: explicit selectors stay terminal/session operations, while a
    // bare/self paste with native front content enters the main-thread input
    // seam. `feed-bin` remains raw PTY input and can never take this branch.
    if paste {
        let principal = if matches!(route.scope, Scope::Owner) {
            NativeControlPrincipal::Owner
        } else {
            NativeControlPrincipal::Edge
        };
        // Lazy: only the native-front arm consumes it. With a terminal front (the
        // ordinary case) the guard returns None and the payload is never scanned or
        // copied here — the real path below re-derives it as a borrowed `Cow`.
        if let Some(route) = sessionless_front_paste_event(
            "paste",
            || String::from_utf8_lossy(&payload).into_owned(),
            selector.as_ref(),
            active_target.is_some(),
            principal,
        ) {
            let response = match route {
                Ok(event) => match dispatch_front_input(event, None) {
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

    // Resolve the target (self or `@<selector>`) and gate it like `feed` (WriteInput),
    // mirroring `handle()`'s self/cross split. The payload was already consumed, so
    // every path below replies AND keeps the stream framed.
    // `paste-bin` needs the TARGET terminal (to run `format_paste` under its lock —
    // bracketing depends on the app's DECSET 2004 state); `feed-bin` uses the same
    // tuple so target resolution and authorization cannot drift between the two.
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

    // Op-scope gate: `feed-bin` is `WriteInput`, exactly like `feed`. SELF and CROSS
    // collapse onto the SAME per-session predicate — Owner keeps full self-power; an
    // Edge (self OR cross) must hold a `decide_edge`-permitted token against the
    // RESOLVED target's table+nonce. Matching the op ALONE on the self path let an
    // edge scoped to session B inject raw bytes into whatever tab became frontmost
    // after a tab/window switch (the global ActiveHandle retargets `@.`) — the same
    // confused-deputy authority escape `handle()`'s self gate closes.
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

    // The active/self selector can retarget while the payload is in flight, and
    // a fresh local input can stamp a new licence after the header fence. Fence
    // the target selected by the post-payload snapshot as well, immediately
    // before any direct egress. App-routed input repeats this boundary
    // harmlessly.
    let cleared = clear_license(target_session);
    if !cleared.starts_with("OK") {
        let _ = writer.write_all(cleared.as_bytes());
        let _ = writer.flush();
        return false;
    }

    // TURN LEASE: `feed-bin` reaches the PTY HERE, bypassing the verb-dispatch
    // fast-fail that refuses `send/key/feed/…` while a turn holds the lease. Without
    // this mirror, raw bytes interleave into a mid-flight turn every other write verb
    // is locked out of — corrupting the turn's captured output. Refuse identically
    // (payload already consumed, so the stream stays framed).
    if let Some(id) = ctx
        .turn_lease
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .and_then(crate::Lease::write_block_turn)
    {
        let _ = writer.write_all(format!("ERR busy turn={id}\n").as_bytes());
        let _ = writer.flush();
        return true;
    }

    // D3 self-feed floor: `feed-bin` reaches the PTY HERE — it is intercepted in
    // `serve` BEFORE `handle()`, so it bypasses the verb-dispatch floor. Apply the
    // SAME per-session injection bucket on the SELF path so a raw client cannot
    // drive a feedback storm via `feed-bin` (the cross path is gated by the edge
    // above, and targets a different session anyway).
    if matches!(&selector, None | Some(Selector::SelfTok))
        && !crate::inject_floor::allow(target_session, payload.len().max(1))
    {
        let _ = writer.write_all(b"ERR rate (self-feed floor)\n");
        let _ = writer.flush();
        return true;
    }

    // A terminal tab currently on screen owns the same App input side effects
    // as human/inline paste: cursor-movement provenance, viewport snap,
    // selection clearing, and blink reset. This includes an explicit `@self`
    // / `@<sid>` that resolves back to the visible tab. Pass the exact session
    // target so a racing tab switch degrades to hidden-session delivery rather
    // than pasting into whichever tab became front.
    if front_terminal_session == Some(target_session) {
        let event = if paste {
            InputEvent::Paste(String::from_utf8_lossy(&payload).into_owned())
        } else {
            // Raw front-session input is not the user's fingers. It still
            // enters App::input so that seam closes any older licence before
            // writing the exact bytes. The direct route below retains its
            // background egress but runs an explicit session-wide fence first,
            // because another window may still present that session.
            InputEvent::KeySequence(payload.clone())
        };
        let response = match dispatch_front_input(event, Some(target_session)) {
            Ok(InputOutcome::Ok) => format!("OK {n} bytes\n"),
            Ok(InputOutcome::RangeRejected) => "ERR out of range\n".to_string(),
            Ok(InputOutcome::WriteFailed) => "ERR write failed\n".to_string(),
            Err(error) => error,
        };
        if writer.write_all(response.as_bytes()).is_err() {
            return false;
        }
        return writer.flush().is_ok();
    }

    if paste {
        // PASTE semantics: run the payload through `format_paste` under the target's
        // lock (bracketed-paste guards when DECSET 2004 is on + control-byte sanitize
        // + LF->CR), exactly as the `paste` verb / human Cmd-V do — then write the
        // transformed bytes. The payload is interpreted as UTF-8 (lossy): a paste is
        // text, and the sanitizer would strip raw control bytes anyway.
        let text = String::from_utf8_lossy(&payload);
        let out = term_lock(&term).format_paste(&text);
        control_input::write_pty(&ctx.sink, &out);
    } else {
        control_input::write_pty(&ctx.sink, &payload);
    }
    let reply = format!("OK {n} bytes\n");
    if writer.write_all(reply.as_bytes()).is_err() {
        return false;
    }
    writer.flush().is_ok()
}

/// Fill one already-bounded authenticated binary frame while treating the
/// socket's liveness-poll timeout as a retry tick. The filled prefix survives
/// every tick, so a slow producer remains byte-exact and wire-compatible.
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

/// Proposal frames occupy an ordinary RPC lane and therefore have an absolute
/// body deadline in addition to the socket's liveness timeout ticks. On expiry
/// the caller closes the now-partial frame; it must never resume line dispatch.
fn read_exact_authenticated_until(
    reader: &mut impl Read,
    payload: &mut [u8],
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < payload.len() {
        if std::time::Instant::now() >= deadline {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        match reader.read(&mut payload[filled..]) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => filled += read,
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

/// AUTHORIZE a `subscribe` request and, on success, FLIP this connection to push
/// mode by running the subscriber push loop (which never returns to the poll loop).
///
/// Grammar: `subscribe @<sel>[,<sel>...] <streams> [since=<seq>]` where `<streams>`
/// is a comma/space list ⊆ {screen,cursor,events,cells,bytes,sessions} plus the
/// `timestamps`/`ts` modifier.
///
/// TWO authority checks, because the request names streams at two different SCOPES.
/// The per-TARGET streams are gated once per `@<sel>`, exactly like a read verb:
/// `@.`/self resolves through the active handle; every target (self included, for a
/// scoped Edge) needs `ReadScreen` authorization through [`resolve_target`] +
/// [`cross_session_authorized`] (Owner reaches same-uid siblings; a scoped Edge needs
/// a `decide_edge` grant against the TARGET's table+nonce). The INSTANCE-scoped
/// `sessions` stream is gated ONCE, before the loop: it reports the whole live-session
/// roster, which no per-target grant covers, so it is Owner-only.
///
/// FAIL-CLOSED throughout: a malformed line, a stream list naming no frame source, an
/// unknown session, a non-Owner asking for `sessions`, or ANY target that fails the
/// gate writes a single `ERR ...` and the connection is closed without entering push
/// mode (no partial subscription). On full success it writes `OK subscribe <n>\n` and
/// hands the socket to [`subscribe::push_loop`].
/// Write an error line and flush, swallowing the I/O result. Returns `()` so it
/// can be used as `return write_err(writer, b"ERR ...\n");` in a `-> ()` fn.
fn write_err<W: Write>(writer: &mut W, msg: &[u8]) {
    let _ = writer.write_all(msg);
    let _ = writer.flush();
}

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

/// Run a production socket subscription with read-side disconnect detection.
///
/// The protocol becomes push-only after the subscribe acknowledgement. Its client
/// MUST keep the write/send half open and send no more bytes; EOF, an unexpected
/// byte, or a read error therefore all mean the peer is gone. The one-millisecond
/// read timeout keeps each 250ms liveness probe bounded on both Unix and Windows.
fn run_subscribe_socket(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    writer: &mut CtlStream,
) {
    let Ok(probe) = writer.try_clone() else {
        return;
    };
    if probe
        .set_read_timeout(Some(std::time::Duration::from_millis(1)))
        .is_err()
    {
        return;
    }
    run_subscribe_with_peer_probe(line, active, store, subscribers, scope, writer, move || {
        subscription_peer_gone(&probe)
    });
}

fn subscription_peer_gone(stream: &CtlStream) -> bool {
    use std::io::Read as _;

    let mut stream = stream;
    let mut byte = [0_u8; 1];
    match stream.read(&mut byte) {
        Ok(0) | Ok(_) => true,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::Interrupted
            ) =>
        {
            false
        }
        Err(_) => true,
    }
}

fn run_subscribe_with_peer_probe<W: Write, P: FnMut() -> bool>(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    writer: &mut W,
    peer_gone: P,
) {
    let line = line.strip_suffix('\r').unwrap_or(line);
    // Strip the verb; the remainder is `@<sel>[,<sel>...] <streams> [since=<seq>]`.
    let rest = match line.split_once(' ') {
        Some(("subscribe", r)) => r.trim(),
        _ => {
            let _ =
                writer.write_all(b"ERR usage: subscribe @<sel>[,<sel>] <streams> [since=<seq>]\n");
            let _ = writer.flush();
            return;
        }
    };
    let mut it = rest.split_whitespace();
    let Some(first) = it.next() else {
        let _ =
            writer.write_all(b"ERR usage: subscribe [@<sel>[,<sel>]] <streams> [since=<seq>]\n");
        let _ = writer.flush();
        return;
    };
    // DEFAULT-TO-SELF (like every other verb): a first token that is NOT an
    // `@<sel>` list but IS a valid stream list means `@.` (self) — so
    // `subscribe screen,events` works. Otherwise the first token is the selector
    // and the second is the streams.
    let (sel_tok, stream_tok) = if first.starts_with('@') {
        match it.next() {
            Some(s) => (first, s),
            None => {
                let _ = writer
                    .write_all(b"ERR usage: subscribe @<sel>[,<sel>] <streams> [since=<seq>]\n");
                let _ = writer.flush();
                return;
            }
        }
    } else if Requested::parse(first).is_some() {
        ("@.", first)
    } else {
        let _ =
            writer.write_all(b"ERR usage: subscribe [@<sel>[,<sel>]] <streams> [since=<seq>]\n");
        let _ = writer.flush();
        return;
    };
    // Trailing args: `since=<seq>` (content_seq resume), `since-turn=<id>` /
    // `since-block=<id>` (events resume — the turn/block watermarks), and the
    // `every-frame` flag (re-emit `cells` every wake for animation fidelity).
    let mut since: Option<u64> = None;
    let mut since_turn: Option<u64> = None;
    let mut since_block: Option<u64> = None;
    let mut non_coalesced = false;
    for tok in it {
        let parse_u64 = |v: &str| v.parse::<u64>().ok();
        if let Some(v) = tok.strip_prefix("since=") {
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
        let _ = writer
            .write_all(b"ERR usage: streams are a subset of screen,cursor,events,cells,bytes,sessions,timestamps(ts) and must name at least one frame source\n");
        let _ = writer.flush();
        return;
    };

    // INSTANCE-SCOPED authority, decided ONCE for the connection — deliberately NOT
    // inside the per-selector loop below. That loop proves `ReadScreen` against each
    // target the client named, which is all a per-target edge can ever speak for; the
    // `sessions` stream instead diffs the WHOLE live-session roster, so it would leak
    // the existence + opaque sid of siblings the subscriber never named and could not
    // have named. Owner already holds that view through the `sessions`/`who` verbs, so
    // Owner keeps it and everyone else is REFUSED here — a hard `ERR denied` rather
    // than a silently-empty stream, because a push-only connection that never pushes
    // is indistinguishable from a hang, and fail-closed matches the rest of the
    // surface. (`InstanceStreams::authorize` re-applies the same rule as a type-level
    // invariant, so a future caller that forgets this refusal still cannot grant it.)
    if req.instance.sessions && !matches!(scope, Scope::Owner) {
        log_denial(
            AUDIT_SUBSYSTEM,
            "subscribe -> sessions",
            aterm_containment::mode_or_containment(),
            "the sessions stream is instance-wide; only Owner holds instance authority",
        );
        return write_err(writer, b"ERR denied\n");
    }
    let instance = InstanceStreams::authorize(req.instance, scope);

    // `@*` — the LIVE INSTANCE TARGET SET. Everything the connection is allowed
    // to watch, INCLUDING sessions that do not exist yet: the push loop adopts
    // each new one and acks it with the same `sub <local> <sid>` line the
    // handshake emits.
    //
    // WHY IT EXISTS. The target list used to be frozen here for the life of the
    // connection, and the push loop never reads another request line, so a client
    // that wanted a newly-opened tab had to open a WHOLE NEW subscription for it —
    // a new process, a new socket, a new server push thread, once per tab-open.
    // The pool admits `CONTROL_SUBSCRIPTION_WORKERS` of those; past that the
    // answer is `ERR subscription capacity busy`, which a push-only client that
    // has already recorded the session as seen never retries. So the fifth
    // staggered tab-open stopped being federated, silently and permanently. One
    // live target set is one connection per instance, and the pool goes back to
    // bounding PEERS, which is what it was meant to bound.
    //
    // AUTHORITY. Owner-only, for exactly the reason the instance `sessions`
    // stream is: it names sessions the subscriber could not have named itself, so
    // it reveals the existence and opaque sid of siblings. Refused HARD rather
    // than degraded to an empty stream — a push-only connection that never
    // pushes is indistinguishable from a hang. `AdoptScope::authorize` re-applies
    // the same rule as a type-level invariant, so this refusal cannot be lost by
    // a future caller.
    let adopt_all = sel_tok == "@*";
    if adopt_all && !matches!(scope, Scope::Owner) {
        log_denial(
            AUDIT_SUBSYSTEM,
            "subscribe -> @*",
            aterm_containment::mode_or_containment(),
            "the @* live target set is instance-wide; only Owner holds instance authority",
        );
        return write_err(writer, b"ERR denied\n");
    }
    // Resume anchors are per-SESSION watermarks (see the refusal below for the
    // explicit-list case). A live target set is multi-target by definition — and
    // becomes so at an unpredictable moment — so seeding one session's anchor
    // into it is never meaningful. Refuse up front rather than let a `@*` that
    // happens to start with one target slip past the count check.
    if adopt_all && (since.is_some() || since_turn.is_some() || since_block.is_some()) {
        return write_err(
            writer,
            b"ERR resume anchors (since=/since-turn=/since-block=) require a single target\n",
        );
    }
    let adopt = subscribe::AdoptScope::authorize(adopt_all, scope);

    // The connection's own session tuple, resolved like every other request so a
    // self `subscribe` (`@.`) follows the active tab the same way a self read does.
    // Resolve + GATE every `@<sel>` in the comma list. Fail-closed: the FIRST bad
    // selector aborts the whole subscribe (no partial push), and the gate denial is
    // audited exactly like a cross-session read denial.
    let mut targets: Vec<subscribe::ResolvedTarget> = Vec::new();
    // The channel-id -> stable-sid map emitted in the ack (below), so a client that
    // subscribed by `@s-…`/`@.` can demultiplex the compact `<local>` frame tags
    // back to the sids it knows (the tmux-control-mode handshake pattern).
    let mut sub_map: Vec<(u64, String)> = Vec::new();
    let mut parsed = 0usize;
    // `@*` seeds from the CURRENT roster and then keeps adopting; every other
    // form is the literal comma list. Expanding to real `@<sid>` selectors (a
    // snapshot cloned out of the store, guard dropped immediately) means the
    // seed targets go through the SAME resolve + `ReadScreen` gate + de-dup +
    // cap loop as an explicit list — there is no second admission path to keep
    // in sync, which is the only way this stays fail-closed.
    let expanded: Vec<String> = if adopt_all {
        store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .live_handles()
            .map(|h| format!("@{}", h.sid.as_str()))
            .collect()
    } else {
        sel_tok
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
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
            Selector::SelfTok => resolve_active(active),
            Selector::Local(_) | Selector::Sid(_) => resolve_explicit(store, &sel),
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
        // Subscribe authorizes EXACTLY like a read (`ReadScreen`) via the same
        // per-session `cross_session_authorized` gate. Owner keeps full self-power; an
        // Edge — self (`@.`) OR cross — must hold a `decide_edge`-permitted token
        // against the RESOLVED target's table+nonce. Treating `@.` as "always
        // allowed" let an edge scoped to session B read whatever tab became frontmost
        // after a tab/window switch (the global ActiveHandle retargets `@.`) — the
        // same confused-deputy read escape `handle()`'s self gate closes.
        if !matches!(scope, Scope::Owner) && !cross_session_authorized(scope, "subscribe", &ctx) {
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
        if targets.iter().any(|(id, _, _, _, _)| *id == local_id) {
            continue;
        }
        sub_map.push((local_id, ctx.self_id.as_str().to_string()));
        targets.push((
            local_id,
            term,
            ctx.byte_fanout.clone(),
            ctx.turns.clone(),
            ctx.timeline.clone(),
        ));
    }
    // An EMPTY set is an error for an explicit list (the client named nothing
    // resolvable) but perfectly normal for `@*`: an instance that momentarily has
    // no sessions is exactly the case a live target set exists to survive.
    if targets.is_empty() && !adopt_all {
        let _ = writer.write_all(b"ERR usage: at least one @<sel> target\n");
        let _ = writer.flush();
        return;
    }

    // Resume anchors are PER-SESSION: `content_seq`, turn ids and block ids each
    // live in a single session's own monotonic space and are NOT comparable across
    // sessions. So a resume anchor is only meaningful for a single-target
    // subscription — reject it against a fan-out rather than silently seeding every
    // target from one session's watermark (which would over- or under-emit on the
    // others). Per-target resume = one subscription per session.
    if targets.len() > 1 && (since.is_some() || since_turn.is_some() || since_block.is_some()) {
        return write_err(
            writer,
            b"ERR resume anchors (since=/since-turn=/since-block=) require a single target\n",
        );
    }

    // Authorized. Ack with the channel map, then FLIP to push-only: the loop owns
    // the socket from here and never reads another request line. The ack is
    // `OK subscribe <n>` + one `sub <local> <sid>` line per target, so the client
    // resolves the compact `<local>` tag on every DELTA/EVENT/BYTES/GAP frame back
    // to the stable sid it subscribed with (frames stay small; the map is one-shot).
    let mut ack = format!("OK subscribe {}\n", targets.len());
    for (local_id, sid) in &sub_map {
        ack.push_str(&format!("sub {local_id} {sid}\n"));
    }
    if writer.write_all(ack.as_bytes()).is_err() {
        return;
    }
    if writer.flush().is_err() {
        return;
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
        },
        writer,
        peer_gone,
    );
}

/// A resolved cross-session TARGET tuple: the SAME `(term, master, id, ctx)`
/// shape `resolve_active` produces, but for an `@<selector>`-addressed session.
/// Cloned OUT of the [`Store`] before the guard drops, so the dispatch never
/// holds the registry lock across a `Terminal` lock.
type Target = (Arc<Mutex<Terminal>>, i32, u64, Arc<SessionCtx>);

/// A parsed `@<selector>`. `SelfTok` (`@.` or bare `@`) names the connection's own
/// session; `Local`/`Sid` name a specific session. Total + fail-closed: an unknown
/// id resolves to `None` at lookup (`ERR no such session`), never to a wrong one.
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
    /// Parse the body AFTER the leading `@`. `.` => self; an all-digits body =>
    /// a local id; anything else is taken verbatim as a `SessionId` string (the
    /// `s-<hex>` form, matching the wire id `whoami`/`sessions` report). An empty
    /// body is treated as self (`@` alone == `@.`).
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

/// Resolve an `@<selector>` to a TARGET tuple, CLONING it out of the registry and
/// dropping the store guard BEFORE the caller locks the target `Terminal` (the
/// clone-then-release discipline — the store lock is never held across a Terminal
/// lock, so mutually-driving agents cannot deadlock). `@.`/`@` resolves to the
/// connection's own `(self_*)` tuple verbatim. Returns `None` (fail closed) for an
/// unknown id — the caller maps that to `ERR no such session`.
fn resolve_target(self_tuple: &Target, store: &Store, sel: &Selector) -> Option<Target> {
    match sel {
        Selector::SelfTok => Some(self_tuple.clone()),
        Selector::Local(n) => {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            let h = g.by_local(*n)?;
            Some((h.term.clone(), h.master, h.local_id, h.ctx.clone()))
            // guard drops here, before any Terminal lock
        }
        Selector::Sid(sid) => {
            let g = store.read().unwrap_or_else(|p| p.into_inner());
            let h = g.by_sid(sid)?;
            Some((h.term.clone(), h.master, h.local_id, h.ctx.clone()))
        }
    }
}

/// Whether a CROSS-session call (target != connection's own session) is authorized
/// for `verb`. Default-DENY.
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
fn cross_session_authorized(scope: Scope, verb: &str, target_ctx: &SessionCtx) -> bool {
    let Some(need) = required_op(verb) else {
        // No op class (privilege/identity/unknown verb): never cross-session.
        return false;
    };
    // Owner reaches any sibling (same trust domain); a scoped Edge needs a
    // `decide_edge`-permitted token for `need` against the TARGET's table + nonce.
    scope_holds_op(scope, need, target_ctx)
}

/// The DISPATCH-gate authority for the whole `verb rest` line against `target_ctx`:
/// like [`cross_session_authorized`] but on the EFFECTIVE op — the verb's base op
/// ESCALATED by its argument for the indirect seams (`invoke <clipboard/config
/// action>`, `open prefs|settings`; see [`escalated_op`]). The escalated op REPLACES
/// (does NOT add to) the base op, so an edge explicitly granted the fine op
/// (ClipboardWrite/ConfigWrite) for exactly that action is ACCEPTED, while a plain
/// WriteInput edge cannot tunnel to the fenced op through the seam. An [`Escalation`] may
/// also demand OWNER-ONLY (the palette gateway / update-apply twins), which no Edge — however
/// privileged — satisfies. Owner reaches self + siblings; a None effective op (unknown
/// verb) default-DENIES.
fn dispatch_authorized(scope: Scope, verb: &str, rest: &str, target_ctx: &SessionCtx) -> bool {
    match escalated_op(verb, rest) {
        // The argument reaches a fenced capability. OwnerOnly admits ONLY the god token;
        // a fine-op escalation is decided (like the base gate) by `scope_holds_op` — an
        // edge granted exactly that op passes, a plain WriteInput edge does not. Owner
        // satisfies both (it is the `Scope::Owner` short-circuit in `scope_holds_op`).
        Some(Escalation::OwnerOnly) => matches!(scope, Scope::Owner),
        Some(Escalation::Op(need)) => scope_holds_op(scope, need, target_ctx),
        // No escalation: fall back to the verb's base required op (None => unknown verb,
        // default-DENY).
        None => match required_op(verb) {
            Some(need) => scope_holds_op(scope, need, target_ctx),
            None => false,
        },
    }
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
fn is_front_driving_verb(verb: &str) -> bool {
    matches!(
        verb,
        "key" | "ctrl" | "mouse" | "paste" | "focus" | "send" | "feed" | "turn"
    )
}

/// PART B pure decision: given the caller's `scope`, input `verb`, and the FRONT
/// main-thread surface observation, return the authority escalation that REPLACES the
/// verb's base WriteInput requirement.  Every overlay remains Owner-only (some bind
/// clipboard/external-process actions); native Settings requires the durable-config
/// fine op.  Owner skips the observation entirely and already satisfies either result.
///
/// Split out pure so the policy is unit-testable without the event-loop hop.  A failed
/// hop is denied by the caller rather than being interpreted as `None` (fail closed).
fn front_drive_escalation(
    scope: Scope,
    verb: &str,
    surface: FrontControlSurface,
) -> Option<Escalation> {
    if matches!(scope, Scope::Owner) || !is_front_driving_verb(verb) {
        return None;
    }
    match surface {
        FrontControlSurface::Overlay(_) => Some(Escalation::OwnerOnly),
        FrontControlSurface::NativeSettings => Some(Escalation::Op(Op::ConfigWrite)),
        FrontControlSurface::None | FrontControlSurface::OtherNative => None,
    }
}

/// Post the connections REPAINT POKE after a successful connection-authority
/// act on the control thread (`connect`/`disconnect`/`grant`/`revoke`, design
/// §6: new verbs "poke the repaint funnel"). The edge tables changed but no
/// funnel epoch moved, so without this the §4 tab marks would show the old
/// state until the next unrelated strip refresh. Keyed on the `OK` reply —
/// a refused/failed act changed nothing. Fire-and-forget (the `Wake::redraw`
/// idiom): a gone event loop only costs the repaint.
fn poke_connections_on_ok(proxy: &EventLoopProxy<Wake>, resp: String) -> String {
    if resp.starts_with("OK") {
        // Advance the §2.4 freshness epoch BEFORE the wake: the wake's
        // refresh recomposes the tab chrome through its cache gate, and the
        // `grant`/`revoke` verbs mutate tables WITHOUT touching the record
        // store — this poke is their only bump site. (`connect`/`disconnect`
        // already bumped in-store; a double bump only re-misses the cache.)
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
        FrontControlSurface::None | FrontControlSurface::OtherNative => "ERR denied\n".to_string(),
    }
}

// ── CROSS-SESSION input arms (P1.2 follow-up) ────────────────────────────────
//
// These run ON THE CONTROL THREAD against a RESOLVED `@<selector>` target — NOT
// the active tab — so there is no App UI/gesture/window state to touch and NO
// `Wake::Input` is posted. They reuse the source-blind seam (`seam_egress`) and
// the engine's own viewport/geometry APIs directly, with `(target_term,
// target_sink) = (term, &ctx.sink)` resolved exactly as `send`/`feed` do. The
// op-scope gate (`cross_session_authorized`) has already passed before any of
// these is reached.

/// Cross-session `key`/`ctrl`/`paste`/`focus`: feed a pre-built [`InputEvent`] to
/// the source-blind seam on the TARGET `(term, sink)`. The seam reads the target's
/// modes ONCE and writes the encoded PTY bytes to the target's sink (a no-op under
/// a mode that suppresses the event, e.g. focus reporting OFF) — byte-identical to
/// what the active-tab seam would emit for the SAME event, preserving the Tier-1
/// indistinguishability invariant (no `Source` is involved). `None` (a malformed
/// verb line) maps to `err`. Always `Egress::Reported` for these arms.
fn cross_input(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: Option<InputEvent>,
    err: &str,
) -> String {
    match ev {
        Some(ev) => {
            // Cross-session verbs run on the CONTROL thread (expendable): block under
            // SPILL_CAP so a machine-rate driver into a wedged target feels
            // backpressure here rather than growing the target sink's spill.
            //
            // This bypasses the App input seam by design, so an in-flight
            // `video ... keys` recording cannot log it. Count the attempts it
            // carries so the recording can SAY that, rather than publishing a
            // silent zero. A `focus` (the other event that reaches this arm) is
            // not an attempt and the classifier contributes nothing for it —
            // announcing a gap for a verb the ledger never records would be its
            // own kind of dishonesty.
            crate::note_unseamed_control_input(&ev);
            seam_egress(term, &ctx.sink, &ev, EgressMode::Backpressured);
            "OK\n".to_string()
        }
        None => err.to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperatorInputStage {
    Paste,
    Submit,
}

impl OperatorInputStage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Paste => "paste",
            Self::Submit => "submit",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OperatorInputFailure {
    stage: OperatorInputStage,
    delivery: Delivery,
}

impl OperatorInputFailure {
    /// Sanitized control reply: stage + kernel count only, never proposal or
    /// screen text.  `operator_result_summary` admits this exact grammar into the
    /// durable in-doubt reason so recovery retains zero-vs-partial evidence.
    fn response(self) -> String {
        match self.delivery {
            Delivery::PartialInDoubt { accepted } => format!(
                "ERR operator input {} partial accepted={accepted}\n",
                self.stage.as_str()
            ),
            Delivery::BusyZero | Delivery::Failed => {
                format!("ERR operator input {} busy-zero\n", self.stage.as_str())
            }
            Delivery::ConflictZero => {
                format!("ERR operator input {} conflict-zero\n", self.stage.as_str())
            }
            Delivery::Full | Delivery::FullAt { .. } => {
                "ERR operator input internal-full\n".to_string()
            }
        }
    }
}

/// Guarded operator egress: one immediate bounded frame, never spill/park. The
/// typed outcome is retained until the transaction layer has durably classified
/// a zero-byte refusal versus a partial kernel mutation. Neither is retried.
#[cfg(test)]
fn operator_input(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: Option<InputEvent>,
) -> Delivery {
    let Some(ev) = ev else {
        return Delivery::BusyZero;
    };
    match seam_egress(term, &ctx.sink, &ev, EgressMode::TryImmediate) {
        Egress::Reported(delivery) => delivery,
        Egress::TrackingOff { .. } => Delivery::BusyZero,
    }
}

/// Epoch-conditional operator egress. A successful frame returns the exact
/// advanced epoch as [`Delivery::FullAt`]; a foreign attempt returns
/// [`Delivery::ConflictZero`] and this event contributes no PTY bytes. The
/// terminal's bounded screen generation is compared and approval-classified
/// while its lock remains held across the conditional sink syscall. Thus output
/// applied by the reader cannot change the checked presentation between compare
/// and paste/Enter.
#[derive(Clone, Copy)]
enum OperatorTerminalFence {
    Exact(aterm_agent::operator::EventGeneration),
}

fn operator_input_if_epoch(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: Option<InputEvent>,
    expected: InputEpoch,
    terminal_fence: OperatorTerminalFence,
) -> Delivery {
    let Some(ev) = ev else {
        return Delivery::BusyZero;
    };
    // This is inside the host actuation gate in production. A blocking terminal
    // acquisition here would let output/reflow hold fleet-fault, unmanagement,
    // and shutdown behind an unbounded wait. Refuse with zero bytes instead.
    let terminal = match term.try_lock() {
        Ok(terminal) => terminal,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return Delivery::BusyZero,
    };
    let OperatorTerminalFence::Exact(expected_generation) = terminal_fence;
    let evidence = crate::operator_host::terminal_evidence(&terminal);
    let fingerprint: [u8; 32] = Sha256::digest(evidence.as_bytes());
    if terminal.is_alternate_screen() != expected_generation.alternate_screen
        || terminal.content_seq() != expected_generation.content_seq
        || fingerprint != expected_generation.fingerprint
        || crate::operator_host::looks_like_approval(&evidence)
    {
        return Delivery::ConflictZero;
    }
    let bytes = match ev {
        InputEvent::Paste(text) => terminal.format_paste(&text),
        InputEvent::Key {
            key,
            mods,
            base_layout,
            event_type,
        } => aterm_types::keyboard::encode_key_with_layout(
            &key,
            mods,
            terminal.keyboard_mode(),
            event_type,
            base_layout,
        ),
        _ => return Delivery::BusyZero,
    };
    if bytes.is_empty() {
        return Delivery::FullAt { epoch: expected };
    }
    let (outcome, epoch) = ctx
        .sink
        .try_write_frame_immediate_if_epoch(expected, &bytes);
    match outcome {
        aterm_session::sink::ImmediateWrite::Full => Delivery::FullAt { epoch },
        aterm_session::sink::ImmediateWrite::BusyZero => Delivery::BusyZero,
        aterm_session::sink::ImmediateWrite::ConflictZero => Delivery::ConflictZero,
        aterm_session::sink::ImmediateWrite::PartialInDoubt { accepted } => {
            Delivery::PartialInDoubt { accepted }
        }
    }
}

/// Should an explicit-selector request be applied through the App INPUT SEAM
/// rather than the background egress? Exactly when the request named a session
/// (`is_cross`) AND that session is the tab currently on screen.
///
/// A flagless request is already on the seam, so it is never "front-routed" by
/// this predicate — it simply never reaches it. A named session that is NOT on
/// screen keeps the direct, round-trip-free egress the background path exists
/// for. The middle case — naming the tab you are looking at, which is what the
/// documented `@self` selector expands to — is the one that was silently taking
/// the background path and losing every side effect the seam owns (blink reset,
/// viewport snap, selection clear, the typing-momentum hints every cursor effect
/// reads, and the `video ... keys` ledger).
///
/// Pure so the law is testable without a running event loop; the answer is
/// re-checked on the event loop before it is acted on, so a stale `front_active`
/// can only cost a path, never a mis-delivery.
const fn front_routed(is_cross: bool, front_active: Option<u64>, target: u64) -> bool {
    is_cross
        && match front_active {
            Some(front) => front == target,
            None => false,
        }
}

/// Execution seam for the two input phases of the composite `turn` verb.
/// Keeping this as one pure decision prevents paste and Return from drifting
/// onto different paths when an explicit selector names the visible tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnInputRoute {
    Front,
    Background,
    Local,
}

const fn turn_input_route(is_cross: bool, targets_front: bool) -> TurnInputRoute {
    if is_cross && targets_front {
        TurnInputRoute::Front
    } else if is_cross {
        TurnInputRoute::Background
    } else {
        TurnInputRoute::Local
    }
}

/// The FRONT-ROUTED twin of [`cross_input`]: an explicit `@<sid>` whose target
/// resolved to the tab currently on screen is not a background session at all, so
/// its event goes through the App input seam (`Wake::Input` carrying the session)
/// rather than straight to the sink.
///
/// This is what makes a named-session drive behave like the flagless one: the
/// blink reset, viewport snap, selection clear, the typing-momentum hints every
/// cursor effect reads, and the `video ... keys` ledger all live on that seam and
/// were silently skipped by the direct egress. Authority is UNCHANGED — the
/// cross-session gate has already run by the time we get here; only the execution
/// path differs. The event loop re-resolves the session, so a tab switch racing
/// this request lands on the hidden path for the named session instead of typing
/// into whatever tab won.
fn front_routed_input(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    ev: Option<InputEvent>,
    err: &str,
) -> String {
    match ev {
        Some(ev) => control_input::input_reply_to_str(post_input_reply_to(
            proxy,
            Op::WriteInput,
            vec![ev],
            Some(session),
        )),
        None => err.to_string(),
    }
}

/// Close the cursor-move LICENCE before a visible-front control operation that
/// does not otherwise pass through `App::input`. The dedicated main-thread wake
/// expires only the two key-hint stamps: unlike an empty synthetic key it
/// cannot heat cadence/rain, stamp input latency, or write a PTY frame. The
/// round trip completes before the direct operation below, so a signal cannot
/// synchronously trigger output that borrows the older licence.
fn front_routed_license_clear(proxy: &EventLoopProxy<Wake>, session: u64) -> String {
    license_clear_reply(control_media::call_main(proxy, |reply| {
        Wake::CursorMoveLicenseClear {
            session,
            reply,
        }
    }))
}

fn license_clear_reply(result: Result<bool, &'static str>) -> String {
    match result {
        // `false` is a safe tab-switch race: the named target is no longer
        // visible and therefore holds no licence, but the authorized
        // direct operation still proceeds against that session.
        Ok(_) => "OK\n".to_string(),
        Err(error) => format!("ERR input dispatch failed: {error}\n"),
    }
}

fn control_attempt_closes_cursor_license(verb: &str) -> bool {
    matches!(
        verb,
        "send"
            | "key"
            | "ctrl"
            | "feed"
            | "signal"
            | "mouse"
            | "paste"
            | "focus"
            | "turn"
            | "resize"
            | "scroll"
    )
}

fn front_routed_resize(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    rest: &str,
) -> String {
    if let Some(px) = rest.trim().strip_prefix("px") {
        let mut it = px.split_whitespace();
        let (Some(ws), Some(hs)) = (it.next(), it.next()) else {
            return "ERR usage: resize px <w> <h>\n".to_string();
        };
        let (Ok(width), Ok(height)) = (ws.parse::<u32>(), hs.parse::<u32>()) else {
            return "ERR bad args\n".to_string();
        };
        return front_routed_input(
            proxy,
            session,
            Some(InputEvent::ResizeWindowPx { width, height }),
            "ERR\n",
        );
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
    if let Err(error) = post_input_reply_to(
        proxy,
        Op::ReadScreen,
        vec![InputEvent::ScrollView(intent)],
        Some(session),
    ) {
        return error;
    }
    let t = term_lock(term);
    format!(
        "OK {} {}\n",
        t.grid().display_offset(),
        t.grid().scrollback_lines()
    )
}

/// Cross-session `mouse`: build the engine-neutral event via [`parse_mouse`] and
/// feed it to the seam on the TARGET. When the target app IS mouse-tracking, the
/// seam writes the report to the target sink (`Egress::Reported`). When it is NOT
/// (`Egress::TrackingOff`):
///   * a WHEEL (`wheel_lines > 0`) moves the TARGET term's viewport via
///     `scroll_display` (positive offset = toward history; `wheel_up` => +lines),
///     mirroring the active-tab wheel fallback (`App::input`), and nudges the
///     target tab to repaint with `Wake::redraw(session)` (the same repaint
///     `cmd_select` fires for a background tab);
///   * a plain PRESS/RELEASE/MOVE (`wheel_lines == 0`) is a DELIBERATE no-op:
///     the active-tab fallback would drive the App SELECTION GESTURE, but a
///     background session has no controller-side selection UI to mutate. We still
///     reply `OK` (the event was accepted; there is simply nothing to render).
fn cross_mouse(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    session: u64,
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
) -> String {
    match cross_mouse_apply(term, ctx, rest) {
        // The viewport moved (a wheel under a non-tracking target): nudge the
        // (possibly-not-active) target tab to repaint, the same way `cmd_select`
        // repaints by the resolved local id.
        Ok(true) => {
            let _ = proxy.send_event(Wake::redraw(session));
            "OK\n".to_string()
        }
        // Tracking-on report, or a no-op press/move: nothing to repaint.
        Ok(false) => "OK\n".to_string(),
        Err(e) => e,
    }
}

/// The proxy-INDEPENDENT core of [`cross_mouse`] (so it is unit-testable headlessly,
/// where an `EventLoopProxy` cannot be built off the main thread). Parses + feeds
/// the event to the seam on the TARGET and applies the `TrackingOff` fallback,
/// returning `Ok(true)` when the TARGET viewport moved (the caller should repaint),
/// `Ok(false)` for a seam-reported event or a deliberate no-op, and `Err(usage)` on
/// a malformed verb line. The repaint nudge itself lives in the wrapper.
fn cross_mouse_apply(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    rest: &str,
) -> Result<bool, String> {
    let ev = parse_mouse(rest)?;
    match seam_egress(term, &ctx.sink, &ev, EgressMode::Backpressured) {
        // Tracking ON but the PTY write failed: honest error, not a false OK.
        Egress::Reported(crate::input::Delivery::Failed) => Err("ERR write failed\n".to_string()),
        // Tracking ON: the seam already wrote the report to the target sink.
        Egress::Reported(_) => Ok(false),
        // Tracking OFF: only a wheel has a meaningful background fallback — move the
        // target viewport. A plain press/release/move is a deliberate no-op (no
        // controller-side selection UI for a background tab).
        Egress::TrackingOff {
            wheel_lines,
            wheel_up,
        } if wheel_lines > 0 => {
            let delta = if wheel_up { wheel_lines } else { -wheel_lines };
            term_lock(term).scroll_display(delta);
            Ok(true)
        }
        Egress::TrackingOff { .. } => Ok(false),
    }
}

/// Cross-session `resize`: `Resize { echo_to_window: false }` confined to the
/// TARGET. This is a SINGLE-SESSION slice of the active-tab path — NOT a full
/// `apply_term_resize`, which is a WINDOW-level op that loops over EVERY session,
/// reconfigures the GPU swapchain, and (via the self `resize` verb's
/// `echo_to_window: true` -> `apply_grid_resize`) calls `request_inner_size`.
/// Here we touch ONLY the target's three artifacts, exactly as `apply_term_resize`
/// does PER SESSION (main.rs:2453-2463): `Terminal::resize` + `aterm_pty::resize` +
/// the asciicast geometry record. We never touch the active window/framebuffer or
/// any other session.
///
/// ASCIICAST FIDELITY (must match the self path): a self resize of session S
/// records `[t, "r", "<cols>x<rows>"]` into S's own `CastRecorder` so a `cast` verb
/// later sees the geometry change on S's timeline. We push the SAME record into the
/// TARGET's `ctx.cast` so a cross resize of S is INDISTINGUISHABLE from a self
/// resize of S in S's recorded history — `cast` is already cross-session-correct
/// (it reads the resolved `ctx.cast`), so omitting this would be an observable
/// cross/self divergence. This is a SIDE-EFFECT-equivalence claim, not a wire-byte
/// one: `seam_egress` emits zero bytes for `Resize`, so the cross arm applies the
/// geometry effect directly rather than routing through the seam.
///
/// Reuses [`parse_resize`] for the identical `ERR out of range` / usage strings.
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
    // Offload the width-change scrollback rewrap OFF the `term` lock (L0 freeze
    // class — see resize_offloading_scrollback). This runs on a control worker,
    // not the main thread, but the same per-session `term` mutex is contended by
    // the PTY reader + main; doing the O(history) rewrap under that lock would
    // still stall them. Detach (brief lock) → rewrap off-lock → re-attach (brief
    // lock). Inline here (already off the main thread), not a spawned worker.
    let pending = term_lock(term).resize_offloading_scrollback(rows, cols);
    if let Some(pending) = pending {
        // Guard the off-lock rewrap: a panic here must not leave the target's detach
        // window wedged (scrollback_detached_for_reflow stuck true → unbounded
        // lazy-buffer leak + all tiered history invisible). Recover to ring-only on
        // panic (audit #5).
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pending.reflow())) {
            Ok(reflowed) => {
                // CONVERGENCE (RFL-3): a width change that raced this rewrap
                // left the store wrapped at the first width; keep rewrapping
                // (still off the main thread, off the lock) until the settled
                // width matches — at most one extra pass once widths settle.
                let mut next = term_lock(term).finish_resize_offload(reflowed);
                while let Some(follow) = next {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| follow.reflow()))
                    {
                        Ok(again) => next = term_lock(term).finish_resize_offload(again),
                        Err(_) => {
                            aterm_log::error!(
                                "cross-session convergence rewrap panicked for session \
                                 {session}; aborting the offload (grid recovered)"
                            );
                            term_lock(term).abort_resize_offload();
                            break;
                        }
                    }
                }
            }
            Err(_) => {
                aterm_log::error!(
                    "cross-session reflow panicked rewrapping session {session} scrollback; \
                     aborting the offload (tiered history lost, grid recovered to ring-only)"
                );
                term_lock(term).abort_resize_offload();
            }
        }
        // Repaint the target: it may be foreground in another window with the reader
        // scrolled into off-screen history, which would otherwise keep the pre-reflow
        // (ring-only, mis-wrapped) view until an unrelated event. Matches the self
        // path's post-reattach Wake::Output (audit #8).
        if let Some(proxy) = proxy {
            let _ = proxy.send_event(Wake::redraw(session));
        }
    }
    aterm_pty::resize(master, rows, cols);
    // Mirror `apply_term_resize`'s per-session asciicast record (main.rs:2459-2463)
    // so the target's own `screen.cast` timeline shows the geometry change.
    {
        let mut rec = ctx.cast.lock().unwrap_or_else(|p| p.into_inner());
        let t = rec.now();
        rec.record_resize(t, cols, rows);
    }
    "OK\n".to_string()
}

/// Cross-session `scroll`: apply the [`ScrollIntent`] DIRECTLY to the TARGET term's
/// viewport (the seam produces no bytes for `ScrollView`; the viewport move lives
/// in `App::input`). Reports `OK <offset> <max>` — the SAME wire shape as the self
/// path's [`cmd_scroll`]. No repaint is posted: a background tab is not visible, so
/// the next time it is shown it reads the new offset; `select` posts `Wake::Output`
/// only because it must repaint a possibly-active selection.
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

/// Apply a [`ScrollIntent`] to a locked [`Terminal`]'s viewport, the same mapping
/// the seam's `App::input` `ScrollView` arm uses (`Up`/`Down` = one screen;
/// `By(n)` = n lines toward history; `Top`/`Bottom` jump). Shared by the
/// cross-session `scroll` arm so its viewport semantics match the self path.
fn apply_scroll_intent(t: &mut Terminal, intent: ScrollIntent) {
    let page = i32::from(t.rows()).max(1);
    match intent {
        ScrollIntent::Up => t.scroll_display(page),
        ScrollIntent::Down => t.scroll_display(-page),
        ScrollIntent::By(n) => t.scroll_display(n),
        ScrollIntent::Top => t.scroll_to_top(),
        ScrollIntent::Bottom => t.scroll_to_bottom(),
        ScrollIntent::PrevPrompt | ScrollIntent::NextPrompt => {
            if let Some(row) =
                crate::input::jump_prompt_target(t, matches!(intent, ScrollIntent::PrevPrompt))
            {
                t.scroll_to_absolute_row(row);
            }
        }
    }
}

/// Dispatch a single request line to its handler, returning the full response
/// (including any trailing data rows) as a string.
///
/// CROSS-SESSION (P1.2): an OPTIONAL leading `@<selector>` token is parsed BEFORE
/// the verb split. A line whose first token does NOT start with `@` takes the
/// verbatim self path (`self_*` args) — byte-for-byte wire-identical, zero
/// regression. A `@<selector>` resolves a DIFFERENT target tuple via
/// `resolve_target` and gates the cross-session access via
/// `cross_session_authorized`; the ~29 per-verb handlers are UNTOUCHED.
#[allow(clippy::too_many_arguments)]
fn handle(
    line: &str,
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
    // The session id of the tab currently on screen, if any — the caller has
    // already resolved it, so naming it here costs nothing and lets an explicit
    // `@<sid>` for that tab take the App input seam (see `front_routed_input`).
    front_active_session: Option<u64>,
    // Artifact verbs park their exact-handle guard here: it must outlive this
    // `String` body all the way to the socket write and the client's ACK.
    reply_retention: &mut Option<ReplyRetention>,
) -> String {
    // Tolerate CRLF clients; the protocol itself is bare-LF terminated.
    let line = line.strip_suffix('\r').unwrap_or(line);

    // P1.2: parse an OPTIONAL leading `@<selector>` BEFORE the verb split. Absence
    // (the first token does not start with '@') is the verbatim self path below —
    // byte-identical to the pre-P1.2 wire form.
    let (selector, line) = match line.split_once(' ') {
        Some((first, tail)) if first.starts_with('@') => (Some(Selector::parse(&first[1..])), tail),
        // A bare `@selector` with no verb (e.g. just `@s-ab12`) is meaningless;
        // strip it and let the empty verb fall through to `ERR unknown verb`.
        None if line.starts_with('@') => (Some(Selector::parse(&line[1..])), ""),
        _ => (None, line),
    };

    let (verb, rest) = match line.split_once(' ') {
        Some((v, r)) => (v, r),
        None => (line, ""),
    };

    // BUILD & META (`version`/`update`/`help`/`verbs`): non-sensitive global
    // provenance answered for ANY authenticated scope BEFORE target resolution (no
    // session needed; a selector is meaningless and ignored). The TABLE is the single
    // source of truth: `is_any_scope_meta(verb)` reads the verb's `Access::AnyScopeMeta`
    // classification, so the dispatch no longer hardcodes the verb names — a verb joins
    // this pre-scope class the moment it is classified in the table, and the binding is
    // total (`access_exceptions_are_exactly_the_declared_sets` pins the set).
    if aterm_types::control_verbs::is_any_scope_meta(verb) {
        return match verb {
            // Global build provenance (version/commit/build-time/binary signature); see
            // crate::build_info (also shown in the macOS About panel).
            "version" => crate::build_info::control_line(),
            // The in-app updater's state — enabled, running build, any staged build's
            // version + "what changed". `update check` forces one synchronous check
            // (may block for tens of seconds on network + disk).
            "update" => cmd_update(rest, scope, proxy),
            // The self-describing protocol catalog — discover every verb this build
            // supports from a running instance (`verbs` is the alias).
            "help" | "verbs" => cmd_help(),
            // The AnyScopeMeta set is pinned by the table test
            // `access_exceptions_are_exactly_the_declared_sets`, so every member has a
            // handler above.
            _ => unreachable!("any-scope-meta verb {verb:?} without a dispatch handler"),
        };
    }

    // OWNER-ONLY, self-scoped verbs (`sessions`/`who`/`whoami`/`grant`/`revoke`/
    // `dial-*`). The TABLE is the single source of truth: `is_owner_only(verb)` reads
    // the verb's `Access::OwnerOnly` classification, so the dispatch no longer
    // hardcodes the verb list — a verb becomes owner-gated the moment it is
    // classified in the table, and the binding is total (`who` is `Read`-op yet
    // Owner-gated: op-class and scope-gate are orthogonal). They read/mutate the
    // connection's OWN ctx/registry and IGNORE a target selector (rejected,
    // fail-closed, so they can never be redirected to mint/read authority on another
    // session's table). Owner scope is required regardless of the verb's op-class.
    if aterm_types::control_verbs::is_owner_only(verb) {
        if !matches!(selector, None | Some(Selector::SelfTok)) {
            return "ERR denied\n".to_string();
        }
        if !matches!(scope, Scope::Owner) {
            return "ERR denied\n".to_string();
        }
        return match verb {
            // Production intercepts these before generic dispatch because it owns
            // the durable handle and (for propose) the following binary frame.
            "operator" | "operator-propose-bin" => "ERR operator unavailable\n".to_string(),
            "sessions" => control_session::cmd_sessions(self_ctx, store),
            "who" => control_session::cmd_who(store, subscribers),
            // The op-level authority primitives and their connection-grain twins
            // (design §6) share the repaint poke: a successful act moves the §4
            // tab marks, which no funnel epoch would otherwise notice.
            "grant" => {
                poke_connections_on_ok(proxy, control_session::cmd_grant(self_ctx, scope, rest))
            }
            "revoke" => {
                poke_connections_on_ok(proxy, control_session::cmd_revoke(self_ctx, scope, rest))
            }
            // `connect`/`disconnect`: the declarative session-connection verbs.
            // Self-scoped like `grant` (the endpoints ride as `dst=`/`src=`
            // arguments); they take the STORE to resolve both endpoints and act
            // on the destination's table through the connections seam.
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
            "whoami" => control_session::cmd_whoami(self_ctx, scope),
            // Network-drive meta verbs. `dial <name>` itself is handled earlier in
            // the serve loop (it takes over the connection); these are its
            // normal-response companions.
            "dial-list" => cmd_dial_list(),
            "dial-token" => cmd_dial_token(rest),
            // A BARE `dial` reaches here: the serve-loop interception fires only on the
            // `"dial "` prefix (a name follows), but `read_request_line` strips the
            // newline leaving exactly "dial" with no trailing space — so a name-less
            // `dial` misses the takeover and lands in this owner-only match. Answer with
            // a clean usage error rather than panicking the connection thread on the
            // `unreachable!` below (which dropped the connection with a stderr backtrace).
            "dial" => {
                "ERR dial expects a connection name and a verb (dial <name> [@<sid>] <verb...>)\n"
                    .to_string()
            }
            // The owner-only set is pinned by the table test
            // `access_exceptions_are_exactly_the_declared_sets`, so every member has
            // a handler above.
            _ => unreachable!("owner-only verb {verb:?} without a dispatch handler"),
        };
    }

    // Resolve the dispatch target. No selector (or `@.`) => the verbatim self
    // tuple (zero regression). Otherwise resolve the sibling from the registry.
    let self_tuple: Target = (
        self_term.clone(),
        self_master,
        self_session,
        self_ctx.clone(),
    );
    let is_cross = !matches!(selector, None | Some(Selector::SelfTok));
    let (term, master, session, ctx) = match &selector {
        None | Some(Selector::SelfTok) => self_tuple,
        Some(sel) => match resolve_target(&self_tuple, store, sel) {
            Some(t) => t,
            None => return "ERR no such session\n".to_string(),
        },
    };
    let ctx: &SessionCtx = &ctx;
    // Does this explicit selector name the tab that is ON SCREEN? The caller
    // already resolved the front tab, so this is a plain id compare — no extra
    // round trip. Only a CROSS request can be front-routed (a flagless one is
    // already on the App seam), and the event loop re-checks before applying, so
    // a stale answer here is harmless.
    let targets_front = front_routed(is_cross, front_active_session, session);

    // Observe a scoped caller's FRONT input consumer before the op gate.  A native
    // Settings tab has no modal `OverlayKind`, but key/mouse/paste events are still
    // consumed by its reducer and can persist default-OFF security knobs.  The
    // main-thread observation therefore supplies an additional argument-aware
    // escalation: Settings -> ConfigWrite; any transient overlay -> Owner-only.
    // Owner and cross-session requests never take this hop.  A failed hop is an
    // authorization failure, never "no surface" (fail closed).
    let front_surface =
        if !is_cross && !matches!(scope, Scope::Owner) && is_front_driving_verb(verb) {
            match control_media::call_main(proxy, |reply| Wake::FrontControlSurface { reply }) {
                Ok(surface) => surface,
                Err(error) => {
                    log_denial(
                        AUDIT_SUBSYSTEM,
                        &format!("self {verb} front-surface authorization failed"),
                        aterm_containment::mode_or_containment(),
                        &format!("could not observe front input authority: {error}"),
                    );
                    return "ERR denied\n".to_string();
                }
            }
        } else {
            FrontControlSurface::None
        };
    let front_escalation = front_drive_escalation(scope, verb, front_surface);

    // Op-scope gate (design 7.2). EXHAUSTIVE, fail-closed. Gates on the EFFECTIVE op
    // (`dispatch_authorized`): the verb's base op ESCALATED by its argument for the
    // indirect seams (`invoke <clipboard/config action>`, `open prefs|settings`) so a
    // plain WriteInput edge cannot tunnel to ClipboardWrite/ConfigWrite through them —
    // the escalated op REPLACES the base op, so an explicit fine-op edge for exactly
    // that action still passes. `rest` is the raw argument here (json-flag stripping is
    // below and read-only, so it never touches these write-class verbs).
    //
    // SELF path (no `@`/`@.`): Owner passes everything BEFORE any lookup (so the
    // existing aterm-ctl client is byte-for-byte unchanged); an Edge may run a verb
    // ONLY if its token authorizes the effective op against the now-active session; the
    // catch-all denies an Edge for any None-op verb.
    //
    // CROSS path (`@other`): in ADDITION the cross-session authority must hold — an
    // Owner reaches siblings (same trust domain); a scoped Edge needs a
    // `decide_edge`-permitted token against the TARGET's table (default-DENY).
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
    } else if !matches!(scope, Scope::Owner)
        && !front_escalation.map_or_else(
            || dispatch_authorized(scope, verb, rest, ctx),
            |need| scope_holds_escalation(scope, need, ctx),
        )
    {
        // SELF path, Edge scope: re-verify the token against the session that is
        // active RIGHT NOW — NOT op-match alone. The control socket has ONE global
        // ActiveHandle that `sync_active_session` retargets to the new frontmost
        // active tab on every tab switch / cross-window focus change. An edge token
        // is a single (src, dst, op) grant against ONE session's table; matching only
        // the op let an edge scoped to session B drive/read whatever session A became
        // frontmost after the user switched tabs or windows — a confused-deputy
        // authority escape (e.g. a WriteInput edge injecting keystrokes into, or
        // resizing, an arbitrary foreground session). Owner keeps full self-power (no
        // lookup, byte-for-byte the legacy client); this collapses SELF and CROSS onto
        // the SAME per-session `decide_edge` predicate, so the only difference is
        // whether UI side-effects run, never whether authority holds.
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
    let term = &term;
    // The `aterm-control` verbs take a host, not a term. Built here from the tuple
    // already resolved above, borrowed and zero-cost, so the cross-session path
    // hands them the RESOLVED target exactly as the per-verb args used to. The
    // FLEET handles are this dispatcher's to give — the registry it was called
    // with and the resolved target's own sink — so the trait's roster/resolve/
    // write_input answer for the real process, not as a host that keeps none; and
    // `session` BINDS the host, so a sid from elsewhere is refused rather than
    // served against this target.
    let host = GuiHost::with_fleet(session, term, Some(proxy), subscribers, store, &ctx.sink);

    // `--json` READ MODE: a structured-JSON foundation for the read verbs. The flag
    // is parsed off `rest` HERE (additive: a line without it is byte-identical text)
    // and routed to the matching `*_json` emitter; the flag is then STRIPPED so the
    // text fall-through below never sees it. Only the json-capable read verbs branch;
    // a read verb with NO json form answers an honest ERR (see [`json_unsupported`]
    // for the allowlist rule); every other verb (and any json-capable verb WITHOUT
    // the flag) is untouched, preserving the existing text wire byte-for-byte. The
    // op-scope gate above already authorized `verb` (json is a serialization choice,
    // not a new op).
    if rest.contains("json")
        && let (true, body) = take_json_flag(rest)
    {
        let json = match verb {
            "text" => Some(control_query::cmd_text_json(term)),
            // `screen` is ALWAYS styled JSON; accept `screen --json` for symmetry.
            "screen" => Some(control_query::cmd_screen_styled_json(term)),
            "cursor" => Some(control_query::cmd_cursor_json(term)),
            "dims" => Some(control_query::cmd_dims_json(term, session, proxy)),
            "metrics" => Some(control_query::cmd_metrics_json(Some(term), &body)),
            "blocks" => Some(control_selection::cmd_blocks_json(&host, session, &body)),
            "edges" | "grants" => Some(control_session::cmd_edges_json(ctx)),
            _ => None,
        };
        if let Some(out) = json {
            return out;
        }
        if let Some(err) = json_unsupported(verb) {
            return err;
        }
    }

    // Every AUTHORIZED control attempt that can inject bytes, signal the
    // child, or mutate its cursor coordinate space is a newer input boundary —
    // even when its arguments are malformed and dispatch later returns an
    // error. Fence before parsing so a swallowed key's LICENCE cannot survive
    // an ignored attempt and be borrowed by subsequent PTY output.
    // Authorization has already completed above; denied callers cannot mutate
    // visible-window effect state through this side channel.
    if control_attempt_closes_cursor_license(verb) {
        let cleared = front_routed_license_clear(proxy, session);
        if !cleared.starts_with("OK") {
            return cleared;
        }
    }

    // D3: the un-bypassable SELF-FEED FLOOR. Every self-targeted input-injection
    // verb passes a per-session token bucket FIRST, so a raw client cannot drive
    // an output->observe->write feedback storm by looping `feed @.` (the L2
    // `SelfGovernor` only binds drivers that link `aterm-agent`; this floor binds
    // everyone). Generous cap; legitimate driving never trips it. The floor scopes
    // to SELF: a cross-session write targets a DIFFERENT session (so it cannot
    // self-loop) and is separately authority-gated by that session's edge token.
    // `feed-bin` is NOT listed here — it is intercepted before this dispatch and
    // passes the SAME floor in `run_feed_bin`.
    if !is_cross
        && matches!(
            verb,
            "send" | "key" | "ctrl" | "feed" | "mouse" | "paste" | "turn"
        )
    {
        let nbytes = rest.len().max(1);
        if !crate::inject_floor::allow(self_session, nbytes) {
            return "ERR rate (self-feed floor)\n".to_string();
        }
    }

    // Turn ARBITRATION: while a `turn` holds this session's lease, another
    // connection's write verbs would interleave into the very exchange the lease
    // protects — refuse them, naming the holder, so an orchestrator can wait and
    // retry. `cmd_turn` itself re-checks under the lease lock (this check is the
    // fast fail; the acquire is the authoritative one). `signal` stays exempt
    // (out-of-band escape hatch) and keyboard input never passes this seam.
    if matches!(
        verb,
        "send" | "key" | "ctrl" | "feed" | "mouse" | "paste" | "turn"
    ) {
        let held = ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(crate::Lease::write_block_turn);
        if let Some(id) = held {
            return format!("ERR busy turn={id}\n");
        }
    }

    let resp = match verb {
        "text" => control_query::cmd_text(term),
        // The LOSSLESS styled-screen read (keystone): full per-cell colour +
        // resolved decorations + cursor + dims + seq as one JSON frame. Always
        // styled-JSON (no plaintext variant) — `--json` is implied.
        "screen" => control_query::cmd_screen_styled_json(term),
        "cursor" => control_query::cmd_cursor(term),
        "cell" => control_query::cmd_cell(term, rest),
        "search" => control_query::cmd_search(term, rest),
        // `edges`/`grants`: list this session's inbound capability edges (the
        // EdgeTable rows). A pure observer of the AUTHORITY surface, so it is gated
        // as `ReadScreen` like every other read verb; cross-session reads a sibling's
        // table through the same `@<selector>` resolution + gate.
        "edges" | "grants" => control_session::cmd_edges(ctx),
        // `lease`: the explicit COOPERATIVE drive lease for raw (non-`turn`) drivers.
        // op-class Write (a driving assertion — cross-session needs a write edge), but
        // deliberately NOT in the write-arbitration seam above (it MANAGES the lease,
        // it does not inject input), so `lease status`/`release` answer even mid-turn.
        "lease" => control_session::cmd_lease(ctx, rest),
        // `family [<sid>]`: the session HIERARCHY (parent + children) for a target,
        // from the registry's parent links. The no-arg form walks from the RESOLVED
        // (gated) session; an EXPLICIT `<sid>` argument walks an ARBITRARY node, so it
        // is Owner-only (a scoped Edge may not enumerate trees it has no edge into) —
        // the `scope` guard mirrors the `sessions` verb's Owner gate.
        "family" => control_session::cmd_family(ctx, store, scope, rest),
        // `ready [timeout_ms]`: block until the target is Alive AND idle (at an
        // OSC-133 prompt, or the kernel idle-settle window), so an agent can chain
        // sessions without busy-polling. Read-side (observes lifecycle/blocks).
        "ready" => control_session::cmd_ready(term, store, session, rest, subscribers),
        // `await <idle|seq|match|block>`: block until the Observation Kernel (L0)
        // latches the predicate. The event-driven, no-silent-loss generalization of
        // `ready`/`wait`: it adds the OSC-133-independent `idle`/`match`/`seq`
        // predicates, so it works for alt-screen agent TUIs (Claude) — unlike
        // `wait` (OSC-133-only). Registers a subscriber so it wakes on output AND at
        // the idle deadline — no fixed-interval poll.
        "await" => control_session::cmd_await(term, store, session, rest, subscribers),
        // `turn`: one complete human turn — type text (paste semantics), VERIFIED
        // submit (content_seq must advance, else re-press: the server-side fix for
        // the client-racy paste-chip/Enter seam), settle, return the settled screen.
        // It both WRITES input and READS the screen, so a scoped caller must hold
        // both authorities; a single-op edge token cannot, so non-Owner scopes that
        // pass the WriteInput gate above are still denied here unless read is also
        // authorized — fail-closed, no read-capability leak through a write edge.
        // Input delivery is resolved HERE per path (the composite itself is
        // input-API-blind): cross = the source-blind seam on the resolved target,
        // self = the event-loop seam, exactly like the plain `paste`/`key` arms.
        "turn" => {
            if !matches!(scope, Scope::Owner) && !cross_session_authorized(scope, "text", ctx) {
                log_denial(
                    AUDIT_SUBSYSTEM,
                    &format!("turn -> {}", ctx.self_id.as_str()),
                    aterm_containment::mode_or_containment(),
                    "turn needs read+write; edge lacks read",
                );
                return "ERR denied\n".to_string();
            }
            if turn_input_route(is_cross, targets_front) == TurnInputRoute::Front {
                // An explicit selector naming the visible tab is still a
                // visible App drive. Route BOTH phases of the composite turn
                // through the same targeted event-loop seam as plain
                // paste/key, so paste/Return movement provenance, input
                // ordering and every other host side effect stay intact.
                let paste = |text: &str| {
                    front_routed_input(
                        proxy,
                        session,
                        Some(InputEvent::Paste(control_input::paste_text(text))),
                        "ERR\n",
                    )
                    .starts_with("OK")
                };
                let press = |name: &str| {
                    front_routed_input(proxy, session, parse_key(name), "ERR\n").starts_with("OK")
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
                    },
                )
            } else if turn_input_route(is_cross, targets_front) == TurnInputRoute::Background {
                let paste = |text: &str| {
                    cross_input(
                        term,
                        ctx,
                        Some(InputEvent::Paste(control_input::paste_text(text))),
                        "ERR\n",
                    )
                    .starts_with("OK")
                };
                let press =
                    |name: &str| cross_input(term, ctx, parse_key(name), "ERR\n").starts_with("OK");
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
                    },
                )
            } else {
                let paste = |text: &str| control_input::cmd_paste(proxy, text).starts_with("OK");
                let press = |name: &str| control_input::cmd_key(proxy, name).starts_with("OK");
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
                    },
                )
            }
        }
        "send" if !is_cross || targets_front => front_routed_input(
            proxy,
            session,
            Some(InputEvent::KeySequence(control_input::send_bytes(rest))),
            "ERR\n",
        ),
        "send" => control_input::cmd_send(&ctx.sink, rest),
        // Phase 0.5: the SELF (active-tab) path funnels `key`/`ctrl`/`mouse`/`paste`/
        // `focus`/`resize`/`scroll` through the source-blind `App::input` seam on the
        // EVENT LOOP (posts `Wake::Input` / a reply-bearing resize), so the bytes are
        // byte-identical to human input AND the renderer/window/gesture side-effects
        // (snap-to-bottom, selection gesture, `request_inner_size`) run for the tab
        // the user is looking at. Leave these arms UNCHANGED — they are the only ones
        // that may touch the active UI.
        //
        // P1.2 follow-up: the CROSS-session (`@other`) path no longer fails closed. A
        // background target is NOT the active tab, so there is no App UI/gesture/window
        // state to touch — and `seam_egress` is already source-blind and session-
        // agnostic (it reads the modes from the GIVEN term and writes the GIVEN sink).
        // So a cross arm resolves `(target_term, target_sink) = (term, &ctx.sink)` the
        // SAME way `send`/`feed` already do, builds the IDENTICAL `InputEvent` via the
        // shared `parse_*` helpers, and calls `seam_egress` DIRECTLY on the control
        // thread (no `Wake::Input`). Op-scope is already `WriteInput`-gated above
        // (`cross_session_authorized`), so these run only after the edge/owner check.
        // An explicit `@<sid>` that resolved to the tab ON SCREEN is not a
        // background target: route it through the App seam so a named-session
        // drive is indistinguishable from a flagless one (see
        // [`front_routed_input`]). Genuinely background targets keep the direct,
        // round-trip-free egress below.
        "key" if is_cross && targets_front => {
            front_routed_input(proxy, session, parse_key(rest), "ERR\n")
        }
        "key" if is_cross => cross_input(term, ctx, parse_key(rest), "ERR\n"),
        "key" => control_input::cmd_key(proxy, rest),
        "ctrl" if is_cross && targets_front => front_routed_input(
            proxy,
            session,
            parse_ctrl(rest),
            "ERR usage: ctrl <single-letter>\n",
        ),
        "ctrl" if is_cross => cross_input(
            term,
            ctx,
            parse_ctrl(rest),
            "ERR usage: ctrl <single-letter>\n",
        ),
        "ctrl" => control_input::cmd_ctrl(proxy, rest),
        "feed" if !is_cross || targets_front => match control_input::feed_bytes(rest) {
            Ok(bytes) => {
                let n = bytes.len();
                let response = front_routed_input(
                    proxy,
                    session,
                    Some(InputEvent::KeySequence(bytes)),
                    "ERR\n",
                );
                if response.starts_with("OK") {
                    format!("OK {n} bytes\n")
                } else {
                    response
                }
            }
            Err(error) => error.to_string(),
        },
        "feed" => control_input::cmd_feed(&ctx.sink, rest),
        "signal" => control_input::cmd_signal(master, rest),
        "mouse" if is_cross && targets_front => front_routed_input(
            proxy,
            session,
            control_input::parse_mouse(rest).ok(),
            "ERR usage: mouse <press|release|move|wheel-up|wheel-down> ...\n",
        ),
        "mouse" if is_cross => cross_mouse(term, ctx, session, proxy, rest),
        // SELF (active-tab) mouse: pass `scope` so a NON-OWNER (scoped-edge) gesture
        // has its copy-on-select CLIPBOARD side-effect suppressed (the exfil fence);
        // Owner and human gestures are unaffected. The suppression is stamped on the
        // injected event, NOT a `Source` branch in the byte seam.
        "mouse" => control_input::cmd_mouse(proxy, scope, rest),
        "paste" if is_cross && targets_front => front_routed_input(
            proxy,
            session,
            Some(InputEvent::Paste(control_input::paste_text(rest))),
            "ERR\n",
        ),
        "paste" if is_cross => cross_input(
            term,
            ctx,
            Some(InputEvent::Paste(control_input::paste_text(rest))),
            "ERR\n",
        ),
        "paste" => control_input::cmd_paste(proxy, rest),
        "focus" if is_cross && targets_front => match control_input::parse_focus(rest) {
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
        // `image` rides the shared renderer + event loop, which act on the ACTIVE tab;
        // `image read [...]` reads the STRUCTURED inline-image payloads from the
        // (target) terminal model — headless-safe and cross-session-correct, so it
        // is matched BEFORE the framebuffer-rasterize arm. `term` is already the
        // resolved target for cross reads.
        "image" if rest.split_whitespace().next() == Some("read") => control_media::cmd_image_read(
            term,
            rest.strip_prefix("read").unwrap_or(rest).trim_start(),
        ),
        // Framebuffer capture. Cross-session (`@<sel>`): capture the window whose
        // ACTIVE tab displays the resolved session through the same app-present
        // path as self, including decorations, tab strip, and overlays. A session
        // no window shows (background tab) fails honestly instead of silently
        // capturing the WRONG (front) session. This is client-destination truth,
        // not platform-compositor visibility or scanout.
        "image" => {
            control_media::cmd_image(proxy, queue, rest, sock_dir, is_cross.then_some(session))
                .into_body_retaining(reply_retention)
        }
        // `window` captures a full FRONT-window artifact (platform chrome stitched
        // around the exact submitted client destination) to a PNG. It does not
        // claim compositor visibility or scanout. APP-LEVEL verb: it acts on the resolved instance's
        // FRONT window (the `@<sid>` ROUTES to the instance — cross-instance via the
        // client relay — it does not pick a specific session's pixels; `image` is the
        // per-session pixel verb). One rule for every app-level verb below: the
        // selector selects WHERE it runs; the verb acts on that instance's front
        // window. So `@peer window` screenshots the PEER's window. Auth is enforced
        // upstream (the cross-session gate) before we get here.
        "window" => {
            control_media::cmd_window(proxy, rest, sock_dir).into_body_retaining(reply_retention)
        }
        // `video <seconds> [full] [keys] [pace]`: record the front window's GPU
        // swapchain-destination frames submitted with application present calls into
        // a PNG sequence + index.json. An AI can inspect renderer smoothness/flashes
        // and correlate pre-routing input attempts with later submitted frames; the
        // tap does not observe compositor selection or scanout. `keys` is owner-only
        // (enforced inside); its samples are not PTY-delivery or glyph receipts.
        "video" => control_media::cmd_video(proxy, rest, sock_dir, matches!(scope, Scope::Owner))
            .into_body_retaining(reply_retention),
        // `chrome`: the resolved instance's front window native UI (app-level; `@<sid>`
        // routes to the instance, per the rule above — `@peer chrome` reads the peer's).
        "chrome" => control_media::cmd_chrome(proxy),
        // `panes`: the ACTIVE-tab split-pane layout (cell rects + focus + zoom —
        // split-pane audit introspection). Cross-session (`@<sel>`): the window
        // whose ACTIVE tab displays the resolved session — the `image` routing
        // rule, so `@<sid> panes` and `@<sid> image` describe the SAME window.
        "panes" => control_media::cmd_panes(proxy, is_cross.then_some(session)),
        // `controls <target>` dumps an own-rendered app surface's controls as text — the
        // analogue of `chrome`. Built on the main thread from the pure surface model, so
        // it works headless.
        // App-level (the resolved instance's aux GUI); `@<sid>` routes to the instance.
        "controls" => control_media::cmd_controls(proxy, rest),
        // `open <target>`: app-level UI on the resolved instance (`@<sid>` routes).
        "open" => control_media::cmd_open(proxy, rest),
        // `invoke <action>`: fire a menu action by name (enabled-gated, single sink).
        // App-level per the rule above — `@peer invoke NewTab` opens a tab in the peer.
        "invoke" => control_media::cmd_invoke(proxy, rest),
        // `rain [status|on|off|toggle]`: the per-session matrix-rain override on the
        // focused window's FRONT session. App-level per the rule above (`@peer rain
        // status` reads the peer's front session).
        "rain" => control_media::cmd_rain(proxy, rest),
        // `tone [status]`: the FRONT window's tone-of-typing mood + every gate on
        // the classifier. Read-only and App-level like `rain status` (`@peer tone`
        // reads the peer's front window).
        "tone" => control_media::cmd_tone(proxy, rest),
        // `trail [status|<n>]`: the FOCUSED window's cursor-trail diagnostics.
        // The `<n>` form prints the last n spawn-seam verdicts from the
        // engine's diagnostic ring — licensed/declined + reason
        // (no-fresh-hint / no-credits / off-shape) + origin/target; `status`
        // prints ONE line of standing engine state (style, every gate to the
        // glass, the cumulative licensed/declined tally, live ribbon).
        // Read-only and App-level like `tone` (`@peer trail` reads the peer's
        // focused window); the one-command face of the ATERM_TRACE_SPAWN
        // sensor and of "I don't see the rainbow cursor trails".
        "trail" => control_media::cmd_trail(proxy, rest),
        // `spawn`: mint ONE new tab session and reply `OK <sid>` — birth as a
        // socket primitive. The sid is immediately addressable (`@<sid> turn …`),
        // so fleet provisioning is a loop of spawn calls, no exec'ing binaries.
        "spawn" => control_media::cmd_spawn(proxy, rest),
        // `@<sid> close`: retire the RESOLVED session by id (self with no selector).
        // The dispatch already resolved `session`/`ctx` from the selector, so close
        // acts on exactly the addressed session — the death half of `spawn`.
        "close" => control_media::cmd_close(proxy, session, ctx.self_id.as_str()),
        // `settings [open|close|toggle]` drives the CROSS-PLATFORM native Settings tab
        // (`open settings` is the same compatibility alias), so a driver can open it
        // then read its current native route with `controls settings` or capture the
        // real frame with `image` on any platform.
        // App-level UI on the MAIN thread (write-class); `@<sid>` routes to the instance.
        "settings" => control_media::cmd_settings_overlay(proxy, rest),
        // `tab`: drive the resolved instance's front-window tabs (app-level; `@<sid>`
        // routes to the instance).
        "tab" => control_input::cmd_tab(proxy, rest),
        // Toggle the drop-target highlight on the FRONT window (testing/automation
        // of the drag-and-drop affordance; a real drag drives the same flag). Always
        // targets the frontmost window, so a `@<sel>` is meaningless here.
        "hover" => control_input::cmd_hover(proxy, rest),
        // Cross-session `resize` does NOT go through the seam: `seam_egress` emits no
        // bytes for `Resize`, and `App::input`'s Resize arm resizes the WINDOW (every
        // tab + the GPU swapchain). A background target has no window to echo to, so we
        // replicate ONLY the term+PTY pair (`echo_to_window: false` semantics) on the
        // TARGET, never the active window/framebuffer.
        "resize" if is_cross && targets_front => front_routed_resize(proxy, session, rest),
        "resize" if is_cross => cross_resize(term, master, ctx, session, Some(proxy), rest),
        "resize" => control_input::cmd_resize(proxy, rest),
        // Cross-session `scroll` also bypasses the seam (`ScrollView` emits no bytes;
        // the viewport move lives in `App::input`). It applies the `ScrollIntent`
        // DIRECTLY to the TARGET term's viewport and reports `OK <offset> <max>` — the
        // SAME wire shape as the self path's `cmd_scroll`. `select` is already
        // cross-correct (mutates the target term + fires a repaint keyed by target id).
        "scroll" if is_cross && targets_front => {
            front_routed_scroll(term, proxy, session, rest)
        }
        "scroll" if is_cross => cross_scroll(term, rest),
        "scroll" => control_input::cmd_scroll(term, proxy, rest),
        "dims" => control_query::cmd_dims(term, session, proxy),
        // `metrics` -> live render/latency counters (process-global; the active tab's
        // grid supplies rows/cols). Lets a driving AI MEASURE responsiveness directly
        // rather than scraping the $ATERM_TRACE_LATENCY stderr log. Read-side.
        "metrics" => control_query::cmd_metrics(Some(term), rest),
        "lines" => control_query::cmd_lines(term),
        "line" => control_query::cmd_line(term, rest),
        "modes" => control_query::cmd_modes(term),
        // `custody` -> WHO last took the reading position or the highlight, by name.
        // Read-side: it reports the engine's own custody record and no screen content.
        "custody" => control_query::cmd_custody(term),
        "title" => control_query::cmd_title(term),
        "cwd" => control_query::cmd_cwd(term),
        "blocks" => control_selection::cmd_blocks(&host, session, rest),
        "blocktext" => control_selection::cmd_blocktext(&host, session, rest),
        "wait" => control_selection::cmd_wait(&host, session, rest),
        "colors" => control_query::cmd_colors(term),
        "select" => control_selection::cmd_select(&host, session, rest),
        "selection" => control_selection::cmd_selection(&host, session),
        "copy" => control_selection::cmd_copy(&host, session),
        // `cast` reads the TARGET session's own asciicast recorder (its recorded
        // program-output history), not the shared renderer, so it is correct
        // cross-session — no `is_cross` guard.
        // `cast frames [count=N]` expands the recording into keyframe screens (line-
        // framed like `text`); bare `cast` is the byte-framed asciicast.
        "cast" if rest.split_whitespace().next() == Some("frames") => {
            control_session::cmd_cast_frames(ctx, rest.strip_prefix("frames").unwrap_or("").trim())
        }
        "cast" => control_session::cmd_cast(ctx),
        // `temporal [tick]` reconstructs the TARGET session's screen at a past
        // instant from its OWN temporal recorder (the read half of B.9), never the
        // shared renderer — correct cross-session like `cast`, no `is_cross` guard.
        "temporal" => control_session::cmd_temporal(ctx, rest),
        // `turns`: read back this session's TURN LEDGER (what was driven + what
        // settled), the durable twin of the live events digest. Read-side.
        // `history [<n>] [since=<id>]`: read the session's TURN LEDGER (what was
        // driven + what settled). Named `history`, not `turns`, so it never collides
        // with the `turn` driver — `turn` writes, `history` reads.
        "history" => control_session::cmd_history(ctx, rest),
        // `meta`: read/write the TARGET session's USER metadata (title/description/
        // icon — the operator's identity for the session, held on its ctx). The
        // write sub-forms were already escalated to WriteInput by the dispatch
        // gate (`escalated_op`). On an ACTUAL change the arm (not the pure
        // handler, which stays headless-testable) fans out the side-effects: a
        // `Wake::MetaChanged` so tab labels showing the session repaint, and a
        // subscriber notify so an `events` watcher drains the fresh timeline
        // record as `EVENT <sid> meta …` immediately.
        "meta" => {
            let (resp, changed) = control_session::cmd_meta(term, store, session, ctx, rest);
            if changed {
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
        // `timeline`: read the TARGET session's EVENT TIMELINE (the lifecycle
        // twin of `history`) — pure observer of the ctx ring, cross-session
        // correct like `cast`/`history`, no `is_cross` guard.
        "timeline" => control_session::cmd_timeline(ctx, rest),
        // `status`: the TARGET session's Subject + classified Status record. A
        // main-thread hop, unlike its `meta`/`timeline` neighbours: the
        // classifier is `App` state owned by the event loop, not ctx state the
        // control thread can read directly.
        "status" => control_media::cmd_session_status(proxy, session, rest),
        // `sessions`/`grant`/`revoke`/`whoami` are handled SELF-SCOPED above.
        _ => "ERR unknown verb (try: help)\n".to_string(),
    };
    // R13: stamp the `content_seq` baseline on a successful input-write reply so a
    // driver can correlate its own input with the resulting output race-free:
    //   read a seq -> write (send/key/…) -> `await seq <that+1>` for the response.
    // Additive (` seq=<n>` before the trailing newline); ERR replies are untouched.
    stamp_input_seq(verb, resp, term)
}

/// Append ` seq=<content_seq>` to a successful input-write reply (R13). The seq is
/// the target grid's content baseline sampled right after the write is dispatched,
/// so `await seq <n+1>` waits for the output the input causes. Only the input-write
/// verbs are stamped, and only an `OK` reply (an `ERR`/`OK timeout` is unchanged);
/// the field is appended before the trailing newline so line-based parsers still
/// read one line.
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

/// Whether `rest` carries the `--json` / `json` read-mode flag, and the `rest`
/// with that flag token removed (so each verb's existing positional parse runs
/// UNCHANGED on the remainder). Additive: a verb line WITHOUT the flag returns
/// `(false, rest.to_string())` verbatim, so the text path is byte-identical.
fn take_json_flag(rest: &str) -> (bool, String) {
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

/// The reply for an EXPLICITLY-requested `--json`/`json` read mode on a verb with
/// no structured emitter: `Some("ERR json: not supported for <verb>\n")` for the
/// allowlisted verbs below, `None` to fall through to the text emitters with the
/// ORIGINAL `rest` (byte-identical to a flag-less line).
///
/// ALLOWLIST, fail-open: only verbs whose grammar can never carry a literal
/// `json`/`--json` token as argument DATA are refused. A payload-bearing verb
/// (`send --json`, `turn … json`, `search json`, `await match json`) must
/// receive the token verbatim, so an unlisted verb keeps
/// the silent text fall-through instead of risking a payload break.
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

/// Phase 0.5: post a reply-bearing [`InputEvent`] and BLOCK on the seam's
/// [`InputOutcome`] (mirrors `cmd_image`'s `mpsc` round-trip). Used by `resize`
/// (range-reject) and the input verbs — the caller maps the outcome to its reply
/// string. `op` is the AUDIT class of the OPERATION (`ReadScreen` for the
/// view-control verbs, `WriteInput` for the input verbs), captured from the verb
/// itself, NOT the connection's scope: a control connection is always a
/// `Controller`, so the scope adds nothing to the audit `Source`.
fn post_input_reply(
    proxy: &EventLoopProxy<Wake>,
    op: Op,
    batch: Vec<InputEvent>,
) -> Result<InputOutcome, String> {
    post_input_reply_to(proxy, op, batch, None)
}

/// [`post_input_reply`] with an EXPLICIT session target — the seam a verb that
/// named `@<sid>` uses when that sid resolved to the tab currently on screen.
/// `None` is the historical front-tab contract; `Some` is re-resolved on the
/// event loop (see [`Wake::Input::session`]), so a tab switch racing the request
/// degrades to the hidden-session path instead of typing into the wrong tab.
fn post_input_reply_to(
    proxy: &EventLoopProxy<Wake>,
    op: Op,
    batch: Vec<InputEvent>,
    session: Option<u64>,
) -> Result<InputOutcome, String> {
    let src = Source::Controller { op };
    control_media::call_main(proxy, |tx| Wake::Input {
        batch,
        src,
        reply: Some(tx),
        session,
    })
    .map_err(|error| format!("ERR input dispatch failed: {error}\n"))
}

#[cfg(test)]
mod tests {

    /// THE ROUTING LAW an explicit selector takes (regression: `@self` — the
    /// selector the docs recommend — expands client-side to `@<sid>`, which was
    /// classified as a background target even when it named the tab on screen.
    /// It then egressed straight to the sink, skipping the App input seam and
    /// therefore every side effect that seam owns: the typing-momentum hints all
    /// the cursor effects read, and the `video ... keys` ledger, which reported
    /// "no input attempts logged" to an AI that had driven the entire take.)
    #[test]
    fn an_explicit_selector_for_the_front_tab_is_routed_through_the_input_seam() {
        use super::{
            TurnInputRoute, license_clear_reply, control_attempt_closes_cursor_license,
            front_routed, turn_input_route,
        };
        // Named the tab on screen -> the seam.
        assert!(front_routed(true, Some(7), 7));
        // Named a BACKGROUND tab -> the direct egress it exists for.
        assert!(!front_routed(true, Some(7), 9));
        // No front tab at all (headless with no window, every tab closed) ->
        // nothing to be the front of; fail to the background path.
        assert!(!front_routed(true, None, 7));
        // A FLAGLESS request is already on the seam and must never be diverted
        // through this predicate, whatever the front tab happens to be.
        assert!(!front_routed(false, Some(7), 7));
        assert!(!front_routed(false, None, 7));

        // `turn` is composite paste + Return, but both phases must make the
        // identical routing decision. In particular, explicit `@self` (which
        // arrives as a cross selector naming the visible sid) is FRONT, while
        // a genuinely hidden target remains on the background sink.
        assert_eq!(turn_input_route(true, true), TurnInputRoute::Front);
        assert_eq!(turn_input_route(true, false), TurnInputRoute::Background);
        assert_eq!(turn_input_route(false, false), TurnInputRoute::Local);
        assert_eq!(turn_input_route(false, true), TurnInputRoute::Local);

        // The reply distinguishes transport failure from a harmless race. A
        // `false` main-thread result means the tab switched before the fence;
        // the target is now background and its otherwise valid signal must not
        // be dropped.
        assert_eq!(license_clear_reply(Ok(true)), "OK\n");
        assert_eq!(license_clear_reply(Ok(false)), "OK\n");
        assert!(license_clear_reply(Err("event loop gone")).starts_with("ERR "));

        // Parsing happens only after this verb-level boundary, so malformed
        // front and cross forms cancel exactly like valid ones. Read-only and
        // unauthorized requests never reach this post-authorization predicate.
        for verb in [
            "send", "key", "ctrl", "feed", "signal", "mouse", "paste", "focus", "turn",
            "resize", "scroll",
        ] {
            assert!(
                control_attempt_closes_cursor_license(verb),
                "authorized {verb} attempt must fence before argument parsing"
            );
        }
        for verb in ["text", "screen", "cursor", "image", "metrics"] {
            assert!(!control_attempt_closes_cursor_license(verb));
        }
    }

    struct WireProbe {
        alive: Arc<AtomicBool>,
        prepared: Arc<AtomicBool>,
        fail_prepare: bool,
    }

    impl WireRetention for WireProbe {
        fn prepare_write(&mut self) -> Result<(), String> {
            self.prepared.store(true, Ordering::Release);
            if self.fail_prepare {
                Err("injected identity replacement".to_string())
            } else {
                Ok(())
            }
        }
    }

    impl Drop for WireProbe {
        fn drop(&mut self) {
            self.alive.store(false, Ordering::Release);
        }
    }

    struct GuardCheckingWriter {
        alive: Arc<AtomicBool>,
        prepared: Arc<AtomicBool>,
        expect_alive: bool,
        bytes: Vec<u8>,
        flushed: bool,
    }

    impl Write for GuardCheckingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert_eq!(
                self.alive.load(Ordering::Acquire),
                self.expect_alive,
                "artifact guard lifetime at socket write"
            );
            assert!(
                self.prepared.load(Ordering::Acquire),
                "wire identity validation must precede write"
            );
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            assert_eq!(
                self.alive.load(Ordering::Acquire),
                self.expect_alive,
                "artifact guard lifetime at socket flush"
            );
            self.flushed = true;
            Ok(())
        }
    }

    struct FailAfterFirstWrite {
        bytes: Vec<u8>,
    }

    impl Write for FailAfterFirstWrite {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.is_empty() {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "injected challenge write failure",
                ))
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn artifact_guard_spans_actual_reply_write_and_flush() {
        let alive = Arc::new(AtomicBool::new(true));
        let prepared = Arc::new(AtomicBool::new(false));
        let reply = ControlReply::guarded(
            "OK 1 1 /private/artifact.png\n".to_string(),
            Some(ReplyRetention::new(WireProbe {
                alive: Arc::clone(&alive),
                prepared: Arc::clone(&prepared),
                fail_prepare: false,
            })),
        );
        let mut writer = GuardCheckingWriter {
            alive: Arc::clone(&alive),
            prepared,
            expect_alive: true,
            bytes: Vec::new(),
            flushed: false,
        };

        let mut pending = write_control_reply(&mut writer, reply)
            .expect("wire write succeeds")
            .expect("guarded reply returns its retention");

        assert!(writer.flushed);
        let wire = String::from_utf8(writer.bytes).unwrap();
        assert!(wire.starts_with("OK 1 1 /private/artifact.png\nACK-CHALLENGE "));
        let nonce = wire
            .lines()
            .nth(1)
            .and_then(|line| {
                line.strip_prefix(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX)
            })
            .unwrap();
        assert!(aterm_types::control_verbs::valid_artifact_ack_nonce(nonce));
        assert!(
            alive.load(Ordering::Acquire),
            "exact handles remain live after write and flush until client acknowledgement"
        );
        pending.retention.acknowledge_peer_anchor();
        drop(pending);
        assert!(
            !alive.load(Ordering::Acquire),
            "client acknowledgement releases the exact handles"
        );
    }

    #[test]
    fn partial_wire_failure_transfers_the_guard_to_quarantine() {
        let alive = Arc::new(AtomicBool::new(true));
        let prepared = Arc::new(AtomicBool::new(false));
        let reply = ControlReply::guarded(
            "OK 1 1 /private/artifact.png\n".to_string(),
            Some(ReplyRetention::new(WireProbe {
                alive: Arc::clone(&alive),
                prepared,
                fail_prepare: false,
            })),
        );
        let mut writer = FailAfterFirstWrite { bytes: Vec::new() };

        let error = match write_control_reply(&mut writer, reply) {
            Err(error) => error,
            Ok(_) => panic!("challenge write must fail after the complete ordinary body"),
        };
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        assert_eq!(writer.bytes, b"OK 1 1 /private/artifact.png\n");
        assert!(
            alive.load(Ordering::Acquire),
            "a full path may already be visible, so write failure must quarantine its guard"
        );
    }

    #[test]
    fn guarded_reply_releases_only_after_explicit_peer_ack() {
        let alive = Arc::new(AtomicBool::new(true));
        let prepared = Arc::new(AtomicBool::new(false));
        let (mut client, server) = CtlStream::pair().unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let server_alive = Arc::clone(&alive);
        let server_prepared = Arc::clone(&prepared);
        let worker = std::thread::spawn(move || {
            let reply = ControlReply::guarded(
                "OK 1 1 /private/artifact.png\n".to_string(),
                Some(ReplyRetention::new(WireProbe {
                    alive: server_alive,
                    prepared: server_prepared,
                    fail_prepare: false,
                })),
            );
            let mut writer = &server;
            let pending = write_control_reply(&mut writer, reply)
                .unwrap()
                .expect("guarded response");
            let mut reader = BufReader::new(&server);
            await_guarded_reply_close_with_quarantine(
                &server,
                &mut reader,
                pending,
                std::time::Duration::from_millis(40),
            )
        });

        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert_eq!(response, "OK 1 1 /private/artifact.png\n");
        let mut challenge = String::new();
        reader.read_line(&mut challenge).unwrap();
        let nonce = challenge
            .trim_end()
            .strip_prefix(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX)
            .unwrap();
        assert!(aterm_types::control_verbs::valid_artifact_ack_nonce(nonce));
        assert!(
            alive.load(Ordering::Acquire),
            "reading the response does not silently release its exact guard"
        );
        client
            .write_all(
                format!(
                    "{}{nonce}\n",
                    aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX
                )
                .as_bytes(),
            )
            .unwrap();
        client.flush().unwrap();
        assert_eq!(worker.join().unwrap(), ArtifactAckOutcome::PeerAcknowledged);
        assert!(
            !alive.load(Ordering::Acquire),
            "the exact ACK transition releases the guard"
        );
    }

    #[test]
    fn pre_pipelined_ack_guess_is_failed_and_quarantined() {
        let alive = Arc::new(AtomicBool::new(true));
        let prepared = Arc::new(AtomicBool::new(false));
        let (mut client, server) = CtlStream::pair().unwrap();
        // This guess exists before the server creates its nonce and therefore
        // cannot causally acknowledge the response it has not read.
        client.write_all(b"ACK artifact\n").unwrap();
        client.flush().unwrap();
        let server_alive = Arc::clone(&alive);
        let worker = std::thread::spawn(move || {
            let reply = ControlReply::guarded(
                "OK 1 1 /private/artifact.png\n".to_string(),
                Some(ReplyRetention::new(WireProbe {
                    alive: server_alive,
                    prepared,
                    fail_prepare: false,
                })),
            );
            let mut writer = &server;
            let pending = write_control_reply(&mut writer, reply)
                .unwrap()
                .expect("guarded response");
            let mut reader = BufReader::new(&server);
            await_guarded_reply_close_with_quarantine(
                &server,
                &mut reader,
                pending,
                std::time::Duration::from_secs(1),
            )
        });

        let mut reader = BufReader::new(&client);
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert_eq!(response, "OK 1 1 /private/artifact.png\n");
        let mut challenge = String::new();
        reader.read_line(&mut challenge).unwrap();
        assert!(challenge.starts_with(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX));
        assert_eq!(
            worker.join().unwrap(),
            ArtifactAckOutcome::AcknowledgementQuarantined
        );
        assert!(
            alive.load(Ordering::Acquire),
            "a pre-challenge guess must retain the exact guard in quarantine"
        );
    }

    #[test]
    fn eager_request_half_close_is_ack_failure_not_peer_ack() {
        let alive = Arc::new(AtomicBool::new(true));
        let prepared = Arc::new(AtomicBool::new(false));
        let (client, server) = CtlStream::pair().unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let server_alive = Arc::clone(&alive);
        let worker = std::thread::spawn(move || {
            let reply = ControlReply::guarded(
                "OK 1 1 /private/artifact.png\n".to_string(),
                Some(ReplyRetention::new(WireProbe {
                    alive: server_alive,
                    prepared,
                    fail_prepare: false,
                })),
            );
            let mut writer = &server;
            let pending = write_control_reply(&mut writer, reply)
                .unwrap()
                .expect("guarded response");
            let mut reader = BufReader::new(&server);
            await_guarded_reply_close_with_quarantine(
                &server,
                &mut reader,
                pending,
                std::time::Duration::from_millis(40),
            )
        });

        let mut reader = BufReader::new(&client);
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert_eq!(response, "OK 1 1 /private/artifact.png\n");
        let mut challenge = String::new();
        reader.read_line(&mut challenge).unwrap();
        assert!(challenge.starts_with(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX));
        assert_eq!(
            worker.join().unwrap(),
            ArtifactAckOutcome::AcknowledgementQuarantined,
            "request EOF is never accepted as proof the response was consumed"
        );
        assert!(
            alive.load(Ordering::Acquire),
            "an abandoned/invalid ACK path transfers the exact guard to quarantine"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while alive.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            !alive.load(Ordering::Acquire),
            "the quarantine reaper releases the guard only after its grace interval"
        );
    }

    #[test]
    fn wire_edge_identity_failure_replaces_ok_and_aborts_guard() {
        let alive = Arc::new(AtomicBool::new(true));
        let prepared = Arc::new(AtomicBool::new(false));
        let reply = ControlReply::guarded(
            "OK stale-path\n".to_string(),
            Some(ReplyRetention::new(WireProbe {
                alive: Arc::clone(&alive),
                prepared: Arc::clone(&prepared),
                fail_prepare: true,
            })),
        );
        let mut writer = GuardCheckingWriter {
            alive: Arc::clone(&alive),
            prepared,
            expect_alive: false,
            bytes: Vec::new(),
            flushed: false,
        };

        let retention = write_control_reply(&mut writer, reply).expect("path-free ERR is writable");

        let body = String::from_utf8(writer.bytes).unwrap();
        assert!(body.starts_with("ERR artifact reply validation failed:"));
        assert!(!body.contains("stale-path"));
        assert!(!alive.load(Ordering::Acquire));
        assert!(
            retention.is_none(),
            "a path-free validation error carries no stale guard"
        );
    }

    #[test]
    fn removed_bottom_hud_verbs_are_unadvertised_stable_tombstones() {
        assert_eq!(
            retired_bottom_hud_verb_error("widgets"),
            Some("ERR verb widgets was removed with the bottom HUD\n")
        );
        assert_eq!(
            retired_bottom_hud_verb_error("metric"),
            Some("ERR verb metric was removed with the bottom HUD\n")
        );
        assert_eq!(retired_bottom_hud_verb_error("metrics"), None);
        assert!(
            aterm_types::control_verbs::spec("widgets").is_none()
                && aterm_types::control_verbs::spec("metric").is_none(),
            "retired verbs must not reappear in help, completion, or authority catalogs"
        );
    }

    /// `cwd` must not let an OSC 7 path forge control-protocol reply lines. OSC 7
    /// percent-decodes its path, so `%0A` becomes a raw newline; pct_encoding the
    /// cwd keeps the reply to its single terminating newline.
    #[test]
    fn cwd_verb_sanitizes_embedded_newline() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term.lock()
            .unwrap()
            .process(b"\x1b]7;file://localhost/tmp/%0AOK%20forged\x07");
        let out = cmd_cwd(&term);
        assert_eq!(
            out.matches('\n').count(),
            1,
            "cwd reply must hold exactly the terminating newline: {out:?}"
        );
        assert!(
            !out.contains("\nOK"),
            "cwd must not forge a second reply line: {out:?}"
        );
        assert!(out.contains("%0A"), "newline must be pct-encoded: {out:?}");
    }

    /// `cell` must pct-encode the OSC 8 hyperlink so a space in the URL cannot
    /// break the space-delimited cell line into spurious fields.
    #[test]
    fn cell_verb_pct_encodes_hyperlink_space() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term.lock()
            .unwrap()
            .process(b"\x1b]8;;https://example.com/a b\x1b\\X\x1b]8;;\x1b\\");
        let out = cmd_cell(&term, "0 0");
        assert!(
            out.contains("link=https://example.com/a%20b"),
            "hyperlink space must be pct-encoded: {out}"
        );
        assert!(
            !out.contains("/a b"),
            "raw space leaked into cell line: {out}"
        );
    }

    // `cmd_feed`/`cmd_send` are drawn only by the unix-gated pipe-backed tests.
    #[cfg(unix)]
    use super::control_input::{cmd_feed, cmd_send};
    use super::control_input::{parse_resize, parse_tab, paste_text, take_mods};
    use super::control_media::{
        MAX_IMAGE_PAYLOAD_BYTES, cmd_image_read, image_payload, image_read_line,
    };
    use super::control_query::{
        AbsRow, abs_row_text, cmd_cell, cmd_colors, cmd_cursor_json, cmd_cwd, cmd_line, cmd_modes,
        cmd_screen_styled_json, cmd_search, cmd_text, cmd_text_json, serialize_dims,
        serialize_dims_json, styled_image_json,
    };
    // `cmd_select`/`cmd_selection` are drawn only by the unix-gated pipe-backed tests.
    use super::control_selection::{cmd_blocks, cmd_blocks_json, cmd_blocktext, cmd_wait};
    #[cfg(unix)]
    use super::control_selection::{cmd_select, cmd_selection};
    use super::control_session::{
        TurnIo, cmd_cast, cmd_connect_in, cmd_disconnect_in, cmd_edges, cmd_edges_json,
        cmd_family, cmd_flows, cmd_grant, cmd_lease, cmd_meta, cmd_ready, cmd_revoke,
        cmd_sessions, cmd_timeline, cmd_turn, cmd_who, cmd_whoami, raise_target,
    };
    use super::*;
    use crate::TabAction;
    use crate::input::InputEvent;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_session::sink::SinkWriter;
    // The pipe-backed byte-drain assertions are Unix-only (the portable tests
    // that read do so through fn-local imports).
    #[cfg(unix)]
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::io::FromRawFd;

    #[test]
    fn operator_proposal_schema_is_closed_and_human_approvals_are_rejected() {
        let valid = br#"{
            "schema":1,
            "event_id":7,
            "claim_token":"0000000000000000000000000000000000000000000000000000000000000000",
            "sid":"s-test",
            "generation":{"lifecycle_epoch":3,"alternate_screen":false,"content_seq":9,"fingerprint":"0000000000000000000000000000000000000000000000000000000000000000"},
            "action":{"kind":"turn","text":"Continue and summarize the result."},
            "expectation":{"kind":"busy_then_attention","deadline_ms":300000}
        }"#;
        assert!(decode_operator_proposal(valid).is_ok());

        let approval = String::from_utf8(valid.to_vec())
            .unwrap()
            .replace("Continue and summarize the result.", "yes");
        assert!(
            decode_operator_proposal(approval.as_bytes())
                .err()
                .unwrap()
                .contains("human-only")
        );
        let unknown = String::from_utf8(valid.to_vec())
            .unwrap()
            .replace("\"schema\":1,", "\"schema\":1,\"extra\":true,");
        assert!(decode_operator_proposal(unknown.as_bytes()).is_err());

        for disallowed in [r"line\nbreak", r"line\tbreak", " leading", "trailing "] {
            let proposal = String::from_utf8(valid.to_vec())
                .unwrap()
                .replace("Continue and summarize the result.", disallowed);
            assert!(
                decode_operator_proposal(proposal.as_bytes()).is_err(),
                "operator text {disallowed:?} must fail before any egress"
            );
        }

        // JSON `\\n` decodes to a literal backslash+n pair. Unlike the interactive
        // `paste` verb, the structured operator path preserves those exact bytes.
        let literal = r"Continue literally: \\n";
        let proposal = String::from_utf8(valid.to_vec())
            .unwrap()
            .replace("Continue and summarize the result.", literal);
        let decoded = decode_operator_proposal(proposal.as_bytes()).unwrap();
        assert_eq!(decoded.text, r"Continue literally: \n");
        assert_eq!(
            operator_paste_event(&decoded.text),
            InputEvent::Paste(r"Continue literally: \n".to_string())
        );
        assert_ne!(
            control_input::paste_text(&decoded.text),
            decoded.text,
            "regression control: interactive paste would expand the literal suffix"
        );
    }

    #[test]
    fn operator_proposal_rejections_close_without_reading_body() {
        struct NoBodyRead;

        impl std::io::Read for NoBodyRead {
            fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
                panic!("operator proposal rejection attempted to read its body")
            }
        }

        impl std::io::BufRead for NoBodyRead {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                panic!("operator proposal rejection attempted to buffer its body")
            }

            fn consume(&mut self, _amount: usize) {}
        }

        let (notify_tx, _notify_rx) = std::sync::mpsc::sync_channel(1);
        let operator = crate::operator_host::ControlHandle::new(
            "test-proposal-pre-body".to_string(),
            notify_tx,
        );
        let store = crate::session_store::new_store();
        let subscribers = crate::subscribe::new_registry();

        let assert_rejected = |line: &str,
                               scope: Scope,
                               operator: Option<&crate::operator_host::ControlHandle>,
                               expected: &str| {
            let mut reader = NoBodyRead;
            let mut output = Vec::new();
            assert!(
                !run_operator_proposal_bin(
                    line,
                    &mut reader,
                    scope,
                    operator,
                    &store,
                    &subscribers,
                    &mut output,
                ),
                "an unread announced body makes the connection unrecoverable"
            );
            assert_eq!(String::from_utf8(output).unwrap(), expected);
        };

        assert_rejected(
            "operator-propose-bin 4",
            Scope::Edge(EdgeToken::generate()),
            Some(&operator),
            "ERR denied\n",
        );
        assert_rejected(
            "@. operator-propose-bin 4",
            Scope::Owner,
            Some(&operator),
            "ERR denied\n",
        );
        assert_rejected(
            "operator-propose-bin 4",
            Scope::Owner,
            None,
            "ERR operator unavailable\n",
        );
        assert_rejected(
            &format!("operator-propose-bin {}", MAX_OPERATOR_PROPOSAL + 1),
            Scope::Owner,
            Some(&operator),
            "ERR operator proposal too large\n",
        );

        operator
            .inject_fleet_fault_for_test(aterm_agent::operator::FleetFaultReason::ObserverPanicked);
        let response = run_operator_proposal(b"not even JSON", &operator, &store, &subscribers);
        assert!(response.contains("fleet faulted"), "{response}");
        assert!(response.contains("observer-panicked"), "{response}");
        assert!(
            !response.contains("invalid proposal JSON"),
            "fleet fault must reject before proposal parsing or any action"
        );
    }

    #[test]
    #[cfg(unix)]
    fn guarded_turn_redacts_operator_text_while_generic_turn_keeps_text() {
        let store = crate::session_store::new_store();
        let (operator_target, _operator_rx) = pipe_session(68);
        store.write().unwrap().register(operator_target.clone());
        let subscribers = subscribe::new_registry();
        let secret = "credential-like proposal body";
        let paste = |text: &str| {
            assert_eq!(text, secret);
            true
        };
        let press = |_: &str| panic!("submit=none must not press");
        let mut preflight = || Ok(());
        let mut pre_submit = || Ok(());
        let response = control_session::cmd_turn_guarded(
            &operator_target.term,
            &store,
            operator_target.local_id,
            &format!("idle=1 timeout=1000 submit=none -- {secret}"),
            &subscribers,
            &operator_target.ctx,
            &control_session::TurnIo {
                paste: &paste,
                press: &press,
            },
            &mut preflight,
            &mut pre_submit,
            Some("[operator action]"),
        );
        assert!(response.starts_with("OK "), "{response}");
        let operator_history = control_session::cmd_history(&operator_target.ctx, "");
        assert!(!operator_history.contains(secret));
        assert!(
            operator_target
                .ctx
                .turns
                .lock()
                .unwrap()
                .since(None)
                .all(|record| record.text == "[operator action]")
        );

        let (generic_target, _generic_rx) = pipe_session(69);
        store.write().unwrap().register(generic_target.clone());
        let generic = control_session::cmd_turn(
            &generic_target.term,
            &store,
            generic_target.local_id,
            &format!("idle=1 timeout=1000 submit=none -- {secret}"),
            &subscribers,
            &generic_target.ctx,
            &control_session::TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert!(generic.starts_with("OK "), "{generic}");
        assert_eq!(
            generic_target
                .ctx
                .turns
                .lock()
                .unwrap()
                .since(None)
                .last()
                .map(|record| record.text.as_str()),
            Some(secret),
            "ordinary turn ledger behavior changed"
        );
    }

    #[test]
    #[cfg(unix)]
    fn foreign_input_between_operator_paste_and_submit_emits_no_enter() {
        use std::collections::BTreeMap;
        use std::io::Read as _;

        use aterm_agent::operator::{
            AttentionCondition, DurableQueue, EventGeneration, EventStatus, NewEvent, QueueConfig,
        };
        use aterm_spec::derive::operator_wal_actuator_model;
        use aterm_spec::verify;

        let store = crate::session_store::new_store();
        let (target, mut reader) = pipe_session(70);
        store.write().unwrap().register(target.clone());
        aterm_pty::set_nonblocking(target.master, true).expect("nonblocking test master");
        target.ctx.sink.note_master_nonblocking(true);

        let unique = format!(
            "aterm-operator-interjection-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let directory = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&directory);
        let queue = DurableQueue::open(&directory, 1, QueueConfig::default()).unwrap();
        queue.manage_sid("s-interjection").unwrap();
        let fingerprint: [u8; 32] = Sha256::digest(b"ready");
        queue
            .enqueue(NewEvent::new(
                "s-interjection",
                EventGeneration::new(1, false, 1, fingerprint),
                AttentionCondition::Ready,
                "ready",
            ))
            .unwrap();
        let claim = queue.claim().unwrap().unwrap();

        let epoch = std::cell::Cell::new(target.ctx.sink.input_epoch());
        let initial_terminal_generation = {
            let terminal = term_lock(&target.term);
            let evidence = crate::operator_host::terminal_evidence(&terminal);
            EventGeneration::new(
                0,
                terminal.is_alternate_screen(),
                terminal.content_seq(),
                Sha256::digest(evidence.as_bytes()),
            )
        };
        let paste = |text: &str| {
            let delivered = operator_input_if_epoch(
                &target.term,
                &target.ctx,
                Some(InputEvent::Paste(text.to_string())),
                epoch.get(),
                OperatorTerminalFence::Exact(initial_terminal_generation),
            );
            let Delivery::FullAt { epoch: advanced } = delivered else {
                panic!("operator paste failed unexpectedly: {delivered:?}");
            };
            epoch.set(advanced);
            // Deterministic human/raw-controller interjection after paste and
            // before cmd_turn_guarded reaches its submit hook.
            assert_eq!(target.ctx.sink.write_frame(b"human").unwrap(), 5);
            true
        };
        let presses = std::sync::atomic::AtomicUsize::new(0);
        let press = |_: &str| {
            presses.fetch_add(1, Ordering::SeqCst);
            false
        };
        let validate_epoch = || {
            if target.ctx.sink.input_epoch() == epoch.get() {
                Ok(())
            } else {
                Err("target input interjected".to_string())
            }
        };
        let response = operator_action_transaction(
            &queue,
            claim.event.id,
            &claim.token,
            &"22".repeat(32),
            validate_epoch,
            |preflight| {
                let mut pre_submit = || {
                    if target.ctx.sink.input_epoch() == epoch.get() {
                        Ok(())
                    } else {
                        Err("target input interjected".to_string())
                    }
                };
                control_session::cmd_turn_guarded(
                    &target.term,
                    &store,
                    target.local_id,
                    "idle=1 timeout=1000 presses=1 -- operator",
                    &subscribe::new_registry(),
                    &target.ctx,
                    &control_session::TurnIo {
                        paste: &paste,
                        press: &press,
                    },
                    preflight,
                    &mut pre_submit,
                    Some("[operator action]"),
                )
            },
        );
        assert!(
            response.starts_with("ERR operator action in doubt"),
            "{response}"
        );
        assert_eq!(presses.load(Ordering::SeqCst), 0, "Enter was attempted");
        assert!(matches!(
            queue.snapshot(claim.event.id).unwrap().status,
            EventStatus::InDoubt { .. }
        ));

        // Tier-1 bind the real conditional sink/write + durable transaction to
        // the extended actuator model. The forged submit across the same foreign
        // epoch is the non-vacuous negative control.
        let project = |before: &BTreeMap<&'static str, i64>, changes: &[(&'static str, i64)]| {
            let mut after = before.clone();
            for (name, value) in changes {
                after.insert(*name, *value);
            }
            after
        };
        let assert_step = |before: &BTreeMap<&'static str, i64>,
                           after: &BTreeMap<&'static str, i64>,
                           action: &str| {
            let model = operator_wal_actuator_model();
            let (accepted, diagnostics) = verify::validate_transition_tiered(
                &model,
                &[("Buggy", 0)],
                before,
                after,
                Some(action),
                "operator real interjection fence",
            );
            assert!(accepted, "{action} rejected\n{diagnostics}");
        };
        let model = operator_wal_actuator_model();
        let initial = model.init_state();
        let intent = project(&initial, &[("phase", 1), ("intent_durable", 1)]);
        let pasted = project(
            &intent,
            &[
                ("phase", 2),
                ("mutations", 1),
                ("input_epoch", 1),
                ("expected_epoch", 1),
            ],
        );
        let interjected = project(&pasted, &[("input_epoch", 2), ("interjected", 1)]);
        let rejected = project(&interjected, &[("phase", 3), ("in_doubt", 1)]);
        assert_step(&initial, &intent, "PersistIntent");
        assert_step(&intent, &pasted, "MutateOnce");
        assert_step(&pasted, &interjected, "ForeignInput");
        assert_step(&interjected, &rejected, "RejectInterjectedSubmit");
        let forged_submit = project(&interjected, &[("submit_writes", 1)]);
        let (accepted, diagnostics) = verify::validate_transition_tiered(
            &model,
            &[("Buggy", 0)],
            &interjected,
            &forged_submit,
            Some("RejectInterjectedSubmit"),
            "operator interjection submit negative control",
        );
        assert!(
            !accepted,
            "foreign-input submit negative control was admitted\n{diagnostics}"
        );

        let mut landed = [0_u8; 13];
        reader.read_exact(&mut landed).expect("paste + human input");
        assert_eq!(&landed, b"operatorhuman");
        assert!(
            drain_pipe(&reader).is_empty(),
            "guarded Enter must never arrive late"
        );
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn input_change_during_intent_fsync_stops_before_paste() {
        use aterm_agent::operator::{
            AttentionCondition, DurableQueue, EventGeneration, EventStatus, NewEvent, QueueConfig,
        };

        let unique = format!(
            "aterm-operator-post-wal-fence-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let directory = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&directory);
        let queue = DurableQueue::open(&directory, 1, QueueConfig::default()).unwrap();
        queue.manage_sid("s-post-wal").unwrap();
        let fingerprint: [u8; 32] = Sha256::digest(b"ready");
        queue
            .enqueue(NewEvent::new(
                "s-post-wal",
                EventGeneration::new(1, false, 1, fingerprint),
                AttentionCondition::Ready,
                "ready",
            ))
            .unwrap();
        let claim = queue.claim().unwrap().unwrap();

        let validations = std::sync::atomic::AtomicUsize::new(0);
        let pastes = std::sync::atomic::AtomicUsize::new(0);
        let response = operator_action_transaction(
            &queue,
            claim.event.id,
            &claim.token,
            &"33".repeat(32),
            || {
                let call = validations.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    Ok(())
                } else {
                    // Deterministic stand-in for an epoch/state change while
                    // begin_action's durable append + fsync was in flight.
                    Err("target input interjected during intent persistence".to_string())
                }
            },
            |preflight| match preflight() {
                Ok(()) => {
                    pastes.fetch_add(1, Ordering::SeqCst);
                    "OK 0 turn submitted=1 status=settled\n".to_string()
                }
                Err(error) => format!("ERR {error}\n"),
            },
        );
        assert!(
            response.starts_with("ERR operator action in doubt"),
            "{response}"
        );
        assert_eq!(validations.load(Ordering::SeqCst), 2);
        assert_eq!(pastes.load(Ordering::SeqCst), 0, "paste crossed WAL fsync");
        assert!(matches!(
            queue.snapshot(claim.event.id).unwrap().status,
            EventStatus::InDoubt { .. }
        ));
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    #[cfg(unix)]
    fn echoed_operator_paste_captures_post_paste_generation_and_submits_once() {
        use std::io::Read as _;

        use aterm_agent::operator::{
            AttentionCondition, DurableQueue, EventGeneration, EventStatus, NewEvent, QueueConfig,
            Resolution,
        };

        let store = crate::session_store::new_store();
        let (target, mut reader) = pipe_session(73);
        store.write().unwrap().register(target.clone());
        aterm_pty::set_nonblocking(target.master, true).expect("nonblocking test master");
        target.ctx.sink.note_master_nonblocking(true);
        term_lock(&target.term).process(b"ready> ");

        let initial_generation = {
            let terminal = term_lock(&target.term);
            let evidence = crate::operator_host::terminal_evidence(&terminal);
            EventGeneration::new(
                0,
                terminal.is_alternate_screen(),
                terminal.content_seq(),
                Sha256::digest(evidence.as_bytes()),
            )
        };
        let initial_evidence = {
            let terminal = term_lock(&target.term);
            crate::operator_host::terminal_evidence(&terminal)
        };
        let unique = format!(
            "aterm-operator-echo-fence-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let directory = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&directory);
        let queue = DurableQueue::open(&directory, 1, QueueConfig::default()).unwrap();
        queue.manage_sid("s-echo").unwrap();
        queue
            .enqueue(NewEvent::new(
                "s-echo",
                initial_generation,
                AttentionCondition::Ready,
                initial_evidence,
            ))
            .unwrap();
        let claim = queue.claim().unwrap().unwrap();
        let proposal = OperatorProposal {
            event_id: claim.event.id,
            token: claim.token.clone(),
            sid: "s-echo".to_string(),
            generation: initial_generation,
            text: "continue work".to_string(),
            action_hash: "44".repeat(32),
        };

        let epoch = std::cell::Cell::new(target.ctx.sink.input_epoch());
        let submit_generation = std::cell::Cell::new(None);
        let paste = |text: &str| {
            let delivery = operator_input_if_epoch(
                &target.term,
                &target.ctx,
                Some(InputEvent::Paste(text.to_string())),
                epoch.get(),
                OperatorTerminalFence::Exact(proposal.generation),
            );
            let Delivery::FullAt { epoch: advanced } = delivery else {
                panic!("paste did not land: {delivery:?}");
            };
            epoch.set(advanced);
            // Realistic editor echo: this is OUTPUT, so it advances the terminal
            // generation without touching the attempted-input epoch.
            term_lock(&target.term).process(text.as_bytes());
            true
        };
        let presses = std::sync::atomic::AtomicUsize::new(0);
        let press = |name: &str| {
            assert_eq!(name, "enter");
            let generation = submit_generation.get().expect("post-paste capture");
            let delivery = operator_input_if_epoch(
                &target.term,
                &target.ctx,
                parse_key(name),
                epoch.get(),
                OperatorTerminalFence::Exact(generation),
            );
            let Delivery::FullAt { epoch: advanced } = delivery else {
                panic!("submit did not land: {delivery:?}");
            };
            epoch.set(advanced);
            presses.fetch_add(1, Ordering::SeqCst);
            term_lock(&target.term).process(b"\r\nresponse\r\n");
            true
        };
        let validate = || {
            validate_operator_action_state(&queue, &proposal, false)?;
            if target.ctx.sink.input_epoch() != epoch.get() {
                return Err("target input interjected".to_string());
            }
            let terminal = term_lock(&target.term);
            let evidence = crate::operator_host::terminal_evidence(&terminal);
            let fingerprint: [u8; 32] = Sha256::digest(evidence.as_bytes());
            if terminal.content_seq() != proposal.generation.content_seq
                || fingerprint != proposal.generation.fingerprint
            {
                return Err("target changed before paste".to_string());
            }
            Ok(())
        };
        let response = operator_action_transaction(
            &queue,
            claim.event.id,
            &claim.token,
            &proposal.action_hash,
            validate,
            |preflight| {
                let mut pre_submit = || {
                    validate_operator_action_state(&queue, &proposal, true)?;
                    if target.ctx.sink.input_epoch() != epoch.get() {
                        return Err("target input interjected".to_string());
                    }
                    let terminal = term_lock(&target.term);
                    let evidence = crate::operator_host::terminal_evidence(&terminal);
                    if crate::operator_host::looks_like_approval(&evidence) {
                        return Err("approval-shaped screens are human-only".to_string());
                    }
                    let generation = EventGeneration::new(
                        proposal.generation.lifecycle_epoch,
                        terminal.is_alternate_screen(),
                        terminal.content_seq(),
                        Sha256::digest(evidence.as_bytes()),
                    );
                    assert_ne!(
                        generation, proposal.generation,
                        "echo must create a distinct post-paste generation"
                    );
                    submit_generation.set(Some(generation));
                    Ok(())
                };
                control_session::cmd_turn_guarded(
                    &target.term,
                    &store,
                    target.local_id,
                    "idle=1 timeout=1000 presses=1 submit_verify=seq -- continue work",
                    &subscribe::new_registry(),
                    &target.ctx,
                    &control_session::TurnIo {
                        paste: &paste,
                        press: &press,
                    },
                    preflight,
                    &mut pre_submit,
                    Some("[operator action]"),
                )
            },
        );
        assert!(response.starts_with("OK "), "{response}");
        assert!(response.contains("submitted=1"), "{response}");
        assert_eq!(presses.load(Ordering::SeqCst), 1);
        assert!(matches!(
            queue.snapshot(claim.event.id).unwrap().status,
            EventStatus::Resolved {
                resolution: Resolution::Acted,
                ..
            }
        ));
        let mut landed = vec![0_u8; proposal.text.len() + 1];
        reader.read_exact(&mut landed).expect("paste plus Enter");
        let mut expected = proposal.text.as_bytes().to_vec();
        expected.push(b'\r');
        assert_eq!(landed, expected);
        assert!(drain_pipe(&reader).is_empty(), "more than one Enter landed");
        drop(queue);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    #[cfg(unix)]
    fn output_change_after_submit_capture_is_rejected_under_terminal_lock() {
        use aterm_agent::operator::EventGeneration;

        let (target, reader) = pipe_session(74);
        aterm_pty::set_nonblocking(target.master, true).expect("nonblocking test master");
        target.ctx.sink.note_master_nonblocking(true);
        term_lock(&target.term).process(b"safe post-paste draft");
        let epoch = target.ctx.sink.input_epoch();
        let captured = {
            let terminal = term_lock(&target.term);
            let evidence = crate::operator_host::terminal_evidence(&terminal);
            EventGeneration::new(
                0,
                terminal.is_alternate_screen(),
                terminal.content_seq(),
                Sha256::digest(evidence.as_bytes()),
            )
        };

        // Output arrives after pre-submit captured the safe generation. The final
        // compare and key encoding share the terminal lock, so Enter contributes
        // zero bytes instead of acting on the new approval presentation.
        term_lock(&target.term).process(b"\r\nAllow this command? [y/N]");
        assert_eq!(
            operator_input_if_epoch(
                &target.term,
                &target.ctx,
                parse_key("enter"),
                epoch,
                OperatorTerminalFence::Exact(captured),
            ),
            Delivery::ConflictZero
        );
        assert!(drain_pipe(&reader).is_empty(), "Enter reached the PTY");
    }

    #[test]
    #[cfg(unix)]
    fn contended_terminal_at_final_egress_refuses_immediately_without_sink_write() {
        use aterm_agent::operator::EventGeneration;

        let (target, reader) = pipe_session(75);
        aterm_pty::set_nonblocking(target.master, true).expect("nonblocking test master");
        target.ctx.sink.note_master_nonblocking(true);
        let epoch = target.ctx.sink.input_epoch();
        let terminal_guard = term_lock(&target.term);
        let evidence = crate::operator_host::terminal_evidence(&terminal_guard);
        let generation = EventGeneration::new(
            0,
            terminal_guard.is_alternate_screen(),
            terminal_guard.content_seq(),
            Sha256::digest(evidence.as_bytes()),
        );

        let started = std::time::Instant::now();
        assert_eq!(
            operator_input_if_epoch(
                &target.term,
                &target.ctx,
                parse_key("enter"),
                epoch,
                OperatorTerminalFence::Exact(generation),
            ),
            Delivery::BusyZero
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "final egress waited on the terminal lock"
        );
        assert_eq!(target.ctx.sink.input_epoch(), epoch);
        drop(terminal_guard);
        assert!(drain_pipe(&reader).is_empty(), "Enter reached the PTY");
    }

    #[test]
    fn operator_action_transaction_is_wal_first_exactly_once_and_never_retries() {
        use std::collections::BTreeMap;
        use std::panic::{AssertUnwindSafe, catch_unwind};

        use aterm_agent::operator::{
            AttentionCondition, DurableQueue, EventGeneration, EventStatus, NewEvent, QueueConfig,
            Resolution,
        };
        use aterm_spec::derive::{Model, operator_wal_actuator_model};
        use aterm_spec::verify;

        type State = BTreeMap<&'static str, i64>;

        fn state_after(before: &State, changes: &[(&'static str, i64)]) -> State {
            let mut after = before.clone();
            for (name, value) in changes {
                after.insert(name, *value);
            }
            after
        }

        fn assert_transition(
            model: &Model,
            before: &State,
            after: &State,
            action: &str,
            label: &str,
        ) {
            let (accepted, diagnostics) = verify::validate_transition_tiered(
                model,
                &[("Buggy", 0)],
                before,
                after,
                Some(action),
                label,
            );
            assert!(
                accepted,
                "shipping {label} transition must be admitted as {action}\n{diagnostics}"
            );
        }

        let unique = format!(
            "aterm-operator-transaction-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let directory = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&directory);
        let queue = DurableQueue::open(&directory, 1, QueueConfig::default()).unwrap();
        queue.manage_sid("s-test").unwrap();

        let claim = |evidence: &str, seq: u64| {
            let fingerprint: [u8; 32] = Sha256::digest(evidence.as_bytes());
            queue
                .enqueue(NewEvent::new(
                    "s-test",
                    EventGeneration::new(1, false, seq, fingerprint),
                    AttentionCondition::Ready,
                    evidence,
                ))
                .unwrap();
            queue.claim().unwrap().unwrap()
        };

        let first = claim("ready one", 1);
        let action_hash = "11".repeat(32);
        let mutations = std::sync::atomic::AtomicUsize::new(0);
        let model = operator_wal_actuator_model();
        let initial = model.init_state();
        let intent = state_after(&initial, &[("phase", 1), ("intent_durable", 1)]);
        let mutated = state_after(
            &intent,
            &[
                ("phase", 2),
                ("mutations", 1),
                ("input_epoch", 1),
                ("expected_epoch", 1),
            ],
        );
        let submitted = state_after(&mutated, &[("submit_writes", 1)]);
        let response = operator_action_transaction(
            &queue,
            first.event.id,
            &first.token,
            &action_hash,
            || Ok(()),
            |preflight| {
                preflight().unwrap();
                assert!(matches!(
                    queue.snapshot(first.event.id).unwrap().status,
                    EventStatus::ActionInFlight { .. }
                ));
                assert_transition(
                    &model,
                    &initial,
                    &intent,
                    "PersistIntent",
                    "operator actuator durable preflight",
                );
                mutations.fetch_add(1, Ordering::SeqCst);
                assert_transition(
                    &model,
                    &intent,
                    &mutated,
                    "MutateOnce",
                    "operator actuator terminal input",
                );
                assert_transition(
                    &model,
                    &mutated,
                    &submitted,
                    "GuardedSubmit",
                    "operator actuator epoch-guarded submit",
                );
                "OK 1 turn submitted=1 status=settled seq=1 id=1 dur_ms=1 hash=abc\nsecret settled screen row\n".to_string()
            },
        );
        assert!(response.starts_with("OK "));
        assert!(response.contains("outcome=acted"), "{response}");
        assert!(
            !response.contains("secret settled screen row"),
            "operator reply leaked cmd_turn screen: {response}"
        );
        assert_eq!(mutations.load(Ordering::SeqCst), 1);
        assert!(matches!(
            queue.snapshot(first.event.id).unwrap().status,
            EventStatus::Resolved {
                resolution: Resolution::Acted,
                ..
            }
        ));
        let result = state_after(
            &submitted,
            &[("phase", 4), ("result_durable", 1), ("resolved", 1)],
        );
        assert_transition(
            &model,
            &submitted,
            &result,
            "PersistResult",
            "operator actuator durable result and resolution",
        );

        let second = claim("ready two", 2);
        let ambiguous_intent = state_after(&initial, &[("phase", 1), ("intent_durable", 1)]);
        let ambiguous_mutation = state_after(
            &ambiguous_intent,
            &[
                ("phase", 2),
                ("mutations", 1),
                ("input_epoch", 1),
                ("expected_epoch", 1),
            ],
        );
        let response = operator_action_transaction(
            &queue,
            second.event.id,
            &second.token,
            &action_hash,
            || Ok(()),
            |preflight| {
                preflight().unwrap();
                assert!(matches!(
                    queue.snapshot(second.event.id).unwrap().status,
                    EventStatus::ActionInFlight { .. }
                ));
                assert_transition(
                    &model,
                    &initial,
                    &ambiguous_intent,
                    "PersistIntent",
                    "ambiguous actuator durable preflight",
                );
                mutations.fetch_add(1, Ordering::SeqCst);
                assert_transition(
                    &model,
                    &ambiguous_intent,
                    &ambiguous_mutation,
                    "MutateOnce",
                    "ambiguous actuator terminal input",
                );
                "ERR submit verification timeout\nsecret-screen-row\n".to_string()
            },
        );
        assert!(response.starts_with("ERR operator action in doubt"));
        assert_eq!(
            mutations.load(Ordering::SeqCst),
            2,
            "ambiguous action is not retried"
        );
        assert!(matches!(
            queue.snapshot(second.event.id).unwrap().status,
            EventStatus::InDoubt { ref reason, .. }
                if !reason.contains("secret-screen-row")
        ));
        let ambiguous = state_after(&ambiguous_mutation, &[("phase", 3), ("in_doubt", 1)]);
        assert_transition(
            &model,
            &ambiguous_mutation,
            &ambiguous,
            "CrashAfterMutation",
            "ambiguous actuator outcome",
        );

        drop(queue);

        // Crash after the shipping helper has durably written intent and called
        // its executor, but before it can append a result. Takeover must expose
        // InDoubt, and re-entering the same transaction must fail preflight
        // before the mocked terminal input seam is reached again.
        let crash_directory = directory.join("crash");
        let crash_queue = DurableQueue::open(&crash_directory, 1, QueueConfig::default()).unwrap();
        crash_queue.manage_sid("s-test").unwrap();
        let crash_evidence = "ready crash";
        let crash_fingerprint: [u8; 32] = Sha256::digest(crash_evidence.as_bytes());
        crash_queue
            .enqueue(NewEvent::new(
                "s-test",
                EventGeneration::new(1, false, 3, crash_fingerprint),
                AttentionCondition::Ready,
                crash_evidence,
            ))
            .unwrap();
        let crash_claim = crash_queue.claim().unwrap().unwrap();
        let crash_mutations = std::sync::atomic::AtomicUsize::new(0);
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            operator_action_transaction(
                &crash_queue,
                crash_claim.event.id,
                &crash_claim.token,
                &action_hash,
                || Ok(()),
                |preflight| {
                    preflight().unwrap();
                    assert!(matches!(
                        crash_queue.snapshot(crash_claim.event.id).unwrap().status,
                        EventStatus::ActionInFlight { .. }
                    ));
                    crash_mutations.fetch_add(1, Ordering::SeqCst);
                    panic!("simulated process loss after terminal input")
                },
            )
        }));
        assert!(panicked.is_err());
        assert_eq!(crash_mutations.load(Ordering::SeqCst), 1);
        drop(crash_queue);

        let recovered = DurableQueue::open(&crash_directory, 2, QueueConfig::default()).unwrap();
        assert!(matches!(
            recovered.snapshot(crash_claim.event.id).unwrap().status,
            EventStatus::InDoubt { .. }
        ));
        let crash_intent = state_after(&initial, &[("phase", 1), ("intent_durable", 1)]);
        let crash_mutated = state_after(
            &crash_intent,
            &[
                ("phase", 2),
                ("mutations", 1),
                ("input_epoch", 1),
                ("expected_epoch", 1),
            ],
        );
        let crash_in_doubt = state_after(&crash_mutated, &[("phase", 3), ("in_doubt", 1)]);
        assert_transition(
            &model,
            &crash_mutated,
            &crash_in_doubt,
            "CrashAfterMutation",
            "crashed actuator takeover",
        );

        let retry = operator_action_transaction(
            &recovered,
            crash_claim.event.id,
            &crash_claim.token,
            &action_hash,
            || Ok(()),
            |preflight| match preflight() {
                Ok(()) => {
                    crash_mutations.fetch_add(1, Ordering::SeqCst);
                    "OK 0 turn submitted=1 status=settled\n".to_string()
                }
                Err(error) => format!("ERR {error}\n"),
            },
        );
        assert!(retry.starts_with("ERR "));
        assert_eq!(
            crash_mutations.load(Ordering::SeqCst),
            1,
            "takeover must not call terminal input again"
        );
        assert_transition(
            &model,
            &crash_in_doubt,
            &crash_in_doubt,
            "ReplayInDoubt",
            "recovered actuator replay refusal",
        );

        // NEGATIVE CONTROL: this is exactly the Buggy=1 duplicate-input
        // successor. Healthy model semantics must reject it.
        let forged_replay = state_after(&crash_in_doubt, &[("mutations", 2), ("replayed", 1)]);
        let (accepted, diagnostics) = verify::validate_transition_tiered(
            &model,
            &[("Buggy", 0)],
            &crash_in_doubt,
            &forged_replay,
            Some("ReplayInDoubt"),
            "operator actuator duplicate-input negative control",
        );
        assert!(
            !accepted,
            "duplicate-input negative control was accepted; binding is vacuous\n{diagnostics}"
        );

        drop(recovered);
        let _ = std::fs::remove_dir_all(directory);
    }

    /// The `dial <name>` relay verb is OWNER-only and takes exactly one connection
    /// name; both are rejected with a friendly `ERR` BEFORE any dial is attempted,
    /// and a non-`dial <name>` line passes straight through (handled elsewhere).
    #[test]
    fn dial_verb_gate_rejects_non_owner_and_malformed_names() {
        let reply = |line: &str, scope: Scope| -> String {
            let (client, server) = CtlStream::pair().unwrap();
            let mut reader = BufReader::new(server.try_clone().unwrap());
            assert!(try_net_dial(line, scope, &server, &mut reader));
            let mut resp = String::new();
            BufReader::new(client).read_line(&mut resp).unwrap();
            resp
        };
        // An edge-scoped connection cannot dial out (owner authority required).
        let edge = Scope::Edge(aterm_session::EdgeToken::generate());
        assert!(
            reply("dial work", edge).contains("owner"),
            "an edge cannot dial out"
        );
        // An EMPTY name is rejected (the name is required).
        assert!(
            reply("dial   ", Scope::Owner).contains("connection name"),
            "an empty name is rejected"
        );
        // `dial <name> <verb...>` is the one-shot remote-verb form: the name+verb
        // is ACCEPTED (no grammar rejection) and attempts a relay, which fails here
        // because no saved connection is named — an `ERR dial ...`, not a usage error.
        let with_verb = reply("dial work text", Scope::Owner);
        assert!(
            with_verb.contains("dial") && !with_verb.contains("connection name"),
            "name+verb is a dial attempt, not a usage rejection: {with_verb:?}"
        );
        // A non-`dial <name>` line is NOT a relay verb (false => fall through).
        let (_c, s) = CtlStream::pair().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        assert!(!try_net_dial("screen", Scope::Owner, &s, &mut r));
        assert!(
            !try_net_dial("dial-list", Scope::Owner, &s, &mut r),
            "dial-list is not a relay verb"
        );
    }

    /// The serve loop's PRE-AUTH read deadline relies on a timed-out read
    /// surfacing as an error: `read_request_line` on a stream with a read
    /// timeout must yield `None` (drop the connection) when the peer stays
    /// silent — never park the thread past the deadline.
    #[test]
    fn read_request_line_returns_none_on_read_timeout() {
        let (client, server) = CtlStream::pair().unwrap();
        server
            .set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .unwrap();
        let mut reader = BufReader::new(server);
        // The client sends nothing (a silent pre-auth peer).
        let start = std::time::Instant::now();
        assert_eq!(
            read_request_line(&mut reader),
            None,
            "a timed-out read must drop the connection"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "the deadline must actually bound the blocking read"
        );
        drop(client);
    }

    /// Accepted authenticated polling sockets retain the established persistent
    /// wire contract: kernel timeout ticks are ignored and a driver may issue a
    /// later request on the same connection. Capacity protection is admission-
    /// time busy/retry, never a surprise idle EOF.
    #[test]
    fn authenticated_poll_preserves_slow_cadence() {
        use std::io::Write;

        let (mut client, server) = CtlStream::pair().expect("real control socket pair");
        server
            .set_read_timeout(Some(std::time::Duration::from_millis(50)))
            .expect("arm representative authenticated poll tick");
        let sender = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            client.write_all(b"version\n").expect("late request");
        });
        let mut reader = BufReader::new(server);
        assert_eq!(
            read_authenticated_request_line(&mut reader).as_deref(),
            Some("version"),
            "authenticated poll ticks must not become an undocumented EOF"
        );
        sender.join().expect("late sender exits");
    }

    /// The authenticated liveness tick also preserves `feed-bin` framing: a
    /// valid bounded payload may arrive in chunks separated by several kernel
    /// read timeouts without losing its already-filled prefix.
    #[test]
    fn authenticated_binary_payload_preserves_delayed_chunks() {
        use std::io::Write;

        let (mut client, server) = CtlStream::pair().expect("real control socket pair");
        server
            .set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .expect("arm representative authenticated poll tick");
        let sender = std::thread::spawn(move || {
            client.write_all(b"\0\x01").expect("first binary chunk");
            std::thread::sleep(std::time::Duration::from_millis(80));
            client.write_all(b"\x02").expect("second binary chunk");
            std::thread::sleep(std::time::Duration::from_millis(80));
            client.write_all(b"\x03\x04").expect("final binary chunk");
        });
        let mut payload = [0u8; 5];
        let mut reader = BufReader::new(server);
        read_exact_authenticated(&mut reader, &mut payload)
            .expect("timeout ticks retain the partially filled binary frame");
        assert_eq!(payload, [0, 1, 2, 3, 4]);
        sender.join().expect("delayed binary sender exits");
    }

    /// Publication is fail-closed if the OS could not start any worker: no
    /// connection can be stranded in an inbox with no consumer.
    #[test]
    fn dispatch_without_workers_rejects_and_returns_ownership() {
        let dispatch: BoundedDispatch<CtlStream> = BoundedDispatch::new(2);
        dispatch.set_capacity(0);
        let (_client, server) = CtlStream::pair().expect("real control socket pair");
        assert!(
            dispatch.try_submit(server).is_err(),
            "zero-worker dispatch must fail closed"
        );
        assert_eq!(dispatch.outstanding(), 0);
    }

    /// A panicking connection handler cannot leak its admission permit and
    /// permanently shrink the fixed pool. This is the exact guard used by both
    /// ordinary and subscription workers.
    #[test]
    fn dispatch_completion_is_panic_safe() {
        let dispatch = BoundedDispatch::new(1);
        dispatch.set_capacity(1);
        dispatch.try_submit(()).expect("first job admitted");
        dispatch.pop();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _completion = dispatch.completion_guard();
            panic!("synthetic handler panic");
        }));
        assert!(panicked.is_err(), "negative control actually panics");
        assert_eq!(dispatch.outstanding(), 0, "unwind released the lane");
        assert!(
            dispatch.try_submit(()).is_ok(),
            "a subsequent client can reuse the recovered lane"
        );
    }

    /// Regression for the native-active timeout incident: connection churn must
    /// reuse a fixed worker, not create one pthread per `aterm-ctl` invocation.
    /// Drive real local sockets through the shipping bounded dispatch and prove
    /// one worker services a burst far larger than its admission capacity.
    #[test]
    fn fixed_connection_worker_reuses_one_thread_across_socket_burst() {
        use std::io::{Read, Write};

        const REQUESTS: usize = 96;
        let dispatch: Arc<BoundedDispatch<CtlStream>> = Arc::new(BoundedDispatch::new(1));
        dispatch.set_capacity(1);
        let worker_dispatch = dispatch.clone();
        let (served_tx, served_rx) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("control-queue-test".to_string())
            .spawn(move || {
                let worker_id = std::thread::current().id();
                for _ in 0..REQUESTS {
                    let mut server = worker_dispatch.pop();
                    let mut request = [0u8; 1];
                    server.read_exact(&mut request).expect("request byte");
                    assert_eq!(request, [b'?']);
                    server.write_all(b"!").expect("reply byte");
                    worker_dispatch.complete();
                    served_tx.send(()).expect("report completed admission");
                }
                worker_id
            })
            .expect("start the one fixed test worker");

        for _ in 0..REQUESTS {
            let (mut client, server) = CtlStream::pair().expect("real control socket pair");
            dispatch
                .try_submit(server)
                .unwrap_or_else(|_| panic!("sequential request must fit the one reusable lane"));
            client.write_all(b"?").expect("write request");
            let mut reply = [0u8; 1];
            client.read_exact(&mut reply).expect("read reply");
            assert_eq!(reply, [b'!']);
            served_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("worker released the lane");
        }

        let worker_id = worker.join().expect("fixed worker exits cleanly");
        assert_ne!(
            worker_id,
            std::thread::current().id(),
            "the socket burst ran on exactly one reusable background worker"
        );
        assert_eq!(dispatch.outstanding(), 0, "every admission completed");
    }

    /// Push capacity is isolated and lane-exact: four long-lived subscribers own
    /// four workers, the FIFTH gets a prompt explicit error rather than waiting
    /// behind them without an ACK, and completion restores one admission.
    #[test]
    fn fifth_subscription_is_promptly_rejected_and_capacity_recovers() {
        use std::io::BufRead;

        let subscriptions = SubscriptionDispatch::new();
        subscriptions
            .jobs
            .set_capacity(CONTROL_SUBSCRIPTION_WORKERS);
        let mut clients = Vec::new();
        for index in 0..CONTROL_SUBSCRIPTION_WORKERS {
            let (client, server) = CtlStream::pair().expect("subscription socket pair");
            clients.push(client);
            assert!(
                subscriptions
                    .try_submit(
                        format!("subscribe @. screen since={index}"),
                        Scope::Owner,
                        server
                    )
                    .is_ok(),
                "subscriber {index} owns one reserved push lane"
            );
        }
        let (overflow_client, overflow_server) =
            CtlStream::pair().expect("fifth subscription pair");
        overflow_client
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .expect("bound rejection read");
        handoff_subscription(
            &subscriptions,
            "subscribe @. screen".to_string(),
            Scope::Owner,
            overflow_server,
        );
        let mut rejection = String::new();
        BufReader::new(overflow_client)
            .read_line(&mut rejection)
            .expect("fifth subscriber gets a prompt response");
        assert_eq!(rejection, "ERR subscription capacity busy; retry\n");

        // Independent ordinary admission remains live while push is saturated.
        let ordinary: BoundedDispatch<CtlStream> = BoundedDispatch::new(1);
        ordinary.set_capacity(1);
        let (_ordinary_client, ordinary_server) = CtlStream::pair().expect("ordinary control pair");
        assert!(ordinary.try_submit(ordinary_server).is_ok());

        // Popping starts long-lived work; it must NOT release capacity early.
        drop(subscriptions.jobs.pop());
        let (_recovery_client, recovery_server) =
            CtlStream::pair().expect("recovery subscription pair");
        assert!(
            subscriptions
                .try_submit(
                    "subscribe @. screen".to_string(),
                    Scope::Owner,
                    recovery_server,
                )
                .is_err(),
            "starting a long-lived subscription does not create a phantom slot"
        );
        subscriptions.jobs.complete();
        let (_restored_client, restored_server) =
            CtlStream::pair().expect("restored subscription pair");
        assert!(
            subscriptions
                .try_submit(
                    "subscribe @. screen".to_string(),
                    Scope::Owner,
                    restored_server,
                )
                .is_ok(),
            "one completed subscriber restores exactly one push lane"
        );
        drop(clients);
    }

    /// `dial-token` validates arity + that the token is real hex BEFORE writing to
    /// the Keychain / a file, so a malformed call has no side effect.
    #[test]
    fn dial_token_verb_validates_usage_and_hex_without_side_effects() {
        assert!(cmd_dial_token("").starts_with("ERR usage"));
        assert!(cmd_dial_token("only-name").starts_with("ERR usage"));
        assert!(cmd_dial_token("a b c").starts_with("ERR usage"));
        // Bad token hex is rejected (no store attempted).
        let r = cmd_dial_token("work not-64-hex");
        assert!(
            r.starts_with("ERR") && !r.contains("OK"),
            "bad hex rejected: {r}"
        );
    }

    /// `take_mods` is ADDITIVE: a line WITHOUT `mods=` parses to empty mods and
    /// the untouched body, so every pre-Phase-0.5 caller stays byte-compatible.
    #[test]
    fn take_mods_is_additive() {
        use aterm_types::keyboard::Modifiers;
        let (m, body) = take_mods("up");
        assert_eq!(m, Modifiers::empty());
        assert_eq!(body, "up");
        let (m, body) = take_mods("up mods=ctrl+shift");
        assert_eq!(m, Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(body, "up");
        // Aliases + comma separator + token position-independence.
        let (m, body) = take_mods("mods=cmd,alt end");
        assert_eq!(m, Modifiers::SUPER | Modifiers::ALT);
        assert_eq!(body, "end");
    }

    /// `parse_key` builds the named-key event the seam encodes; unknown -> None.
    #[test]
    fn parse_key_grammar() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey as Nk};
        let press = KeyEventType::Press;
        assert_eq!(
            parse_key("up"),
            Some(InputEvent::Key {
                key: Key::Named(Nk::ArrowUp),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: press
            }),
        );
        assert_eq!(
            parse_key("f5 mods=ctrl"),
            Some(InputEvent::Key {
                key: Key::Named(Nk::F5),
                mods: Modifiers::CTRL,
                base_layout: None,
                event_type: press
            }),
        );
        assert_eq!(parse_key("nope"), None);

        // Inline modifier+character combos build the SAME (Key::Character, mods)
        // event `parse_ctrl` does, so the encoder derives the control byte
        // (`ctrl+u` -> 0x15) — see `parse_ctrl_eq_inline_key` for the byte proof.
        for c in ['u', 'c', 'a', 'd', 'l', 'w'] {
            assert_eq!(
                parse_key(&format!("ctrl+{c}")),
                Some(InputEvent::Key {
                    key: Key::Character(c),
                    mods: Modifiers::CTRL,
                    base_layout: None,
                    event_type: press,
                }),
                "ctrl+{c} should be Character('{c}') + CTRL",
            );
        }
        // Case-insensitive on the character, matching `parse_ctrl`.
        assert_eq!(
            parse_key("ctrl+U"),
            Some(InputEvent::Key {
                key: Key::Character('u'),
                mods: Modifiers::CTRL,
                base_layout: None,
                event_type: press
            }),
        );
        // alt+/shift+/super+ and stacked prefixes.
        assert_eq!(
            parse_key("alt+x"),
            Some(InputEvent::Key {
                key: Key::Character('x'),
                mods: Modifiers::ALT,
                base_layout: None,
                event_type: press
            }),
        );
        assert_eq!(
            parse_key("ctrl+shift+a"),
            Some(InputEvent::Key {
                key: Key::Character('a'),
                mods: Modifiers::CTRL | Modifiers::SHIFT,
                base_layout: None,
                event_type: press,
            }),
        );
        // Inline prefixes are additive with a trailing `mods=` token.
        assert_eq!(parse_key("ctrl+u"), parse_key("u mods=ctrl"));
        // Inline prefixes also apply to NAMED keys.
        assert_eq!(
            parse_key("ctrl+up"),
            Some(InputEvent::Key {
                key: Key::Named(Nk::ArrowUp),
                mods: Modifiers::CTRL,
                base_layout: None,
                event_type: press
            }),
        );
        // The literal `+` key (no recognized modifier before it) survives.
        assert_eq!(
            parse_key("+"),
            Some(InputEvent::Key {
                key: Key::Character('+'),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: press
            }),
        );
        // A multi-char residual that is not a named key is still rejected.
        assert_eq!(parse_key("ctrl+nope"), None);
    }

    /// The FULL `NamedKey` vocabulary is reachable — numpad, F13–F35, media/audio,
    /// modifier-side keys, system keys — so a controller can press any physical key
    /// a human can (closes the `key`-grammar fidelity gap). Table-driven.
    #[test]
    fn parse_key_full_named_vocabulary() {
        use aterm_types::keyboard::{Key, NamedKey as Nk};
        let cases: &[(&str, Nk)] = &[
            ("space", Nk::Space),
            ("capslock", Nk::CapsLock),
            ("menu", Nk::ContextMenu),
            ("contextmenu", Nk::ContextMenu),
            ("printscreen", Nk::PrintScreen),
            ("f13", Nk::F13),
            ("f35", Nk::F35),
            ("kp0", Nk::Numpad0),
            ("kp9", Nk::Numpad9),
            ("kpdot", Nk::NumpadDecimal),
            ("kpenter", Nk::NumpadEnter),
            ("kpadd", Nk::NumpadAdd),
            ("kpbegin", Nk::NumpadBegin),
            ("shiftleft", Nk::ShiftLeft),
            ("metaright", Nk::MetaRight),
            ("hyperleft", Nk::HyperLeft),
            ("mediaplaypause", Nk::MediaPlayPause),
            ("volumeup", Nk::AudioVolumeUp),
            ("mute", Nk::AudioVolumeMute),
        ];
        for (tok, want) in cases {
            assert_eq!(
                parse_key(tok),
                Some(InputEvent::Key {
                    key: Key::Named(*want),
                    mods: aterm_types::keyboard::Modifiers::empty(),
                    base_layout: None,
                    event_type: aterm_types::keyboard::KeyEventType::Press,
                }),
                "token `{tok}` should map to {want:?}",
            );
        }
    }

    /// `type=press|repeat|release` reaches the event (and drives the Kitty CSI-u
    /// event-type sub-field); an unknown value rejects the whole line.
    #[test]
    fn parse_key_event_type() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey as Nk};
        let ev = |t| {
            Some(InputEvent::Key {
                key: Key::Named(Nk::ArrowUp),
                mods: Modifiers::empty(),
                base_layout: None,
                event_type: t,
            })
        };
        assert_eq!(parse_key("up"), ev(KeyEventType::Press));
        assert_eq!(parse_key("up type=press"), ev(KeyEventType::Press));
        assert_eq!(parse_key("up type=repeat"), ev(KeyEventType::Repeat));
        assert_eq!(parse_key("up type=release"), ev(KeyEventType::Release));
        assert_eq!(parse_key("up type=up"), ev(KeyEventType::Release));
        // Additive with mods=, position-independent.
        assert_eq!(
            parse_key("type=release mods=ctrl up"),
            Some(InputEvent::Key {
                key: Key::Named(Nk::ArrowUp),
                mods: Modifiers::CTRL,
                base_layout: None,
                event_type: KeyEventType::Release,
            }),
        );
        // Unknown event type rejects the line.
        assert_eq!(parse_key("up type=bogus"), None);
    }

    /// `base=<char>` carries the US-QWERTY base-layout key (Kitty
    /// REPORT_ALTERNATE_KEYS 3rd field); a non-single-char value rejects.
    #[test]
    fn parse_key_base_layout() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers};
        assert_eq!(
            parse_key("q base=a"),
            Some(InputEvent::Key {
                key: Key::Character('q'),
                mods: Modifiers::empty(),
                base_layout: Some('a'),
                event_type: KeyEventType::Press,
            }),
        );
        assert_eq!(parse_key("q base=ab"), None);
    }

    /// `meta` and `hyper` are their OWN modifier bits (Kitty), distinct from ALT.
    #[test]
    fn take_mods_parses_meta_and_hyper_distinctly() {
        use aterm_types::keyboard::Modifiers;
        let (m, _) = take_mods("a mods=meta");
        assert_eq!(m, Modifiers::META);
        let (m, _) = take_mods("a mods=hyper");
        assert_eq!(m, Modifiers::HYPER);
        // alt is still ALT, and meta no longer aliases it.
        let (m, _) = take_mods("a mods=alt");
        assert_eq!(m, Modifiers::ALT);
        let (m, _) = take_mods("a mods=ctrl+meta+hyper");
        assert_eq!(m, Modifiers::CTRL | Modifiers::META | Modifiers::HYPER);
        // Inline-prefix form agrees.
        assert_eq!(parse_key("meta+x"), parse_key("x mods=meta"));
    }

    /// `send` is byte-faithful: interior whitespace is NOT collapsed (the line
    /// decoder / dispatcher preserve the tail verbatim).
    #[test]
    #[cfg(unix)]
    fn send_preserves_whitespace() {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let sink = SinkWriter::new(fds[1]);
        let reply = cmd_send(&sink, "a   b\tc");
        assert_eq!(reply, "OK\n");
        // `SinkWriter::new` does NOT own the fd, so close the write end explicitly
        // (mirroring `input::tests::egress_bytes`) or `read_to_end` blocks forever.
        unsafe { libc::close(fds[1]) };
        let mut got = Vec::new();
        let mut r = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"a   b\tc");
    }

    /// ITEM 1 keystone: the styled `screen` frame carries EVERY resolved
    /// decoration — including the four the legacy `cell` verb dropped (underline
    /// SUBSTYLE, overline, underline colour, and — via the bold path — the
    /// renderer's resolved rendition). This is the regression that proves
    /// losslessness vs the old plaintext/flag-bits projection.
    #[test]
    fn screen_styled_reports_all_resolved_decorations() {
        use aterm_core::terminal::Terminal;
        let mut t = Terminal::new(3, 10);
        // bold + curly underline (SGR 4:3) + overline (SGR 53) + RGB underline
        // colour (SGR 58:2::255:0:0) applied to 'Z'.
        t.process(b"\x1b[1m\x1b[4:3m\x1b[53m\x1b[58:2::255:0:0mZ");
        let frame = styled_frame_payload(&t);
        assert!(
            frame.contains("\"underline_style\":\"curly\""),
            "curly underline lost: {frame}"
        );
        assert!(
            frame.contains("\"overline\":true"),
            "overline lost: {frame}"
        );
        assert!(
            frame.contains("\"underline_color\":\"ff0000\""),
            "underline colour lost: {frame}"
        );
        assert!(frame.contains("\"bold\""), "bold attr lost: {frame}");
    }

    /// The styled frame is the FULL grid with NO trim: every one of rows×cols
    /// cells is present (the lossless contract), dims/seq are reported.
    #[test]
    fn screen_styled_frame_shape_no_trim() {
        use aterm_core::terminal::Terminal;
        let mut t = Terminal::new(3, 10);
        t.process(b"hi");
        let frame = styled_frame_payload(&t);
        assert!(
            frame.contains("\"dims\":{\"rows\":3,\"cols\":10}"),
            "{frame}"
        );
        // 3 rows × 10 cols = 30 cells, each carries exactly one "glyph" key.
        let glyphs = frame.matches("\"glyph\"").count();
        assert_eq!(glyphs, 30, "expected 30 cells with no trim, got {glyphs}");
        assert!(
            frame.contains(&format!("\"seq\":{}", t.content_seq())),
            "{frame}"
        );
    }

    /// The glyph is the combining-aware grapheme (é = e+U+0301, not bare 'e'), and
    /// the cell's OSC-8 hyperlink target is surfaced — both lossless vs a human.
    #[test]
    fn screen_styled_glyph_and_hyperlink_faithful() {
        use aterm_core::terminal::Terminal;
        let mut t = Terminal::new(2, 20);
        t.process("e\u{0301}".as_bytes());
        t.process(b"\x1b]8;;https://example.com\x1b\\L\x1b]8;;\x1b\\");
        let frame = styled_frame_payload(&t);
        assert!(
            frame.contains("\"glyph\":\"e\u{0301}\""),
            "combining grapheme lost: {frame}"
        );
        assert!(
            frame.contains("\"hyperlink\":\"https://example.com\""),
            "hyperlink lost: {frame}"
        );
    }

    /// The `screen` verb wraps the frame in the standard single-line `OK 1\n…\n`
    /// read framing — the JSON body is ONE physical line so the existing
    /// line-count client streams it unchanged regardless of grid size.
    #[test]
    fn screen_verb_framing_is_single_line_json() {
        use aterm_core::terminal::Terminal;
        let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
        let out = cmd_screen_styled_json(&term);
        assert!(out.starts_with("OK 1\n"), "{out}");
        let body = out
            .strip_prefix("OK 1\n")
            .unwrap()
            .strip_suffix('\n')
            .unwrap();
        assert!(
            !body.contains('\n'),
            "styled frame must be single-line JSON: {body}"
        );
        assert!(
            body.starts_with("{\"seq\":") && body.ends_with('}'),
            "{body}"
        );
    }

    /// `screen` is gated as a READ verb (ReadScreen), like every other observer.
    #[test]
    fn screen_verb_is_read_gated() {
        assert_eq!(required_op("screen"), Some(Op::ReadScreen));
    }

    /// F4: a small inline image encodes normally; an oversized one (user-supplied
    /// OSC 1337) is `truncated` with an EMPTY payload — but its real `nbytes` is
    /// still reported, so a consumer learns it exists without the multi-MB blowup on
    /// every `image read` / styled frame.
    #[test]
    fn oversized_image_payload_is_truncated_not_encoded() {
        use aterm_core::grid::extra::{ImageData, ImageFormat};
        let small = ImageData {
            bytes: vec![1, 2, 3, 4],
            format: ImageFormat::Png,
            cols: 1,
            rows: 1,
            z_index: 0,
            band_lift_px: 0,
        };
        let (fmt, b64) = image_payload(&small);
        assert_eq!(fmt, "png");
        assert!(!b64.is_empty(), "small image must carry its payload");

        let big = ImageData {
            bytes: vec![0u8; MAX_IMAGE_PAYLOAD_BYTES + 1],
            format: ImageFormat::Png,
            cols: 80,
            rows: 24,
            z_index: 0,
            band_lift_px: 0,
        };
        let (fmt, b64) = image_payload(&big);
        assert_eq!(fmt, "truncated", "oversized image must be marked truncated");
        assert!(b64.is_empty(), "oversized image must NOT be base64-encoded");
        // The line form still reports the real size (so the consumer can decide).
        let line = image_read_line(0, 0, 0, 0, &big);
        assert!(
            line.contains(&format!("truncated {}", MAX_IMAGE_PAYLOAD_BYTES + 1)),
            "{line}"
        );
        // And the JSON form is well-formed with an empty payload + real nbytes.
        let js = styled_image_json(0, 0, &big);
        assert!(
            js.contains("\"format\":\"truncated\"") && js.contains("\"b64\":\"\""),
            "{js}"
        );
        assert!(
            js.contains(&format!("\"nbytes\":{}", MAX_IMAGE_PAYLOAD_BYTES + 1)),
            "{js}"
        );
    }

    /// LOSSLESS FIDELITY (F1/F2/F3): the styled frame carries inline IMAGES (not
    /// blank cells), DEC double-width/height LINE SIZES, and the text SELECTION —
    /// the three fields the renderer consumes that were once dropped. A human sees
    /// all three; an outer agent watching the frame now does too.
    #[test]
    fn screen_styled_frame_carries_images_line_sizes_and_selection() {
        use aterm_core::terminal::Terminal;
        let mut t = Terminal::new(3, 10);
        // F1: an inline image (OSC 1337 File=, 2x1) — PNG magic + 4 NULs.
        t.process(b"\x1b]1337;File=inline=1;width=2;height=1:iVBORw0KGgoAAAAA\x1b\\");
        // F2: make row 1 double-width (DECDWL, ESC # 6).
        t.process(b"\x1b[2;1H\x1b#6");
        let frame = styled_frame_payload(&t);
        assert!(
            frame.contains("\"images\":[{\"row\":0,\"col\":0,\"cols\":2,\"rows\":1,\"format\":\"png\",\"nbytes\":12,\"b64\":\"iVBORw0KGgoAAAAA\"}]"),
            "image must be in the frame, not a blank cell: {frame}"
        );
        assert!(
            frame.contains("\"double_width\""),
            "double-width line size lost: {frame}"
        );
        // F3: select a region, assert it surfaces.
        {
            let sel = t.text_selection_mut();
            sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 5, SelectionSide::Right);
            sel.complete_selection();
        }
        let frame = styled_frame_payload(&t);
        assert!(
            frame.contains("\"selection\":{\"start_row\":0,\"start_col\":1,"),
            "selection must surface in the frame: {frame}"
        );
        // And no-selection / no-image stays null / empty (the cheap common case).
        let plain = Terminal::new(2, 4);
        let pf = styled_frame_payload(&plain);
        assert!(
            pf.contains("\"selection\":null"),
            "no selection -> null: {pf}"
        );
        assert!(pf.contains("\"images\":[]"), "no images -> empty: {pf}");
    }

    /// An inline iTerm2 image (OSC 1337 `File=`) read back as STRUCTURED base64,
    /// deduplicated across its covered cells. The base64 INPUT below is a
    /// hand-computed literal (PNG magic + 4 NUL bytes), so the OUTPUT matching it
    /// proves `b64_encode` independently. `image read` is the headless,
    /// framebuffer-free path.
    #[test]
    fn image_read_returns_payload_and_dedups() {
        use aterm_core::terminal::Terminal;
        // 12 raw bytes = PNG magic (8) + 4×0x00; standard base64 = "iVBORw0KGgoAAAAA".
        let term = Arc::new(Mutex::new(Terminal::new(3, 10)));
        term_lock(&term)
            .process(b"\x1b]1337;File=inline=1;width=2;height=1:iVBORw0KGgoAAAAA\x1b\\");
        let out = cmd_image_read(&term, "");
        let mut lines = out.lines();
        assert_eq!(
            lines.next().unwrap(),
            "OK 1",
            "expected one deduped image: {out}"
        );
        let line = lines.next().unwrap();
        // <row> <col> <img_cols> <img_rows> <cell_row> <cell_col> <format> <nbytes> <b64>
        assert_eq!(line, "0 0 2 1 0 0 png 12 iVBORw0KGgoAAAAA", "got: {line}");
        assert!(
            lines.next().is_none(),
            "image must be deduped to one line: {out}"
        );
    }

    /// `image read` on a screen with no images is `OK 0`.
    #[test]
    fn image_read_empty_screen_is_ok_zero() {
        use aterm_core::terminal::Terminal;
        let term = Arc::new(Mutex::new(Terminal::new(3, 10)));
        assert_eq!(cmd_image_read(&term, ""), "OK 0\n");
    }

    /// Cell addressing: `image read <r> <c>` returns the covering tile, with the
    /// tile coords of the queried cell; a cell with no image is `ERR none`.
    #[test]
    fn image_read_cell_addressing_and_none() {
        use aterm_core::terminal::Terminal;
        let term = Arc::new(Mutex::new(Terminal::new(3, 10)));
        term_lock(&term)
            .process(b"\x1b]1337;File=inline=1;width=2;height=1:iVBORw0KGgoAAAAA\x1b\\");
        // Cell (0,1) is the right tile of the 2-wide image: cell_col == 1.
        let out = cmd_image_read(&term, "0 1");
        assert_eq!(
            out, "OK 1\n0 0 2 1 0 1 png 12 iVBORw0KGgoAAAAA\n",
            "got: {out}"
        );
        // A cell with no image -> ERR none.
        assert_eq!(cmd_image_read(&term, "0 5"), "ERR none\n");
        // Out of grid -> ERR out of range.
        assert_eq!(cmd_image_read(&term, "9 9"), "ERR out of range\n");
    }

    /// `image` (incl. `image read`) is ReadScreen-gated and therefore allowed
    /// cross-session (the read path is matched before the rasterize fail-closed).
    #[test]
    fn image_read_is_readscreen() {
        assert_eq!(required_op("image"), Some(Op::ReadScreen));
    }

    /// ITEM 5b: the cross-process forward DECISION — Owner-only, presents the
    /// per-op edge token, rewrites the child selector to `@.`, and fails closed on
    /// an Edge scope, an unknown child, or a relaunched (nonce-mismatched) child.
    #[test]
    fn proxy_forward_plan_owner_only_op_scoped_and_nonce_guarded() {
        use aterm_session::{EdgeToken, LaunchNonce, SessionId};
        let dir = std::env::temp_dir().join(format!("aterm-fwd-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = crate::session_store::new_store();
        let child = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let entry = crate::proxy::ProxyEntry {
            nonce,
            read: EdgeToken::generate(),
            write: EdgeToken::generate(),
            signal: EdgeToken::generate(),
        };
        let read_hex = entry.read.to_hex();
        let write_hex = entry.write.to_hex();
        crate::proxy::register_child(child.clone(), entry);
        // The published socket MUST live directly in `dir` (the runtime sock dir);
        // `confine_proxy_sock` rejects anything else. The forward dials the confined
        // (canonical-dir-rooted) path.
        let child_sock = dir.join("aterm-child.sock");
        let child_sock_str = child_sock.to_string_lossy().into_owned();
        let confined = std::fs::canonicalize(&dir)
            .unwrap()
            .join("aterm-child.sock");
        let confined_str = confined.to_string_lossy().into_owned();
        crate::proxy::write_graph_entry(&dir, &child, &child_sock_str, &nonce);

        let line = format!("@{} screen", child.as_str());
        // Owner forwards: read verb → READ token, selector rewritten to @.
        let plan = proxy_forward_plan(&line, Scope::Owner, &store, &dir).expect("owner forwards");
        assert_eq!(plan.0, confined_str);
        assert_eq!(plan.1, format!("TOKEN {read_hex} @. screen\n"));
        // A write verb selects the WRITE token (op→token mapping).
        let kline = format!("@{} key up", child.as_str());
        let kplan = proxy_forward_plan(&kline, Scope::Owner, &store, &dir).expect("write forwards");
        assert_eq!(kplan.1, format!("TOKEN {write_hex} @. key up\n"));

        // M1: the selector-SECOND `subscribe` grammar is forwarded too (the bug
        // was that only @-first lines were), presenting the READ token, rewriting
        // the child selector to `@.`, and preserving the streams + flags.
        let sline = format!("subscribe @{} cells,bytes every-frame", child.as_str());
        let splan =
            proxy_forward_plan(&sline, Scope::Owner, &store, &dir).expect("subscribe forwards");
        assert_eq!(splan.0, confined_str);
        assert_eq!(
            splan.1,
            format!("TOKEN {read_hex} subscribe @. cells,bytes every-frame\n")
        );

        // Edge scope cannot escalate to a child.
        assert!(
            proxy_forward_plan(&line, Scope::Edge(EdgeToken::generate()), &store, &dir).is_none(),
            "edge scope must not forward",
        );
        // A subscribe with a comma-list of targets stays local (not a single child).
        let mixed = format!("subscribe @.,@{} cells", child.as_str());
        assert!(proxy_forward_plan(&mixed, Scope::Owner, &store, &dir).is_none());
        // An unregistered child id → no plan.
        let other = format!("@{} screen", SessionId::generate().as_str());
        assert!(proxy_forward_plan(&other, Scope::Owner, &store, &dir).is_none());

        // A relaunch (graph entry under a NEW nonce) fails closed.
        crate::proxy::write_graph_entry(&dir, &child, &child_sock_str, &LaunchNonce::generate());
        assert!(
            proxy_forward_plan(&line, Scope::Owner, &store, &dir).is_none(),
            "nonce mismatch must fail closed",
        );

        // CONFINEMENT: a graph entry redirected to a socket OUTSIDE our runtime dir
        // (a hostile same-uid overwrite that copies the readable nonce) fails closed —
        // the parent never dials it nor presents the edge token. Restore the correct
        // nonce so ONLY the out-of-dir path is what trips the gate.
        crate::proxy::write_graph_entry(&dir, &child, "/tmp/evil-attacker.sock", &nonce);
        assert!(
            proxy_forward_plan(&line, Scope::Owner, &store, &dir).is_none(),
            "out-of-dir socket path must fail closed (token must not leak)",
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SIBLING HOP: an `@<sid>` that is neither hosted locally nor a spawned child
    /// but IS published in the graph by another live same-uid instance forwards to
    /// that instance's socket, presenting the SIBLING'S OWN per-launch token (the
    /// same-uid 0600 credential any direct client reads) and KEEPING the original
    /// selector so the sibling resolves the session among its own tabs. Fail-closed
    /// guards each asserted: Edge scope, locally-hosted sid, self-dial (a stale
    /// entry pointing at our own socket), dead listener, unreadable token, and
    /// non-op verbs. All self-sock assertions live in THIS ONE test — the recorded
    /// self socket is process-global and must not race across tests.
    #[test]
    #[cfg(unix)]
    fn sibling_forward_plan_presents_instance_token_and_keeps_selector() {
        use aterm_session::{LaunchNonce, SessionId};
        // Serialize with every other test touching the process-global self sock.
        let _guard = crate::proxy::self_sock_test_guard();
        let dir = std::env::temp_dir().join(format!("aterm-sib-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = crate::session_store::new_store();
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();

        // The "sibling": a LIVE listener at <dir>/aterm-77001.sock + its 0600 token.
        let sib_sock = dir.join("aterm-77001.sock");
        let sib_sock_str = sib_sock.to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&sib_sock);
        let _listener = std::os::unix::net::UnixListener::bind(&sib_sock).expect("bind sibling");
        std::fs::write(dir.join("aterm-77001.token"), "cafe0123\n").expect("token");
        crate::proxy::write_graph_entry(&dir, &sid, &sib_sock_str, &nonce);

        let confined = std::fs::canonicalize(&dir)
            .unwrap()
            .join("aterm-77001.sock")
            .to_string_lossy()
            .into_owned();

        // Owner + live sibling + op verb → forward, selector KEPT, sibling token.
        let line = format!("@{} text", sid.as_str());
        let plan = proxy_forward_plan(&line, Scope::Owner, &store, &dir).expect("sibling forwards");
        assert_eq!(plan.0, confined);
        assert_eq!(plan.1, format!("TOKEN cafe0123 @{} text\n", sid.as_str()));

        // A write verb forwards identically (no per-op token split on this hop —
        // the sibling's own gate classifies the op).
        let kline = format!("@{} send hi there", sid.as_str());
        let kplan = proxy_forward_plan(&kline, Scope::Owner, &store, &dir).expect("write forwards");
        assert_eq!(
            kplan.1,
            format!("TOKEN cafe0123 @{} send hi there\n", sid.as_str())
        );

        // Selector-SECOND subscribe grammar: forwarded verbatim (selector kept).
        let sline = format!("subscribe @{} cells,bytes", sid.as_str());
        let splan =
            proxy_forward_plan(&sline, Scope::Owner, &store, &dir).expect("subscribe forwards");
        assert_eq!(splan.0, confined);
        assert_eq!(
            splan.1,
            format!("TOKEN cafe0123 subscribe @{} cells,bytes\n", sid.as_str())
        );

        // Edge scope must NOT reach a sibling.
        assert!(
            proxy_forward_plan(
                &line,
                Scope::Edge(aterm_session::EdgeToken::generate()),
                &store,
                &dir
            )
            .is_none(),
            "edge scope must not forward to a sibling",
        );

        // A non-op verb (identity/privilege) never crosses processes.
        let gline = format!("@{} whoami", sid.as_str());
        assert!(proxy_forward_plan(&gline, Scope::Owner, &store, &dir).is_none());

        // A LOCALLY-HOSTED sid is not a hop (in-process resolution owns it).
        let local = registered_session(91, -1, b"");
        let local_line = format!("@{} text", local.sid.as_str());
        crate::proxy::write_graph_entry(&dir, &local.sid, &sib_sock_str, &local.nonce);
        store.write().unwrap().register(local.clone());
        assert!(
            proxy_forward_plan(&local_line, Scope::Owner, &store, &dir).is_none(),
            "locally-hosted sid must resolve in-process, never hop",
        );

        // SELF-DIAL guard: an entry pointing at OUR OWN socket (stale record of a
        // session we no longer host) is refused rather than looped back. Record
        // the RAW (non-canonical) path exactly as production does — on macOS the
        // temp dir sits behind the `/var` → `/private/var` symlink, so this also
        // proves the guard survives the normalization mismatch a review
        // adversary found (raw recorded path vs canonicalized dial path).
        crate::proxy::set_self_sock(&dir, &sib_sock_str);
        let stale = SessionId::generate();
        crate::proxy::write_graph_entry(&dir, &stale, &sib_sock_str, &nonce);
        let stale_line = format!("@{} text", stale.as_str());
        assert!(
            proxy_forward_plan(&stale_line, Scope::Owner, &store, &dir).is_none(),
            "an entry naming our own socket must never self-dial",
        );
        crate::proxy::clear_self_sock();

        // CONFINEMENT on the SIBLING hop: an entry redirected OUTSIDE our runtime
        // dir must never be dialed (same posture as the child hop).
        let outside = SessionId::generate();
        crate::proxy::write_graph_entry(&dir, &outside, "/tmp/evil-sibling.sock", &nonce);
        let out_line = format!("@{} text", outside.as_str());
        assert!(
            proxy_forward_plan(&out_line, Scope::Owner, &store, &dir).is_none(),
            "an out-of-dir sibling socket must fail closed",
        );

        // DEAD listener: entry present, socket file present, nobody listening.
        let dead = SessionId::generate();
        let dead_sock = dir.join("aterm-77002.sock");
        let _ = std::fs::remove_file(&dead_sock);
        drop(std::os::unix::net::UnixListener::bind(&dead_sock).expect("bind then drop"));
        std::fs::write(dir.join("aterm-77002.token"), "beef\n").expect("token");
        crate::proxy::write_graph_entry(&dir, &dead, &dead_sock.to_string_lossy(), &nonce);
        let dead_line = format!("@{} text", dead.as_str());
        // PRECONDITION, stated rather than assumed: the kernel must answer this
        // dial with ECONNREFUSED. `socket_is_live` deliberately treats ANY OTHER
        // connect error (EMFILE under a saturated test process, EINTR, EAGAIN) as
        // "maybe live — never hijack", so under load the plan below can legitimately
        // return `Some` for reasons that are the environment's, not the code's.
        // Wait out that transient instead of reporting it as a proxy defect.
        let refused = (0..50).any(|_| match aterm_uds::CtlStream::connect(&dead_sock) {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => true,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                false
            }
            Ok(_) => panic!("nobody should be listening on the dead sibling socket"),
        });
        assert!(
            refused,
            "PRECONDITION: the dead sibling socket never answered ECONNREFUSED (transient \
             connect errors persisted for a second) — environment, not the proxy"
        );
        assert!(
            proxy_forward_plan(&dead_line, Scope::Owner, &store, &dir).is_none(),
            "a dead sibling must fail closed, not hang a dial",
        );

        // UNREADABLE token: live listener but no token file → no forward (the
        // relay must never dial unauthenticated).
        let toothless = SessionId::generate();
        let tl_sock = dir.join("aterm-77003.sock");
        let _tl = std::os::unix::net::UnixListener::bind(&tl_sock).expect("bind");
        crate::proxy::write_graph_entry(&dir, &toothless, &tl_sock.to_string_lossy(), &nonce);
        let tl_line = format!("@{} text", toothless.as_str());
        assert!(
            proxy_forward_plan(&tl_line, Scope::Owner, &store, &dir).is_none(),
            "a sibling with no readable token must fail closed",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CONFORMANCE (Tier-1) of the REAL router to `dispatch_complete_model`'s
    /// invariant `ForwardableRemoteAlwaysForwarded`: for an Owner reaching a
    /// registered REMOTE child, `proxy_forward_plan` forwards a verb IFF that verb
    /// is forwardable — exactly the op-bearing verbs (`required_op` ⇒ read/write/
    /// signal). This is the code↔model binding the abstract `ty` proof needs: it
    /// drives the real decision function over every verb CLASS and checks it
    /// matches the modeled predicate. Drop the subscribe forward arm (M1), or let
    /// any forwardable verb fall to the local path, and this fails.
    #[test]
    fn proxy_forward_plan_conforms_to_dispatch_model() {
        use aterm_session::{EdgeToken, LaunchNonce, SessionId};
        let dir = std::env::temp_dir().join(format!("aterm-conform-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = crate::session_store::new_store();
        let child = SessionId::generate();
        let nonce = LaunchNonce::generate();
        crate::proxy::register_child(
            child.clone(),
            crate::proxy::ProxyEntry {
                nonce,
                read: EdgeToken::generate(),
                write: EdgeToken::generate(),
                signal: EdgeToken::generate(),
            },
        );
        // The published socket must live directly in `dir` — `confine_proxy_sock`
        // (the proxy forward's anti-token-capture guard) rejects any out-of-dir path.
        let child_sock = dir.join("aterm-child.sock").to_string_lossy().into_owned();
        crate::proxy::write_graph_entry(&dir, &child, &child_sock, &nonce);
        let sid = child.as_str();

        // Representative verb lines across EVERY class: forwardable read-side,
        // forwardable write-side, signal, and NON-forwardable (owner-only / global).
        let cases: &[&str] = &[
            "screen",
            "text",
            "cell 0 0",
            "search x",
            "modes",
            "image read",
            "cast",
            "temporal",
            "scroll up",
            "key up",
            "ctrl c",
            "feed 03",
            "feed-bin 4",
            "paste hi",
            "resize 10 20",
            "focus in",
            "send hi",
            "signal int", // forwardable (read/write/signal)
            "grant x",
            "revoke x",
            "whoami",
            "sessions",
            "version",
            "bogus", // NOT forwardable
        ];
        for verb_line in cases {
            let verb = verb_line.split_whitespace().next().unwrap();
            let line = format!("@{sid} {verb_line}");
            let forwarded = proxy_forward_plan(&line, Scope::Owner, &store, &dir).is_some();
            let forwardable = required_op(verb).is_some();
            assert_eq!(
                forwarded, forwardable,
                "router ≠ model for `{verb}`: forwarded={forwarded} forwardable={forwardable}"
            );
        }
        // `subscribe` is selector-SECOND; it MUST forward (read-side) — the exact M1
        // case the generic @-first parse missed. (Not in the loop: different grammar.)
        let sub = format!("subscribe @{sid} cells,bytes");
        assert!(
            proxy_forward_plan(&sub, Scope::Owner, &store, &dir).is_some(),
            "subscribe @<child> must forward (the M1 regression guard)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `key ctrl+<c>` and `ctrl <c>` build the IDENTICAL event, so both drive
    /// the encoder through the same seam and write the same control byte to the
    /// PTY (`ctrl+u` -> 0x15). This is the load-bearing invariant of the fix.
    #[test]
    fn parse_ctrl_eq_inline_key() {
        for c in ['u', 'c', 'a', 'd', 'l', 'w'] {
            assert_eq!(
                parse_key(&format!("ctrl+{c}")),
                parse_ctrl(&c.to_string()),
                "key ctrl+{c} must equal ctrl {c}",
            );
        }
    }

    /// `parse_ctrl` lower-cases and CTRL-modifies exactly one letter; else None.
    #[test]
    fn parse_ctrl_grammar() {
        use aterm_types::keyboard::{Key, Modifiers};
        assert_eq!(
            parse_ctrl("C"),
            Some(InputEvent::Key {
                key: Key::Character('c'),
                mods: Modifiers::CTRL,
                base_layout: None,
                event_type: aterm_types::keyboard::KeyEventType::Press,
            }),
        );
        assert_eq!(parse_ctrl(""), None);
        assert_eq!(parse_ctrl("ab"), None);
    }

    /// `parse_mouse` — the additive `mods=`/`count=`/`side=`/`block=` grammar, the
    /// load-bearing half of the mouse-convergence claim (kills a/b/i + the
    /// ambient-state read for block-select).
    #[test]
    fn parse_mouse_grammar() {
        use aterm_core::selection::SelectionSide;
        use aterm_types::mouse::{ALT_MASK, MouseButton, SHIFT_MASK};
        // Bare press: empty mods, count 1, left side, simple (block=false).
        assert_eq!(
            parse_mouse("press left 5 9"),
            Ok(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                row: 5,
                col: 9,
                mods: 0,
                click_count: 1,
                side: SelectionSide::Left,
                block: false,
                suppress_copy_on_select: false,
                px_off: crate::input::PixelOffset::CELL_ORIGIN,
            }),
        );
        // Full grammar, tokens in any position.
        assert_eq!(
            parse_mouse("count=2 press left side=right 5 9 mods=shift+alt block=1"),
            Ok(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                row: 5,
                col: 9,
                mods: SHIFT_MASK | ALT_MASK,
                click_count: 2,
                side: SelectionSide::Right,
                block: true,
                suppress_copy_on_select: false,
                px_off: crate::input::PixelOffset::CELL_ORIGIN,
            }),
        );
        // count clamps to 1..=3.
        let Ok(InputEvent::MouseButton { click_count, .. }) = parse_mouse("press left 0 0 count=9")
        else {
            panic!("press parses")
        };
        assert_eq!(click_count, 3);
        // move: bare = hover code 3; with a button = its X10 drag code.
        assert_eq!(
            parse_mouse("move 7 3"),
            Ok(InputEvent::MouseMove {
                buttons: 3,
                row: 7,
                col: 3,
                mods: 0,
                side: SelectionSide::Left,
                px_off: crate::input::PixelOffset::CELL_ORIGIN,
            }),
        );
        let Ok(InputEvent::MouseMove { buttons, .. }) = parse_mouse("move left 7 3") else {
            panic!("drag move parses")
        };
        assert_eq!(buttons, MouseButton::Left.code());
        // wheel actions default to lines=1.
        assert_eq!(
            parse_mouse("wheelup left 2 4"),
            Ok(InputEvent::Wheel {
                dir: aterm_types::mouse::WheelDir::Up,
                lines: 1,
                row: 2,
                col: 4,
                mods: 0,
                px_off: crate::input::PixelOffset::CELL_ORIGIN,
            }),
        );
        // errors.
        assert!(parse_mouse("press left").is_err(), "missing row/col");
        assert!(parse_mouse("press banana 1 1").is_err(), "bad button");
        assert!(parse_mouse("jump left 1 1").is_err(), "bad action");
    }

    /// The control socket follows the ACTIVE tab: `resolve_active` snapshots
    /// whatever the shared `ActiveHandle` currently points at, so after the GUI
    /// updates it on a tab switch, the next request targets the new session.
    #[test]
    fn resolve_active_follows_handle_updates() {
        use aterm_session::sink::SinkWriter;
        use aterm_session::{EdgeTable, LaunchNonce, SessionId};
        let term_a = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let term_b = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(11)),
            edges: std::sync::Mutex::new(EdgeTable::new()),
            turn_lease: std::sync::Mutex::new(None),
            self_id: SessionId::generate(),
            nonce: LaunchNonce::generate(),
            cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
                80, 24,
            ))),
            temporal: Arc::new(std::sync::Mutex::new(
                crate::temporal::TemporalRecorder::new(),
            )),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: Arc::new(std::sync::Mutex::new(
                crate::turn_ledger::TurnLedger::default(),
            )),
            meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
            app_kitty: std::sync::Mutex::new(crate::app_kitty::AppKittySlot::default()),
            timeline: Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
        });
        let active: ActiveHandle = Arc::new(Mutex::new(Some(ActiveSession {
            term: term_a.clone(),
            master: 11,
            id: 0,
            ctx: ctx.clone(),
        })));

        let (t, m, id, _ctx) = resolve_active(&active).expect("active terminal");
        assert!(
            Arc::ptr_eq(&t, &term_a) && m == 11 && id == 0,
            "tab 0 active"
        );

        // GUI switches to a new tab (sync_active_session).
        {
            let mut g = active.lock().unwrap();
            let g = g.as_mut().expect("active terminal");
            g.term = term_b.clone();
            g.master = 22;
            g.id = 3;
        }
        let (t, m, id, _ctx) = resolve_active(&active).expect("active terminal");
        assert!(
            Arc::ptr_eq(&t, &term_b) && m == 22 && id == 3,
            "resolve_active must track the switch to tab 3",
        );
    }

    #[test]
    fn zero_terminal_owner_app_and_meta_classify_before_session_resolution() {
        use aterm_types::control_verbs::{Access, Target};

        let active: ActiveHandle = Arc::new(Mutex::new(None));
        let store = crate::session_store::new_store();
        let subscribers = crate::subscribe::new_registry();
        assert!(resolve_active(&active).is_none());
        assert_eq!(
            control_session::cmd_sessions_store(&store),
            "OK 0\n",
            "Owner fleet meta remains truthful with an empty SessionStore"
        );
        assert_eq!(control_session::cmd_who(&store, &subscribers), "OK 0\n");

        let (selector, verb, rest) = request_head("open app settings /about");
        assert!(selector.is_none());
        assert_eq!(verb, "open");
        assert_eq!(rest, "app settings /about");
        let open = aterm_types::control_verbs::spec(verb).expect("open verb");
        assert_eq!(open.target, Target::App);
        assert_eq!(open.access, Access::Scoped);

        let text = aterm_types::control_verbs::spec("text").expect("legacy text verb");
        assert_eq!(text.target, Target::Session);
        assert_eq!(NO_ACTIVE_TERMINAL, "ERR no active terminal\n");
        assert!(
            resolve_explicit(&store, &Selector::Local(7)).is_none(),
            "an explicit unknown PTY never falls back to hidden front content"
        );
    }

    #[test]
    fn sessionless_front_paste_is_owner_bare_only_and_terminal_routing_is_unchanged() {
        let route = sessionless_front_paste_event(
            "paste",
            || "Paragraph".to_string(),
            None,
            false,
            NativeControlPrincipal::Owner,
        )
        .expect("native-front paste is classified before session resolution")
        .expect("owner may drive its front app");
        assert_eq!(route, InputEvent::Paste("Paragraph".to_string()));

        assert!(
            sessionless_front_paste_event(
                "paste",
                || "terminal".to_string(),
                None,
                true,
                NativeControlPrincipal::Owner,
            )
            .is_none(),
            "a terminal front retains the established session/PTY paste path",
        );
        assert!(
            sessionless_front_paste_event(
                "paste",
                || "explicit".to_string(),
                Some(&Selector::Local(7)),
                false,
                NativeControlPrincipal::Owner,
            )
            .is_none(),
            "an explicit selector remains an exact terminal-session target",
        );
        assert!(
            sessionless_front_paste_event(
                "send",
                || "raw".to_string(),
                None,
                false,
                NativeControlPrincipal::Owner,
            )
            .is_none(),
            "raw send never mutates a native document",
        );
        assert_eq!(
            sessionless_front_paste_event(
                "paste",
                || "denied".to_string(),
                None,
                false,
                NativeControlPrincipal::Edge,
            ),
            Some(Err("ERR denied\n")),
        );
    }

    #[test]
    fn shipping_front_paste_event_drives_editor_minibuffer_then_buffer() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let dir = std::env::temp_dir().join(format!(
            "aterm-control-front-paste-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("front-paste.md");
        std::fs::write(&path, "base\n").unwrap();
        app.open_document_tab(
            crate::native_app::AppKind::Editor,
            // The shipping encoder, not a hand-rolled `format!` — the latter is
            // malformed on Windows (drive letter + backslashes after the
            // authority slot), so this test could not even open its document there.
            &crate::native_document_host::path_to_file_uri(&path).unwrap(),
        )
        .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();

        app.dispatch_native_event(
            wid,
            crate::native_app::AppEvent::EditorCommand(
                crate::native_editor::EditorCommand::IncrementalSearch,
            ),
        )
        .unwrap();
        let search_paste = sessionless_front_paste_event(
            "paste",
            || "base".to_string(),
            None,
            false,
            NativeControlPrincipal::Owner,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            app.input(wid, search_paste, Source::Controller { op: Op::WriteInput },),
            InputOutcome::Ok,
        );
        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor view remains live");
        };
        assert!(matches!(
            state.buffer.as_ref().unwrap().minibuffer,
            crate::native_editor::Minibuffer::Search { ref query, .. } if query == "base"
        ));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "base\n",
            "a focused minibuffer owns the control paste",
        );

        assert_eq!(
            app.input(
                wid,
                InputEvent::Key {
                    key: aterm_types::keyboard::Key::Named(
                        aterm_types::keyboard::NamedKey::Escape,
                    ),
                    mods: aterm_types::keyboard::Modifiers::empty(),
                    base_layout: None,
                    event_type: aterm_types::keyboard::KeyEventType::Press,
                },
                Source::Controller { op: Op::WriteInput },
            ),
            InputOutcome::Ok,
        );
        let buffer_paste = sessionless_front_paste_event(
            "paste",
            || "Paragraph ".to_string(),
            Some(&Selector::SelfTok),
            false,
            NativeControlPrincipal::Owner,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            app.input(wid, buffer_paste, Source::Controller { op: Op::WriteInput },),
            InputOutcome::Ok,
        );
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "Paragraph base\n",
            "without a minibuffer the active editor buffer owns the paste",
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A live, PIPE-backed session: a real `SinkWriter` over the WRITE end of a
    /// `pipe(2)` (so `seam_egress`/`cmd_send` bytes are readable from `rx`), its own
    /// `Terminal`, and a `SessionHandle` registered under `local_id`. The read end
    /// is returned separately so the test can drain the bytes that reached THIS
    /// session's PTY. Mirrors the production wiring (term+sink+ctx are the SAME
    /// `Arc`s the registry hands back) so the cross-session resolve is exercised
    /// for real, not stubbed.
    #[cfg(unix)]
    fn pipe_session(local_id: u64) -> (crate::session_store::SessionHandle, std::fs::File) {
        use crate::session_store::{SessionHandle, SessionState};
        use aterm_session::sink::SinkWriter;
        use aterm_session::{EdgeTable, LaunchNonce, SessionId};
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe(2)");
        let (rd, wr) = (fds[0], fds[1]);
        let rx = unsafe { std::fs::File::from_raw_fd(rd) };
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(wr)),
            edges: std::sync::Mutex::new(EdgeTable::new()),
            turn_lease: std::sync::Mutex::new(None),
            self_id: sid.clone(),
            nonce,
            cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
                80, 24,
            ))),
            temporal: Arc::new(std::sync::Mutex::new(
                crate::temporal::TemporalRecorder::new(),
            )),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: Arc::new(std::sync::Mutex::new(
                crate::turn_ledger::TurnLedger::default(),
            )),
            meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
            app_kitty: std::sync::Mutex::new(crate::app_kitty::AppKittySlot::default()),
            timeline: Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
        });
        let handle = SessionHandle {
            sid,
            nonce,
            local_id,
            parent: None,
            state: SessionState::Alive,
            title: String::new(),
            term: Arc::new(Mutex::new(Terminal::new(24, 80))),
            master: wr, // the write end doubles as the "master" for this headless test
            ctx,
        };
        (handle, rx)
    }

    /// Read whatever bytes are buffered in a pipe WITHOUT blocking when empty:
    /// flips the read end non-blocking, then `read`s once. Used to assert a sink
    /// got (or did NOT get) bytes.
    #[cfg(unix)]
    fn drain_pipe(rx: &std::fs::File) -> Vec<u8> {
        use std::os::fd::AsRawFd;
        let fd = rx.as_raw_fd();
        let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        unsafe { libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) };
        let mut buf = [0u8; 4096];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            buf[..n as usize].to_vec()
        } else {
            Vec::new()
        }
    }

    /// A wedged target must not make the operator's foreground `turn` exceed its
    /// bound before the watcher deadline even starts.  The ordinary cross-input
    /// path is intentionally backpressured; the operator path uses the immediate
    /// sink result, stops before Enter, and leaves no rejected paste in the spill
    /// drainer for surprise delivery after the call returns.
    #[test]
    #[cfg(unix)]
    fn operator_turn_refuses_full_pty_promptly_without_late_input() {
        use std::io::Read as _;

        let store = crate::session_store::new_store();
        let (handle, mut reader) = pipe_session(71);
        store.write().unwrap().register(handle.clone());
        aterm_pty::set_nonblocking(handle.master, true).expect("nonblocking test master");
        handle.ctx.sink.note_master_nonblocking(true);

        let mut filled = 0_usize;
        loop {
            match aterm_pty::write_some(handle.master, &[b'.'; 4096]) {
                Ok(n) if n > 0 => filled += n,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                other => panic!("unexpected pipe fill result: {other:?}"),
            }
        }
        assert!(filled > 0, "pipe reached real backpressure");

        let paste = |text: &str| {
            operator_input(
                &handle.term,
                &handle.ctx,
                Some(InputEvent::Paste(control_input::paste_text(text))),
            ) == Delivery::Full
        };
        let press = |_: &str| panic!("BusyZero paste must stop before Enter");
        let started = std::time::Instant::now();
        let response = control_session::cmd_turn(
            &handle.term,
            &store,
            handle.local_id,
            "timeout=9000 -- REJECTED",
            &subscribe::new_registry(),
            &handle.ctx,
            &control_session::TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert_eq!(response, "ERR paste delivery failed\n");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "foreground turn waited behind a full PTY: {:?}",
            started.elapsed()
        );

        let mut old = vec![0_u8; filled];
        reader.read_exact(&mut old).expect("drain original fill");
        assert!(old.iter().all(|byte| *byte == b'.'));
        assert!(
            drain_pipe(&reader).is_empty(),
            "the refused operator paste must not arrive after the turn returned"
        );
    }

    #[test]
    fn operator_input_diagnostics_retain_zero_vs_partial_and_stage() {
        let paste_zero = OperatorInputFailure {
            stage: OperatorInputStage::Paste,
            delivery: Delivery::BusyZero,
        }
        .response();
        assert_eq!(
            operator_result_summary(&paste_zero),
            "turn input=paste outcome=busy-zero"
        );

        let submit_partial = OperatorInputFailure {
            stage: OperatorInputStage::Submit,
            delivery: Delivery::PartialInDoubt { accepted: 1 },
        }
        .response();
        assert_eq!(
            operator_result_summary(&submit_partial),
            "turn input=submit outcome=partial accepted=1"
        );
        assert_eq!(
            operator_result_summary("ERR operator input submit partial accepted=1 leaked"),
            "turn result unavailable",
            "non-canonical fields must not enter the durable summary"
        );
    }

    /// THE follow-up's core claim: a cross-session input verb resolves the
    /// `@<selector>` TARGET the SAME way `send`/`feed` do and drives the source-blind
    /// seam against THAT session — bytes land in the TARGET's sink, never self's,
    /// and the self path is unchanged. Round-trips `key`/`feed`/`select`/`send`.
    #[test]
    #[cfg(unix)]
    fn cross_session_input_reaches_target_sink_not_self() {
        use crate::session_store::new_store;
        let store = new_store();
        let (h_self, self_rx) = pipe_session(1);
        let (h_target, target_rx) = pipe_session(2);
        // Both terms are left in the default keyboard mode (a `key enter` is a bare
        // CR). Mouse tracking is exercised separately in
        // `cross_session_mouse_reports_or_scrolls_target`.
        store.write().unwrap().register(h_self.clone());
        store.write().unwrap().register(h_target.clone());

        let self_tuple: Target = (
            h_self.term.clone(),
            h_self.master,
            h_self.local_id,
            h_self.ctx.clone(),
        );

        // Resolve `@2` (the target's local id) EXACTLY as `handle()` does.
        let sel = Selector::parse("2");
        assert!(matches!(sel, Selector::Local(2)), "@2 parses to Local(2)");
        let (term, master, session, ctx) =
            resolve_target(&self_tuple, &store, &sel).expect("@2 resolves");
        assert!(
            Arc::ptr_eq(&term, &h_target.term),
            "resolved the TARGET term"
        );
        assert_eq!(session, 2);
        let _ = master;
        let ctx: &SessionCtx = &ctx;

        // `key enter` cross-session: CR reaches the TARGET sink, nothing to self.
        assert_eq!(cross_input(&term, ctx, parse_key("enter"), "ERR\n"), "OK\n");
        assert_eq!(
            drain_pipe(&target_rx),
            b"\r",
            "key bytes hit the TARGET pty"
        );
        assert!(
            drain_pipe(&self_rx).is_empty(),
            "self pty must be untouched"
        );

        // `feed 03` (Ctrl-C) cross-session: `cmd_feed(&ctx.sink, ..)` is ALREADY the
        // resolved-target path — assert it stays correct alongside the new arms.
        assert_eq!(cmd_feed(&ctx.sink, "03"), "OK 1 bytes\n");
        assert_eq!(
            drain_pipe(&target_rx),
            b"\x03",
            "feed bytes hit the TARGET pty"
        );
        assert!(drain_pipe(&self_rx).is_empty(), "feed must not touch self");

        // `send` (the other always-cross writer) still writes to the resolved sink.
        // `send` is RAW (no implicit CR unless a literal trailing `\n` is given).
        let _ = cmd_send(&ctx.sink, "ls");
        assert_eq!(
            drain_pipe(&target_rx),
            b"ls",
            "send bytes hit the TARGET pty"
        );
        assert!(drain_pipe(&self_rx).is_empty(), "send must not touch self");

        // `select` is ALREADY cross-correct and is left untouched (it has no
        // `is_cross` guard): it mutates the RESOLVED `term`'s selection and repaints
        // by the RESOLVED `session` id. It used to be unreachable here — it took an
        // `EventLoopProxy`, which is not buildable off the main thread — but a
        // proxy-less `GuiHost` now drives the REAL verb against the resolved target
        // and reads it back with `cmd_selection`, proving the resolved target is the
        // one selected (and that it emits no pty bytes, the read-side contract).
        let reg = subscribe::new_registry();
        let host = GuiHost::new(session, &term, None, &reg);
        term_lock(&term).process(b"hello world");
        assert_eq!(cmd_select(&host, session, "0 0 0 4"), "OK\n");
        let reply = cmd_selection(&host, session);
        assert!(
            reply.starts_with("OK ") && reply.contains("hello"),
            "TARGET selection: {reply}"
        );
        assert!(
            drain_pipe(&target_rx).is_empty(),
            "select must not write pty bytes"
        );
        assert_eq!(session, 2, "select repaints by the RESOLVED target id");
    }

    /// Cross-session `resize` applies `echo_to_window:false` to the TARGET term+pty
    /// ONLY (never the active window). It exercises ALL THREE per-session artifacts
    /// `apply_term_resize` touches (main.rs:2453-2463): the engine grid, the PTY
    /// winsize (over a REAL `openpty` master, asserted via `TIOCGWINSZ` — a pipe fd
    /// would make the `TIOCSWINSZ` ioctl silently no-op), and the target's own
    /// asciicast geometry record (so a cross resize is indistinguishable from a self
    /// resize in the target's `cast` timeline). Out-of-range requests reuse the
    /// shared `parse_resize` errors and mutate nothing.
    #[test]
    #[cfg(unix)]
    fn cross_session_resize_targets_term_only() {
        use aterm_session::sink::SinkWriter;
        use aterm_session::{EdgeTable, LaunchNonce, SessionId};

        // A REAL pty pair so `aterm_pty::resize`'s TIOCSWINSZ actually takes effect
        // (a plain pipe is not a tty and the ioctl would no-op).
        let mut master = 0i32;
        let mut slave = 0i32;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0,
            "openpty"
        );
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(master)),
            edges: std::sync::Mutex::new(EdgeTable::new()),
            turn_lease: std::sync::Mutex::new(None),
            self_id: sid,
            nonce,
            cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
                80, 24,
            ))),
            temporal: Arc::new(std::sync::Mutex::new(
                crate::temporal::TemporalRecorder::new(),
            )),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: Arc::new(std::sync::Mutex::new(
                crate::turn_ledger::TurnLedger::default(),
            )),
            meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
            app_kitty: std::sync::Mutex::new(crate::app_kitty::AppKittySlot::default()),
            timeline: Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
        });
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let before = ctx.cast.lock().unwrap().event_count();

        assert_eq!(cross_resize(&term, master, &ctx, 0, None, "10 40"), "OK\n");
        // (1) engine grid.
        {
            let t = term_lock(&term);
            assert_eq!((t.rows(), t.cols()), (10, 40), "TARGET grid resized");
        }
        // (2) PTY winsize — the half a pipe-fd test could not prove.
        {
            let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::ioctl(master, libc::TIOCGWINSZ, &mut ws) },
                0,
                "TIOCGWINSZ"
            );
            assert_eq!(
                (ws.ws_row, ws.ws_col),
                (10, 40),
                "TARGET pty winsize updated"
            );
        }
        // (3) asciicast record — one new `[t,"r","40x10"]` on the target timeline,
        // matching the self path's `apply_term_resize`.
        assert_eq!(
            ctx.cast.lock().unwrap().event_count(),
            before + 1,
            "cross resize recorded into the TARGET's cast"
        );

        // Out-of-range reuses parse_resize's exact string; nothing is mutated (no
        // extra cast event, grid unchanged).
        assert_eq!(
            cross_resize(&term, master, &ctx, 0, None, "65535 65535"),
            "ERR out of range\n"
        );
        {
            let t = term_lock(&term);
            assert_eq!(
                (t.rows(), t.cols()),
                (10, 40),
                "grid unchanged after reject"
            );
        }
        assert_eq!(
            ctx.cast.lock().unwrap().event_count(),
            before + 1,
            "rejected resize records nothing"
        );

        unsafe {
            libc::close(slave);
            libc::close(master);
        }
    }

    /// Cross-session `mouse` against the TARGET, BOTH tracking states (the
    /// `cross_mouse_apply` core, proxy-free):
    ///   * tracking ON  -> the seam writes a mouse REPORT to the TARGET sink (and
    ///     nothing to self); `cross_mouse_apply` returns `Ok(false)` (no viewport
    ///     move, so no repaint).
    ///   * tracking OFF -> a WHEEL falls back to `scroll_display` on the TARGET term
    ///     (viewport moves, `Ok(true)` = repaint) and emits NO pty bytes; a plain
    ///     PRESS is a deliberate no-op (`Ok(false)`, sink empty, offset unchanged).
    #[test]
    #[cfg(unix)]
    fn cross_session_mouse_reports_or_scrolls_target() {
        let (_h_self, self_rx) = pipe_session(1);
        let (h_target, target_rx) = pipe_session(2);
        let term = &h_target.term;
        let ctx: &SessionCtx = &h_target.ctx;

        // Build scrollback so a wheel can actually move the viewport.
        for i in 0..60 {
            term_lock(term).process(format!("line {i}\r\n").as_bytes());
        }
        drain_pipe(&target_rx); // engine query replies, if any, are not sink writes — clear anyway

        // ── Tracking ON (SGR mouse, DEC 1000 + 1006) ──
        term_lock(term).process(b"\x1b[?1000h\x1b[?1006h");
        assert!(
            term_lock(term).mouse_tracking_enabled(),
            "DEC 1000 enabled tracking"
        );
        assert_eq!(
            cross_mouse_apply(term, ctx, "press left 1 1"),
            Ok(false),
            "report, no viewport move"
        );
        let report = drain_pipe(&target_rx);
        assert!(
            !report.is_empty() && report.starts_with(b"\x1b["),
            "SGR press report to TARGET: {report:?}"
        );
        assert!(
            drain_pipe(&self_rx).is_empty(),
            "self sink untouched by a cross mouse"
        );

        // ── Tracking OFF (DEC 1000 / 1006 reset) ──
        term_lock(term).process(b"\x1b[?1006l\x1b[?1000l");
        assert!(!term_lock(term).mouse_tracking_enabled(), "tracking reset");
        term_lock(term).scroll_to_bottom();
        assert_eq!(term_lock(term).grid().display_offset(), 0, "at live tail");

        // Wheel-up: scroll_display fallback moves the TARGET viewport into history,
        // emits no pty bytes, and asks for a repaint.
        assert_eq!(
            cross_mouse_apply(term, ctx, "wheelup left 2 4 count=3"),
            Ok(true),
            "wheel => repaint"
        );
        assert!(
            term_lock(term).grid().display_offset() > 0,
            "wheel moved TARGET viewport"
        );
        assert!(
            drain_pipe(&target_rx).is_empty(),
            "wheel fallback emits no pty bytes"
        );

        // A plain press under tracking-off: deliberate no-op (no selection UI for a
        // background tab) — sink empty, offset unchanged, no repaint.
        let off_before = term_lock(term).grid().display_offset();
        assert_eq!(
            cross_mouse_apply(term, ctx, "press left 1 1"),
            Ok(false),
            "press no-op"
        );
        assert!(
            drain_pipe(&target_rx).is_empty(),
            "press fallback emits no pty bytes"
        );
        assert_eq!(
            term_lock(term).grid().display_offset(),
            off_before,
            "press did not move viewport"
        );
    }

    /// Cross-session `scroll` moves the TARGET term's viewport directly (no seam, no
    /// pty bytes) and reports `OK <offset> <max>` — the SAME wire shape as the self
    /// path. With history present, `scroll top` jumps to the oldest line.
    #[test]
    #[cfg(unix)]
    fn cross_session_scroll_moves_target_viewport() {
        let (h_target, rx) = pipe_session(2);
        // Generate scrollback: print more lines than the 24-row screen.
        for i in 0..60 {
            term_lock(&h_target.term).process(format!("line {i}\r\n").as_bytes());
        }
        let reply = cross_scroll(&h_target.term, "top");
        assert!(reply.starts_with("OK "), "scroll reply shape: {reply}");
        assert!(
            term_lock(&h_target.term).grid().display_offset() > 0,
            "viewport moved into history"
        );
        assert!(drain_pipe(&rx).is_empty(), "scroll emits no pty bytes");
        // `scroll bottom` returns to the live tail (offset 0).
        let _ = cross_scroll(&h_target.term, "bottom");
        assert_eq!(
            term_lock(&h_target.term).grid().display_offset(),
            0,
            "back to live bottom"
        );
    }

    /// The exact bytes the `paste` verb puts on the wire: the seam applies
    /// `format_paste` to the verb's `paste_text(rest)` transform. (Phase 0.5: the
    /// verb itself now posts an `InputEvent::Paste` to the seam, which a headless
    /// unit test can't drive — but the OBSERVABLE bytes are exactly this, so we
    /// assert on the same `format_paste` output the seam produces.)
    fn paste_to_pipe(term: &Arc<Mutex<Terminal>>, rest: &str) -> Vec<u8> {
        term_lock(term).format_paste(&paste_text(rest))
    }

    /// A paste planted with ESC[201~ must not terminate the bracket guard:
    /// the engine sanitizer strips ESC, so the only ESC[201~ on the wire is
    /// the final guard and the planted "[201~" is inert text.
    #[test]
    fn paste_verb_cannot_escape_bracket_guard() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term.lock().unwrap().process(b"\x1b[?2004h");
        let got = paste_to_pipe(&term, "safe\x1b[201~rm -rf ~");
        assert_eq!(got, b"\x1b[200~safe[201~rm -rf ~\x1b[201~");
    }

    /// The literal trailing `\n` still ends the paste with a line break,
    /// which the engine sends as CR exactly like a real paste.
    #[test]
    fn paste_verb_trailing_newline_becomes_cr() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        assert_eq!(paste_to_pipe(&term, "echo hi\\n"), b"echo hi\r");
    }

    /// Mint a fresh `SessionCtx` for the auth/gate tests (no real PTY needed; the
    /// sink wraps a harmless `-1` fd and is never written by these tests).
    fn test_ctx() -> Arc<crate::SessionCtx> {
        use aterm_session::sink::SinkWriter;
        use aterm_session::{EdgeTable, LaunchNonce, SessionId};
        Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(-1)),
            edges: std::sync::Mutex::new(EdgeTable::new()),
            turn_lease: std::sync::Mutex::new(None),
            self_id: SessionId::generate(),
            nonce: LaunchNonce::generate(),
            cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
                80, 24,
            ))),
            temporal: Arc::new(std::sync::Mutex::new(
                crate::temporal::TemporalRecorder::new(),
            )),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: Arc::new(std::sync::Mutex::new(
                crate::turn_ledger::TurnLedger::default(),
            )),
            meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
            app_kitty: std::sync::Mutex::new(crate::app_kitty::AppKittySlot::default()),
            timeline: Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
        })
    }

    /// The exact decision the op-scope gate at the top of `handle()` makes. Kept in
    /// lockstep with the inline `match (scope, required_op(verb))` so the gate is
    /// testable without an `EventLoopProxy` (which can't be built off the main
    /// thread): the deny path returns BEFORE any proxy/queue use, so the decision is
    /// all that matters. Mirrors the three exhaustive arms verbatim.
    fn gate_allows(scope: Scope, verb: &str, active: &SessionCtx) -> bool {
        // Mirrors the SELF-path op-scope gate in `handle()`: Owner passes everything
        // (no lookup); an Edge is permitted iff its token is authorized against the
        // NOW-ACTIVE session for the verb's op (`cross_session_authorized`) — NOT
        // op-match alone, so an edge cannot drive a session it was never granted on
        // after the active handle swings (tab/window switch).
        matches!(scope, Scope::Owner) || cross_session_authorized(scope, verb, active)
    }

    /// An `Edge` scope carrying a throwaway, UNGRANTED token. Since authority is
    /// re-derived from the token per request (`decide_edge`/`authorize`), an ungranted
    /// token is denied for EVERY op against any real session — so the deny-side / body-
    /// guard tests need no op (the scope no longer caches one).
    fn edge() -> Scope {
        Scope::Edge(EdgeToken::generate())
    }

    /// An `Edge` scope whose token is GRANTED on `ctx` (so the gate /
    /// `cross_session_authorized` permit it against THAT session for `op` verbs) —
    /// mirrors a real connection whose token was authorized against its session at
    /// connect time.
    fn edge_granted(op: Op, ctx: &SessionCtx) -> Scope {
        let tok = {
            let mut tbl = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            tbl.grant(
                SessionId::new("s-test-controller"),
                ctx.self_id.clone(),
                op,
                ctx.nonce,
            )
        };
        Scope::Edge(tok)
    }

    /// SINGLE SOURCE OF TRUTH, part 1: every verb the DISPATCH router handles must
    /// be in the [`VERBS`] table — so a verb cannot be added to the router without
    /// being classified (and, by part 2, documented). Scrapes the verb string
    /// literals out of the dispatch `match` arms and asserts each resolves in the
    /// table. Guards against the exact drift the audit found (a router arm with no
    /// op-class / catalog entry).
    #[test]
    fn every_dispatched_verb_is_in_the_table() {
        let src = include_str!("control.rs");
        // The dispatch match body. ANCHOR ON `let resp = match verb {` — the
        // ROUTER's match, closing with `};` because it is a let-binding.
        //
        // REGRESSION (this test was vacuous): the anchor used to be the bare
        // `    match verb {`, whose FIRST occurrence in this file is
        // `escalated_op`'s match (~line 411), not the router (~line 3841). The
        // scrape therefore walked a handful of already-classified escalation
        // arms and never saw the dispatch table at all, so a router arm missing
        // from `VERBS` would have sailed straight through. Keep both the anchor
        // and the `\n    };\n` terminator exact.
        let body = src
            .split_once("    let resp = match verb {")
            .and_then(|(_, r)| r.split_once("\n    };\n"))
            .map(|(m, _)| m)
            .expect("dispatch match body");
        // Non-vacuity: the router body must actually contain known dispatch arms.
        // Without this, a future anchor drift silently empties the scrape again.
        for sentinel in ["\"text\" =>", "\"screen\" =>", "\"cursor\" =>"] {
            assert!(
                body.contains(sentinel),
                "dispatch scrape lost its anchor — {sentinel} not found in the \
                 scraped body (the router match moved or was renamed)"
            );
        }
        let known = |t: &str| aterm_types::control_verbs::spec(t).is_some();
        // Verbs matched via `"x" if …` / `"x" =>` / `"x" | "y"` arms. Sub-forms like
        // `image read` / `cast frames` are matched on their base verb (image/cast),
        // so scraping every quoted lowercase token and checking table membership is
        // exact — a base verb is present, a sub-keyword (`read`/`frames`) is not a
        // verb and is excluded by not being a standalone arm token we assert on.
        for arm in body.split("=>") {
            // Only the LEFT of each `=>` holds the arm's verb literals.
            for tok in arm.split('"').skip(1).step_by(2) {
                // A verb literal is lowercase-with-dashes and not an argument word.
                let is_sub_keyword = matches!(tok, "read" | "frames" | "set" | "unset" | "new");
                if tok.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    && !tok.is_empty()
                    && !is_sub_keyword
                    && !known(tok)
                    // Only assert on tokens that are an actual dispatch arm (`"x" =>`),
                    // not arg keywords that merely appear inside a guard expression.
                    && body.contains(&format!("\"{tok}\" =>"))
                {
                    panic!("dispatch verb {tok:?} is missing from the VERBS table");
                }
            }
        }
    }

    /// SINGLE SOURCE OF TRUTH, part 1c: every `cmd_<verb>_json` handler the
    /// server defines must have its verb listed in
    /// [`aterm_types::control_verbs::JSON_CAPABLE_VERBS`], and vice versa.
    ///
    /// json-capability is the ONE framing input that cannot be derived from the
    /// verb table — it is a property of the server's handler, not of the row — so
    /// the list is a hand-maintained duplicate, and duplicates drift. `metrics`
    /// drifted: `cmd_metrics_json` wrote the `json_ok` `OK 1\n<body>` (Lines)
    /// shape while the table still framed `metrics --json` as Status, so the
    /// client consumed the header and SILENTLY DROPPED the JSON body. This test
    /// is what binds the two ends.
    ///
    /// Scrapes handler NAMES rather than `json_ok` call sites: a name is a stable
    /// contract (`cmd_<verb>_json`), while a call site can sit behind a helper.
    /// Irregular names declare their verb in `IRREGULAR` below.
    #[test]
    fn json_ok_sites_match_the_json_capable_verbs() {
        // Handlers whose name does not literally contain their verb.
        const IRREGULAR: &[(&str, &str)] = &[
            ("cmd_screen_styled_json", "screen"),
            // `edges`/`grants` are aliases served by ONE handler.
            ("cmd_edges_json", "edges"),
        ];
        // `*_json` helpers that are serializers, not verb handlers.
        const NOT_HANDLERS: &[&str] = &[
            "serialize_dims_json",
            "styled_image_json",
            // Sub-object of the styled frame (`screen`/`cells`), like the two
            // above — there is no `styled_selection` verb.
            "styled_selection_json",
            "write_styled_cell_json",
        ];

        let sources = [
            include_str!("control_query.rs"),
            include_str!("control_session.rs"),
            // The selection verbs live in `aterm-control` now; scrape them THERE
            // or `blocks` becomes an unserved JSON_CAPABLE_VERBS entry and the
            // non-vacuity floor below goes soft.
            include_str!("../../aterm-control/src/selection.rs"),
        ];
        let mut found: Vec<String> = Vec::new();
        for src in sources {
            for line in src.lines() {
                let Some(rest) = line.split_once("fn ").map(|(_, r)| r) else {
                    continue;
                };
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '_' || c.is_ascii_digit())
                    .collect();
                if name.ends_with("_json") && !NOT_HANDLERS.contains(&name.as_str()) {
                    found.push(name);
                }
            }
        }
        found.sort();
        found.dedup();
        assert!(
            found.len() >= 6,
            "scrape found only {found:?} — the `fn <name>_json` shape moved and this              guard went vacuous"
        );

        for name in &found {
            let verb = IRREGULAR
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| (*v).to_string())
                .unwrap_or_else(|| {
                    name.trim_start_matches("cmd_")
                        .trim_end_matches("_json")
                        .to_string()
                });
            assert!(
                aterm_types::control_verbs::JSON_CAPABLE_VERBS.contains(&verb.as_str()),
                "handler {name} serves verb {verb:?}, which is NOT in JSON_CAPABLE_VERBS \u{2014}                  `{verb} --json` will frame as Status and the client will drop the body"
            );
        }

        // …and nothing is listed that no handler serves (a stale entry would make
        // a Status-framed verb claim Lines under --json).
        for v in aterm_types::control_verbs::JSON_CAPABLE_VERBS {
            let served = *v == "grants" // alias of `edges`
                || found.iter().any(|n| {
                    IRREGULAR.iter().any(|(hn, hv)| hn == n && hv == v)
                        || n.trim_start_matches("cmd_").trim_end_matches("_json") == *v
                });
            assert!(
                served,
                "JSON_CAPABLE_VERBS lists {v:?} but no `*_json` handler serves it"
            );
        }
    }

    /// The `--json` sentence in the help header must NAME every json-capable
    /// verb. It was a fifth hand-maintained replica of that set and had already
    /// drifted (`metrics` was missing), so it is now generated — this pins that.
    #[test]
    fn help_header_names_every_json_capable_verb() {
        let help = cmd_help();
        for v in aterm_types::control_verbs::JSON_CAPABLE_VERBS {
            assert!(
                help.contains(v),
                "help header omits json-capable verb {v:?} — the --json sentence drifted"
            );
        }
        assert!(
            help.contains("metrics"),
            "non-vacuity: the regression verb must appear"
        );
    }

    /// SINGLE SOURCE OF TRUTH, part 1b: some verbs are INTERCEPTED in the serve
    /// loop BEFORE the dispatch `match verb {` — `dial <name>` relays the
    /// connection, `subscribe` flips it to push mode, `feed-bin` reads a binary
    /// frame off the same stream — so part 1's dispatch scrape cannot see them.
    /// Scrape each interception helper's verb literal and assert table
    /// membership, so a serve-loop verb cannot ship without a [`VERBS`] row (and
    /// therefore a generated catalog line) either. A NEW interception must be
    /// added to the helper list here — that is the price of bypassing dispatch.
    #[test]
    fn every_serve_loop_intercepted_verb_is_in_the_table() {
        let src = include_str!("control.rs");
        let known = |t: &str| aterm_types::control_verbs::spec(t).is_some();
        let helpers = [
            "fn try_net_dial",
            "fn is_subscribe_line",
            "fn binary_frame_verb",
        ];
        for helper in helpers {
            // The helper's body: from its first definition (the real fn — this
            // test's own literals sit later in the file) to the column-0 `}`.
            let body = src
                .split_once(helper)
                .and_then(|(_, r)| r.split_once("\n}\n"))
                .map(|(b, _)| b)
                .unwrap_or_else(|| panic!("serve-loop interception helper {helper:?} not found"));
            for tok in body.split('"').skip(1).step_by(2) {
                // A verb literal may carry a trailing space (`strip_prefix("dial ")`);
                // multi-word strings (error messages) are not verb literals.
                let tok = tok.trim();
                if !tok.is_empty()
                    && tok.chars().all(|c| c.is_ascii_lowercase() || c == '-')
                    && !known(tok)
                {
                    panic!("serve-loop intercepted verb {tok:?} is missing from the VERBS table");
                }
            }
        }
    }

    /// SINGLE SOURCE OF TRUTH, part 2: the help CATALOG and the [`VERBS`] table
    /// agree — every catalog line's verb is in the table, and every real (op-
    /// classed) table verb is documented in the catalog. Binds the two so neither
    /// can drift from the other (the audit's "three places" reduced to one truth +
    /// two bound projections).
    #[test]
    fn catalog_and_verb_table_agree() {
        use aterm_types::control_verbs::{OpClass, VERBS};
        let catalog = super::cmd_help();
        let known: std::collections::HashSet<&str> = VERBS.iter().map(|s| s.name).collect();
        // Every catalog body line's leading token is a known verb (skip comments,
        // the header, and blank/continuation lines).
        let mut documented: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for line in catalog.lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') || line.starts_with("OK ") {
                continue;
            }
            let verb = line.split_whitespace().next().unwrap_or("");
            // Catalog lines lead with the verb (or `a | b` alt forms — check the first).
            if known.contains(verb) {
                documented.insert(verb);
            } else if verb.chars().all(|c| c.is_ascii_lowercase() || c == '-') && !verb.is_empty() {
                panic!("catalog documents {verb:?} which is not in the VERBS table");
            }
        }
        // Every op-classed verb (a real, gated verb) must be documented. Owner-only/
        // meta verbs (op None) are documented under grouped lines, so only assert the
        // op-classed set to keep the check precise.
        // The catalog is GENERATED from VERBS, so every op-classed verb is present by
        // construction; assert it to lock the generation in.
        for spec in VERBS {
            if spec.op != OpClass::Owner {
                assert!(
                    catalog.contains(spec.name),
                    "verb {:?} is in the table but absent from the generated catalog",
                    spec.name
                );
            }
        }
    }

    /// `required_op` is the single source of truth for which `Op` each verb needs;
    /// the design 7.2 read != write != signal split must hold exactly. TOTAL binding:
    /// EVERY verb in [`VERBS`] is partitioned by the `Op` `required_op` returns and each
    /// partition is pinned to an explicit expected set (below). So no verb can SILENTLY
    /// change op-class: shifting a verb's class moves it between buckets and fails a set
    /// assert; adding a table verb without listing it here fails the exhaustiveness
    /// count. This closes the gap where video/history/metrics/hover/
    /// ready/await/subscribe/family/edges/grants were previously unpinned.
    #[test]
    fn required_op_classifies_each_verb() {
        use aterm_types::control_verbs::VERBS;
        // The verbs of the table whose `required_op` is exactly `want`, sorted.
        let by_op = |want: Option<Op>| -> Vec<&'static str> {
            let mut v: Vec<&str> = VERBS
                .iter()
                .map(|s| s.name)
                .filter(|n| required_op(n) == want)
                .collect();
            v.sort_unstable();
            v
        };
        let assert_set = |want: Option<Op>, mut expected: Vec<&'static str>, label: &str| {
            expected.sort_unstable();
            assert_eq!(by_op(want), expected, "{label}");
        };

        // READ-side (`ReadScreen`): every observer + the controller's own view-state
        // controls (scroll/select are viewport nav — nothing leaves the process). Note
        // `who` is Read-op yet Owner-GATED in dispatch (op-class ⟂ scope-gate), and
        // `copy` is NOT here — it is the clipboard-exfil boundary (ClipboardWrite).
        assert_set(
            Some(Op::ReadScreen),
            vec![
                "text",
                "screen",
                "line",
                "lines",
                "cell",
                "cursor",
                "dims",
                "modes",
                "custody",
                "title",
                "cwd",
                "colors",
                "search",
                "selection",
                "blocks",
                "blocktext",
                "image",
                "window",
                "video",
                "chrome",
                "panes",
                "controls",
                "inspect",
                "cast",
                "temporal",
                "history",
                // `meta` is base-Read (the bare readout); its `set`/`unset`
                // sub-forms escalate to WriteInput via `escalated_op` (pinned in
                // `escalated_op_fences_invoke_and_open_indirect_seams`).
                "meta",
                "timeline",
                // `status` OBSERVES the local status classifier. It has no write
                // sub-form at all — the only things that move a status are the
                // session itself and `settings set tab_status …` — so unlike
                // `meta` it takes no `escalated_op` entry.
                "status",
                "metrics",
                // `tone` OBSERVES the mood classifier; the knob it reports is
                // rewritten through `settings` (ConfigWrite), never here.
                "tone",
                // `trail` OBSERVES the cursor-trail engine — the licence
                // verdict ring and (`trail status`) its standing state. It
                // has no write form at all (both are engine-written diagnostic
                // state beside decisions already made; the knobs it reports are
                // rewritten through `settings`, never here).
                "trail",
                "scroll",
                "select",
                "ready",
                "await",
                "wait",
                "subscribe",
                "who",
                "family",
                "edges",
                "grants",
            ],
            "ReadScreen set (every observer/view-state verb, incl. Owner-gated `who`)",
        );
        // WRITE-side (`WriteInput`): the human input vocabulary + app-drive verbs.
        // `open` MUTATES app-level UI (pops an aux surface), so a read-only edge cannot
        // `open` a window; `invoke` fires a menu action. (Their clipboard/config
        // ESCALATIONS are fenced by `escalated_op`, tested separately.)
        assert_set(
            Some(Op::WriteInput),
            vec![
                "send",
                "key",
                "ctrl",
                "feed",
                "feed-bin",
                "paste-bin",
                "mouse",
                "paste",
                "resize",
                "focus",
                "tab",
                "open",
                "invoke",
                "rain",
                "hover",
                "turn",
                "spawn",
                "close",
                "lease",
                "act",
            ],
            "WriteInput set (input vocabulary + app-drive verbs)",
        );
        // The out-of-band signal class, and the two FINE ops carved out of Read/Write:
        // `settings` rewrites durable config, `copy` exfiltrates to the OS clipboard —
        // neither reachable by an inherited read/write edge (those carry only base ops).
        assert_set(Some(Op::Signal), vec!["signal"], "Signal set");
        assert_set(Some(Op::ConfigWrite), vec!["settings"], "ConfigWrite set");
        assert_set(Some(Op::ClipboardWrite), vec!["copy"], "ClipboardWrite set");
        // None (OpClass::Owner): privilege/identity + build-meta verbs, gated before the
        // op check. Every one default-DENIES for an Edge (no op edge reaches them).
        assert_set(
            None,
            vec![
                "version",
                "update",
                "help",
                "verbs",
                "operator",
                "operator-propose-bin",
                "sessions",
                "whoami",
                "grant",
                "revoke",
                // The session-connection verbs (design §6): connection-grain
                // authority twins of grant/revoke, plus the aggregated graph
                // and the raise act — all Owner-only, no op edge reaches them.
                "connect",
                "disconnect",
                "flows",
                "raise",
                "dial",
                "dial-list",
                "dial-token",
            ],
            "None set (Owner-only privilege + build-meta verbs)",
        );

        // EXHAUSTIVE: the pinned partitions cover the WHOLE table (no verb left
        // unclassified). 39 read + 21 write + 1 signal + 1 config + 1 clip + 11 none
        // = 74 — but the NUMBERS here are prose; the machine-checked truth is the
        // set-equality assertions below plus `pinned == VERBS.len()`.
        let pinned = by_op(Some(Op::ReadScreen)).len()
            + by_op(Some(Op::WriteInput)).len()
            + by_op(Some(Op::Signal)).len()
            + by_op(Some(Op::ConfigWrite)).len()
            + by_op(Some(Op::ClipboardWrite)).len()
            + by_op(None).len();
        assert_eq!(
            pinned,
            VERBS.len(),
            "every table verb must fall in exactly one pinned op-class partition",
        );

        // `who` is Read-op yet Owner-GATED — the orthogonality this test relies on.
        assert!(
            aterm_types::control_verbs::is_owner_only("who"),
            "who is owner-gated in the table"
        );
        // Unknown verbs default-deny (absent from the table => None).
        assert_eq!(required_op("bogus"), None, "unknown verb default-deny");
        assert_eq!(required_op(""), None, "empty verb default-deny");
    }

    /// INDIRECT-SEAM FENCE: `invoke <clipboard/config action>` and `open prefs|settings`
    /// ESCALATE their op on the argument, so a plain WriteInput edge cannot reach the
    /// fenced fine ops through them. A WriteInput edge is DENIED; Owner and an
    /// explicitly-granted ClipboardWrite/ConfigWrite edge are ALLOWED.
    /// The direct `update` verb face: `status` is a readable AnyScopeMeta pure-read,
    /// but `check` (stages) and `apply` (re-execs) are owner-only mutations — the
    /// twin of the OwnerOnly fence on `invoke ApplyUpdate`/`open update`. Closes the
    /// third-pass escalation path (c): a ReadScreen child edge `update check`+`update
    /// apply`-ing its way to a re-exec.
    #[test]
    fn update_check_and_apply_are_owner_only_subcommands() {
        assert!(update_is_owner_only_subcmd("check"));
        assert!(update_is_owner_only_subcmd("apply"));
        assert!(update_is_owner_only_subcmd("  apply  "));
        // Reads stay any-scope so an in-session observer can SEE update state.
        assert!(!update_is_owner_only_subcmd(""));
        assert!(!update_is_owner_only_subcmd("status"));
        assert!(!update_is_owner_only_subcmd("bogus"));
    }

    #[test]
    fn escalated_op_fences_invoke_and_open_indirect_seams() {
        use Escalation::{Op as EOp, OwnerOnly};
        // The argument classification the dispatch reads.
        assert_eq!(
            escalated_op("invoke", "Copy"),
            Some(EOp(Op::ClipboardWrite))
        );
        assert_eq!(
            escalated_op("invoke", "Paste"),
            Some(EOp(Op::ClipboardWrite))
        );
        assert_eq!(
            escalated_op("invoke", "SelectAll"),
            Some(EOp(Op::ClipboardWrite))
        );
        assert_eq!(
            escalated_op("invoke", "ToggleSettings"),
            Some(EOp(Op::ConfigWrite))
        );
        assert_eq!(
            escalated_op("invoke", "Preferences"),
            Some(EOp(Op::ConfigWrite))
        );
        // The palette GATEWAY and the staged-update RE-EXEC twins are OWNER-ONLY — no
        // fine op expresses "only the god token", so even a granted fine-op edge is denied.
        assert_eq!(escalated_op("invoke", "OpenPalette"), Some(OwnerOnly));
        assert_eq!(escalated_op("invoke", "SoftwareUpdate"), Some(OwnerOnly));
        assert_eq!(escalated_op("invoke", "ApplyUpdate"), Some(OwnerOnly));
        // The connected-spawn presets MINT standing session-connection authority
        // (design §5.3/§6) — their `invoke` twins carry the same OwnerOnly fence
        // as the `spawn connected=` arm below.
        assert_eq!(escalated_op("invoke", "NewControlledWindow"), Some(OwnerOnly));
        assert_eq!(escalated_op("invoke", "NewControlledTab"), Some(OwnerOnly));
        assert_eq!(escalated_op("invoke", "NewControllerWindow"), Some(OwnerOnly));
        assert_eq!(escalated_op("invoke", "NewControllerTab"), Some(OwnerOnly));
        // The context-menu picker/map rows (§2.3): the `invoke` twins of the
        // `open connections` OwnerOnly arm below.
        assert_eq!(escalated_op("invoke", "ConnectToSession"), Some(OwnerOnly));
        assert_eq!(escalated_op("invoke", "ShowConnectionMap"), Some(OwnerOnly));
        // The configure/disconnect ids (§2.3) rewrite/dissolve standing
        // session-connection authority — the `connect`/`disconnect` verbs'
        // OwnerOnly class, fenced identically at the invoke seam.
        assert_eq!(
            escalated_op("invoke", "ConfigureConnection"),
            Some(OwnerOnly)
        );
        assert_eq!(escalated_op("invoke", "DisconnectSession"), Some(OwnerOnly));
        // A benign action / a benign `open` target does NOT escalate (base gate suffices).
        // Font zoom is runtime-only view state (no config persist).
        assert_eq!(escalated_op("invoke", "NewTab"), None);
        assert_eq!(escalated_op("invoke", "FontIncrease"), None);
        // An UNKNOWN action name does not escalate — the base WriteInput gate applies and
        // `cmd_invoke` rejects the unknown name at execution (harmless: nothing runs).
        assert_eq!(escalated_op("invoke", "TotallyBogusAction"), None);
        assert_eq!(escalated_op("open", "about"), None);
        assert_eq!(escalated_op("open", "perf"), None);
        // `open prefs`/`open settings`/`open preferences` all raise the security-knob
        // overlay => ConfigWrite.
        assert_eq!(escalated_op("open", "prefs"), Some(EOp(Op::ConfigWrite)));
        assert_eq!(escalated_op("open", "settings"), Some(EOp(Op::ConfigWrite)));
        assert_eq!(
            escalated_op("open", "preferences"),
            Some(EOp(Op::ConfigWrite))
        );
        assert_eq!(
            escalated_op("open", "app settings /security"),
            Some(EOp(Op::ConfigWrite)),
            "the versioned native Settings route has the same fine-op fence"
        );
        assert_eq!(
            escalated_op(
                "act",
                "app/v1 view 42 settings/control/allow_osc52_query settings/set/allow_osc52_query true"
            ),
            Some(EOp(Op::ConfigWrite)),
            "semantic Settings actions cannot inherit WriteInput"
        );
        assert_eq!(
            escalated_op("open", "app editor file:///tmp/example.md"),
            None,
            "opening a document retains the established WriteInput class"
        );
        // `open menu`/`open palette` (gateway) and `open update` (re-exec surface) are
        // OWNER-ONLY — the same fence as their `invoke` twins.
        assert_eq!(escalated_op("open", "menu"), Some(OwnerOnly));
        assert_eq!(escalated_op("open", "palette"), Some(OwnerOnly));
        assert_eq!(escalated_op("open", "update"), Some(OwnerOnly));
        // The `close` VARIANT must inherit the SAME fence as the open — `cmd_open` acts
        // on the first token and dismisses the overlay, so `open prefs close` /
        // `open menu close` / `open update close` escalate identically. (Regression: the
        // fence classified `rest.trim()`, so `AuxTarget::parse("prefs close")` → None
        // collapsed it to the base WriteInput and a scoped edge could dismiss a
        // privileged overlay it is fenced from opening.)
        assert_eq!(
            escalated_op("open", "prefs close"),
            Some(EOp(Op::ConfigWrite))
        );
        assert_eq!(
            escalated_op("open", "settings close"),
            Some(EOp(Op::ConfigWrite))
        );
        assert_eq!(escalated_op("open", "menu close"), Some(OwnerOnly));
        assert_eq!(escalated_op("open", "palette close"), Some(OwnerOnly));
        assert_eq!(escalated_op("open", "update close"), Some(OwnerOnly));

        // SESSION CONNECTIONS (design §5.3/§6): a CONNECTED spawn mints standing
        // cross-session authority, so `spawn connected=…` is OWNER-ONLY — no fine
        // op expresses it — while a plain/`cwd=` spawn keeps its base WriteInput.
        assert_eq!(
            escalated_op("spawn", "connected=controller of=s-abc"),
            Some(OwnerOnly)
        );
        assert_eq!(
            escalated_op("spawn", "connected=controlled place=tab of=s-abc cwd=/tmp"),
            Some(OwnerOnly)
        );
        assert_eq!(escalated_op("spawn", ""), None);
        assert_eq!(escalated_op("spawn", "cwd=/tmp"), None);

        // The fence must classify with the PARSER's tokenizer, not a whitespace
        // split. Every spelling below is accepted by `parse_spawn_args` as a
        // connected spawn, so every one of them must escalate; before this was
        // wired to `split_quoted_tokens` the quoted forms walked straight past
        // the gate and minted read-screen/signal/write-input on a real instance.
        assert_eq!(
            escalated_op("spawn", "\"connected=controller\" of=s-abc"),
            Some(OwnerOnly),
            "a quoted connected= must not walk past the Owner fence"
        );
        assert_eq!(
            escalated_op("spawn", "of=s-abc \"connected=controlled\" place=tab"),
            Some(OwnerOnly),
            "position must not matter either"
        );
        // Unparseable input is fenced too: the fence is never weaker than the
        // parser, and an unterminated quote reaches the parser as an error.
        assert_eq!(escalated_op("spawn", "\"connected=controller"), Some(OwnerOnly));
        // Every AGGREGATED-graph surface carries the `flows` Owner gate: the map
        // (`open connections`), its text readout (`controls connections`), and its
        // capture (`window connections`) — including the `close`-variant dismiss
        // (the `open prefs close` rule). Benign targets keep their base class.
        assert_eq!(escalated_op("open", "connections"), Some(OwnerOnly));
        assert_eq!(escalated_op("controls", "connections"), Some(OwnerOnly));
        assert_eq!(escalated_op("window", "connections"), Some(OwnerOnly));
        assert_eq!(escalated_op("open", "connections close"), Some(OwnerOnly));
        assert_eq!(escalated_op("controls", "front"), None);
        assert_eq!(escalated_op("window", ""), None);
        assert_eq!(escalated_op("window", "front shot.png"), None);

        // `meta` (base Read) escalates its WRITE sub-forms to WriteInput — a
        // read-only edge can read `meta` but never `meta set`/`meta unset` — and
        // the bare/status read does NOT escalate (base Read gate suffices).
        assert_eq!(
            escalated_op("meta", "set title build agent"),
            Some(EOp(Op::WriteInput))
        );
        assert_eq!(
            escalated_op("meta", "unset icon"),
            Some(EOp(Op::WriteInput))
        );
        // The typed role/attention keys ride the same write escalation.
        assert_eq!(
            escalated_op("meta", "set role operator"),
            Some(EOp(Op::WriteInput))
        );
        assert_eq!(
            escalated_op("meta", "unset attention"),
            Some(EOp(Op::WriteInput))
        );
        assert_eq!(escalated_op("meta", ""), None);
        assert_eq!(escalated_op("timeline", "10 since=3"), None);

        // The FULL dispatch decision for `verb rest` — the exact `dispatch_authorized`
        // the op-scope gate runs (effective op = base op ESCALATED by the argument;
        // Owner short-circuits to full power). Not `gate_allows` (verb-only): the
        // escalation REPLACES the base op, so a fine-op edge passes and a WriteInput
        // edge is fenced — the additive interpretation would wrongly deny the fine-op
        // edge on the base WriteInput check.
        let dispatch_allows = dispatch_authorized;

        let ctx = test_ctx();
        let owner = Scope::Owner;
        let edge_write = edge_granted(Op::WriteInput, &ctx);
        let edge_clip = edge_granted(Op::ClipboardWrite, &ctx);
        let edge_cfg = edge_granted(Op::ConfigWrite, &ctx);

        // THE SEAM CLOSURE: a WriteInput edge may `invoke NewTab` (benign) but is DENIED
        // the clipboard/config escalations and `open prefs` — it can no longer tunnel to
        // ClipboardWrite/ConfigWrite through these indirect seams.
        assert!(
            dispatch_allows(edge_write, "invoke", "NewTab", &ctx),
            "write edge: invoke NewTab (benign)"
        );
        assert!(
            !dispatch_allows(edge_write, "invoke", "Copy", &ctx),
            "write edge DENIED invoke Copy (clipboard escalation)"
        );
        assert!(
            !dispatch_allows(edge_write, "invoke", "ToggleSettings", &ctx),
            "write edge DENIED invoke ToggleSettings (config escalation)"
        );
        assert!(
            !dispatch_allows(edge_write, "open", "prefs", &ctx),
            "write edge DENIED open prefs (config escalation)"
        );
        assert!(
            !dispatch_allows(edge_write, "open", "app settings /security", &ctx),
            "write edge DENIED native Settings route"
        );
        assert!(
            !dispatch_allows(
                edge_write,
                "act",
                "app/v1 view 42 settings/control/allow_osc52_query settings/set/allow_osc52_query true",
                &ctx,
            ),
            "write edge DENIED semantic Settings action"
        );
        assert!(
            dispatch_allows(edge_write, "open", "about", &ctx),
            "write edge: open about (benign)"
        );
        // THE THIRD-PASS HOLE: a plain WriteInput edge is DENIED the privileged
        // MenuActions reachable via `invoke` — the palette gateway, the update re-exec
        // twins, the settings surface, and clipboard exfil.
        for rest in [
            "OpenPalette",
            "ApplyUpdate",
            "SoftwareUpdate",
            "ToggleSettings",
            "Preferences",
            "Copy",
            "Paste",
            "SelectAll",
        ] {
            assert!(
                !dispatch_allows(edge_write, "invoke", rest, &ctx),
                "write edge DENIED invoke {rest}"
            );
        }
        // The palette/update `open` twins are Owner-only too.
        for rest in ["menu", "palette", "update"] {
            assert!(
                !dispatch_allows(edge_write, "open", rest, &ctx),
                "write edge DENIED open {rest} (owner-only surface)"
            );
        }

        // Owner keeps full power over every seam, INCLUDING the owner-only ones.
        for (verb, rest) in [
            ("invoke", "Copy"),
            ("invoke", "ToggleSettings"),
            ("invoke", "OpenPalette"),
            ("invoke", "ApplyUpdate"),
            ("open", "prefs"),
            ("open", "menu"),
            ("open", "update"),
        ] {
            assert!(
                dispatch_allows(owner, verb, rest, &ctx),
                "Owner: {verb} {rest}"
            );
        }

        // An explicitly-granted fine-op edge (Owner minted one via `grant`) works: the
        // fence is about DEFAULT inheritance, not an absolute ban.
        assert!(
            dispatch_allows(edge_clip, "invoke", "Copy", &ctx),
            "explicit clipboard-write edge: invoke Copy"
        );
        assert!(
            dispatch_allows(edge_cfg, "invoke", "ToggleSettings", &ctx),
            "explicit config-write edge: invoke ToggleSettings"
        );
        assert!(
            dispatch_allows(edge_cfg, "open", "prefs", &ctx),
            "explicit config-write edge: open prefs"
        );
        assert!(
            dispatch_allows(edge_cfg, "open", "app settings /security", &ctx),
            "explicit config-write edge: native Settings route"
        );
        assert!(
            dispatch_allows(
                edge_cfg,
                "act",
                "app/v1 view 42 settings/control/allow_osc52_query settings/set/allow_osc52_query true",
                &ctx,
            ),
            "explicit config-write edge: semantic Settings action"
        );
        // But OWNER-ONLY actions are denied even to a granted fine-op edge: no fine op —
        // ClipboardWrite or ConfigWrite — buys the palette gateway or the update re-exec.
        assert!(
            !dispatch_allows(edge_cfg, "invoke", "OpenPalette", &ctx),
            "config-write edge STILL DENIED invoke OpenPalette (owner-only)"
        );
        assert!(
            !dispatch_allows(edge_cfg, "invoke", "ApplyUpdate", &ctx),
            "config-write edge STILL DENIED invoke ApplyUpdate (owner-only)"
        );
        assert!(
            !dispatch_allows(edge_clip, "invoke", "OpenPalette", &ctx),
            "clipboard-write edge STILL DENIED invoke OpenPalette (owner-only)"
        );

        // SESSION CONNECTIONS: the aggregated-graph surfaces and the connected
        // spawn are OWNER-ONLY through the full dispatch decision — a write edge
        // (and even a granted read edge, for the Read-class controls/window) is
        // denied, while the same edge keeps the verb's benign targets.
        let edge_read = edge_granted(Op::ReadScreen, &ctx);
        for (verb, rest) in [
            ("open", "connections"),
            ("controls", "connections"),
            ("window", "connections"),
            ("spawn", "connected=controller of=s-abc"),
        ] {
            assert!(
                !dispatch_allows(edge_write, verb, rest, &ctx),
                "write edge DENIED {verb} {rest} (owner-only surface)"
            );
            assert!(
                !dispatch_allows(edge_read, verb, rest, &ctx),
                "read edge DENIED {verb} {rest} (owner-only surface)"
            );
            assert!(dispatch_allows(owner, verb, rest, &ctx), "Owner: {verb} {rest}");
        }
        assert!(
            dispatch_allows(edge_read, "controls", "front", &ctx),
            "read edge keeps benign controls targets"
        );
        assert!(
            dispatch_allows(edge_write, "spawn", "cwd=/tmp", &ctx),
            "write edge keeps the plain (non-minting) spawn"
        );
    }

    /// PART B — the front-drive fence. A scoped input verb escalates to Owner while
    /// any overlay consumes input, and to ConfigWrite while native Settings consumes
    /// input. Owner is exempt; an ordinary native app/terminal and non-input verbs keep
    /// their base class. The full path obtains this exact enum through the
    /// `Wake::FrontControlSurface` main-thread hop.
    #[test]
    fn front_drive_fence_requires_owner_for_overlays_and_config_write_for_native_settings() {
        use crate::overlay::OverlayKind;
        let scoped = edge(); // an Edge scope; authority already passed the op gate above
        let owner = Scope::Owner;

        // Every overlay is a gateway with Owner-only bindings (About includes
        // copy/open-URL; the connection map raises and DISCONNECTS, §5.3), so
        // all injected input verbs escalate identically.
        for kind in [
            OverlayKind::Settings,
            OverlayKind::About,
            OverlayKind::Palette,
            OverlayKind::Update,
            OverlayKind::ConnectionMap,
        ] {
            for verb in [
                "key", "ctrl", "mouse", "paste", "focus", "send", "feed", "turn",
            ] {
                assert_eq!(
                    front_drive_escalation(scoped, verb, FrontControlSurface::Overlay(kind)),
                    Some(Escalation::OwnerOnly),
                    "{verb} must be owner-only under {kind:?}"
                );
            }
        }

        // OWNER is ALWAYS exempt — the owner's established control path is untouched.
        for kind in [
            OverlayKind::Settings,
            OverlayKind::About,
            OverlayKind::Palette,
            OverlayKind::Update,
        ] {
            assert_eq!(
                front_drive_escalation(owner, "key", FrontControlSurface::Overlay(kind)),
                None,
                "Owner may drive its own {kind:?} overlay"
            );
        }

        // Native Settings is not an Overlay at all, but it still consumes the
        // same key/mouse vocabulary and can emit ConfigPatch effects.
        let escalation = front_drive_escalation(scoped, "key", FrontControlSurface::NativeSettings);
        assert_eq!(escalation, Some(Escalation::Op(Op::ConfigWrite)));
        let ctx = test_ctx();
        assert!(
            !scope_holds_escalation(
                edge_granted(Op::WriteInput, &ctx),
                escalation.unwrap(),
                &ctx
            ),
            "a WriteInput edge cannot drive native Settings"
        );
        assert!(scope_holds_escalation(
            edge_granted(Op::ConfigWrite, &ctx),
            escalation.unwrap(),
            &ctx,
        ));

        // No privileged front consumer => preserve the verb's ordinary authority.
        assert_eq!(
            front_drive_escalation(scoped, "key", FrontControlSurface::None),
            None
        );
        assert_eq!(
            front_drive_escalation(scoped, "key", FrontControlSurface::OtherNative),
            None
        );
        // A NON-input verb is never dynamically escalated.
        assert_eq!(
            front_drive_escalation(scoped, "text", FrontControlSurface::NativeSettings),
            None,
            "reads are not front-driving input"
        );
    }

    #[test]
    fn real_native_settings_front_is_explicitly_fenced_while_owner_open_still_works() {
        let mut app = crate::App::headless_for_test();
        let before = app.native_config_service.snapshot();
        assert_eq!(app.front_control_surface(), FrontControlSurface::None);

        assert!(
            app.open_settings_tab(crate::native_settings::SettingsRoute::Security),
            "the established owner open path remains usable"
        );
        assert_eq!(
            app.front_control_surface(),
            FrontControlSurface::NativeSettings,
            "the main-thread observation must not collapse native Settings into no overlay"
        );

        let ctx = test_ctx();
        let write_edge = edge_granted(Op::WriteInput, &ctx);
        let escalation = front_drive_escalation(write_edge, "key", app.front_control_surface())
            .expect("native Settings replaces the base input authority");
        assert!(!scope_holds_escalation(write_edge, escalation, &ctx));
        assert_eq!(
            app.native_config_service.snapshot().text,
            before.text,
            "denied scoped input cannot mutate config"
        );
        assert_eq!(
            front_drive_escalation(Scope::Owner, "key", app.front_control_surface()),
            None,
            "Owner keeps the established input path"
        );
    }

    /// COPY-ON-SELECT EXFIL FENCE (pure decision): a scoped-edge `mouse` gesture must
    /// have its copy-on-select clipboard side-effect suppressed, while Owner (the
    /// god token / in-session automation) and — implicitly — a real human gesture do
    /// NOT. Mirrors the `front_drive_escalation` Owner carve-out. This pins the
    /// control-authority DECISION headlessly; the seam honouring it (a scoped-edge
    /// release does not auto-copy) is pinned by `copy_on_select_fires_only_on_...`.
    #[test]
    fn scoped_edge_mouse_suppresses_copy_on_select_owner_does_not() {
        use super::control_input::{apply_copy_on_select_policy, scope_suppresses_copy_on_select};

        // The predicate: NON-OWNER suppresses, OWNER is exempt.
        assert!(
            scope_suppresses_copy_on_select(edge()),
            "a scoped edge must suppress copy-on-select (exfil fence)"
        );
        assert!(
            !scope_suppresses_copy_on_select(Scope::Owner),
            "Owner (god token / owner automation) keeps copy-on-select"
        );

        // The application onto the event: a scoped-edge release carries the
        // suppression flag; the SAME event from Owner leaves it false.
        let release = || parse_mouse("release left 5 9").expect("release parses");
        let InputEvent::MouseButton {
            suppress_copy_on_select: scoped_flag,
            ..
        } = apply_copy_on_select_policy(edge(), release())
        else {
            panic!("mouse event")
        };
        assert!(scoped_flag, "scoped-edge release is stamped to suppress");

        let InputEvent::MouseButton {
            suppress_copy_on_select: owner_flag,
            ..
        } = apply_copy_on_select_policy(Scope::Owner, release())
        else {
            panic!("mouse event")
        };
        assert!(
            !owner_flag,
            "Owner release is not stamped — copy-on-select fires"
        );
    }

    /// The Owner-path regression invariant: an Owner passes EVERY verb (the gate's
    /// `(Owner, _)` arm short-circuits before any lookup), so the existing aterm-ctl
    /// client is byte-identical. A `ReadScreen` Edge is denied for write/signal/
    /// privilege verbs but allowed for read-side verbs.
    #[test]
    fn op_scope_gate_owner_full_power_edge_read_only() {
        // The active session the gate evaluates against; the edges below are GRANTED
        // on it (a real connection's token is authorized against its session).
        let ctx = test_ctx();
        let owner = Scope::Owner;
        let edge_read = edge_granted(Op::ReadScreen, &ctx);

        // Owner: every verb is permitted, including grant/revoke/whoami and image.
        let all_verbs = [
            "text", "image", "scroll", "select", "feed", "signal", "send", "resize", "grant",
            "revoke", "whoami",
        ];
        for v in all_verbs {
            assert!(gate_allows(owner, v, &ctx), "Owner must pass {v}");
        }

        // ReadScreen Edge (granted on ctx): read-side verbs pass; write/signal denied.
        assert!(gate_allows(edge_read, "text", &ctx), "read edge: text");
        assert!(gate_allows(edge_read, "image", &ctx), "read edge: image");
        assert!(gate_allows(edge_read, "select", &ctx), "read edge: select");
        assert!(!gate_allows(edge_read, "feed", &ctx), "read edge: NOT feed");
        assert!(
            !gate_allows(edge_read, "signal", &ctx),
            "read edge: NOT signal"
        );
        assert!(!gate_allows(edge_read, "send", &ctx), "read edge: NOT send");

        // No Edge — regardless of op — may grant/revoke/whoami (Owner-only, None-op).
        for op in [Op::ReadScreen, Op::WriteInput, Op::Signal] {
            let e = edge_granted(op, &ctx);
            assert!(!gate_allows(e, "grant", &ctx), "no edge may grant");
            assert!(!gate_allows(e, "revoke", &ctx), "no edge may revoke");
            assert!(!gate_allows(e, "whoami", &ctx), "no edge may whoami");
            assert!(!gate_allows(e, "bogus", &ctx), "no edge: unknown verb");
        }

        // A WriteInput edge mirrors the split: it may write but not read or signal.
        let edge_write = edge_granted(Op::WriteInput, &ctx);
        assert!(gate_allows(edge_write, "feed", &ctx), "write edge: feed");
        assert!(
            !gate_allows(edge_write, "text", &ctx),
            "write edge: NOT read"
        );
        assert!(
            !gate_allows(edge_write, "signal", &ctx),
            "write edge: NOT signal"
        );

        // THE SECURITY WIN (finding #1): the fine ops are unreachable by an inherited
        // edge. A keystroke (WriteInput) edge cannot persist a config knob via
        // `settings`; a read edge cannot exfiltrate via `copy`; Owner keeps both.
        assert!(
            !gate_allows(edge_write, "settings", &ctx),
            "write edge: NOT settings (config-write is not carried by a write edge)"
        );
        assert!(
            !gate_allows(edge_read, "copy", &ctx),
            "read edge: NOT copy (clipboard-write is not carried by a read edge)"
        );
        // Neither op-inheritance leaks the OTHER fine op either.
        assert!(
            !gate_allows(edge_read, "settings", &ctx),
            "read edge: NOT settings"
        );
        assert!(
            !gate_allows(edge_write, "copy", &ctx),
            "write edge: NOT copy"
        );
        // The in-session Owner (the user driving their own terminal) keeps full power.
        assert!(gate_allows(owner, "settings", &ctx), "Owner: settings");
        assert!(gate_allows(owner, "copy", &ctx), "Owner: copy");

        // A DELIBERATELY-granted fine-op edge (Owner minting one via `grant`) works:
        // the split is about DEFAULT inheritance, not an absolute ban.
        let edge_cfg = edge_granted(Op::ConfigWrite, &ctx);
        assert!(
            gate_allows(edge_cfg, "settings", &ctx),
            "explicit config-write edge: settings"
        );
        let edge_clip = edge_granted(Op::ClipboardWrite, &ctx);
        assert!(
            gate_allows(edge_clip, "copy", &ctx),
            "explicit clipboard-write edge: copy"
        );
    }

    /// grant/revoke enforce Owner-only INSIDE the body too (defense in depth beyond
    /// the gate): an Edge scope is rejected even if the body is reached directly.
    /// `whoami` has no body guard (the gate already keeps it Owner-only); its body
    /// reports the edge's EFFECTIVE op re-derived from the presented token against the
    /// CURRENT session — an ungranted token therefore reads `edge unauthorized`.
    #[test]
    fn privilege_verbs_reject_edge_scope_in_body() {
        let ctx = test_ctx();
        let edge = edge();
        assert_eq!(
            cmd_grant(&ctx, edge, "s-deadbeef read-screen"),
            "ERR denied\n"
        );
        assert_eq!(cmd_revoke(&ctx, edge, &"0".repeat(64)), "ERR denied\n");
        // whoami re-derives the op from the token: an UNGRANTED token holds no
        // authority against this session, so it reports `edge unauthorized` (never an
        // over-stated op). The gate still keeps grant/revoke Owner-only.
        let who = cmd_whoami(&ctx, edge);
        assert!(
            who.trim_end().ends_with("edge unauthorized"),
            "ungranted edge: {who}"
        );
        // A GRANTED ReadScreen token reports its real effective op.
        let granted = edge_granted(Op::ReadScreen, &ctx);
        let who_granted = cmd_whoami(&ctx, granted);
        assert!(
            who_granted.trim_end().ends_with("edge read-screen"),
            "granted edge: {who_granted}"
        );
    }

    /// REGRESSION (introspection integrity): `whoami` must report the EFFECTIVE op
    /// against the session active RIGHT NOW, re-derived from the presented token — NOT
    /// a cached connect-time op. A token granted on session B, presented on a
    /// connection whose `@.` has swung to session A, must read `edge unauthorized` (it
    /// holds no authority over A), never over-state "edge read-screen". Mirrors the
    /// gate's per-request `authorize`, so whoami can never claim power the gate denies.
    #[test]
    fn whoami_reports_unauthorized_after_active_session_swings() {
        let ctx_b = test_ctx(); // session active when the edge connected
        let ctx_a = test_ctx(); // a DIFFERENT session `@.` later swings to
        let edge_b = edge_granted(Op::ReadScreen, &ctx_b);

        // Against its OWN granted session B, whoami reports the real op.
        assert!(
            cmd_whoami(&ctx_b, edge_b)
                .trim_end()
                .ends_with("edge read-screen"),
            "whoami on granted session B",
        );
        // After the active handle swings to A, the SAME token authorizes nothing.
        assert!(
            cmd_whoami(&ctx_a, edge_b)
                .trim_end()
                .ends_with("edge unauthorized"),
            "whoami must not over-state authority on swung-to session A",
        );
    }

    /// An Owner can mint an edge with `grant`, that edge then authenticates as an
    /// `Edge(op)` via `edge_scope_from_first_line`, and `revoke` invalidates it —
    /// the full mint -> authorize -> revoke fabric round-trip through the verbs.
    #[test]
    fn grant_then_edge_handshake_then_revoke_roundtrip() {
        let ctx = test_ctx();
        let owner = Scope::Owner;

        // Owner mints a ReadScreen edge from some source session into THIS session.
        let reply = cmd_grant(&ctx, owner, "s-source01 read-screen");
        let hex = reply
            .strip_prefix("OK ")
            .and_then(|s| s.strip_suffix('\n'))
            .expect("OK <hex>");
        assert_eq!(hex.len(), 64, "edge token is 64 hex chars");

        // The bearer presents it as the handshake hex => resolves to Edge(ReadScreen).
        let line = format!("AUTH {hex}");
        let (op, _tok, inline) =
            edge_scope_from_first_line(&line, &ctx).expect("edge authenticates");
        assert_eq!(op, Op::ReadScreen);
        assert_eq!(inline, None);

        // A folded TOKEN form preserves the inline verb.
        let line2 = format!("TOKEN {hex} text");
        let (op2, _tok2, inline2) =
            edge_scope_from_first_line(&line2, &ctx).expect("edge authenticates");
        assert_eq!(op2, Op::ReadScreen);
        assert_eq!(inline2.as_deref(), Some("text"));

        // Owner revokes it; the same hex no longer authenticates (fail closed).
        assert_eq!(cmd_revoke(&ctx, owner, hex), "OK\n");
        assert!(
            edge_scope_from_first_line(&line, &ctx).is_none(),
            "revoked => fail closed"
        );
        // A second revoke reports no-such-edge.
        assert_eq!(cmd_revoke(&ctx, owner, hex), "ERR no such edge\n");

        // whoami as Owner reports this session's identity + the owner scope.
        let who = cmd_whoami(&ctx, owner);
        assert!(who.starts_with("OK s-"), "whoami: {who}");
        assert!(who.trim_end().ends_with("owner"), "whoami scope: {who}");
    }

    /// `revoke src=<sid>` (design §1.4#4/§6): the source sweep dissolves a WHOLE
    /// connection — every row the source holds, across ops — and replies the
    /// removed count; an unknown source is the fail-closed `ERR no such edge`
    /// (never `OK 0`, which would read as a successful dissolution of nothing).
    /// The sweep form keeps the token form's in-body Owner guard.
    #[test]
    fn revoke_src_form_sweeps_the_whole_connection_and_fails_closed_on_unknown() {
        let ctx = test_ctx();
        let owner = Scope::Owner;
        let src = SessionId::new("s-sweepsrc1");

        // Mint a full BOTH connection (three rows) through the ONE mint path
        // (`grant_connection`, §1.4#2) — the graph the sweep must dissolve whole.
        let minted = {
            let mut edges = ctx.edges.lock().unwrap();
            edges.grant_connection(
                &src,
                &ctx.self_id,
                aterm_session::ConnectionKind::Both,
                &ctx.nonce,
            )
        };
        assert_eq!(minted.len(), 3, "Both mints all three human-fidelity ops");
        for (_op, tok) in &minted {
            let line = format!("AUTH {}", tok.to_hex());
            assert!(
                edge_scope_from_first_line(&line, &ctx).is_some(),
                "live before the sweep"
            );
        }

        // The sweep removes all three rows and reports the count; every minted
        // token then fails closed at the handshake.
        assert_eq!(cmd_revoke(&ctx, owner, "src=s-sweepsrc1"), "OK 3\n");
        for (_op, tok) in &minted {
            let line = format!("AUTH {}", tok.to_hex());
            assert!(
                edge_scope_from_first_line(&line, &ctx).is_none(),
                "swept => fail closed"
            );
        }

        // Unknown source / empty source / non-Owner scope all refuse.
        assert_eq!(cmd_revoke(&ctx, owner, "src=s-nobody99"), "ERR no such edge\n");
        assert_eq!(
            cmd_revoke(&ctx, owner, "src="),
            "ERR usage: revoke <edge-hex> | revoke src=<sid>\n"
        );
        assert_eq!(cmd_revoke(&ctx, edge(), "src=s-sweepsrc1"), "ERR denied\n");
    }

    /// The `session_edge` AUDIT seam (design §1.4#5, §7): each wire act — grant,
    /// token revoke, source sweep — emits exactly one structured event on the
    /// dedicated `session_edge` target carrying (action, origin, src, dst, op),
    /// and NEVER a token hex (the bearer secret must not reach any log sink).
    /// Captured records are filtered by this test's unique src sid, so parallel
    /// tests minting their own edges cannot interfere.
    #[test]
    fn session_edge_audit_events_are_structured_and_hex_free() {
        use std::sync::OnceLock;

        struct Capture;
        static CAPTURED: OnceLock<Mutex<Vec<(String, String)>>> = OnceLock::new();
        fn captured() -> &'static Mutex<Vec<(String, String)>> {
            CAPTURED.get_or_init(|| Mutex::new(Vec::new()))
        }
        impl aterm_log::Log for Capture {
            fn enabled(&self, _m: &aterm_log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, record: &aterm_log::Record<'_>) {
                captured()
                    .lock()
                    .unwrap()
                    .push((record.target().to_string(), format!("{}", record.args())));
            }
            fn flush(&self) {}
        }
        static LOGGER: Capture = Capture;
        // First installer wins process-wide (the logger OnceLock); either way the
        // max level must admit the seam's Info tier.
        let _ = aterm_log::set_logger(&LOGGER);
        aterm_log::set_max_level(aterm_log::LevelFilter::Info);

        let ctx = test_ctx();
        let owner = Scope::Owner;
        let src = "s-auditsrc7";

        // grant -> revoke <hex> -> grant -> revoke src= : all four audited acts.
        let reply1 = cmd_grant(&ctx, owner, &format!("{src} read-screen"));
        let hex1 = reply1.strip_prefix("OK ").unwrap().trim_end();
        assert_eq!(cmd_revoke(&ctx, owner, hex1), "OK\n");
        let reply2 = cmd_grant(&ctx, owner, &format!("{src} write-input"));
        let hex2 = reply2.strip_prefix("OK ").unwrap().trim_end();
        assert_eq!(cmd_revoke(&ctx, owner, &format!("src={src}")), "OK 1\n");

        let records: Vec<String> = captured()
            .lock()
            .unwrap()
            .iter()
            .filter(|(target, msg)| target == "session_edge" && msg.contains(src))
            .map(|(_, msg)| msg.clone())
            .collect();
        let dst = ctx.self_id.as_str();
        assert_eq!(records.len(), 4, "one event per act: {records:?}");
        assert_eq!(
            records[0],
            format!("EDGE: action=grant origin=wire src={src} dst={dst} op=read-screen")
        );
        assert_eq!(
            records[1],
            format!("EDGE: action=revoke origin=wire src={src} dst={dst} op=read-screen")
        );
        assert_eq!(
            records[2],
            format!("EDGE: action=grant origin=wire src={src} dst={dst} op=write-input")
        );
        assert_eq!(
            records[3],
            format!("EDGE: action=revoke_src origin=wire src={src} dst={dst} op=*")
        );
        // The hex-free obligation: no captured line ANYWHERE carries a token.
        for (_, msg) in captured().lock().unwrap().iter() {
            assert!(
                !msg.contains(hex1) && !msg.contains(hex2),
                "a token hex leaked into a log line: {msg}"
            );
        }
    }

    /// `resize 65535 65535` asks for a ~4.3-billion-cell allocation; the parse
    /// must reject anything outside 1..=MAX_GRID_ROWS/COLS. (RES-1: the verb now
    /// forwards a `Wake::Resize` to the geometry-owning main thread; the pure
    /// `parse_resize` is the validator the verb gates on.)
    #[test]
    fn resize_rejects_out_of_range() {
        for req in ["65535 65535", "4097 80", "24 4097", "0 80", "24 0"] {
            assert_eq!(parse_resize(req), Err("ERR out of range\n".to_string()));
        }
    }

    /// `cwd` surfaces the OSC 7-reported working directory (empty until set).
    #[test]
    fn cwd_verb_reports_working_directory() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        assert_eq!(cmd_cwd(&term), "OK \n");
        // OSC 7: a program reports its cwd as a file:// URI.
        term.lock()
            .unwrap()
            .process(b"\x1b]7;file://localhost/Users//example/x\x07");
        let out = cmd_cwd(&term);
        assert!(out.contains("/Users//example/x"), "cwd not surfaced: {out}");
    }

    /// `cast` serializes the session's asciicast recorder behind the read-verb
    /// framing `OK <nbytes>\n<body>`, where `<nbytes>` is the exact body length
    /// and the body is valid asciicast v2 (header line + recorded events).
    #[test]
    fn cast_verb_frames_asciicast_body() {
        let ctx = test_ctx();
        // Empty recording: header-only body, framed with its true byte length.
        let reply = cmd_cast(&ctx);
        let (hdr_line, body) = reply.split_once('\n').expect("OK <n>\\n<body>");
        let n: usize = hdr_line
            .strip_prefix("OK ")
            .expect("OK prefix")
            .parse()
            .expect("nbytes");
        assert_eq!(n, body.len(), "framed length must equal the body length");
        let header = body.lines().next().expect("a header line");
        assert!(
            header.contains("\"version\": 2"),
            "not asciicast v2: {header}"
        );

        // Fold in an output burst, then the body grows by one well-formed event.
        {
            let mut rec = ctx.cast.lock().unwrap();
            let t = rec.now();
            rec.record_output(t, b"hi there\r\n");
        }
        let reply2 = cmd_cast(&ctx);
        let (hdr2, body2) = reply2.split_once('\n').unwrap();
        let n2: usize = hdr2.strip_prefix("OK ").unwrap().parse().unwrap();
        assert_eq!(n2, body2.len());
        assert!(
            body2.lines().count() >= 2,
            "expected header + >=1 event: {body2}"
        );
        let event = body2.lines().nth(1).unwrap();
        assert!(
            event.starts_with('[') && event.contains("\"o\""),
            "bad event: {event}"
        );
    }

    /// `blocks` surfaces the OSC 133/633 shell-integration command blocks so an
    /// AI can navigate by command: exit codes, output row range, command text.
    #[test]
    fn blocks_verb_surfaces_command_blocks() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let reg = subscribe::new_registry();
        let host = GuiHost::new(0, &term, None, &reg);
        // No shell integration yet -> no blocks.
        assert_eq!(cmd_blocks(&host, 0, ""), "OK 0\n");
        // Two command blocks via OSC 133 (+ OSC 633;E commandline): exit 0, exit 1.
        // Each OSC mark is BEL-terminated so the surrounding text isn't swallowed.
        term.lock().unwrap().process(
            b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07\
\x1b]133;A\x07$ \x1b]633;E;false\x07\x1b]133;B\x07false\n\x1b]133;C\x07\x1b]133;D;1\x07",
        );
        let out = cmd_blocks(&host, 0, "");
        assert!(out.starts_with("OK 2\n"), "expected 2 blocks: {out}");
        assert!(
            out.contains("exit=0") && out.contains("cmdline=echo%20hi"),
            "block 1 wrong: {out}"
        );
        assert!(
            out.contains("exit=1") && out.contains("cmdline=false"),
            "block 2 wrong: {out}"
        );
        // `blocks 1` returns only the most recent (the failed one).
        let last = cmd_blocks(&host, 0, "1");
        assert!(
            last.starts_with("OK 1\n") && last.contains("exit=1"),
            "last block wrong: {last}"
        );
        // `blocktext 0` reads block 0's OUTPUT directly (no coordinate math).
        let txt = cmd_blocktext(&host, 0, "0");
        assert!(
            txt.starts_with("OK ") && txt.contains("hi"),
            "block 0 output wrong: {txt}"
        );
        assert_eq!(cmd_blocktext(&host, 0, "99"), "ERR no such block\n");
    }

    /// `wait` blocks until the in-flight command completes, then reports it; with
    /// no completion at all it times out.
    #[test]
    fn wait_verb_blocks_until_command_completes() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let reg = subscribe::new_registry();
        let host = GuiHost::new(0, &term, None, &reg);
        // No command in flight and nothing ever completed -> a short wait times out.
        assert_eq!(cmd_wait(&host, 0, "0"), "OK timeout\n");
        // Start a command (executing), then complete it from another thread,
        // firing the SAME notify the GUI's Wake::Output hook fires on output.
        term.lock()
            .unwrap()
            .process(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep\n\x1b]133;C\x07");
        let bg = term.clone();
        let reg_bg = reg.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(40));
            bg.lock().unwrap().process(b"\x1b]133;D;0\x07");
            reg_bg.lock().unwrap().notify(0);
        });
        let resp = cmd_wait(&host, 0, "5000");
        h.join().unwrap();
        assert!(
            resp.starts_with("OK complete ") && resp.contains("exit=0"),
            "wait should report the completed command: {resp}"
        );
    }

    /// The send→wait ENTRY RACE: a fast command that completed BEFORE `wait` was
    /// called is returned immediately (no block in flight, a completion exists) —
    /// never waited past into a timeout — while an IN-FLIGHT block suppresses
    /// that fast path so a stale completion is not reported for a newer command.
    #[test]
    fn wait_verb_returns_a_prerace_completion_immediately() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let reg = subscribe::new_registry();
        let host = GuiHost::new(0, &term, None, &reg);
        // The command ran to completion (exit 1) and the shell is back at a fresh
        // IDLE prompt — including the prompt-end 133;B a real shell emits before
        // any typing, which parks the new block in `EnteringCommand`. This is the
        // exact state `wait` sees when a fast command beat it to the socket; the
        // idle prompt must NOT read as in-flight (verified live: counting it made
        // every post-completion `wait` time out).
        term.lock().unwrap().process(
            b"\x1b]133;A\x07$ \x1b]133;B\x07false\n\x1b]133;C\x07\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07",
        );
        let start = std::time::Instant::now();
        let resp = cmd_wait(&host, 0, "30000");
        assert!(
            resp.starts_with("OK complete ") && resp.contains("exit=1"),
            "an already-completed command must be reported: {resp}"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "the entry fast path must answer immediately, not near the timeout"
        );

        // A NEW command is now executing: the stale completion above must NOT be
        // returned — with nothing completing in time, `wait` times out instead.
        term.lock()
            .unwrap()
            .process(b"\x1b]133;B\x07sleep 99\n\x1b]133;C\x07");
        assert_eq!(
            cmd_wait(&host, 0, "50"),
            "OK timeout\n",
            "an in-flight block must suppress the last-completed fast path"
        );
    }

    /// `cell` appends `link=<url>` for an OSC 8 hyperlinked cell, and nothing
    /// for a plain cell (positional fields unchanged for non-link cells).
    #[test]
    fn cell_verb_surfaces_osc8_hyperlink() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        // OSC 8 open (target https://example.com), one glyph 'X', OSC 8 close.
        term.lock()
            .unwrap()
            .process(b"\x1b]8;;https://example.com\x1b\\X\x1b]8;;\x1b\\");
        let linked = cmd_cell(&term, "0 0");
        assert!(
            linked.contains("link=https://example.com"),
            "linked cell missing hyperlink: {linked}"
        );
        let plain = cmd_cell(&term, "0 5");
        assert!(
            !plain.contains("link="),
            "plain cell has a stray link: {plain}"
        );
    }

    /// `colors` reports the theme and reflects OSC 10/11/12 dynamic changes.
    #[test]
    fn colors_verb_reports_theme() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let out = cmd_colors(&term);
        assert!(
            out.starts_with("OK fg=") && out.contains(" bg=") && out.contains(" cursor="),
            "unexpected colors format: {out}"
        );
        // OSC 11 sets the background; the verb must reflect it.
        term.lock().unwrap().process(b"\x1b]11;#102030\x07");
        assert!(
            cmd_colors(&term).contains("bg=102030"),
            "bg not updated: {}",
            cmd_colors(&term)
        );
    }

    /// `modes` exposes IRM / DECAWM / DECOM, which a driving client needs to
    /// predict how typed input and printed output land.
    #[test]
    fn modes_verb_exposes_insert_wrap_origin() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let out = cmd_modes(&term);
        assert!(out.contains("insert_mode=false"), "{out}");
        assert!(out.contains("auto_wrap=true"), "{out}");
        assert!(out.contains("origin_mode=false"), "{out}");
        // IRM on (ESC[4h), auto-wrap off (ESC[?7l), origin on (ESC[?6h).
        term.lock().unwrap().process(b"\x1b[4h\x1b[?7l\x1b[?6h");
        let out2 = cmd_modes(&term);
        assert!(
            out2.contains("insert_mode=true")
                && out2.contains("auto_wrap=false")
                && out2.contains("origin_mode=true"),
            "{out2}"
        );
    }

    /// An in-range resize parses to the requested `(rows, cols)` (RES-1: the
    /// engine/PTY/window resize then happens on the main thread via
    /// `Wake::Resize`, which a headless unit test cannot drive — so we verify the
    /// validated geometry the verb forwards).
    #[test]
    fn resize_parses_in_range() {
        assert_eq!(parse_resize("30 100"), Ok((30, 100)));
        assert_eq!(
            parse_resize(""),
            Err("ERR usage: resize <r> <c>\n".to_string())
        );
        assert_eq!(parse_resize("x y"), Err("ERR bad args\n".to_string()));
    }

    /// The `px` form must NOT be parsed as a cell geometry.
    ///
    /// `resize px <w> <h>` exists because the cell form cannot reach the live-drag
    /// path: it applies the grid first and echoes the pixel size after, so the
    /// window event arrives with the columns already correct and the width throttle
    /// sees no reflow. Routing `px` through `parse_resize` would silently reinstate
    /// exactly that — and worse, a plausible pixel size like `1400 900` is a VALID
    /// cell geometry only by accident of the range check, so the mistake would not
    /// announce itself. Pin the discrimination at the parser.
    #[test]
    fn resize_px_is_not_a_cell_geometry() {
        // Whatever `px …` means, it is never "30 rows by 100 cols".
        assert!(parse_resize("px 1400 900").is_err());
        // A pixel pair that is ALSO in cell range must still not be mistaken for
        // one: it is the `px` token, not the magnitudes, that decides.
        assert!(parse_resize("px 100 40").is_err());
    }

    /// `tab` parses each form to its `TabAction`; the actual App mutation happens on
    /// the main thread via `Wake::TabCmd` (a headless unit test cannot drive it), so
    /// we verify the action the verb forwards. An unknown / missing arg is `None` (the
    /// verb then replies with the usage error).
    #[test]
    fn tab_parses_each_form() {
        assert_eq!(parse_tab("new"), Some(TabAction::New));
        assert_eq!(parse_tab("next"), Some(TabAction::Next));
        assert_eq!(parse_tab("prev"), Some(TabAction::Prev));
        assert_eq!(parse_tab("0"), Some(TabAction::Select(0)));
        assert_eq!(parse_tab("3"), Some(TabAction::Select(3)));
        // Surrounding whitespace is tolerated (the rest-of-line may carry it).
        assert_eq!(parse_tab("  2 "), Some(TabAction::Select(2)));
        // `close` (active tab) and `close <N>` (a specific tab).
        assert_eq!(parse_tab("close"), Some(TabAction::Close(None)));
        assert_eq!(parse_tab("close 1"), Some(TabAction::Close(Some(1))));
        assert_eq!(parse_tab("  close 0 "), Some(TabAction::Close(Some(0))));
        // `move <from> <to>` reorders.
        assert_eq!(
            parse_tab("move 2 0"),
            Some(TabAction::Move { from: 2, to: 0 })
        );
        assert_eq!(
            parse_tab("move 0 3"),
            Some(TabAction::Move { from: 0, to: 3 })
        );
        // Unknown / empty / negative => None (usage error).
        assert_eq!(parse_tab(""), None);
        assert_eq!(parse_tab("bogus"), None);
        assert_eq!(parse_tab("-1"), None);
        // Malformed close/move => None.
        assert_eq!(parse_tab("close x"), None);
        assert_eq!(parse_tab("close 1 2"), None);
        assert_eq!(parse_tab("move 1"), None);
        assert_eq!(parse_tab("move 1 x"), None);
        assert_eq!(parse_tab("move 1 2 3"), None);
        // A trailing word after a keyword is rejected (not silently swallowed).
        assert_eq!(parse_tab("new x"), None);
        assert_eq!(parse_tab("next y"), None);
    }

    /// `tab` is classed as a WRITE verb (it DRIVES the GUI), so a `ReadScreen` edge
    /// cannot run it and a `WriteInput` edge can — same as `send`/`key`/`resize`.
    #[test]
    fn tab_is_write_classified() {
        assert_eq!(required_op("tab"), Some(Op::WriteInput));
    }

    /// The combining-aware grapheme content of a single cell, taken via the
    /// SELECTION path (`select` that one cell + `selection_to_string`) — the
    /// fidelity ground truth the pixels also render. Used by the I-1 test to
    /// prove `text`/`cell`/`search` agree with selection.
    fn selection_of_cell(term: &Arc<Mutex<Terminal>>, row: i32, col: u16) -> String {
        let mut t = term_lock(term);
        let sel = t.text_selection_mut();
        sel.start_selection(row, col, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(row, col, SelectionSide::Right);
        sel.complete_selection();
        t.selection_to_string().unwrap_or_default()
    }

    /// I-1 FIDELITY: `text`/`cell`/`search` must return the SAME grapheme content
    /// (base char + combining marks + complex cluster) the SELECTION path returns
    /// — the renderer consumes that same content via combining_row/cluster_row, so
    /// this also proves text/cell/search agree with the rendered pixels. The old
    /// code read only the resolved base `RenderCell.ch`, silently dropping an NFD
    /// accent and a ZWJ emoji family; this test would fail against that code.
    #[test]
    fn read_paths_preserve_combining_and_zwj_clusters() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        // Row 0: an NFD accent "é" = 'e' + U+0301 in one cell.
        // Row 1: a ZWJ family "👨‍👩‍👧" = man + ZWJ + woman + ZWJ + girl in one
        // (wide) cell. Both are folded into a single grid cell with the trailing
        // codepoints stored as combining marks (the same path the renderer reads).
        term.lock()
            .unwrap()
            .process("e\u{0301}\r\n\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".as_bytes());

        // Ground truth from the selection path.
        let accent_sel = selection_of_cell(&term, 0, 0);
        let family_sel = selection_of_cell(&term, 1, 0);
        assert_eq!(accent_sel, "e\u{0301}", "selection ground truth (accent)");
        assert_eq!(
            family_sel, "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "selection ground truth (ZWJ family)"
        );

        // ---- cell verb: grapheme token (pct-encoded) decodes to the selection.
        let accent_cell = cmd_cell(&term, "0 0");
        let accent_tok = accent_cell
            .strip_prefix("OK ")
            .and_then(|s| s.split(' ').next())
            .expect("cell OK token");
        assert_eq!(
            pct_decode(accent_tok),
            accent_sel,
            "cell grapheme must equal selection (accent): {accent_cell}"
        );
        let family_cell = cmd_cell(&term, "1 0");
        let family_tok = family_cell
            .strip_prefix("OK ")
            .and_then(|s| s.split(' ').next())
            .expect("cell OK token");
        assert_eq!(
            pct_decode(family_tok),
            family_sel,
            "cell grapheme must equal selection (ZWJ family): {family_cell}"
        );
        // The old `ch as u32` field would have been a bare codepoint, NOT the
        // multi-codepoint cluster — assert the cluster is really present.
        assert!(
            pct_decode(family_tok).chars().count() >= 5,
            "ZWJ family must keep all 5 codepoints, got {family_tok}"
        );

        // ---- text verb: the row line equals the full-row selection.
        let text = cmd_text(&term);
        let lines: Vec<&str> = text.lines().collect();
        // lines[0] is the "OK <n>" header; row r is lines[r+1].
        assert_eq!(
            lines[1], accent_sel,
            "text row 0 must equal selection: {text}"
        );
        assert_eq!(
            lines[2], family_sel,
            "text row 1 must equal selection: {text}"
        );

        // ---- search verb: searching the cluster finds it, and the located cell
        // reads back (via cell) the same grapheme the selection shows.
        let s = cmd_search(&term, "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}");
        assert!(
            s.starts_with("OK 1"),
            "search must find the ZWJ family once: {s}"
        );
        let hit = s.lines().nth(1).expect("a match row");
        let mut parts = hit.split(' ');
        let abs_row: u64 = parts.next().unwrap().parse().expect("abs row");
        // The absolute row resolves (via abs_row_text, the `line`/`text` space)
        // to a row whose text contains the faithful cluster.
        let row_text = match abs_row_text(&term_lock(&term), abs_row) {
            AbsRow::Text(t) => t,
            AbsRow::Evicted => panic!("search row {abs_row} unexpectedly evicted"),
            AbsRow::OutOfRange => panic!("search row {abs_row} out of range"),
        };
        assert!(
            row_text.contains(&family_sel),
            "search's absolute row must resolve to the cluster: {row_text:?}"
        );
    }

    /// Decode the percent-encoding `pct_encode` produces (the shared wire
    /// decoder — promoted beside the encoder so the pair cannot drift).
    fn pct_decode(s: &str) -> String {
        aterm_control::wire::pct_decode(s)
    }

    /// SEARCH-1: a term scrolled OFF the visible screen into scrollback is still
    /// found, the match's ABSOLUTE row resolves (via `line`) to the same content,
    /// and a regex query returns the expected matches. Proves the real
    /// `TerminalSearch` (scrollback + visible, regex) replaced the naive
    /// visible-only substring scan.
    #[test]
    fn search_finds_scrollback_and_regex() {
        // Searches a needle scrolled deep into history, so it assumes the default (large)
        // cap — serialize against the cap-mutation test that lowers the process-global cap.
        let _serial = super::search_cap_test_guard();
        // Small grid so content scrolls into history quickly.
        let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
        // Print a unique needle, then enough lines to push it off-screen.
        term.lock().unwrap().process(b"NEEDLE_alpha\r\n");
        for i in 0..20 {
            term.lock()
                .unwrap()
                .process(format!("filler line {i}\r\n").as_bytes());
        }
        // The needle is no longer on the visible 4-row screen.
        let visible = cmd_text(&term);
        assert!(
            !visible.contains("NEEDLE_alpha"),
            "needle should have scrolled off-screen: {visible}"
        );
        // But search (which indexes scrollback) finds it.
        let s = cmd_search(&term, "NEEDLE_alpha");
        assert!(
            s.starts_with("OK 1"),
            "scrolled-off needle must be found: {s}"
        );
        let hit = s.lines().nth(1).expect("match row");
        let abs_row: u64 = hit.split(' ').next().unwrap().parse().expect("abs row");
        // The `line` verb resolves that absolute row to the needle's content.
        let line_out = cmd_line(&term, &abs_row.to_string());
        assert!(
            line_out.contains("NEEDLE_alpha"),
            "line {abs_row} must resolve to the needle (got {line_out})"
        );

        // Regex: a single-token pattern + the `regex` flag. `fill[a-z]+` matches
        // every "filler" row (the pattern carries no spaces, so it stays one
        // token; the trailing `regex` is parsed as a flag).
        let rx = cmd_search(&term, "fill[a-z]+ regex");
        assert!(
            rx.starts_with("OK "),
            "regex search should succeed (regex feature enabled): {rx}"
        );
        let count: usize = rx
            .lines()
            .next()
            .and_then(|h| h.strip_prefix("OK "))
            .and_then(|n| n.split(' ').next())
            .and_then(|n| n.parse().ok())
            .expect("count");
        assert!(
            count >= 2,
            "regex `fill[a-z]+` should match many filler rows: {rx}"
        );

        // Case sensitivity: default is insensitive, `case` flips it.
        let ci = cmd_search(&term, "needle_alpha");
        assert!(
            ci.starts_with("OK 1"),
            "case-insensitive default must match: {ci}"
        );
        let cs = cmd_search(&term, "needle_alpha case");
        assert!(
            cs.starts_with("OK 0"),
            "case-sensitive must NOT match lowercased: {cs}"
        );
    }

    /// The search pattern is the REST OF THE LINE: a literal containing spaces
    /// (an exact shell error, say) matches verbatim, tail flags still parse
    /// after it, and a single-token pattern behaves exactly as before.
    #[test]
    fn search_matches_a_spaced_literal() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term.lock()
            .unwrap()
            .process(b"zsh: command not found: frobnicate\r\nCOMMAND NOT FOUND\r\n");
        // The spaced literal is ONE pattern (case-insensitive default: both rows).
        let s = cmd_search(&term, "command not found");
        assert!(
            s.starts_with("OK 2"),
            "a spaced literal must match verbatim: {s}"
        );
        // A tail flag still parses after a spaced pattern.
        let cs = cmd_search(&term, "COMMAND NOT FOUND case");
        assert!(
            cs.starts_with("OK 1"),
            "case flag after a spaced literal: {cs}"
        );
        // A single remaining token is always the PATTERN, never a flag.
        let bare = cmd_search(&term, "case");
        assert!(
            bare.starts_with("OK 0"),
            "bare `search case` searches for the literal: {bare}"
        );
    }

    // ---- P1.2: cross-session @selector addressing --------------------------------

    use crate::session_store::{self, SessionHandle, SessionState};
    use aterm_session::{EdgeTable, LaunchNonce, decide_edge};

    /// Build a registered session: a fresh `Terminal` (optionally pre-fed `seed`
    /// bytes so its `text` read is distinctive), a fresh fabric identity, and a sink
    /// over `master` (a pipe write-end in the write tests, else `-1`). Returns the
    /// handle (carrying the shared `Arc`s) so a test can register it AND assert on
    /// the same live engine.
    fn registered_session(local_id: u64, master: i32, seed: &[u8]) -> SessionHandle {
        let sid = SessionId::generate();
        let nonce = LaunchNonce::generate();
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        if !seed.is_empty() {
            term.lock().unwrap().process(seed);
        }
        let ctx = Arc::new(crate::SessionCtx {
            sink: Arc::new(SinkWriter::new(master)),
            edges: std::sync::Mutex::new(EdgeTable::new()),
            turn_lease: std::sync::Mutex::new(None),
            self_id: sid.clone(),
            nonce,
            cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
                80, 24,
            ))),
            temporal: Arc::new(std::sync::Mutex::new(
                crate::temporal::TemporalRecorder::new(),
            )),
            byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
            turns: Arc::new(std::sync::Mutex::new(
                crate::turn_ledger::TurnLedger::default(),
            )),
            meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
            app_kitty: std::sync::Mutex::new(crate::app_kitty::AppKittySlot::default()),
            timeline: Arc::new(std::sync::Mutex::new(
                crate::session_timeline::SessionTimeline::default(),
            )),
        });
        SessionHandle {
            sid,
            nonce,
            local_id,
            parent: None,
            state: SessionState::Alive,
            title: format!("tab-{local_id}"),
            term,
            master,
            ctx,
        }
    }

    /// (a) `@.` (explicit self) resolves to the EXACT self tuple, so a read of `@.`
    /// is byte-identical to the verbatim self read (zero-change guarantee).
    #[test]
    fn at_dot_selector_resolves_self_verbatim() {
        let store = session_store::new_store();
        let self_h = registered_session(0, -1, b"hello-self\r\n");
        store.write().unwrap().register(self_h.clone());

        let self_tuple: Target = (
            self_h.term.clone(),
            self_h.master,
            self_h.local_id,
            self_h.ctx.clone(),
        );

        // `@.` and `@` (empty body) both name self.
        for body in ["", "."] {
            let sel = Selector::parse(body);
            let (t, m, id, _ctx) =
                resolve_target(&self_tuple, &store, &sel).expect("self resolves");
            assert!(
                Arc::ptr_eq(&t, &self_h.term),
                "@{body} is the same Arc as self"
            );
            assert_eq!(m, self_h.master);
            assert_eq!(id, self_h.local_id);
        }

        // The read through `@.` equals the verbatim self read — byte-for-byte.
        let sel = Selector::parse(".");
        let (t, _, _, _) = resolve_target(&self_tuple, &store, &sel).unwrap();
        assert_eq!(cmd_text(&t), cmd_text(&self_h.term), "@. text == self text");
    }

    /// (b) A SECOND registered session is readable via `@<local>` and `@<sid>` by an
    /// Owner connection, and returns ITS state (its text), not self's.
    #[test]
    fn owner_reads_a_sibling_via_local_and_sid_selectors() {
        let store = session_store::new_store();
        let self_h = registered_session(0, -1, b"I-am-self\r\n");
        let peer_h = registered_session(7, -1, b"I-am-peer\r\n");
        store.write().unwrap().register(self_h.clone());
        store.write().unwrap().register(peer_h.clone());

        let self_tuple: Target = (
            self_h.term.clone(),
            self_h.master,
            self_h.local_id,
            self_h.ctx.clone(),
        );

        let self_text = cmd_text(&self_h.term);
        let peer_text = cmd_text(&peer_h.term);
        assert_ne!(self_text, peer_text, "the two sessions read distinctly");

        // By process-local id.
        let by_local =
            resolve_target(&self_tuple, &store, &Selector::parse("7")).expect("by local");
        assert!(
            Arc::ptr_eq(&by_local.0, &peer_h.term),
            "resolved the peer term"
        );
        assert_eq!(
            cmd_text(&by_local.0),
            peer_text,
            "@7 returns the PEER's state"
        );
        assert_ne!(cmd_text(&by_local.0), self_text, "@7 is NOT self's state");

        // By stable SessionId.
        let by_sid = resolve_target(&self_tuple, &store, &Selector::parse(peer_h.sid.as_str()))
            .expect("by sid");
        assert!(
            Arc::ptr_eq(&by_sid.0, &peer_h.term),
            "@s-... resolved the peer term"
        );
        assert_eq!(
            cmd_text(&by_sid.0),
            peer_text,
            "@s-... returns the PEER's state"
        );

        // Owner is authorized to read the sibling (same trust domain).
        assert!(
            cross_session_authorized(Scope::Owner, "text", &peer_h.ctx),
            "Owner may read a sibling",
        );

        // An unknown selector fails closed (no such session).
        assert!(resolve_target(&self_tuple, &store, &Selector::parse("999")).is_none());
        assert!(resolve_target(&self_tuple, &store, &Selector::parse("s-nope")).is_none());
    }

    /// (c) An Edge connection WITHOUT an authorizing edge into the target is DENIED
    /// `@other` (fail-closed); WITH a granted edge for the right op it is ALLOWED,
    /// and the op-split holds (a read edge cannot write the target).
    #[test]
    fn edge_cross_session_is_fail_closed_without_edge_and_allowed_with_one() {
        let peer_h = registered_session(7, -1, b"peer\r\n");

        // A connection presenting some token NOT recorded in the peer's table.
        let stray = EdgeToken::generate();
        let read_scope = Scope::Edge(stray);
        assert!(
            !cross_session_authorized(read_scope, "text", &peer_h.ctx),
            "no edge in the target table => DENY (fail-closed)",
        );

        // The peer (as Owner of its own table) grants a ReadScreen edge from the
        // controller's source id into itself, returning the bearer token.
        let src = SessionId::new("s-controller");
        let granted = {
            let mut tbl = peer_h.ctx.edges.lock().unwrap();
            tbl.grant(
                src.clone(),
                peer_h.ctx.self_id.clone(),
                Op::ReadScreen,
                peer_h.ctx.nonce,
            )
        };

        // The bearer presenting THAT token is now authorized to READ the peer...
        let auth_read = Scope::Edge(granted);
        assert!(
            cross_session_authorized(auth_read, "text", &peer_h.ctx),
            "a granted ReadScreen edge authorizes a cross-session read",
        );
        // ...but the op-split denies a WRITE through a read edge.
        assert!(
            !cross_session_authorized(auth_read, "send", &peer_h.ctx),
            "a ReadScreen edge may NOT write the target (read != write)",
        );

        // A restarted target (nonce mismatch) fails the SAME edge closed (the
        // confused-deputy guard). Simulate by checking decide_edge against a fresh
        // nonce, which is what a relaunched session would publish.
        let restarted_nonce = LaunchNonce::generate();
        let tbl = peer_h.ctx.edges.lock().unwrap();
        assert_eq!(
            decide_edge(
                &tbl,
                &granted,
                &peer_h.ctx.self_id,
                Op::ReadScreen,
                &restarted_nonce
            ),
            aterm_session::EdgeDecision::Deny,
            "an edge bound to the old nonce fails closed across a restart",
        );
    }

    /// (d) A WRITE verb (`send @<local>`) reaches the TARGET's master only when
    /// authorized: an authorized write lands the bytes on the peer's pipe; the
    /// op-gate denies an unauthorized (read-edge) write before any byte is sent.
    #[test]
    #[cfg(unix)]
    fn cross_session_send_reaches_target_master_only_when_authorized() {
        // The peer's "master" is a pipe; we read back what `send` writes.
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (read_fd, write_fd) = (fds[0], fds[1]);

        let store = session_store::new_store();
        let self_h = registered_session(0, -1, b"");
        let peer_h = registered_session(7, write_fd, b"");
        store.write().unwrap().register(self_h.clone());
        store.write().unwrap().register(peer_h.clone());

        let self_tuple: Target = (
            self_h.term.clone(),
            self_h.master,
            self_h.local_id,
            self_h.ctx.clone(),
        );

        // An Owner (cross-session authorized) resolves the peer and writes to it.
        let target = resolve_target(&self_tuple, &store, &Selector::parse("7")).expect("peer");
        assert!(
            cross_session_authorized(Scope::Owner, "send", &peer_h.ctx),
            "owner write ok"
        );
        assert_eq!(cmd_send(&target.3.sink, "echo-into-peer"), "OK\n");

        // A read-only Edge is denied the SAME write BEFORE any byte is sent (op-gate).
        let read_scope = Scope::Edge(EdgeToken::generate());
        assert!(
            !cross_session_authorized(read_scope, "send", &peer_h.ctx),
            "a read edge may not write the peer",
        );

        // Read back: only the authorized write's bytes reached the peer's master.
        unsafe { libc::close(write_fd) };
        let mut buf = Vec::new();
        let mut reader = unsafe { std::fs::File::from_raw_fd(read_fd) };
        reader.read_to_end(&mut buf).expect("read peer pipe");
        assert_eq!(
            buf, b"echo-into-peer",
            "exactly the authorized write reached the PEER"
        );
    }

    /// The `who` verb is the PRESENCE readout: a session with no driver and no
    /// watcher reads `driving=- watchers=0`; while its turn lease is held it reads
    /// `driving=<id>`, and a live subscriber bumps `watchers`. Proves the hand
    /// (lease) and the eye (subscriber count) are both surfaced.
    #[test]
    fn who_verb_reports_driver_and_watchers() {
        let store = session_store::new_store();
        let root = registered_session(0, -1, b"");
        store.write().unwrap().register(root.clone());
        let subs = subscribe::new_registry();

        // Idle: no driver, no watcher.
        let out = cmd_who(&store, &subs);
        let line = out.lines().nth(1).expect("one session line");
        assert!(
            line.starts_with("0 ") && line.contains("driving=- watchers=0 turns=0"),
            "idle presence: {line}"
        );

        // A held turn lease shows as the driver; a live subscription as a watcher.
        *root.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn(9));
        let _watch = subscribe::SubscriberSet::register(&subs, &[0]);
        let out = cmd_who(&store, &subs);
        let line = out.lines().nth(1).expect("one session line");
        assert!(
            line.contains("driving=9 watchers=1"),
            "driver + watcher surfaced: {line}"
        );

        // A COOPERATIVE drive lease surfaces as `driving=lease:<holder>` (R17), so a
        // raw driver's hold is visible in the same presence readout as a turn.
        *root.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
            holder: "agent-a".to_string(),
            expires_us: crate::metrics::now_us() + 60_000_000,
        });
        let line = cmd_who(&store, &subs)
            .lines()
            .nth(1)
            .expect("one session line")
            .to_string();
        assert!(
            line.contains("driving=lease:agent-a"),
            "cooperative lease surfaced in who: {line}"
        );
    }

    /// The `sessions` verb lists the registry: a single-session store yields exactly
    /// one line == the lone session (the zero-regression base case); a family yields
    /// one line per registered session with parent + state.
    #[test]
    fn sessions_verb_lists_the_registry() {
        let store = session_store::new_store();
        let root = registered_session(0, -1, b"");
        let root_ctx = root.ctx.clone();
        store.write().unwrap().register(root.clone());

        // Base case: one session, one data line.
        let one = cmd_sessions(&root_ctx, &store);
        let mut lines = one.lines();
        assert_eq!(lines.next(), Some("OK 1"), "header counts one session");
        let only = lines.next().expect("one data line");
        assert!(only.starts_with("0 "), "local id 0 first: {only}");
        assert!(only.contains(root.sid.as_str()), "carries the sid: {only}");
        assert!(only.contains(" - alive "), "no parent, alive: {only}");
        assert_eq!(lines.next(), None, "exactly one data line");

        // Family case: a child links to the root and the listing is sorted by local.
        let mut child = registered_session(1, -1, b"");
        child.parent = Some(root.sid.clone());
        store.write().unwrap().register(child.clone());
        let two = cmd_sessions(&root_ctx, &store);
        let mut l = two.lines();
        assert_eq!(l.next(), Some("OK 2"));
        assert!(l.next().unwrap().starts_with("0 "), "root first");
        let child_line = l.next().unwrap();
        assert!(child_line.starts_with("1 "), "child second");
        assert!(
            child_line.contains(root.sid.as_str()),
            "child names its parent sid"
        );
    }

    /// SESSION-METADATA stage 1 — the `meta` verb round-trip: `meta set` stores a
    /// pct-round-trippable UNICODE value, the bare `meta` read reports it (and the
    /// engine title/state alongside), a same-value re-set is NOT a change (no
    /// repaint/notify fan-out), and `meta unset` clears back to `-`. The `sessions`
    /// listing's trailing `meta=` bit tracks whether ANY field is set.
    #[test]
    fn meta_set_get_unset_roundtrip_with_unicode() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"\x1b]2;vim main.rs\x07");
        store.write().unwrap().register(h.clone());

        // Baseline read: engine title present, every user field unset.
        let (read0, changed0) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(!changed0, "a read is never a change");
        assert!(
            read0.starts_with(&format!("OK title={} ", pct_encode("vim main.rs"))),
            "engine title reported pct-encoded: {read0}"
        );
        assert!(read0.contains(" user_title=- "), "unset reads -: {read0}");
        assert!(read0.contains(" description=- "), "unset reads -: {read0}");
        assert!(read0.contains(" icon=- "), "unset reads -: {read0}");
        assert!(read0.contains(" role=- "), "unset reads -: {read0}");
        assert!(read0.contains(" attention=- "), "unset reads -: {read0}");
        assert!(read0.trim_end().ends_with(" state=alive"), "{read0}");
        assert!(
            cmd_sessions(&h.ctx, &store)
                .lines()
                .nth(1)
                .unwrap()
                .ends_with("meta=0"),
            "no user metadata yet -> meta=0"
        );

        // Set a unicode title (spaces + CJK + emoji) — stored verbatim, reported
        // pct-encoded so the reply stays one parseable line.
        let title = "构建 agent ✨";
        let (r, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, &format!("set title {title}"));
        assert_eq!(r, "OK\n");
        assert!(changed, "first set IS a change");
        let (read1, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(
            read1.contains(&format!(" user_title={} ", pct_encode(title))),
            "unicode round-trips through pct: {read1}"
        );
        // The stored value is the exact unicode string (not the encoding).
        assert_eq!(
            h.ctx.meta.lock().unwrap().user_title.as_deref(),
            Some(title)
        );
        // Re-setting the SAME value is a no-change (no repaint/notify fan-out).
        let (_, changed_again) =
            cmd_meta(&h.term, &store, 0, &h.ctx, &format!("set title {title}"));
        assert!(!changed_again, "same-value re-set is not a change");

        // Description + icon fill their slots; sessions now flags meta=1.
        let (_, c1) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            "set description builds the release",
        );
        let (_, c2) = cmd_meta(&h.term, &store, 0, &h.ctx, "set icon 🚀");
        assert!(c1 && c2);
        let (read2, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(read2.contains(&format!(
            " description={} ",
            pct_encode("builds the release")
        )));
        assert!(read2.contains(&format!(" icon={} ", pct_encode("🚀"))));
        assert!(
            cmd_sessions(&h.ctx, &store)
                .lines()
                .nth(1)
                .unwrap()
                .ends_with("meta=1"),
            "user metadata present -> meta=1"
        );

        // The typed role/attention keys ride the same set/read/unset cycle.
        let (_, c3) = cmd_meta(&h.term, &store, 0, &h.ctx, "set role operator");
        let (_, c4) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            "set attention needs human: approval box",
        );
        assert!(c3 && c4);
        let (read_typed, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(read_typed.contains(&format!(" role={} ", pct_encode("operator"))));
        assert!(read_typed.contains(&format!(
            " attention={} ",
            pct_encode("needs human: approval box")
        )));
        // A STORED "-" is not the unset sentinel: it reads back escaped.
        let (_, dash) = cmd_meta(&h.term, &store, 0, &h.ctx, "set role -");
        assert!(dash);
        let (read_dash, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(read_dash.contains(" role=%2D "), "{read_dash}");
        let (_, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "set role operator");
        let (_, cleared) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset attention");
        assert!(cleared);
        let (read_cleared, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(read_cleared.contains(" attention=- "), "{read_cleared}");

        // Unset clears back to '-' (labels fall back down the chain); a second
        // unset of the same field is a no-change.
        let (r, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset title");
        assert_eq!(r, "OK\n");
        assert!(changed);
        let (read3, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(read3.contains(" user_title=- "), "cleared: {read3}");
        let (_, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset title");
        assert!(!changed, "second unset is a no-change");

        // Unknown fields / malformed forms answer honest usage ERRs.
        let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "set colour red");
        assert!(r.starts_with("ERR unknown meta field") && !c);
        let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset colour");
        assert!(r.starts_with("ERR unknown meta field") && !c);
        let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "set title");
        assert!(r.starts_with("ERR usage") && !c, "value required: {r}");
        let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "bogus");
        assert!(r.starts_with("ERR usage") && !c);
    }

    /// SESSION-METADATA stage 1 — the byte caps are HARD refusals (never a silent
    /// truncation): title > 120B, description > 1024B, icon > 64B, role > 64B,
    /// attention > 256B each answer an `ERR … too long` naming the cap, and the
    /// stored value is untouched.
    #[test]
    fn meta_caps_reject_over_cap_values() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let over = |n: usize| "x".repeat(n + 1);

        let (r, changed) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            &format!("set title {}", over(120)),
        );
        assert_eq!(r, "ERR title too long (max 120 bytes)\n");
        assert!(!changed);
        let (r, _) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            &format!("set description {}", over(1024)),
        );
        assert_eq!(r, "ERR description too long (max 1024 bytes)\n");
        let (r, _) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            &format!("set icon {}", over(64)),
        );
        assert_eq!(r, "ERR icon too long (max 64 bytes)\n");
        let (r, _) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            &format!("set role {}", over(64)),
        );
        assert_eq!(r, "ERR role too long (max 64 bytes)\n");
        let (r, _) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            &format!("set attention {}", over(256)),
        );
        assert_eq!(r, "ERR attention too long (max 256 bytes)\n");
        // Nothing was stored by any refused write.
        assert!(!h.ctx.meta.lock().unwrap().any_set());
        // AT-cap values are accepted (the cap is inclusive, after trim).
        let (r, changed) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            &format!("set title {}", "x".repeat(120)),
        );
        assert_eq!(r, "OK\n");
        assert!(changed);
    }

    /// USER metadata crosses into native OS chrome, so terminal/control bytes
    /// that could create a second line or reverse/isolate its visual order are
    /// hard refusals. A rejected write mutates neither metadata nor timeline.
    #[test]
    fn meta_rejects_controls_line_separators_and_bidi_formatting() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let baseline_events = h.ctx.timeline.lock().unwrap().len();

        for hostile in [
            "set title safe\nsecond-line",
            "set description left\u{202e}right",
            "set description one\u{2029}two",
            "set icon \u{2066}🚀\u{2069}",
            "set title hidden\u{200b}suffix",
            "set title soft\u{00ad}hyphen",
            "set title tagged\u{e0001}text",
        ] {
            let (reply, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, hostile);
            assert!(
                reply.contains("single-line") && reply.contains("control, bidi, or invisible"),
                "hostile value gets an explicit refusal: {reply:?}"
            );
            assert!(!changed, "a refused write is not a change: {hostile:?}");
        }
        assert!(!h.ctx.meta.lock().unwrap().any_set());
        assert_eq!(
            h.ctx.timeline.lock().unwrap().len(),
            baseline_events,
            "refusals do not create meta-change events"
        );

        // ZWJ is allowed: rejecting it would break ordinary compound emoji.
        let (reply, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, "set icon 👨‍👩‍👧‍👦");
        assert_eq!(reply, "OK\n");
        assert!(changed);
    }

    /// SESSION-METADATA stage 1 — the `timeline` verb: lifecycle events (spawned /
    /// state-change / title-change / meta-change) are recorded IN ORDER with
    /// monotonic ids by the store mutators + `meta set`, and the verb's `<n>` /
    /// `since=<id>` grammar mirrors `history` exactly.
    #[test]
    fn timeline_records_lifecycle_in_order_and_honors_n_and_since() {
        let store = session_store::new_store();
        let h = registered_session(3, -1, b"");
        store.write().unwrap().register(h.clone()); // -> spawned
        store.write().unwrap().set_title(3, "vim"); // -> title-change
        let (_, changed) = cmd_meta(&h.term, &store, 3, &h.ctx, "set title build agent");
        assert!(changed); // -> meta-change
        store
            .write()
            .unwrap()
            .set_state(3, session_store::SessionState::Exited); // -> state-change
        // A SAME-state re-mark records nothing (change-gated).
        store
            .write()
            .unwrap()
            .set_state(3, session_store::SessionState::Exited);

        let out = cmd_timeline(&h.ctx, "");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[0], "OK 4",
            "exactly one event per actual change: {out}"
        );
        assert!(
            lines[1].starts_with("event 1 t=") && lines[1].contains("kind=spawned state=alive"),
            "birth first: {}",
            lines[1]
        );
        assert!(
            lines[2].contains(&format!("kind=title-change title={}", pct_encode("vim"))),
            "{}",
            lines[2]
        );
        assert!(
            lines[3].contains(&format!(
                "kind=meta-change field=title value={}",
                pct_encode("build agent")
            )),
            "{}",
            lines[3]
        );
        assert!(
            lines[4].contains("kind=state-change state=exited"),
            "{}",
            lines[4]
        );
        // Monotonic ids 1..=4, monotonic timestamps.
        let ids: Vec<u64> = lines[1..]
            .iter()
            .map(|l| l.split_whitespace().nth(1).unwrap().parse().unwrap())
            .collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);

        // `<n>` keeps the LAST n; `since=<id>` is a strict suffix; both compose.
        let last_two = cmd_timeline(&h.ctx, "2");
        assert!(last_two.starts_with("OK 2\n"));
        assert!(last_two.contains("kind=meta-change") && last_two.contains("kind=state-change"));
        let since = cmd_timeline(&h.ctx, "since=3");
        assert_eq!(since.lines().count(), 2, "{since}");
        assert!(since.starts_with("OK 1\n") && since.contains("event 4 "));
        // Malformed args answer a usage ERR, mirroring `history`.
        assert!(cmd_timeline(&h.ctx, "since=bogus").starts_with("ERR usage"));
        assert!(cmd_timeline(&h.ctx, "wat").starts_with("ERR usage"));

        // The death half: deregistration records the final state-change.
        store.write().unwrap().deregister_local(3);
        let after = cmd_timeline(&h.ctx, "since=4");
        assert!(
            after.contains("kind=state-change state=closed"),
            "deregister leaves the closing event: {after}"
        );
    }

    /// A malformed / cross-session selector on a SELF-SCOPED verb (sessions/grant/
    /// revoke/whoami) is rejected — those verbs can never be redirected to act on
    /// another session's table. (The selector PARSE itself is total + fail-closed:
    /// an unknown id resolves to None, not a wrong session.)
    #[test]
    fn self_scoped_verbs_reject_a_target_selector() {
        // A non-self selector is `Local`/`Sid`, which the handle() guard rejects for
        // these verbs. Here we assert the parse classification the guard relies on.
        assert!(matches!(Selector::parse("."), Selector::SelfTok));
        assert!(matches!(Selector::parse(""), Selector::SelfTok));
        assert!(matches!(Selector::parse("7"), Selector::Local(7)));
        assert!(matches!(Selector::parse("s-abc"), Selector::Sid(_)));
    }

    // ── P1.3 subscribe wiring ────────────────────────────────────────────────

    /// Build an [`ActiveHandle`] over a registered session's tuple, so `@.` self
    /// subscribe follows the active tab the same way a self read does.
    fn active_for(h: &SessionHandle) -> ActiveHandle {
        Arc::new(Mutex::new(Some(ActiveSession {
            term: h.term.clone(),
            master: h.master,
            id: h.local_id,
            ctx: h.ctx.clone(),
        })))
    }

    /// Accumulate pushed bytes from a `CtlStream` until `pred` is satisfied by the
    /// accumulated text or a generous deadline passes (so a correctly-silent
    /// producer doesn't hang the test). Frames may arrive split or coalesced across
    /// `read`s — accumulating and matching on substrings makes the assertion robust
    /// to that and to parallel-test scheduling jitter.
    fn read_until(s: &CtlStream, mut acc: String, pred: impl Fn(&str) -> bool) -> String {
        use std::io::Read;
        let mut s = s.try_clone().expect("clone client end");
        s.set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .unwrap();
        // FAILURE bound, not a wait: the loop returns the moment `pred` matches, so a
        // generous deadline costs a passing test nothing and only decides how long a
        // genuine hang takes to report. It must clear the SLOWEST push path by a wide
        // margin — the `sessions` stream surfaces a sibling registration on the bounded
        // 250ms instance-diff tick, and that tick is a background thread. The old 3s
        // (12 ticks) looked generous but is not: under a saturated machine the whole
        // suite stretches ~2x and the tick thread is not scheduled inside the window,
        // so `owner_subscription_keeps_the_instance_sessions_stream` failed having read
        // only its initial DELTA. Scheduling starvation is unbounded in principle;
        // pick a deadline no realistic load can cross.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut buf = [0u8; 8192];
        while !pred(&acc) && std::time::Instant::now() < deadline {
            match s.read(&mut buf) {
                Ok(n) if n > 0 => acc.push_str(&String::from_utf8_lossy(&buf[..n])),
                Ok(_) => break, // EOF
                Err(_) => {}    // timeout: loop until the deadline
            }
        }
        acc
    }

    /// (a) SELF subscribe to `screen`: a write to the term pushes a sid-tagged DELTA
    /// carrying the live text; a subsequent PURE viewport scroll (which never bumps
    /// `content_seq`) pushes NOTHING. End-to-end through `run_subscribe` (auth +
    /// flip) and the push loop over a real socket, with the production notify hook
    /// (the registry `notify`) driving the wake.
    #[test]
    fn subscribe_self_screen_delta_on_write_none_on_scroll() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let active = active_for(&h);
        let registry = subscribe::new_registry();

        let (client, server) = CtlStream::pair().unwrap();
        let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
        let join = std::thread::spawn(move || {
            let mut w = server;
            run_subscribe(
                "subscribe @. screen",
                &active_t,
                &store_t,
                &reg_t,
                Scope::Owner,
                &mut w,
            );
        });

        // The ack confirms the flip to push mode (accumulate past any immediate
        // catch-up frame that may coalesce into the same read).
        let acc = read_until(&client, String::new(), |s| s.contains("OK subscribe 1\n"));
        assert!(acc.contains("OK subscribe 1\n"), "subscribe ack: {acc:?}");

        // Produce real content, then fire the SAME notify the GUI's Wake::Output
        // hook fires. The push loop re-reads the latest grid and emits a DELTA.
        crate::term_lock(&h.term).process(b"hello-live");
        registry.lock().unwrap().notify(0);
        let frame = read_until(&client, acc, |s| s.contains("hello-live"));
        assert!(
            frame.contains("DELTA 0 seq="),
            "screen delta pushed: {frame:?}"
        );
        assert!(
            frame.contains("hello-live"),
            "delta carries live text: {frame:?}"
        );

        // A PURE viewport scroll does not bump content_seq -> no further DELTA even
        // though we notify (a coalesced/spurious wake reads unchanged content).
        crate::term_lock(&h.term).scroll_display(1);
        registry.lock().unwrap().notify(0);
        // POSITIVE-BY-PROXY, deliberately. `read_quiet` is a single 300ms read that
        // returns "" on timeout, so asserting silence directly passes whenever the
        // push thread simply did not run inside the window — and a regression that
        // DID push a scroll-derived delta would pass with it. Instead, follow the
        // scroll with a KNOWN content change and require that the very next DELTA is
        // that change. If the scroll had pushed one it would arrive first and fail
        // this, and the wait for `after-scroll` cannot be satisfied by a starved
        // thread, so load turns into a timeout rather than a false pass.
        crate::term_lock(&h.term).process(b"after-scroll");
        registry.lock().unwrap().notify(0);
        let next = read_until(&client, String::new(), |s| s.contains("after-scroll"));
        let first_delta = next
            .split("DELTA ")
            .nth(1)
            .expect("a delta must follow the content change");
        assert!(
            first_delta.contains("after-scroll"),
            "the first delta after a pure viewport scroll must be the CONTENT change, \
             not a scroll-derived one: {next:?}"
        );

        // Drop the client: the loop's next write fails and it returns (deregister).
        // Windows afunix defers the connection-reset by ONE send (the first
        // post-close send is buffered as success; the next fails with
        // WSAECONNRESET), so keep producing until the loop notices — on Unix the
        // very first post-drop write already fails (EPIPE) and the loop exits on
        // round one.
        drop(client);
        for _ in 0..40 {
            crate::term_lock(&h.term).process(b"x");
            registry.lock().unwrap().notify(0);
            if join.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        join.join()
            .expect("push loop ends cleanly on a dead client");
        assert_eq!(
            registry.lock().unwrap().watched_sessions(),
            0,
            "deregistered on drop"
        );
    }

    /// A push-only client can disappear while every watched terminal is quiet. The
    /// server must discover that from the socket's read side, not from a future
    /// write, or each crash permanently occupies one of the four reserved workers.
    /// Run more sequential crash cycles than the pool size without producing a
    /// single terminal byte; every loop must return and deregister on its own.
    #[test]
    fn quiet_subscription_crashes_reap_past_the_worker_pool_size() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let active = active_for(&h);
        let registry = subscribe::new_registry();

        for cycle in 0..(CONTROL_SUBSCRIPTION_WORKERS + 2) {
            let (client, server) = CtlStream::pair().expect("subscription socket pair");
            let (store_t, active_t, registry_t) = (store.clone(), active.clone(), registry.clone());
            let join = std::thread::spawn(move || {
                let mut server = server;
                run_subscribe_socket(
                    "subscribe @. screen",
                    &active_t,
                    &store_t,
                    &registry_t,
                    Scope::Owner,
                    &mut server,
                );
            });
            let ack = read_until(&client, String::new(), |text| {
                text.contains("OK subscribe 1\n")
            });
            assert!(
                ack.contains("OK subscribe 1\n"),
                "cycle {cycle} entered push mode: {ack:?}"
            );

            // Let the immediate catch-up finish and the server enter a genuinely
            // quiet liveness wait. Dropping earlier could be detected by a pending
            // catch-up write, which would not exercise the HUP-only failure mode.
            std::thread::sleep(std::time::Duration::from_millis(350));
            assert!(
                !join.is_finished(),
                "cycle {cycle} keeps a fully-open quiet peer registered"
            );

            // No output or registry notify follows. Socket EOF is the only fact
            // capable of releasing this worker.
            drop(client);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !join.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(
                join.is_finished(),
                "quiet dead subscriber {cycle} was reaped within one liveness window"
            );
            join.join().expect("subscription loop exits cleanly");
            assert_eq!(
                registry.lock().unwrap().watched_sessions(),
                0,
                "cycle {cycle} released its registry entry"
            );
        }
    }

    /// The protocol requires a live subscriber to keep its write/send half open.
    /// This lets the server distinguish a genuinely quiet reader from a dead or
    /// half-closed one without emitting heartbeat traffic.
    #[test]
    fn subscription_probe_keeps_quiet_peer_and_rejects_write_half_close() {
        let (client, server) = CtlStream::pair().expect("probe socket pair");
        server
            .set_read_timeout(Some(std::time::Duration::from_millis(1)))
            .expect("bound liveness read");
        assert!(
            !subscription_peer_gone(&server),
            "a fully-open quiet peer remains subscribed"
        );

        client
            .shutdown(std::net::Shutdown::Write)
            .expect("client closes its send half");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while !subscription_peer_gone(&server) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            subscription_peer_gone(&server),
            "client write-half close is a protocol-level disconnect"
        );
    }

    /// (b)-deny FAIL-CLOSED: a scoped `Edge` connection that subscribes to a SIBLING
    /// it has NO authorizing edge for gets `ERR denied\n` and never enters push mode
    /// (no partial subscription, no registry entry). Uses a buffer writer since the
    /// denial path returns before the push loop.
    #[test]
    fn subscribe_sibling_without_edge_is_fail_closed() {
        let store = session_store::new_store();
        let self_h = registered_session(0, -1, b"");
        let sib = registered_session(2, -1, b"sibling-screen");
        store.write().unwrap().register(self_h.clone());
        store.write().unwrap().register(sib.clone());
        let active = active_for(&self_h);
        let registry = subscribe::new_registry();

        // An Edge scope carrying a throwaway token with NO grant on the sibling's
        // table => decide_edge denies => fail closed.
        let scope = Scope::Edge(EdgeToken::generate());
        let mut out: Vec<u8> = Vec::new();
        run_subscribe(
            "subscribe @2 screen",
            &active,
            &store,
            &registry,
            scope,
            &mut out,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "ERR denied\n",
            "cross subscribe fail-closed"
        );
        assert_eq!(
            registry.lock().unwrap().watched_sessions(),
            0,
            "no registration on denial"
        );
    }

    /// AUTHORITY SCOPE (audit 4.4): the `sessions` stream is INSTANCE-wide — it diffs
    /// the store's whole live roster — so a per-target `ReadScreen` grant cannot buy
    /// it. An Edge that legitimately authorizes `subscribe` against its own session
    /// (the positive control below) is still refused `sessions`, which proves the
    /// denial comes from the instance gate and not incidentally from the target loop.
    /// The refusal is hard (`ERR denied`), not a silently-empty stream: a push-only
    /// connection that never pushes is indistinguishable from a hang.
    #[test]
    fn sessions_stream_is_denied_to_a_scoped_edge_that_may_read_its_target() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let active = active_for(&h);
        let registry = subscribe::new_registry();

        // A ReadScreen edge granted ON this very session: the per-target gate passes.
        let tok = {
            let mut edges = h.ctx.edges.lock().unwrap();
            edges.grant(h.sid.clone(), h.sid.clone(), Op::ReadScreen, h.nonce)
        };
        let scope = Scope::Edge(tok);
        assert!(
            cross_session_authorized(scope, "subscribe", &h.ctx),
            "positive control: this edge DOES authorize subscribe on its target",
        );

        let mut out: Vec<u8> = Vec::new();
        run_subscribe(
            "subscribe @. screen,sessions",
            &active,
            &store,
            &registry,
            scope,
            &mut out,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "ERR denied\n",
            "the instance-wide sessions stream needs Owner, not a per-target grant",
        );
        assert_eq!(
            registry.lock().unwrap().watched_sessions(),
            0,
            "no registration on denial"
        );
    }

    /// The other half of audit 4.4: Owner KEEPS the `sessions` stream (it already
    /// reads the roster through the `sessions`/`who` verbs), and the stream really
    /// reports a SIBLING the subscriber never named — `@.` watches session 0 while
    /// the `EVENT *` line announces a session registered afterwards. End-to-end
    /// through `run_subscribe` + the push loop, so it also pins that the narrowing
    /// did not sever the wire path.
    #[test]
    fn owner_subscription_keeps_the_instance_sessions_stream() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let active = active_for(&h);
        let registry = subscribe::new_registry();

        let (client, server) = CtlStream::pair().unwrap();
        let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
        let join = std::thread::spawn(move || {
            let mut w = server;
            run_subscribe(
                "subscribe @. screen,sessions",
                &active_t,
                &store_t,
                &reg_t,
                Scope::Owner,
                &mut w,
            );
        });
        // Wait for the IMMEDIATE catch-up frame, not just the ack: the ack is written
        // by `run_subscribe` BEFORE it enters the push loop, whereas the roster
        // watermark is seeded inside the loop. Registering the sibling on the ack
        // alone races the seed — the sibling can land in the baseline and then it is
        // correctly never announced.
        let acc = read_until(&client, String::new(), |s| s.contains("DELTA 0 seq="));
        assert!(acc.contains("OK subscribe 1\n"), "subscribe ack: {acc:?}");
        assert!(acc.contains("DELTA 0 seq="), "catch-up frame: {acc:?}");

        // A sibling spawns. It is NOT a subscribe target and fires no notify, so the
        // instance diff surfaces it on the bounded 250ms tick — which is exactly the
        // roster read that a per-target grant could never authorize.
        let sib = registered_session(2, -1, b"");
        store.write().unwrap().register(sib.clone());
        let seen = read_until(&client, acc, |s| s.contains("session-created"));
        assert!(
            seen.contains(&format!("EVENT * session-created {}\n", sib.sid.as_str())),
            "Owner sees the sibling spawn: {seen:?}"
        );

        // Tear down: drop the client so the loop's next write fails and it returns.
        drop(client);
        for _ in 0..40 {
            crate::term_lock(&h.term).process(b"x");
            registry.lock().unwrap().notify(0);
            if join.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        join.join()
            .expect("push loop ends cleanly on a dead client");
    }

    /// REGRESSION (capability escape): a SELF (`@.`) subscribe must re-verify the edge
    /// against the session active RIGHT NOW — not treat `@.` as unconditionally
    /// allowed. The one global ActiveHandle retargets `@.` to the new frontmost tab on
    /// every switch (`sync_active_session`); a `ReadScreen` edge granted on session B
    /// must NOT be able to read whatever session A became frontmost. (Pre-fix the SELF
    /// target skipped the gate entirely — `is_cross && ...` — so any edge read A.)
    #[test]
    fn subscribe_self_edge_denied_after_active_session_swings() {
        let store = session_store::new_store();
        let a = registered_session(0, -1, b"secret-of-A"); // frontmost; what `@.` resolves to
        let b = registered_session(2, -1, b""); // a DIFFERENT session, where the edge was granted
        store.write().unwrap().register(a.clone());
        let active = active_for(&a);
        let registry = subscribe::new_registry();

        // A ReadScreen edge GRANTED on B's table (op matches subscribe) — but no grant
        // against the now-active A.
        let tok = {
            let mut edges = b.ctx.edges.lock().unwrap();
            edges.grant(a.sid.clone(), b.sid.clone(), Op::ReadScreen, b.nonce)
        };
        let scope = Scope::Edge(tok);
        // Positive control: the edge legitimately authorizes subscribe on its OWN B.
        assert!(
            cross_session_authorized(scope, "subscribe", &b.ctx),
            "B's edge authorizes subscribe against its own session",
        );

        // A SELF subscribe while A is frontmost is DENIED — no partial push, no entry.
        let mut out: Vec<u8> = Vec::new();
        run_subscribe(
            "subscribe @. screen",
            &active,
            &store,
            &registry,
            scope,
            &mut out,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "ERR denied\n",
            "B's edge must NOT subscribe to the swung-to active session A",
        );
        assert_eq!(
            registry.lock().unwrap().watched_sessions(),
            0,
            "no registration on denial"
        );
    }

    /// (b)-allow: the SAME scoped `Edge`, once it holds a `ReadScreen` grant on the
    /// sibling's edge table (minted by the owner, presented as the connection token),
    /// subscribes to the sibling and receives pushed sid-tagged DELTAs. Mirrors the
    /// cross-session read authorization path exactly (`decide_edge` against the
    /// TARGET table + nonce).
    #[test]
    fn subscribe_sibling_with_read_edge_pushes_deltas() {
        let store = session_store::new_store();
        let self_h = registered_session(0, -1, b"");
        let sib = registered_session(2, -1, b"");
        store.write().unwrap().register(self_h.clone());
        store.write().unwrap().register(sib.clone());
        let active = active_for(&self_h);
        let registry = subscribe::new_registry();

        // Owner mints a ReadScreen edge (self -> sibling). The bearer presents that
        // token as the connection's Edge scope.
        let tok = {
            let mut edges = sib.ctx.edges.lock().unwrap();
            edges.grant(
                self_h.sid.clone(),
                sib.sid.clone(),
                Op::ReadScreen,
                sib.nonce,
            )
        };
        // Sanity: the gate would PERMIT this exact (token, target) pair.
        assert!(
            cross_session_authorized(Scope::Edge(tok), "subscribe", &sib.ctx),
            "minted edge authorizes the cross subscribe",
        );

        let (client, server) = CtlStream::pair().unwrap();
        let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
        let scope = Scope::Edge(tok);
        let join = std::thread::spawn(move || {
            let mut w = server;
            run_subscribe(
                "subscribe @2 screen",
                &active_t,
                &store_t,
                &reg_t,
                scope,
                &mut w,
            );
        });

        let ack = read_until(&client, String::new(), |s| s.contains("OK subscribe 1\n"));
        assert!(
            ack.contains("OK subscribe 1\n"),
            "edge subscribe authorized: {ack:?}"
        );

        crate::term_lock(&sib.term).process(b"from-sibling");
        registry.lock().unwrap().notify(2);
        let frame = read_until(&client, ack, |s| s.contains("from-sibling"));
        assert!(
            frame.contains("DELTA 2 seq="),
            "sibling delta tagged with its sid: {frame:?}"
        );
        assert!(
            frame.contains("from-sibling"),
            "carries the sibling's screen: {frame:?}"
        );

        // Windows afunix defers the connection-reset by ONE send (see
        // `subscribe_self_screen_delta_on_write_none_on_scroll`): keep producing
        // until the loop notices; Unix exits on round one (EPIPE).
        drop(client);
        for _ in 0..40 {
            crate::term_lock(&sib.term).process(b"x");
            registry.lock().unwrap().notify(2);
            if join.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        join.join().expect("push loop ends on a dead client");
    }

    /// (d) MULTIPLEX: one connection subscribing to `@0,@2` receives DELTAs tagged
    /// with EACH session's own sid, so the client demultiplexes by the leading token.
    #[test]
    fn subscribe_multiplex_two_sids_tags_each() {
        let store = session_store::new_store();
        let a = registered_session(0, -1, b"");
        let b = registered_session(2, -1, b"");
        store.write().unwrap().register(a.clone());
        store.write().unwrap().register(b.clone());
        let active = active_for(&a);
        let registry = subscribe::new_registry();

        let (client, server) = CtlStream::pair().unwrap();
        let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
        let join = std::thread::spawn(move || {
            let mut w = server;
            // `@0` is self (active), `@2` a sibling; Owner reaches both.
            run_subscribe(
                "subscribe @0,@2 screen",
                &active_t,
                &store_t,
                &reg_t,
                Scope::Owner,
                &mut w,
            );
        });
        // The ack carries the channel map: `OK subscribe 2` + one `sub <local> <sid>`
        // line per target, so the client can demux the compact <local> frame tags
        // back to the sids it subscribed with.
        let a_sid = a.sid.as_str().to_string();
        let b_sid = b.sid.as_str().to_string();
        let ack = read_until(&client, String::new(), |s| {
            s.contains(&a_sid) && s.contains(&b_sid)
        });
        assert!(
            ack.contains("OK subscribe 2\n"),
            "two targets acked: {ack:?}"
        );
        assert!(
            ack.contains(&format!("sub 0 {a_sid}\n")),
            "channel 0 maps to its sid: {ack:?}"
        );
        assert!(
            ack.contains(&format!("sub 2 {b_sid}\n")),
            "channel 2 maps to its sid: {ack:?}"
        );

        crate::term_lock(&a.term).process(b"AAA");
        crate::term_lock(&b.term).process(b"BBB");
        registry.lock().unwrap().notify(0);
        registry.lock().unwrap().notify(2);
        // Accumulate until BOTH sids' deltas (with their text) have shown up.
        let seen = read_until(&client, ack, |s| s.contains("AAA") && s.contains("BBB"));
        assert!(
            seen.contains("DELTA 0 ") && seen.contains("AAA"),
            "sid 0 frame: {seen:?}"
        );
        assert!(
            seen.contains("DELTA 2 ") && seen.contains("BBB"),
            "sid 2 frame: {seen:?}"
        );

        // Windows afunix defers the connection-reset by ONE send (see
        // `subscribe_self_screen_delta_on_write_none_on_scroll`): keep producing
        // until the loop notices; Unix exits on round one (EPIPE).
        drop(client);
        for _ in 0..40 {
            crate::term_lock(&a.term).process(b"x");
            registry.lock().unwrap().notify(0);
            if join.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        join.join()
            .expect("multiplex push loop ends on a dead client");
    }

    /// R7 default-to-self: `subscribe <streams>` with NO `@<sel>` targets the
    /// connection's own session (`@.`), exactly like every other verb's selectorless
    /// form. The first token is a valid stream list, so it is the streams — not a
    /// selector — and the target resolves to self.
    #[test]
    fn subscribe_without_selector_defaults_to_self() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let active = active_for(&h);
        let registry = subscribe::new_registry();

        let (client, server) = CtlStream::pair().unwrap();
        let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
        let join = std::thread::spawn(move || {
            let mut w = server;
            // NOTE: no `@<sel>` — just the stream list. Must resolve to self (`@.`).
            run_subscribe(
                "subscribe screen",
                &active_t,
                &store_t,
                &reg_t,
                Scope::Owner,
                &mut w,
            );
        });

        let h_sid = h.sid.as_str().to_string();
        let ack = read_until(&client, String::new(), |s| s.contains("OK subscribe 1\n"));
        assert!(
            ack.contains("OK subscribe 1\n"),
            "selectorless subscribe acks one (self) target: {ack:?}"
        );
        assert!(
            ack.contains(&format!("sub 0 {h_sid}\n")),
            "the single target maps to the connection's own sid: {ack:?}"
        );

        crate::term_lock(&h.term).process(b"self-default");
        registry.lock().unwrap().notify(0);
        let frame = read_until(&client, ack, |s| s.contains("self-default"));
        assert!(
            frame.contains("DELTA 0 seq=") && frame.contains("self-default"),
            "self delta pushed for the selectorless subscribe: {frame:?}"
        );

        drop(client);
        for _ in 0..40 {
            crate::term_lock(&h.term).process(b"x");
            registry.lock().unwrap().notify(0);
            if join.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        join.join()
            .expect("push loop ends cleanly on a dead client");
    }

    /// R4 per-session resume anchors: `since=`/`since-turn=`/`since-block=` name a
    /// point in ONE session's monotonic space, so they are rejected against a
    /// multi-target fan-out (each session has its own space) rather than silently
    /// seeding every target from one session's watermark. This errors BEFORE the
    /// push loop, so a synchronous `Vec<u8>` writer captures the `ERR` directly.
    #[test]
    fn subscribe_resume_anchor_rejected_on_multi_target() {
        let store = session_store::new_store();
        let a = registered_session(0, -1, b"");
        let b = registered_session(2, -1, b"");
        store.write().unwrap().register(a.clone());
        store.write().unwrap().register(b.clone());
        let active = active_for(&a);
        let registry = subscribe::new_registry();

        let mut out: Vec<u8> = Vec::new();
        run_subscribe(
            "subscribe @0,@2 screen since=5",
            &active,
            &store,
            &registry,
            Scope::Owner,
            &mut out,
        );
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.starts_with("ERR ") && s.contains("single target"),
            "multi-target resume anchor is rejected: {s:?}"
        );
        // And it never entered the push loop (no ack), so no subscriber registered.
        assert_eq!(
            registry.lock().unwrap().watched_sessions(),
            0,
            "rejected subscribe registers nothing"
        );
    }

    /// (c) A STALLED subscriber (its socket buffer full, never drained) cannot block
    /// or backpressure the PRODUCER: the producing session's `content_seq` keeps
    /// advancing freely while a subscription is registered and never `wait`ed on.
    /// This is the registry-level guarantee the GUI's one-line notify hook relies on
    /// — `notify` is a single-slot `try_send`, O(1) and infallible.
    #[test]
    fn stalled_subscriber_never_blocks_producer_content_seq() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let registry = subscribe::new_registry();

        // Register a subscriber for session 0 and NEVER wait() on it (wedged).
        let _wedged = subscribe::SubscriberSet::register(&registry, &[0]);

        let before = crate::term_lock(&h.term).content_seq();
        let start = std::time::Instant::now();
        // Drive a flood of producer output + the matching notify hook. If a stalled
        // subscriber could backpressure, this would stall; it must stay fast.
        for _ in 0..2000 {
            crate::term_lock(&h.term).process(b"x");
            registry.lock().unwrap().notify(0);
        }
        let after = crate::term_lock(&h.term).content_seq();
        assert!(
            after > before,
            "producer content_seq advanced past a stalled subscriber"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "producer not blocked"
        );
    }

    // ── wf3: --json read mode + edges/family/feed-bin/ready verbs ─────────────

    /// A minimal JSON spot-checker: confirms a string is a balanced JSON object
    /// (braces/brackets nest, strings are closed) so the schema tests assert on
    /// shape without a full parser dependency. Returns the contained substring of
    /// the FIRST top-level object for convenience.
    fn assert_balanced_json(s: &str) {
        let mut depth: i32 = 0;
        let mut in_str = false;
        let mut esc = false;
        for c in s.chars() {
            if in_str {
                if esc {
                    esc = false;
                } else if c == '\\' {
                    esc = true;
                } else if c == '"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0, "JSON brace underflow in {s:?}");
        }
        assert_eq!(depth, 0, "unbalanced JSON braces in {s:?}");
        assert!(!in_str, "unterminated JSON string in {s:?}");
    }

    /// `json_escape` produces RFC 8259-valid string bodies: quotes/backslashes and
    /// the whitespace controls get two-char escapes, other C0 bytes get `\u00XX`,
    /// and ordinary (incl. non-ASCII) text is verbatim.
    #[test]
    fn json_escape_handles_quotes_controls_and_unicode() {
        assert_eq!(json_escape("plain"), "plain");
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(json_escape("tab\tnl\ncr\r"), "tab\\tnl\\ncr\\r");
        assert_eq!(json_escape("\u{0001}"), "\\u0001");
        // Non-ASCII is emitted verbatim (JSON strings are UTF-8).
        assert_eq!(json_escape("café 日本"), "café 日本");
    }

    /// `text --json` emits the documented schema: a `rows` array whose entries are
    /// the SAME grapheme-faithful tail-trimmed lines the TEXT form emits, plus a
    /// `cursor` object, `dims`, and the `seq` (content_seq). The text path is
    /// byte-identical when the flag is absent.
    #[test]
    fn text_json_mode_matches_text_rows_and_carries_cursor_seq() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term.lock()
            .unwrap()
            .process(b"line-zero\r\nsecond \"quoted\"");

        // Text form (the byte-identical baseline) and JSON form.
        let text = cmd_text(&term);
        let json = cmd_text_json(&term);

        // Framing: `OK 1\n<json>\n` (one body line), and the body is balanced JSON.
        assert!(json.starts_with("OK 1\n"), "json framing: {json}");
        let body = json.strip_prefix("OK 1\n").unwrap().trim_end();
        assert_balanced_json(body);

        // Rows carry the same visible content as the text form's data lines.
        let text_rows: Vec<&str> = text.lines().skip(1).collect();
        assert!(body.contains("\"rows\":["), "has rows array: {body}");
        assert!(
            body.contains(&format!("\"{}\"", text_rows[0])),
            "row0 present: {body}"
        );
        // The quote in row 1 is escaped in JSON, NOT in text.
        assert!(
            text_rows[1].contains("second \"quoted\""),
            "text keeps raw quote"
        );
        assert!(
            body.contains("second \\\"quoted\\\""),
            "json escapes the quote: {body}"
        );

        // cursor + dims + seq members are present and consistent with the verbs.
        let c = term.lock().unwrap().cursor();
        assert!(
            body.contains(&format!("\"row\":{}", c.row)),
            "cursor row: {body}"
        );
        assert!(
            body.contains("\"dims\":{\"rows\":24,\"cols\":80}"),
            "dims: {body}"
        );
        assert!(body.contains("\"seq\":"), "carries content_seq: {body}");
    }

    /// The `--json`/`json` flag is parsed off `rest` additively: a line without it
    /// is byte-identical, and the flag is stripped from the remainder so the verb's
    /// own positional parse runs unchanged (e.g. `blocks 1 --json`).
    #[test]
    fn take_json_flag_is_additive_and_strips_the_flag() {
        assert_eq!(take_json_flag(""), (false, String::new()));
        assert_eq!(take_json_flag("1"), (false, "1".to_string()));
        assert_eq!(take_json_flag("--json"), (true, String::new()));
        assert_eq!(take_json_flag("1 --json"), (true, "1".to_string()));
        assert_eq!(take_json_flag("json 1"), (true, "1".to_string()));
    }

    /// An EXPLICIT `--json`/`json` request on a verb with no structured form is
    /// an honest ERR (exact grammar), never a silent text fallback — while a
    /// payload-bearing verb, for which `json` is legitimate argument DATA, falls
    /// through untouched.
    #[test]
    fn explicit_json_on_unsupported_verb_is_an_honest_err() {
        assert_eq!(
            json_unsupported("modes"),
            Some("ERR json: not supported for modes\n".to_string())
        );
        for v in ["selection", "colors", "cell", "line", "history", "wait"] {
            assert!(
                json_unsupported(v).is_some(),
                "{v} must refuse an explicit json request"
            );
        }
        // Payload-bearing verbs keep the token as data (`send --json` writes the
        // literal flag to the PTY; `search json` searches for it) must fall through.
        for v in ["send", "paste", "feed", "turn", "search", "await"] {
            assert_eq!(json_unsupported(v), None, "{v} must fall through");
        }
        // The json-CAPABLE verbs never reach the fallback (their emitters answer
        // first), so the allowlist must not shadow them either.
        for v in [
            "text", "screen", "cursor", "dims", "metrics", "blocks", "edges", "grants",
        ] {
            assert_eq!(json_unsupported(v), None, "{v} has a real json form");
        }
    }

    /// `cursor --json` / `dims --json` / `blocks --json` round-trip the same data as
    /// their text forms in a balanced-JSON body.
    #[test]
    fn cursor_dims_blocks_json_schemas() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let reg = subscribe::new_registry();
        let host = GuiHost::new(0, &term, None, &reg);
        // cursor: row/col/visible/style.
        let cj = cmd_cursor_json(&term);
        let cbody = cj.strip_prefix("OK 1\n").unwrap().trim_end();
        assert_balanced_json(cbody);
        assert!(
            cbody.contains("\"row\":0") && cbody.contains("\"style\":\"blinking_block\""),
            "{cbody}"
        );

        // dims: the compatible leading grid facts plus the live
        // cell/frame/surface placement record.
        let dims = crate::App::headless_for_test().dims_snapshot(0, 24, 80);
        let dj = serialize_dims_json(&dims);
        let dbody = dj.strip_prefix("OK 1\n").unwrap().trim_end();
        assert_balanced_json(dbody);
        assert!(
            dbody.contains(&format!(
                "\"rows\":24,\"cols\":80,\"pixel_w\":{},\"pixel_h\":{}",
                dims.pixel_w, dims.pixel_h
            )),
            "{dbody}"
        );
        assert!(
            dbody.contains(&format!(
                "\"cell_w\":{},\"cell_h\":{}",
                dims.cell_w, dims.cell_h
            )) && dbody.contains(&format!(
                "\"frame_w\":{},\"frame_h\":{},\"surface_w\":{},\"surface_h\":{}",
                dims.frame_w, dims.frame_h, dims.surface_w, dims.surface_h
            )),
            "{dbody}"
        );
        let dt = serialize_dims(&dims);
        let leading = format!("OK 24 80 {} {} ", dims.pixel_w, dims.pixel_h);
        assert!(dt.starts_with(&leading), "compatible leading fields: {dt}");
        assert!(
            dt.contains("geometry=headless")
                && dt.contains("band_left=0")
                && dt.contains("crop_right=0")
                && dt.contains("present_retry_state=ready")
                && dt.contains("present_retry_in_ms=none"),
            "{dt}"
        );
        assert!(
            dbody.contains("\"present_retry_state\":\"ready\"")
                && dbody.contains("\"present_retry_count\":0")
                && dbody.contains("\"present_retry_in_ms\":null"),
            "{dbody}"
        );

        // blocks: two OSC-133 blocks -> a `blocks` array; absent rows are JSON null.
        term.lock().unwrap().process(
            b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07",
        );
        let bj = cmd_blocks_json(&host, 0, "");
        let bbody = bj.strip_prefix("OK 1\n").unwrap().trim_end();
        assert_balanced_json(bbody);
        assert!(bbody.contains("\"blocks\":[{"), "blocks array: {bbody}");
        assert!(
            bbody.contains("\"exit\":0") && bbody.contains("\"cmdline\":\"echo hi\""),
            "{bbody}"
        );
        assert!(bbody.contains("\"state\":\"complete\""), "{bbody}");
    }

    /// `edges`/`grants` lists this session's inbound EdgeTable rows as
    /// `<src> <dst> <op>`, sorted, WITHOUT ever leaking the bearer token; the JSON
    /// form carries the same triples. An empty table is `OK 0`.
    #[test]
    fn edges_verb_lists_capability_edges_without_tokens() {
        let ctx = test_ctx();
        assert_eq!(cmd_edges(&ctx), "OK 0\n", "no edges yet");

        // Mint two edges into this session (the data `grant` records).
        let tok = {
            let mut tbl = ctx.edges.lock().unwrap();
            tbl.grant(
                SessionId::new("s-src-a"),
                ctx.self_id.clone(),
                Op::ReadScreen,
                ctx.nonce,
            );
            tbl.grant(
                SessionId::new("s-src-b"),
                ctx.self_id.clone(),
                Op::WriteInput,
                ctx.nonce,
            )
        };
        let out = cmd_edges(&ctx);
        let mut lines = out.lines();
        assert_eq!(
            lines.next(),
            Some("OK 2"),
            "header counts both edges: {out}"
        );
        // Sorted by (src, op): s-src-a read-screen, then s-src-b write-input.
        let l1 = lines.next().unwrap();
        let l2 = lines.next().unwrap();
        assert!(
            l1.starts_with("s-src-a ") && l1.ends_with(" read-screen"),
            "edge 1: {l1}"
        );
        assert!(
            l2.starts_with("s-src-b ") && l2.ends_with(" write-input"),
            "edge 2: {l2}"
        );
        // The dst column is always THIS session.
        assert!(l1.contains(ctx.self_id.as_str()), "dst is self: {l1}");
        // The secret token NEVER appears in the listing.
        assert!(
            !out.contains(&tok.to_hex()),
            "edge token must not leak: {out}"
        );

        // JSON form: same triples, balanced, no token.
        let j = cmd_edges_json(&ctx);
        let body = j.strip_prefix("OK 1\n").unwrap().trim_end();
        assert_balanced_json(body);
        assert!(
            body.contains("\"src\":\"s-src-a\"") && body.contains("\"op\":\"read-screen\""),
            "{body}"
        );
        assert!(
            !body.contains(&tok.to_hex()),
            "json must not leak the token: {body}"
        );
    }

    /// `family` emits the hierarchy for a node: a `self` line, a `parent` line
    /// (`-` for a root), and a `child` line per direct child (sorted by local id).
    /// An explicit `<sid>` argument is Owner-only; the no-arg form is scoped to the
    /// resolved session.
    #[test]
    fn family_verb_emits_parent_and_children() {
        let store = session_store::new_store();
        let root = registered_session(0, -1, b"");
        let root_ctx = root.ctx.clone();
        let mut child_a = registered_session(1, -1, b"");
        child_a.parent = Some(root.sid.clone());
        let mut child_b = registered_session(2, -1, b"");
        child_b.parent = Some(root.sid.clone());
        store.write().unwrap().register(root.clone());
        store.write().unwrap().register(child_a.clone());
        store.write().unwrap().register(child_b.clone());

        // No-arg form from the ROOT's ctx (Owner): self=root, parent=-, two children.
        let out = cmd_family(&root_ctx, &store, Scope::Owner, "");
        let mut lines = out.lines();
        assert_eq!(
            lines.next(),
            Some("OK 4"),
            "counted header (self+parent+2 children): {out}"
        );
        let self_line = lines.next().unwrap();
        assert!(
            self_line.starts_with(&format!("self {} ", root.sid.as_str())),
            "self: {self_line}"
        );
        assert_eq!(
            lines.next(),
            Some("parent - - -"),
            "root has no parent: {out}"
        );
        let kids: Vec<&str> = lines.collect();
        assert_eq!(kids.len(), 2, "two children: {out}");
        assert!(
            kids[0].starts_with(&format!("child {} ", child_a.sid.as_str())),
            "child a: {out}"
        );
        assert!(
            kids[1].starts_with(&format!("child {} ", child_b.sid.as_str())),
            "child b: {out}"
        );

        // Explicit `<sid>` of a child (Owner): self=child, parent=root, no children.
        let cout = cmd_family(&root_ctx, &store, Scope::Owner, child_a.sid.as_str());
        assert!(
            cout.contains(&format!("self {} ", child_a.sid.as_str())),
            "child self: {cout}"
        );
        assert!(
            cout.contains(&format!("parent {} ", root.sid.as_str())),
            "child parent=root: {cout}"
        );
        assert!(!cout.contains("\nchild "), "a leaf has no children: {cout}");

        // An explicit sid is OWNER-ONLY: a scoped Edge is denied (cannot enumerate
        // arbitrary trees).
        let edge = Scope::Edge(EdgeToken::generate());
        assert_eq!(
            cmd_family(&root_ctx, &store, edge, child_a.sid.as_str()),
            "ERR denied\n",
            "explicit-sid family is owner-only",
        );
        // The no-arg form (resolved session) is allowed for an Edge (already gated).
        assert!(
            cmd_family(&root_ctx, &store, edge, "").starts_with("OK "),
            "no-arg edge ok"
        );

        // An unknown sid fails closed.
        assert_eq!(
            cmd_family(&root_ctx, &store, Scope::Owner, "s-nope"),
            "ERR no such session\n"
        );
    }

    // ---- session connections: the §6 wire verbs ---------------------------------

    /// Register two sessions and return `(store, private conn store, src, dst)`
    /// — the fixture every connection-verb test starts from. The PRIVATE record
    /// store keeps these tests off the process-wide singleton.
    fn connection_fixture() -> (
        crate::session_store::Store,
        crate::connections::ConnectionStore,
        SessionHandle,
        SessionHandle,
    ) {
        let store = session_store::new_store();
        let a = registered_session(1, -1, b"");
        let b = registered_session(2, -1, b"");
        store.write().unwrap().register(a.clone());
        store.write().unwrap().register(b.clone());
        (store, crate::connections::new_connection_store(), a, b)
    }

    /// The token hex bound to `op=` on a `connect` reply line.
    fn reply_token(reply: &str, op: &str) -> Option<EdgeToken> {
        reply
            .split_whitespace()
            .find_map(|t| t.strip_prefix(&format!("{op}=")))
            .and_then(EdgeToken::from_hex)
    }

    /// `connect dst= src=` (default kind=both) mints the three human-fidelity
    /// rows into the DESTINATION's table, replies every minted `op=<hex>`, and
    /// the round trip is visible in the `edges` listing (token-free).
    #[test]
    fn connect_verb_round_trip_is_visible_in_edges() {
        let (store, conn, a, b) = connection_fixture();
        let out = cmd_connect_in(
            &conn,
            &store,
            Scope::Owner,
            &format!("dst={} src={}", b.sid.as_str(), a.sid.as_str()),
        );
        assert!(
            out.starts_with("OK read-screen="),
            "reply leads with the pull op: {out}"
        );
        for op in ["read-screen", "write-input", "signal"] {
            assert!(
                reply_token(&out, op).is_some(),
                "{op}=<hex> must be delivered: {out}"
            );
        }
        // The delivered write token is a REAL edge on the destination.
        let tok = reply_token(&out, "write-input").unwrap();
        assert!(
            decide_edge(
                &b.ctx.edges.lock().unwrap(),
                &tok,
                &b.sid,
                Op::WriteInput,
                &b.ctx.nonce
            )
            .is_permitted(),
            "the delivered token authorizes"
        );
        // Visible in `edges` on the destination — triples only, no hex.
        let edges = cmd_edges(&b.ctx);
        assert!(edges.starts_with("OK 3\n"), "{edges}");
        assert!(
            edges.contains(&format!("{} {} read-screen", a.sid.as_str(), b.sid.as_str())),
            "{edges}"
        );
        assert!(!edges.contains(&tok.to_hex()), "no token leak: {edges}");
        // A scoped edge cannot connect (Owner-only, also gated by the table).
        assert_eq!(
            cmd_connect_in(&conn, &store, edge(), "dst=s-x src=s-y"),
            "ERR denied\n"
        );
    }

    /// DECLARATIVE set semantics on the wire (design §2.5/§9): a same-kind
    /// re-connect replies the SAME live tokens (no churn); a pull→push re-kind
    /// leaves exactly the push rows — the replaced pull row is gone, so no
    /// observer ever reads both kinds at once via `edges`.
    #[test]
    fn connect_verb_is_declarative_across_kind_transitions() {
        let (store, conn, a, b) = connection_fixture();
        let line = |kind: &str| format!("dst={} src={} kind={kind}", b.sid.as_str(), a.sid.as_str());

        let first = cmd_connect_in(&conn, &store, Scope::Owner, &line("pull"));
        assert!(first.starts_with("OK read-screen="), "{first}");
        assert!(!first.contains("write-input="), "pull mints no push: {first}");
        // Idempotent re-connect: byte-identical reply — the original token stays.
        let again = cmd_connect_in(&conn, &store, Scope::Owner, &line("pull"));
        assert_eq!(first, again, "same-kind re-connect must not rotate tokens");

        // Re-kind pull→push: ONE atomic transition; `edges` shows exactly push.
        let pull_tok = reply_token(&first, "read-screen").unwrap();
        let pushed = cmd_connect_in(&conn, &store, Scope::Owner, &line("push"));
        assert!(!pushed.contains("read-screen="), "{pushed}");
        assert!(
            pushed.contains("write-input=") && pushed.contains("signal="),
            "{pushed}"
        );
        let edges = cmd_edges(&b.ctx);
        assert!(edges.starts_with("OK 2\n"), "exactly the push rows: {edges}");
        assert!(
            !edges.contains("read-screen"),
            "pull must not survive the transition: {edges}"
        );
        assert_eq!(
            decide_edge(
                &b.ctx.edges.lock().unwrap(),
                &pull_tok,
                &b.sid,
                Op::ReadScreen,
                &b.ctx.nonce
            ),
            aterm_session::EdgeDecision::Deny,
            "the replaced kind's token dies in the transition"
        );
    }

    /// The `connect` argument fences: usage, self-loop, unknown/exited dst.
    #[test]
    fn connect_verb_arguments_fail_closed() {
        let (store, conn, a, b) = connection_fixture();
        for bad in [
            "",
            "dst=s-x",
            "src=s-x",
            "dst=s-x src=s-y kind=bogus",
            "dst= src=s-y",
            "dst=s-x src=s-y extra",
        ] {
            assert!(
                cmd_connect_in(&conn, &store, Scope::Owner, bad).starts_with("ERR usage"),
                "{bad:?} must be a usage error"
            );
        }
        assert_eq!(
            cmd_connect_in(
                &conn,
                &store,
                Scope::Owner,
                &format!("dst={0} src={0}", a.sid.as_str())
            ),
            "ERR self-loop\n"
        );
        assert_eq!(
            cmd_connect_in(&conn, &store, Scope::Owner, "dst=s-nope src=s-any"),
            "ERR no such session\n",
            "dst must be registered"
        );
        // An Exited dst refuses the mint (its nonce already fails closed).
        let mut dead = registered_session(9, -1, b"");
        dead.state = SessionState::Exited;
        let dead_sid = dead.sid.clone();
        store.write().unwrap().register(dead);
        assert_eq!(
            cmd_connect_in(
                &conn,
                &store,
                Scope::Owner,
                &format!("dst={} src={}", dead_sid.as_str(), b.sid.as_str())
            ),
            "ERR exited\n"
        );
        // Nothing above left residue.
        assert!(conn.records().is_empty());
    }

    /// `disconnect … kind=pull` removes ONLY the read-screen row of a recorded
    /// connection; the bare form dissolves the remainder; an unknown pair fails
    /// closed; unrecorded wire-grant rows dissolve through the op-filtered
    /// sweep fallback.
    #[test]
    fn disconnect_verb_kind_filters_and_sweeps_unrecorded_grants() {
        let (store, conn, a, b) = connection_fixture();
        let pair = format!("dst={} src={}", b.sid.as_str(), a.sid.as_str());
        assert!(
            cmd_connect_in(&conn, &store, Scope::Owner, &pair).starts_with("OK"),
            "fixture connect"
        );

        // kind=pull: exactly the read row goes; push stays live.
        assert_eq!(
            cmd_disconnect_in(&conn, &store, Scope::Owner, &format!("{pair} kind=pull")),
            "OK 1\n"
        );
        let edges = cmd_edges(&b.ctx);
        assert!(
            edges.starts_with("OK 2\n") && !edges.contains("read-screen"),
            "only read-screen removed: {edges}"
        );

        // The bare form dissolves the rest; a repeat fails closed.
        assert_eq!(
            cmd_disconnect_in(&conn, &store, Scope::Owner, &pair),
            "OK 2\n"
        );
        assert_eq!(cmd_edges(&b.ctx), "OK 0\n");
        assert_eq!(
            cmd_disconnect_in(&conn, &store, Scope::Owner, &pair),
            "ERR no such connection\n"
        );

        // Wire-granted rows with NO record: the op-filtered sweep fallback.
        let c = SessionId::new("s-wire-src");
        {
            let mut tbl = b.ctx.edges.lock().unwrap();
            let _ = tbl.grant(c.clone(), b.sid.clone(), Op::ReadScreen, b.ctx.nonce);
            let _ = tbl.grant(c.clone(), b.sid.clone(), Op::WriteInput, b.ctx.nonce);
        }
        let wire_pair = format!("dst={} src={}", b.sid.as_str(), c.as_str());
        assert_eq!(
            cmd_disconnect_in(&conn, &store, Scope::Owner, &format!("{wire_pair} kind=push")),
            "OK 1\n",
            "push sweep takes only write-input"
        );
        let edges = cmd_edges(&b.ctx);
        assert!(
            edges.starts_with("OK 1\n") && edges.contains("read-screen"),
            "{edges}"
        );
        assert_eq!(
            cmd_disconnect_in(&conn, &store, Scope::Owner, &wire_pair),
            "OK 1\n"
        );
        // A scoped edge cannot disconnect; unknown dst fails closed.
        assert_eq!(
            cmd_disconnect_in(&conn, &store, edge(), &pair),
            "ERR denied\n"
        );
        assert_eq!(
            cmd_disconnect_in(&conn, &store, Scope::Owner, "dst=s-nope src=s-x"),
            "ERR no such session\n"
        );
    }

    /// `flows` lists a minted triple across the whole instance (Owner-only), and
    /// `flows --json` groups the pair's ops — no token ever appears.
    #[test]
    fn flows_verb_lists_the_aggregated_graph() {
        let (store, conn, a, b) = connection_fixture();
        assert_eq!(cmd_flows(&store, Scope::Owner, ""), "OK 0\n", "empty fabric");
        let out = cmd_connect_in(
            &conn,
            &store,
            Scope::Owner,
            &format!("dst={} src={} kind=pull", b.sid.as_str(), a.sid.as_str()),
        );
        let tok = reply_token(&out, "read-screen").unwrap();

        assert_eq!(
            cmd_flows(&store, Scope::Owner, ""),
            format!("OK 1\n{} {} read-screen\n", a.sid.as_str(), b.sid.as_str())
        );
        let json = cmd_flows(&store, Scope::Owner, "--json");
        let body = json.strip_prefix("OK 1\n").expect("json_ok framing");
        assert!(body.contains("\"flows\":[{"), "{body}");
        assert!(
            body.contains(&format!("\"src\":\"{}\"", a.sid.as_str()))
                && body.contains(&format!("\"dst\":\"{}\"", b.sid.as_str()))
                && body.contains("\"ops\":[\"read-screen\"]"),
            "{body}"
        );
        assert!(!json.contains(&tok.to_hex()), "no token leak: {json}");

        // Owner-only + strict flags.
        assert_eq!(cmd_flows(&store, edge(), ""), "ERR denied\n");
        assert!(cmd_flows(&store, Scope::Owner, "bogus").starts_with("ERR usage"));
    }

    /// `family` gains the §6 discovery rows for OWNER only: the source session
    /// reads `pushes`, the destination `pushed-by`, a pull pair reads
    /// `pulls`/`pulled-by`, a foreign (unregistered) src prints `unknown -` —
    /// and an edge-scoped caller gets NONE of them (count intact).
    #[test]
    fn family_connection_rows_are_owner_only() {
        let (store, conn, a, b) = connection_fixture();
        assert!(
            cmd_connect_in(
                &conn,
                &store,
                Scope::Owner,
                &format!("dst={} src={} kind=push", b.sid.as_str(), a.sid.as_str()),
            )
            .starts_with("OK"),
            "fixture connect"
        );
        // A foreign wire-granted src pulling A: rows in A's table, src unknown.
        let x = SessionId::new("s-foreign");
        {
            let mut tbl = a.ctx.edges.lock().unwrap();
            let _ = tbl.grant(x.clone(), a.sid.clone(), Op::ReadScreen, a.ctx.nonce);
        }

        // Owner @ A: self+parent + `pushes B` + `pulled-by X` = 4 rows.
        let fa = cmd_family(&a.ctx, &store, Scope::Owner, "");
        assert!(fa.starts_with("OK 4\n"), "{fa}");
        assert!(
            fa.contains(&format!("pushes {} alive ", b.sid.as_str())),
            "{fa}"
        );
        assert!(
            fa.contains(&format!("pulled-by {} unknown -", x.as_str())),
            "foreign src prints honestly: {fa}"
        );
        // Owner @ B: the inbound face of the same connection.
        let fb = cmd_family(&b.ctx, &store, Scope::Owner, "");
        assert!(
            fb.contains(&format!("pushed-by {} alive ", a.sid.as_str())),
            "{fb}"
        );
        assert!(!fb.contains("\npushes "), "B pushes nothing: {fb}");

        // An edge-scoped caller gets NO connection rows — and the count agrees.
        let fe = cmd_family(&a.ctx, &store, edge(), "");
        assert!(fe.starts_with("OK 2\n"), "self+parent only: {fe}");
        for kind in ["pushes ", "pushed-by ", "pulls ", "pulled-by "] {
            assert!(!fe.contains(kind), "edge scope must not see {kind}: {fe}");
        }
    }

    /// The pure half of `raise <sid>`: exactly one sid token resolves through
    /// the registry to the local id the main-thread hop raises; unknown sids
    /// and malformed argument lists fail closed (the proxy is never touched).
    #[test]
    fn raise_target_resolves_registered_sids() {
        let store = session_store::new_store();
        let h = registered_session(5, -1, b"");
        store.write().unwrap().register(h.clone());
        assert_eq!(raise_target(&store, h.sid.as_str()), Ok(5));
        assert_eq!(
            raise_target(&store, &format!("  {}  ", h.sid.as_str())),
            Ok(5),
            "whitespace-tolerant"
        );
        assert_eq!(
            raise_target(&store, "s-nope"),
            Err("ERR no such session\n".to_string())
        );
        assert_eq!(
            raise_target(&store, ""),
            Err("ERR usage: raise <sid>\n".to_string())
        );
        assert_eq!(
            raise_target(&store, "s-a s-b"),
            Err("ERR usage: raise <sid>\n".to_string())
        );
    }

    /// `feed-bin` FRAMING: the request-line parse extracts the optional selector and
    /// the declared length, rejects malformed/oversize lines, and recognizes the
    /// verb (with or without an `@<sel>` prefix).
    #[test]
    fn feed_bin_framing_parse() {
        // Bare form.
        assert!(matches!(
            parse_feed_bin("feed-bin 4", "feed-bin"),
            FeedBinFrame::Ok(None, 4)
        ));
        assert!(binary_frame_verb("feed-bin 4").is_some());
        // Cross-session form.
        assert!(matches!(
            parse_feed_bin("@7 feed-bin 10", "feed-bin"),
            FeedBinFrame::Ok(Some(Selector::Local(7)), 10)
        ));
        assert!(binary_frame_verb("@7 feed-bin 10").is_some());
        // Not feed-bin.
        assert!(binary_frame_verb("feed 0a").is_none());
        assert!(binary_frame_verb("@7 feed 0a").is_none());
        // Malformed (missing / non-numeric length) -> Malformed: no payload was
        // announced, so the caller keeps the connection.
        assert!(matches!(
            parse_feed_bin("feed-bin", "feed-bin"),
            FeedBinFrame::Malformed
        ));
        assert!(matches!(
            parse_feed_bin("feed-bin xx", "feed-bin"),
            FeedBinFrame::Malformed
        ));
        // Valid-but-oversize length -> TooLarge (NOT Malformed): the client
        // pipelined N bytes we refuse to read, so the caller must CLOSE — reusing
        // the Malformed "keep connection" path here would desync the stream.
        assert!(matches!(
            parse_feed_bin(&format!("feed-bin {}", MAX_FEED_BIN + 1), "feed-bin"),
            FeedBinFrame::TooLarge
        ));
        // Exactly the cap is allowed.
        assert!(
            matches!(parse_feed_bin(&format!("feed-bin {MAX_FEED_BIN}"), "feed-bin"), FeedBinFrame::Ok(None, n) if n == MAX_FEED_BIN)
        );
        // Trailing tokens after the length are Malformed (NOT a valid frame): a
        // canonical frame is exactly `[@sel] feed-bin <n>`. Treating `feed-bin 1 junk`
        // as a 1-byte frame would consume a byte of the next pipelined line and
        // desync the stream. Trailing whitespace, by contrast, is fine.
        assert!(matches!(
            parse_feed_bin("feed-bin 1 junk", "feed-bin"),
            FeedBinFrame::Malformed
        ));
        assert!(matches!(
            parse_feed_bin("@7 feed-bin 10 extra", "feed-bin"),
            FeedBinFrame::Malformed
        ));
        assert!(matches!(
            parse_feed_bin("feed-bin 4   ", "feed-bin"),
            FeedBinFrame::Ok(None, 4)
        ));
    }

    /// A bounded binary header is itself the authoritative input boundary.  The
    /// licence fence must complete before the server can block waiting for a
    /// slow payload; rejected/unauthorized headers may never mutate cursor state.
    #[test]
    #[cfg(unix)]
    fn binary_input_header_fences_before_payload_and_only_after_authorization() {
        struct FenceCheckedReader {
            inner: std::io::Cursor<Vec<u8>>,
            fenced: std::rc::Rc<std::cell::Cell<bool>>,
        }

        impl std::io::Read for FenceCheckedReader {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                assert!(
                    self.fenced.get(),
                    "payload read began before the authorized target fence"
                );
                self.inner.read(buf)
            }
        }

        impl std::io::BufRead for FenceCheckedReader {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                self.inner.fill_buf()
            }

            fn consume(&mut self, amount: usize) {
                self.inner.consume(amount);
            }
        }

        let store = session_store::new_store();
        let (front, front_rx) = pipe_session(1);
        store.write().unwrap().register(front.clone());
        let active = active_for_handle(&front);
        let fenced = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut reader = FenceCheckedReader {
            inner: std::io::Cursor::new(b"ABCafter\n".to_vec()),
            fenced: fenced.clone(),
        };
        let mut dispatch = |_event, _session| Ok(InputOutcome::Ok);
        let mut cancel = {
            let fenced = fenced.clone();
            move |session| {
                assert_eq!(session, 1);
                fenced.set(true);
                "OK\n".to_string()
            }
        };
        let mut out = Vec::new();
        assert!(run_feed_bin_routed(
            "@1 feed-bin 3",
            "feed-bin",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut cancel,
            &mut out,
        ));
        assert!(fenced.get());
        assert_eq!(String::from_utf8_lossy(&out), "OK 3 bytes\n");
        assert_eq!(read_request_line(&mut reader).as_deref(), Some("after"));
        assert!(drain_pipe(&front_rx).is_empty());

        // A rejected frame still consumes its bounded payload, but an
        // unauthorized header cannot use the licence fence as a UI side
        // channel.
        let cancel_count = std::cell::Cell::new(0usize);
        let mut cancel = |_session| {
            cancel_count.set(cancel_count.get() + 1);
            "OK\n".to_string()
        };
        let mut reader = std::io::Cursor::new(b"XYZafter\n".to_vec());
        let mut denied = Vec::new();
        assert!(run_feed_bin_routed(
            "@1 feed-bin 3",
            "feed-bin",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Edge(EdgeToken::generate()),
            },
            &mut dispatch,
            &mut cancel,
            &mut denied,
        ));
        assert_eq!(cancel_count.get(), 0, "denied header must not touch UI");
        assert_eq!(String::from_utf8_lossy(&denied), "ERR denied\n");
        assert_eq!(read_request_line(&mut reader).as_deref(), Some("after"));

        // A syntactically targetable authorized attempt supersedes even when
        // its length is malformed or over cap, matching ordinary control verbs'
        // pre-argument-parse boundary. An unknown target remains side-effect free.
        let mut empty = std::io::Cursor::new(Vec::<u8>::new());
        let mut malformed = Vec::new();
        assert!(run_feed_bin_routed(
            "@1 feed-bin nope",
            "feed-bin",
            &mut empty,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut cancel,
            &mut malformed,
        ));
        assert_eq!(cancel_count.get(), 1);
        let mut oversize = Vec::new();
        assert!(!run_feed_bin_routed(
            &format!("@1 feed-bin {}", MAX_FEED_BIN + 1),
            "feed-bin",
            &mut empty,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut cancel,
            &mut oversize,
        ));
        assert_eq!(cancel_count.get(), 2);
        let mut missing = Vec::new();
        assert!(run_feed_bin_routed(
            "@999999 feed-bin nope",
            "feed-bin",
            &mut empty,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut cancel,
            &mut missing,
        ));
        assert_eq!(cancel_count.get(), 2, "unknown target cannot fence a window");
    }

    /// The header fence is an early liveness boundary, not an authority cache.
    /// Target selection and capability scope are sampled again after the payload:
    /// a stalled `@.` frame follows the new active session, and a revoked edge can
    /// no longer write. A failed early fence closes before touching payload bytes.
    #[test]
    #[cfg(unix)]
    fn binary_input_revalidates_target_and_authority_after_payload() {
        struct OnFirstRead<F> {
            inner: std::io::Cursor<Vec<u8>>,
            action: Option<F>,
        }

        impl<F: FnOnce()> std::io::Read for OnFirstRead<F> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if let Some(action) = self.action.take() {
                    action();
                }
                self.inner.read(buf)
            }
        }

        impl<F: FnOnce()> std::io::BufRead for OnFirstRead<F> {
            fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
                self.inner.fill_buf()
            }

            fn consume(&mut self, amount: usize) {
                self.inner.consume(amount);
            }
        }

        let store = session_store::new_store();
        let (first, first_rx) = pipe_session(1);
        let (second, second_rx) = pipe_session(2);
        store.write().unwrap().register(first.clone());
        store.write().unwrap().register(second.clone());
        let active = active_for_handle(&first);

        let switched_active = active.clone();
        let switched_to = second.clone();
        let mut reader = OnFirstRead {
            inner: std::io::Cursor::new(b"X".to_vec()),
            action: Some(move || {
                *switched_active.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(ActiveSession {
                        term: switched_to.term.clone(),
                        master: switched_to.master,
                        id: switched_to.local_id,
                        ctx: switched_to.ctx.clone(),
                    });
            }),
        };
        let canceled = std::cell::RefCell::new(Vec::new());
        let mut cancel = |session| {
            canceled.borrow_mut().push(session);
            "OK\n".to_string()
        };
        let routed = std::cell::RefCell::new(None);
        let mut dispatch = |event, session| {
            *routed.borrow_mut() = Some((event, session));
            Ok(InputOutcome::Ok)
        };
        let mut out = Vec::new();
        assert!(run_feed_bin_routed(
            "@. feed-bin 1",
            "feed-bin",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut cancel,
            &mut out,
        ));
        assert_eq!(&*canceled.borrow(), &[1, 2]);
        assert!(matches!(
            &*routed.borrow(),
            Some((InputEvent::KeySequence(bytes), Some(2))) if bytes == b"X"
        ));
        assert_eq!(String::from_utf8_lossy(&out), "OK 1 bytes\n");
        assert!(drain_pipe(&first_rx).is_empty());
        assert!(drain_pipe(&second_rx).is_empty());

        let (guarded, guarded_rx) = pipe_session(3);
        store.write().unwrap().register(guarded.clone());
        let scope = edge_granted(Op::WriteInput, &guarded.ctx);
        let Scope::Edge(token) = scope else {
            unreachable!("edge_granted always returns an edge scope");
        };
        let revoke_ctx = guarded.ctx.clone();
        let mut reader = OnFirstRead {
            inner: std::io::Cursor::new(b"Y".to_vec()),
            action: Some(move || {
                assert!(
                    revoke_ctx
                        .edges
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .revoke(&token),
                    "the header-authorized edge was live before payload receipt",
                );
            }),
        };
        let mut cancel_count = 0usize;
        let mut cancel = |_session| {
            cancel_count += 1;
            "OK\n".to_string()
        };
        let mut dispatch = |_event, _session| -> Result<InputOutcome, String> {
            panic!("revoked payload reached the App input seam")
        };
        let mut denied = Vec::new();
        assert!(run_feed_bin_routed(
            "@3 feed-bin 1",
            "feed-bin",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope,
            },
            &mut dispatch,
            &mut cancel,
            &mut denied,
        ));
        assert_eq!(cancel_count, 1, "only the header fence ran before revocation");
        assert_eq!(String::from_utf8_lossy(&denied), "ERR denied\n");
        assert!(drain_pipe(&guarded_rx).is_empty());

        // The inverse transition is safe too: a header that was not yet
        // authorized performs no early UI mutation, but a grant made while its
        // bounded payload arrives is rechecked and the actual target is fenced
        // before direct egress.
        let (granted_late, granted_late_rx) = pipe_session(4);
        store.write().unwrap().register(granted_late.clone());
        let token = EdgeToken::generate();
        let late_scope = Scope::Edge(token);
        let grant_ctx = granted_late.ctx.clone();
        let mut reader = OnFirstRead {
            inner: std::io::Cursor::new(b"W".to_vec()),
            action: Some(move || {
                let dst = grant_ctx.self_id.clone();
                let nonce = grant_ctx.nonce;
                assert!(grant_ctx
                    .edges
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(
                        token,
                        SessionId::new("s-late-controller"),
                        dst,
                        Op::WriteInput,
                        nonce,
                    ));
            }),
        };
        let canceled_late = std::cell::RefCell::new(Vec::new());
        let mut cancel = |session| {
            canceled_late.borrow_mut().push(session);
            "OK\n".to_string()
        };
        let mut dispatch = |_event, _session| -> Result<InputOutcome, String> {
            panic!("background explicit feed unexpectedly entered App input")
        };
        let mut accepted = Vec::new();
        assert!(run_feed_bin_routed(
            "@4 feed-bin 1",
            "feed-bin",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: late_scope,
            },
            &mut dispatch,
            &mut cancel,
            &mut accepted,
        ));
        assert_eq!(&*canceled_late.borrow(), &[4]);
        assert_eq!(String::from_utf8_lossy(&accepted), "OK 1 bytes\n");
        assert_eq!(drain_pipe(&granted_late_rx), b"W");

        let mut reader = OnFirstRead {
            inner: std::io::Cursor::new(b"Z".to_vec()),
            action: Some(|| panic!("payload read despite a failed header fence")),
        };
        let mut cancel = |_session| "ERR licence fence unavailable\n".to_string();
        let mut dispatch = |_event, _session| Ok(InputOutcome::Ok);
        let mut failed = Vec::new();
        assert!(!run_feed_bin_routed(
            "@2 feed-bin 1",
            "feed-bin",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut cancel,
            &mut failed,
        ));
        assert_eq!(
            String::from_utf8_lossy(&failed),
            "ERR licence fence unavailable\n"
        );
    }

    /// `feed-bin <n>\n<bytes>` end-to-end: an Owner connection's length-prefixed
    /// payload lands the EXACT raw bytes on the resolved target's PTY (binary-clean,
    /// no hex), replies `OK <n> bytes`, and leaves the stream correctly framed for
    /// the NEXT request. Mirrors the production `serve` wiring: a `BufReader` over a
    /// pipe holding `feed-bin 3\n\x00\x01\x02` then a following line.
    #[test]
    #[cfg(unix)]
    fn feed_bin_writes_raw_bytes_and_keeps_stream_framed() {
        let store = session_store::new_store();
        let (h_self, self_rx) = pipe_session(1);
        store.write().unwrap().register(h_self.clone());
        let active = active_for_handle(&h_self);

        // The control INPUT stream: the request line + 3 raw bytes (incl. NUL, which
        // a line-delimited `send` could never carry) + a trailing line to prove the
        // stream stays framed after the payload.
        let input: Vec<u8> = b"feed-bin 3\n\x00\x01\x02next-line\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let mut out: Vec<u8> = Vec::new();

        // Read the (already-known) first line, then run the binary frame.
        let line = read_request_line(&mut reader).expect("the feed-bin request line");
        assert_eq!(line, "feed-bin 3");
        assert!(binary_frame_verb(&line).is_some());
        let keep = run_feed_bin(
            &line,
            "feed-bin",
            &mut reader,
            &active,
            &store,
            Scope::Owner,
            &mut out,
        );
        assert!(keep, "connection kept after a clean frame");
        assert_eq!(
            String::from_utf8_lossy(&out),
            "OK 3 bytes\n",
            "reply framing"
        );

        // The raw bytes (incl. the NUL) reached the TARGET's PTY verbatim.
        assert_eq!(
            drain_pipe(&self_rx),
            b"\x00\x01\x02",
            "raw binary bytes hit the pty"
        );

        // The stream is still framed: the NEXT request line is intact.
        let next = read_request_line(&mut reader).expect("the following line survives");
        assert_eq!(
            next, "next-line",
            "stream stays framed past the binary payload"
        );
    }

    /// `paste-bin <n>\n<bytes>` end-to-end: the payload lands on the target's PTY with
    /// PASTE semantics (the seam's `format_paste` — a bare LF becomes CR), NOT the raw
    /// bytes `feed-bin` writes. Same framing/auth path; the difference is the transform.
    #[test]
    #[cfg(unix)]
    fn paste_bin_applies_paste_semantics_lf_to_cr() {
        let store = session_store::new_store();
        let (h_self, self_rx) = pipe_session(1);
        store.write().unwrap().register(h_self.clone());
        let active = active_for_handle(&h_self);

        // 5-byte payload "ab\ncd": `format_paste` maps the bare LF -> CR (default, no
        // bracketed paste in a fresh engine), so the PTY sees "ab\rcd".
        let input: Vec<u8> = b"paste-bin 5\nab\ncdafter\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let mut out: Vec<u8> = Vec::new();

        let line = read_request_line(&mut reader).expect("the paste-bin request line");
        assert_eq!(line, "paste-bin 5");
        assert_eq!(binary_frame_verb(&line), Some("paste-bin"));
        let keep = run_feed_bin(
            &line,
            "paste-bin",
            &mut reader,
            &active,
            &store,
            Scope::Owner,
            &mut out,
        );
        assert!(keep, "connection kept after a clean paste frame");
        assert_eq!(
            String::from_utf8_lossy(&out),
            "OK 5 bytes\n",
            "reply framing"
        );
        assert_eq!(
            drain_pipe(&self_rx),
            b"ab\rcd",
            "paste seam converted the bare LF to CR"
        );
        let next = read_request_line(&mut reader).expect("the following line survives");
        assert_eq!(next, "after", "stream stays framed past the paste payload");
    }

    #[test]
    #[cfg(unix)]
    fn explicit_front_paste_bin_routes_to_the_targeted_app_input_seam() {
        let store = session_store::new_store();
        let (front, front_rx) = pipe_session(1);
        store.write().unwrap().register(front.clone());
        let active = active_for_handle(&front);
        let input = b"@1 paste-bin 7\n\xe4\xb8\xad\xf0\x9f\x99\x82after\n";
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let line = read_request_line(&mut reader).expect("paste-bin request line");
        let mut reply = Vec::new();
        let mut routed = None;
        let mut dispatch = |event, session| {
            routed = Some((event, session));
            Ok(InputOutcome::Ok)
        };
        let mut clear_license = |_session| "OK\n".to_string();
        assert!(run_feed_bin_routed(
            &line,
            "paste-bin",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch,
            &mut clear_license,
            &mut reply,
        ));
        assert_eq!(String::from_utf8_lossy(&reply), "OK 7 bytes\n");
        let Some((InputEvent::Paste(text), target)) = routed else {
            panic!("visible explicit paste-bin bypassed the App input seam");
        };
        assert_eq!(text, "中🙂");
        assert_eq!(target, Some(1), "the exact named session rides the wake");
        assert!(
            drain_pipe(&front_rx).is_empty(),
            "front paste-bin is not double-written through background egress"
        );
        assert_eq!(
            read_request_line(&mut reader).as_deref(),
            Some("after"),
            "the following request remains framed"
        );
    }

    /// The shipping binary-frame parser and native input dispatcher compose all
    /// the way through to the focused Editor minibuffer. This covers the
    /// `aterm-ctl paste --stdin` route, including exact payload framing, rather
    /// than testing only the target classifier in isolation.
    #[test]
    fn native_paste_bin_drives_focused_editor_minibuffer_and_keeps_stream_framed() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let dir = std::env::temp_dir().join(format!(
            "aterm-control-native-paste-bin-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("native-paste-bin.md");
        std::fs::write(&path, "base\n").unwrap();
        app.open_document_tab(
            crate::native_app::AppKind::Editor,
            // The shipping encoder, not a hand-rolled `format!` — the latter is
            // malformed on Windows (drive letter + backslashes after the
            // authority slot), so this test could not even open its document there.
            &crate::native_document_host::path_to_file_uri(&path).unwrap(),
        )
        .unwrap();
        let (instance, view) = app.active_native_view(wid).unwrap();
        let document = app.native_runtime.document_id(instance).unwrap();
        app.dispatch_native_event(
            wid,
            crate::native_app::AppEvent::EditorCommand(
                crate::native_editor::EditorCommand::IncrementalSearch,
            ),
        )
        .unwrap();

        let active: ActiveHandle = Arc::new(Mutex::new(None));
        let store = session_store::new_store();
        let input = b"paste-bin 6\nBinaryafter\n";
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let line = read_request_line(&mut reader).expect("paste-bin request line");
        let mut out = Vec::new();
        let mut dispatch_front_input =
            |event, _session| Ok(app.input(wid, event, Source::Controller { op: Op::WriteInput }));
        let mut clear_license = |_session| "OK\n".to_string();
        assert!(run_feed_bin_routed(
            &line,
            "paste-bin",
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            &mut dispatch_front_input,
            &mut clear_license,
            &mut out,
        ));
        assert_eq!(String::from_utf8_lossy(&out), "OK 6 bytes\n");
        assert_eq!(
            read_request_line(&mut reader).as_deref(),
            Some("after"),
            "the following request remains framed",
        );

        let Some(crate::native_app::AppViewState::Editor(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("editor view remains live");
        };
        assert!(matches!(
            state.buffer.as_ref().unwrap().minibuffer,
            crate::native_editor::Minibuffer::Search { ref query, .. } if query == "Binary"
        ));
        assert_eq!(
            app.document_store.snapshot(document).unwrap().text.as_ref(),
            "base\n",
            "the focused minibuffer, not the document, owns binary paste",
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// R13: an input-write reply is stamped with the `content_seq` baseline (`seq=<n>`
    /// before the trailing newline), so a driver can `await seq <n+1>` for the output
    /// its input caused. Only input verbs, only on `OK`; ERR/non-input untouched.
    #[test]
    fn stamp_input_seq_appends_baseline_to_ok_input_replies() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        crate::term_lock(&term).process(b"state"); // advance content_seq off 0
        let seq = crate::term_lock(&term).content_seq();
        // `send`/`feed` OK replies gain ` seq=<n>` before the newline.
        assert_eq!(
            stamp_input_seq("send", "OK\n".to_string(), &term),
            format!("OK seq={seq}\n")
        );
        assert_eq!(
            stamp_input_seq("feed", "OK 3 bytes\n".to_string(), &term),
            format!("OK 3 bytes seq={seq}\n")
        );
        // A non-input verb is NOT stamped.
        assert_eq!(
            stamp_input_seq("text", "OK\n".to_string(), &term),
            "OK\n".to_string()
        );
        // An ERR reply is NOT stamped (no baseline to correlate).
        assert_eq!(
            stamp_input_seq("send", "ERR no window\n".to_string(), &term),
            "ERR no window\n".to_string()
        );
    }

    /// REGRESSION (stream desync): a `feed-bin <n>` whose declared length exceeds
    /// MAX_FEED_BIN must CLOSE the connection, not reply-and-keep. The client has
    /// (per the wire form) already pipelined N bytes; we refuse to read unbounded N,
    /// and reusing the malformed-line "keep connection" path would let those bytes
    /// fall through to the next read and dispatch as control verbs. Pre-fix,
    /// run_feed_bin returned true here (desync); post-fix it returns false (close).
    #[test]
    #[cfg(unix)]
    fn feed_bin_oversize_length_closes_connection_no_desync() {
        let store = session_store::new_store();
        let (h_self, self_rx) = pipe_session(1);
        store.write().unwrap().register(h_self.clone());
        let active = active_for_handle(&h_self);

        // An over-cap length, then bytes that — if reinterpreted as a request line —
        // would be a control verb. The fix must prevent that reinterpretation.
        let oversize = MAX_FEED_BIN + 1;
        let input: Vec<u8> = format!("feed-bin {oversize}\nsubscribe all\n").into_bytes();
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let mut out: Vec<u8> = Vec::new();

        let line = read_request_line(&mut reader).expect("the feed-bin request line");
        assert!(binary_frame_verb(&line).is_some());
        let keep = run_feed_bin(
            &line,
            "feed-bin",
            &mut reader,
            &active,
            &store,
            Scope::Owner,
            &mut out,
        );
        assert!(
            !keep,
            "an over-cap feed-bin length must CLOSE the connection (can't safely reframe)"
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "ERR feed-bin too large\n",
            "over-cap feed-bin replies a distinct error, then the caller closes"
        );
        // Nothing was written to the PTY, and — because the connection closes — the
        // pipelined `subscribe all` bytes are never dispatched as a verb.
        assert!(
            drain_pipe(&self_rx).is_empty(),
            "an over-cap feed-bin writes nothing to the pty"
        );
    }

    /// `feed-bin` AUTH: a ReadScreen Edge is DENIED the write — but the N payload
    /// bytes are still CONSUMED so the stream stays framed (the denial reads-and-
    /// discards), and NOTHING reaches the PTY.
    #[test]
    #[cfg(unix)]
    fn feed_bin_read_edge_is_denied_but_consumes_payload() {
        let store = session_store::new_store();
        let (h_self, self_rx) = pipe_session(1);
        store.write().unwrap().register(h_self.clone());
        let active = active_for_handle(&h_self);

        let input: Vec<u8> = b"feed-bin 3\nABCafter\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let mut out: Vec<u8> = Vec::new();

        let line = read_request_line(&mut reader).unwrap();
        // A read-only Edge: WriteInput is required for feed/feed-bin -> denied.
        let scope = Scope::Edge(EdgeToken::generate());
        let keep = run_feed_bin(
            &line,
            "feed-bin",
            &mut reader,
            &active,
            &store,
            scope,
            &mut out,
        );
        assert!(keep, "denied frame keeps the connection");
        assert_eq!(
            String::from_utf8_lossy(&out),
            "ERR denied\n",
            "read edge denied feed-bin"
        );
        // No bytes reached the pty.
        assert!(
            drain_pipe(&self_rx).is_empty(),
            "denied feed-bin writes nothing"
        );
        // But the 3 payload bytes WERE consumed: the next line is correctly framed.
        let next = read_request_line(&mut reader).unwrap();
        assert_eq!(
            next, "after",
            "denial still consumes the payload (stream framed)"
        );
    }

    /// REGRESSION (capability escape): `feed-bin`'s SELF path must re-verify the edge
    /// against the session active RIGHT NOW — not op-match alone. The one global
    /// ActiveHandle retargets `@.`/self to the new frontmost tab on every switch
    /// (`sync_active_session`); a `WriteInput` edge granted on session B must NOT inject
    /// raw bytes into whatever session A became frontmost. (Pre-fix the SELF branch
    /// matched `Edge(WriteInput, _)` on the op alone and let B's token write into A.)
    #[test]
    #[cfg(unix)]
    fn feed_bin_self_edge_denied_after_active_session_swings() {
        let store = session_store::new_store();
        // Session A is FRONTMOST — what `@.`/self resolves to via the active handle.
        let (h_a, a_rx) = pipe_session(1);
        // Session B is where the edge was legitimately granted (a DIFFERENT session).
        let (h_b, _b_rx) = pipe_session(2);
        store.write().unwrap().register(h_a.clone());
        let active = active_for_handle(&h_a);

        // A WriteInput edge GRANTED on B's table: the op matches feed-bin, but it holds
        // no grant against the now-active A. (Scope is Copy, reused for both asserts.)
        let edge_b = edge_granted(Op::WriteInput, &h_b.ctx);
        // Positive control: the SAME edge legitimately drives its OWN session B.
        assert!(
            cross_session_authorized(edge_b, "feed", &h_b.ctx),
            "B's edge authorizes feed-bin against its own session",
        );

        let input: Vec<u8> = b"feed-bin 3\nXYZafter\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let mut out: Vec<u8> = Vec::new();
        let line = read_request_line(&mut reader).unwrap();
        let keep = run_feed_bin(
            &line,
            "feed-bin",
            &mut reader,
            &active,
            &store,
            edge_b,
            &mut out,
        );

        assert!(keep, "denied frame keeps the connection");
        assert_eq!(
            String::from_utf8_lossy(&out),
            "ERR denied\n",
            "B's edge must NOT feed-bin the swung-to active session A",
        );
        assert!(drain_pipe(&a_rx).is_empty(), "nothing reached A's pty");
        // The denial still CONSUMED the N payload bytes, so the stream stays framed.
        let next = read_request_line(&mut reader).unwrap();
        assert_eq!(
            next, "after",
            "denial still consumes the payload (stream framed)"
        );
    }

    /// `feed-bin` with a malformed length replies `ERR usage` WITHOUT consuming any
    /// payload (the next line is whatever followed the bad request line verbatim).
    #[test]
    #[cfg(unix)]
    fn feed_bin_bad_length_does_not_consume_payload() {
        let store = session_store::new_store();
        let (h_self, _self_rx) = pipe_session(1);
        store.write().unwrap().register(h_self.clone());
        let active = active_for_handle(&h_self);

        let input: Vec<u8> = b"feed-bin notanumber\nfollowing\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let mut out: Vec<u8> = Vec::new();
        let line = read_request_line(&mut reader).unwrap();
        let keep = run_feed_bin(
            &line,
            "feed-bin",
            &mut reader,
            &active,
            &store,
            Scope::Owner,
            &mut out,
        );
        assert!(keep);
        assert!(
            String::from_utf8_lossy(&out).starts_with("ERR usage"),
            "bad length usage: {out:?}"
        );
        // Nothing consumed: the line right after the bad request is intact.
        let next = read_request_line(&mut reader).unwrap();
        assert_eq!(next, "following", "a parse error consumes no payload");
    }

    /// `read_request_line` yields the SAME line shape the old `lines()` iterator did:
    /// strips the `\n` (and a CRLF `\r`), yields a final unterminated line at EOF,
    /// and drops a runaway line past the cap (returns None).
    #[test]
    fn read_request_line_strips_newline_and_cr() {
        let input: Vec<u8> = b"plain\r\ncrlf\r\nlast-no-nl".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        assert_eq!(read_request_line(&mut reader).as_deref(), Some("plain"));
        assert_eq!(read_request_line(&mut reader).as_deref(), Some("crlf"));
        assert_eq!(
            read_request_line(&mut reader).as_deref(),
            Some("last-no-nl"),
            "EOF yields the tail"
        );
        assert_eq!(read_request_line(&mut reader), None, "then EOF");
    }

    /// `ready` reports the target's readiness: a fresh OSC-133 prompt is `prompt`,
    /// an in-flight command times out (not ready), a completed command is `prompt`
    /// again, and a session marked `Exited` in the registry fails closed.
    #[test]
    fn ready_verb_reports_prompt_executing_and_exit() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;

        // A fresh prompt (OSC 133 A) -> PromptOnly -> ready immediately.
        term.lock().unwrap().process(b"\x1b]133;A\x07$ ");
        assert_eq!(
            cmd_ready(term, &store, 0, "2000", &subscribe::new_registry()),
            "OK ready prompt\n",
            "fresh prompt is ready"
        );

        // An executing command -> NOT ready -> a short timeout returns `OK timeout`.
        term.lock()
            .unwrap()
            .process(b"\x1b]133;B\x07sleep\n\x1b]133;C\x07");
        assert_eq!(
            cmd_ready(term, &store, 0, "0", &subscribe::new_registry()),
            "OK timeout\n",
            "executing is not ready"
        );

        // The command completes -> Complete -> ready again (prompt-end).
        term.lock().unwrap().process(b"\x1b]133;D;0\x07");
        assert_eq!(
            cmd_ready(term, &store, 0, "2000", &subscribe::new_registry()),
            "OK ready prompt\n",
            "completed is ready"
        );

        // A session marked Exited never becomes ready -> ERR exited (fail closed).
        store
            .write()
            .unwrap()
            .set_state(0, session_store::SessionState::Exited);
        assert_eq!(
            cmd_ready(term, &store, 0, "2000", &subscribe::new_registry()),
            "ERR exited\n",
            "exited fails closed"
        );
    }

    /// `ready` for a PLAIN shell (no OSC-133 integration) settles on a stable
    /// `content_seq`: with no in-flight block it returns `OK ready idle` once output
    /// has stopped changing across the settle window.
    #[test]
    fn ready_verb_settles_on_idle_without_shell_integration() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"some output\r\n");
        store.write().unwrap().register(h.clone());
        // No OSC-133 at all: the settle heuristic fires (content_seq holds steady).
        assert_eq!(
            cmd_ready(&h.term, &store, 0, "2000", &subscribe::new_registry()),
            "OK ready idle\n",
            "idle plain shell"
        );
    }

    /// `turn` happy path: the text is delivered ONCE with paste semantics, ONE
    /// Enter press verifiably lands (the app echoes → content_seq advances), the
    /// reply settles, and the settled screen rows ride the reply — the whole
    /// type→verified-submit→settle exchange in one request.
    #[test]
    fn turn_types_submits_verified_and_returns_settled_screen() {
        use std::cell::{Cell, RefCell};
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;

        let pasted: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let presses = Cell::new(0u32);
        let paste = |text: &str| {
            pasted.borrow_mut().push(text.to_string());
            // The app echoes the typed text (content advances, then goes quiet).
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |name: &str| {
            assert_eq!(name, "enter", "default submit key is enter");
            presses.set(presses.get() + 1);
            // The app accepts the submit and prints its reply — seq advances.
            term.lock().unwrap().process(b"\r\nresponse-line\r\n");
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            "idle=50 timeout=5000 hello peer",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert!(
            out.starts_with("OK 24 turn submitted=1 status=settled seq="),
            "verified submit + settle, screen framed like text: {}",
            out.lines().next().unwrap_or("")
        );
        // The verdict carries the inline `dur_ms=`/`hash=` the catalog + INTROSPECTION.md
        // promise (the FNV of the settled screen, for replay-diffing without a 2nd call).
        let verdict = out.lines().next().unwrap_or("");
        assert!(
            verdict.contains(" dur_ms=") && verdict.contains(" hash="),
            "turn verdict carries dur_ms= and hash=: {verdict}"
        );
        assert!(out.contains("response-line"), "settled rows in the reply");
        assert_eq!(
            pasted.borrow().as_slice(),
            ["hello peer"],
            "the message text (options stripped) is delivered exactly once"
        );
        assert_eq!(presses.get(), 1, "one press sufficed — no blind re-press");
    }

    /// `turn` verified-submit retry: an Enter SWALLOWED mid-paste-ingestion (the
    /// live race this verb exists to close) does not advance `content_seq`, so
    /// the verb re-presses; the second press lands and the turn completes. The
    /// press count proves the retry was driven by verification, not a timer.
    #[test]
    fn turn_represses_when_first_enter_is_swallowed() {
        use std::cell::Cell;
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;

        let presses = Cell::new(0u32);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            if presses.get() == 1 {
                // Swallowed: the editor was still ingesting the paste — no echo,
                // no seq advance. The verb must detect this and press again.
                return true;
            }
            term.lock().unwrap().process(b"\r\nlanded\r\n");
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            "idle=50 timeout=8000 msg",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert!(
            out.contains("submitted=1 status=settled"),
            "second press verified: {}",
            out.lines().next().unwrap_or("")
        );
        assert_eq!(presses.get(), 2, "exactly one re-press after the swallow");
    }

    /// R11 DEFAULT-SIDE: at a shell prompt, `turn` AUTO-verifies the submit against
    /// the OSC-133 command-start (a block transitions to Executing). No
    /// `submit_verify=` is passed: the default detects the prompt and picks
    /// block-verification, and the command start attributes the press immediately.
    #[test]
    fn turn_auto_block_verifies_at_a_shell_prompt() {
        use std::cell::Cell;
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;
        // AT A SHELL PROMPT ready for input: OSC 133;A opens the block, 133;B marks
        // input-ready (state EnteringCommand — a submit will start the command).
        term.lock()
            .unwrap()
            .process(b"\x1b]133;A\x07$ \x1b]133;B\x07");

        let presses = Cell::new(0u32);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            // The submit lands: the shell starts the command (133;C -> Executing).
            term.lock()
                .unwrap()
                .process(b"echo\x1b]133;C\x07\r\nrunning");
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            "idle=50 timeout=8000 build", // NO submit_verify= : exercise the default
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert!(
            out.contains("submitted=1"),
            "auto-default block-verifies once the command starts: {}",
            out.lines().next().unwrap_or("")
        );
        assert_eq!(presses.get(), 1, "one press started the command — no re-press");
    }

    /// AUTO's honest DEGRADE (the stock-Ubuntu-bash regression): the target LOOKS
    /// like a shell prompt (a 133;A/B block sits in EnteringCommand) but the 133
    /// stream is desynced — no press will EVER produce a command-start (the field
    /// case: vte.sh double-sourced by the profile chain clobbers its PS0 `133;C`,
    /// and a sibling precmd wedges the DEBUG-trap capture). The press's echo DOES
    /// advance `content_seq`. AUTO must not blind-re-press (each extra Enter is
    /// REAL input typed into the target) and must not report the false
    /// `submitted=0 status=timeout` that made drivers re-type whole turns:
    /// it degrades to the seq verdict — submitted=1, ONE press, settled.
    #[test]
    fn turn_auto_degrades_when_prompt_block_is_stale() {
        use std::cell::Cell;
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;
        // Prompt-SHAPED block state: A opens, B marks input-ready… and the stream
        // dies there. No C will ever arrive, at this prompt or any later one.
        term.lock()
            .unwrap()
            .process(b"\x1b]133;A\x07$ \x1b]133;B\x07");

        let presses = Cell::new(0u32);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            // The shell consumed the Enter and ran the command — output flows,
            // content advances — but NO 133 mark ever transitions a block.
            term.lock().unwrap().process(b"\r\nran-anyway\r\n$ ");
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            // Small submit_window so the degrade point arrives fast; NO
            // submit_verify=: AUTO picks block off the stale prompt shape.
            "idle=50 timeout=8000 submit_window=120 run",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert!(
            out.contains("submitted=1 status=settled"),
            "stale prompt block degrades to the honest seq verdict: {}",
            out.lines().next().unwrap_or("")
        );
        assert_eq!(
            presses.get(),
            1,
            "a press whose echo moved the screen was consumed — re-pressing would double-type"
        );
    }

    /// AUTO's degrade must NOT fire on a PROVEN-healthy stream: once this session
    /// has started a command block (133;C demonstrated), no-block-plus-ambient-
    /// movement is exactly what a swallowed Enter beside background output looks
    /// like — claiming submitted=1 there would break the "a press VERIFIABLY
    /// landed" contract. The strict lane holds: re-press, and report the honest
    /// submitted=0 when nothing ever starts.
    #[test]
    fn turn_auto_stays_strict_when_the_stream_has_started_blocks_before() {
        use std::cell::Cell;
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;
        // A PROVEN stream: one full command block (A/B prompt, C start, D exit),
        // then a fresh healthy prompt.
        term.lock().unwrap().process(
            b"\x1b]133;A\x07$ \x1b]133;B\x07echo\x1b]133;C\x07\r\nout\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07",
        );

        let presses = Cell::new(0u32);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            // The Enter is SWALLOWED — but ambient output (a background job, a
            // spinner) moves content inside the submit window anyway.
            term.lock().unwrap().process(b"\r\n[bg] tick");
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            "idle=50 timeout=1500 submit_window=120 presses=2 run",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert!(
            out.contains("submitted=0"),
            "a proven stream never degrades on ambient movement: {}",
            out.lines().next().unwrap_or("")
        );
        assert_eq!(
            presses.get(),
            2,
            "strictness holds — the press budget retries instead of overclaiming"
        );
    }

    /// R11: `submit_verify=block` verifies the submit against the OSC-133 command
    /// start, NOT a bare `content_seq` advance — so an ambient repaint (a TUI
    /// painting between the press and the real submit) cannot false-verify. Here the
    /// press first emits a bare repaint (advances content_seq, NO command) and only
    /// on the SECOND press starts a real command block (133;A/C/D). `block` mode must
    /// ignore the repaint and re-press until the command starts.
    #[test]
    fn turn_submit_verify_block_ignores_ambient_repaint() {
        use std::cell::Cell;
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;
        // A shell prompt ready for input (133;A + 133;B).
        term.lock()
            .unwrap()
            .process(b"\x1b]133;A\x07$ \x1b]133;B\x07");

        let presses = Cell::new(0u32);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            if presses.get() == 1 {
                // AMBIENT REPAINT: content_seq advances (a spinner frame), but NO
                // shell command block starts. `seq` mode would false-verify here;
                // `block` mode must not.
                term.lock().unwrap().process(b"\x1b[2K spinner-tick ");
                return true;
            }
            // The real submit: the shell starts the command (133;C -> Executing).
            term.lock().unwrap().process(b"echo\x1b]133;C\x07\r\ndone");
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            "idle=50 timeout=8000 submit_verify=block run",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert!(
            out.contains("submitted=1"),
            "block mode verifies once the real command block appears: {}",
            out.lines().next().unwrap_or("")
        );
        assert_eq!(
            presses.get(),
            2,
            "the ambient repaint did NOT verify; a second press started the block"
        );
    }

    /// `turn submit=none` is type-only (emacs-style buffers, pre-filling an
    /// editor): no press ever fires, `submitted=0` is reported honestly, and the
    /// settle phase still runs so the caller gets the painted screen. A message
    /// whose FIRST word merely contains `=` is not eaten by the option parser.
    #[test]
    fn turn_submit_none_types_without_pressing_and_keeps_odd_text() {
        use std::cell::{Cell, RefCell};
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;

        let pasted: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let presses = Cell::new(0u32);
        let paste = |text: &str| {
            pasted.borrow_mut().push(text.to_string());
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            "idle=50 timeout=5000 submit=none x=1 stays verbatim",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert!(
            out.contains("submitted=0 status=settled"),
            "type-only turn settles with an honest submitted=0: {}",
            out.lines().next().unwrap_or("")
        );
        assert_eq!(presses.get(), 0, "submit=none never presses");
        assert_eq!(
            pasted.borrow().as_slice(),
            ["x=1 stays verbatim"],
            "an unknown k=v token ends option parsing and stays in the text"
        );
    }

    /// The turn LEASE: a second driver colliding with an in-flight turn is
    /// refused with the holder's id, the turn itself reports that id on its
    /// status line, and the lease is RELEASED on completion so the next turn
    /// proceeds — arbitration, not deadlock.
    #[test]
    fn turn_lease_refuses_concurrent_writers_and_releases() {
        use std::cell::Cell;
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;

        // A held lease (as if another connection's turn were mid-flight) refuses
        // a new turn, naming the holder.
        *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn(41));
        let paste = |_: &str| true;
        let press = |_: &str| true;
        let out = cmd_turn(
            term,
            &store,
            0,
            "timeout=2000 hi",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert_eq!(
            out, "ERR busy turn=41\n",
            "held lease refuses a second turn"
        );
        *h.ctx.turn_lease.lock().unwrap() = None;

        // A completed turn reports its id and leaves the lease RELEASED.
        let presses = Cell::new(0u32);
        let paste2 = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press2 = |_: &str| {
            presses.set(presses.get() + 1);
            term.lock().unwrap().process(b"\r\nok\r\n");
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            "idle=50 timeout=5000 hello",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste2,
                press: &press2,
            },
        );
        assert!(
            out.contains("submitted=1 status=settled") && out.contains(" id="),
            "turn reports its lease id: {}",
            out.lines().next().unwrap_or("")
        );
        assert_eq!(
            *h.ctx.turn_lease.lock().unwrap(),
            None,
            "lease released after the turn"
        );
    }

    /// R17 cooperative lease: `lease acquire` grants a holder-named, TTL'd hold that
    /// `lease status` reports and `lease release` (holder-matched) drops. A DIFFERENT
    /// holder's acquire is refused while it is live; the same holder renews.
    #[test]
    fn lease_acquire_status_release_and_holder_exclusion() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());

        // Idle: status is `none`.
        assert_eq!(cmd_lease(&h.ctx, "status"), "OK lease none\n");
        // Bare `lease` == `lease status`.
        assert_eq!(cmd_lease(&h.ctx, ""), "OK lease none\n");

        // Acquire with an explicit holder + ttl; status now reports that holder.
        let out = cmd_lease(&h.ctx, "acquire ttl=60000 holder=agent-a");
        assert!(
            out.contains("holder=agent-a") && out.contains("ttl_ms=60000"),
            "{out}"
        );
        assert!(
            cmd_lease(&h.ctx, "status").contains("holder=agent-a"),
            "status shows holder"
        );

        // A DIFFERENT holder is refused while it is live.
        let denied = cmd_lease(&h.ctx, "acquire holder=agent-b");
        assert!(
            denied.starts_with("ERR lease held holder=agent-a"),
            "{denied}"
        );
        // The SAME holder renews (not refused).
        assert!(cmd_lease(&h.ctx, "acquire holder=agent-a").starts_with("OK lease acquired"));

        // Wrong holder cannot release; the right holder can.
        assert!(
            cmd_lease(&h.ctx, "release holder=agent-b").starts_with("ERR lease held by agent-a")
        );
        assert_eq!(
            cmd_lease(&h.ctx, "release holder=agent-a"),
            "OK lease released\n"
        );
        assert_eq!(cmd_lease(&h.ctx, "status"), "OK lease none\n");

        // `force` steals any held cooperative lease.
        assert!(cmd_lease(&h.ctx, "acquire holder=agent-c").starts_with("OK lease acquired"));
        assert_eq!(cmd_lease(&h.ctx, "release force"), "OK lease released\n");
    }

    /// R17 mutual exclusion with `turn`: a LIVE cooperative lease blocks a `turn`
    /// from stomping it (`ERR busy lease=<holder>`), but does NOT hard-block a raw
    /// write path — it is advisory for `send`/`key`/`feed`, which stay governed by
    /// the `turn` lease. (The write-seam check ignores `Drive` leases.)
    #[test]
    fn cooperative_lease_blocks_turn_but_not_raw_writes() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;

        assert!(cmd_lease(&h.ctx, "acquire holder=agent-a ttl=60000").starts_with("OK lease"));

        // A `turn` is HARD arbitration — it refuses to stomp a live cooperative hold.
        let paste = |_: &str| true;
        let press = |_: &str| true;
        let out = cmd_turn(
            term,
            &store,
            0,
            "timeout=2000 hi",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert_eq!(
            out, "ERR busy lease=agent-a\n",
            "turn refuses a live cooperative lease"
        );

        // But the cooperative lease does NOT hard-block a raw write: the dispatch
        // write-arbitration seam sees no turn-lease and lets `send` through. Assert
        // via the shared predicate the seam uses (a `Drive` lease yields no block id).
        let held = h.ctx.turn_lease.lock().unwrap();
        assert_eq!(
            held.as_ref().and_then(crate::Lease::write_block_turn),
            None,
            "a cooperative lease never hard-blocks a raw write"
        );
    }

    /// R17 TTL: an EXPIRED cooperative lease reads as idle (`who`/`status` = none),
    /// is stealable by a new acquire, and no longer blocks a `turn`.
    #[test]
    fn expired_cooperative_lease_is_free() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());

        // Plant an ALREADY-expired cooperative lease (expiry in the past).
        *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
            holder: "stale".to_string(),
            expires_us: crate::metrics::now_us().saturating_sub(1),
        });
        assert_eq!(
            cmd_lease(&h.ctx, "status"),
            "OK lease none\n",
            "expired reads as none"
        );
        // A fresh acquire by anyone succeeds (steals the lapsed slot).
        assert!(cmd_lease(&h.ctx, "acquire holder=fresh").starts_with("OK lease acquired"));
    }

    /// A HARD `turn` lease (as if a driver's turn is mid-flight) cannot be released by
    /// a bare `lease release`, but `lease release force` PREEMPTS it — the crash-
    /// recovery escape hatch for a wedged turn whose driver disconnected (round-4).
    #[test]
    fn turn_lease_is_force_preemptible() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());

        // A wedged turn holds the hard lease.
        *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn(77));
        // A plain release refuses (a turn releases its own lease)...
        let refused = cmd_lease(&h.ctx, "release");
        assert!(refused.starts_with("ERR busy turn=77"), "{refused}");
        assert!(
            refused.contains("force"),
            "the error points at the force escape hatch"
        );
        // ...but `force` preempts it, freeing the write seam for another driver.
        assert_eq!(cmd_lease(&h.ctx, "release force"), "OK lease released\n");
        assert_eq!(
            cmd_lease(&h.ctx, "status"),
            "OK lease none\n",
            "the wedged turn lease is cleared"
        );
    }

    /// `turn` on a dead session fails closed with `ERR exited`, and a bad submit
    /// key name is a usage error — both before any settle wait burns the caller's
    /// deadline.
    #[test]
    fn turn_fails_closed_on_exit_and_bad_submit_key() {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;

        let paste = |_: &str| true;
        let bad_press = |_: &str| false; // parse_key rejected the name
        let out = cmd_turn(
            term,
            &store,
            0,
            "submit=bogus-key timeout=3000 hi",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &bad_press,
            },
        );
        assert!(
            out.starts_with("ERR usage: turn"),
            "bad key is usage: {out}"
        );

        store
            .write()
            .unwrap()
            .set_state(0, session_store::SessionState::Exited);
        let press = |_: &str| true;
        let out = cmd_turn(
            term,
            &store,
            0,
            "timeout=3000 hi",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
            },
        );
        assert_eq!(out, "ERR exited\n", "exited session fails closed");
    }

    /// `edges`/`grants`/`family`/`ready` are classified as READ-side (`ReadScreen`)
    /// in `required_op`, so a ReadScreen edge may run them but a WriteInput edge may
    /// not — the same read != write split every other read verb honors.
    #[test]
    fn new_read_verbs_are_read_scoped() {
        let ctx = test_ctx();
        let read = edge_granted(Op::ReadScreen, &ctx);
        let write = edge_granted(Op::WriteInput, &ctx);
        for v in ["edges", "grants", "family", "ready", "status"] {
            assert_eq!(required_op(v), Some(Op::ReadScreen), "{v} is read-side");
            assert!(gate_allows(read, v, &ctx), "read edge may {v}");
            assert!(!gate_allows(write, v, &ctx), "write edge may NOT {v}");
        }
        // `status` has no write sub-form at all, so nothing about its arguments
        // may ever escalate it — unlike `meta`, whose `set`/`unset` do.
        for rest in ["status", "@s-a status", "status set idle"] {
            assert_eq!(escalated_op("status", rest), None, "{rest}");
        }
    }

    /// REGRESSION (integration audit): the SELF-path op-scope gate must re-verify an
    /// Edge against the session that is active RIGHT NOW — not op-match alone. The one
    /// global ActiveHandle is retargeted to the new frontmost active tab on every tab
    /// switch / cross-window focus change (`sync_active_session`); an edge granted on
    /// session B must NOT be able to drive whatever session A became frontmost after
    /// the swing (a confused-deputy authority escape: e.g. a WriteInput edge injecting
    /// keystrokes into, or resizing, an arbitrary foreground session). Owner keeps
    /// full self-power regardless of which session is active.
    #[test]
    fn self_path_edge_denied_after_active_session_swings() {
        let ctx_b = test_ctx(); // the session active when the edge connected
        let ctx_a = test_ctx(); // a DIFFERENT session the active handle later swings to
        let edge_b = edge_granted(Op::WriteInput, &ctx_b);

        // While B is active, the edge drives its OWN granted session (legitimate).
        assert!(
            gate_allows(edge_b, "send", &ctx_b),
            "edge drives its granted session B"
        );

        // After the active handle SWINGS to A, the SAME edge is DENIED on the SELF
        // path — it holds no grant against A. (Pre-fix this passed on op-match alone.)
        assert!(
            !gate_allows(edge_b, "send", &ctx_a),
            "edge must NOT drive swung-to session A"
        );
        assert!(
            !gate_allows(edge_b, "resize", &ctx_a),
            "incl. resize (whole-window effect)"
        );

        // Owner is unaffected — full self-power against whichever session is active.
        assert!(
            gate_allows(Scope::Owner, "send", &ctx_a),
            "owner drives the active session"
        );
    }

    /// Build an [`ActiveHandle`] over a `pipe_session` handle (the cross-session
    /// feed-bin tests resolve `@.`/self through the active handle the same way the
    /// production `serve` loop does).
    #[cfg(unix)]
    fn active_for_handle(h: &crate::session_store::SessionHandle) -> ActiveHandle {
        Arc::new(Mutex::new(Some(ActiveSession {
            term: h.term.clone(),
            master: h.master,
            id: h.local_id,
            ctx: h.ctx.clone(),
        })))
    }
}
