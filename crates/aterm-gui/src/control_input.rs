// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Input-injection verbs + their parsers.

use std::sync::{Arc, Mutex};

use aterm_core::grid::{MAX_GRID_COLS, MAX_GRID_ROWS};
use aterm_core::terminal::{RowMatch, RowRange, Terminal, first_matching_row};
use aterm_session::sink::SinkWriter;
use aterm_types::control_verbs::Input;
use aterm_window::event_loop::EventLoopProxy;

use super::post_input_reply;
use crate::input::{Delivery, InputEvent, InputOutcome, Source};
use crate::{ReadOn, SessionCtx, TabAction, Wake, term_lock, term_lock_whole_when};

/// `scroll <up|down|top|bottom|prev-prompt|next-prompt|N>` -> move the viewport and answer `OK <display_offset> <scrollback_lines>`.
pub(crate) fn cmd_scroll(
    term: &Arc<Mutex<Terminal>>,
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
) -> String {
    let intent = match super::parse_scroll_intent(rest) {
        Ok(intent) => intent,
        Err(usage) => return usage,
    };
    match post_input_reply(proxy, vec![InputEvent::ScrollView(intent)]) {
        Ok(_) => {}
        Err(e) => return e,
    }
    let t = term_lock(term);
    let offset = t.grid().display_offset();
    let max = t.grid().scrollback_lines();
    format!("OK {offset} {max}\n")
}

/// The class of a control input verb's key as that verb delivers `body` (fix E8): `key`'s one
/// key, `send`'s text (a lone `\n` is Enter), `feed`'s raw bytes; `paste`, `ctrl` and the rest
/// choose nothing.
pub(crate) fn key_class(verb: &str, body: &str) -> crate::session_log::KeyClass {
    use crate::session_log::KeyClass;
    match verb {
        "key" => parse_key(body)
            .as_ref()
            .map_or(KeyClass::Other, KeyClass::of_event),
        "send" => KeyClass::of_bytes(&send_bytes(body)),
        "feed" => feed_bytes(body).map_or(KeyClass::Other, |b| KeyClass::of_bytes(&b)),
        _ => KeyClass::Other,
    }
}

/// `send <text>` -> the bytes to write to the PTY.
pub(crate) fn send_bytes(rest: &str) -> Vec<u8> {
    aterm_types::control_request::send_payload(rest.as_bytes())
}

/// Parse the optional trailing `mods=<list>` token (e.g. `mods=ctrl+shift`), returning the modifier mask and the rest of the line with the token removed.
pub(crate) fn take_mods(rest: &str) -> Option<(aterm_types::keyboard::Modifiers, String)> {
    use aterm_types::keyboard::Modifiers;
    let mut m = Modifiers::empty();
    let mut kept: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(list) = tok.strip_prefix("mods=") {
            for name in list.split(['+', ',']) {
                m |= match name {
                    "shift" => Modifiers::SHIFT,
                    "ctrl" | "control" => Modifiers::CTRL,
                    "alt" | "option" => Modifiers::ALT,
                    "meta" => Modifiers::META,
                    "hyper" => Modifiers::HYPER,
                    "super" | "cmd" | "command" => Modifiers::SUPER,
                    "" => Modifiers::empty(),
                    _ => return None,
                };
            }
        } else {
            kept.push(tok);
        }
    }
    Some((m, kept.join(" ")))
}

fn take_event_type(rest: &str) -> Option<(aterm_types::keyboard::KeyEventType, String)> {
    use aterm_types::keyboard::KeyEventType;
    let mut et = KeyEventType::Press;
    let mut kept: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix("type=") {
            et = match v {
                "press" | "down" => KeyEventType::Press,
                "repeat" => KeyEventType::Repeat,
                "release" | "up" => KeyEventType::Release,
                _ => return None,
            };
        } else {
            kept.push(tok);
        }
    }
    Some((et, kept.join(" ")))
}

fn take_base_layout(rest: &str) -> Option<(Option<char>, String)> {
    let mut base: Option<char> = None;
    let mut kept: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(v) = tok.strip_prefix("base=") {
            let mut chars = v.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => base = Some(c),
                _ => return None,
            }
        } else {
            kept.push(tok);
        }
    }
    Some((base, kept.join(" ")))
}

fn named_key_from_token(body: &str) -> Option<aterm_types::keyboard::NamedKey> {
    use aterm_types::keyboard::NamedKey as Nk;
    Some(match body {
        "enter" => Nk::Enter,
        "tab" => Nk::Tab,
        "esc" | "escape" => Nk::Escape,
        "backspace" => Nk::Backspace,
        "delete" | "del" => Nk::Delete,
        "insert" | "ins" => Nk::Insert,
        "up" => Nk::ArrowUp,
        "down" => Nk::ArrowDown,
        "right" => Nk::ArrowRight,
        "left" => Nk::ArrowLeft,
        "home" => Nk::Home,
        "end" => Nk::End,
        "pageup" | "pgup" => Nk::PageUp,
        "pagedown" | "pgdn" => Nk::PageDown,
        "f1" => Nk::F1,
        "f2" => Nk::F2,
        "f3" => Nk::F3,
        "f4" => Nk::F4,
        "f5" => Nk::F5,
        "f6" => Nk::F6,
        "f7" => Nk::F7,
        "f8" => Nk::F8,
        "f9" => Nk::F9,
        "f10" => Nk::F10,
        "f11" => Nk::F11,
        "f12" => Nk::F12,
        "space" => Nk::Space,
        "capslock" => Nk::CapsLock,
        "numlock" => Nk::NumLock,
        "scrolllock" => Nk::ScrollLock,
        "printscreen" | "prtsc" => Nk::PrintScreen,
        "pause" | "break" => Nk::Pause,
        "menu" | "contextmenu" => Nk::ContextMenu,
        "f13" => Nk::F13,
        "f14" => Nk::F14,
        "f15" => Nk::F15,
        "f16" => Nk::F16,
        "f17" => Nk::F17,
        "f18" => Nk::F18,
        "f19" => Nk::F19,
        "f20" => Nk::F20,
        "f21" => Nk::F21,
        "f22" => Nk::F22,
        "f23" => Nk::F23,
        "f24" => Nk::F24,
        "f25" => Nk::F25,
        "f26" => Nk::F26,
        "f27" => Nk::F27,
        "f28" => Nk::F28,
        "f29" => Nk::F29,
        "f30" => Nk::F30,
        "f31" => Nk::F31,
        "f32" => Nk::F32,
        "f33" => Nk::F33,
        "f34" => Nk::F34,
        "f35" => Nk::F35,
        "kp0" => Nk::Numpad0,
        "kp1" => Nk::Numpad1,
        "kp2" => Nk::Numpad2,
        "kp3" => Nk::Numpad3,
        "kp4" => Nk::Numpad4,
        "kp5" => Nk::Numpad5,
        "kp6" => Nk::Numpad6,
        "kp7" => Nk::Numpad7,
        "kp8" => Nk::Numpad8,
        "kp9" => Nk::Numpad9,
        "kpdot" | "kpdecimal" => Nk::NumpadDecimal,
        "kpdiv" | "kpdivide" => Nk::NumpadDivide,
        "kpmul" | "kpmultiply" => Nk::NumpadMultiply,
        "kpsub" | "kpminus" => Nk::NumpadSubtract,
        "kpadd" | "kpplus" => Nk::NumpadAdd,
        "kpenter" => Nk::NumpadEnter,
        "kpequal" => Nk::NumpadEqual,
        "kpsep" | "kpseparator" => Nk::NumpadSeparator,
        "kpbegin" => Nk::NumpadBegin,
        "kpleft" => Nk::NumpadArrowLeft,
        "kpright" => Nk::NumpadArrowRight,
        "kpup" => Nk::NumpadArrowUp,
        "kpdown" => Nk::NumpadArrowDown,
        "kppageup" | "kppgup" => Nk::NumpadPageUp,
        "kppagedown" | "kppgdn" => Nk::NumpadPageDown,
        "kphome" => Nk::NumpadHome,
        "kpend" => Nk::NumpadEnd,
        "kpinsert" | "kpins" => Nk::NumpadInsert,
        "kpdelete" | "kpdel" => Nk::NumpadDelete,
        "shiftleft" => Nk::ShiftLeft,
        "shiftright" => Nk::ShiftRight,
        "ctrlleft" | "controlleft" => Nk::ControlLeft,
        "ctrlright" | "controlright" => Nk::ControlRight,
        "altleft" => Nk::AltLeft,
        "altright" => Nk::AltRight,
        "superleft" => Nk::SuperLeft,
        "superright" => Nk::SuperRight,
        "hyperleft" => Nk::HyperLeft,
        "hyperright" => Nk::HyperRight,
        "metaleft" => Nk::MetaLeft,
        "metaright" => Nk::MetaRight,
        "mediaplay" => Nk::MediaPlay,
        "mediapause" => Nk::MediaPause,
        "mediaplaypause" => Nk::MediaPlayPause,
        "mediastop" => Nk::MediaStop,
        "mediareverse" => Nk::MediaReverse,
        "mediafastforward" | "mediaff" => Nk::MediaFastForward,
        "mediarewind" => Nk::MediaRewind,
        "medianext" | "mediatracknext" => Nk::MediaTrackNext,
        "mediaprev" | "mediatrackprevious" => Nk::MediaTrackPrevious,
        "mediarecord" => Nk::MediaRecord,
        "volumeup" => Nk::AudioVolumeUp,
        "volumedown" => Nk::AudioVolumeDown,
        "mute" => Nk::AudioVolumeMute,
        _ => return None,
    })
}

