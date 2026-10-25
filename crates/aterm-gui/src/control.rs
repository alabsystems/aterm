// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-socket server and `aterm ctl` request dispatcher.
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

#[path = "control_query.rs"]
mod control_query;
#[cfg(test)]
pub(crate) use control_query::rows_hash;
#[cfg(all(test, unix))]
pub(crate) use control_query::screen_stamp;
#[cfg(test)]
pub(crate) use control_query::search_full_history;
#[cfg(test)]
pub(crate) use control_query::styled_frame_payload;
pub(crate) use control_query::{
    ScreenGen, StyledFrameSnapshot, gather_styled_frame, line_size_name, row_line_size, screen_gen,
    screen_text, serialize_styled_frame, trimmed_len, visible_row,
};
#[cfg(test)]
pub(crate) use control_query::{cmd_screen_styled_json, cmd_text_json_looked, text_args};
pub(crate) use control_query::{
    search_full_history_direction, search_full_history_point, set_search_max_lines,
};

#[path = "control_input.rs"]
mod control_input;
use control_input::INPUT_QUEUE_FULL;
pub(crate) use control_input::compile_guard;
pub(crate) use control_input::signal_verb;
pub(crate) use control_input::{GuardedInput, InputFence, input_if_fenced_observed};
#[cfg(all(test, target_os = "macos"))]
pub(crate) use control_input::{cmd_signal, restart_verb};
#[cfg(all(test, unix))]
pub(crate) use control_input::{control_fence, paste_event};
pub(crate) use control_input::{
    feed_bytes, no_such_tab, parse_ctrl, parse_key, parse_mouse, send_bytes,
};

use aterm_control::selection as control_selection;
pub(crate) use aterm_control::selection::{select_line, select_word, word_span};
#[cfg(test)]
pub(crate) use aterm_control::wire::json_escape;
pub(crate) use aterm_control::wire::{json_escape_into, pct_encode, visible_char};
use aterm_control::wire::{json_ok, json_str_field};

#[path = "control_host.rs"]
mod control_host;
use control_host::GuiHost;

#[cfg(debug_assertions)]
#[path = "control_redraw_conformance.rs"]
pub(crate) mod control_redraw_conformance;

#[path = "clipboard.rs"]
mod clipboard;
#[cfg(test)]
pub(crate) use clipboard::PBPASTE_STUB;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use clipboard::PRIMARY_STUB;
pub(crate) use clipboard::{pbcopy, pbpaste};
#[cfg(target_os = "linux")]
pub(crate) use clipboard::{pbpaste_owned, primary_get, primary_get_owned, primary_set};
#[cfg(windows)]
pub(crate) use clipboard::{pbpaste_paths, write_with_owner};

#[path = "control_media.rs"]
pub(crate) mod control_media;
pub(crate) use control_media::SpawnAim;

#[path = "control_denials.rs"]
pub(crate) mod control_denials;
#[path = "control_disk.rs"]
pub(crate) mod control_disk;
#[path = "control_session.rs"]
mod control_session;
#[cfg(all(test, unix))]
pub(crate) use control_query::cmd_offscreen;
#[cfg(all(test, unix))]
pub(crate) use control_session::autopilot_by_fields;
#[cfg(test)]
pub(crate) use control_session::cmd_cast;
#[cfg(all(test, unix))]
pub(crate) use control_session::cmd_history;
#[cfg(test)]
pub(crate) use control_session::cmd_meta;
#[cfg(test)]
pub(crate) use control_session::cmd_sessions_store;
#[cfg(test)]
pub(crate) use control_session::harness_act_face;
#[cfg(all(test, unix))]
pub(crate) use control_session::mint_turn_id_for_test;
#[cfg(test)]
pub(crate) use control_session::own_turn_ids;
#[cfg(any(unix, test))]
pub(crate) use control_session::turn_ids_for_handoff;
#[cfg(unix)]
pub(crate) use control_session::turn_ids_past_handoff_ceiling;
pub(crate) use control_session::{TURN_ID_RESERVE, raise_turn_ids, turn_ids_minted};
#[cfg(test)]
pub(crate) use control_session::{TurnIo, cmd_turn};
#[cfg(test)]
pub(crate) use control_session::{cmd_brief, cmd_log, cmd_log_json};
#[cfg(test)]
pub(crate) use control_session::{cmd_meta_by, meta_questions_denied};

#[cfg(test)]
pub(crate) use control_session::cmd_await;

const AUDIT_SUBSYSTEM: &str = "control_socket";

/// The active tab's engine and PTY master, shared with the GUI so control verbs follow tab switches.
pub(crate) struct ActiveSession {
    pub term: Arc<Mutex<Terminal>>,
    pub master: i32,
    pub id: u64,
    pub ctx: Arc<SessionCtx>,
}

/// Shared optional front-terminal handle.
pub(crate) type ActiveHandle = Arc<Mutex<Option<ActiveSession>>>;

/// One coherent `dims` observation assembled on the main thread.
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
    pub(crate) pad: u32,
    pub(crate) pad_top: u32,
    pub(crate) pad_bottom: u32,
    pub(crate) head: u32,
    pub(crate) tab_rows: u32,
    pub(crate) deck_y: u32,
    pub(crate) deck_h: u32,
    pub(crate) viewers: u32,
    pub(crate) visible_viewers: u32,
    pub(crate) geometry: &'static str,
    pub(crate) present_retry_state: &'static str,
    pub(crate) present_retry_count: u32,
    pub(crate) present_retry_remaining: u32,
    pub(crate) present_retry_in_ms: Option<u64>,
    pub(crate) layer_presentation: Option<(String, f64, bool)>,
}

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

fn owner_only_refusal(verb: &str, selector: Option<&Selector>, scope: Scope) -> Option<String> {
    if !matches!(selector, None | Some(Selector::SelfTok)) {
        return Some(format!("ERR denied: {verb} takes no @<sid>\n"));
    }
    if !scope.is_owner_class() {
        return Some(format!("ERR denied: {verb} is owner-only\n"));
    }
    None
}

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

/// Pure authority/target decision used by the shipping dispatcher and Tier-1 conformance.
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

enum AppAim {
    Front,
    Aimed(Target),
}

impl AppAim {
    const fn session(&self) -> Option<u64> {
        match self {
            AppAim::Front => None,
            AppAim::Aimed((_, _, session, _)) => Some(*session),
        }
    }
}

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

/// What a connection is authorized to do, resolved at handshake.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    Owner,
    Edge(EdgeToken),
    #[cfg(any(unix, test))]
    Bridge,
    #[cfg(any(unix, test))]
    Fleet {
        content: bool,
        drive: bool,
    },
}

impl Scope {
    /// Owner-class.
    pub(crate) const fn is_owner_class(self) -> bool {
        match self {
            Scope::Owner => true,
            #[cfg(any(unix, test))]
            Scope::Bridge => true,
            Scope::Edge(_) => false,
            #[cfg(any(unix, test))]
            Scope::Fleet { .. } => false,
        }
    }

    fn principal(self) -> NativeControlPrincipal {
        if self.is_owner_class() {
            NativeControlPrincipal::Owner
        } else {
            NativeControlPrincipal::Edge
        }
    }

    /// Whether this is the inherited bridge connection.
    pub(crate) const fn is_bridge(self) -> bool {
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

/// WHO PRESENTED an Owner connection's token.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Principal {
    #[default]
    Instance,
    Harness,
}

const fn op_of(class: aterm_types::control_verbs::OpClass) -> Option<Op> {
    use aterm_types::control_verbs::OpClass;
    match class {
        OpClass::Read => Some(Op::ReadScreen),
        OpClass::Write => Some(Op::WriteInput),
        OpClass::Signal => Some(Op::Signal),
        OpClass::ConfigWrite => Some(Op::ConfigWrite),
        OpClass::ClipboardWrite => Some(Op::ClipboardWrite),
        OpClass::Owner => None,
    }
}

fn required_op(verb: &str) -> Option<Op> {
    aterm_types::control_verbs::spec(verb).and_then(|s| op_of(s.op))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Need {
    Op(Op),
    OwnerOnly,
}

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

fn argument_escalation(verb: &str, rest: &str) -> Option<Need> {
    use crate::menu::{InvokeAuthority, MenuAction};
    let (_, rest) = control_input::take_app_unread_ok(input_of(verb), rest);
    match verb {
        "invoke" => {
            let action = MenuAction::from_invoke_name(rest.trim())?;
            match action.invoke_authority() {
                InvokeAuthority::WriteInput => None,
                InvokeAuthority::ConfigWrite => Some(Need::Op(Op::ConfigWrite)),
                InvokeAuthority::ClipboardWrite => Some(Need::Op(Op::ClipboardWrite)),
                InvokeAuthority::OwnerOnly => Some(Need::OwnerOnly),
                InvokeAuthority::PersonOnly => Some(Need::OwnerOnly),
            }
        }
        "open"
            if matches!(
                aterm_types::app_inspection::parse_open_app(rest),
                Ok(aterm_types::app_inspection::OpenAppRequest::Settings { .. }
                    | aterm_types::app_inspection::OpenAppRequest::SystemState)
            ) =>
        {
            Some(Need::Op(Op::ConfigWrite))
        }
        "act"
            if aterm_types::app_inspection::parse_act(rest).is_ok_and(|act| {
                act.action
                    .starts_with(crate::harness_screen::SESSION_ACTION_PREFIX)
            }) =>
        {
            Some(Need::OwnerOnly)
        }
        "act" if aterm_types::app_inspection::parse_act(rest).is_ok() => {
            Some(Need::Op(Op::ConfigWrite))
        }
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
            Some(crate::app_introspect::AuxTarget::Prefs) => Some(Need::Op(Op::ConfigWrite)),
            Some(
                crate::app_introspect::AuxTarget::Menu
                | crate::app_introspect::AuxTarget::TabMenu
                | crate::app_introspect::AuxTarget::AutopilotMenu
                | crate::app_introspect::AuxTarget::FactMenu
                | crate::app_introspect::AuxTarget::Update,
            ) => Some(Need::OwnerOnly),
            _ => None,
        },
        "brief" if !crate::session_log::BriefQuery::parse(rest).is_ok_and(|q| !q.all) => {
            Some(Need::OwnerOnly)
        }
        "sys" if !aterm_vitals::sys::SysQuery::parse(rest).is_ok_and(|q| q.target_rows_only()) => {
            Some(Need::OwnerOnly)
        }
        "await"
            if rest.split_whitespace().next() == Some("sys")
                && crate::sys_host::await_reads_every_tab(rest) =>
        {
            Some(Need::OwnerOnly)
        }
        "log" if crate::session_log_node::names_node_log(rest) => Some(Need::OwnerOnly),
        _ => None,
    }
}

/// What owns controller-directed input in the front window (internal, main-thread).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FrontControlSurface {
    None,
    Overlay(crate::overlay::OverlayKind),
    NativeSettings,
    HarnessSettings,
    OtherNative,
}

fn scope_holds_op(scope: Scope, need: Op, ctx: &SessionCtx) -> bool {
    match scope {
        Scope::Owner => true,
        #[cfg(any(unix, test))]
        Scope::Bridge => true,
        Scope::Edge(presented) => {
            let table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
            decide_edge(&table, &presented, &ctx.self_id, need, &ctx.nonce).is_permitted()
        }
        #[cfg(any(unix, test))]
        Scope::Fleet { .. } => false,
    }
}

fn scope_holds_need(scope: Scope, need: Need, ctx: &SessionCtx) -> bool {
    match need {
        Need::Op(op) => scope_holds_op(scope, op, ctx),
        Need::OwnerOnly => scope.is_owner_class(),
    }
}

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

static HELP_SHORT: LazyLock<Box<str>> = LazyLock::new(|| help_short_catalog().into_boxed_str());
static HELP_FULL: LazyLock<Box<str>> = LazyLock::new(|| help_full_catalog().into_boxed_str());
static HELP_JSON: LazyLock<Box<str>> = LazyLock::new(|| help_json_catalog().into_boxed_str());

