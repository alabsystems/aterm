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
use crate::input::{Delivery, InputEvent, InputOutcome, ScrollIntent, Source};
use crate::{ReadOn, SessionCtx, TabAction, Wake, term_lock, term_lock_whole_when};

/// `scroll <up|down|top|bottom|prev-prompt|next-prompt|N>` -> move the viewport and answer `OK <display_offset> <scrollback_lines>`.
pub(crate) fn cmd_scroll(
    term: &Arc<Mutex<Terminal>>,
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
) -> String {
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
    match post_input_reply(proxy, vec![InputEvent::ScrollView(intent)]) {
        Ok(_) => {}
        Err(e) => return e,
    }
    let t = term_lock(term);
    let offset = t.grid().display_offset();
    let max = t.grid().scrollback_lines();
    format!("OK {offset} {max}\n")
}

/// `send <text>` -> the bytes to write to the PTY.
pub(crate) fn send_bytes(rest: &str) -> Vec<u8> {
    if let Some(head) = rest.strip_suffix("\\n") {
        let mut b = head.as_bytes().to_vec();
        b.push(0x0d);
        b
    } else if let Some(head) = rest.strip_suffix('\n') {
        let head = head.strip_suffix('\r').unwrap_or(head);
        let mut b = head.as_bytes().to_vec();
        b.push(0x0d);
        b
    } else {
        rest.as_bytes().to_vec()
    }
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
    let fence = crate::app_input::press_fence(&ev);
    if fenced_write {
        Some(fence == Some(true))
    } else {
        fence
    }
}