/// Parse `key <name> [mods=<list>] [type=<t>] [base=<c>]` into an [`InputEvent::Key`].
pub(crate) fn parse_key(rest: &str) -> Option<InputEvent> {
    use aterm_types::keyboard::Key;
    let (mut mods, body) = take_mods(rest)?;
    let (event_type, body) = take_event_type(&body)?;
    let (base_explicit, body) = take_base_layout(&body)?;
    let (prefix_mods, body) = take_inline_mods(body.trim());
    mods |= prefix_mods;
    let body = body.trim();
    let Some(named) = named_key_from_token(body) else {
        let mut chars = body.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) => Some(InputEvent::Key {
                key: Key::Character(c.to_ascii_lowercase()),
                mods,
                base_layout: base_explicit,
                layout_text: None,
                event_type,
            }),
            _ => None,
        };
    };
    Some(InputEvent::Key {
        key: Key::Named(named),
        mods,
        base_layout: base_explicit,
        layout_text: None,
        event_type,
    })
}

fn take_inline_mods(body: &str) -> (aterm_types::keyboard::Modifiers, &str) {
    use aterm_types::keyboard::Modifiers;
    let mut m = Modifiers::empty();
    let mut rest = body;
    while let Some(plus) = rest.find('+') {
        let bit = match &rest[..plus] {
            "shift" => Modifiers::SHIFT,
            "ctrl" | "control" => Modifiers::CTRL,
            "alt" | "option" => Modifiers::ALT,
            "meta" => Modifiers::META,
            "hyper" => Modifiers::HYPER,
            "super" | "cmd" | "command" => Modifiers::SUPER,
            _ => break,
        };
        m |= bit;
        rest = &rest[plus + 1..];
    }
    (m, rest)
}

/// The event a fencing verb's body delivers, parsed with that verb's own parser so the fence classifies exactly what the verb sends.
pub(crate) fn control_press(verb: &str, rest: &str) -> Option<InputEvent> {
    match verb {
        "key" => parse_key(rest),
        "ctrl" => parse_ctrl(rest),
        "paste" => Some(paste_event(rest)),
        "focus" => parse_focus(rest).map(InputEvent::Focus),
        "mouse" => parse_mouse(rest).ok(),
        "resize" => parse_window_resize(rest).ok().filter(|event| match event {
            InputEvent::ResizeWindowPx { width, height } => *width != 0 && *height != 0,
            _ => true,
        }),
        _ => None,
    }
}

/// Licence fence for one authorized attempt of a verb whose input class is `class`: `None` is no fence.
pub(crate) fn control_fence(
    class: Input,
    verb: &str,
    rest: &str,
    fenced_write: bool,
) -> Option<bool> {
    if !class.contains(Input::FENCES) {
        return None;
    }
    let Some(ev) = control_press(verb, rest) else {
        return Some(false);
    };
    let fence = crate::app_input::press_fence(&ev, false);
    if fenced_write {
        Some(fence == Some(true))
    } else {
        fence
    }
}

/// The usage line every malformed `key` body answers (the plain, guarded and both cross-session arms).
pub(crate) const KEY_USAGE: &str = "ERR usage: key [id=<epoch>:<producer>:<seq>] [if=<re>] \
                                    [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] \
                                    [if-supervisor=<holder>] [if-supervisor-revision=<n>] \
                                    [harness-duty=<scope>] [unread=ok] \
                                    [act=approve|deny|answer] [<mod>+]<name|char> \
                                    [mods=<list>] [type=press|repeat|release] [base=<c>] — \
                                    names include enter tab esc space backspace delete insert \
                                    up down left right home end pageup pagedown f1..f35 \
                                    kp0..kp9; e.g. key ctrl+c, key y\n";

/// Leading `k=v` options stripped from a `send`/`key`/`turn` line before verb-specific parsing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LeadingInputOptions {
    /// The idempotency key's raw `<epoch>:<producer>:<seq>` value.
    pub idem: Option<String>,
    /// The `if=<re>` guard's raw pattern.
    pub guard: Option<String>,
    /// The `if-gen=<epoch>.<seq>` fence: the screen generation the caller last read (`status gen=`, `agent_gen=`).
    pub if_gen: Option<crate::control::ScreenGen>,
    /// The `if-fp=<hex16>` fence: the visible screen's FNV-1a-64 the caller last read (`status hash=`, `turn hash=`).
    pub if_fp: Option<u64>,
    /// The `if-human=<n>` fence: the person-gesture count the caller last read (`status human_seq=`, `text --json`'s `"human_seq"`).
    pub if_human: Option<u64>,
    /// `unread=ok`: write even though the program has left earlier input unread, the one override of the unread-input gate (`crate::input_stall::admission`).
    pub unread_ok: bool,
    /// The `if-supervisor=<holder>` fence (the self-claim contract, §2).
    pub if_supervisor: Option<String>,
    /// A typed supervisor input's exact authority revision, read from `status`.
    pub supervisor_revision: Option<u64>,
    /// The duty this coordinated input performs, never inferred from a key's text.
    pub harness_duty: Option<aterm_agent::harness::contract::Scope>,
    /// `cause=@<off>|<seq>`: the mail or record this input acted on, which the turn it opens names.
    pub cause: Option<crate::session_log::CauseRef>,
    /// `act=approve|deny|answer`: what this input means for the approval box or question it dismisses.
    pub act: Option<aterm_slog::ActWord>,
    /// A recognized but malformed leading option (a guard with no pattern, an option given twice), answered by the dispatch after the halt gate, so a halted session says `ERR halted` whatever the rest of the line is.
    pub refusal: Option<String>,
    /// `as=person` (a development seam, [`PERSON_SEAM`]).
    pub as_person: bool,
}

/// Whether this build takes `as=person` ([`LeadingInputOptions::as_person`]): a development build only.
pub(crate) const PERSON_SEAM: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// What a build without the seam answers `as=person` with: nothing is written.
pub(crate) const PERSON_SEAM_REFUSED: &str = "ERR usage: as=person is a development seam (a debug \
     or dev-seams build's); a shipped aterm writes every control verb as a controller\n";

fn take_as_person(opts: &mut LeadingInputOptions, seam: bool) {
    if !seam {
        opts.refusal = Some(PERSON_SEAM_REFUSED.to_string());
    } else if std::mem::replace(&mut opts.as_person, true) {
        opts.refusal = Some("ERR usage: as=person given twice\n".to_string());
    }
}

/// The App lane's `unread=ok`.
pub(crate) fn take_app_unread_ok(class: Input, rest: &str) -> (bool, &str) {
    if !class.contains(Input::APP_UNREAD) {
        return (false, rest);
    }
    let trimmed = rest.trim_start();
    match trimmed.split_once(char::is_whitespace) {
        Some(("unread=ok", tail)) => (true, tail.trim_start()),
        None if trimmed == "unread=ok" => (true, ""),
        _ => (false, rest),
    }
}

/// The refusal for a malformed or repeated `act=`: nothing is written.
pub(crate) const ACT_USAGE: &str =
    "ERR usage: act=approve|deny|answer says what the input means for the box it dismisses, once\n";

const IF_SUPERVISOR_USAGE: &str =
    "ERR usage: if-supervisor=<holder> needs the holder the claim was made under\n";

const SUPERVISOR_INPUT_USAGE: &str = "ERR usage: typed supervisor input takes if-supervisor=<holder> if-supervisor-revision=<n> harness-duty=<scope> together, once\n";

impl LeadingInputOptions {
    /// The complete typed request; malformed or incomplete groups are already a usage refusal.
    pub(crate) fn supervisor_request(
        &self,
    ) -> Option<crate::session_timeline::SupervisorInputRequest> {
        Some(crate::session_timeline::SupervisorInputRequest {
            holder: self.if_supervisor.clone()?,
            revision: self.supervisor_revision?,
            duty: self.harness_duty?,
        })
    }
}

fn take_supervisor_option(opts: &mut LeadingInputOptions, head: &str) -> bool {
    if let Some(value) = head.strip_prefix("if-supervisor=") {
        take_if_supervisor(opts, value);
    } else if let Some(value) = head.strip_prefix("if-supervisor-revision=") {
        let revision = (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse::<u64>().ok())
            .flatten();
        match revision {
            Some(revision) if opts.supervisor_revision.replace(revision).is_none() => {}
            _ => opts.refusal = Some(SUPERVISOR_INPUT_USAGE.to_string()),
        }
    } else if let Some(value) = head.strip_prefix("harness-duty=") {
        match aterm_agent::harness::contract::Scope::from_word(value) {
            Some(duty) if opts.harness_duty.replace(duty).is_none() => {}
            _ => opts.refusal = Some(SUPERVISOR_INPUT_USAGE.to_string()),
        }
    } else {
        return false;
    }
    true
}

fn finish_supervisor_options(opts: &mut LeadingInputOptions) {
    if (opts.supervisor_revision.is_some() || opts.harness_duty.is_some())
        && opts.supervisor_request().is_none()
    {
        opts.refusal = Some(SUPERVISOR_INPUT_USAGE.to_string());
    }
}

fn take_if_supervisor(opts: &mut LeadingInputOptions, value: &str) {
    if !crate::session_timeline::valid_owner_token(value) || value == "-" {
        opts.refusal = Some(IF_SUPERVISOR_USAGE.to_string());
    } else if opts.if_supervisor.replace(value.to_string()).is_some() {
        opts.refusal = Some("ERR usage: if-supervisor= given twice\n".to_string());
    }
}