fn help_short_catalog() -> String {
    use aterm_types::control_verbs::{catalog_lines, json_verbs};
    let json_verbs = json_verbs().collect::<Vec<_>>().join("/");
    let mut body = format!(
        "# Framing: one \"OK …\"/\"ERR …\" status line, or \"OK <count>\" + <count> content lines \
         (\"OK <nbytes>\" + raw bytes for cast, temporal, doc read, inbox get, but not cast \
         frames|drift or temporal status); empty content = no data, never an error\n\
         # --json on {json_verbs} = structured output · \"@<sid> <verb>\" = aim it at that session \
         (an app-level verb: its instance's front window; an @ that names nothing, as \"@$W\" with \
         $W empty, is refused)\n\
         # help <verb> = the full entry · help --full = the complete catalog · help --json = the \
         catalog as JSON\n"
    );
    for line in catalog_lines() {
        body.push_str(&line);
        body.push('\n');
    }
    let n = body.lines().count();
    format!("OK {n} aterm introspection protocol v1\n{body}")
}

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

fn help_full_catalog() -> String {
    let json_verbs = aterm_types::control_verbs::json_verbs()
        .collect::<Vec<_>>()
        .join("/");
    let header = &format!(
        r#"# Framing: every reply is one STATUS line ("OK …" / "ERR …"), or "OK <count>" then <count>
# CONTENT lines, or, for the bytes verbs (cast, temporal, doc read, inbox get; not
# `cast frames|drift` or `temporal status`), "OK <nbytes>" then exactly that many raw bytes.
# `aterm ctl` prints the content without its OK line, so EMPTY output means NO DATA, never
# an error (errors are always "ERR …").
# "--json" (or a bare "json") on {json_verbs} gives structured output; any other verb
# answers "ERR json: not supported for <verb>" or reads "--json" as an ordinary argument
# (`send --json` types it; `version --json` answers "ERR usage: version"), so do not probe
# with it.
# Cross-session: prefix a verb with "@<selector>" (@<sid>, @<n>, or @. for the active tab;
# e.g. "@s-ab12 text"; `aterm ctl` also expands @self to the sid of the session it runs in).
# `subscribe` takes its targets after the verb instead ("subscribe @s-ab12 screen"). An "@"
# that names nothing (an empty $W in "@$W") is refused, never read as the active tab. Same
# user only; `aterm ctl` reads the per-launch token itself. On an owner connection, a sid
# that ANOTHER aterm instance hosts is relayed to it (not a `spawn agent=` with its task
# frame: send that to the hosting instance), so every session of every instance is
# addressable here: `sessions` lists this instance's, the client-side `aterm ctl ls` every
# instance's.
# TARGETING RULE: the @<sid> selects WHERE the verb runs. SESSION verbs (text/screen/
# image/turn/send/…) act on that session. APP-LEVEL verbs (window/chrome/controls/open/
# settings/invoke/…) act on the FRONT window of the instance hosting it: `@<sid> window`
# screenshots that instance's front window, `@<sid> invoke NewTab` opens a tab there. `tab`,
# `spawn` and `panes` act on the window HOSTING that session instead (the front window
# without an @<sid>), and `theme session` on that session; `edit` needs the @<sid> and
# opens beside that session's tab.
#
# SEEING a terminal (yours or a peer's) — five modes, pick by need:
#   text / screen        what the PROGRAM wrote (plain rows / lossless styled grid)
#   image                what ATERM drew (decorations, tab strip, overlays; `image plain`
#                        leaves out the visual effects), not what the compositor showed
#   subscribe            the LIVE stream (pushed deltas, events, mail; every-frame for animation)
#   cast | temporal <t>  HISTORY: a replayable timestamped recording | the screen as it was
#                        at <t> (temporal needs config temporal_recording = true)
#   search <pat>         FIND in the scrollback (its newest search_history_lines lines)
#
# NATIVE TAB APPS (open in the front window; a document already open is focused):
#   open app settings /about              About inside the Settings tab
#   open app settings /updates            Software Update inside the Settings tab
#   open app system-state                 System State: the setup plan and its progress
#   open app markdown file:///abs/doc.md  read one bounded, local UTF-8 file
#   open app editor file:///abs/doc.md    edit that file through a read/write grant
#   inspect app/v1 tabs                   discover stable native view ids
#   inspect app/v1 view <id> text|controls|tree|audit
#   act app/v1 view <id> <key> <action> [value]
# A document is an absolute local file: URI (no remote host, no cloud-storage folder); the
# grant covers that one file.
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
            header[0].contains("ERR") && header[0].contains("empty"),
            "framing line documents the OK/ERR/empty framing: {}",
            header[0]
        );
        let rows: Vec<&str> = lines.skip(3).collect();
        let want: Vec<String> = catalog_lines().collect();
        assert_eq!(rows, want, "the rows are exactly the table's summary rows");
        assert!(
            !h.contains("image --meta"),
            "a full entry leaked into the short form"
        );
        assert_eq!(super::cmd_help("  "), h);
    }

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

fn await_commit_to_publish(gate: Option<&crate::spawn::DeferredReaderGate>) -> bool {
    gate.is_none_or(|gate| gate.wait_until_released(&AtomicBool::new(false)))
}

fn update_check_refused(refusal: &crate::control::control_media::MainHopError) -> String {
    format!("ERR update check: {refusal}\n")
}

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
            let _asked = UpdateCheckAsked::begin(proxy);
            let status = aterm_update::check_now_with_settings(build, &provider);
            crate::update_control::announce_stage(&status, |wake| {
                let _ = proxy.send_event(wake);
            });
            Some(status)
        }
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
    let failing = if st.failing_checks == 0 {
        "0".to_string()
    } else {
        let kind = if st.failing_checks_kind.is_empty() {
            st.failing_kind.as_str()
        } else {
            st.failing_checks_kind.as_str()
        };
        format!("{}:{kind}", st.failing_checks)
    };
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
    let checker = crate::update_checker_watch::checker_status_tokens(
        aterm_update::checker_watch::checker_snapshot(),
        aterm_update::checker_watch::now_secs(),
    );
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
        st.failing_applies,
        st.is_failing_persistently(),
        st.outcome
    );
    if let Some(phase) = apply_phase {
        out = format!("{} apply_phase={phase}\n", out.trim_end_matches('\n'));
    }
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
    if let Some(delivery) = aterm_update::delivery() {
        let suffix = delivery.status_line_suffix();
        if !suffix.is_empty() {
            let line = out.trim_end_matches('\n');
            out = format!("{line}{suffix}\n");
        }
    }
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
    if let Some(apply) = aterm_update::apply_lane_report(st.current_build) {
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
    let version = Some(activation.version.as_str())
        .filter(|version| *version != format!("build {}", activation.build));
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
            let line = first.strip_suffix('\r').unwrap_or(first);
            let head = line.split_once(' ').map_or(line, |(head, _)| head);
            if !matches!(head, "AUTH" | "TOKEN") {
                return Err(control_denials::NO_TOKEN);
            }
            let ctx = active_ctx().ok_or(control_denials::NO_CONTEXT)?;
            edge_scope_from_first_line(first, &ctx)
                .map(|(_op, tok, verb)| (Scope::Edge(tok), Principal::Instance, verb))
                .ok_or(control_denials::WRONG_TOKEN)
        }
    }
}

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
    let op = edges.authorize(&tok, &ctx.self_id, &ctx.nonce)?;
    Some((op, tok, inline))
}

/// A request for the main thread to render the live screen to a PNG.
#[derive(Debug)]
pub(crate) struct ImageReq {
    pub target: control_auth::ConfinedImage,
    pub clean: bool,
    pub session: Option<u64>,
    pub want_bytes: bool,
    pub want_metadata: bool,
    pub frame_metadata: Arc<std::sync::OnceLock<ImageFrameMetadata>>,
    pub cancel: CaptureCancellation,
    pub reply: Sender<ImageReply>,
    pub(crate) handoff: ReplyRetentionPermit,
}

const CAPTURE_CANCEL_LIVE: u8 = 0;
const CAPTURE_CANCELLED: u8 = 1;
const CAPTURE_COMMIT_AUTHORIZED: u8 = 2;

/// One-shot cancellation/publication election for `image` and `window`.
#[derive(Clone, Debug, Default)]
pub(crate) struct CaptureCancellation(Arc<AtomicU8>);

impl CaptureCancellation {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

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

/// A filesystem-artifact guard carried to the socket edge.
pub(crate) trait WireRetention: Send {
    fn prepare_write(&mut self) -> Result<(), String>;
}

const ARTIFACT_HANDOFF_LIMIT: usize = 4;
const ARTIFACT_HANDOFF_SUFFIX_UNITS: usize =
    crate::pinned_dir::PINNED_DIR_OPERATION_DESCRIPTOR_UNITS;
const ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT: usize =
    crate::pinned_dir::PINNED_DIR_OPEN_COMPONENT_LIMIT + ARTIFACT_HANDOFF_SUFFIX_UNITS;
const ARTIFACT_CLEANUP_LIMIT: usize = 1;
/// Cleanup tasks reserved and not yet run.
pub(crate) const ARTIFACT_CLEANUP_QUEUE_LIMIT: usize = 32;
pub(crate) const ARTIFACT_HANDOFF_BUSY: &str = "other captures are still pending; retry";

/// The capacity artifact verbs admit against.
pub(crate) struct ArtifactBudgets {
    in_flight: Arc<Slots>,
    descriptors: Arc<UnitBudget>,
    video: Arc<UnitBudget>,
    cleanup: Arc<Slots>,
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

    /// The process's own set, which every shipping artifact verb shares.
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

    /// Admission for a target-free body.
    pub(crate) fn try_reserve_in_memory(&self) -> Option<ReplyRetentionPermit> {
        Some(ReplyRetentionPermit {
            in_flight: self.in_flight.try_take()?,
            descriptors: None,
        })
    }

    /// Admission for a path artifact.
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

    /// Transfer one open directory chain to the cleanup worker.
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

    /// Pre-reserve the root capacity an armed video sweep will hand the cleanup worker, independent of best-effort cleanup admission.
    pub(crate) fn try_reserve_video_retention_for_path(
        &self,
        path: &std::path::Path,
    ) -> Option<VideoRetentionPermit> {
        Some(VideoRetentionPermit {
            _root: self.video.try_take(artifact_descriptor_units(path))?,
        })
    }
}

/// Capacity reserved before an artifact job starts.
#[derive(Debug)]
pub(crate) struct ReplyRetentionPermit {
    in_flight: Slot,
    descriptors: Option<UnitPermit>,
}

/// Admission for an in-memory artifact body.
pub(crate) struct WriteOnlyRetention {
    _in_flight: Slot,
}

/// One directory chain held by the asynchronous retention worker.
pub(crate) struct ArtifactCleanupPermit {
    _slot: Slot,
}

/// A slot reserved while one video artifact is live.
#[derive(Debug)]
pub(crate) struct VideoRetentionPermit {
    _root: UnitPermit,
}

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

/// Type-erased ownership that stays live until a reply is consumed and acknowledged.
pub(crate) struct ReplyRetention {
    guard: Option<Box<dyn WireRetention>>,
    #[cfg(any(test, debug_assertions))]
    phase: ReplyRetentionPhase,
    _descriptors: Option<UnitPermit>,
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

