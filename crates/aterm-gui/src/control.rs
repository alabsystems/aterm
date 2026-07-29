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
use aterm_session::{EdgeToken, Op, SessionId, decide_edge};
use winit::event_loop::EventLoopProxy;

use crate::control_auth::{self, AuthOutcome};
use crate::input::{
    Egress, EgressMode, InputEvent, InputOutcome, ScrollIntent, Source, seam_egress,
};
use crate::session_store::Store;
use crate::subscribe::{self, InstanceStreams, PushOptions, Requested, Subscribers};
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
/// `blocktext`/`wait`). Child module of `control`; dispatched as
/// `control_selection::cmd_*` from [`handle`]. The file lives flat at
/// `src/control_selection.rs` (sibling of `control.rs`), so `#[path]` points at it.
#[path = "control_selection.rs"]
mod control_selection;
// Re-export `pbcopy` (GUI OSC-52 path, `main.rs`) and the smart-selection
// gesture helpers (`app_mouse.rs`'s double/triple-click), which both reach
// through the stable `crate::control::NAME` path, so those paths keep resolving.
pub(crate) use control_selection::{pbcopy, pbpaste, select_line, select_word, word_cols};
// `primary_get`/`primary_set` (PRIMARY-selection paste/own) are wired ONLY to Linux
// middle-click / selection-release in `app_mouse`, so their re-exports are
// Linux-only — on macOS they would be unused imports.
#[cfg(target_os = "linux")]
pub(crate) use control_selection::{pbpaste_owned, primary_get, primary_set};

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
fn sessionless_front_paste_event(
    verb: &str,
    text: String,
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
            NativeControlDecision::WithoutSession => Ok(InputEvent::Paste(text)),
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
        "open" => match crate::app_introspect::AuxTarget::parse(
            rest.split_whitespace().next().unwrap_or(""),
        ) {
            // The Settings tab flips default-OFF security knobs => ConfigWrite.
            Some(crate::app_introspect::AuxTarget::Prefs) => Some(Escalation::Op(Op::ConfigWrite)),
            // Raising the PALETTE (a gateway to every action) or the SOFTWARE-UPDATE
            // overlay (the re-exec surface) matches the OwnerOnly `invoke` twins, so
            // a scoped edge cannot open either through the `open` seam.
            Some(
                crate::app_introspect::AuxTarget::Menu | crate::app_introspect::AuxTarget::Update,
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
    let Some(st) = st else {
        return "OK enabled=false outcome=\"no updater on this platform\"\n".to_string();
    };
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
        format!("{}:{}", st.failing_checks, st.failing_kind)
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
         failing_applies={} rescues={} persistent={} outcome={:?}\n",
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
        );
    }

    fn serve_subscription(&self, mut job: SubscriptionJob) {
        run_subscribe(
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
            control_input::paste_text(rest),
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
    loop {
        let now = std::time::Instant::now();
        if now >= deadline
            || stream
                .set_read_timeout(Some(deadline.saturating_duration_since(now)))
                .is_err()
        {
            retention.acknowledge_failed_anchor();
            quarantine_artifact_reply_until(
                retention,
                std::time::Instant::now() + failure_quarantine,
            );
            return ArtifactAckOutcome::AcknowledgementQuarantined;
        }
        match read_request_line(reader) {
            Some(line) if line == expected => {
                retention.acknowledge_peer_anchor();
                let _ = stream.shutdown(std::net::Shutdown::Write);
                drop(retention);
                return ArtifactAckOutcome::PeerAcknowledged;
            }
            Some(_) => {
                retention.acknowledge_failed_anchor();
                quarantine_artifact_reply_until(
                    retention,
                    std::time::Instant::now() + failure_quarantine,
                );
                return ArtifactAckOutcome::AcknowledgementQuarantined;
            }
            None => {
                retention.acknowledge_failed_anchor();
                quarantine_artifact_reply_until(
                    retention,
                    std::time::Instant::now() + failure_quarantine,
                );
                return ArtifactAckOutcome::AcknowledgementQuarantined;
            }
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

    // A folded-in verb runs first (empty tail = bare TOKEN line, just an ack).
    if let Some(verb) = inline_verb
        && !verb.is_empty()
    {
        // Cross-process forward (Item 5b): a `@<child-sid>` we don't host but
        // spawned is relayed to the child's socket; this connection is then
        // owned by the relay and never returns here.
        if try_proxy_forward(&verb, scope, store, sock_dir, stream, &mut reader) {
            return ServeDisposition::Close;
        }
        // Network drive: a folded-in `dial <name>` relays this connection over TLS
        // to a saved remote aterm; this connection is then owned by the relay.
        if try_net_dial(&verb, scope, stream, &mut reader) {
            return ServeDisposition::Close;
        }
        // A folded-in `subscribe` flips straight to push mode (and never
        // returns to this poll loop) the same as a request-line one. Transfer the
        // socket to the reserved push pool so a quiet subscriber never consumes
        // an ordinary RPC worker.
        if is_subscribe_line(&verb) {
            return ServeDisposition::Subscribe { line: verb, scope };
        }
        // A folded-in `feed-bin`/`paste-bin <n>` reads its N-byte payload from the
        // buffered stream (same as a request-line one), then dispatches as a write verb.
        if let Some(bin_verb) = binary_frame_verb(&verb) {
            let mut dispatch_front_input =
                |event| post_input_reply(proxy, Op::WriteInput, vec![event]);
            if !run_feed_bin_routed(
                &verb,
                bin_verb,
                &mut reader,
                FeedBinRoute {
                    active,
                    store,
                    scope,
                },
                &mut dispatch_front_input,
                &mut writer,
            ) {
                return ServeDisposition::Close;
            }
        } else {
            let resp = dispatch_request(
                &verb,
                active,
                store,
                subscribers,
                scope,
                proxy,
                queue,
                sock_dir,
            );
            match write_control_reply(&mut writer, resp) {
                Ok(Some(retention)) => {
                    await_guarded_reply_close(stream, &mut reader, retention);
                    return ServeDisposition::Close;
                }
                Ok(None) => {}
                Err(_) => return ServeDisposition::Close,
            }
        }
    }

    while let Some(line) = read_authenticated_request_line(&mut reader) {
        // Cross-process forward (Item 5b): relay a `@<child-sid>` we spawned but
        // don't host to the child's socket; the relay then owns the connection.
        if try_proxy_forward(&line, scope, store, sock_dir, stream, &mut reader) {
            return ServeDisposition::Close;
        }
        // Network drive: `dial <name>` relays this connection over TLS to a saved
        // remote aterm; the relay then owns the connection.
        if try_net_dial(&line, scope, stream, &mut reader) {
            return ServeDisposition::Close;
        }
        // PUSH FLIP: `subscribe` authorizes its targets EXACTLY like a read verb,
        // then this connection becomes push-only (never reads another line). On an
        // auth/parse failure `run_subscribe` writes a single `ERR ...` and returns,
        // and we close the connection (a half-subscribed connection is meaningless).
        if is_subscribe_line(&line) {
            return ServeDisposition::Subscribe { line, scope };
        }
        // BINARY FRAME: `feed-bin`/`paste-bin <n>` consumes the following N raw bytes
        // from the SAME buffered stream and feeds them to the resolved target's PTY —
        // the length-prefixed (vs hex) wire form. `feed-bin` writes raw; `paste-bin`
        // applies paste semantics. Both authorize EXACTLY like `feed` (WriteInput) via
        // the normal `@<selector>` + op gate inside `run_feed_bin`.
        if let Some(bin_verb) = binary_frame_verb(&line) {
            let mut dispatch_front_input =
                |event| post_input_reply(proxy, Op::WriteInput, vec![event]);
            if !run_feed_bin_routed(
                &line,
                bin_verb,
                &mut reader,
                FeedBinRoute {
                    active,
                    store,
                    scope,
                },
                &mut dispatch_front_input,
                &mut writer,
            ) {
                break;
            }
            continue;
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
        match write_control_reply(&mut writer, resp) {
            Ok(Some(retention)) => {
                await_guarded_reply_close(stream, &mut reader, retention);
                return ServeDisposition::Close;
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
    ServeDisposition::Close
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

/// The maximum `feed-bin` payload accepted in one frame (256 KiB) — large enough
/// for a bracketed-paste or a control burst, bounded so a hostile/garbled length
/// cannot make the server `read_exact` an unbounded payload.
const MAX_FEED_BIN: usize = 256 * 1024;

/// Maximum number of `@<sel>` targets accepted in one `subscribe` request. Each
/// accepted target installs a `ByteFanout` slot that the producer's PTY reader
/// tees on EVERY output burst, so an unbounded comma list — e.g. `@.,@.,…` — would
/// make that per-burst tee O(N) on the hot reader thread and reserve N queues. A
/// legit client subscribes to at most a handful of sessions; 256 is generous
/// headroom. Selectors are also de-duplicated by session, so repeats collapse.
const MAX_SUBSCRIBE_TARGETS: usize = 256;

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
        _ => None,
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
    let mut unavailable = |_event| Err("ERR input dispatch unavailable\n".to_string());
    run_feed_bin_routed(
        line,
        verb,
        reader,
        FeedBinRoute {
            active,
            store,
            scope,
        },
        &mut unavailable,
        writer,
    )
}

#[derive(Clone, Copy)]
struct FeedBinRoute<'a> {
    active: &'a ActiveHandle,
    store: &'a Store,
    scope: Scope,
}

fn run_feed_bin_routed<W: Write, F>(
    line: &str,
    verb: &str,
    reader: &mut impl BufRead,
    route: FeedBinRoute<'_>,
    dispatch_front_input: &mut F,
    writer: &mut W,
) -> bool
where
    F: FnMut(InputEvent) -> Result<InputOutcome, String>,
{
    // `paste-bin` routes the payload through the PASTE seam (bracketing + sanitize +
    // LF->CR under the target's lock), `feed-bin` writes it RAW — otherwise the two
    // share the entire framing/auth/lease/floor path below.
    let paste = verb == "paste-bin";
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

    // The binary paste twin has the same hybrid target contract as inline
    // `paste`: explicit selectors stay terminal/session operations, while a
    // bare/self paste with native front content enters the main-thread input
    // seam. `feed-bin` remains raw PTY input and can never take this branch.
    let active_target = resolve_active(route.active);
    if paste {
        let principal = if matches!(route.scope, Scope::Owner) {
            NativeControlPrincipal::Owner
        } else {
            NativeControlPrincipal::Edge
        };
        let text = String::from_utf8_lossy(&payload).into_owned();
        if let Some(route) = sessionless_front_paste_event(
            "paste",
            text,
            selector.as_ref(),
            active_target.is_some(),
            principal,
        ) {
            let response = match route {
                Ok(event) => match dispatch_front_input(event) {
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
    let target = match &selector {
        None | Some(Selector::SelfTok) => active_target,
        Some(sel) => resolve_explicit(route.store, sel),
    };
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
    let authorized =
        matches!(route.scope, Scope::Owner) || cross_session_authorized(route.scope, "feed", &ctx);
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

fn run_subscribe<W: Write>(
    line: &str,
    active: &ActiveHandle,
    store: &Store,
    subscribers: &Subscribers,
    scope: Scope,
    writer: &mut W,
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
    for raw in sel_tok.split(',').filter(|s| !s.is_empty()) {
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
    if targets.is_empty() {
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
    subscribe::push_loop(
        subscribers,
        store,
        &targets,
        req.targets,
        instance,
        PushOptions {
            since,
            since_turn,
            since_block,
            non_coalesced,
            timestamps: req.timestamps,
        },
        writer,
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
            seam_egress(term, &ctx.sink, &ev, EgressMode::Backpressured);
            "OK\n".to_string()
        }
        None => err.to_string(),
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
            Ok(reflowed) => term_lock(term).finish_resize_offload(reflowed),
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
    let intent = match rest.trim() {
        "" => ScrollIntent::By(0),
        "top" => ScrollIntent::Top,
        "bottom" => ScrollIntent::Bottom,
        "up" => ScrollIntent::Up,
        "down" => ScrollIntent::Down,
        "prev-prompt" => ScrollIntent::PrevPrompt,
        "next-prompt" => ScrollIntent::NextPrompt,
        n => match n.parse::<i32>() {
            Ok(d) => ScrollIntent::By(d),
            Err(_) => {
                return "ERR usage: scroll <up|down|top|bottom|prev-prompt|next-prompt|N>\n"
                    .to_string();
            }
        },
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
            "sessions" => control_session::cmd_sessions(self_ctx, store),
            "who" => control_session::cmd_who(store, subscribers),
            "grant" => control_session::cmd_grant(self_ctx, scope, rest),
            "revoke" => control_session::cmd_revoke(self_ctx, scope, rest),
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
            "blocks" => Some(control_selection::cmd_blocks_json(term, &body)),
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
            if is_cross {
                let paste = |text: &str| {
                    let _ = cross_input(
                        term,
                        ctx,
                        Some(InputEvent::Paste(control_input::paste_text(text))),
                        "ERR\n",
                    );
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
                let paste = |text: &str| {
                    let _ = control_input::cmd_paste(proxy, text);
                };
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
        "feed" => control_input::cmd_feed(&ctx.sink, rest),
        "signal" => control_input::cmd_signal(master, rest),
        "mouse" if is_cross => cross_mouse(term, ctx, session, proxy, rest),
        // SELF (active-tab) mouse: pass `scope` so a NON-OWNER (scoped-edge) gesture
        // has its copy-on-select CLIPBOARD side-effect suppressed (the exfil fence);
        // Owner and human gestures are unaffected. The suppression is stamped on the
        // injected event, NOT a `Source` branch in the byte seam.
        "mouse" => control_input::cmd_mouse(proxy, scope, rest),
        "paste" if is_cross => cross_input(
            term,
            ctx,
            Some(InputEvent::Paste(control_input::paste_text(rest))),
            "ERR\n",
        ),
        "paste" => control_input::cmd_paste(proxy, rest),
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
        "resize" if is_cross => cross_resize(term, master, ctx, session, Some(proxy), rest),
        "resize" => control_input::cmd_resize(proxy, rest),
        // Cross-session `scroll` also bypasses the seam (`ScrollView` emits no bytes;
        // the viewport move lives in `App::input`). It applies the `ScrollIntent`
        // DIRECTLY to the TARGET term's viewport and reports `OK <offset> <max>` — the
        // SAME wire shape as the self path's `cmd_scroll`. `select` is already
        // cross-correct (mutates the target term + fires a repaint keyed by target id).
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
        "title" => control_query::cmd_title(term),
        "cwd" => control_query::cmd_cwd(term),
        "blocks" => control_selection::cmd_blocks(term, rest),
        "blocktext" => control_selection::cmd_blocktext(term, rest),
        "wait" => control_selection::cmd_wait(term, session, rest, subscribers),
        "colors" => control_query::cmd_colors(term),
        "select" => control_selection::cmd_select(term, proxy, session, rest),
        "selection" => control_selection::cmd_selection(term),
        "copy" => control_selection::cmd_copy(term),
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

/// Map a [`RenderCell`](aterm_core::terminal::RenderCell) char to its on-screen
/// glyph, collapsing NUL/control chars to a space. `pub(crate)` because the push
/// face ([`crate::subscribe`]) must produce byte-identical rows to this poll face.
pub(crate) fn visible_char(ch: char) -> char {
    if ch == '\0' || ch.is_control() {
        ' '
    } else {
        ch
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

/// Percent-encode a string so it occupies ONE space-free token in a response
/// line: every byte that is not ASCII-graphic (and `%` itself) becomes `%XX`.
/// Spaces, newlines and non-ASCII are escaped; the client decodes. Empty -> "".
pub(crate) fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Escape a string as a JSON string BODY (no surrounding quotes): the two-char
/// escapes for `"`, `\`, and the C0 whitespace controls, and `\u00XX` for the
/// remaining control bytes. Non-ASCII UTF-8 is emitted verbatim (a JSON string is
/// UTF-8), so this is allocation-light for ordinary text. Shared by every `*_json`
/// emitter so the `--json` read mode produces RFC 8259-valid strings. `pub(crate)`
/// so [`crate::cast`]'s asciicast emitter reuses the one JSON-escape (no divergence).
pub(crate) fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    json_escape_into(&mut out, s);
    out
}

/// [`json_escape`] APPENDING into a caller-owned buffer — byte-identical output,
/// no allocation of its own.
///
/// The lossless styled frame escapes a glyph (and sometimes a hyperlink) for
/// EVERY cell on screen — up to ~15 000 per snapshot on a large window, and
/// again for every subscriber on every change — so the allocating twin above was
/// paying a `String` per cell purely to be copied into the row buffer and
/// dropped. This is the same loop writing where the answer is already going.
pub(crate) fn json_escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
}

/// A `"key":"<escaped>"` JSON member.
fn json_str_field(key: &str, val: &str) -> String {
    format!("\"{key}\":\"{}\"", json_escape(val))
}

/// Wrap a one-line JSON object body in the read-verb framing: `OK 1\n<json>\n`.
/// The framing matches the other read verbs (`OK <n>` header + body) so the
/// EXISTING client streams the body identically whether or not `--json` is set —
/// only the body bytes change. A JSON reply is always a single body line.
fn json_ok(body: &str) -> String {
    format!("OK 1\n{body}\n")
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
        use super::front_routed;
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
    // `cmd_selection` is drawn only by the unix-gated pipe-backed tests.
    #[cfg(unix)]
    use super::control_selection::cmd_selection;
    use super::control_selection::{cmd_blocks, cmd_blocks_json, cmd_blocktext, cmd_wait};
    use super::control_session::{
        TurnIo, cmd_cast, cmd_edges, cmd_edges_json, cmd_family, cmd_grant, cmd_lease, cmd_meta,
        cmd_ready, cmd_revoke, cmd_sessions, cmd_timeline, cmd_turn, cmd_who, cmd_whoami,
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
        drop(std::os::unix::net::UnixListener::bind(&dead_sock).expect("bind then drop"));
        std::fs::write(dir.join("aterm-77002.token"), "beef\n").expect("token");
        crate::proxy::write_graph_entry(&dir, &dead, &dead_sock.to_string_lossy(), &nonce);
        let dead_line = format!("@{} text", dead.as_str());
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
                dir_up: true,
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
            "Paragraph".to_string(),
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
                "terminal".to_string(),
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
                "explicit".to_string(),
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
                "raw".to_string(),
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
                "denied".to_string(),
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
            &format!("file://{}", path.to_string_lossy()),
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
            "base".to_string(),
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
            "Paragraph ".to_string(),
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
        // by the RESOLVED `session` id. `cmd_select` needs an `EventLoopProxy` (not
        // buildable off the main thread), so we exercise the SAME engine selection
        // path it uses on the resolved target — `text_selection_mut()` over the rows
        // we just drove in — and read it back via `cmd_selection` on the TARGET term,
        // proving the resolved target is the one being selected (and that it emits no
        // pty bytes, the read-side contract).
        {
            term_lock(&term).process(b"hello world");
            let mut t = term_lock(&term);
            let sel = t.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 4, SelectionSide::Right);
            sel.complete_selection();
        }
        let reply = cmd_selection(&term);
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
            include_str!("control_selection.rs"),
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
                "metrics",
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
                "sessions",
                "whoami",
                "grant",
                "revoke",
                "dial",
                "dial-list",
                "dial-token",
            ],
            "None set (Owner-only privilege + build-meta verbs)",
        );

        // EXHAUSTIVE: the pinned partitions cover the WHOLE table (no verb left
        // unclassified). 38 read + 21 write + 1 signal + 1 config + 1 clip + 11 none
        // = 73 — but the NUMBERS here are prose; the machine-checked truth is the
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
        // copy/open-URL), so all injected input verbs escalate identically.
        for kind in [
            OverlayKind::Settings,
            OverlayKind::About,
            OverlayKind::Palette,
            OverlayKind::Update,
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
        // No shell integration yet -> no blocks.
        assert_eq!(cmd_blocks(&term, ""), "OK 0\n");
        // Two command blocks via OSC 133 (+ OSC 633;E commandline): exit 0, exit 1.
        // Each OSC mark is BEL-terminated so the surrounding text isn't swallowed.
        term.lock().unwrap().process(
            b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07\
\x1b]133;A\x07$ \x1b]633;E;false\x07\x1b]133;B\x07false\n\x1b]133;C\x07\x1b]133;D;1\x07",
        );
        let out = cmd_blocks(&term, "");
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
        let last = cmd_blocks(&term, "1");
        assert!(
            last.starts_with("OK 1\n") && last.contains("exit=1"),
            "last block wrong: {last}"
        );
        // `blocktext 0` reads block 0's OUTPUT directly (no coordinate math).
        let txt = cmd_blocktext(&term, "0");
        assert!(
            txt.starts_with("OK ") && txt.contains("hi"),
            "block 0 output wrong: {txt}"
        );
        assert_eq!(cmd_blocktext(&term, "99"), "ERR no such block\n");
    }

    /// `wait` blocks until the in-flight command completes, then reports it; with
    /// no completion at all it times out.
    #[test]
    fn wait_verb_blocks_until_command_completes() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        let reg = subscribe::new_registry();
        // No command in flight and nothing ever completed -> a short wait times out.
        assert_eq!(cmd_wait(&term, 0, "0", &reg), "OK timeout\n");
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
        let resp = cmd_wait(&term, 0, "5000", &reg);
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
        let resp = cmd_wait(&term, 0, "30000", &reg);
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
            cmd_wait(&term, 0, "50", &reg),
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

    /// Decode the percent-encoding `pct_encode` produces (test helper).
    fn pct_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).expect("valid utf8")
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
    /// truncation): title > 120B, description > 1024B, icon > 64B each answer an
    /// `ERR … too long` naming the cap, and the stored value is untouched.
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
        let bj = cmd_blocks_json(&term, "");
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
            &format!("file://{}", path.to_string_lossy()),
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
            |event| Ok(app.input(wid, event, Source::Controller { op: Op::WriteInput }));
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
    /// the OSC-133 command-start (a block transitions to Executing), NOT a bare
    /// `content_seq` advance — so an ambient repaint at the prompt cannot false-verify
    /// a swallowed Enter. No `submit_verify=` is passed: the default detects the
    /// prompt and picks block-verification.
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
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            if presses.get() == 1 {
                // Ambient repaint at the prompt: content advances, but NO command
                // starts (no 133;C). The default (auto=block here) must not verify.
                term.lock().unwrap().process(b"\x1b[2K refresh ");
                return true;
            }
            // The real submit: the shell starts the command (133;C -> Executing).
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
        assert_eq!(
            presses.get(),
            2,
            "the ambient repaint at the prompt did NOT verify; a second press started the command"
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
        let paste = |_: &str| {};
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
        let paste = |_: &str| {};
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

        let paste = |_: &str| {};
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
        for v in ["edges", "grants", "family", "ready"] {
            assert_eq!(required_op(v), Some(Op::ReadScreen), "{v} is read-side");
            assert!(gate_allows(read, v, &ctx), "read edge may {v}");
            assert!(!gate_allows(write, v, &ctx), "write edge may NOT {v}");
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