/// Take the LEADING options off a keyed/guarded verb's argument tail.
pub(crate) fn take_leading_options(class: Input, rest: &str) -> (LeadingInputOptions, &str) {
    let keyed = class.contains(Input::KEYED);
    let guarded = class.contains(Input::GUARDED);
    let supervised = class.contains(Input::SUPERVISED);
    let caused = class.contains(Input::CAUSE);
    let mut opts = LeadingInputOptions::default();
    if !keyed && !guarded {
        // `paste` keeps its raw body, including extra spaces after the one separating each
        // leading authority option. `--` ends those options before a literal option-like body.
        if supervised {
            let mut body = rest;
            loop {
                let (head, tail) = body.split_once(' ').unwrap_or((body, ""));
                if head == "--"
                    && (opts.supervisor_revision.is_some() || opts.harness_duty.is_some())
                {
                    body = tail;
                    break;
                }
                if !take_supervisor_option(&mut opts, head) {
                    break;
                }
                body = tail;
            }
            finish_supervisor_options(&mut opts);
            return (opts, body);
        }
        return (opts, rest);
    }
    let mut cur = rest.trim_start();
    let mut consumed = false;
    loop {
        let (head, tail) = match cur.split_once(char::is_whitespace) {
            Some((h, t)) => (h, t.trim_start()),
            None => (cur, ""),
        };
        if head == "--" {
            cur = tail;
            consumed = true;
            break;
        }
        if keyed && let Some(value) = head.strip_prefix("id=") {
            if opts.idem.replace(value.to_string()).is_some() {
                opts.refusal = Some("ERR usage: id= given twice\n".to_string());
            }
            cur = tail;
            consumed = true;
            continue;
        }
        if guarded && let Some(pattern) = head.strip_prefix("if=") {
            if pattern.is_empty() {
                opts.refusal = Some(
                    "ERR usage: if=<re> needs a pattern — ONE token, e.g. \
                     if=Do.you.want.to.proceed\n"
                        .to_string(),
                );
            } else if opts.guard.replace(pattern.to_string()).is_some() {
                opts.refusal = Some("ERR usage: if= given twice\n".to_string());
            }
            cur = tail;
            consumed = true;
            continue;
        }
        if guarded && let Some(value) = head.strip_prefix("if-gen=") {
            match crate::control::ScreenGen::parse(value) {
                Some(generation) if opts.if_gen.replace(generation).is_some() => {
                    opts.refusal = Some("ERR usage: if-gen= given twice\n".to_string());
                }
                Some(_) => {}
                None => {
                    opts.refusal = Some(
                        "ERR usage: if-gen=<epoch>.<seq> needs the screen generation a read \
                         returned (status gen=)\n"
                            .to_string(),
                    );
                }
            }
            cur = tail;
            consumed = true;
            continue;
        }
        if guarded && head.starts_with("if-seq=") {
            opts.refusal = Some(
                "ERR usage: if-seq= is not a fence (seq= repeats when an alternate screen is \
                 re-entered); use if-gen=<status gen=>\n"
                    .to_string(),
            );
            cur = tail;
            consumed = true;
            continue;
        }
        if guarded && let Some(value) = head.strip_prefix("unread=") {
            if value != "ok" {
                opts.refusal = Some("ERR usage: unread=ok is the only value\n".to_string());
            } else if std::mem::replace(&mut opts.unread_ok, true) {
                opts.refusal = Some("ERR usage: unread= given twice\n".to_string());
            }
            cur = tail;
            consumed = true;
            continue;
        }
        if guarded && let Some(value) = head.strip_prefix("if-human=") {
            match (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
                .then(|| value.parse::<u64>().ok())
                .flatten()
            {
                Some(n) if opts.if_human.replace(n).is_some() => {
                    opts.refusal = Some("ERR usage: if-human= given twice\n".to_string());
                }
                Some(_) => {}
                None => {
                    opts.refusal = Some(
                        "ERR usage: if-human=<n> needs the person count a read returned \
                         (status human_seq=)\n"
                            .to_string(),
                    );
                }
            }
            cur = tail;
            consumed = true;
            continue;
        }
        if supervised && take_supervisor_option(&mut opts, head) {
            cur = tail;
            consumed = true;
            continue;
        }
        if guarded && let Some(value) = head.strip_prefix("act=") {
            match aterm_slog::ActWord::from_word(value) {
                Some(act) if opts.act.replace(act).is_none() => {}
                _ => opts.refusal = Some(ACT_USAGE.to_string()),
            }
            cur = tail;
            consumed = true;
            continue;
        }
        if caused && let Some(value) = head.strip_prefix("cause=") {
            match crate::session_log::CauseRef::parse(value) {
                Some(cause) if opts.cause.replace(cause).is_none() => {}
                _ => opts.refusal = Some(crate::session_log::CAUSE_USAGE.to_string()),
            }
            cur = tail;
            consumed = true;
            continue;
        }
        if guarded && head == "as=person" {
            take_as_person(&mut opts, PERSON_SEAM);
            cur = tail;
            consumed = true;
            continue;
        }
        if guarded && let Some(value) = head.strip_prefix("if-fp=") {
            match parse_fp(value) {
                Some(fp) if opts.if_fp.replace(fp).is_some() => {
                    opts.refusal = Some("ERR usage: if-fp= given twice\n".to_string());
                }
                Some(_) => {}
                None => {
                    opts.refusal = Some(
                        "ERR usage: if-fp=<hex16> needs the screen hash a read returned \
                         (status hash=)\n"
                            .to_string(),
                    );
                }
            }
            cur = tail;
            consumed = true;
            continue;
        }
        break;
    }
    finish_supervisor_options(&mut opts);
    if opts.as_person
        && (opts.idem.is_some()
            || opts.guard.is_some()
            || opts.if_gen.is_some()
            || opts.if_fp.is_some()
            || opts.if_human.is_some()
            || opts.if_supervisor.is_some()
            || opts.supervisor_revision.is_some()
            || opts.harness_duty.is_some()
            || opts.act.is_some()
            || opts.cause.is_some())
    {
        opts.refusal = Some(
            "ERR usage: as=person takes no fence, id=, act= or cause=: a person's own keys \
             are held to none\n"
                .to_string(),
        );
    }
    (opts, if consumed { cur } else { rest })
}

/// `key|send as=person <body>` (the [`PERSON_SEAM`]).
pub(crate) fn person_input(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    verb: &str,
    rest: &str,
) -> String {
    let ev = if verb == "key" {
        match parse_key(rest) {
            Some(ev) => ev,
            None => return KEY_USAGE.to_string(),
        }
    } else {
        InputEvent::KeySequence(send_bytes(rest))
    };
    input_reply_to_str(
        crate::control::control_media::call_main(proxy, |tx| Wake::Input {
            batch: vec![ev],
            mark: None,
            src: Source::Human,
            reply: Some(tx),
            session: Some(session),
            fence: None,
        })
        .map_err(|error| format!("ERR input dispatch failed: {error}\n")),
    )
}

fn parse_fp(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 16 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(value, 16).ok()
}

/// Compile a leading `if=<re>` guard outside the terminal lock, with the bounded `aterm_observe::row_matcher` `await match` uses.
pub(crate) fn compile_guard(pattern: Option<&str>) -> Result<Option<Arc<dyn RowMatch>>, String> {
    match pattern {
        None => Ok(None),
        Some(pattern) => match aterm_observe::row_matcher(pattern) {
            Ok(matcher) => Ok(Some(matcher)),
            Err(_) => Err("ERR badregex\n".to_string()),
        },
    }
}

/// Conditions checked under the terminal lock before a fenced write.
#[derive(Clone, Copy, Default)]
pub(crate) struct InputFence<'a> {
    /// `if=<re>` — some visible row must match.
    pub guard: Option<&'a dyn RowMatch>,
    /// `if-gen=<epoch>.<seq>` — the screen generation must still be this.
    pub generation: Option<crate::control::ScreenGen>,
    /// `if-fp=<hex16>` — the visible screen's FNV-1a-64 must still be this.
    pub fp: Option<u64>,
    /// `if-human=<n>` — the session's person-gesture count must still be this.
    pub human: Option<u64>,
    /// The published foreground identity the decision belongs to.
    pub foreground: Option<&'a crate::session_timeline::ForegroundStamp>,
    /// Match `guard` against the cursor's row only.
    pub guard_at_cursor: bool,
    /// The worker's initial command still targets an input-ready shell block.
    pub entering_command: bool,
    /// The writer's own standing, judged under the lock of its write after the person's (a
    /// turn's guarded Enter: the turn still holds its slot). Its loss writes nothing and
    /// answers [`GuardedInput::Skipped`]; the writer, which knows what it judged, says why.
    pub standing: Option<&'a (dyn Fn() -> bool + Sync)>,
}

impl InputFence<'_> {
    fn reads_rows(&self) -> bool {
        self.guard.is_some() || self.fp.is_some()
    }

    /// Whether any condition is armed; the dispatch routes only an armed write to the fenced arm.
    pub(crate) fn is_armed(&self) -> bool {
        self.guard.is_some()
            || self.generation.is_some()
            || self.fp.is_some()
            || self.human.is_some()
            || self.foreground.is_some()
            || self.entering_command
    }

    fn check(
        &self,
        terminal: &Terminal,
        look: &mut crate::frame_memo::Look<'_>,
    ) -> Option<GuardedInput> {
        if self
            .generation
            .is_some_and(|generation| crate::control::screen_gen(terminal) != generation)
        {
            return Some(GuardedInput::Changed);
        }
        if let Some(fp) = self.fp
            && look.text(terminal).fnv() != fp
        {
            return Some(GuardedInput::Changed);
        }
        if self.entering_command
            && !matches!(
                terminal.all_blocks().last().map(|b| b.state),
                Some(aterm_core::terminal::BlockState::EnteringCommand)
            )
        {
            return Some(GuardedInput::Skipped);
        }
        if let Some(guard) = self.guard {
            let range = if self.guard_at_cursor {
                let row = usize::from(terminal.cursor().row);
                RowRange::Span {
                    start: row,
                    end: row,
                }
            } else {
                RowRange::All
            };
            if first_matching_row(terminal, guard, range).is_none() {
                return Some(GuardedInput::Skipped);
            }
        }
        None
    }
}