    const fn classified(&mut self) {
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
    /// Reconcile the provisional charge after path resolution, before the ancestor chain opens (called by `PinnedDir`).
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

    /// Close `path_capability`, then give back its descriptor chain.
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
    /// An in-memory permit from a fresh budget at the shipping caps.
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
        drop(self.guard.take());
        self.release_guard_anchor();
    }
}

/// Two completion boundaries.
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
    pub(crate) const fn plain(value: T) -> Self {
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

    pub(crate) const fn write_retained(value: T, retention: WriteOnlyRetention) -> Self {
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

/// One RPC response plus any artifact ownership that must span its socket write.
pub(crate) struct ControlReply {
    body: String,
    retention: Option<ReplyHandoff>,
    written: Option<ReplyWritten>,
}

/// A latch the connection releases once a reply is on the wire (written and flushed, or failed, closed or dropped).
#[derive(Clone, Debug, Default)]
pub(crate) struct ReplyWire(Arc<(Mutex<WireLatch>, std::sync::Condvar)>);

#[derive(Debug, Default)]
struct WireLatch {
    released: bool,
    /// The reply reached the peer's socket whole, written and flushed ([`ReplyWritten::delivered`]).
    delivered: bool,
    /// What runs once the latch is released ([`ReplyWire::on_release`]).
    on_release: Option<OnRelease>,
    #[cfg(test)]
    awaited: bool,
}

/// A hook a reply's latch runs once released, told whether the reply was delivered.
struct OnRelease(Box<dyn FnOnce(bool) + Send>);

impl std::fmt::Debug for OnRelease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OnRelease")
    }
}

/// The connection thread's half of a [`ReplyWire`].
#[derive(Debug)]
pub(crate) struct ReplyWritten(ReplyWire);

impl ReplyWire {
    /// A fresh, unreleased latch and the half that releases it.
    #[must_use]
    pub(crate) fn new() -> (Self, ReplyWritten) {
        let wire = Self::default();
        (wire.clone(), ReplyWritten(wire))
    }

    /// Run `hook` once the latch is released, with whether the reply was delivered (written and
    /// flushed whole): at once where it is released already. A verb whose state must follow its
    /// reply's fate (an `await wake` answer, told only once it is on the wire) hangs it here.
    pub(crate) fn on_release(&self, hook: impl FnOnce(bool) + Send + 'static) {
        let (latch, _) = &*self.0;
        let mut latch = latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if latch.released {
            let delivered = latch.delivered;
            drop(latch);
            hook(delivered);
            return;
        }
        latch.on_release = Some(OnRelease(Box::new(hook)));
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

    /// Wait until the latch is released or `deadline` passes.
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
    /// TESTS ONLY.
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

impl ReplyWritten {
    /// The reply is on the peer's socket whole, written and flushed: the latch's hook, when it is
    /// released, is told it was delivered ([`ReplyWire::on_release`]).
    pub(crate) fn delivered(&self) {
        let (latch, _) = &*(self.0).0;
        latch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .delivered = true;
    }
}

impl Drop for ReplyWritten {
    fn drop(&mut self) {
        let (latch, changed) = &*(self.0).0;
        let (hook, delivered) = {
            let mut latch = latch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            latch.released = true;
            (latch.on_release.take(), latch.delivered)
        };
        changed.notify_all();
        if let Some(OnRelease(hook)) = hook {
            hook(delivered);
        }
    }
}

/// What a verb handler hands the connection beside its body.
#[derive(Default)]
pub(crate) struct ReplyTail {
    retention: Option<ReplyHandoff>,
    written: Option<ReplyWritten>,
}

impl ControlReply {
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

    pub(crate) const fn with_handoff(body: String, retention: Option<ReplyHandoff>) -> Self {
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

/// `image` reply payload.
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

/// `image --meta` identity of the encoded capture (version 2).
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
pub(crate) struct BoundedConnectionQueue<T> {
    capacity: usize,
    items: Mutex<VecDeque<T>>,
    ready: Condvar,
}

/// Admission semaphore over queued plus running jobs.
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

    /// `Self::new`, with inbox room for `resume_room` resumed items beyond the `max_capacity` fresh admissions.
    #[must_use]
    pub(crate) fn with_resume_room(max_capacity: usize, resume_room: usize) -> Self {
        Self {
            inbox: BoundedConnectionQueue::new(max_capacity + resume_room),
            max_capacity,
            capacity: AtomicUsize::new(0),
            outstanding: AtomicUsize::new(0),
        }
    }

    /// Publish the number of reusable workers that actually exist.
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

    /// Reserve a lane and enqueue without blocking the listener.
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

        self.inbox.try_push(item).inspect_err(|_| {
            let previous = self.outstanding.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous > 0);
        })
    }

    /// Queue a resumed item behind running work, charged like a fresh admission.
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

    const fn completion_guard(&self) -> DispatchCompletion<'_, T> {
        DispatchCompletion { dispatch: self }
    }

    /// One turn of a worker lane.
    pub(crate) fn serve_next(&self, serve: impl FnOnce(T)) -> bool {
        let item = self.pop();
        let _completion = self.completion_guard();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(item))).is_ok()
    }

    /// Release one queued-or-running admission after its worker has completely returned, including panic recovery.
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

    /// Admit without blocking the listener.
    pub(crate) fn try_push(&self, item: T) -> Result<(), T> {
        let mut items = self.items.lock().unwrap_or_else(|p| p.into_inner());
        if items.len() >= self.capacity {
            return Err(item);
        }
        items.push_back(item);
        self.ready.notify_one();
        Ok(())
    }

    /// Wait for one admitted connection.
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

#[path = "control_lanes.rs"]
mod control_lanes;

const CONTROL_WORKERS: usize = 8;
const CONTROL_WAIT_WORKERS: usize = 64;
const CONTROL_INLINE_WAITS: usize = CONTROL_WORKERS - ARTIFACT_HANDOFF_LIMIT - 1;
const _: () = assert!(
    ARTIFACT_HANDOFF_LIMIT + CONTROL_INLINE_WAITS < CONTROL_WORKERS && CONTROL_INLINE_WAITS > 0,
    "captures and inline waits together must always leave a request lane for requests"
);
const CONTROL_CONNECTIONS_MAX: usize = 1024;

/// Most control connections open at once in this process (push subscriptions have a cap of their own, [`control_subscriptions_cap`]).
pub(crate) fn control_connections_cap() -> usize {
    connections_cap_for(soft_descriptor_limit())
}

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
    // SAFETY: valid resource id and an exclusively borrowed writable out-parameter for this call.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) };
    (read == 0).then_some(limit.rlim_cur)
}

/// No parker runs off unix, so the cap bounds nothing there (the lanes do).
#[cfg(not(unix))]
pub(crate) fn soft_descriptor_limit() -> Option<u64> {
    None
}

const CONTROL_SUBSCRIPTIONS_MIN: usize = 4;
const CONTROL_SUBSCRIPTIONS_MAX: usize = 256;

fn control_subscriptions_cap() -> usize {
    subscriptions_cap_for(soft_descriptor_limit())
}

fn subscriptions_cap_for(soft_limit: Option<u64>) -> usize {
    soft_limit.map_or(CONTROL_SUBSCRIPTIONS_MIN, |soft| {
        usize::try_from(soft / 16)
            .unwrap_or(usize::MAX)
            .clamp(CONTROL_SUBSCRIPTIONS_MIN, CONTROL_SUBSCRIPTIONS_MAX)
    })
}

#[cfg(any(unix, test))]
struct BridgeLostGuard {
    store: Store,
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

/// Local resource preparation, not Commit authority.
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
        let _ = self.0.compare_exchange(
            0,
            if ready { 1 } else { 2 },
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

const fn control_lanes_prepared(rpc: usize, subscriptions: usize) -> bool {
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
            _ => true,
        }
    }

    fn is_wait(&self, request: &Request, scope: Scope) -> bool {
        self.recovery_count_admission.ready() && is_wait_request(request, scope, &self.store)
    }

    fn subscribe(&self, request: Request, scope: Scope, mut stream: CtlStream) {
        let _serving = ServingFd::enter(&stream);
        #[cfg(unix)]
        let _direct = DirectPeer::enter();
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
    #[cfg(unix)]
    fn serve_bridge(&self, mut stream: CtlStream, generation: crate::fabric::BridgeGeneration) {
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
            ServeDisposition::Close => {
                aterm_uds::hangup::hang_up(&stream);
            }
            ServeDisposition::Subscribe { request, scope } => {
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

static BRIDGE_CONTEXT: std::sync::OnceLock<Arc<ControlWorkerContext>> = std::sync::OnceLock::new();

/// Serve one inherited fabric-bridge connection.
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
            context.serve_bridge(stream, generation);
        },
    )
    .is_ok()
}

/// Serve one fleet connection the fleet listener admitted ([`crate::fleet_listen`]).
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
        crate::qos::Role::Responsive,
        move || context.serve_fleet(stream, (content, drive), peer, None),
    )
    .is_ok()
}

/// [`crate::fabric::lift_ungranted_remote_holds`] over this instance's sessions: how many it lifted.
#[cfg(unix)]
pub(crate) fn lift_ungranted_remote_holds(granted: &dyn Fn(&str) -> bool) -> usize {
    let Some(context) = BRIDGE_CONTEXT.get() else {
        return 0;
    };
    crate::fabric::lift_ungranted_remote_holds(&context.store, granted, &|sid| {
        wake_sid_watchers(&context.store, sid);
    })
}

/// Serve one DRIVE connection the fleet listener admitted.
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

mod bound_control {
    use std::sync::Arc;

    use aterm_session::{LaunchNonce, SessionId};
    use aterm_uds::CtlListener;

    use crate::control_auth;
    use crate::session_store::Store;

    /// A control socket this instance has bound.
    pub(super) struct BoundControl {
        listener: CtlListener,
        token: Arc<String>,
        sock_path: String,
    }

    impl BoundControl {
        /// `listener` must be the socket just bound at `sock_path`.
        pub(super) const fn bound(
            listener: CtlListener,
            token: Arc<String>,
            sock_path: String,
        ) -> Self {
            Self {
                listener,
                token,
                sock_path,
            }
        }

        pub(super) const fn listener(&self) -> &CtlListener {
            &self.listener
        }

        pub(super) const fn token(&self) -> &Arc<String> {
            &self.token
        }

        /// The listener and token WITHOUT publishing.
        #[cfg(all(test, unix))]
        pub(super) fn into_parts(self) -> (CtlListener, Arc<String>) {
            (self.listener, self.token)
        }
    }

    /// Publish this instance's discovery entries, record the bound socket so later sessions publish their own, and return the listener to serve on.
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
        let handles = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
        for h in handles {
            crate::proxy::publish_graph_entry(&sock_dir, &h.sid, &sock_path, &h.nonce);
        }
        listener
    }

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

const HANDOFF_BIND_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

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
        let deadline = std::time::Instant::now() + handoff_wait;
        if !parent_exited(deadline) {
            aterm_log::warn!(
                "control socket {sock_path}: predecessor still alive after handoff Commit; \
                 preserving its socket and token"
            );
            return None;
        }
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
    if plan.latest_link.is_some() {
        sweep_dead_instance_files(&sock_dir);
        #[cfg(windows)]
        control_auth::sweep_stale_socket_files_at_launch(&sock_dir);
    }
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
    let _ = aterm_uds::keep_out_of_children(&listener);
    Some(BoundControl::bound(listener, token, sock_path))
}

fn sweep_dead_instance_files(sock_dir: &std::path::Path) {
    control_auth::sweep_stale_instances(sock_dir);
    crate::proxy::sweep_stale_graph(sock_dir);
}

#[cfg(unix)]
const HANDOFF_SWEEP_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

/// Sweep the control directory again [`HANDOFF_SWEEP_AFTER`] after taking over from an update predecessor, on a short-lived thread.
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
            std::thread::sleep(HANDOFF_SWEEP_AFTER);
            sweep_dead_instance_files(&sock_dir);
            crate::seamless::sweep_dead_handoff_leftovers();
        },
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnlinkRule {
    Never,
    Stale,
}

#[derive(Clone, Copy, Debug)]
struct BindRetry {
    attempts: u32,
    backoff: fn(u32) -> std::time::Duration,
    unlink: UnlinkRule,
}

impl BindRetry {
    const STARTUP: Self = Self {
        attempts: 3,
        backoff: |_| std::time::Duration::from_millis(750),
        unlink: UnlinkRule::Stale,
    };

    const HANDOFF: Self = Self {
        attempts: 3,
        backoff: |attempt| std::time::Duration::from_millis(50 * u64::from(attempt)),
        unlink: UnlinkRule::Never,
    };

    const fn retry_allowed(self, failed_attempt: u32, path_missing: bool) -> bool {
        failed_attempt < self.attempts
            && match self.unlink {
                UnlinkRule::Never => path_missing,
                UnlinkRule::Stale => true,
            }
    }
}

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