/// The usage line every malformed `key` body answers (the plain, guarded and both cross-session arms).
pub(crate) const KEY_USAGE: &str = "ERR usage: key [id=<epoch>:<producer>:<seq>] [if=<re>] \
                                    [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] \
                                    [if-supervisor=<holder>] [unread=ok] \
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
        // `paste`'s body is raw: only a leading `if-supervisor=<holder>` and
        // the one space after it are taken off, and nothing else.
        if supervised && let Some(after) = rest.strip_prefix("if-supervisor=") {
            let (value, body) = after.split_once(' ').unwrap_or((after, ""));
            take_if_supervisor(&mut opts, value);
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
            match value.parse::<u64>() {
                Ok(n) if opts.if_human.replace(n).is_some() => {
                    opts.refusal = Some("ERR usage: if-human= given twice\n".to_string());
                }
                Ok(_) => {}
                Err(_) => {
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
        if supervised && let Some(value) = head.strip_prefix("if-supervisor=") {
            take_if_supervisor(&mut opts, value);
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
    if opts.as_person
        && (opts.idem.is_some()
            || opts.guard.is_some()
            || opts.if_gen.is_some()
            || opts.if_fp.is_some()
            || opts.if_human.is_some()
            || opts.if_supervisor.is_some()
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
    let (outcome, accepted_order) = ctx.sink.try_write_frame_whole_with_receipt(&bytes);
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
    let delivery = match outcome {
        aterm_session::sink::ImmediateWrite::Full
        | aterm_session::sink::ImmediateWrite::Queued { .. } => Delivery::Full,
        aterm_session::sink::ImmediateWrite::BusyZero => Delivery::BusyZero,
        aterm_session::sink::ImmediateWrite::PartialInDoubt { accepted } => {
            Delivery::PartialInDoubt { accepted }
        }
    };
    echo_write.finish_delivery(delivery, accepted_order, &ctx.sink);
    drop(terminal);
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
        GuardedInput::Pressed(Delivery::BusyZero) => "ERR busy sink\n".to_string(),
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

/// `signal <name> [pid=<n>] [quiet=<s>]` -> deliver a job-control signal to the PTY's foreground process group (`tcgetpgrp` on the master, then `killpg`), or with `pid=` (which must be that group's id) to its leader alone (`kill`), whatever the line discipline would generate.
#[cfg(unix)]
pub(crate) fn cmd_signal(
    master: i32,
    rest: &str,
    sink: &SinkWriter,
    human_ms: Option<u64>,
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
    let rc = match pid {
        // SAFETY: `kill` on a pid this terminal's own foreground group is led by, re-read just above.
        Some(pid) => unsafe { libc::kill(pid, sig) },
        // SAFETY: `killpg` on the terminal's own foreground group, read from the master just above; it takes no pointers.
        None => unsafe { libc::killpg(pgrp, sig) },
    };
    let reply = match (pid, rc) {
        (Some(pid), 0) => format!("OK signalled pid {pid}{tail}\n"),
        (Some(_), _) => format!("ERR kill failed{tail}\n"),
        (None, 0) => format!("OK signalled pgrp {pgrp}{tail}\n"),
        (None, _) => format!("ERR killpg failed{tail}\n"),
    };
    crate::input_stall::after_signal(sink, sig);
    reply
}

/// The `signal` verb as the control dispatch runs it ([`super::serve_session_verb`]'s arm).
pub(crate) fn signal_verb(master: i32, rest: &str, ctx: &SessionCtx) -> String {
    cmd_signal(
        master,
        rest,
        &ctx.sink,
        ctx.human_input.ms_since(crate::metrics::now_us()),
    )
}

/// The `restart` verb as the control dispatch runs it ([`super::serve_session_verb`]'s arm).
pub(crate) fn restart_verb(master: i32, rest: &str, ctx: &std::sync::Arc<SessionCtx>) -> String {
    crate::restart_act::restart(master, rest, ctx)
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
            match s.parse::<u32>() {
                Ok(s) if quiet.is_none() => quiet = Some(s),
                _ => return Err(format!("ERR bad signal argument: {w}\n")),
            }
            continue;
        }
        match w.strip_prefix("pid=").map(str::parse::<i32>) {
            Some(Ok(n)) if n > 1 && pid.is_none() => pid = Some(n),
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
) -> String {
    let _ = (master, rest, sink, human_ms);
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
            click_count = c.parse::<u8>().unwrap_or(1).clamp(1, 3);
        } else if let Some(s) = tok.strip_prefix("side=") {
            side = if s == "right" {
                SelectionSide::Right
            } else {
                SelectionSide::Left
            };
        } else if let Some(b) = tok.strip_prefix("block=") {
            block = matches!(b, "1" | "true" | "yes" | "block");
        } else if let Some(l) = tok.strip_prefix("lines=") {
            wheel_lines = l.parse::<i32>().unwrap_or(1).clamp(1, MAX_WHEEL_LINES);
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
        match (r.parse::<u16>(), c.parse::<u16>()) {
            (Ok(r), Ok(c)) => Ok((r, c)),
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

/// Parse `tab <arg>` into its [`TabAction`]: `new`, a 0-based `<N>`, `next`/`prev`, `close [N]`, `move <from> <to>`.
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
                    let i = n.parse::<usize>().ok()?;
                    it.next().is_none().then_some(TabAction::Close(Some(i)))
                }
            };
        }
        Some("move") => {
            let (from, to) = (it.next()?, it.next()?);
            if it.next().is_some() {
                return None;
            }
            let from = from.parse::<usize>().ok()?;
            let to = to.parse::<usize>().ok()?;
            return Some(TabAction::Move { from, to });
        }
        _ => {}
    }
    rest.parse::<usize>().ok().map(TabAction::Select)
}

/// The refusal `tab <N>` / `tab close <N>` / `tab move <from> <to>` answer for an index no tab holds.
#[must_use]
pub(crate) fn no_such_tab(index: usize, count: usize) -> String {
    match count {
        0 => format!("no tab at index {index}: this window has no tabs"),
        1 => format!("no tab at index {index}: this window has 1 tab, index 0"),
        n => format!(
            "no tab at index {index}: this window has {n} tabs, indexes 0..{}",
            n - 1
        ),
    }
}

/// `tab new | <N> | next | prev | close [N] | move <from> <to>` -> drive the front window's tabs and reply `OK <active_index> <tab_count>` when the action happened, or `ERR <why>`.
pub(crate) fn cmd_tab(
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
    by: crate::session_store::ExitActor,
) -> String {
    let Some(action) = parse_tab(rest) else {
        return "ERR usage: tab <new|N|next|prev|close [N]|move <from> <to>>\n".to_string();
    };
    match super::control_media::call_main(proxy, |reply| Wake::TabCmd { action, by, reply }) {
        Ok(Ok((active, count))) => format!("OK {active} {count}\n"),
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
                if let Some(key) = rs.strip_prefix("key=").filter(|key| !key.is_empty()) {
                    return Ok(PointerAction::MoveNative(NativePointerTarget::Key(
                        key.to_string(),
                    )));
                }
                if let Some((x, y)) = rs.strip_prefix("xy=").and_then(|xy| xy.split_once(','))
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
            "ERR usage: pointer [move <r> <c>|move key=<ui-key>|move xy=<x>,<y>|click|wheel dy=<n>|leave|status|mods <list>]\n"
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
        value
            .parse::<f64>()
            .ok()
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
    "ERR usage: pointer move <r> <c> | move key=<ui-key> | move xy=<x>,<y>\n";

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
mod tests {
    use super::*;
    use aterm_types::control_verbs::input_of;

    #[cfg(unix)]
    #[test]
    fn signal_parses_the_bare_form_and_the_exact_pid_form() {
        assert_eq!(parse_signal("int"), Ok((libc::SIGINT, None, None)));
        assert_eq!(parse_signal(" term "), Ok((libc::SIGTERM, None, None)));
        assert_eq!(
            parse_signal("int pid=52489"),
            Ok((libc::SIGINT, Some(52489), None))
        );
        assert_eq!(
            parse_signal("term pid=52489 quiet=120"),
            Ok((libc::SIGTERM, Some(52489), Some(120)))
        );
        assert_eq!(
            parse_signal("term quiet=0"),
            Ok((libc::SIGTERM, None, Some(0)))
        );
        assert_eq!(
            parse_signal("nope pid=5"),
            Err("ERR unknown signal: nope pid=5\n".to_string())
        );
        for bad in [
            "int pid=",
            "int pid=x",
            "int pid=1",
            "int pid=0",
            "int pid=-3",
            "int 5",
            "int pid=5 pid=6",
            "term quiet=",
            "term quiet=-1",
            "term quiet=5 quiet=6",
        ] {
            assert!(parse_signal(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_cross_session_px_resize_is_refused_by_name_and_the_cell_form_is_not() {
        for px in [
            "px 1400 900",
            "  px  1400   900  ",
            "px 1400",
            "px wide tall",
        ] {
            assert_eq!(
                cross_resize_px_refusal(px).as_deref(),
                Some(RESIZE_PX_FRONT_ONLY),
                "{px:?}: every px spelling is refused by target, before its arguments"
            );
        }
        assert_ne!(
            RESIZE_PX_FRONT_ONLY, "ERR bad args\n",
            "the refusal names the target rule, not the cell parser's verdict"
        );
        assert!(RESIZE_PX_FRONT_ONLY.ends_with('\n') && RESIZE_PX_FRONT_ONLY.starts_with("ERR "));
        for cell in ["24 80", "x y", ""] {
            assert_eq!(
                cross_resize_px_refusal(cell),
                None,
                "{cell:?}: the cell form keeps its own parser and its own replies"
            );
        }
    }

    #[test]
    fn the_control_fence_is_the_seam_s_fence_of_the_parsed_body() {
        let key = |body: &str| control_fence(input_of("key"), "key", body, false);
        for body in [
            "enter",
            "tab",
            "shift+tab",
            "alt+tab",
            "a",
            "space",
            "ctrl+v",
        ] {
            assert_eq!(key(body), None, "`key {body}` arms its own licence class");
        }
        for body in [
            "shiftleft",
            "altright",
            "left type=release",
            "a type=release",
        ] {
            assert_eq!(
                key(body),
                None,
                "`key {body}` is fenced no more than a physical one"
            );
        }
        for body in [
            "backspace",
            "left",
            "right",
            "up",
            "down",
            "home",
            "end",
            "delete",
            "ctrl+a",
            "ctrl+e",
            "ctrl+w",
            "ctrl+u",
            "ctrl+k",
            "alt+b",
            "alt+d",
            "left type=repeat",
        ] {
            assert_eq!(key(body), Some(true), "`key {body}` edits the line");
        }
        for body in [
            "ctrl+r",
            "ctrl+s",
            "esc",
            "f1",
            "f5",
            "ctrl+enter",
            "shift+enter",
            "ctrl+c",
            "alt+x",
            "super+left",
            "super+home",
            "",
            "notakey",
            "shift",
        ] {
            assert_eq!(
                key(body),
                Some(false),
                "`key {body}` takes the hand off its row"
            );
        }
        for c in ["w", "u", "a", "e", "b", "k", "r", "c", "v", "x"] {
            assert_eq!(
                control_fence(input_of("ctrl"), "ctrl", c, false),
                key(&format!("ctrl+{c}")),
                "`ctrl {c}` and `key ctrl+{c}` are one press"
            );
        }
        assert_eq!(
            control_fence(input_of("ctrl"), "ctrl", "ww", false),
            Some(false),
            "malformed"
        );
        assert_eq!(
            control_fence(input_of("paste"), "paste", "hello", false),
            Some(true)
        );
        for body in ["in", "out"] {
            assert_eq!(
                control_fence(input_of("focus"), "focus", body, false),
                Some(true)
            );
        }
        for body in [
            "press left 2 4",
            "release left 2 4",
            "move left 2 5",
            "wheelup left 2 4",
        ] {
            assert_eq!(
                control_fence(input_of("mouse"), "mouse", body, false),
                Some(true),
                "`mouse {body}` keeps the hand on its row"
            );
        }
        assert_eq!(
            control_fence(input_of("mouse"), "mouse", "move 2 5", false),
            None,
            "a hover holds no button and draws no fence"
        );
        assert_eq!(
            control_fence(input_of("key"), "key", "a", true),
            Some(false)
        );
        assert_eq!(
            control_fence(input_of("key"), "key", "enter", true),
            Some(false)
        );
        assert_eq!(
            control_fence(input_of("key"), "key", "left", true),
            Some(true)
        );
        assert_eq!(
            control_fence(input_of("send"), "send", "ls", true),
            Some(false)
        );
        for verb in [
            "send", "feed", "signal", "restart", "mouse", "focus", "turn", "resize", "scroll",
        ] {
            assert_eq!(
                control_fence(input_of(verb), verb, "", false),
                Some(false),
                "{verb}"
            );
        }
        for verb in ["text", "screen", "cursor", "image", "metrics"] {
            assert_eq!(
                control_fence(input_of(verb), verb, "left", false),
                None,
                "{verb}"
            );
        }
    }

    #[test]
    fn leading_options_compose_in_either_order_and_a_body_option_is_body() {
        let opts = |idem: Option<&str>, guard: Option<&str>| LeadingInputOptions {
            idem: idem.map(str::to_string),
            guard: guard.map(str::to_string),
            ..LeadingInputOptions::default()
        };
        assert_eq!(
            take_leading_options(input_of("send"), "id=a:b:c hello there"),
            (opts(Some("a:b:c"), None), "hello there")
        );
        assert_eq!(
            take_leading_options(input_of("send"), "hello id=a:b:c"),
            (opts(None, None), "hello id=a:b:c")
        );
        assert_eq!(
            take_leading_options(input_of("send"), "  two spaces"),
            (opts(None, None), "  two spaces")
        );
        assert_eq!(
            take_leading_options(input_of("send"), "-- if=literal"),
            (opts(None, None), "if=literal")
        );
        assert_eq!(
            take_leading_options(input_of("paste"), "if=x id=a:b:c hi"),
            (opts(None, None), "if=x id=a:b:c hi")
        );
        assert_eq!(
            take_leading_options(input_of("turn"), "id=a:b:c"),
            (opts(Some("a:b:c"), None), "")
        );
        assert_eq!(
            take_leading_options(input_of("key"), "if=Do.you.want.to.proceed 1"),
            (opts(None, Some("Do.you.want.to.proceed")), "1")
        );
        assert_eq!(
            take_leading_options(input_of("key"), "id=a:b:c if=Do.you.want 1"),
            (opts(Some("a:b:c"), Some("Do.you.want")), "1")
        );
        assert_eq!(
            take_leading_options(input_of("key"), "if=Do.you.want id=a:b:c 1"),
            (opts(Some("a:b:c"), Some("Do.you.want")), "1")
        );
        assert_eq!(
            take_leading_options(input_of("send"), "if=proceed id=a:b:c y\\n"),
            (opts(Some("a:b:c"), Some("proceed")), "y\\n")
        );
        assert_eq!(
            take_leading_options(input_of("turn"), "if=x id=a:b:c hello"),
            (opts(None, None), "if=x id=a:b:c hello")
        );
        assert_eq!(
            take_leading_options(input_of("turn"), "id=a:b:c if=x hello"),
            (opts(Some("a:b:c"), None), "if=x hello")
        );
        let (o, tail) = take_leading_options(input_of("key"), "if= 1");
        assert!(
            o.refusal
                .as_deref()
                .is_some_and(|r| r.starts_with("ERR usage: if="))
        );
        assert_eq!(tail, "1");
        let (o, _) = take_leading_options(input_of("key"), "if=a if=b 1");
        assert_eq!(o.refusal.as_deref(), Some("ERR usage: if= given twice\n"));
        let (o, _) = take_leading_options(input_of("key"), "id=a:b:c id=d:e:f 1");
        assert_eq!(o.refusal.as_deref(), Some("ERR usage: id= given twice\n"));
    }

    #[test]
    #[cfg(unix)]
    fn signal_cont_is_a_known_signal() {
        for name in ["cont", "sigcont"] {
            assert_eq!(
                cmd_signal(-1, name, &SinkWriter::new(-1), None),
                "ERR no foreground process group\n",
                "{name}"
            );
        }
        assert_eq!(
            cmd_signal(-1, "continue", &SinkWriter::new(-1), None),
            "ERR unknown signal: continue\n"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn signal_term_drops_a_frozen_programs_keys_before_the_shell_can_run_them() {
        let (ran, reply) = typeahead_after_restart(Remedy::BareKillpg, Typed::Direct);
        assert!(
            ran,
            "the control: a bare killpg lets the shell run the line"
        );
        assert_eq!(reply, None);
        let (ran, reply) = typeahead_after_restart(Remedy::Verb, Typed::Direct);
        assert!(!ran, "signal term dropped the line before the shell got it");
        let reply = reply.expect("the verb's reply");
        assert!(
            reply.starts_with("OK signalled pgrp ") && reply.ends_with(" discarded=17\n"),
            "{reply}"
        );
        let (ran, reply) = typeahead_after_restart(Remedy::VerbPid, Typed::Direct);
        assert!(!ran, "signal term pid= dropped the line too");
        let reply = reply.expect("the verb's reply");
        assert!(
            reply.starts_with("OK signalled pid ") && reply.ends_with(" discarded=17\n"),
            "{reply}"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn signal_term_drops_a_spill_behind_a_full_queue_and_the_session_still_hears() {
        let (ran, reply) = typeahead_after_restart(Remedy::BareKillpg, Typed::ThroughASpill);
        assert!(ran, "the control: the spilled line runs in the shell");
        assert_eq!(reply, None);
        let (ran, reply) = typeahead_after_restart(Remedy::Verb, Typed::ThroughASpill);
        assert!(!ran, "signal term dropped the spilled line too");
        let reply = reply.expect("the verb's reply");
        assert!(
            reply.starts_with("OK signalled pgrp ") && reply.ends_with(" discarded=1118\n"),
            "{reply}"
        );
    }

    #[cfg(target_os = "macos")]
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Remedy {
        BareKillpg,
        Verb,
        VerbPid,
    }

    #[cfg(target_os = "macos")]
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Typed {
        Direct,
        ThroughASpill,
    }

    #[cfg(target_os = "macos")]
    fn typeahead_after_restart(remedy: Remedy, typed: Typed) -> (bool, Option<String>) {
        use aterm_uds::spawnfd::InheritNothing as _;
        use std::os::fd::{FromRawFd, OwnedFd};
        use std::os::unix::process::CommandExt;
        use std::time::Duration;
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: `slave` is a fresh fd this function alone owns.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let stdio = || std::process::Stdio::from(slave.try_clone().expect("dup slave"));
        let home = aterm_tempfile::tempdir().expect("scratch home");
        let mut shell = std::process::Command::new("/bin/zsh");
        shell
            .inherit_nothing()
            .args(["-f", "-i"])
            .envs(aterm_spec::host::hermetic_env(home.path()))
            .env("TERM", "xterm")
            .stdin(stdio())
            .stdout(stdio())
            .stderr(stdio());
        for key in aterm_spec::host::SHELL_START_POINTERS {
            shell.env_remove(key);
        }
        // SAFETY: runs in the forked child before exec, after stdio is in
        // place: `setsid` and `ioctl(TIOCSCTTY)` are async-signal-safe and
        // touch nothing of the parent's.
        unsafe {
            shell.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        struct Reap(std::process::Child);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let shell = Reap(shell.spawn().expect("spawn zsh"));
        drop(slave);
        aterm_pty::set_nonblocking(master, true).expect("nonblocking master");
        let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drawer = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Acquire) {
                    let mut buf = [0u8; 4096];
                    // SAFETY: a bounded read into a live stack buffer from the master, which outlives this thread (joined before close).
                    let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
                    match usize::try_from(n) {
                        Ok(n) if n > 0 => seen.lock().expect("seen").extend_from_slice(&buf[..n]),
                        _ => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        let drawn = |needle: &str| {
            let seen = seen.lock().expect("seen");
            seen.windows(needle.len()).any(|w| w == needle.as_bytes())
        };
        let wait_for = |what: &str, done: &dyn Fn() -> bool| {
            let drawn = aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                Duration::from_millis(10),
                || done().then_some(()),
            );
            assert!(
                drawn.is_some(),
                "timed out waiting for {what}: {:?}",
                String::from_utf8_lossy(&seen.lock().expect("seen"))
            );
        };
        let type_in = |bytes: &[u8]| {
            // SAFETY: a bounded write of a live slice to the live master.
            let n = unsafe { libc::write(master, bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(usize::try_from(n).ok(), Some(bytes.len()), "write(master)");
        };

        wait_for("the prompt", &|| drawn("\x1b[?2004h"));
        type_in(
            format!(
                "(stty raw -echo; exec sleep {})\r",
                aterm_spec::wait::PARKED_SECS
            )
            .as_bytes(),
        );
        let shell_pgrp = libc::pid_t::try_from(shell.0.id()).expect("pid");
        wait_for("the job's raw mode", &|| {
            // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
            let fg = unsafe { libc::tcgetpgrp(master) };
            fg > 0
                && fg != shell_pgrp
                && aterm_pty::tty_echo(master)
                    .is_some_and(|e| !e.canonical && !e.echo && e.signals.is_none())
        });
        let sink = SinkWriter::new(master);
        sink.note_master_nonblocking(true);
        match typed {
            Typed::Direct => {
                type_in(b"echo TYPE''AHEAD\r");
                assert_eq!(aterm_pty::input_queue_len(master), Some(17));
            }
            Typed::ThroughASpill => {
                let mut paste = vec![b'#'; 1100];
                paste.extend_from_slice(b"\recho TYPE''AHEAD\r");
                assert_eq!(paste.len(), 1118);
                assert_eq!(sink.write_frame_nonparking(&paste).expect("paste"), 1118);
                wait_for("the paste parked", &|| {
                    sink.input_backlog()
                        .is_some_and(|b| b.spilled.is_some() && b.unread() == 1118)
                });
                let parked = sink.input_backlog().expect("a pty is measured");
                assert_eq!(parked.queued, 1022, "the raw queue is full: {parked:?}");
            }
        }

        // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
        let pgrp = unsafe { libc::tcgetpgrp(master) };
        assert!(pgrp > 0, "a foreground job");
        let reply = match remedy {
            Remedy::Verb => Some(cmd_signal(master, "term", &sink, None)),
            Remedy::VerbPid => {
                let unread = sink.input_backlog().expect("measured").unread();
                assert_eq!(
                    cmd_signal(master, &format!("term pid={pgrp} quiet=0"), &sink, None),
                    format!("ERR busy input-unread bytes={unread}\n")
                );
                assert_eq!(
                    cmd_signal(
                        master,
                        &format!("term pid={pgrp} quiet=120"),
                        &sink,
                        Some(5_000),
                    ),
                    "ERR busy person human_ms=5000\n"
                );
                assert_eq!(
                    cmd_signal(
                        master,
                        &format!("term pid={pgrp} quiet=1"),
                        &sink,
                        Some(5_000),
                    ),
                    format!("ERR busy input-unread bytes={unread}\n"),
                    "a person past the grace is no hold; the unread input still is"
                );
                assert_eq!(
                    sink.input_backlog().expect("measured").unread(),
                    unread,
                    "nothing dropped"
                );
                Some(cmd_signal(master, &format!("term pid={pgrp}"), &sink, None))
            }
            Remedy::BareKillpg => {
                // SAFETY: `pgrp` is this test's own job.
                assert_eq!(unsafe { libc::killpg(pgrp, libc::SIGTERM) }, 0);
                None
            }
        };
        sink.write_frame_nonparking(b"echo MARK''ALIVE\r")
            .expect("the marker");
        wait_for("the shell's marker", &|| drawn("MARKALIVE"));
        let ran = drawn("TYPEAHEAD");

        drop(shell);
        stop.store(true, std::sync::atomic::Ordering::Release);
        drawer.join().expect("the drawer");
        aterm_pty::close_fd(master);
        (ran, reply)
    }

    #[test]
    fn the_app_lane_takes_a_leading_unread_ok_on_its_gesture_verbs() {
        use super::take_app_unread_ok;
        assert_eq!(
            take_app_unread_ok(input_of("invoke"), "unread=ok Paste"),
            (true, "Paste")
        );
        assert_eq!(
            take_app_unread_ok(input_of("hwkey"), " unread=ok v mods=cmd"),
            (true, "v mods=cmd")
        );
        assert_eq!(
            take_app_unread_ok(input_of("invoke"), "Paste"),
            (false, "Paste")
        );
        assert_eq!(
            take_app_unread_ok(input_of("invoke"), "Paste unread=ok"),
            (false, "Paste unread=ok")
        );
        assert_eq!(
            take_app_unread_ok(input_of("hwkey"), "unread=yes v"),
            (false, "unread=yes v")
        );
        assert_eq!(
            take_app_unread_ok(input_of("tab"), "unread=ok close"),
            (false, "unread=ok close")
        );
    }

    #[test]
    fn take_leading_options_takes_unread_ok_on_send_and_key_only() {
        let (o, tail) = take_leading_options(input_of("key"), "unread=ok down");
        assert!(o.unread_ok);
        assert_eq!(o.refusal, None);
        assert_eq!(tail, "down");
        let (o, tail) = take_leading_options(input_of("send"), "id=a:b:c unread=ok if=x hi");
        assert!(o.unread_ok);
        assert_eq!(o.idem.as_deref(), Some("a:b:c"));
        assert_eq!(o.guard.as_deref(), Some("x"));
        assert_eq!(tail, "hi");
        let (o, tail) = take_leading_options(input_of("key"), "unread=ok unread=ok down");
        assert_eq!(
            o.refusal.as_deref(),
            Some("ERR usage: unread= given twice\n")
        );
        assert_eq!(tail, "down");
        for bad in ["yes", "", "OK", "ok1"] {
            let (o, _) = take_leading_options(input_of("key"), &format!("unread={bad} down"));
            assert_eq!(
                o.refusal.as_deref(),
                Some("ERR usage: unread=ok is the only value\n"),
                "unread={bad:?}"
            );
            assert!(!o.unread_ok, "unread={bad:?} must not lift the gate");
        }
        let (o, tail) = take_leading_options(input_of("send"), "-- unread=ok");
        assert!(!o.unread_ok);
        assert_eq!(tail, "unread=ok");
        let (o, tail) = take_leading_options(input_of("send"), "hello unread=ok");
        assert!(!o.unread_ok);
        assert_eq!(tail, "hello unread=ok");
        for verb in ["turn", "paste", "feed", "ctrl"] {
            let (o, tail) = take_leading_options(input_of(verb), "unread=ok x");
            assert!(!o.unread_ok, "{verb}");
            assert_eq!(tail, "unread=ok x", "{verb}");
        }
    }

    #[test]
    fn if_supervisor_is_the_claim_fence_on_every_typing_verb() {
        for verb in ["key", "send", "turn"] {
            let (o, tail) = take_leading_options(
                input_of(verb),
                "if-supervisor=aterm-harness@9 if-gen=3.15 1",
            );
            assert_eq!(
                o.if_supervisor.as_deref(),
                Some("aterm-harness@9"),
                "{verb}"
            );
            assert_eq!(o.refusal, None, "{verb}");
            let want = if verb == "turn" { "if-gen=3.15 1" } else { "1" };
            assert_eq!(tail, want, "{verb}");
        }
        let (o, tail) = take_leading_options(input_of("paste"), "if-supervisor=h@1  two spaces");
        assert_eq!(o.if_supervisor.as_deref(), Some("h@1"));
        assert_eq!(tail, " two spaces", "only the separating space goes");
        let (o, tail) = take_leading_options(input_of("paste"), "if=x if-supervisor=h@1 hi");
        assert_eq!((o.if_supervisor, tail), (None, "if=x if-supervisor=h@1 hi"));
        assert_eq!(
            take_leading_options(input_of("send"), "hello if-supervisor=h@1").1,
            "hello if-supervisor=h@1"
        );
        assert_eq!(
            take_leading_options(input_of("feed"), "if-supervisor=h@1 0d").1,
            "if-supervisor=h@1 0d"
        );
        for bad in [
            "if-supervisor= 1",
            "if-supervisor=- 1",
            "if-supervisor=a\u{7f} 1",
        ] {
            assert_eq!(
                take_leading_options(input_of("key"), bad)
                    .0
                    .refusal
                    .as_deref(),
                Some(IF_SUPERVISOR_USAGE),
                "{bad:?}"
            );
        }
        assert_eq!(
            take_leading_options(input_of("key"), "if-supervisor=a if-supervisor=a 1")
                .0
                .refusal
                .as_deref(),
            Some("ERR usage: if-supervisor= given twice\n")
        );
        assert!(
            KEY_USAGE.contains("[if-supervisor=<holder>]"),
            "{KEY_USAGE}"
        );
    }

    #[test]
    fn as_person_is_the_seam_on_key_and_send_and_takes_no_fence() {
        for verb in ["key", "send"] {
            let (o, tail) = take_leading_options(input_of(verb), "as=person enter");
            assert!(o.as_person, "{verb}");
            assert_eq!(o.refusal, None, "{verb}");
            assert_eq!(tail, "enter", "{verb}");
        }
        let (o, _) = take_leading_options(input_of("key"), "as=person as=person 1");
        assert_eq!(
            o.refusal.as_deref(),
            Some("ERR usage: as=person given twice\n")
        );
        for fenced in [
            "as=person if=x 1",
            "if-gen=3.15 as=person 1",
            "as=person if-fp=ab 1",
            "as=person if-human=2 1",
            "id=a:b:c as=person 1",
            "as=person act=answer 1",
            "as=person if-supervisor=h 1",
        ] {
            let (o, tail) = take_leading_options(input_of("key"), fenced);
            assert!(
                o.refusal
                    .as_deref()
                    .is_some_and(|r| r.starts_with("ERR usage: as=person takes no fence")),
                "{fenced:?} must be refused: {:?}",
                o.refusal
            );
            assert_eq!(tail, "1", "{fenced:?}");
        }
        let (o, _) = take_leading_options(input_of("send"), "as=person cause=@1 hi");
        assert!(o.refusal.is_some(), "a person's write names no cause");
        let mut off = LeadingInputOptions::default();
        take_as_person(&mut off, false);
        assert!(!off.as_person);
        assert_eq!(off.refusal.as_deref(), Some(PERSON_SEAM_REFUSED));
        let (o, tail) = take_leading_options(input_of("send"), "as=persona hi");
        assert!(!o.as_person);
        assert_eq!(o.refusal, None);
        assert_eq!(tail, "as=persona hi");
        assert_eq!(
            take_leading_options(input_of("paste"), "as=person x").1,
            "as=person x"
        );
        assert_eq!(
            take_leading_options(input_of("send"), "hello as=person").1,
            "hello as=person"
        );
        assert!(
            !take_leading_options(input_of("send"), "hello as=person")
                .0
                .as_person
        );
    }

    #[test]
    fn if_human_is_the_person_fence_on_key_and_send() {
        for verb in ["key", "send"] {
            let (o, tail) = take_leading_options(input_of(verb), "if-human=7 if-gen=3.15 1");
            assert_eq!(o.if_human, Some(7), "{verb}");
            assert_eq!(o.refusal, None, "{verb}");
            assert_eq!(tail, "1", "{verb}");
        }
        for bad in ["", "-1", "x", "1.5", "0x7"] {
            let line = format!("if-human={bad} 1");
            let (o, tail) = take_leading_options(input_of("key"), &line);
            assert!(
                o.refusal
                    .as_deref()
                    .is_some_and(|r| r.starts_with("ERR usage: if-human=")),
                "if-human={bad:?} must be refused"
            );
            assert_eq!(tail, "1");
        }
        let (o, _) = take_leading_options(input_of("key"), "if-human=1 if-human=1 1");
        assert_eq!(
            o.refusal.as_deref(),
            Some("ERR usage: if-human= given twice\n")
        );
        assert_eq!(
            take_leading_options(input_of("paste"), "if-human=1 x").1,
            "if-human=1 x"
        );
        assert_eq!(
            take_leading_options(input_of("send"), "hello if-human=1").1,
            "hello if-human=1"
        );
        assert!(
            InputFence {
                human: Some(1),
                ..InputFence::default()
            }
            .is_armed()
        );
        assert!(KEY_USAGE.contains("[if-human=<n>]"), "{KEY_USAGE}");
    }

    #[test]
    fn if_gen_is_the_fence_and_if_seq_is_refused_not_typed() {
        use crate::control::ScreenGen;
        let (o, tail) = take_leading_options(input_of("key"), "if-gen=3.15 if-fp=00ff 1");
        assert_eq!(o.if_gen, Some(ScreenGen { epoch: 3, seq: 15 }));
        assert_eq!(o.if_fp, Some(0xff));
        assert_eq!(o.refusal, None);
        assert_eq!(tail, "1");
        assert_eq!(ScreenGen { epoch: 3, seq: 15 }.to_string(), "3.15");
        for bad in ["15", "3.", ".15", "3.15.1", "-3.15", "3.+15", "x.1", ""] {
            let (o, _) = take_leading_options(input_of("key"), &format!("if-gen={bad} 1"));
            assert!(
                o.refusal
                    .as_deref()
                    .is_some_and(|r| r.starts_with("ERR usage: if-gen=")),
                "if-gen={bad:?} must be refused"
            );
        }
        let (o, _) = take_leading_options(input_of("key"), "if-gen=1.2 if-gen=1.2 1");
        assert_eq!(
            o.refusal.as_deref(),
            Some("ERR usage: if-gen= given twice\n")
        );
        for verb in ["key", "send"] {
            let (o, tail) = take_leading_options(input_of(verb), "if-seq=15 1");
            assert!(
                o.refusal
                    .as_deref()
                    .is_some_and(|r| r.starts_with("ERR usage: if-seq= is not a fence")),
                "{verb}: {o:?}"
            );
            assert_eq!(tail, "1", "{verb}: the option is taken, not left as body");
        }
        assert_eq!(
            take_leading_options(input_of("send"), "hello if-seq=15").1,
            "hello if-seq=15"
        );
    }

    #[test]
    fn a_guard_compiles_outside_the_lock_and_a_bad_one_is_badregex() {
        assert!(compile_guard(None).unwrap().is_none());
        assert!(compile_guard(Some("Do.you.want")).unwrap().is_some());
        assert_eq!(compile_guard(Some("(")).unwrap_err(), "ERR badregex\n");
    }

    #[test]
    fn a_find_reply_names_no_match_position_when_there_is_no_match() {
        let status = crate::app_search::FindStatus {
            open: true,
            query: "needle".to_string(),
            matches: 0,
            current: None,
            current_match: None,
            ..crate::app_search::FindStatus::CLOSED
        };
        let line = find_status_line(&status);
        assert_eq!(
            line,
            "OK open=1 query=needle case=0 regex=0 regex_error=0 matches=0 current=- \
             row=- col=- len=- truncated=0 stale=0\n"
        );
        let line = find_status_line(&crate::app_search::FindStatus {
            open: true,
            query: "needle".to_string(),
            matches: 7,
            current: Some(2),
            current_match: Some((-4, 11, 16)),
            ..crate::app_search::FindStatus::CLOSED
        });
        assert!(
            line.contains("matches=7 current=3 row=-4 col=11 len=6"),
            "{line}"
        );
    }

    #[test]
    fn a_find_reply_marks_a_count_the_terminal_has_already_moved_past() {
        let counted = crate::app_search::FindStatus {
            open: true,
            query: "needle".to_string(),
            matches: 3,
            current: Some(0),
            current_match: Some((1, 0, 5)),
            ..crate::app_search::FindStatus::CLOSED
        };
        assert!(
            find_status_line(&counted).contains("matches=3 current=1")
                && find_status_line(&counted).contains("stale=0"),
            "a fresh count is stated plainly: {}",
            find_status_line(&counted)
        );

        let moved = crate::app_search::FindStatus {
            stale: true,
            ..counted
        };
        let line = find_status_line(&moved);
        assert!(
            line.contains("matches=3 current=1"),
            "the last real census is still reported: {line}"
        );
        assert!(
            line.contains("stale=1"),
            "…and is marked as older than the screen: {line}"
        );
    }

    #[test]
    fn a_closed_find_bar_answers_only_that_it_is_closed() {
        assert_eq!(
            find_status_line(&crate::app_search::FindStatus::CLOSED),
            "OK open=0\n"
        );
    }

    #[test]
    fn find_type_carries_its_text_verbatim_including_the_spaces() {
        use crate::app_search::FindAction;
        assert_eq!(
            parse_find("type hello world"),
            Ok(FindAction::Type("hello world".to_string()))
        );
        assert_eq!(
            parse_find("type  leading"),
            Ok(FindAction::Type(" leading".to_string()))
        );
        assert_eq!(
            parse_find("type trailing "),
            Ok(FindAction::Type("trailing ".to_string()))
        );
    }

    #[test]
    fn a_bare_find_form_refuses_an_argument_it_cannot_honour() {
        for line in ["next 3", "prev 2", "open now", "cancel please", "case on"] {
            assert!(
                parse_find(line).is_err(),
                "find {line:?} must not be accepted"
            );
        }
        use crate::app_search::FindAction;
        assert_eq!(parse_find("next"), Ok(FindAction::Step { forward: true }));
        assert_eq!(parse_find("prev"), Ok(FindAction::Step { forward: false }));
        assert_eq!(parse_find("case"), Ok(FindAction::ToggleCase));
        assert_eq!(parse_find(""), Ok(FindAction::Status));
    }

    #[test]
    fn pointer_move_refuses_a_cell_it_cannot_address() {
        use crate::app_mouse::{PointerAction, PointerAt};
        assert_eq!(
            parse_pointer("move 3 9"),
            Ok(PointerAction::Move { row: 3, col: 9 })
        );
        assert_eq!(parse_pointer("leave"), Ok(PointerAction::Leave));
        assert_eq!(
            parse_pointer("move key=settings/cursor-style/comet"),
            Ok(PointerAction::MoveNative(
                crate::app_mouse::NativePointerTarget::Key("settings/cursor-style/comet".into())
            ))
        );
        assert_eq!(
            parse_pointer("move xy=120.5,64"),
            Ok(PointerAction::MoveNative(
                crate::app_mouse::NativePointerTarget::Point(120.5, 64.0)
            ))
        );
        for junk in [
            "move key=",
            "move xy=1",
            "move xy=a,b",
            "move xy=inf,2",
            "move 3",
        ] {
            assert!(parse_pointer(junk).is_err(), "{junk}");
        }
        assert_eq!(parse_pointer(""), Ok(PointerAction::Status));
        assert_eq!(
            parse_pointer(&format!("move {MAX_GRID_ROWS} 0")),
            Err("ERR out of range\n".to_string())
        );
        assert_eq!(
            parse_pointer(&format!("move 0 {MAX_GRID_COLS}")),
            Err("ERR out of range\n".to_string())
        );
        for line in [
            "move 1",
            "move 1 2 3",
            "move a b",
            "leave now",
            "wiggle",
            "mods",
            "mods hyper",
            "mods cmd+",
            "mods cmd shift",
        ] {
            assert!(parse_pointer(line).is_err(), "pointer {line:?}");
        }
        use aterm_window::keyboard::ModifiersState as M;
        assert_eq!(
            parse_pointer("mods none"),
            Ok(PointerAction::Mods(M::empty()))
        );
        assert_eq!(
            parse_pointer("mods cmd+alt"),
            Ok(PointerAction::Mods(M::SUPER | M::ALT))
        );
        let link = parse_pointer("mods link");
        assert!(
            matches!(link, Ok(PointerAction::Mods(m)) if crate::app_mouse::link_modifier_held(m)),
            "{link:?}"
        );
        assert_eq!(
            parse_pointer("move -1 5"),
            Ok(PointerAction::MoveChrome { up: 1, col: 5 })
        );
        assert_eq!(
            parse_pointer(&format!("move -{} 0", u16::MAX)),
            Ok(PointerAction::MoveChrome {
                up: u16::MAX,
                col: 0
            })
        );
        assert_eq!(
            parse_pointer(&format!("move -{} 0", u32::from(u16::MAX) + 1)),
            Err("ERR out of range\n".to_string())
        );
        assert_eq!(parse_pointer("click"), Ok(PointerAction::Click));
        assert!(parse_pointer("click right").is_err());
        for (reply, expected) in [
            (
                Ok(PointerAt::Grid {
                    cell: (3, 9),
                    px: Some((100, 120)),
                }),
                "OK at=3,9 px=100,120\n",
            ),
            (
                Ok(PointerAt::Grid {
                    cell: (-1, 5),
                    px: None,
                }),
                "OK at=-1,5\n",
            ),
            (
                Ok(PointerAt::Native {
                    view: 2,
                    x: 120.5,
                    y: 64.0,
                    key: Some("editor/buffer".to_string()),
                    px: Some((241, 128)),
                }),
                "OK at=native view=2 x=120.5 y=64.0 key=editor/buffer px=241,128\n",
            ),
            (
                Ok(PointerAt::Native {
                    view: 2,
                    x: 0.0,
                    y: 1.0,
                    key: None,
                    px: None,
                }),
                "OK at=native view=2 x=0.0 y=1.0 key=-\n",
            ),
            (Ok(PointerAt::Absent), "OK at=-\n"),
            (Err("refused".to_string()), "ERR refused\n"),
        ] {
            assert_eq!(pointer_reply_line(reply), expected);
        }
        let detail = "frame\n\t100% λ";
        let line = pointer_reply_line(Ok(PointerAt::Dispatched {
            readback_error: detail.to_string(),
        }));
        assert_eq!(
            line,
            "OK dispatched=1 readback=unavailable detail=frame%0A%09100%25%20%CE%BB\n"
        );
        let encoded = line
            .split_whitespace()
            .find_map(|word| word.strip_prefix("detail="))
            .unwrap();
        assert_eq!(aterm_control::wire::pct_decode(encoded), detail);
    }

    #[test]
    fn pointer_wheel_parses_its_deltas_unit_and_aim() {
        use crate::app_mouse::{NativePointerTarget, PointerAction, WheelUnit};
        let wheel = |dx: f64, dy: f64, unit, aim| PointerAction::Wheel { dx, dy, unit, aim };
        assert_eq!(
            parse_pointer("wheel dy=3"),
            Ok(wheel(0.0, 3.0, WheelUnit::Lines, None))
        );
        assert_eq!(
            parse_pointer("wheel unit=pt dx=-12.5 dy=40 key=settings/choice/Nord"),
            Ok(wheel(
                -12.5,
                40.0,
                WheelUnit::Points,
                Some(NativePointerTarget::Key("settings/choice/Nord".into()))
            ))
        );
        assert_eq!(
            parse_pointer("wheel xy=300,120 dy=-0.5 unit=lines"),
            Ok(wheel(
                0.0,
                -0.5,
                WheelUnit::Lines,
                Some(NativePointerTarget::Point(300.0, 120.0))
            ))
        );
        assert_eq!(
            parse_pointer(&format!("wheel dy={MAX_POINTER_WHEEL}")),
            Ok(wheel(0.0, MAX_POINTER_WHEEL, WheelUnit::Lines, None))
        );
        assert_eq!(
            parse_pointer("wheel dy=10001"),
            Err("ERR out of range\n".to_string())
        );
        for junk in [
            "wheel",
            "wheel dy=0",
            "wheel dy=0 dx=0",
            "wheel 3",
            "wheel dy=3 dy=4",
            "wheel dy=nan",
            "wheel dy=inf",
            "wheel dy=x",
            "wheel dy=3 unit=px",
            "wheel dy=3 unit=pt unit=lines",
            "wheel dy=3 key=",
            "wheel dy=3 key=a xy=1,2",
            "wheel dy=3 xy=1",
            "wheel dy=3 xy=1,inf",
            "wheel dy=3 down",
        ] {
            assert_eq!(
                parse_pointer(junk),
                Err(POINTER_WHEEL_USAGE.to_string()),
                "pointer {junk:?}"
            );
        }
    }

    #[test]
    fn parse_mouse_wheel_lines() {
        let Ok(InputEvent::Wheel { lines, dir, .. }) = parse_mouse("wheeldown left 2 4 lines=8")
        else {
            panic!("wheeldown lines=8 parses");
        };
        assert_eq!(lines, 8);
        assert_eq!(dir, aterm_types::mouse::WheelDir::Down);
        let Ok(InputEvent::Wheel { lines, .. }) = parse_mouse("wheelup left 2 4") else {
            panic!("bare wheelup parses");
        };
        assert_eq!(lines, 1);
        let Ok(InputEvent::Wheel { lines, .. }) = parse_mouse("wheelup left 0 0 lines=100000")
        else {
            panic!("over-cap parses");
        };
        assert_eq!(lines, MAX_WHEEL_LINES);
        for line in ["wheelup left 0 0 lines=0", "wheelup left 0 0 lines=abc"] {
            let Ok(InputEvent::Wheel { lines, .. }) = parse_mouse(line) else {
                panic!("{line} parses");
            };
            assert_eq!(lines, 1, "{line}");
        }
    }

    #[test]
    fn parse_mouse_covers_every_wheel_direction_and_button() {
        use aterm_types::mouse::{MouseButton, WheelDir};
        for (line, want) in [
            ("wheelup left 2 4", WheelDir::Up),
            ("wheeldown left 2 4", WheelDir::Down),
            ("wheelleft left 2 4", WheelDir::Left),
            ("wheelright left 2 4", WheelDir::Right),
        ] {
            let Ok(InputEvent::Wheel { dir, .. }) = parse_mouse(line) else {
                panic!("{line} parses");
            };
            assert_eq!(dir, want, "{line}");
        }
        for (line, want) in [
            ("press back 2 4", MouseButton::Back),
            ("press forward 2 4", MouseButton::Forward),
        ] {
            let Ok(InputEvent::MouseButton { button, .. }) = parse_mouse(line) else {
                panic!("{line} parses");
            };
            assert_eq!(button, want, "{line}");
        }
        assert!(parse_mouse("press thumb3 2 4").is_err());
    }
}

#[cfg(test)]
mod video_keys_ledger_tests {

    use super::{feed_bytes, parse_key, send_bytes};
    use crate::input::InputEvent;
    use crate::{VideoInputSample, record_controller_video_attempts, video_sample_for_key};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    #[test]
    fn video_keys_ledger_logs_the_send_and_feed_verbs() {
        let bytes = send_bytes("ls\\n");
        assert_eq!(bytes, b"ls\r".to_vec(), "the verb's bytes, not a stand-in");
        let mut log = Vec::new();
        let _ =
            record_controller_video_attempts(&mut log, &InputEvent::KeySequence(bytes), 100, 64);
        assert_eq!(
            log,
            vec![
                (100, VideoInputSample::Char('l')),
                (100, VideoInputSample::Char('s')),
                (100, VideoInputSample::Char('\r')),
            ],
            "`send ls\\n` must land three timestamped attempts, not silence"
        );

        let bytes = feed_bytes("1b5b41").expect("valid hex");
        let mut fed = Vec::new();
        let _ =
            record_controller_video_attempts(&mut fed, &InputEvent::KeySequence(bytes), 200, 64);
        assert_eq!(
            fed,
            vec![
                (200, VideoInputSample::Char('\u{1b}')),
                (200, VideoInputSample::Char('[')),
                (200, VideoInputSample::Char('A')),
            ],
            "`feed` bytes are attempts on the frame clock too"
        );

        let raw = [b'a', 0xff, 0xfe, b'b'];
        let mut lossy = Vec::new();
        let _ = record_controller_video_attempts(
            &mut lossy,
            &InputEvent::KeySequence(raw.to_vec()),
            300,
            64,
        );
        assert_eq!(
            lossy,
            vec![
                (300, VideoInputSample::Char('a')),
                (300, VideoInputSample::Char('\u{fffd}')),
                (300, VideoInputSample::Char('\u{fffd}')),
                (300, VideoInputSample::Char('b')),
            ]
        );
        assert_eq!(
            lossy.len(),
            String::from_utf8_lossy(&raw).chars().count(),
            "the in-place decoder agrees with from_utf8_lossy's char count"
        );

        let mut capped = Vec::new();
        let dropped = record_controller_video_attempts(
            &mut capped,
            &InputEvent::KeySequence(b"abcdefgh".to_vec()),
            400,
            3,
        );
        assert_eq!(capped.len(), 3, "the bounded ledger is still bounded");
        assert_eq!(dropped, 5, "and counts the five attempts it could not take");
    }

    #[test]
    fn video_keys_ledger_names_keys_that_have_no_character() {
        for (verb, expect) in [
            ("up", VideoInputSample::Named(NamedKey::ArrowUp)),
            ("esc", VideoInputSample::Named(NamedKey::Escape)),
            ("backspace", VideoInputSample::Named(NamedKey::Backspace)),
            ("f1", VideoInputSample::Named(NamedKey::F1)),
            ("pagedown", VideoInputSample::Named(NamedKey::PageDown)),
            ("enter", VideoInputSample::Char('\n')),
            ("tab", VideoInputSample::Char('\t')),
            ("space", VideoInputSample::Char(' ')),
        ] {
            let ev = parse_key(verb).unwrap_or_else(|| panic!("`key {verb}` parses"));
            let mut log = Vec::new();
            let _ = record_controller_video_attempts(&mut log, &ev, 7, 64);
            assert_eq!(log, vec![(7, expect)], "`key {verb}` must be an attempt");
        }

        assert_eq!(
            video_sample_for_key(&Key::Named(NamedKey::ShiftLeft)),
            None,
            "a bare Shift expresses no input intent"
        );
        assert_eq!(video_sample_for_key(&Key::Named(NamedKey::CapsLock)), None);

        assert_eq!(
            VideoInputSample::Named(NamedKey::ArrowUp).json_field(),
            "\"key\":\"ArrowUp\""
        );
        assert_eq!(VideoInputSample::Char('a').json_field(), "\"ch\":\"a\"");
        assert_eq!(
            VideoInputSample::Char('\r').json_field(),
            "\"ch\":\"\\u000d\""
        );
        assert_eq!(
            VideoInputSample::Char('"').json_field(),
            "\"ch\":\"\\\"\"",
            "a typed quote cannot break the index"
        );
    }

    #[test]
    fn unloggable_counter_ignores_events_the_ledger_never_records() {
        for quiet in [
            InputEvent::Focus(true),
            InputEvent::Focus(false),
            InputEvent::Key {
                key: Key::Named(NamedKey::ShiftLeft),
                mods: Modifiers::default(),
                base_layout: None,
                layout_text: None,
                event_type: KeyEventType::Press,
            },
        ] {
            assert_eq!(
                crate::video_attempt_count(&quiet),
                0,
                "{quiet:?} is not an input attempt and must not be reported as an unlogged one"
            );
        }

        assert_eq!(
            crate::video_attempt_count(&InputEvent::Text("ab".into())),
            2,
            "a cross-session paste of two chars IS two attempts this ledger missed"
        );

        assert_eq!(
            crate::video_attempt_count(&InputEvent::KeySequence(Vec::new())),
            0
        );
        assert_eq!(
            crate::video_attempt_count(&InputEvent::KeySequence(send_bytes("hey"))),
            3
        );
    }
}