/// What a guarded input attempt decided, under one hold of the terminal lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GuardedInput {
    Skipped,
    Changed,
    Person,
    /// Input queued for the program in order (a paste its writer is still writing, a key behind
    /// it) had not all gone: nothing was written and no fence past the person and the writer's
    /// standing was judged, so the same write may be tried again once it has.
    Queued,
    Pressed(Delivery),
}

/// Atomic check-and-press; a control thread with a row fence takes the lock in a whole frame ([`crate::term_lock_whole_when`]), the event loop as it stands.
pub(crate) fn input_if_fenced_observed(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: InputEvent,
    fence: &InputFence<'_>,
    on: ReadOn,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> GuardedInput {
    let mut look = ctx.frame.look_frame();
    let waits = on.waits() && fence.reads_rows();
    let terminal = term_lock_whole_when(term, on.lane(), |_| waits);
    if fence.human.is_some_and(|n| ctx.human_input.seq() != n) {
        return GuardedInput::Person;
    }
    if fence.standing.is_some_and(|stands| !stands()) {
        return GuardedInput::Skipped;
    }
    // Input queued for the program in order goes first: a write now would overtake it, and the
    // screen any fence would judge is one that input has not reached, so none is judged.
    if ctx.sink.ordered_egress_count() > 0 {
        return GuardedInput::Queued;
    }
    if fence.reads_rows() && crate::frame_unfinished_now(&terminal) {
        return GuardedInput::Changed;
    }
    if let Some(declined) = fence.check(&terminal, &mut look) {
        return declined;
    }
    let bytes = match &ev {
        InputEvent::Key {
            key,
            mods,
            base_layout,
            layout_text,
            event_type,
        } => aterm_types::keyboard::encode_key_with_text(
            key,
            *mods,
            terminal.keyboard_mode(),
            *event_type,
            *base_layout,
            *layout_text,
        ),
        InputEvent::KeySequence(bytes) => bytes.clone(),
        _ => return GuardedInput::Pressed(Delivery::BusyZero),
    };
    let vim = crate::vim_custody::capture(&terminal, ctx, true);
    let foreground = fence.foreground.map(|_| {
        ctx.timeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    });
    if let Some(expected) = fence.foreground
        && foreground.as_ref().and_then(|tl| tl.foreground_stamp()) != Some(*expected)
    {
        return GuardedInput::Changed;
    }
    crate::note_unseamed_control_input(&ev);
    let echo_write = ctx.output_echo.begin_event(&ev);
    if bytes.is_empty() {
        drop(foreground);
        echo_write.finish_delivery(Delivery::Full, None, &ctx.sink);
        return GuardedInput::Pressed(Delivery::Full);
    }
    if completion
        .as_ref()
        .is_some_and(|completion| !completion.start_supervised_inline())
    {
        return GuardedInput::Pressed(Delivery::BusyZero);
    }
    let supervised = completion
        .as_ref()
        .is_some_and(|completion| completion.is_supervised());
    let (outcome, accepted_order, prefix_receipt) = if supervised {
        let (outcome, receipt) = ctx.sink.try_write_frame_whole_with_prefix(&bytes);
        (outcome, receipt.order(), Some(receipt))
    } else {
        let (outcome, order) = ctx.sink.try_write_frame_whole_with_receipt(&bytes);
        (outcome, order, None)
    };
    if matches!(
        outcome,
        aterm_session::sink::ImmediateWrite::Full
            | aterm_session::sink::ImmediateWrite::Queued { .. }
    ) && let Some(key) = crate::control::control_vim_key(&ev)
    {
        match vim {
            Some(frame) => {
                ctx.licence.vim.key(frame, key);
            }
            None => ctx.licence.vim.invalidate(),
        }
    }
    drop(foreground);
    let mut delivery = match outcome {
        aterm_session::sink::ImmediateWrite::Full
        | aterm_session::sink::ImmediateWrite::Queued { .. } => Delivery::Full,
        aterm_session::sink::ImmediateWrite::BusyZero => Delivery::BusyZero,
        aterm_session::sink::ImmediateWrite::PartialInDoubt { accepted } => {
            Delivery::PartialInDoubt { accepted }
        }
    };
    drop(terminal);
    if let Some(receipt) = prefix_receipt
        && receipt.order().is_some()
        && !receipt
            .kernel_prefix()
            .is_some_and(|prefix| ctx.sink.wait_prefix_to_kernel(prefix))
    {
        delivery = match outcome {
            aterm_session::sink::ImmediateWrite::Queued { accepted }
            | aterm_session::sink::ImmediateWrite::PartialInDoubt { accepted } => {
                Delivery::PartialInDoubt { accepted }
            }
            _ => Delivery::Failed,
        };
    }
    echo_write.finish_delivery(delivery, accepted_order, &ctx.sink);
    if let Some(completion) = completion {
        completion.finish(crate::input::EgressReceipt::from_reported_delivery(
            delivery,
            accepted_order,
        ));
    }
    GuardedInput::Pressed(delivery)
}

/// The verb reply for a [`GuardedInput`].
pub(crate) fn guarded_input_reply(decision: GuardedInput) -> String {
    match decision {
        GuardedInput::Skipped => "OK skipped\n".to_string(),
        GuardedInput::Changed => "OK skipped reason=changed\n".to_string(),
        GuardedInput::Person => "OK skipped reason=person\n".to_string(),
        GuardedInput::Pressed(Delivery::Full) => "OK\n".to_string(),
        GuardedInput::Queued | GuardedInput::Pressed(Delivery::BusyZero) => {
            "ERR busy sink\n".to_string()
        }
        GuardedInput::Pressed(Delivery::PartialInDoubt { accepted }) => {
            format!("ERR write failed partial accepted={accepted}\n")
        }
        GuardedInput::Pressed(Delivery::Failed) => "ERR write failed\n".to_string(),
    }
}

/// `key if=<re> <name>`: the `key` verb's guarded twin.
#[cfg(all(test, unix))]
pub(crate) fn cmd_key_guarded(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    guard: &dyn RowMatch,
    rest: &str,
) -> String {
    cmd_key_fenced(
        term,
        ctx,
        &InputFence {
            guard: Some(guard),
            ..InputFence::default()
        },
        rest,
    )
}

/// `key [if=<re>] [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] <name>`: the fenced press.
#[cfg(all(test, unix))]
pub(crate) fn cmd_key_fenced(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    fence: &InputFence<'_>,
    rest: &str,
) -> String {
    cmd_key_fenced_observed(term, ctx, fence, rest, None)
}

pub(crate) fn cmd_key_fenced_observed(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    fence: &InputFence<'_>,
    rest: &str,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> String {
    match parse_key(rest) {
        Some(ev) => guarded_input_reply(input_if_fenced_observed(
            term,
            ctx,
            ev,
            fence,
            ReadOn::Control,
            completion,
        )),
        None => KEY_USAGE.to_string(),
    }
}

/// `send if=<re> <text>`: the guarded `send`.
#[cfg(all(test, unix))]
pub(crate) fn cmd_send_guarded(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    guard: &dyn RowMatch,
    rest: &str,
) -> String {
    cmd_send_fenced(
        term,
        ctx,
        &InputFence {
            guard: Some(guard),
            ..InputFence::default()
        },
        rest,
    )
}

/// `send [if=<re>] [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] <text>`: the fenced raw write.
#[cfg(all(test, unix))]
pub(crate) fn cmd_send_fenced(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    fence: &InputFence<'_>,
    rest: &str,
) -> String {
    cmd_send_fenced_observed(term, ctx, fence, rest, None)
}

pub(crate) fn cmd_send_fenced_observed(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    fence: &InputFence<'_>,
    rest: &str,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> String {
    let ev = InputEvent::KeySequence(send_bytes(rest));
    guarded_input_reply(input_if_fenced_observed(
        term,
        ctx,
        ev,
        fence,
        ReadOn::Control,
        completion,
    ))
}

/// `hwkey <char|name> [mods=…] [count=…] [interval=…]` -> inject the key through the OS event queue instead of the control seam.
pub(crate) fn cmd_hwkey(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    #[cfg(target_os = "macos")]
    if crate::askpass_sheet::sheets_open() {
        return "ERR hwkey: a sudo password sheet is up\n".to_string();
    }
    if crate::alert_keys::confirmation_up() {
        return crate::alert_keys::CONFIRMATION_UP_REFUSAL.to_string();
    }
    let spec = match crate::hwkey::parse_hwkey(rest) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let (window_number, key) = match crate::control::control_media::call_main(proxy, |reply| {
        Wake::HwKeyTarget { reply }
    }) {
        Ok(Ok(target)) => target,
        Ok(Err(refusal)) => return format!("ERR hwkey: {refusal}\n"),
        Err(e) => return format!("ERR hwkey: {e}\n"),
    };
    match crate::hwkey::post(&spec, window_number) {
        Ok(n) => format!("OK posted={n} key={}\n", u8::from(key)),
        Err(e) => e,
    }
}

/// A write refused at the session's input cap (the sink's spill, or its ordered writer's queue) with nothing written ([`InputOutcome::QueueFull`]), on the front tab and in the background alike.
pub(super) const INPUT_QUEUE_FULL: &str =
    "ERR busy input-queue-full: nothing written (the program has not read its queued input)\n";

/// Map a reply-bearing input outcome to a verb reply line; an `Err` (event loop closed, no window) is already a full `ERR …\n` line.
pub(super) fn input_reply_to_str(r: Result<InputOutcome, String>) -> String {
    match r {
        Ok(InputOutcome::Ok) => "OK\n".to_string(),
        Ok(InputOutcome::RangeRejected) => "ERR out of range\n".to_string(),
        Ok(InputOutcome::WriteFailed) => "ERR write failed\n".to_string(),
        Ok(InputOutcome::QueueFull) => INPUT_QUEUE_FULL.to_string(),
        Ok(InputOutcome::NotApplied(why)) => format!("ERR {why}\n"),
        Ok(InputOutcome::Resized(resize)) => resize.reply(),
        Err(e) => e,
    }
}

/// [`input_reply_to_str`] for a verb whose `OK` counts the bytes it wrote (`paste`, `feed`, `paste-bin`, `feed-bin`): `OK <n> bytes`.
pub(super) fn bytes_reply(r: Result<InputOutcome, String>, n: usize) -> String {
    match r {
        Ok(InputOutcome::Ok) => format!("OK {n} bytes\n"),
        other => input_reply_to_str(other),
    }
}

/// PURE parser for `ctrl <letter>` -> a Control-modified character key, the event the `ctrl` verb delivers by its route, so it is unit-testable headless.
pub(crate) fn parse_ctrl(rest: &str) -> Option<InputEvent> {
    use aterm_types::keyboard::{Key, Modifiers};
    let mut chars = rest.trim().chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    Some(InputEvent::Key {
        key: Key::Character(c.to_ascii_lowercase()),
        mods: Modifiers::CTRL,
        base_layout: None,
        layout_text: None,
        event_type: aterm_types::keyboard::KeyEventType::Press,
    })
}

/// The usage line every malformed `ctrl` body answers, whatever its route.
pub(crate) const CTRL_USAGE: &str = "ERR usage: ctrl <single-letter>\n";

/// `feed <hex>` -> write raw bytes (decoded from a hex string, whitespace allowed) straight to the PTY.
pub(crate) fn feed_bytes(rest: &str) -> Result<Vec<u8>, &'static str> {
    let hex: String = rest.chars().filter(|c| !c.is_whitespace()).collect();
    if !hex.len().is_multiple_of(2) {
        return Err("ERR bad hex (odd length)\n");
    }
    aterm_codec::hex::decode_vec(hex.as_bytes()).ok_or("ERR bad hex\n")
}

/// `signal <name> [pid=<n>] [quiet=<s>]` -> deliver a job-control signal to the PTY's foreground process group (`tcgetpgrp` on the master, then `killpg`), or with `pid=` (which must be that group's id) to its leader alone (`kill`), whatever the line discipline would generate. A signal that ends a program (`term`, `kill`, `int`, `quit`, `hup`; never `cont` or `tstp`) is `asker`'s ask: past every refusal and just before the signal it writes `act word=signal by=<asker>` (with `from=<program>` when a control connection asked) and the ask ([`crate::session_log::EndAsker::ask`]), taken back when the signal is not delivered.
#[cfg(unix)]
pub(crate) fn cmd_signal(
    master: i32,
    rest: &str,
    sink: &SinkWriter,
    human_ms: Option<u64>,
    asker: Option<crate::session_log::EndAsker<'_>>,
) -> String {
    let (sig, pid, quiet) = match parse_signal(rest) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let pgrp = unsafe { libc::tcgetpgrp(master) };
    if pgrp <= 0 {
        return "ERR no foreground process group\n".to_string();
    }
    if let Some(pid) = pid
        && pid != pgrp
    {
        return format!("ERR pid {pid} is not the foreground process group ({pgrp})\n");
    }
    if let Some(grace_s) = quiet {
        if let Some(ms) = human_ms.filter(|ms| *ms < u64::from(grace_s).saturating_mul(1000)) {
            return format!("ERR busy person human_ms={ms}\n");
        }
        if let Some(unread) = sink.input_backlog().map(|b| b.unread()).filter(|n| *n > 0) {
            return format!("ERR busy input-unread bytes={unread}\n");
        }
    }
    let discarded = crate::input_stall::discard_before_signal(master, sink, sig);
    let tail = discarded.map_or_else(String::new, |n| format!(" discarded={n}"));
    let asker = ask_signal_end(asker, sig, pgrp);
    let rc = match pid {
        // SAFETY: `kill` on a pid this terminal's own foreground group is led by, re-read just above.
        Some(pid) => unsafe { libc::kill(pid, sig) },
        // SAFETY: `killpg` on the terminal's own foreground group, read from the master just above; it takes no pointers.
        None => unsafe { libc::killpg(pgrp, sig) },
    };
    if let Some(asker) = asker {
        let _ = asker.delivered(pgrp, rc);
    }
    let reply = match (pid, rc) {
        (Some(pid), 0) => format!("OK signalled pid {pid}{tail}\n"),
        (Some(_), _) => format!("ERR kill failed{tail}\n"),
        (None, 0) => format!("OK signalled pgrp {pgrp}{tail}\n"),
        (None, _) => format!("ERR killpg failed{tail}\n"),
    };
    crate::input_stall::after_signal(sink, sig);
    reply
}

/// Whether signal `sig`'s default action ends the process: `term`, `kill`, `int`, `quit` and
/// `hup` do; `cont` resumes a job and `tstp` stops one, so neither asks for an end.
#[cfg(unix)]
const fn ends_a_program(sig: i32) -> bool {
    matches!(
        sig,
        libc::SIGTERM | libc::SIGKILL | libc::SIGINT | libc::SIGQUIT | libc::SIGHUP
    )
}

/// THE ASK A SIGNAL MAKES: a signal `sig` to group `pgrp` that ends a program
/// ([`ends_a_program`]) writes `asker`'s act mark and its ask ([`crate::session_log::EndAsker::ask`])
/// and hands the asker back, so a signal that fails takes it back; any other asks nothing. An
/// `int` asks as an interrupt ([`crate::session_log::AskWord::Interrupt`]: Claude Code's Ctrl-C
/// stops a turn, so the end it brings is asked but owns no agent's exit); every other is a stop
/// ([`crate::session_log::AskWord::Signal`]).
#[cfg(unix)]
pub(crate) fn ask_signal_end(
    asker: Option<crate::session_log::EndAsker<'_>>,
    sig: i32,
    pgrp: i32,
) -> Option<crate::session_log::EndAsker<'_>> {
    use crate::session_log::AskWord;
    let asker = asker.filter(|_| ends_a_program(sig))?;
    asker.ask(
        pgrp,
        if sig == libc::SIGINT {
            AskWord::Interrupt
        } else {
            AskWord::Signal
        },
    );
    Some(asker)
}

/// The `signal` verb on `by`'s ask, as aterm's own callers run it (the Overview's Stop, the
/// credit claim's end): no control connection asks, so no program is named ([`signal_verb_from`]).
pub(crate) fn signal_verb(
    master: i32,
    rest: &str,
    ctx: &SessionCtx,
    by: aterm_slog::Actor,
) -> String {
    signal_verb_from(master, rest, ctx, by, None)
}

/// The `signal` verb as the control dispatch runs it ([`super::serve_session_verb`]'s arm), on
/// `by`'s ask, `from` the words naming the program whose connection asked (its typing's credit):
/// the act's `from=`, and who the host says ended an agent whose work an Autopilot carried.
pub(crate) fn signal_verb_from(
    master: i32,
    rest: &str,
    ctx: &SessionCtx,
    by: aterm_slog::Actor,
    from: Option<&str>,
) -> String {
    cmd_signal(
        master,
        rest,
        &ctx.sink,
        ctx.human_input.ms_since(crate::metrics::now_us()),
        Some(crate::session_log::EndAsker {
            timeline: &ctx.timeline,
            by,
            from,
        }),
    )
}

/// The `restart` verb as the control dispatch runs it ([`super::serve_session_verb`]'s arm), on
/// `by`'s ask.
pub(crate) fn restart_verb(
    master: i32,
    rest: &str,
    ctx: &std::sync::Arc<SessionCtx>,
    by: aterm_slog::Actor,
) -> String {
    crate::restart_act::restart(master, rest, ctx, by)
}

/// Pure parser for `signal <name> [pid=<n>] [quiet=<s>]` -> (signal number, exact pid, quiet seconds).
#[cfg(unix)]
pub(crate) fn parse_signal(rest: &str) -> Result<(i32, Option<i32>, Option<u32>), String> {
    let mut words = rest.split_whitespace();
    let name = words.next().unwrap_or("");
    let sig = match name {
        "int" | "c" | "sigint" => libc::SIGINT,
        "quit" | "sigquit" => libc::SIGQUIT,
        "tstp" | "z" | "sigtstp" => libc::SIGTSTP,
        "hup" | "sighup" => libc::SIGHUP,
        "term" | "sigterm" => libc::SIGTERM,
        "kill" | "sigkill" => libc::SIGKILL,
        "cont" | "sigcont" => libc::SIGCONT,
        _ => return Err(format!("ERR unknown signal: {}\n", rest.trim())),
    };
    let mut pid = None;
    let mut quiet = None;
    for w in words {
        if let Some(s) = w.strip_prefix("quiet=") {
            match (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u32>().ok())
                .flatten()
            {
                Some(s) if quiet.is_none() => quiet = Some(s),
                _ => return Err(format!("ERR bad signal argument: {w}\n")),
            }
            continue;
        }
        match w
            .strip_prefix("pid=")
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<i32>().ok())
        {
            Some(n) if n > 1 && pid.is_none() => pid = Some(n),
            _ => return Err(format!("ERR bad signal argument: {w}\n")),
        }
    }
    Ok((sig, pid, quiet))
}