#[allow(
    clippy::too_many_arguments,
    reason = "the control server's full set of independent collaborators (active handle, store, proxy, socket plan); a config struct would only move the list"
)]
pub(crate) fn spawn(
    active: ActiveHandle,
    store: Store,
    proxy: EventLoopProxy<Wake>,
    plan: control_auth::SocketPlan,
    startup_config: crate::app_config::Config,
    root_identity: Option<(SessionId, aterm_session::LaunchNonce)>,
    bound: Arc<AtomicBool>,
    endpoint: crate::control_listener::Endpoint,
    recovery_count_admission: Arc<crate::keeper_link::RecoveryCountAdmission>,
    handoff: Option<IncomingControlHandoff>,
    publish_gate: Option<crate::spawn::DeferredReaderGate>,
) -> ControlPreparation {
    #[cfg(not(unix))]
    let _ = endpoint;
    let preparation = ControlPreparation::default();
    let preparation_guard = ControlPreparationGuard {
        preparation: preparation.clone(),
        proxy: proxy.clone(),
    };
    let _ = crate::qos::spawn_lane_thread(
        "aterm-control-listener",
        crate::qos::Role::Responsive,
        move || {
            let preparation_guard = preparation_guard;
            let sock_path = plan.sock_path.clone();
            let sock_dir = control_auth::dir_of_socket(&sock_path);
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
            lanes.publish(worker_context.clone());
            let _ = BRIDGE_CONTEXT.set(worker_context.clone());
            if workers != CONTROL_WORKERS {
                aterm_log::warn!(
                    "control socket started with {workers}/{CONTROL_WORKERS} connection workers"
                );
            }
            if let Some(identity) = crate::control_socket_identity::SocketIdentity::capture(
                &plan,
                socket.listener(),
                token.as_str(),
            ) {
                let _ = crate::control_socket_identity::publish(identity);
            }
            bound.store(true, Ordering::SeqCst);
            control_auth::lock_socket_file(&sock_path);
            crate::owned_endpoint::publish(&plan);
            if handoff.is_none() {
                preparation_guard.ready();
            }
            let Some(listener) = publish_when_committed(
                socket,
                publish_gate.as_ref(),
                plan.latest_link.as_deref(),
                root_identity.as_ref(),
                &store,
            ) else {
                return;
            };
            aterm_log::info!(
                "control socket listening at {sock_path} ({workers} RPC lanes, up to {CONTROL_WAIT_WORKERS} wait lanes and {CONTROL_INLINE_WAITS} waits on RPC lanes, up to {connections_cap} connections, up to {subscriptions_cap} subscription lanes; excess peers, waits and subscriptions get retry)"
            );
            #[cfg(not(windows))]
            crate::logging::stderr_line!(
                "aterm-gui: control socket listening at {sock_path} (token-gated, same-uid only)"
            );
            #[cfg(windows)]
            crate::logging::stderr_line!(
                "aterm-gui: control socket listening at {sock_path} (token-gated, dir-ACL + peer pid{})",
                if aterm_uds::elevation::is_elevated() {
                    "; elevated: Administrators only"
                } else {
                    ""
                }
            );
            crate::identity_claim::settle_transferred();
            if crate::build_info::binary_signature_reads_the_file() {
                let _ = crate::qos::spawn_lane_thread(
                    "aterm-build-signature",
                    crate::qos::Role::Responsive,
                    || {
                        let _ = crate::build_info::binary_signature();
                    },
                );
            }
            #[cfg(unix)]
            crate::fabric_launch::spawn_supervisor(&startup_config);
            crate::fleet_listen::start_fleet_listener(startup_config.fleet.as_ref());
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
                    aterm_uds::hangup::hang_up(&stream);
                    return;
                }
                if let Err(mut stream) = lanes.admit(stream) {
                    crate::control_listener::note_busy_reply();
                    let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(100)));
                    let _ =
                        stream.write_all(aterm_types::control_verbs::CONTROL_BUSY_LINE.as_bytes());
                    let _ = stream.flush();
                }
            };
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
                        let _ = crate::control_socket_identity::republish_rebound(
                            &rebind_plan,
                            previous,
                        );
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

/// Accept-loop backoff after `err`, given the previous wait in this streak.
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

fn resolve_sibling(sid: Sid<'_>, sock_dir: &std::path::Path) -> Option<String> {
    let (sock_path, _nonce) = crate::proxy::read_graph_entry(sock_dir, sid)?;
    let sock_path = crate::control_auth::confine_proxy_sock(sock_dir, &sock_path)?;
    if crate::proxy::self_sock_path().as_deref() == Some(sock_path.as_str()) {
        return None; // stale entry for a session we no longer host — never self-dial
    }
    Some(sock_path)
}

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

fn proxy_forward_plan(
    request: &Request,
    scope: Scope,
    store: &Store,
    sock_dir: &std::path::Path,
) -> Option<String> {
    let sid = relay_candidate(request, scope)?;
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
    if client.set_read_timeout(None).is_err() {
        reply("ERR forward timeout setup\n");
        return true;
    }
    let pre = reader.take_buffered();
    if let Err(error) = crate::proxy::relay_hop(hop, request.line(), client, &pre) {
        forward_error(&error);
    }
    true
}

struct Request {
    line: String,
    spans: control_request::Spans,
    form: Option<Resolved>,
    body: usize,
}

impl Request {
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

    fn line(&self) -> &str {
        &self.line
    }

    fn head(&self) -> Head<'_> {
        self.spans.head(&self.line)
    }

    const fn form(&self) -> Option<Resolved> {
        self.form
    }

    const fn body_len(&self) -> usize {
        self.body
    }

    fn frame(&self) -> Option<FeedBinFrame<'_>> {
        match self.form?.row.body {
            Body::Counted { keyed } => Some(parse_frame_header(self.head().rest, keyed)),
            Body::None | Body::PostLen => None,
        }
    }

    fn post_frame(&self) -> Option<Result<usize, String>> {
        match self.form?.row.body {
            Body::PostLen => post_frame_len(self.head().rest),
            Body::None | Body::Counted { .. } => None,
        }
    }
}

fn refuse_stray_args(verb: &str, rest: &str) -> Option<String> {
    let stray = rest.trim();
    (!stray.is_empty()).then(|| format!("ERR usage: {verb}\n"))
}

#[cfg(test)]
mod stray_args_tests {
    use super::refuse_stray_args;

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
                _ => no_handler(verb),
            }
            .into();
        }
    }
    let response = match verb {
        "window" => return control_media::cmd_window(proxy, rest, sock_dir),
        "video" => return control_media::cmd_video(proxy, rest, sock_dir, scope.is_owner_class()),
        "chrome" => control_media::cmd_chrome(proxy),
        "appearance" => crate::os_appearance::cmd_appearance(proxy, rest),
        "text-scale" => crate::native_appearance::cmd_text_scale(proxy, rest),
        "pressure" => crate::crash_restore::cmd_pressure(proxy, rest),
        "program-check" => crate::program_check_watch::cmd(rest),
        "login-notice" => crate::login_notice_verb::cmd(proxy, rest),
        "card" => crate::card_verb::cmd(proxy, rest),
        "aos" => crate::aos_verb::cmd(proxy, rest),
        "security-posture" => crate::security_posture::cmd(proxy, rest),
        "appstatus" => control_query::cmd_appstatus(proxy),
        "messages" => control_query::cmd_messages(proxy, rest),
        "panes" => control_media::cmd_panes(proxy, aim.session()),
        "controls" => control_media::cmd_controls(proxy, rest),
        "inspect" => control_media::cmd_inspect(proxy, rest),
        "open" => control_media::cmd_open(proxy, rest),
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
        "theme" => control_media::cmd_theme(proxy, rest, aim.session()),
        "fx" => control_media::cmd_fx(proxy, rest),
        "streak" => control_media::cmd_streak(proxy, rest),
        "tone" => control_media::cmd_tone(proxy, rest),
        "trail" => control_media::cmd_trail(proxy, rest),
        "kitty" => control_media::cmd_kitty(proxy, rest),
        "spawn" => control_media::cmd_spawn(proxy, rest, aim.session()),
        "settings" => control_media::cmd_settings_overlay(proxy, rest),
        "tab" => {
            let by = control_session::caller_actor_of(scope, None);
            match aim.session() {
                Some(session) => control_media::cmd_tab_aimed(proxy, rest, session, by),
                None => control_input::cmd_tab(proxy, rest, by),
            }
        }
        "hwkey" => control_input::cmd_hwkey(proxy, rest),
        "hover" => control_input::cmd_hover(proxy, rest),
        "find" => control_input::cmd_find(proxy, rest),
        "pane" => control_input::cmd_pane(proxy, rest),
        "pointer" => control_input::cmd_pointer(proxy, rest),
        "metrics" => control_query::cmd_metrics(active_term, rest),
        _ => no_handler(verb),
    };
    response.into()
}

fn retired_verb_error(verb: &str) -> Option<String> {
    match verb {
        "operator" | "operator-propose-bin" => Some(format!(
            "ERR verb {verb} was removed with the embedded operator: a manager session \
             replaced it (`aterm ctl survey` reads every session, `aterm ctl @self spawn \
             agent=claude -` starts a worker)\n"
        )),
        "dial" | "dial-list" | "dial-token" => Some(format!(
            "ERR verb {verb} was removed with the network drive: a fleet replaced it (`aterm \
             fleet init` on one machine, `aterm fleet join` on each other; `aterm ctl survey \
             --fleet` reads them, and `aterm ctl --host <h> …` drives one that ran `aterm fleet \
             allow drive --from <node>`)\n"
        )),
        _ => None,
    }
}

fn post_scope_denied(scope: Scope) -> bool {
    !scope.is_owner_class()
}

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
    #[cfg(unix)]
    if issuer == HoldIssuer::Owner
        && let Some(sid) = local_off_sid(rest)
        && let Some(reply) = crate::fleet_listen::local_off_of_remote(store, sid, &|sid| {
            wake_sid_watchers(store, sid);
        })
    {
        return reply;
    }
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
    if hold_names_local_slot(rest, issuer) {
        word_journaled(store, reply)
    } else {
        reply
    }
}

/// The session an Owner's `hold <sid> off` names, where it names the local slot.
#[cfg(unix)]
fn local_off_sid(rest: &str) -> Option<&str> {
    let mut words = rest.split_whitespace();
    let (Some(sid), Some("off")) = (words.next(), words.next()) else {
        return None;
    };
    words
        .all(|word| word.starts_with("reason=") || word == "origin=local")
        .then_some(sid)
}

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
    let snapshot = control_session::harness_act_reads_counts(rest).then(|| {
        use aterm_core::terminal::program_state::ProgramStateWord as W;
        // The program wrote its turn's marks before this act: count them first.
        crate::act_barrier::before_act(ctx);
        let report = term_lock(term).program_state();
        let left_at_rest = report
            .state
            .is_some_and(|state| matches!(state.state, W::Idle | W::Done | W::Error));
        control_session::HarnessActSnapshot {
            mark: crate::session_timeline::LandingMark {
                program_turns: report.turns,
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
        crate::self_claim::meta_changed(ctx);
        on_change();
    }
    if meta_moves_autopilot(rest) {
        word_journaled(store, resp)
    } else {
        resp
    }
}

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

fn meta_moves_autopilot(rest: &str) -> bool {
    let mut words = rest.split_whitespace();
    matches!(words.next(), Some("set" | "unset")) && words.next() == Some("autopilot")
}

const FABRIC_USAGE: &str =
    "ERR usage: fabric status | fabric attach [<command...>] | fabric retire <sid>...\n";

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
    let by = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_local(session)
        .map_or(aterm_slog::Actor::Nobody, |h| {
            control_session::log_actor(scope, &h.ctx)
        });
    control_query::cmd_story(proxy, session, rest, by)
}

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
            // SAFETY: `tcgetpgrp` on this session's open PTY master fd takes no pointers.
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

#[cfg(not(unix))]
fn cmd_rekey(
    _term: &Arc<Mutex<Terminal>>,
    _master: i32,
    _sid: &aterm_session::SessionId,
    _rest: &str,
) -> String {
    "ERR rekey unsupported on this platform\n".to_string()
}

#[cfg(all(test, unix))]
mod rekey_verb_tests {
    use super::*;

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
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: `slave` is a fresh fd this function alone owns.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let stdio = || std::process::Stdio::from(slave.try_clone().expect("dup slave"));
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.inherit_nothing()
            .arg("30")
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio());
        // SAFETY: `setsid` and `ioctl(TIOCSCTTY)` in the forked child before `exec` are async-signal-safe.
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
    crate::fabric::cmd_topic(&ctx, rest)
}

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