/// Windows arm of the `signal` verb.
#[cfg(windows)]
pub(crate) fn cmd_signal(
    master: i32,
    rest: &str,
    sink: &SinkWriter,
    human_ms: Option<u64>,
    asker: Option<crate::session_log::EndAsker<'_>>,
) -> String {
    let _ = (master, rest, sink, human_ms, asker.map(|a| a.by));
    "ERR signal unsupported on this platform\n".to_string()
}

pub(crate) const MOUSE_USAGE: &str = "ERR usage: mouse <press|release|move|wheelup|wheeldown|wheelleft|wheelright> <left|middle|right|back|forward> <row> <col> [mods=..] [count=N] [side=left|right] [block=0|1] [lines=N] (move may omit the button)\n";

const MAX_WHEEL_LINES: i32 = crate::input::MAX_WHEEL_BURST;

/// Pure parser for the `mouse` verb -> an engine-neutral mouse [`InputEvent`].
pub(crate) fn parse_mouse(rest: &str) -> Result<InputEvent, String> {
    use aterm_core::selection::SelectionSide;
    use aterm_types::mouse::{ALT_MASK, CTRL_MASK, MouseButton, SHIFT_MASK, WheelDir};
    let mut action = "";
    let mut mods: u8 = 0;
    let mut click_count: u8 = 1;
    let mut side = SelectionSide::Left;
    let mut block = false;
    let mut wheel_lines: i32 = 1;
    let mut positional: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(list) = tok.strip_prefix("mods=") {
            for name in list.split(['+', ',']) {
                mods |= match name {
                    "shift" => SHIFT_MASK,
                    "alt" | "option" | "meta" => ALT_MASK,
                    "ctrl" | "control" => CTRL_MASK,
                    "" => 0,
                    other => {
                        return Err(format!(
                            "ERR bad modifier `{other}` (a mouse report carries \
                             shift|alt|meta|ctrl only)\n"
                        ));
                    }
                };
            }
        } else if let Some(c) = tok.strip_prefix("count=") {
            click_count = (!c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()))
                .then(|| c.parse::<u8>().ok())
                .flatten()
                .unwrap_or(1)
                .clamp(1, 3);
        } else if let Some(s) = tok.strip_prefix("side=") {
            side = if s == "right" {
                SelectionSide::Right
            } else {
                SelectionSide::Left
            };
        } else if let Some(b) = tok.strip_prefix("block=") {
            block = matches!(b, "1" | "true" | "yes" | "block");
        } else if let Some(l) = tok.strip_prefix("lines=") {
            let s = l.strip_prefix('-').unwrap_or(l);
            wheel_lines = (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| l.parse::<i32>().ok())
                .flatten()
                .unwrap_or(1)
                .clamp(1, MAX_WHEEL_LINES);
        } else if action.is_empty() {
            action = tok;
        } else {
            positional.push(tok);
        }
    }
    let parse_btn = |s: &str| -> Result<MouseButton, String> {
        match s {
            "left" => Ok(MouseButton::Left),
            "middle" => Ok(MouseButton::Middle),
            "right" => Ok(MouseButton::Right),
            "back" => Ok(MouseButton::Back),
            "forward" => Ok(MouseButton::Forward),
            _ => Err("ERR bad button\n".to_string()),
        }
    };
    let parse_rc = |r: &str, c: &str| -> Result<(u16, u16), String> {
        let uint = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u16>().ok())
                .flatten()
        };
        match (uint(r), uint(c)) {
            (Some(r), Some(c)) => Ok((r, c)),
            _ => Err("ERR bad args\n".to_string()),
        }
    };
    let ev = match action {
        "move" => match positional.as_slice() {
            [r, c] => {
                let (row, col) = parse_rc(r, c)?;
                InputEvent::MouseMove {
                    buttons: 3,
                    row,
                    col,
                    mods,
                    side,
                    px_off: crate::input::PixelOffset::CELL_ORIGIN,
                }
            }
            [b, r, c] => {
                let button = parse_btn(b)?;
                let (row, col) = parse_rc(r, c)?;
                InputEvent::MouseMove {
                    buttons: button.code(),
                    row,
                    col,
                    mods,
                    side,
                    px_off: crate::input::PixelOffset::CELL_ORIGIN,
                }
            }
            _ => return Err(MOUSE_USAGE.to_string()),
        },
        "press" | "release" | "wheelup" | "wheeldown" | "wheelleft" | "wheelright" => {
            let [b, r, c] = positional.as_slice() else {
                return Err(MOUSE_USAGE.to_string());
            };
            let button = parse_btn(b)?;
            let (row, col) = parse_rc(r, c)?;
            match action {
                "press" => InputEvent::MouseButton {
                    button,
                    pressed: true,
                    row,
                    col,
                    mods,
                    click_count,
                    side,
                    block,
                    suppress_copy_on_select: false,
                    px_off: crate::input::PixelOffset::CELL_ORIGIN,
                },
                "release" => InputEvent::MouseButton {
                    button,
                    pressed: false,
                    row,
                    col,
                    mods,
                    click_count,
                    side,
                    block,
                    suppress_copy_on_select: false,
                    px_off: crate::input::PixelOffset::CELL_ORIGIN,
                },
                wheel => InputEvent::Wheel {
                    dir: match wheel {
                        "wheelup" => WheelDir::Up,
                        "wheelleft" => WheelDir::Left,
                        "wheelright" => WheelDir::Right,
                        _ => WheelDir::Down,
                    },
                    lines: wheel_lines,
                    detent: true,
                    row,
                    col,
                    mods,
                    px_off: crate::input::PixelOffset::CELL_ORIGIN,
                },
            }
        }
        _ => return Err("ERR bad action\n".to_string()),
    };
    Ok(ev)
}

/// `mouse <action> <button> <row> <col> …` -> post [`parse_mouse`]'s event to the seam (mouse report when tracking is on, or local selection when off).
pub(crate) fn scope_suppresses_copy_on_select(scope: super::Scope) -> bool {
    !scope.is_owner_class()
}

/// Stamp the scoped-edge copy-on-select suppression onto a mouse `InputEvent` (a no-op for Owner scope, and for non-`MouseButton` events which never trigger the copy-on-select side-effect).
pub(crate) fn apply_copy_on_select_policy(scope: super::Scope, ev: InputEvent) -> InputEvent {
    if !scope_suppresses_copy_on_select(scope) {
        return ev;
    }
    match ev {
        InputEvent::MouseButton {
            button,
            pressed,
            row,
            col,
            mods,
            click_count,
            side,
            block,
            px_off,
            ..
        } => InputEvent::MouseButton {
            button,
            pressed,
            row,
            col,
            mods,
            click_count,
            side,
            block,
            suppress_copy_on_select: true,
            px_off,
        },
        other => other,
    }
}

/// The `paste` verb's event, framed at the drain: one builder for the verb and for the fence that classifies it ([`control_press`]).
pub(crate) fn paste_event(rest: &str) -> InputEvent {
    InputEvent::Paste(paste_text(rest), crate::input::PasteFraming::AtDrain)
}

/// The `paste` verb's text: a literal trailing `\n` (backslash + n) becomes a real line break, which `format_paste` sends as CR.
pub(crate) fn paste_text(rest: &str) -> String {
    match rest.strip_suffix("\\n") {
        Some(head) => format!("{head}\n"),
        None => rest.to_string(),
    }
}

/// PURE parser for the `focus` verb's `in/out` argument.
pub(crate) fn parse_focus(rest: &str) -> Option<bool> {
    match rest.trim() {
        "in" | "1" | "true" | "focus" => Some(true),
        "out" | "0" | "false" | "blur" => Some(false),
        _ => None,
    }
}

fn parse_tab_number(rest: &str) -> Option<usize> {
    (!rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())).then_some(())?;
    rest.parse::<usize>().ok()?.checked_sub(1)
}

/// Parse `tab <arg>` into its [`TabAction`]: `new`, a one-based `<N>`, `next`/`prev`, `close [N]`, `move <from> <to>`.
pub(crate) fn parse_tab(rest: &str) -> Option<TabAction> {
    let rest = rest.trim();
    let mut it = rest.split_whitespace();
    match it.next() {
        Some("new") if it.next().is_none() => return Some(TabAction::New),
        Some("next") if it.next().is_none() => return Some(TabAction::Next),
        Some("prev") if it.next().is_none() => return Some(TabAction::Prev),
        Some("close") => {
            return match it.next() {
                None => Some(TabAction::Close(None)),
                Some(n) => {
                    let i = parse_tab_number(n)?;
                    it.next().is_none().then_some(TabAction::Close(Some(i)))
                }
            };
        }
        Some("move") => {
            let (from, to) = (it.next()?, it.next()?);
            if it.next().is_some() {
                return None;
            }
            let from = parse_tab_number(from)?;
            let to = parse_tab_number(to)?;
            return Some(TabAction::Move { from, to });
        }
        _ => {}
    }
    parse_tab_number(rest).map(TabAction::Select)
}

/// The refusal `tab <N>` / `tab close <N>` / `tab move <from> <to>` answer for an index no tab holds.
#[must_use]
pub(crate) fn no_such_tab(index: usize, count: usize) -> String {
    let number = index + 1;
    match count {
        0 => format!("no tab at index {number}: this window has no tabs"),
        1 => format!("no tab at index {number}: this window has 1 tab, index 1"),
        n => format!("no tab at index {number}: this window has {n} tabs, indexes 1..={n}"),
    }
}

pub(crate) fn tab_state_reply(active: usize, count: usize) -> String {
    let active = if count == 0 { 0 } else { active + 1 };
    format!("OK {active} {count}\n")
}