#[cfg(unix)]
fn command_token(argv: &[String]) -> String {
    pct_encode(&argv.join(" "))
}

#[cfg(unix)]
fn argv_token(argv: &[String]) -> String {
    pct_encode(&aterm_codec::pct::join_words(argv))
}

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
    let log_wake = |sid: &aterm_session::SessionId| wake_sid_watchers(store, sid);
    let reply = match verb {
        "deliver" => crate::fabric::cmd_deliver_waking(store, rest, &log_wake),
        "link" => {
            let reply = crate::fabric::cmd_link_waking(
                store,
                rest,
                generation,
                || crate::session_log_mirror::forget_landed(store),
                &log_wake,
            );
            if reply == "OK\n" && rest.split_whitespace().next() == Some("mirror") {
                note_mirror_word(store, generation);
            }
            reply
        }
        "log-take" => crate::session_log_mirror::cmd_log_take(store, rest, generation),
        "log-landed" => crate::session_log_mirror::cmd_log_landed(store, rest, generation),
        "hello" => {
            let reply = crate::fabric::cmd_hello(rest, generation);
            if reply.starts_with("OK ") && !reply.starts_with("OK stale=1") {
                note_mirror_word(store, generation);
            }
            reply
        }
        "outbox" => crate::fabric::cmd_outbox_verb_waking(store, rest, &log_wake),
        _ => no_handler(verb),
    };
    if reply.starts_with("ERR usage") {
        crate::fabric::note_lane_fault(generation);
    }
    reply
}

fn await_task_list(rest: &str) -> Option<Vec<&str>> {
    let mut words = rest.split_whitespace();
    if words.next() != Some("task") {
        return None;
    }
    let words: Vec<&str> = words.collect();
    words.iter().any(|w| w.starts_with('@')).then_some(words)
}

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
    /// A target resolved: its local id, its session, its `rev` edge and its fact edge.
    type Held = (u64, Arc<SessionCtx>, Option<u64>, Option<u64>);
    let mut held: Vec<Held> = Vec::new();
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
        held.push((
            local_id,
            ctx,
            target.since.or(args.since),
            target.fact_since.or(args.fact_since),
        ));
    }
    let targets: Vec<control_session::TaskTarget<'_>> = held
        .iter()
        .map(
            |(local_id, ctx, since, fact_since)| control_session::TaskTarget {
                local_id: *local_id,
                ctx,
                since: *since,
                facts: args.facts,
                fact_since: *fact_since,
            },
        )
        .collect();
    control_session::await_task(
        store,
        &targets,
        args.combine,
        args.timeout_ms
            .unwrap_or(aterm_control::wire::WAIT_DEFAULT_MS),
    )
}

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

fn no_handler(verb: &str) -> String {
    format!("ERR {verb} has no handler\n")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Route {
    Meta,
    Bridge,
    Owner,
    App,
    Session,
}

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
        (Access::Scoped, RowTarget::Session | RowTarget::Meta) => Route::Session,
    };
    Ok((route, form))
}

struct DispatchCx<'a> {
    active: &'a ActiveHandle,
    store: &'a Store,
    scope: Scope,
    proxy: &'a EventLoopProxy<Wake>,
    sock_dir: &'a std::path::Path,
}

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OwnerAim {
    Instance,
    OwnSession,
}

fn owner_aim(verb: &str, selector: Option<&Selector>) -> OwnerAim {
    match verb {
        "grant" | "revoke" | "whoami" => OwnerAim::OwnSession,
        "topic" | "story" if selector.is_none_or(Selector::is_self) => OwnerAim::OwnSession,
        _ => OwnerAim::Instance,
    }
}

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
        "survey" => crate::survey::cmd_survey(
            store,
            rest,
            crate::survey::SurveyLevel::Content,
            usize::MAX,
            &crate::survey::Here::now(),
        ),
        "who" => refuse_stray_args(verb, rest).unwrap_or_else(|| control_session::cmd_who(store)),
        "exits" => control_session::cmd_exits(store, rest),
        "grant" => on_own_session(own_ctx, verb, |ctx| {
            poke_connections_on_ok(proxy, control_session::cmd_grant(ctx, scope, rest))
        }),
        "revoke" => on_own_session(own_ctx, verb, |ctx| {
            poke_connections_on_ok(
                proxy,
                control_session::cmd_revoke_with_closed(ctx, store, scope, rest),
            )
        }),
        "whoami" => on_own_session(own_ctx, verb, |ctx| {
            refuse_stray_args(verb, rest).unwrap_or_else(|| control_session::cmd_whoami(ctx, scope))
        }),
        "connect" => {
            poke_connections_on_ok(proxy, control_session::cmd_connect(store, scope, rest))
        }
        "disconnect" => {
            poke_connections_on_ok(proxy, control_session::cmd_disconnect(store, scope, rest))
        }
        "flows" => control_session::cmd_flows(store, scope, rest),
        "raise" => control_session::cmd_raise(proxy, store, scope, rest),
        "appnotice" => control_query::cmd_appnotice(proxy, rest),
        "notice" => control_query::cmd_notice(proxy, rest),
        "devtools" => control_query::cmd_devtools(proxy, rest),
        "person" => control_media::cmd_person(proxy, rest),
        "fabric" => dispatch_fabric_verb(rest, selector, scope, store),
        _ => no_handler(verb),
    }
}