/// `tab new | <N> | next | prev | close [N] | move <from> <to>` -> drive the front window's tabs and reply `OK <active_number> <tab_count>` when the action happened, or `ERR <why>`.
pub(crate) fn cmd_tab(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    by: crate::session_store::ExitActor,
) -> String {
    let Some(action) = parse_tab(rest) else {
        return "ERR usage: tab <new|N|next|prev|close [N]|move <from> <to>>\n".to_string();
    };
    match super::control_media::call_main(proxy, |reply| Wake::TabCmd { action, by, reply }) {
        Ok(Ok((active, count))) => tab_state_reply(active, count),
        Ok(Err(why)) => format!("ERR {why}\n"),
        Err(error) => format!("ERR tab command failed: {error}\n"),
    }
}

/// `hover <on|off>` -> toggle the drag-and-drop drop-target highlight on the front window (the flag a real drag drives) on the main thread ([`Wake::SetDragHover`]), so the drop affordance can be driven and captured headlessly.
pub(crate) fn cmd_hover(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let hovering = match rest.trim() {
        "on" | "1" | "true" => true,
        "off" | "0" | "false" => false,
        _ => return "ERR usage: hover <on|off>\n".to_string(),
    };
    match super::control_media::call_main(proxy, |reply| Wake::SetDragHover { hovering, reply }) {
        Ok(true) => "OK\n".to_string(),
        Ok(false) => "ERR no window\n".to_string(),
        Err(error) => format!("ERR hover command failed: {error}\n"),
    }
}

/// Pure parser for `find <sub…>`; `Err` is the usage line the verb replies with.
pub(crate) fn parse_find(rest: &str) -> Result<crate::app_search::FindAction, String> {
    use crate::app_search::{FindAction, SearchEdit};
    let rest = rest.trim_start();
    let (head, tail) = match rest.split_once(' ') {
        Some((head, tail)) => (head, Some(tail)),
        None => (rest, None),
    };
    let bare = |action: FindAction| -> Result<FindAction, String> {
        match tail {
            None | Some("") => Ok(action),
            Some(_) => Err(format!("ERR usage: find {head}\n")),
        }
    };
    match head {
        "" | "status" => bare(FindAction::Status),
        "open" => bare(FindAction::Open),
        "next" => bare(FindAction::Step { forward: true }),
        "prev" => bare(FindAction::Step { forward: false }),
        "case" => bare(FindAction::ToggleCase),
        "regex" => bare(FindAction::ToggleRegex),
        "accept" => bare(FindAction::Accept),
        "cancel" => bare(FindAction::Cancel),
        "type" => match tail {
            Some(text) if !text.is_empty() => Ok(FindAction::Type(text.to_string())),
            _ => Err("ERR usage: find type <text>\n".to_string()),
        },
        "key" => {
            let edit = match tail.map(str::trim) {
                Some("back") => SearchEdit::DeleteBack,
                Some("delete") => SearchEdit::DeleteForward,
                Some("left") => SearchEdit::MoveCharLeft,
                Some("right") => SearchEdit::MoveCharRight,
                Some("word-left") => SearchEdit::MoveWordLeft,
                Some("word-right") => SearchEdit::MoveWordRight,
                Some("home") => SearchEdit::MoveStart,
                Some("end") => SearchEdit::MoveEnd,
                Some("kill-start") => SearchEdit::KillToStart,
                Some("kill-end") => SearchEdit::KillToEnd,
                Some("kill-word-back") => SearchEdit::DeleteWordBack,
                Some("kill-word-forward") => SearchEdit::DeleteWordForward,
                _ => {
                    return Err("ERR usage: find key \
                                <back|delete|left|right|word-left|word-right|home|end|\
                                kill-start|kill-end|kill-word-back|kill-word-forward>\n"
                        .to_string());
                }
            };
            Ok(FindAction::Edit(edit))
        }
        _ => Err(
            "ERR usage: find [open|type <text>|key <name>|next|prev|case|regex|accept|\
                  cancel|status]\n"
                .to_string(),
        ),
    }
}

/// `find <sub…>` -> drive the front window's find bar on the main thread ([`Wake::FindCmd`]; [`crate::App::find_cmd`]'s arms are the keystroke handler's own functions) and reply with the bar's state read back after the action.
pub(crate) fn cmd_find(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let action = match parse_find(rest) {
        Ok(action) => action,
        Err(usage) => return usage,
    };
    match super::control_media::call_main(proxy, |reply| Wake::FindCmd { action, reply }) {
        Ok(Some(status)) => find_status_line(&status),
        Ok(None) => "ERR no window\n".to_string(),
        Err(error) => format!("ERR find command failed: {error}\n"),
    }
}

/// Format a [`crate::app_search::FindStatus`] as the `find` reply.
pub(crate) fn find_status_line(status: &crate::app_search::FindStatus) -> String {
    if !status.open {
        return "OK open=0\n".to_string();
    }
    let bit = |b: bool| u8::from(b);
    let (row, col, len) = match status.current_match {
        Some((row, start, end)) => (
            row.to_string(),
            start.to_string(),
            (end.saturating_sub(u32::from(start)).saturating_add(1)).to_string(),
        ),
        None => ("-".to_string(), "-".to_string(), "-".to_string()),
    };
    let current = status
        .current
        .map_or_else(|| "-".to_string(), |i| (i + 1).to_string());
    format!(
        "OK open=1 query={} case={} regex={} regex_error={} matches={} current={current} \
         row={row} col={col} len={len} truncated={} stale={}\n",
        super::pct_encode(&status.query),
        bit(status.case_sensitive),
        bit(status.is_regex),
        bit(status.regex_error),
        status.matches,
        bit(status.truncated),
        bit(status.stale),
    )
}

/// Pure parser for `pane <left|right|up|down>`.
pub(crate) fn parse_pane_dir(rest: &str) -> Option<crate::pane::FocusDir> {
    match rest.trim() {
        "left" => Some(crate::pane::FocusDir::Left),
        "right" => Some(crate::pane::FocusDir::Right),
        "up" => Some(crate::pane::FocusDir::Up),
        "down" => Some(crate::pane::FocusDir::Down),
        _ => None,
    }
}

/// `pane <left|right|up|down>` -> move keyboard focus to the adjacent pane of the front window's active tab through `focus_pane_in` (the `focus_pane_*` bindings' path) and reply `OK moved=<0|1>`.
pub(crate) fn cmd_pane(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let Some(dir) = parse_pane_dir(rest) else {
        return "ERR usage: pane <left|right|up|down>\n".to_string();
    };
    match super::control_media::call_main(proxy, |reply| Wake::PaneFocus { dir, reply }) {
        Ok(Some(moved)) => format!("OK moved={}\n", u8::from(moved)),
        Ok(None) => "ERR no window\n".to_string(),
        Err(error) => format!("ERR pane command failed: {error}\n"),
    }
}

const POINTER_MODS_USAGE: &str =
    "ERR usage: pointer mods <none|link|cmd|ctrl|alt|shift joined by +>\n";

fn parse_pointer_mods(list: &str) -> Option<aterm_window::keyboard::ModifiersState> {
    use aterm_window::keyboard::ModifiersState as M;
    if list == "none" {
        return Some(M::empty());
    }
    let link = if cfg!(target_os = "macos") {
        M::SUPER
    } else {
        M::CONTROL
    };
    list.split('+').try_fold(M::empty(), |acc, word| {
        let bit = match word {
            "cmd" | "super" => M::SUPER,
            "ctrl" => M::CONTROL,
            "alt" | "option" => M::ALT,
            "shift" => M::SHIFT,
            "link" => link,
            _ => return None,
        };
        Some(acc | bit)
    })
}

pub(crate) fn parse_pointer(rest: &str) -> Result<crate::app_mouse::PointerAction, String> {
    use crate::app_mouse::PointerAction;
    let mut it = rest.split_whitespace();
    match it.next() {
        None | Some("status") => match it.next() {
            None => Ok(PointerAction::Status),
            Some(_) => Err("ERR usage: pointer status\n".to_string()),
        },
        Some("leave") => match it.next() {
            None => Ok(PointerAction::Leave),
            Some(_) => Err("ERR usage: pointer leave\n".to_string()),
        },
        Some("move") => {
            let (Some(rs), cs) = (it.next(), it.next()) else {
                return Err(POINTER_MOVE_USAGE.to_string());
            };
            if cs.is_none() {
                use crate::app_mouse::NativePointerTarget;
                if let Some(item) = rs.strip_prefix("item=") {
                    return item
                        .parse::<usize>()
                        .map(PointerAction::MoveItem)
                        .map_err(|_| POINTER_MOVE_USAGE.to_string());
                }
                if let Some(key) = rs.strip_prefix("key=").filter(|key| !key.is_empty()) {
                    return Ok(PointerAction::MoveNative(NativePointerTarget::Key(
                        key.to_string(),
                    )));
                }
                if let Some((x, y)) = rs.strip_prefix("xy=").and_then(|xy| xy.split_once(','))
                    && !x.starts_with('+')
                    && !y.starts_with('+')
                    && let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>())
                    && x.is_finite()
                    && y.is_finite()
                {
                    return Ok(PointerAction::MoveNative(NativePointerTarget::Point(x, y)));
                }
                return Err(POINTER_MOVE_USAGE.to_string());
            }
            let Some(cs) = cs else {
                return Err(POINTER_MOVE_USAGE.to_string());
            };
            if it.next().is_some() {
                return Err(POINTER_MOVE_USAGE.to_string());
            }
            let rs_digits = rs.strip_prefix('-').unwrap_or(rs);
            if rs_digits.is_empty()
                || !rs_digits.bytes().all(|b| b.is_ascii_digit())
                || cs.is_empty()
                || !cs.bytes().all(|b| b.is_ascii_digit())
            {
                return Err("ERR bad args\n".to_string());
            }
            let (Ok(row), Ok(col)) = (rs.parse::<i32>(), cs.parse::<u16>()) else {
                return Err("ERR bad args\n".to_string());
            };
            if col >= MAX_GRID_COLS {
                return Err("ERR out of range\n".to_string());
            }
            if row < 0 {
                return match u16::try_from(row.unsigned_abs()) {
                    Ok(up) => Ok(PointerAction::MoveChrome { up, col }),
                    _ => Err("ERR out of range\n".to_string()),
                };
            }
            match u16::try_from(row) {
                Ok(row) if row < MAX_GRID_ROWS => Ok(PointerAction::Move { row, col }),
                _ => Err("ERR out of range\n".to_string()),
            }
        }
        Some("click") => match it.next() {
            None => Ok(PointerAction::Click),
            Some(_) => Err("ERR usage: pointer click\n".to_string()),
        },
        Some("wheel") => parse_pointer_wheel(it),
        Some("mods") => match (it.next(), it.next()) {
            (Some(list), None) => parse_pointer_mods(list)
                .map(PointerAction::Mods)
                .ok_or_else(|| POINTER_MODS_USAGE.to_string()),
            _ => Err(POINTER_MODS_USAGE.to_string()),
        },
        Some(_) => Err(
            "ERR usage: pointer [move <r> <c>|move key=<ui-key>|move xy=<x>,<y>|move item=<n>|click|wheel dy=<n>|leave|status|mods <list>]\n"
                .to_string(),
        ),
    }
}