fn on_own_session(
    own: Option<&SessionCtx>,
    verb: &str,
    act: impl FnOnce(&SessionCtx) -> String,
) -> String {
    own.map_or_else(|| no_handler(verb), act)
}

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
        NativeControlDecision::NoSuchSession => {
            return closed_session_reply(selector, head.verb, head.rest, cx.store, cx.scope).into();
        }
        NativeControlDecision::WithoutSession => {
            return "ERR invalid control target\n".into();
        }
    }
    let front_active_session = active_target.as_ref().map(|(_, _, session, _)| *session);
    let target = match resolution {
        Resolution::SelfTok => active_target,
        Resolution::Hosted(target) => Some(target),
        Resolution::Missing => None,
        Resolution::Ambiguous(refusal) => return refusal.into(),
    };
    let Some(target) = target else {
        return if selector.is_some_and(|s| !s.is_self()) {
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

enum Resolution {
    SelfTok,
    Hosted(Target),
    Missing,
    Ambiguous(String),
}

fn resolve(store: &Store, selector: &Selector<'_>) -> Resolution {
    if selector.is_self() {
        return Resolution::SelfTok;
    }
    let found = store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_selector(selector)
        .map(|h| (h.term.clone(), h.master, h.local_id, h.ctx.clone()));
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
    match ambiguous_sid_refusal(selector) {
        Some(refusal) => Resolution::Ambiguous(refusal),
        None => Resolution::Hosted(target),
    }
}

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
        JournalRead::Cut => return Some(journal_read::JOURNAL_CUT.to_string()),
    };
    Some(match verb {
        "log" => journal_read::cmd_log_journal(&view, rest),
        _ => {
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

enum ServeDisposition {
    Close,
    Subscribe { request: Request, scope: Scope },
}

enum PendingHandoff {
    WriteOnly {
        deadline: std::time::Instant,
        _permit: WriteOnlyRetention,
    },
    Acknowledged(AwaitedAck),
}

impl PendingHandoff {
    const fn deadline(&self) -> std::time::Instant {
        match self {
            Self::WriteOnly { deadline, .. } => *deadline,
            Self::Acknowledged(ack) => ack.deadline,
        }
    }
}

struct AwaitedAck {
    retention: Option<ReplyRetention>,
    nonce: String,
    deadline: std::time::Instant,
    on_wire: bool,
}

impl AwaitedAck {
    fn mark_on_wire(&mut self) {
        artifact_reply_write_wire_anchor();
        self.on_wire = true;
    }

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

const ARTIFACT_HANDOFF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const ARTIFACT_FAILURE_QUARANTINE: std::time::Duration = std::time::Duration::from_secs(30);
const MIN_ARTIFACT_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(1);

struct QuarantinedArtifactReply {
    until: std::time::Instant,
    _retention: ReplyRetention,
    #[cfg(test)]
    ticket: u64,
}

#[cfg(test)]
thread_local! {
    static LAST_QUARANTINE_TICKET: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

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

static ARTIFACT_REPLY_QUARANTINE: std::sync::OnceLock<&'static ArtifactReplyQuarantine> =
    std::sync::OnceLock::new();

impl ArtifactReplyQuarantine {
    fn leak() -> &'static Self {
        Box::leak(Box::new(Self {
            entries: std::sync::Mutex::new(Vec::new()),
            changed: std::sync::Condvar::new(),
        }))
    }

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

const ARTIFACT_QUARANTINE_REAPER_ROLE: crate::qos::Role = crate::qos::Role::Background;

fn artifact_reply_quarantine() -> &'static ArtifactReplyQuarantine {
    ARTIFACT_REPLY_QUARANTINE.get_or_init(|| {
        let quarantine = ArtifactReplyQuarantine::leak();
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

const ARTIFACT_ACK_NONCE_BYTES: usize = 16;

const ACK_LINE_MAX: usize = aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX.len()
    + 2 * ARTIFACT_ACK_NONCE_BYTES
    + "\r\n".len();

impl ControlReply {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactAckOutcome {
    PeerAcknowledged,
    AcknowledgementQuarantined,
}

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
    static SERVING_CONNECTION: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static CONNECTION_CLAIMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static INPUT_COMMITTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    #[cfg(any(unix, test))]
    static SERVING_FLEET_PEER: std::cell::Cell<Option<std::net::SocketAddr>> =
        const { std::cell::Cell::new(None) };
    #[cfg(unix)]
    static SERVING_DIRECT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CARRIED_FENCE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    static SERVING_PRINCIPAL: std::cell::Cell<Principal> =
        const { std::cell::Cell::new(Principal::Instance) };
    static SERVING_CALLER: std::cell::RefCell<Option<Option<Arc<crate::typed_by::Caller>>>> =
        const { std::cell::RefCell::new(None) };
}

/// The connection's typed-by memo, lent to this lane while it serves the connection and handed back when it lets go.
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

/// The [`Principal`] of the connection this thread serves (`Instance` off a lane).
pub(crate) fn serving_principal() -> Principal {
    SERVING_PRINCIPAL.with(std::cell::Cell::get)
}

/// Marks the connection this thread serves as presented by `principal` until dropped ([`SERVING_PRINCIPAL`]).
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

fn carried_fence() -> Option<String> {
    CARRIED_FENCE.with(|fence| fence.borrow().clone())
}

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

/// One write of a `turn` under its `if-supervisor=` fence: `Ok` once `deliver` delivered it;
/// `Err(Some(line))` when it was refused whole, nothing of it written (the fence, or the
/// session's input queue full), `line` the `ERR busy …` reply `send` gives in that state;
/// `Err(None)` for any other outcome, which may have written part of it.
fn turn_write(
    ctx: &SessionCtx,
    if_supervisor: Option<&str>,
    deliver: impl FnOnce() -> Result<InputOutcome, String>,
) -> Result<(), Option<String>> {
    match under_fence(ctx, if_supervisor, deliver) {
        Ok(Ok(InputOutcome::Ok)) => Ok(()),
        Ok(Ok(InputOutcome::QueueFull)) => Err(Some(INPUT_QUEUE_FULL.to_string())),
        Ok(_) => Err(None),
        Err(fence) => Err(Some(fence)),
    }
}

/// Marks the connection this lane serves as a DIRECT local one until dropped ([`SERVING_DIRECT`]).
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

/// A set's PROOF ([`crate::self_claim`]).
pub(crate) fn prove_self_claim(holder: &str, master: i32) -> Option<crate::self_claim::Proven> {
    prove_self_claim_by(holder, master, &crate::self_claim::runs_alab_build)
}

/// [`prove_self_claim`] with the image fact read by `alab_image`, which is asked only once the peer and the foreground facts hold.
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

/// THE KERNEL'S PEER PID of the direct local connection this thread serves, read now.
pub(crate) fn serving_peer_pid() -> Option<u32> {
    #[cfg(unix)]
    {
        if !SERVING_DIRECT.with(std::cell::Cell::get) {
            return None;
        }
        let fd = aterm_control::hangup::serving_fd()?;
        aterm_uds::fdpass::peer_pid_of_fd(fd).ok()
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Marks the verb on this thread as COMMITTED to input it has begun, until dropped.
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

pub(crate) use aterm_control::hangup::{HANGUP_POLL, HUNG_UP_REPLY, caller_hung_up, hangup_park};

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

static NEXT_CONNECTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The connection this thread is serving, if any (see [`SERVING_CONNECTION`]).
pub(crate) fn serving_connection() -> Option<u64> {
    SERVING_CONNECTION.with(std::cell::Cell::get)
}

fn note_connection_claim() {
    CONNECTION_CLAIMED.with(|claimed| claimed.set(true));
}

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

fn release_connection_claims(store: &Store, proxy: Option<&EventLoopProxy<Wake>>, conn: u64) {
    let handles = store.read().unwrap_or_else(|p| p.into_inner()).snapshot();
    let now = crate::metrics::now_us();
    for handle in handles {
        control_session::release_connection_lease(&handle.ctx, conn);
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
    let _connection = ConnectionScope::enter(store, proxy, stream);
    let _principal = ServingPrincipal::enter(Principal::Instance);
    if arm_authenticated_read_poll(stream).is_err() {
        return ServeDisposition::Close;
    }
    let mut unread = control_lanes::Unread::default();
    let mut reader = control_lanes::LaneReader::new(stream, &mut unread);
    let mut out = control_lanes::Outbox::default();
    let mut served = 0usize;
    loop {
        let Some(line) = read_authenticated_request_line(&mut reader, &mut out) else {
            let _ = out.finish_waiting(stream, &mut reader);
            return ServeDisposition::Close;
        };
        let request = Request::parse(line);
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

#[cfg(unix)]
const FLEET_REQUESTS: usize = 4;

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
    let mut writer = stream;
    let first = read_request_line_until(stream, reader, &mut writer, deadline)?;
    let active_ctx = || resolve_active(active).map(|(_, _, _, ctx)| ctx);
    let (scope, principal, inline_verb) = match first_line_scope(&first, token, active_ctx) {
        Ok(authenticated) => authenticated,
        Err(denial) => {
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
    arm_authenticated_read_poll(stream).ok()?;
    Some((scope, principal, inline_verb))
}

fn is_wait_request(request: &Request, scope: Scope, store: &Store) -> bool {
    use aterm_types::control_verbs::Lane;
    let head = request.head();
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
        "subscribe" if head.selector.is_none() => false,
        _ => match request.form().map(|form| form.lane) {
            Some(Lane::Wait) => true,
            Some(Lane::WaitWhenAimed) => head.selector.is_some_and(|s| !s.is_self()),
            Some(Lane::Rpc) | None => false,
        },
    }
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct DriveAct {
    verb: &'static str,
    sid: Option<SessionId>,
    hold: Option<HoldAsk>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HoldAsk {
    on: bool,
    held: bool,
}

#[cfg(any(unix, test))]
#[derive(Debug, PartialEq, Eq)]
enum DriveLine {
    Fleet,
    Act {
        line: String,
        write: Option<DriveAct>,
    },
}

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

#[cfg(any(unix, test))]
const DRIVE_CLOSE_REFUSED: &str = "a drive closes only a worker it spawned since this aterm \
                                   started (an update or a restart forgets which; the person \
                                   here closes it)";

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

#[cfg(any(unix, test))]
fn drive_spawn(
    line: &str,
    rest: &str,
    hosts: &dyn Fn(&SessionId) -> bool,
) -> Result<String, String> {
    const USAGE: &str = "a drive spawns only a worker: spawn agent=<claude|codex|gemini|agy|muse> \
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

#[cfg(any(unix, test))]
static DRIVE_SPAWNED: Mutex<VecDeque<(SessionId, String)>> = Mutex::new(VecDeque::new());

#[cfg(any(unix, test))]
const DRIVE_SPAWNED_MAX: usize = 256;

#[cfg(any(unix, test))]
fn drive_spawned_by(sid: &SessionId) -> Option<String> {
    DRIVE_SPAWNED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .find(|(spawned, _)| spawned == sid)
        .map(|(_, node)| node.clone())
}

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

fn recovery_count_refusal(
    admission: &crate::keeper_link::RecoveryCountAdmission,
) -> Option<&'static str> {
    (!admission.ready()).then_some("ERR recovery-count-custody unconfirmed\n")
}

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
    let (request, scope, driven) = match drive_admission(request, scope, store) {
        Ok(admitted) => admitted,
        Err(refusal) => {
            out.push_owned(refusal.into_bytes());
            return Some(ServeDisposition::Close);
        }
    };
    if let Some(refusal) = fleet_refusal(&request, scope) {
        out.push_owned(refusal.into_bytes());
        return Some(ServeDisposition::Close);
    }
    if let Some(refusal) = empty_selector_refusal(&request) {
        out.push_owned(refusal.into_bytes());
        return Some(ServeDisposition::Close);
    }
    let head = request.head();
    if head.verb == "askpass" {
        let response = if head.selector.is_some() {
            crate::control_askpass::public_reply("ERR askpass refused takes no @<sid>\n")
        } else {
            crate::control_askpass::serve(stream, scope, proxy, store)
        };
        out.push_secret(response);
        return None;
    }
    if let Some(refusal) = worker_frame_refusal(&head, store) {
        out.push_owned(refusal.into_bytes());
        return Some(ServeDisposition::Close);
    }
    if try_proxy_forward(&request, scope, store, sock_dir, stream, reader) {
        return Some(ServeDisposition::Close);
    }
    if head.verb == "subscribe" && head.selector.is_none() {
        return Some(ServeDisposition::Subscribe { request, scope });
    }
    if let Some(len) = request.post_frame() {
        if !run_post_bin(&request, len, reader, active, store, scope, out) {
            return Some(ServeDisposition::Close);
        }
        return None;
    }
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
    out.attach(resp);
    None
}

/// The replies this instance owes for requests whose own action can END the process, each counted by a [`FinalReply`] and waited for at exit.
pub(super) struct FinalReplies {
    owed: Mutex<usize>,
    delivered: Condvar,
    owes: fn(&str, &str) -> bool,
}

/// The instance's [`FinalReplies`]: the requests whose own action can end it.
pub(super) static FINAL_REPLIES: FinalReplies = FinalReplies::new();

fn may_end(verb: &str, rest: &str) -> bool {
    match verb {
        "close" | "invoke" | "confirm" => true,
        "tab" => rest.split_whitespace().next() == Some("close"),
        _ => false,
    }
}

fn writes_input(verb: &str, _rest: &str) -> bool {
    matches!(
        verb,
        "send" | "key" | "hwkey" | "ctrl" | "paste" | "paste-bin" | "feed" | "feed-bin" | "mouse"
    )
}

/// The replies owed for input written into one registry's sessions ([`FinalReplies`] has why).
#[derive(Clone)]
pub(crate) struct InputReplies(Arc<FinalReplies>);

impl Default for InputReplies {
    fn default() -> Self {
        Self(Arc::new(FinalReplies::counting(writes_input)))
    }
}

/// Exclusive admission at a handoff's final Commit.
#[cfg(any(unix, test))]
pub(crate) struct InputRepliesFence<'a> {
    _owed: std::sync::MutexGuard<'a, usize>,
}

impl InputReplies {
    /// [`FinalReplies::enter`] on this count, for a request read by the request grammar ([`Request::head`]).
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

    /// Whether a reply is still owed.
    #[cfg(any(unix, test))]
    pub(crate) fn owed(&self) -> bool {
        self.0.owed_now() > 0
    }

    /// Close admission only while no input reply is owed.
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

fn owed_input_reply(store: &Store, request: &Request) -> Option<FinalReply> {
    let head = request.head();
    if !writes_input(head.verb, head.rest) {
        return None;
    }
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

    /// Count a request's reply as owed when this count owes its verb (`head`, the request grammar's reading of it).
    pub(super) fn enter(&'static self, head: &Head<'_>) -> Option<FinalReply> {
        self.admit(head).then(|| FinalReply {
            replies: Counted::Instance(self),
        })
    }

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

/// One owed reply ([`FinalReplies`]).
pub(crate) struct FinalReply {
    replies: Counted,
}

enum Counted {
    Instance(&'static FinalReplies),
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

/// Wait, at most `bound`, until every [`FinalReply`] is delivered.
pub(crate) fn settle_final_replies(bound: std::time::Duration, input: &InputReplies) -> bool {
    let started = std::time::Instant::now();
    FINAL_REPLIES.settle(bound) && input.settle(bound.saturating_sub(started.elapsed()))
}

#[cfg(test)]
mod final_reply_tests {
    use super::FinalReplies;
    use aterm_types::control_request::head;
    use std::time::Duration;

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
        assert!(
            REPLIES.settle(aterm_spec::wait::MUST_HAPPEN),
            "delivered: the wait ends"
        );
        writer.join().expect("writer thread");
    }

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

#[cfg(test)]
fn read_request_line(reader: &mut impl BufRead) -> Option<String> {
    read_request_line_with_policy(reader, &mut std::io::sink(), false, || Ok(()))
}

fn read_request_line_until(
    stream: &CtlStream,
    reader: &mut impl BufRead,
    refusal: &mut impl Write,
    deadline: std::time::Instant,
) -> Option<String> {
    let mut bound = DeadlineRead::new(stream, deadline);
    read_request_line_with_policy(reader, refusal, false, || bound.arm())
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

struct DeadlineRead<'a> {
    stream: &'a CtlStream,
    deadline: std::time::Instant,
    draining_detached_peer: bool,
}

impl<'a> DeadlineRead<'a> {
    const fn new(stream: &'a CtlStream, deadline: std::time::Instant) -> Self {
        Self {
            stream,
            deadline,
            draining_detached_peer: false,
        }
    }

    fn arm(&mut self) -> std::io::Result<()> {
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
        wait_authenticated_readable(stream)
    })
}

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
        // SAFETY: `stream` remains borrowed for the call and `fd` is a valid initialized `pollfd`.
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
        return Ok(());
    }
    Err(std::io::Error::other(
        "control socket poll returned no read event",
    ))
}

#[cfg(not(unix))]
fn wait_authenticated_readable(_stream: &CtlStream) -> std::io::Result<()> {
    Ok(())
}

fn read_request_line_with_policy(
    reader: &mut impl BufRead,
    refusal: &mut impl Write,
    retry_idle_timeout: bool,
    mut before_read: impl FnMut() -> std::io::Result<()>,
) -> Option<String> {
    let mut buf = Vec::with_capacity(64);
    loop {
        before_read().ok()?;
        match reader.fill_buf() {
            Ok([]) => {
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

const MAX_REQUEST_LINE: usize = aterm_types::control_verbs::MAX_REQUEST_LINE;

const AUTH_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

const AUTHENTICATED_READ_POLL: std::time::Duration = std::time::Duration::from_millis(250);

fn arm_authenticated_read_poll(stream: &CtlStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(AUTHENTICATED_READ_POLL))
}

fn decode_request_line(mut buf: Vec<u8>) -> String {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn request_line_text(line: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line))
}

const MAX_FEED_BIN: usize = 256 * 1024;

/// Maximum `@<sel>` targets in one `subscribe`.
pub(crate) const MAX_SUBSCRIBE_TARGETS: usize = 256;

fn post_frame_len(rest: &str) -> Option<Result<usize, String>> {
    crate::fabric::post_options(rest.split_whitespace())
        .find_map(|tok| tok.strip_prefix("len="))
        .map(crate::fabric::post_len)
}

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

fn announced_body_of(line: &str) -> usize {
    let head = control_request::head(line);
    announced_body(
        &head,
        aterm_types::control_verbs::spec_for(head.verb, head.rest),
    )
}

#[derive(Debug, PartialEq, Eq)]
enum FeedBinFrame<'a> {
    Malformed,
    TooLarge,
    Ok(usize, Option<&'a str>),
}

fn parse_frame_header(rest: &str, keyed: bool) -> FeedBinFrame<'_> {
    let mut it = rest.split_whitespace();
    let Some(len_tok) = it.next() else {
        return FeedBinFrame::Malformed;
    };
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

fn dispatch_mark(ctx: &SessionCtx, scope: Scope, rest: &str) -> String {
    let reads = scope.is_owner_class() || cross_session_authorized(scope, "text", ctx);
    let (reply, owed) =
        crate::session_log::cmd_mark(ctx, control_session::log_actor(scope, ctx), reads, rest);
    if owed {
        ctx.watchers.ring_watchers();
    }
    reply
}

/// RING THE WATCHERS of the session whose stable id is `sid`.
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

fn feed_bin_header<W: Write>(
    selector: Option<&Selector<'_>>,
    route: FeedBinRoute<'_>,
    writer: &mut W,
) -> bool {
    let header_target = match selector.map_or(Resolution::SelfTok, |s| resolve(route.store, s)) {
        Resolution::SelfTok => resolve_active(route.active),
        Resolution::Hosted(target) => Some(target),
        Resolution::Missing => None,
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
    let paste = verb == "paste-bin";
    if !feed_bin_header(selector.as_ref(), route, writer) {
        return false;
    }
    let (n, idem_key) = match frame {
        FeedBinFrame::Ok(n, idem) => (n, idem),
        FeedBinFrame::Malformed => {
            let _ =
                writer.write_all(format!("ERR usage: {verb} <n> then <n> raw bytes\n").as_bytes());
            let _ = writer.flush();
            return true;
        }
        FeedBinFrame::TooLarge => {
            let _ = writer.write_all(
                format!("ERR {verb} too large: the cap is {MAX_FEED_BIN} bytes\n").as_bytes(),
            );
            let _ = writer.flush();
            return false;
        }
    };
    let mut payload = vec![0u8; n];
    if read_exact_authenticated(reader, &mut payload).is_err() {
        return false;
    }

    let active_target = resolve_active(route.active);
    let front_terminal_session = active_target.as_ref().map(|(_, _, session, _)| *session);
    let target = match selector.map_or(Resolution::SelfTok, |s| resolve(route.store, &s)) {
        Resolution::SelfTok => active_target.clone(),
        Resolution::Hosted(target) => Some(target),
        Resolution::Missing => None,
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

    if paste {
        let principal = if route.scope.is_owner_class() {
            NativeControlPrincipal::Owner
        } else {
            NativeControlPrincipal::Edge
        };
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

    ctx.licence.fence(false);

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

    if let Some(refusal) = crate::pty_idem::empty_payload_refusal(idem_key, payload.is_empty()) {
        let _ = writer.write_all(refusal.as_bytes());
        let _ = writer.flush();
        return true;
    }
    let input_route = InputRoute::of(
        class,
        is_cross,
        front_terminal_session == Some(target_session),
        target_session,
    );
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
        crate::pty_idem::guarded_waking(&ctx, route.scope, verb, idem_key, &wake, |_| {
            let event = if paste {
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

fn write_err<W: Write>(writer: &mut W, msg: &[u8]) {
    let _ = writer.write_all(msg);
    let _ = writer.flush();
}

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

fn run_subscribe_socket(
    head: Head<'_>,
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
    writer: &mut CtlStream,
) {
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
    #[cfg(not(unix))]
    run_subscribe_with_peer_probe(head, active, store, scope, &mut out, || {
        subscription_peer_gone(stream)
    });
    aterm_uds::hangup::hang_up(stream);
}

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

#[cfg(unix)]
const PUSH_PROBE_FLAGS: libc::c_int = libc::MSG_PEEK | libc::MSG_DONTWAIT;

#[cfg(unix)]
fn peek_push_peer(stream: &CtlStream) -> std::io::Result<usize> {
    use std::os::fd::AsRawFd as _;

    let fd = stream.as_raw_fd();
    let mut byte = 0_u8;
    // SAFETY: `stream` is borrowed for the call and `MSG_PEEK | MSG_DONTWAIT` reads into one owned byte.
    let n = unsafe { libc::recv(fd, (&raw mut byte).cast(), 1, PUSH_PROBE_FLAGS) };
    usize::try_from(n).map_err(|_| std::io::Error::last_os_error())
}

#[cfg(not(unix))]
fn peek_push_peer(stream: &CtlStream) -> std::io::Result<usize> {
    let mut byte = [0_u8; 1];
    stream.read_nonblocking(&mut byte)
}

const SUBSCRIBE_USAGE: &str = "ERR usage: subscribe [@<sel>[,@<sel>...]|@*] <streams> \
                               [since=<seq>] [since-turn=<id>] [since-block=<id>] \
                               [since-log=<seq> log-ep=<e>] [every-frame]\n";

fn subscribe_usage_bytes() -> Vec<u8> {
    format!(
        "{SUBSCRIBE_USAGE}  <streams>: a comma list from {}, naming at least one besides \
         timestamps (ts) and trim; no target means @. (the active tab); mail takes \
         mail:kinds=<k,..>, mail:from=<class|principal> and/or mail:topic=<t>; log takes \
         log:lvl=<session|run|turn|step> and/or log:class=turn\n",
        aterm_types::control_verbs::SUBSCRIBE_STREAMS.join(",")
    )
    .into_bytes()
}

/// Authorize a `subscribe` request and, on success, flip the connection to push mode.
pub(crate) fn run_subscribe_with_peer_probe<W: Write, L: subscribe::PeerLiveness>(
    head: Head<'_>,
    active: &ActiveHandle,
    store: &Store,
    scope: Scope,
    writer: &mut W,
    liveness: L,
) {
    let usage = subscribe_usage_bytes();
    let usage = usage.as_slice();
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
    let mut since: Option<u64> = None;
    let mut since_turn: Option<u64> = None;
    let mut since_block: Option<u64> = None;
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

    let mut targets: Vec<subscribe::ResolvedTarget> = Vec::new();
    let mut sub_map: Vec<(u64, String)> = Vec::new();
    let mut parsed = 0usize;
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
        if targets.iter().any(|t| t.local_id == local_id) {
            continue;
        }
        sub_map.push((local_id, ctx.self_id.as_str().to_string()));
        targets.push(subscribe::ResolvedTarget {
            local_id,
            term,
            src: subscribe::WatchSource::of(&ctx),
        });
    }
    if targets.is_empty() && !adopt_all {
        let _ = writer.write_all(b"ERR usage: at least one @<sel> target\n");
        let _ = writer.flush();
        return;
    }

    if targets.len() > 1
        && (since.is_some() || since_turn.is_some() || since_block.is_some() || log_since.is_some())
    {
        return write_err(
            writer,
            b"ERR resume anchors (since=/since-turn=/since-block=/since-log=) require a single \
              target\n",
        );
    }

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

type Target = (Arc<Mutex<Terminal>>, i32, u64, Arc<SessionCtx>);

fn empty_selector_refusal(request: &Request) -> Option<String> {
    let body = control_request::words(request.line().trim_end_matches('\r'))
        .next()?
        .strip_prefix('@')?;
    (!aterm_types::control_verbs::selector_body_names_something(body))
        .then(|| format!("{}\n", aterm_types::control_verbs::EMPTY_SELECTOR))
}

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

/// Whether a cross-session call (target != the connection's own session) is authorized for `verb`.
pub(crate) fn cross_session_authorized(scope: Scope, verb: &str, target_ctx: &SessionCtx) -> bool {
    let Some(need) = required_op(verb) else {
        return false;
    };
    scope_holds_op(scope, need, target_ctx)
}

fn dispatch_authorized(
    scope: Scope,
    form: &Resolved,
    verb: &str,
    rest: &str,
    target_ctx: &SessionCtx,
) -> bool {
    need_of(form, verb, rest).is_some_and(|need| scope_holds_need(scope, need, target_ctx))
}

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

fn poke_connections_on_ok(proxy: &EventLoopProxy<Wake>, resp: String) -> String {
    if resp.starts_with("OK") {
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

const PASTE_RECEIPT_WAIT: std::time::Duration =
    std::time::Duration::from_millis(aterm_control::wire::WAIT_DEFAULT_MS);

const PASTE_UNREAD: &str =
    "ERR in-doubt paste-unread: queued, the program has not read it yet (signal drops it)\n";

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

const QUEUED_INPUT_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

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

enum Waited {
    Wrote(crate::input::EgressReceipt),
    Unrun,
    Queued,
    HungUp,
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputRoute {
    Seam { pin: Option<u64> },
    Background,
}

impl InputRoute {
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

fn cross_mouse(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    session: u64,
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
) -> String {
    match cross_mouse_apply(term, ctx, rest) {
        Ok(true) => {
            let _ = proxy.send_event(Wake::redraw(session));
            "OK\n".to_string()
        }
        Ok(false) => "OK\n".to_string(),
        Err(e) => e,
    }
}

fn cross_mouse_apply(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    rest: &str,
) -> Result<bool, String> {
    let ev = parse_mouse(rest)?;
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
        Egress::Reported(crate::input::Delivery::Failed) => Err("ERR write failed\n".to_string()),
        Egress::Reported(_) => Ok(false),
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
    let policy = crate::app_render::pty_resize_policy(master);
    let (pending, journal, cast_stamp) = {
        let mut t = term_lock(term);
        let pending = t
            .resize_offloading_scrollback_with_policy(rows, cols, policy)
            .map(|pending| {
                let rewrap = ctx.rewrap_gauge.begin_job(pending.lines_total());
                (pending, rewrap)
            });
        ctx.crash_journal.host_resized(&t);
        (
            pending,
            crate::resize_ledger::JournalCopy::take(&t),
            crate::cast::ResizeStamp::take(&t),
        )
    };
    if let Some((pending, rewrap)) = pending {
        let never = std::sync::atomic::AtomicBool::new(false);
        let mut ended = aterm_messages::WaitEnd::Done;
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
        if let Some(proxy) = proxy {
            let _ = proxy.send_event(Wake::redraw(session));
        }
    }
    let cell_px = term_lock(term).host_cell_pixel_size();
    aterm_pty::resize_with_cell_px(master, rows, cols, cell_px);
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

struct InputAdmission<'a> {
    class: Input,
    verb: &'a str,
    ctx: &'a SessionCtx,
    session: u64,
    is_cross: bool,
    args: &'a [u8],
    unread_ok: bool,
    usage: Option<String>,
}

fn admit(a: InputAdmission<'_>) -> Result<Option<String>, String> {
    admit_with(a, || Ok(())).map(|((), warning)| warning)
}

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

struct InputWords {
    epoch: Option<u32>,
    cause: Option<aterm_slog::ShortId>,
}

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
    // The key name is the request's own word: refused here, before the `id=` claim,
    // it consumes no sequence (refused inside it, the claim would stand in doubt).
    if verb == "key" && parse_key(rest).is_none() {
        return Err(control_input::KEY_USAGE.to_string());
    }
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

fn serve_session_verb(
    head: &Head<'_>,
    form: &Resolved,
    target: &Target,
    front_active_session: Option<u64>,
    cx: &DispatchCx<'_>,
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
    let is_cross = !matches!(selector, None | Some(Selector::SelfTok));
    let (term, master, session, self_ctx) = (&target.0, target.1, target.2, &target.3);
    let ctx: &SessionCtx = self_ctx;
    let class = form.row.input_class();
    let targets_front = front_routed(
        is_cross,
        scope.is_owner_class(),
        front_active_session,
        session,
    );
    let route = InputRoute::of(class, is_cross, targets_front, session);
    let front = |ev, pin, completion: Option<crate::control_input_ack::InputCompletion>| {
        post_input_reply_to_observed(proxy, vec![ev], pin, completion.map(|c| c.into_mark()))
    };

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
    let host = GuiHost::with_fleet(session, term, Some(proxy), store, ctx);

    match json_ask(form, verb, rest) {
        JsonAsk::No => {}
        JsonAsk::Refuse(refusal) => return refusal,
        JsonAsk::Emit(body) => {
            return match verb {
                "text" => match control_query::text_args(&body) {
                    Ok(args) => control_query::cmd_text_json_looked(term, ctx, args),
                    Err(err) => err,
                },
                "screen" => control_query::cmd_screen_styled_json(term, &ctx.frame, &body),
                "cursor" => control_query::cmd_cursor_json(term),
                "dims" => control_query::cmd_dims_json(term, session, proxy),
                "blocks" => control_selection::cmd_blocks_json(&host, session, &body),
                "edges" | "grants" => control_session::cmd_edges_json(ctx),
                "log" => control_session::cmd_log_json(ctx, &body),
                "sys" => crate::sys_host::cmd_sys(
                    store,
                    session,
                    term,
                    master,
                    scope.is_owner_class(),
                    rest,
                ),
                _ => no_handler(verb),
            };
        }
    }

    let (leading, rest) = control_input::take_leading_options(class, rest);
    let idem_key = leading.idem;
    let as_person = leading.as_person;
    let if_supervisor = leading.if_supervisor.as_deref();

    let fenced_write = leading.guard.is_some()
        || leading.if_gen.is_some()
        || leading.if_fp.is_some()
        || leading.if_human.is_some();
    if let Some(keep_hand) = control_input::control_fence(class, verb, rest, fenced_write) {
        ctx.licence.fence(keep_hand);
    }

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

    if verb == "send"
        && let Some(refusal) = crate::pty_idem::empty_payload_refusal(
            idem_key.as_deref(),
            control_input::send_bytes(rest).is_empty(),
        )
    {
        return refusal;
    }
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
    let log_wake = || ctx.watchers.ring_watchers();
    let idem = idem_key.as_deref();
    let resp = crate::pty_idem::guarded_waking(ctx, scope, verb, idem, &log_wake, |w| match verb {
        "text" => match control_query::text_args(rest) {
            Ok(args) => control_query::cmd_text_opt(term, &ctx.frame, args),
            Err(err) => err,
        },
        "screen" => control_query::cmd_screen_styled_json(term, &ctx.frame, rest),
        "cursor" => control_query::cmd_cursor(term),
        "cell" => control_query::cmd_cell(term, rest),
        "search" => control_query::cmd_search(term, rest, &ctx.rewrap_gauge),
        "edges" | "grants" => control_session::cmd_edges(ctx),
        "inbox" => crate::fabric::cmd_inbox_verb(ctx, rest),
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
        "family" => control_session::cmd_family(ctx, store, scope, rest),
        "ready" => control_session::cmd_ready(term, store, session, ctx, rest),
        "await" if rest.split_whitespace().next() == Some("consent") => {
            let exited = control_session::exit_probe(store, session);
            let exited = || exited(store);
            crate::control_privacy::cmd_await_consent(proxy, scope, session, ctx, &exited, rest)
        }
        "await" if rest.split_whitespace().next() == Some("sys") => {
            crate::sys_host::cmd_await_sys(term, store, session, scope, rest)
        }
        "await" if rest.split_whitespace().next() == Some("momentum") => {
            let read = || typing_momentum_reading(proxy, session);
            control_session::cmd_await_momentum(term, store, session, ctx, rest, &read)
        }
        "await" if rest.split_whitespace().next() == Some("edit") => {
            crate::control_edit::await_edit(proxy, scope, rest)
        }
        "await" if rest.split_whitespace().next() == Some("wake") => {
            crate::occupancy_wakes::cmd_await_wake(store, session, rest)
                .into_body_retaining(reply_retention)
        }
        "wake" => crate::occupancy_wakes::cmd_wake(store, session, rest),
        "waits" => crate::occupancy_wakes::cmd_waits(store, session, rest),
        "await" => control_session::cmd_await(term, store, session, ctx, rest),
        "task" => crate::task_record::cmd_task(ctx, session, scope, rest),
        "result" => crate::task_record::cmd_result(ctx, session, rest),
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
            let momentum = || typing_momentum_reading(proxy, session);
            let driver = turn_driver(scope, ctx);
            let by = control_session::turn_by(scope, driver.as_ref());
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
            let refused = std::cell::RefCell::new(None);
            let applied = |ev: InputEvent| {
                let wrote = turn_write(ctx, if_supervisor, || {
                    deliver(route, class, front, term, ctx, ev, Some(mark.completion()))
                });
                let delivered = wrote.is_ok();
                *refused.borrow_mut() = wrote.err().flatten();
                delivered
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
                    witness: Some(w),
                    refused: &|| refused.take(),
                },
            )
        }
        "send" | "key" if as_person => control_input::person_input(proxy, session, verb, rest),
        "send" | "key" if fence.is_armed() => {
            if verb == "key" {
                control_input::cmd_key_fenced_observed(term, ctx, &fence, rest, completion())
            } else {
                control_input::cmd_send_fenced_observed(term, ctx, &fence, rest, completion())
            }
        }
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
        "restart" => control_input::restart_verb(master, rest, self_ctx),
        "reset" => {
            let reply = crate::manual_reset::cmd_reset(session, term, ctx, rest);
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
        "image" if rest.split_whitespace().next() == Some("read") => control_media::cmd_image_read(
            term,
            rest.strip_prefix("read").unwrap_or(rest).trim_start(),
        ),
        "image" if !scope.is_owner_class() => image_frame_refusal(ctx),
        "image" => control_media::cmd_image(proxy, rest, sock_dir, is_cross.then_some(session))
            .into_body_retaining(reply_retention),
        "close" => match control_media::close_no_arg(rest) {
            Ok(()) => control_media::cmd_close(
                proxy,
                session,
                ctx.self_id.as_str(),
                control_session::caller_actor(scope, ctx),
            ),
            Err(usage) => usage,
        },
        "resize" => match route {
            InputRoute::Seam { pin: Some(_) } => front_routed_resize(proxy, session, rest),
            InputRoute::Seam { pin: None } => control_input::cmd_resize(proxy, rest),
            InputRoute::Background => match control_input::cross_resize_px_refusal(rest) {
                Some(refusal) => refusal,
                None => cross_resize(term, master, ctx, session, Some(proxy), rest),
            },
        },
        "scroll" => match route {
            InputRoute::Seam { pin: Some(_) } => front_routed_scroll(term, proxy, session, rest),
            InputRoute::Seam { pin: None } => control_input::cmd_scroll(term, proxy, rest),
            InputRoute::Background => cross_scroll(term, rest),
        },
        "dims" => control_query::cmd_dims(term, session, proxy),
        "lines" => control_query::cmd_lines(term),
        "offscreen" => control_query::cmd_offscreen(term, &ctx.frame, rest),
        "mainscreen" => refuse_stray_args(verb, rest).unwrap_or_else(|| {
            crate::alt_screen_resync::mainscreen_via_host(term, master)
                .unwrap_or_else(|| control_query::cmd_mainscreen(&host, session))
        }),
        "line" => control_query::cmd_line(term, rest),
        "modes" => control_query::cmd_modes(term),
        "custody" => control_query::cmd_custody(term),
        "title" => control_query::cmd_title(term),
        "cwd" => control_query::cmd_cwd(term),
        "blocks" => control_selection::cmd_blocks(&host, session, rest),
        "blocktext" => control_query::cmd_blocktext_args(rest, |id, trim| {
            control_selection::blocktext(&host, session, id, trim)
        }),
        "wait" => control_selection::cmd_wait(&host, session, rest),
        "colors" => control_query::cmd_colors(term),
        "select" => control_selection::cmd_select(&host, session, rest),
        "selection" => control_selection::cmd_selection(&host, session),
        "copy" => control_selection::cmd_copy(&host, session),
        "cast" if rest.split_whitespace().next() == Some("frames") => {
            let policy = crate::app_render::pty_resize_policy(master);
            control_session::cmd_cast_frames(
                ctx,
                policy,
                rest.strip_prefix("frames").unwrap_or("").trim(),
            )
        }
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
        "temporal" => control_session::cmd_temporal(ctx, term, rest),
        "history" => control_session::cmd_history(ctx, rest),
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
        "timeline" => control_session::cmd_timeline(ctx, rest),
        "log" => control_session::cmd_log(ctx, rest),
        "brief" => control_session::cmd_brief_with_jobs(
            ctx,
            store,
            rest,
            crate::sys_host::brief_jobs_line(session),
        ),
        "sys" => {
            crate::sys_host::cmd_sys(store, session, term, master, scope.is_owner_class(), rest)
        }
        "occupancy" => {
            let sid = store
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .by_local(session)
                .map(|h| h.sid.as_str().to_string());
            crate::occupancy_host::cmd_occupancy(sid.as_deref(), rest)
        }
        "mark" => dispatch_mark(ctx, scope, rest),
        "resizes" => crate::resize_ledger::cmd_resizes(ctx, term, rest),
        "status" => control_media::cmd_session_status(proxy, session, rest),
        "resources" => {
            let reply = crate::resource_jobs::command(ctx, scope.is_owner_class(), rest);
            if reply.starts_with("OK") {
                let _ = proxy.send_event(Wake::Vitals);
            }
            reply
        }
        "harness" => crate::harness_policy_verb::cmd(rest, scope.is_owner_class(), || {
            control_media::call_main(proxy, |tx| Wake::ReadHarnessPolicy { session, reply: tx })
                .map_err(|e| e.to_string())
                .and_then(|facts| facts)
        }),
        "subscribe" => SUBSCRIBE_USAGE.to_string(),
        _ => no_handler(verb),
    });
    control_session::ledger_typed_input(term, ctx, scope, verb, rest, &resp);
    crate::input_stall::with_warning(stamp_input_seq(class, resp, term), warning.as_deref())
}

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
pub(crate) const fn cursor_style_name(style: CursorStyle) -> &'static str {
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
        _ => "unknown",
    }
}

#[derive(Debug, PartialEq, Eq)]
enum JsonAsk {
    No,
    Refuse(String),
    Emit(String),
}

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

/// Whether `rest` carries the `--json` / `json` flag ([`aterm_types::control_verbs::json_flag`], the rule [`json_ask`] reads), and `rest` with every such token removed.
pub(crate) fn take_json_flag(rest: &str) -> (bool, String) {
    use aterm_types::control_verbs::json_flag;
    let kept = rest
        .split_whitespace()
        .filter(|token| !json_flag(token))
        .collect::<Vec<_>>()
        .join(" ");
    (json_flag(rest), kept)
}

fn post_input_reply(
    proxy: &EventLoopProxy<Wake>,
    batch: Vec<InputEvent>,
) -> Result<InputOutcome, String> {
    post_input_reply_to(proxy, batch, None)
}

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

/// One `hold` request as a client's line carries it (`hold <sid> off`, `@<sid> hold off`).
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

/// One `meta <rest>` line as the dispatch answers it from the Owner token ([`serve_meta`], with no proxy to wake).
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

/// [`meta_line_for_test`] served on connection `conn`.
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

/// [`meta_wakes_for_test`] with the kernel's look at a self-claim's holder answered by `prove`.
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

#[cfg(test)]
mod update_check_refusal_tests {
    use super::control_media::MainHopError;

    #[test]
    fn a_refused_settings_read_is_answered_err_in_its_own_words() {
        let dialog = MainHopError::Reason("aterm is showing a dialog; answer it, then retry");
        assert_eq!(
            super::update_check_refused(&dialog),
            "ERR update check: aterm is showing a dialog; answer it, then retry\n"
        );
    }

    #[test]
    fn a_per_process_candidate_publishes_only_after_the_commit() {
        assert!(super::await_commit_to_publish(None), "an ordinary launch");
        let gate = crate::spawn::DeferredReaderGate::closed();
        let waiter = {
            let gate = gate.clone();
            std::thread::spawn(move || super::await_commit_to_publish(Some(&gate)))
        };
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