const POINTER_WHEEL_USAGE: &str = "ERR usage: pointer wheel [dy=<n>] [dx=<n>] (at least one nonzero) [unit=lines|pt] [key=<ui-key>|xy=<x>,<y>]\n";

const MAX_POINTER_WHEEL: f64 = 10_000.0;

fn parse_pointer_wheel<'a>(
    words: impl Iterator<Item = &'a str>,
) -> Result<crate::app_mouse::PointerAction, String> {
    use crate::app_mouse::{NativePointerTarget, PointerAction, WheelUnit};
    let usage = || POINTER_WHEEL_USAGE.to_string();
    let delta = |value: &str| {
        (!value.starts_with('+'))
            .then(|| value.parse::<f64>().ok())
            .flatten()
            .filter(|n| n.is_finite())
            .ok_or_else(usage)
            .and_then(|n| {
                if n.abs() <= MAX_POINTER_WHEEL {
                    Ok(n)
                } else {
                    Err("ERR out of range\n".to_string())
                }
            })
    };
    let (mut dx, mut dy, mut unit, mut aim) = (None, None, None, None);
    for word in words {
        let (field, value) = word.split_once('=').ok_or_else(usage)?;
        let fresh = match field {
            "dy" => dy.replace(delta(value)?).is_none(),
            "dx" => dx.replace(delta(value)?).is_none(),
            "unit" => unit
                .replace(match value {
                    "lines" => WheelUnit::Lines,
                    "pt" => WheelUnit::Points,
                    _ => return Err(usage()),
                })
                .is_none(),
            "key" if !value.is_empty() => aim
                .replace(NativePointerTarget::Key(value.to_string()))
                .is_none(),
            "xy" => {
                let (x, y) = value.split_once(',').ok_or_else(usage)?;
                if x.starts_with('+') || y.starts_with('+') {
                    return Err(usage());
                }
                let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) else {
                    return Err(usage());
                };
                if !(x.is_finite() && y.is_finite()) {
                    return Err(usage());
                }
                aim.replace(NativePointerTarget::Point(x, y)).is_none()
            }
            _ => return Err(usage()),
        };
        if !fresh {
            return Err(usage());
        }
    }
    let (dx, dy) = (dx.unwrap_or(0.0), dy.unwrap_or(0.0));
    if dx == 0.0 && dy == 0.0 {
        return Err(usage());
    }
    Ok(PointerAction::Wheel {
        dx,
        dy,
        unit: unit.unwrap_or(WheelUnit::Lines),
        aim,
    })
}

const POINTER_MOVE_USAGE: &str =
    "ERR usage: pointer move <r> <c> | move key=<ui-key> | move xy=<x>,<y> | move item=<n>\n";

/// `pointer [move <r> <c>|click|wheel dy=<n>|leave|status|mods <list>]` -> drive the front window's pointer and report its position or acknowledge dispatch when readback is unavailable.
pub(crate) fn cmd_pointer(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let action = match parse_pointer(rest) {
        Ok(action) => action,
        Err(usage) => return usage,
    };
    match super::control_media::call_main(proxy, |reply| Wake::PointerCmd { action, reply }) {
        Ok(reply) => pointer_reply_line(reply),
        Err(error) => format!("ERR pointer command failed: {error}\n"),
    }
}

fn pointer_reply_line(reply: crate::app_mouse::PointerReply) -> String {
    use crate::app_mouse::PointerAt;
    let px_field =
        |px: Option<(i32, i32)>| px.map_or_else(String::new, |(x, y)| format!(" px={x},{y}"));
    match reply {
        Ok(PointerAt::Grid {
            cell: (row, col),
            px,
        }) => format!("OK at={row},{col}{}\n", px_field(px)),
        Ok(PointerAt::Native {
            view,
            x,
            y,
            key,
            px,
        }) => format!(
            "OK at=native view={view} x={x:.1} y={y:.1} key={}{}\n",
            key.as_deref().unwrap_or("-"),
            px_field(px)
        ),
        Ok(PointerAt::Absent) => "OK at=-\n".to_string(),
        Ok(PointerAt::Dispatched { readback_error }) => format!(
            "OK dispatched=1 readback=unavailable detail={}\n",
            super::pct_encode(&readback_error)
        ),
        Err(why) => format!("ERR {why}\n"),
    }
}

/// Parse and range-check `resize <r> <c>`: the validated `(rows, cols)` or the reply line.
pub(crate) fn parse_resize(rest: &str) -> Result<(u16, u16), String> {
    let mut it = rest.split_whitespace();
    let (Some(rs), Some(cs)) = (it.next(), it.next()) else {
        return Err("ERR usage: resize <r> <c>\n".to_string());
    };
    if rs.is_empty()
        || cs.is_empty()
        || !rs.bytes().all(|b| b.is_ascii_digit())
        || !cs.bytes().all(|b| b.is_ascii_digit())
    {
        return Err("ERR bad args\n".to_string());
    }
    let (Ok(r), Ok(c)) = (rs.parse::<u16>(), cs.parse::<u16>()) else {
        return Err("ERR bad args\n".to_string());
    };
    if !(1..=MAX_GRID_ROWS).contains(&r) || !(1..=MAX_GRID_COLS).contains(&c) {
        return Err(format!(
            "ERR out of range: rows 1-{MAX_GRID_ROWS}, cols 1-{MAX_GRID_COLS}\n"
        ));
    }
    Ok((r, c))
}

/// `resize <r> <c>` -> resize the engine grid, the PTY, and the GUI (RES-1).
pub(crate) fn cmd_resize(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    // `resize px <w> <h>` resizes the window in physical pixels and the grid follows from
    // the platform's `Resized`, as an edge drag does. The cell form applies the grid first
    // and echoes the pixel size after, so the live-resize throttle sees no reflow; the px
    // form is what drives the throttle. Several back to back reproduce a drag's pressure.
    match parse_window_resize(rest) {
        Ok(event) => input_reply_to_str(post_input_reply(proxy, vec![event])),
        Err(usage) => usage,
    }
}

/// Parse either form of `resize` into the event that resizes the front window: `resize px <w> <h>`
/// ([`parse_resize_px`]), else `resize <r> <c>` ([`parse_resize`]) with the window echoed to match.
pub(crate) fn parse_window_resize(rest: &str) -> Result<InputEvent, String> {
    if let Some(event) = parse_resize_px(rest) {
        return event;
    }
    let (rows, cols) = parse_resize(rest)?;
    Ok(InputEvent::Resize {
        rows,
        cols,
        echo_to_window: true,
    })
}

/// Parse `resize px <w> <h>` (the window form) into its event, or `None` for the cell form ([`parse_resize`]).
pub(crate) fn parse_resize_px(rest: &str) -> Option<Result<InputEvent, String>> {
    let px = rest.trim().strip_prefix("px")?;
    let mut it = px.split_whitespace();
    let (Some(ws), Some(hs)) = (it.next(), it.next()) else {
        return Some(Err("ERR usage: resize px <w> <h>\n".to_string()));
    };
    if ws.is_empty()
        || hs.is_empty()
        || !ws.bytes().all(|b| b.is_ascii_digit())
        || !hs.bytes().all(|b| b.is_ascii_digit())
    {
        return Some(Err("ERR bad args\n".to_string()));
    }
    let (Ok(width), Ok(height)) = (ws.parse::<u32>(), hs.parse::<u32>()) else {
        return Some(Err("ERR bad args\n".to_string()));
    };
    Some(Ok(InputEvent::ResizeWindowPx { width, height }))
}

/// The refusal when the px form targets a session that is not the focused pane of the front window's active tab.
pub(crate) const RESIZE_PX_FRONT_ONLY: &str =
    "ERR resize px: only the focused pane of the front window's active tab\n";

/// The reply a cross-session `resize` gives for the px form on a target that is not the front window's focused pane.
pub(crate) fn cross_resize_px_refusal(rest: &str) -> Option<String> {
    parse_resize_px(rest).map(|_| RESIZE_PX_FRONT_ONLY.to_string())
}

#[cfg(test)]
#[path = "control_input_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "control_input_video_keys_ledger_tests.rs"]
mod video_keys_ledger_tests;
