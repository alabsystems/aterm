// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Input-injection verbs + their parsers — the control protocol's "drive the
//! shell" surface: key/ctrl/send/feed/signal/mouse/paste/focus/resize/scroll/tab.
//! The seam plumbing (`post_input_reply`) and the cross-session `apply_scroll_intent`
//! cluster live in `control.rs`; this module reaches them via `super::`.

use std::sync::{Arc, Mutex};

use aterm_core::grid::{MAX_GRID_COLS, MAX_GRID_ROWS};
use aterm_core::terminal::{RowMatch, RowRange, Terminal, first_matching_row};
use aterm_session::sink::SinkWriter;
use aterm_types::control_verbs::Input;
use aterm_window::event_loop::EventLoopProxy;

use super::post_input_reply;
use crate::input::{Delivery, InputEvent, InputOutcome, ScrollIntent, Source};
use crate::{SessionCtx, TabAction, Wake, term_lock};

/// `scroll <up|down|top|bottom|prev-prompt|next-prompt|N>` -> move the viewport and
/// answer `OK <display_offset> <scrollback_lines>`. `up`/`down` move one screen, a signed
/// `N` moves N lines into history (negative toward the bottom), and no argument just
/// reports.
pub(crate) fn cmd_scroll(
    term: &Arc<Mutex<Terminal>>,
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
) -> String {
    // The seam is the sole `scroll_display`/`scroll_to_*` caller; `""` is `By(0)`, so
    // the round trip still reports the position.
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
    // The reply comes after the seam applied the scroll on the main thread, so the read
    // below sees it. `scroll` is read-side view control (display offset only).
    match post_input_reply(proxy, vec![InputEvent::ScrollView(intent)]) {
        Ok(_) => {}
        Err(e) => return e,
    }
    let t = term_lock(term);
    let offset = t.grid().display_offset();
    let max = t.grid().scrollback_lines();
    format!("OK {offset} {max}\n")
}

/// `send <text>` -> the bytes to write to the PTY. A trailing two-character literal `\n`
/// (`aterm-ctl send 'ls\n'`) becomes one CR (`0x0d`, the Return key's byte), so the line
/// editor runs the command. A real trailing LF cannot arrive over the socket (the
/// framing consumes it); its arm serves in-process callers. Embedded newlines pass
/// through.
pub(crate) fn send_bytes(rest: &str) -> Vec<u8> {
    if let Some(head) = rest.strip_suffix("\\n") {
        let mut b = head.as_bytes().to_vec();
        b.push(0x0d);
        b
    } else if let Some(head) = rest.strip_suffix('\n') {
        // A real trailing LF (and an optional preceding CR for CRLF) → one CR.
        let head = head.strip_suffix('\r').unwrap_or(head);
        let mut b = head.as_bytes().to_vec();
        b.push(0x0d);
        b
    } else {
        rest.as_bytes().to_vec()
    }
}

/// Parse the optional trailing `mods=<list>` token (e.g. `mods=ctrl+shift`),
/// returning the modifier mask and the rest of the line with the token removed.
/// Additive: a verb line without `mods=` parses to `Modifiers::empty()` and the
/// untouched line. Unknown modifier names return `None` rather than silently
/// dropping the modifier.
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
                    // Meta and Hyper are their own bits, distinct from ALT; only the
                    // Kitty keyboard protocol encodes them (legacy encoders ignore them).
                    "meta" => Modifiers::META,
                    "hyper" => Modifiers::HYPER,
                    "super" | "cmd" | "command" => Modifiers::SUPER,
                    // A bare `mods=` or a trailing separator asks for none.
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

/// Parse the optional trailing `type=<press|repeat|release>` token (`down`/`up` alias
/// press/release; default press), returning the type and the body without it. An
/// unknown value is `None`, so [`parse_key`] rejects the line.
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

/// Parse the optional trailing `base=<char>` token: the US-QWERTY base-layout key for
/// Kitty `REPORT_ALTERNATE_KEYS` (the 3rd CSI-u sub-field), so a controller on a non-US
/// layout reproduces a human's bytes. A value that is not exactly one char is `None`.
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

/// Map a `key` verb wire token to a [`NamedKey`](aterm_types::keyboard::NamedKey),
/// or `None` if it is not a named key (the caller then tries a single character).
/// Covers the full `NamedKey` vocabulary the engine models — navigation, editing,
/// locks/system, F1–F35, numpad, modifier-side keys, and media/audio.
fn named_key_from_token(body: &str) -> Option<aterm_types::keyboard::NamedKey> {
    use aterm_types::keyboard::NamedKey as Nk;
    Some(match body {
        // --- navigation / F1..F12 ---
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
        // --- editing / system ---
        "space" => Nk::Space,
        "capslock" => Nk::CapsLock,
        "numlock" => Nk::NumLock,
        "scrolllock" => Nk::ScrollLock,
        "printscreen" | "prtsc" => Nk::PrintScreen,
        "pause" | "break" => Nk::Pause,
        "menu" | "contextmenu" => Nk::ContextMenu,
        // --- F13..F35 ---
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
        // --- numpad (kp* spellings) ---
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
        // --- modifier-side keys (reported as key events under Kitty) ---
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
        // --- media / audio ---
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

/// PURE parser for `key <name> [mods=<list>] [type=<t>] [base=<c>]` -> an
/// [`InputEvent::Key`]: the event the `key` verb delivers by its route
/// (`control::InputRoute`), and pure, so the additive grammar is unit-testable
/// WITHOUT an `EventLoopProxy`. The SAME (Key, mods, base_layout, event_type) tuple a
/// human's named-key press builds, so the seam (the plain verb's encoder caller) yields
/// byte-identical output incl. Kitty REPORT_ALTERNATE_KEYS. All trailing tokens
/// are ADDITIVE — a bare `key up` still parses to empty mods / Press / no base.
/// Returns `None` for an unknown key name or a malformed `mods=`/`type=`/`base=`
/// value.
pub(crate) fn parse_key(rest: &str) -> Option<InputEvent> {
    use aterm_types::keyboard::Key;
    let (mut mods, body) = take_mods(rest)?;
    let (event_type, body) = take_event_type(&body)?;
    let (base_explicit, body) = take_base_layout(&body)?;
    // Inline prefixes (`ctrl+u`, `ctrl+shift+a`) add to `mods=`, so `ctrl+u` and
    // `u mods=ctrl` agree, and the encoder derives the control byte (0x15) itself.
    let (prefix_mods, body) = take_inline_mods(body.trim());
    mods |= prefix_mods;
    let body = body.trim();
    let Some(named) = named_key_from_token(body) else {
        // A single residual character: lower-cased, so `ctrl+U` == `ctrl+u`, as in
        // `parse_ctrl`.
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

/// Strip leading inline modifier prefixes (`ctrl+`, `alt+`, ... with the [`take_mods`]
/// aliases) from a `key` body, returning their mask and the rest. A prefix is consumed
/// only when `+` follows a known name, so a bare `up` is untouched.
fn take_inline_mods(body: &str) -> (aterm_types::keyboard::Modifiers, &str) {
    use aterm_types::keyboard::Modifiers;
    let mut m = Modifiers::empty();
    let mut rest = body;
    while let Some(plus) = rest.find('+') {
        let bit = match &rest[..plus] {
            "shift" => Modifiers::SHIFT,
            "ctrl" | "control" => Modifiers::CTRL,
            "alt" | "option" => Modifiers::ALT,
            // `meta`/`hyper` are their own bits (see `take_mods`).
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

/// The event a fencing verb's body delivers, parsed with that verb's own parser so the
/// fence classifies exactly what the verb sends. `None` for a malformed body and for
/// every verb whose body the fence does not read.
pub(crate) fn control_press(verb: &str, rest: &str) -> Option<InputEvent> {
    match verb {
        "key" => parse_key(rest),
        "ctrl" => parse_ctrl(rest),
        "paste" => Some(paste_event(rest)),
        _ => None,
    }
}

/// Licence fence for one authorized attempt of a verb whose input class is `class`:
/// `None` is no fence; `Some(keep_hand)` closes the move-licence cohort ahead of
/// the verb, keeping the glow's hand on its row or taking it off.
///
/// Every attempt whose `class` has [`Input::FENCES`] is a newer input boundary —
/// even a malformed one, which fences before its refusal so a swallowed key's
/// licence cannot survive an ignored attempt, and takes the hand off its row.
/// A body its verb's own parser reads ([`control_press`]) is fenced via
/// [`crate::app_input::press_fence`]: `ctrl w` and `key ctrl+w` keep the hand;
/// a release and a bare modifier are not fenced; a plain typed glyph, plain
/// Enter, Tab, and bare ⌃V arm their own licence class at the seam and pass
/// unfenced; a paste keeps the hand. A fenced write (`if=`, `if-gen=`, `if-fp=`,
/// `if-human=`) is delivered on the control thread past the seam
/// ([`input_if_fenced_observed`]), so it always fences, keeping the hand only for
/// events that keep it.
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

/// The usage line every malformed `key` body answers (the plain, guarded and both
/// cross-session arms).
pub(crate) const KEY_USAGE: &str = "ERR usage: key [id=<epoch>:<producer>:<seq>] [if=<re>] \
                                    [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] \
                                    [if-supervisor=<holder>] [unread=ok] [<mod>+]<name|char> \
                                    [mods=<list>] [type=press|repeat|release] [base=<c>] — \
                                    names include enter tab esc space backspace delete insert \
                                    up down left right home end pageup pagedown f1..f35 \
                                    kp0..kp9; e.g. key ctrl+c, key y\n";

// `if=<re>`: the guarded press, one check-and-press under the terminal lock

/// The leading `k=v` options a `send`/`key`/`turn` line may carry, taken off the tail at
/// the dispatch so every arm parses the tail without them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LeadingInputOptions {
    /// The idempotency key's raw `<epoch>:<producer>:<seq>` value.
    pub idem: Option<String>,
    /// The `if=<re>` guard's raw pattern — compiled later by [`compile_guard`],
    /// so a bad regex is answered where the halt gate has already been passed.
    pub guard: Option<String>,
    /// The `if-gen=<epoch>.<seq>` fence: the screen generation the caller last
    /// read (`status gen=`, `agent_gen=`). See [`InputFence`].
    pub if_gen: Option<crate::control::ScreenGen>,
    /// The `if-fp=<hex16>` fence: the visible screen's FNV-1a-64 the caller last
    /// read (`status hash=`, `turn hash=`). See [`InputFence`].
    pub if_fp: Option<u64>,
    /// The `if-human=<n>` fence: the person-gesture count the caller last read
    /// (`status human_seq=`, `text --json`'s `"human_seq"`). See [`InputFence`].
    pub if_human: Option<u64>,
    /// `unread=ok`: write even though the program has left earlier input unread, the one
    /// override of the unread-input gate (`crate::input_stall::admission`).
    pub unread_ok: bool,
    /// The `if-supervisor=<holder>` fence (the self-claim contract, §2): the
    /// write lands only while `<holder>` holds the session's live supervisor
    /// claim, read under the meta lock right before the write
    /// ([`crate::session_timeline::supervisor_fence`]). The window host's
    /// loop fences every write with its own holder, so no keystroke of its
    /// lands after a proven self-claim replaced it. Taken on the verbs whose
    /// input class has [`Input::SUPERVISED`] (`send`, `key`, `paste`, `turn`).
    pub if_supervisor: Option<String>,
    /// `cause=@<off>|<seq>`: the mail or record this input acted on, which the
    /// turn it opens names. Taken on the verbs whose input class has
    /// [`Input::CAUSE`] (`send`, `turn`); checked against what the session holds
    /// before any byte moves (`session_log::validate_cause`).
    pub cause: Option<crate::session_log::CauseRef>,
    /// `act=approve|deny|answer`: what this input means for the approval box or
    /// question it dismisses. Taken on the [`Input::GUARDED`] verbs (`send`,
    /// `key`); the box's close reads it only from a proven author
    /// ([`aterm_slog::Actor::is_proven`]).
    pub act: Option<aterm_slog::ActWord>,
    /// A recognized but malformed leading option (a guard with no pattern, an option
    /// given twice), answered by the dispatch after the halt gate, so a halted session
    /// says `ERR halted` whatever the rest of the line is.
    pub refusal: Option<String>,
    /// `as=person` (a development seam, [`PERSON_SEAM`]): the write goes in as
    /// a PERSON's gesture, [`Source::Human`] through the App input seam
    /// ([`person_input`]), so it stamps `status human_ms=` as a window
    /// keystroke does. A headless lab has no window to key, and the harness's
    /// rule for a person's own exchange is read off that stamp. Taken on the
    /// [`Input::GUARDED`] verbs (`send`, `key`), with no fence: a person's own
    /// keys are held to none.
    pub as_person: bool,
}

/// Whether this build takes `as=person` ([`LeadingInputOptions::as_person`]):
/// a development build only. The release cutter never enables `dev-seams`,
/// so a shipped aterm refuses it, and no control client can pose as a person.
pub(crate) const PERSON_SEAM: bool = cfg!(any(debug_assertions, feature = "dev-seams"));

/// What a build without the seam answers `as=person` with: nothing is written.
pub(crate) const PERSON_SEAM_REFUSED: &str = "ERR usage: as=person is a development seam (a debug \
     or dev-seams build's); a shipped aterm writes every control verb as a controller\n";

/// `as=person` read once on `opts` ([`LeadingInputOptions::as_person`]),
/// refused where `seam` is off and when given twice.
fn take_as_person(opts: &mut LeadingInputOptions, seam: bool) {
    if !seam {
        opts.refusal = Some(PERSON_SEAM_REFUSED.to_string());
    } else if std::mem::replace(&mut opts.as_person, true) {
        opts.refusal = Some("ERR usage: as=person given twice\n".to_string());
    }
}

/// The App lane's `unread=ok`: `hwkey`, `pointer` and `invoke` (the
/// `Input::APP_UNREAD` rows) take it as their first token, lifting the
/// unread-input gate for that one gesture. `ok` is the only value; any other
/// token, or none, leaves the line as it was (the arm refuses what it cannot
/// parse). Returns whether it was given and the rest.
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

/// The refusal for a malformed `if-supervisor=` value.
const IF_SUPERVISOR_USAGE: &str =
    "ERR usage: if-supervisor=<holder> needs the holder the claim was made under\n";

/// One `if-supervisor=<holder>` value into `opts`: a holder token, once.
fn take_if_supervisor(opts: &mut LeadingInputOptions, value: &str) {
    if !crate::session_timeline::valid_owner_token(value) || value == "-" {
        opts.refusal = Some(IF_SUPERVISOR_USAGE.to_string());
    } else if opts.if_supervisor.replace(value.to_string()).is_some() {
        opts.refusal = Some("ERR usage: if-supervisor= given twice\n".to_string());
    }
}

/// Take the LEADING options off a keyed/guarded verb's argument tail.
///
/// OPTIONS LEAD, the rule `post`'s frame detector already runs on: an option is
/// recognized only while the tail still begins with one, so `send hello id=1`
/// sends the ten characters `hello id=1` exactly as it always did and only a
/// leading `id=`/`if=` token changes meaning. The two options compose IN EITHER
/// ORDER (`key id=… if=… 1` and `key if=… id=… 1` are the same request). A
/// leading `--` ends option parsing and is dropped, so a caller that really must
/// `send` text beginning with `id=` or `if=` writes `send -- if=…`.
///
/// `id=` is scanned only on a verb whose `class` has [`Input::KEYED`] and `if=`
/// only on one with [`Input::GUARDED`] (`send` and `key`; `turn`'s own `settle=`
/// options own that slot); every other verb's tail is returned untouched, so an
/// `id=` or `if=` token elsewhere stays argument data (a leading `if=` on `paste`
/// is delivered as literal text, exactly like a leading `id=` on it).
///
/// `if-gen=<epoch>.<seq>` and `if-fp=<hex16>` are the other two [`InputFence`]
/// conditions, taken on the same two verbs and composing with `id=`/`if=` in any
/// order; a value that does not parse is carried as a refusal like a
/// pattern-less `if=`. A leading `if-seq=` is RECOGNIZED AND REFUSED: it was
/// this fence's first spelling, on the per-grid `content_seq`, which repeats
/// across an alternate-screen re-entry (see [`crate::control::ScreenGen`]); a
/// caller still spelling it gets `ERR usage` naming `if-gen=`, never a press on
/// an unsound fence and never the option typed as `send` text.
///
/// `unread=ok` rides the same two verbs and composes the same way: it lifts the
/// unread-input gate (`crate::input_stall::admission`) for this one write, so a
/// driver that means to queue behind input the program has not read can. `ok`
/// is its only value; any other, or a second `unread=`, is a carried refusal.
///
/// `if-supervisor=<holder>` is taken on a verb whose class has
/// [`Input::SUPERVISED`] (on `paste`, whose body is raw, it is the one option
/// taken), `cause=` on one with [`Input::CAUSE`], and `act=` on a guarded one.
///
/// The regex is ONE wire token: the control line is split on whitespace and
/// nothing quotes, so a pattern with a space in it must be written with `.` in
/// its place (`if=Do.you.want.to.proceed`). The client says so before sending
/// when it sees whitespace in a guard.
///
/// Returns the options and the remaining tail. With no leading option the tail
/// is `rest` byte for byte (leading whitespace included — `send` bodies are raw).
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
        // `unread=ok`: queue behind input the program has not read. `ok` is the
        // only value, so a typo is refused rather than typed into the program as text.
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
        // `act=`: a closed word, once, or the line is refused before a byte moves.
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
        // `as=person` only, spelled whole: any other `as=` word is the text's.
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

/// `key|send as=person <body>` (the [`PERSON_SEAM`]): the verb's own event
/// ([`parse_key`], [`send_bytes`]) posted as ONE [`Source::Human`] batch to
/// `session` through the App input seam (`Wake::Input`), which routes it to
/// the session's view, or its hidden path when it is not on screen, and
/// stamps the person's clock as a window keystroke would. Every admission
/// before it (halt, edit hold, turn arbitration, the unread gate) has run.
/// Pinned to `session`, the write passes every card and overlay as any scoped
/// control write does, so it reaches the session's program and never a
/// palette's or a gateway card's rows (Take Over among them).
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

/// An `if-fp=` value: 1..=16 hex digits, the spelling `status hash=` prints
/// (`{:016x}`). Anything else — empty, a sign, a `0x` prefix, a 17th digit — is
/// refused rather than read as some other number.
fn parse_fp(value: &str) -> Option<u64> {
    if value.is_empty() || value.len() > 16 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(value, 16).ok()
}

/// Compile a leading `if=<re>` guard outside the terminal lock, with the bounded
/// `aterm_observe::row_matcher` `await match` uses. A bad pattern is `ERR badregex`.
pub(crate) fn compile_guard(pattern: Option<&str>) -> Result<Option<Arc<dyn RowMatch>>, String> {
    match pattern {
        None => Ok(None),
        Some(pattern) => match aterm_observe::row_matcher(pattern) {
            Ok(matcher) => Ok(Some(matcher)),
            Err(_) => Err("ERR badregex\n".to_string()),
        },
    }
}

/// The conditions a fenced write is held to, all checked under the same hold of the
/// terminal lock the write is made under. Every armed condition must hold.
///
/// * `generation` (`if-gen=<epoch>.<seq>`): the screen generation
///   ([`crate::control::ScreenGen`]) still equals the one the caller read. ANY
///   output since — a repaint of identical text included — moves its seq, and
///   any screen switch moves its epoch, so this is the strict fence: "the
///   screen I decided on is the screen you are about to write to". The bare
///   `content_seq` is not enough for that: it is per grid, and a re-entered
///   alternate screen can show a new box at an old value.
/// * `fp` (`if-fp=<hex16>`): FNV-1a-64 of the untrimmed visible screen (the
///   `status hash=` / `turn hash=` value, [`crate::frame_memo::ScreenText::fnv`])
///   still equals the caller's. A re-render of the same text keeps it, so a TUI
///   that repaints without changing (Claude Code's does) does not skip a press
///   the screen still vouches for.
/// * `guard` (`if=<re>`): some visible row matches — or, with
///   `guard_at_cursor`, the row holding the cursor does (`turn …
///   submit=guarded:<re>`, where the row that matters is the one a submit
///   lands in).
/// * `human` (`if-human=<n>`): no person has given the session input since
///   the read that returned `n` ([`crate::human_input::HumanInputStamp::seq`])
///   — the one thing the screen fences cannot see: a person's key already
///   written to the PTY and not yet read by the program moves no screen.
///   Refused `OK skipped reason=person`. Not atomic with the person's own write,
///   which takes no terminal lock: a gesture counted after this check and written
///   before this write, inside the one lock hold, still lands first (the module
///   header of [`crate::human_input`] says why). A controller's own write —
///   this one included — never counts.
///
/// It compares the screen generation, then writes, under one lock: a
/// supervisor that decided on box A cannot have its `1` land on a box B that
/// replaced A between its read and its press.
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
    /// The published foreground identity the decision belongs to. Checked
    /// after the terminal fence, with the timeline held through delivery.
    pub foreground: Option<&'a crate::session_timeline::ForegroundStamp>,
    /// Match `guard` against the cursor's row only. A composer holds the cursor and a
    /// select box does not, so a transcript row that matches (Claude Code draws earlier
    /// prompts with the composer's `❯`) cannot vouch for an Enter an approval box would
    /// take as "Yes".
    pub guard_at_cursor: bool,
    /// The worker's initial command still targets an input-ready shell
    /// block. Checked under the same terminal lock as the generation/write.
    pub entering_command: bool,
}

impl InputFence<'_> {
    /// Whether any condition is armed; the dispatch routes only an armed write to the
    /// fenced arm.
    pub(crate) fn is_armed(&self) -> bool {
        self.guard.is_some()
            || self.generation.is_some()
            || self.fp.is_some()
            || self.human.is_some()
            || self.foreground.is_some()
            || self.entering_command
    }

    /// Decide the fence against `terminal` (the caller holds its lock). `fp` is
    /// read off the session's published text through `look`, taken before that
    /// lock. `generation` and `fp` are asked before the guard: a moved screen
    /// answers [`GuardedInput::Changed`] even where the guard would also have
    /// missed, because "the screen moved" is the more useful thing to tell a
    /// driver.
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
    /// No visible row matched the guard: NOTHING was written.
    Skipped,
    /// The screen moved since the caller's read (`if-gen=`/`if-fp=` no longer
    /// holds): NOTHING was written.
    Changed,
    /// A person gave the session input since the caller's read (`if-human=`
    /// no longer holds): NOTHING was written.
    Person,
    /// A row matched, and the bytes were handed to the kernel — the rest of a
    /// frame the kernel took only part of queued right behind it — or refused
    /// with zero bytes, or accepted in part with the rest lost, while the lock
    /// was still held.
    Pressed(Delivery),
}

/// Atomic check-and-press: decide `fence` ([`InputFence`]: the screen
/// generation, the screen hash, the row guard) and, if every armed condition
/// holds, deliver `ev` — both under the same hold of the terminal lock.
///
/// The PTY reader applies output to the engine under `term` for the whole of
/// `process(&buf)`, so a check made under `term` and a write made before it is
/// released cannot have an output batch land between them: what the guard saw is
/// the screen the keystroke answers.
///
/// The bytes go through the sink's bounded, non-parking guarded write
/// (`try_write_frame_whole_with_receipt`), not the spilling non-parking egress
/// the App seam uses: a frame the kernel cannot begin taking now would otherwise
/// be queued behind the spill and delivered later, to whatever screen is up by
/// then. A busy sink (a spill ahead of it, another writer on the fd, a full tty
/// buffer, or a master the spawn could not make non-blocking) answers zero
/// bytes, which the caller reports as a transient `ERR busy`, and the driver
/// retries against the live screen. A frame the kernel began taking is
/// delivered whole: a pty's input queue holds about 1 KB (1022 bytes on
/// Darwin), so a longer text comes back short, and its rest is queued in order
/// right behind the prefix (the sink's spill, ahead of every later write). The
/// reply is in doubt only where bytes are lost (a rest no drainer can be
/// arranged for). The lock hold is bounded by the same non-parking property:
/// nothing here waits, and the drainer's spawn is deferred to the session's
/// reply writer.
///
/// Runs on the control thread against the resolved target's `(term, sink)`, like
/// every cross-session input verb, so for the tab on screen it skips the App
/// input seam's side effects (blink reset, viewport snap, typing-momentum hints,
/// the `video … keys` ledger — counted as unseamed, never silently zero) and
/// does not arm a cursor licence of its own; the dispatch keeps the licence
/// fence for it. A press that must ride the seam is a plain `key`.
pub(crate) fn input_if_fenced_observed(
    term: &Arc<Mutex<Terminal>>,
    ctx: &SessionCtx,
    ev: InputEvent,
    fence: &InputFence<'_>,
    completion: Option<crate::control_input_ack::InputCompletion>,
) -> GuardedInput {
    let mut look = ctx.frame.look_frame();
    let terminal = term_lock(term);
    // A person's hand since the caller's read, asked first under the same hold: "a
    // person keyed" tells a driver more than "the screen moved".
    if fence.human.is_some_and(|n| ctx.human_input.seq() != n) {
        return GuardedInput::Person;
    }
    if let Some(declined) = fence.check(&terminal, &mut look) {
        return declined;
    }
    // The encoder is read under the same lock as the guard: the keyboard mode is
    // part of the screen state the check vouched for.
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
        // Raw bytes go to the PTY verbatim — the `send` policy, no encoder.
        InputEvent::KeySequence(bytes) => bytes.clone(),
        _ => return GuardedInput::Pressed(Delivery::BusyZero),
    };
    // frame -> terminal -> timeline, the status sweep's same leaf-lock order.
    // Keep publication still only through the bounded sink write; receipt
    // publication and completion callbacks run after this leaf is released.
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
        // A faithful no-op (a legacy release): nothing to deliver, nothing owed.
        drop(foreground);
        echo_write.finish_delivery(Delivery::Full, None, &ctx.sink);
        return GuardedInput::Pressed(Delivery::Full);
    }
    let (outcome, accepted_order) = ctx.sink.try_write_frame_whole_with_receipt(&bytes);
    drop(foreground);
    let delivery = match outcome {
        // Queued: the rest rides the spill right behind the accepted prefix,
        // accepted for ordered delivery as a spilled frame is (`Delivery::Full`).
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

/// The verb reply for a [`GuardedInput`]. `OK skipped` is an ANSWER, not an
/// error (the guard is a question and "no" is one of its answers); the dispatch
/// stamps ` seq=<n>` on it like every other `OK` from an input verb, so the
/// driver's next `await seq` is anchored either way. Zero bytes moved is the
/// transient `ERR busy` class — the sequence an `id=` claimed is given back,
/// because nothing was written; a partial write whose rest could not be queued
/// is in-doubt and says so. A frame whose rest WAS queued behind its prefix is
/// `OK`: accepted for ordered delivery, as a spilled `send` is.
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

/// `key if=<re> <name>`: the `key` verb's guarded twin — the same [`parse_key`]
/// grammar, delivered by [`input_if_fenced_observed`] instead of the seam. A
/// malformed name is the usage line BEFORE any lock is taken. The guard-only
/// spelling of [`cmd_key_fenced`], which is what the dispatch calls.
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

/// `key [if=<re>] [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] <name>`: the
/// fenced press. A moved screen answers `OK skipped reason=changed`, a person who keyed
/// since the read `OK skipped reason=person`; either writes nothing.
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
        Some(ev) => guarded_input_reply(input_if_fenced_observed(term, ctx, ev, fence, completion)),
        None => KEY_USAGE.to_string(),
    }
}

/// `send if=<re> <text>`: the guarded `send` — the same [`send_bytes`] body
/// (the literal `\n` submit form included), delivered by
/// [`input_if_fenced_observed`]. The guard-only spelling of [`cmd_send_fenced`].
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

/// `send [if=<re>] [if-gen=<epoch>.<seq>] [if-fp=<hex16>] [if-human=<n>] <text>`: the
/// fenced raw write.
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
    guarded_input_reply(input_if_fenced_observed(term, ctx, ev, fence, completion))
}

/// `hwkey <char|name> [mods=…] [count=…] [interval=…]` -> inject the key through the OS
/// event queue instead of the control seam.
///
/// The deliberately different spelling from `key` is the point: the two
/// verbs are not interchangeable and must never be confused in a measurement.
/// `key` posts a decoded `InputEvent` to the main thread over a `CFRunLoopSource`
/// and the arrival is stamped inside the handler, so the key is BORN already
/// dequeued — no socket-injected key has ever measured OS-level key queueing.
/// `hwkey` builds a real `NSEvent` and posts it into this application's own event
/// queue, from THIS (control) thread, so it is dequeued, routed and translated by
/// the same code a physical keypress runs, including the
/// `note_key_arrival_queued` backdate that prices a parked event loop.
///
/// Reply honesty: `OK posted=<n> key=<0|1>` means `n` press events were handed to
/// the OS event queue, NOT that any of them reached the PTY; `key=1` that the target
/// window was the key window of the active app when they were posted, where a
/// person's keys land and the input method composes (`key=0`: another app held the
/// keyboard, so an Option dead key is not composed as a press there would be). That is a weaker claim than
/// `key`'s `OK` (which means the seam applied the event), and it is the strongest
/// claim this path can truthfully make — the whole point is that delivery is the
/// OS's business from the post onward. Read the result through `metrics`
/// (`n_key_write` only ever moves on the hardware path) or `text`.
pub(crate) fn cmd_hwkey(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    // A sudo password sheet is answered by a person only: `hwkey` posts real
    // NSEvents that could reach its secure field, so it is refused while one
    // stands (`docs/DESIGN-sudo-askpass-2026-10-15.md` §3.5).
    #[cfg(target_os = "macos")]
    if crate::askpass_sheet::sheets_open() {
        return "ERR hwkey: a sudo password sheet is up\n".to_string();
    }
    let spec = match crate::hwkey::parse_hwkey(rest) {
        Ok(s) => s,
        Err(e) => return e,
    };
    // One main-thread round trip for the window number, then nothing else touches the
    // main thread: the posting loop must run, and the posted keys wait, while that thread
    // is parked inside `nextDrawable`.
    // A GATEWAY CARD (the palette, Overview, a tab menu) open on the window is
    // refused there too: the posted keys reach the person's own unmarked key
    // path, so they could press Take Over through it (`App::hwkey_target`).
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

/// A write refused at the session's input cap (the sink's spill, or its ordered
/// writer's queue) with nothing written ([`InputOutcome::QueueFull`]), on the front
/// tab and in the background alike. `ERR busy`, so a driver's back-off retries it
/// and an `id=` mark is given back (`pty_idem`).
pub(super) const INPUT_QUEUE_FULL: &str =
    "ERR busy input-queue-full: nothing written (the program has not read its queued input)\n";

/// Map a reply-bearing input outcome to a verb reply line; an `Err` (event loop closed,
/// no window) is already a full `ERR …\n` line.
pub(super) fn input_reply_to_str(r: Result<InputOutcome, String>) -> String {
    match r {
        Ok(InputOutcome::Ok) => "OK\n".to_string(),
        Ok(InputOutcome::RangeRejected) => "ERR out of range\n".to_string(),
        Ok(InputOutcome::WriteFailed) => "ERR write failed\n".to_string(),
        Ok(InputOutcome::QueueFull) => INPUT_QUEUE_FULL.to_string(),
        Err(e) => e,
    }
}

/// [`input_reply_to_str`] for a verb whose `OK` counts the bytes it wrote
/// (`paste`, `feed`, `paste-bin`, `feed-bin`): `OK <n> bytes`.
pub(super) fn bytes_reply(r: Result<InputOutcome, String>, n: usize) -> String {
    match r {
        Ok(InputOutcome::Ok) => format!("OK {n} bytes\n"),
        other => input_reply_to_str(other),
    }
}

/// PURE parser for `ctrl <letter>` -> a Control-modified character key, the event
/// the `ctrl` verb delivers by its route, so it is unit-testable headless. The seam encodes it (under the
/// CURRENT keyboard mode) as a proper CSI-u sequence (Kitty/xterm) or the legacy
/// control byte, byte-identical to a human Ctrl chord. Returns `None` unless
/// `rest` is exactly one (non-whitespace) char.
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

/// `feed <hex>` -> write raw bytes (decoded from a hex string, whitespace
/// allowed) straight to the PTY. The escape hatch for control/binary bytes the
/// line-delimited `send` verb can't carry: `feed 03` = Ctrl-C, `feed 1b5b41` =
/// ESC[A, `feed 0a` = a real newline. Replies `OK <n> bytes\n` or `ERR bad hex`.
pub(crate) fn feed_bytes(rest: &str) -> Result<Vec<u8>, &'static str> {
    let hex: String = rest.chars().filter(|c| !c.is_whitespace()).collect();
    if !hex.len().is_multiple_of(2) {
        return Err("ERR bad hex (odd length)\n");
    }
    aterm_codec::hex::decode_vec(hex.as_bytes()).ok_or("ERR bad hex\n")
}

/// `signal <name> [pid=<n>] [quiet=<s>]` -> deliver a job-control signal to the PTY's
/// foreground process group (`tcgetpgrp` on the master, then `killpg`), whatever the line
/// discipline would generate. `name` is `int`/`c`, `quit`, `tstp`/`z`, `hup`, `term`,
/// `kill` or `cont`.
///
/// `pid=<n>` sends to exactly `n` (`kill`), and only while `n` leads the foreground group;
/// otherwise `ERR pid <n> is not the foreground process group` and nothing is sent. Every
/// form is refused under a `hold` by the dispatcher, so "this exact process, and no hold"
/// is one server-side decision.
///
/// `quiet=<s>` makes the signal wait for the tab to be left alone, in the
/// same decision: it is refused `ERR busy person human_ms=<ms>` when a person
/// gave the session input within `s` seconds (`human_ms`, the stamp `status`
/// reads), and `ERR busy input-unread bytes=<n>` while input someone wrote is
/// queued unread — a first prompt on its way, which a `term` would drop and the
/// shell after it run — and nothing is sent or discarded.
///
/// `term`, `kill`, `hup` and `quit`, in either form, first discard the input
/// a raw-mode (or stopped) program left unread and say so (`discarded=<n>`):
/// the shell that takes the tty back would otherwise run it
/// ([`crate::input_stall::discard_before_signal`]). A `pid=` the
/// verb refuses discards nothing. The reply does not wait to see the program
/// end: a published stall whose program lives through the signal stays
/// published, and names `signal kill` once it has for five seconds
/// ([`crate::input_stall::Restart`]) — or, where the act would be carried,
/// the explicit `restart` ([`crate::restart_act::offered`]).
///
/// `human_ms` is the session's person stamp at the request (ms since a person last gave
/// it input; `None`: never), read by the caller.
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
    // A signal that ends the program first drops the input it left unread,
    // so the shell that inherits the tty never runs it
    // (`input_stall::discard_before_signal`).
    let discarded = crate::input_stall::discard_before_signal(master, sink, sig);
    let tail = discarded.map_or_else(String::new, |n| format!(" discarded={n}"));
    // A signal is just a signal: the harness reads the exit it causes as the
    // program's own. The restart that is recognised as one is `restart`
    // (`crate::restart_act`), whose record comes before its signal.
    let rc = match pid {
        // SAFETY: `kill` on a pid this terminal's own foreground group is led
        // by, re-read just above.
        Some(pid) => unsafe { libc::kill(pid, sig) },
        // SAFETY: `killpg` on the terminal's own foreground group, read from
        // the master just above; it takes no pointers.
        None => unsafe { libc::killpg(pgrp, sig) },
    };
    let reply = match (pid, rc) {
        (Some(pid), 0) => format!("OK signalled pid {pid}{tail}\n"),
        (Some(_), _) => format!("ERR kill failed{tail}\n"),
        (None, 0) => format!("OK signalled pgrp {pgrp}{tail}\n"),
        (None, _) => format!("ERR killpg failed{tail}\n"),
    };
    // A program that lives through it keeps its stall: the watch looks now
    // (`input_stall::after_signal`).
    crate::input_stall::after_signal(sink, sig);
    reply
}

/// The `signal` verb as the control dispatch runs it
/// ([`super::serve_session_verb`]'s arm): [`cmd_signal`] on the session's own person stamp (`human_ms`, read
/// now).
pub(crate) fn signal_verb(master: i32, rest: &str, ctx: &SessionCtx) -> String {
    cmd_signal(
        master,
        rest,
        &ctx.sink,
        ctx.human_input.ms_since(crate::metrics::now_us()),
    )
}

/// The `restart` verb as the control dispatch runs it
/// ([`super::serve_session_verb`]'s arm): explicit restart of the tab's agent in place
/// ([`crate::restart_act`]).
pub(crate) fn restart_verb(master: i32, rest: &str, ctx: &std::sync::Arc<SessionCtx>) -> String {
    crate::restart_act::restart(master, rest, ctx)
}

/// Pure parser for `signal <name> [pid=<n>] [quiet=<s>]` -> (signal number, exact pid,
/// quiet seconds).
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
        // The remedy the unread-input gate names for `input=stopped`: a job
        // stopped with input queued reads nothing until it is continued.
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

/// Windows arm of the `signal` verb: job-control signals have no ConPTY analogue, so it
/// refuses (for Ctrl-C, `feed 03`, which the ConPTY host cooks into a console Ctrl-C).
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

pub(crate) const MOUSE_USAGE: &str = "ERR usage: mouse <press|release|move|wheelup|wheeldown|wheelleft|wheelright> <left|middle|right|back|forward> <row> <col> [mods=..] [count=N] [side=left|right] [block=0|1] [lines=N]\n";

/// Upper bound on one wheel `mouse` verb's `lines=N`: the seam emits one wheel report per
/// line under a tracking app, so an unbounded count would flood the PTY. It is the seam's
/// own [`crate::input::MAX_WHEEL_BURST`], which also bounds the DEC 1007 alt-scroll
/// arrows, so the two ceilings cannot drift apart.
const MAX_WHEEL_LINES: i32 = crate::input::MAX_WHEEL_BURST;

/// Pure parser for the `mouse` verb -> an engine-neutral mouse [`InputEvent`].
/// Returns `Err(usage/err string)` for a malformed line, `Ok(event)` otherwise.
///
/// A tracking-OFF press/release runs the same selection machinery as a human
/// gesture, and `mods`/`count`/`side`/`block` are carried as data. The verb is
/// reply-bearing: `OK` once the seam has applied the batch on the main thread,
/// `ERR input dispatch failed: …` when there is no frontmost window.
///
/// One `mouse move` verb line posts one `MouseMove`, so a controller that wants
/// intermediate motion reports under a tracking app issues a `press` then N
/// `move`s then a `release` as separate verb lines.
///
/// Grammar: `mouse <action> <button> <row> <col> [mods=..] [count=N]
/// [side=left|right] [block=0|1] [lines=N]`. `action` is `press|release|move|
/// wheelup|wheeldown|wheelleft|wheelright`; `button` is
/// `left|middle|right|back|forward` (ignored for the wheel actions); `row`/`col`
/// are 0-based.
///
/// The additive tokens carry `mods=` (modifier mask), `count=` (click depth
/// 1..=3), `side=` (cell-half), `block=1` (rectangular-selection intent for a
/// single-click press, carried as data so the seam never reads ambient modifier
/// state), and `lines=N` (wheel-notch count for wheel actions, default 1,
/// clamped to `1..=MAX_WHEEL_LINES`, ignored by non-wheel actions).
pub(crate) fn parse_mouse(rest: &str) -> Result<InputEvent, String> {
    use aterm_core::selection::SelectionSide;
    use aterm_types::mouse::{ALT_MASK, CTRL_MASK, MouseButton, SHIFT_MASK, WheelDir};
    let mut action = "";
    let mut mods: u8 = 0;
    let mut click_count: u8 = 1;
    let mut side = SelectionSide::Left;
    let mut block = false;
    let mut wheel_lines: i32 = 1;
    // Positional tokens (in order), interpreted per-action below: press/release/wheel
    // take `<button> <row> <col>`, and `move` takes either `<row> <col>` (no-button
    // hover, code 3) or `<button> <row> <col>` (held-button drag).
    let mut positional: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(list) = tok.strip_prefix("mods=") {
            for name in list.split(['+', ',']) {
                // Unknown modifiers are refused (`take_mods` rule). Legacy/SGR
                // mouse reports only have bits for SHIFT/META/CTRL, so `super`,
                // `cmd`, and `hyper` are rejected here rather than silently dropped.
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
            // A non-positive or non-numeric count is 1; clamped so one line cannot
            // flood the PTY.
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
        // `move` with two positionals is a no-button hover (code 3); with three, a
        // held-button drag.
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
            // `button` is ignored for the wheel actions but still required.
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
                    // parse_mouse is scope-BLIND: the copy-on-select suppression
                    // policy is a control-authority decision applied by the arm
                    // AFTER parsing (only for a non-Owner scope), so the pure parser
                    // output stays byte/struct-identical to the human builder.
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
                // The outer match fenced the action set, so `_` is `wheeldown`.
                wheel => InputEvent::Wheel {
                    dir: match wheel {
                        "wheelup" => WheelDir::Up,
                        "wheelleft" => WheelDir::Left,
                        "wheelright" => WheelDir::Right,
                        _ => WheelDir::Down,
                    },
                    lines: wheel_lines,
                    // The verb has no pixel field: each of its `lines` is a notch,
                    // worth the platform's lines-per-detent where a hand's notch is.
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

/// `mouse <action> <button> <row> <col> [mods=..] [count=N] [side=left|right]
/// [block=0|1]` -> post [`parse_mouse`]'s event to the seam, which reads the current
/// mouse mode once and either emits the `encode_mouse_*` report (tracking on) or runs the
/// human's local selection gesture (tracking off). Reply-bearing: `OK` once the seam
/// applied it, `ERR input dispatch failed: …` with no frontmost window.
///
/// PURE control-authority decision: whether a `mouse`-verb gesture from `scope`
/// must have its copy-on-select CLIPBOARD side-effect suppressed. Only a NON-OWNER
/// (scoped-edge) gesture is suppressed; the Owner god token (in-session / owner
/// automation) is exempt, matching the `front_drive_escalation` Owner
/// carve-out. Split out pure so the policy is unit-testable without an event loop
/// (mirrors `front_drive_escalation`).
pub(crate) fn scope_suppresses_copy_on_select(scope: super::Scope) -> bool {
    !scope.is_owner_class()
}

/// Stamp the scoped-edge copy-on-select suppression onto a mouse `InputEvent` (a
/// no-op for Owner scope, and for non-`MouseButton` events which never trigger the
/// copy-on-select side-effect). Applied by the `mouse` arm AFTER the scope-blind
/// [`parse_mouse`], so the pure parser output stays byte/struct-identical to human
/// input and the suppression rides ONLY the release that settles the selection.
///
/// COPY-ON-SELECT FENCE: a scoped (non-Owner) WriteInput edge can inject a
/// select+release gesture that, on the active tab, would auto-copy the selected
/// (attacker-chosen or on-screen) text to the SYSTEM clipboard — exfil past the
/// `copy` → `ClipboardWrite` fence. The policy is stamped on the event at the
/// control-authority layer, the only place `Scope` is known, so
/// `finish_selection` skips the pbcopy/PRIMARY side-effect for THIS gesture. The
/// selection itself is still made (viewport nav is not exfil); only the automatic
/// clipboard write is suppressed. Owner scope and a real human gesture leave the
/// flag `false`. This is a clipboard SIDE-EFFECT, never a PTY byte, so it does NOT
/// gate `seam_egress` / violate `bytes_human_eq_controller`.
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

/// The `paste` verb's event, framed at the drain: one builder for the verb and for the
/// fence that classifies it ([`control_press`]).
///
/// `paste <text>` writes `<text>` to the PTY exactly as if the user pasted it:
/// [`Terminal::format_paste`] strips control bytes that could escape the guards
/// (ESC, C1 controls), converts line breaks to CR, and wraps the body in the
/// bracketed-paste guards `ESC[200~` ... `ESC[201~` when the app has enabled
/// bracketed paste (DECSET 2004) AT THE MOMENT THE FRAME IS WRITTEN — the verb
/// makes no gesture and is shown no confirmation, so it has no earlier answer to
/// keep faith with ([`crate::input::PasteFraming::AtDrain`]; the human Cmd-V path
/// captures its answer instead, see `App::deliver_paste`). For raw unsanitized
/// bytes use `feed`/`send` instead.
pub(crate) fn paste_event(rest: &str) -> InputEvent {
    InputEvent::Paste(paste_text(rest), crate::input::PasteFraming::AtDrain)
}

/// The `paste` verb's text: a literal trailing `\n` (backslash + n) becomes a real line
/// break, which `format_paste` sends as CR.
pub(crate) fn paste_text(rest: &str) -> String {
    match rest.strip_suffix("\\n") {
        Some(head) => format!("{head}\n"),
        None => rest.to_string(),
    }
}

/// PURE parser for the `focus` verb's `in/out` argument: the [`InputEvent::Focus`]
/// it delivers by its route, one grammar for every route. `in`/`1`/`true`/`focus`
/// => focus-in. `focus <in|out>` drives DEC 1004 focus reporting (kills
/// divergence j: a controller-only session can satisfy a focus-tracking app's
/// oracle): the seam emits ESC[I / ESC[O when the app enabled focus reporting,
/// byte-identical to the human `on_focus` egress.
pub(crate) fn parse_focus(rest: &str) -> Option<bool> {
    match rest.trim() {
        "in" | "1" | "true" | "focus" => Some(true),
        "out" | "0" | "false" | "blur" => Some(false),
        _ => None,
    }
}

/// Parse `tab <arg>` into its [`TabAction`]: `new`, a 0-based `<N>`, `next`/`prev`,
/// `close [N]`, `move <from> <to>`. `None` is the usage error.
pub(crate) fn parse_tab(rest: &str) -> Option<TabAction> {
    let rest = rest.trim();
    // Multi-word forms first: `close [N]` and `move <from> <to>`.
    let mut it = rest.split_whitespace();
    match it.next() {
        Some("new") if it.next().is_none() => return Some(TabAction::New),
        Some("next") if it.next().is_none() => return Some(TabAction::Next),
        Some("prev") if it.next().is_none() => return Some(TabAction::Prev),
        // `close` (active tab) or `close <N>` (a specific tab).
        Some("close") => {
            return match it.next() {
                None => Some(TabAction::Close(None)),
                Some(n) => {
                    let i = n.parse::<usize>().ok()?;
                    // Reject trailing junk after the index.
                    it.next().is_none().then_some(TabAction::Close(Some(i)))
                }
            };
        }
        // `move <from> <to>` — reorder.
        Some("move") => {
            let (from, to) = (it.next()?, it.next()?);
            if it.next().is_some() {
                return None; // trailing junk
            }
            let from = from.parse::<usize>().ok()?;
            let to = to.parse::<usize>().ok()?;
            return Some(TabAction::Move { from, to });
        }
        _ => {}
    }
    // Otherwise a bare 0-based index selects a tab.
    rest.parse::<usize>().ok().map(TabAction::Select)
}

/// The refusal `tab <N>` / `tab close <N>` / `tab move <from> <to>` answer for
/// an index no tab holds. Pure (`App::apply_tab_cmd_in` formats through it).
///
/// The wire's `<N>` is 0-based (matching `inspect app/v1 tabs`'s `index=`),
/// while the strip paints 1-based ordinals, so the reply states the valid
/// 0-based index range (`indexes 0..2`).
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

/// `tab new | <N> | next | prev | close [N] | move <from> <to>` -> drive the front window's
/// tabs and reply `OK <active_index> <tab_count>` when the action happened, or `ERR <why>`:
/// an index no tab holds, a host that declined the close, or a `close` refused by a
/// running foreground job (`ERR close refused (a running job armed the last-tab confirm)`
/// for the last tab, `ERR close refused (a running job in that tab)` among several;
/// `App::tab_cmd_front` runs the same `WireRefuseBusy` policy).
///
/// `App` is mutated only on the event loop, so this posts [`Wake::TabCmd`] and blocks on
/// the reply, as `cmd_chrome` does; the main thread applies the action through the
/// keyboard/menu paths (`open_tab`, `switch_tab`, `cycle_tab`). `by` is the caller, the
/// exit ledger's `by=` for a session `close` retires.
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
        // A refused action says so, in the words of `cmd_tab_aimed` and `@<sid> close`.
        Ok(Err(why)) => format!("ERR {why}\n"),
        Err(error) => format!("ERR tab command failed: {error}\n"),
    }
}

/// `hover <on|off>` -> toggle the drag-and-drop drop-target highlight on the front window
/// (the flag a real drag drives) on the main thread ([`Wake::SetDragHover`]), so the drop
/// affordance can be driven and captured headlessly.
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

/// Pure parser for `find <sub…>`; `Err` is the usage line the verb replies with. `type`'s
/// payload is the rest of the line verbatim after the one separating space, so a query
/// may begin or end with a space.
pub(crate) fn parse_find(rest: &str) -> Result<crate::app_search::FindAction, String> {
    use crate::app_search::{FindAction, SearchEdit};
    let rest = rest.trim_start();
    let (head, tail) = match rest.split_once(' ') {
        Some((head, tail)) => (head, Some(tail)),
        None => (rest, None),
    };
    // A no-argument form refuses a trailing argument: `OK` to `find next 3` would report
    // one step as three.
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

/// `find <sub…>` -> drive the front window's find bar on the main thread
/// ([`Wake::FindCmd`]; [`crate::App::find_cmd`]'s arms are the keystroke handler's own
/// functions) and reply with the bar's state read back after the action.
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

/// Format a [`crate::app_search::FindStatus`] as the `find` reply. An absent match prints
/// `-`, never a coordinate: `row`/`col`/`len` and `current` come from `Option`s.
pub(crate) fn find_status_line(status: &crate::app_search::FindStatus) -> String {
    if !status.open {
        return "OK open=0\n".to_string();
    }
    let bit = |b: bool| u8::from(b);
    // A length on the wire, as `search` reports. `SearchState::matches` carries
    // `(row, start_col, end_col)` with `end_col` inclusive and unclamped (a hit across a
    // soft wrap runs past the width by the part on the next row), so the length is
    // `end - start + 1` and may exceed the width, as `search`'s continuation marker does.
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
        // 1-based, the "3 of 7" the find bar and the window title show.
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
        // The bar's `…`: the terminal moved since `matches` was counted, so the count
        // is never stated unqualified.
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

/// `pane <left|right|up|down>` -> move keyboard focus to the adjacent pane of the front
/// window's active tab through `focus_pane_in` (the `focus_pane_*` bindings' path) and
/// reply `OK moved=<0|1>`. `moved=0` (a single pane, an edge pane) is an answer, not an
/// error. The layout is `panes`' to report: a write edge has no read authority here.
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

/// Pure parser for `pointer [move <r> <c>|click|leave|status]`. `Err` is the reply line;
/// an out-of-range cell is refused, never clamped.
const POINTER_MODS_USAGE: &str =
    "ERR usage: pointer mods <none|link|cmd|ctrl|alt|shift joined by +>\n";

/// `pointer mods <list>`: the modifier state a window holds, `+`-joined from
/// `cmd` (alias `super`), `ctrl`, `alt` (alias `option`), `shift` and `link`
/// (the platform's open-link modifier: Cmd on macOS, Ctrl elsewhere), or
/// `none`. `None` for a word outside that set or an empty part.
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
            // A native view's control or point (cs-25): `key=<ui-key>` or
            // `xy=<x>,<y>` in the view's logical coordinates.
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
            // A NEGATIVE row is a chrome row above the grid (`-1` the one
            // right above it): the band, the strip.
            if row < 0 {
                return match u16::try_from(row.unsigned_abs()) {
                    Ok(up) if up <= MAX_CHROME_ROWS => Ok(PointerAction::MoveChrome { up, col }),
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

/// The `pointer wheel` usage line.
const POINTER_WHEEL_USAGE: &str =
    "ERR usage: pointer wheel dy=<n> [dx=<n>] [unit=lines|pt] [key=<ui-key>|xy=<x>,<y>]\n";

/// The largest `pointer wheel` delta on either axis, in its unit: past any
/// list's end in lines, about six hundred rows in points.
const MAX_POINTER_WHEEL: f64 = 10_000.0;

/// `pointer wheel dy=<n> [dx=<n>] [unit=lines|pt] [key=<ui-key>|xy=<x>,<y>]`:
/// each field at most once, in any order; `dy` down and `dx` right (a
/// fraction of a line banks as a free-spinning wheel's does); at least one
/// non-zero, each within [`MAX_POINTER_WHEEL`]. `key=`/`xy=` aim at a native
/// view's control or point as `pointer move` does.
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

/// The `pointer move` usage line: a cell, or a native view's control or point.
const POINTER_MOVE_USAGE: &str =
    "ERR usage: pointer move <r> <c> | move key=<ui-key> | move xy=<x>,<y>\n";

/// The deepest chrome row `pointer move -<up> <c>` may name: the strip and
/// the message band's rows together stay far below it.
const MAX_CHROME_ROWS: u16 = 16;

/// `pointer [move <r> <c>|click|wheel dy=<n>|leave|status|mods <list>]` -> put the
/// front window's POINTER on a cell (a negative `r` is a chrome row above the
/// grid), click or turn the wheel where it is, or withdraw it, and reply where it
/// actually is.
///
/// This is the verb that makes hover and wheel states drivable: it goes through
/// [`crate::App::pointer_cmd`], whose arms are `on_cursor_moved` / `on_cursor_left`
/// / `on_mouse_wheel_phased` — the functions winit's own
/// `CursorMoved`/`CursorLeft`/`MouseWheel` call. `mouse move` cannot
/// substitute: it posts an engine `InputEvent` and never touches the window pointer
/// state a hover is resolved from.
///
/// `OK at=-` is the honest answer for a window holding no pointer position — after
/// a `leave`, and before any motion has happened. It says only that; the position
/// fields are not printed at all, so there is nothing there to be believed. A held
/// position also says the window pixel it is at (`px=<x>,<y>`), whoever put the
/// pointer there.
pub(crate) fn cmd_pointer(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let action = match parse_pointer(rest) {
        Ok(action) => action,
        Err(usage) => return usage,
    };
    use crate::app_mouse::PointerAt;
    let px_field =
        |px: Option<(i32, i32)>| px.map_or_else(String::new, |(x, y)| format!(" px={x},{y}"));
    match super::control_media::call_main(proxy, |reply| Wake::PointerCmd { action, reply }) {
        Ok(Ok(PointerAt::Grid {
            cell: (row, col),
            px,
        })) => format!("OK at={row},{col}{}\n", px_field(px)),
        Ok(Ok(PointerAt::Native {
            view,
            x,
            y,
            key,
            px,
        })) => format!(
            "OK at=native view={view} x={x:.1} y={y:.1} key={}{}\n",
            key.as_deref().unwrap_or("-"),
            px_field(px)
        ),
        Ok(Ok(PointerAt::Absent)) => "OK at=-\n".to_string(),
        Ok(Err(why)) => format!("ERR {why}\n"),
        Err(error) => format!("ERR pointer command failed: {error}\n"),
    }
}

/// Parse and range-check `resize <r> <c>`: the validated `(rows, cols)` or the reply line.
/// A size outside `1..=MAX_GRID_ROWS`/`MAX_GRID_COLS` is `ERR out of range`, never
/// clamped.
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
///
/// The main thread is the sole geometry owner (`App.rows/cols`, the framebuffer,
/// the window). The verb validates and forwards an `InputEvent::Resize` (in a
/// `Wake::Input`) to the main thread, which applies the term + PTY + window
/// resize and requests a redraw in one owner. A dropped proxy (event loop gone)
/// means the GUI is shutting down: report it.
///
/// RES-1: the verb sets `echo_to_window: true` so the seam also asks the window to
/// match the new grid pixel size (the verb has no window event of its own). The
/// interactive winit `Resized` path uses `echo_to_window: false` (the window is
/// already that size). `echo_to_window` is a transport flag, not a `Source` branch.
pub(crate) fn cmd_resize(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    // `resize px <w> <h>` resizes the window in physical pixels and the grid follows from
    // the platform's `Resized`, as an edge drag does. The cell form applies the grid first
    // and echoes the pixel size after, so the live-resize throttle sees no reflow; the px
    // form is what drives the throttle. Several back to back reproduce a drag's pressure.
    if let Some(event) = parse_resize_px(rest) {
        return match event {
            Err(usage) => usage,
            Ok(event) => match post_input_reply(proxy, vec![event]) {
                Ok(InputOutcome::RangeRejected) => "ERR out of range\n".to_string(),
                Ok(_) => "OK\n".to_string(),
                Err(e) => e,
            },
        };
    }
    // Range-check up front for the precise `ERR out of range`/usage replies; the seam
    // re-clamps and answers `RangeRejected` for anything that slips past.
    let (r, c) = match parse_resize(rest) {
        Ok(rc) => rc,
        Err(e) => return e,
    };
    match post_input_reply(
        proxy,
        vec![InputEvent::Resize {
            rows: r,
            cols: c,
            echo_to_window: true,
        }],
    ) {
        Ok(InputOutcome::RangeRejected) => "ERR out of range\n".to_string(),
        Ok(_) => "OK\n".to_string(),
        Err(e) => e,
    }
}

/// Parse `resize px <w> <h>` (the window form) into its event, or `None` for the cell form
/// ([`parse_resize`]). The px form asks the OS window for a size and touches no engine
/// state; it is spelled here once for its three routers ([`cmd_resize`], the front-routed
/// cross-session twin, the native-front window lane).
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

/// The refusal when the px form targets a session that is not the focused pane
/// of the front window's active tab: `resize px` resizes the front window
/// (`App::input_resize_window_px`), whereas a sibling pane, background tab, or
/// another window has no independent px target (use the cell form to resize a
/// background session's engine + PTY).
pub(crate) const RESIZE_PX_FRONT_ONLY: &str =
    "ERR resize px: only the focused pane of the front window's active tab\n";

/// The reply a cross-session `resize` gives for the px form on a target that is not the
/// front window's focused pane; `None` for the cell form, which the background arm
/// applies. Any px spelling is refused by target before its arguments are read.
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
        // `ERR unknown signal`, which the harness reads as a server without `pid=`,
        // stays the answer for an unknown name.
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

    /// The px form on a target that is not the focused pane of the active tab
    /// is refused by name, and the cell form is untouched.
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

    /// A verb body is fenced via `app_input::press_fence`: no fence for a body
    /// that arms its own licence class at the seam (plain glyphs, plain Enter,
    /// Tab, bare ⌃V) or for a release or bare modifier; a fence keeping the
    /// glow's hand on its row for a line edit (Backspace, arrows, Home/End,
    /// Delete, emacs motions/kills) and a paste; a fence taking it off for
    /// everything else, malformed bodies included. `ctrl <c>` matches `key
    /// ctrl+<c>`. A fenced write always fences, keeping the hand only for line
    /// edits; a read verb never does.
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
        // `ctrl <c>` is `key ctrl+<c>`, whatever the letter.
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
        // A fenced write fences whatever it is, keeping the hand for a line edit.
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
        // The other writers fence taking the hand off; the readers never fence.
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

    /// Options lead, and `id=` and `if=` compose in either order: a body `id=`
    /// is body, `--` ends options, an unkeyed verb is never scanned, `if=` is
    /// recognized on `send`/`key` only, and a guard with no pattern or given
    /// twice is carried as a refusal so the halt gate still speaks first.
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
        // Not the first token: argument data.
        assert_eq!(
            take_leading_options(input_of("send"), "hello id=a:b:c"),
            (opts(None, None), "hello id=a:b:c")
        );
        // A body with no leading option is returned byte for byte, leading whitespace
        // included: `send` bodies are raw.
        assert_eq!(
            take_leading_options(input_of("send"), "  two spaces"),
            (opts(None, None), "  two spaces")
        );
        // `--` is the escape hatch for text that really does start with an option.
        assert_eq!(
            take_leading_options(input_of("send"), "-- if=literal"),
            (opts(None, None), "if=literal")
        );
        // An unkeyed, unguarded verb is never scanned.
        assert_eq!(
            take_leading_options(input_of("paste"), "if=x id=a:b:c hi"),
            (opts(None, None), "if=x id=a:b:c hi")
        );
        // A lone key with no tail is legal (`send id=…` types nothing).
        assert_eq!(
            take_leading_options(input_of("turn"), "id=a:b:c"),
            (opts(Some("a:b:c"), None), "")
        );
        // `if=` alone, and beside `id=` in EITHER order.
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
        // `turn` takes `id=` but NOT `if=`: the guard stays in its tail, where
        // its own option parser answers usage for it.
        assert_eq!(
            take_leading_options(input_of("turn"), "if=x id=a:b:c hello"),
            (opts(None, None), "if=x id=a:b:c hello")
        );
        assert_eq!(
            take_leading_options(input_of("turn"), "id=a:b:c if=x hello"),
            (opts(Some("a:b:c"), None), "if=x hello")
        );
        // A guard with no pattern, or an option given twice, is a CARRIED refusal:
        // the tail is still taken so the dispatch answers it after the halt gate.
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

    /// `signal cont` is a known signal — the remedy the unread-input gate names
    /// for a stopped job — so it reaches the process-group lookup rather than
    /// `ERR unknown signal`.
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

    /// Restart typeahead hazard, end to end: an interactive shell (`zsh -f -i`,
    /// its own session, the pty its controlling tty) runs a job that puts the
    /// tty in raw mode and never reads it. A line is typed into it and sits
    /// unread. `signal term` ends the job and drops the unread input before the
    /// shell takes the terminal back, whereas a bare `killpg` negative control
    /// lets the shell execute the line.
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
        // The exact-pid form (`signal term pid=<n>`) drops the same keys.
        let (ran, reply) = typeahead_after_restart(Remedy::VerbPid, Typed::Direct);
        assert!(!ran, "signal term pid= dropped the line too");
        let reply = reply.expect("the verb's reply");
        assert!(
            reply.starts_with("OK signalled pid ") && reply.ends_with(" discarded=17\n"),
            "{reply}"
        );
    }

    /// Spill behind a full queue: a paste into a frozen raw program fills its
    /// input queue (1022 bytes on Darwin) and the sink spills the rest; the
    /// spill's drainer parks in `poll(POLLOUT)` holding the fd lock. `signal
    /// term` drops the spill along with the kernel queue (counting both, 1118)
    /// so the spilled line never runs and the drainer releases the sink for
    /// subsequent input.
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

    /// How the frozen program is ended.
    #[cfg(target_os = "macos")]
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Remedy {
        /// A bare `killpg(SIGTERM)`: the negative control.
        BareKillpg,
        /// `signal term`, through the verb.
        Verb,
        /// `signal term pid=<the job's leader>`, through the verb.
        VerbPid,
    }

    /// How the typed-ahead line reaches the frozen program.
    #[cfg(target_os = "macos")]
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Typed {
        /// 17 bytes written to the master directly: all in the kernel queue.
        Direct,
        /// 1118 bytes through the SINK: 1022 fill the raw queue, and the tail
        /// — the line — spills behind it with a drainer parked on the queue.
        ThroughASpill,
    }

    /// One run of the hazard: whether the typed line ran in the shell, and the verb's reply
    /// for a [`Remedy`] that goes through it. The marker proving the shell is back goes
    /// through the sink, as a later key would.
    #[cfg(target_os = "macos")]
    fn typeahead_after_restart(remedy: Remedy, typed: Typed) -> (bool, Option<String>) {
        use aterm_uds::spawnfd::InheritNothing as _;
        use std::os::fd::{FromRawFd, OwnedFd};
        use std::os::unix::process::CommandExt;
        use std::time::Duration;
        // Both ends are close-on-exec from birth, so the shell holds its
        // terminal and never a copy of its master.
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: `slave` is a fresh fd this function alone owns.
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let stdio = || std::process::Stdio::from(slave.try_clone().expect("dup slave"));
        // A real interactive shell, so a hermetic one (§8.6 of the init design):
        // a scratch home, and none of the person's start pointers.
        let home = aterm_tempfile::tempdir().expect("scratch home");
        // std's Command on purpose: the PTY child needs `pre_exec` (setsid + TIOCSCTTY), which spawnfd lacks;
        // the strip first, so the shell holds its terminal and nothing else of this process.
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
        /// Reaps the shell however the run ends, so a failed assertion leaves
        /// no interactive shell behind (a job the remedy did not end takes the
        /// SIGHUP the shell's end sends its terminal's foreground group).
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
        // The terminal's side: everything the shell and its jobs draw, read
        // as it comes — a pty's output queue is small, and a shell that
        // cannot draw its prompt reads nothing either.
        let seen = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let drawer = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Acquire) {
                    let mut buf = [0u8; 4096];
                    // SAFETY: a bounded read into a live stack buffer from the
                    // master, which outlives this thread (joined before close).
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
        // What the shell draws must come: a hang detector, not a latency budget.
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

        // The frozen program: raw mode, never reading, and parked past the
        // marker's hang detector (`PARKED_SECS`), so the shell's marker below
        // is the remedy's doing. A `sleep 30` here ended by itself inside that
        // minute, and a remedy that sent no signal passed. Typed once the
        // shell is at its prompt (bracketed paste on is its line editor
        // starting).
        wait_for("the prompt", &|| drawn("\x1b[?2004h"));
        type_in(
            format!(
                "(stty raw -echo; exec sleep {})\r",
                aterm_spec::wait::PARKED_SECS
            )
            .as_bytes(),
        );
        // The JOB's raw mode, not the line editor's: ISIG off (the editor
        // keeps it on), and the job in the foreground, not the shell.
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
                // The line typed into it, unread (17 bytes).
                type_in(b"echo TYPE''AHEAD\r");
                assert_eq!(aterm_pty::input_queue_len(master), Some(17));
            }
            Typed::ThroughASpill => {
                // A paste of a long comment line, then the line: 1118 bytes.
                let mut paste = vec![b'#'; 1100];
                paste.extend_from_slice(b"\recho TYPE''AHEAD\r");
                assert_eq!(paste.len(), 1118);
                assert_eq!(sink.write_frame_nonparking(&paste).expect("paste"), 1118);
                // The drainer takes the spill's mutex as it starts, and a
                // reading that meets it busy leaves the spill uncounted.
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
                // `quiet=` refuses while written input sits unread or a person's key is
                // inside the grace, sending and dropping nothing; a stamp past the grace
                // holds nothing of its own.
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
        // A marker typed after the remedy, through the sink: the shell is
        // back and reading, and the sink still reaches it.
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

    /// The App lane's `unread=ok`: a leading token on `hwkey`, `pointer` and
    /// `invoke` only, `ok` its only value; anything else leaves the line
    /// untouched for the arm to parse.
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
        // Not leading, another value, another verb: the line is untouched.
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

    /// `unread=ok` is the one override of the unread-input gate: taken off
    /// `send`/`key` only, in any order beside the other options; `ok` is its
    /// only value; a bad value or a second one is a carried refusal (the halt
    /// gate still speaks first); and `send -- unread=ok` types the text.
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
        // Given twice, or with any value but `ok`: refused, never typed.
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
        // `--` ends options: the text is sent literally and the gate stands.
        let (o, tail) = take_leading_options(input_of("send"), "-- unread=ok");
        assert!(!o.unread_ok);
        assert_eq!(tail, "unread=ok");
        // A body token is body.
        let (o, tail) = take_leading_options(input_of("send"), "hello unread=ok");
        assert!(!o.unread_ok);
        assert_eq!(tail, "hello unread=ok");
        // `turn`, `paste`, `feed` take no override: their tail is untouched.
        for verb in ["turn", "paste", "feed", "ctrl"] {
            let (o, tail) = take_leading_options(input_of(verb), "unread=ok x");
            assert!(!o.unread_ok, "{verb}");
            assert_eq!(tail, "unread=ok x", "{verb}");
        }
    }

    /// `if-supervisor=<holder>` leads on `key`, `send` and `turn` beside the
    /// other options, and is the ONE leading option `paste` takes (its body is
    /// raw: the one space after it goes, nothing else). A value that is no
    /// holder token, or a second one, is a carried refusal — never typed.
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
        // NEGATIVE CONTROLS: paste takes no other option, and an option that
        // does not lead is text.
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

    /// `as=person` (the development seam) is read on `key` and `send` only,
    /// spelled whole; it takes no fence, `id=`, `act=` or `cause=`; given twice
    /// it is refused; and a build without the seam refuses it outright, so a
    /// shipped aterm never takes a controller's write for a person's.
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
        // A build without the seam refuses it, and says why.
        let mut off = LeadingInputOptions::default();
        take_as_person(&mut off, false);
        assert!(!off.as_person);
        assert_eq!(off.refusal.as_deref(), Some(PERSON_SEAM_REFUSED));
        // NEGATIVE CONTROL: any other `as=` word is the text's, an unguarded verb
        // keeps it as body, and a body token spelled like it is still body.
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

    /// `if-human=<n>` parses to the person count `status human_seq=` prints,
    /// composes with the other fences on `key` and `send`, and refuses any
    /// other shape — never typed into the program as text.
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
        // NEGATIVE CONTROL: a verb that is not guarded keeps it as body, and a
        // body token spelled like it is still body.
        assert_eq!(
            take_leading_options(input_of("paste"), "if-human=1 x").1,
            "if-human=1 x"
        );
        assert_eq!(
            take_leading_options(input_of("send"), "hello if-human=1").1,
            "hello if-human=1"
        );
        // The fence alone routes to the fenced arm, and the usage line every
        // malformed `key` answers names it beside the other leading options.
        assert!(
            InputFence {
                human: Some(1),
                ..InputFence::default()
            }
            .is_armed()
        );
        assert!(KEY_USAGE.contains("[if-human=<n>]"), "{KEY_USAGE}");
    }

    /// `if-gen=<epoch>.<seq>` parses to the generation `status gen=` prints, composes with
    /// the other options, and refuses any other shape. A leading `if-seq=` is taken and
    /// refused, on `send` too, never typed as text.
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
        // NEGATIVE CONTROL: a body token spelled like it is still body.
        assert_eq!(
            take_leading_options(input_of("send"), "hello if-seq=15").1,
            "hello if-seq=15"
        );
    }

    /// A bad guard is `ERR badregex` — the `await match` spelling, from the same
    /// bounded compiler — and no guard is no guard.
    #[test]
    fn a_guard_compiles_outside_the_lock_and_a_bad_one_is_badregex() {
        assert!(compile_guard(None).unwrap().is_none());
        assert!(compile_guard(Some("Do.you.want")).unwrap().is_some());
        assert_eq!(compile_guard(Some("(")).unwrap_err(), "ERR badregex\n");
    }

    /// With no current match there is no row, column or length, and the
    /// formatter prints `-` (the fields come from `Option`s).
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
        // A present match is reported 1-based, so the pin above is about absence. The
        // fixture is reachable (`end_col` inclusive, not below the start), so a formatter
        // that read the third field as a length would fail.
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

    /// A count the terminal has moved past is still reported, marked `stale=1` as the bar
    /// marks it.
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

    /// A closed find bar says exactly that and nothing else: no query, no counters,
    /// no coordinates from the zero-valued struct behind them.
    #[test]
    fn a_closed_find_bar_answers_only_that_it_is_closed() {
        assert_eq!(
            find_status_line(&crate::app_search::FindStatus::CLOSED),
            "OK open=0\n"
        );
    }

    /// A find query may begin or end with a space, and `type` keeps it: the payload
    /// is the line after the ONE separating space, not a whitespace-split rejoin.
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

    /// A bare form refuses a trailing argument instead of ignoring it.
    #[test]
    fn a_bare_find_form_refuses_an_argument_it_cannot_honour() {
        for line in ["next 3", "prev 2", "open now", "cancel please", "case on"] {
            assert!(
                parse_find(line).is_err(),
                "find {line:?} must not be accepted"
            );
        }
        // …and the bare forms still parse.
        use crate::app_search::FindAction;
        assert_eq!(parse_find("next"), Ok(FindAction::Step { forward: true }));
        assert_eq!(parse_find("prev"), Ok(FindAction::Step { forward: false }));
        assert_eq!(parse_find("case"), Ok(FindAction::ToggleCase));
        assert_eq!(parse_find(""), Ok(FindAction::Status));
    }

    /// `pointer move` refuses a cell outside the addressable grid rather than clamping it.
    #[test]
    fn pointer_move_refuses_a_cell_it_cannot_address() {
        use crate::app_mouse::PointerAction;
        assert_eq!(
            parse_pointer("move 3 9"),
            Ok(PointerAction::Move { row: 3, col: 9 })
        );
        assert_eq!(parse_pointer("leave"), Ok(PointerAction::Leave));
        // A native view's control or point (cs-25).
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
        // A negative row names a chrome row above the grid (the message band),
        // and `click` presses where the pointer is.
        assert_eq!(
            parse_pointer("move -1 5"),
            Ok(PointerAction::MoveChrome { up: 1, col: 5 })
        );
        assert_eq!(
            parse_pointer(&format!("move -{MAX_CHROME_ROWS} 0")),
            Ok(PointerAction::MoveChrome {
                up: MAX_CHROME_ROWS,
                col: 0
            })
        );
        assert_eq!(
            parse_pointer(&format!("move -{} 0", MAX_CHROME_ROWS + 1)),
            Err("ERR out of range\n".to_string())
        );
        assert_eq!(parse_pointer("click"), Ok(PointerAction::Click));
        assert!(parse_pointer("click right").is_err());
    }

    /// `pointer wheel` takes its deltas, unit and aim as named fields in any
    /// order, each once; `dy` down, lines unless `unit=pt`. A wheel that turns
    /// nothing, a field twice, a delta past the cap or a stray word is refused,
    /// never read as some other wheel.
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

    /// `lines=N` reaches `InputEvent::Wheel.lines`: absent it is 1, and it is clamped to
    /// `1..=MAX_WHEEL_LINES`, a non-positive or non-numeric value falling back to 1.
    #[test]
    fn parse_mouse_wheel_lines() {
        let Ok(InputEvent::Wheel { lines, dir, .. }) = parse_mouse("wheeldown left 2 4 lines=8")
        else {
            panic!("wheeldown lines=8 parses");
        };
        assert_eq!(lines, 8);
        assert_eq!(dir, aterm_types::mouse::WheelDir::Down);
        // No token: one notch.
        let Ok(InputEvent::Wheel { lines, .. }) = parse_mouse("wheelup left 2 4") else {
            panic!("bare wheelup parses");
        };
        assert_eq!(lines, 1);
        // Over-cap clamps down.
        let Ok(InputEvent::Wheel { lines, .. }) = parse_mouse("wheelup left 0 0 lines=100000")
        else {
            panic!("over-cap parses");
        };
        assert_eq!(lines, MAX_WHEEL_LINES);
        // Non-positive and non-numeric both fall back to 1.
        for line in ["wheelup left 0 0 lines=0", "wheelup left 0 0 lines=abc"] {
            let Ok(InputEvent::Wheel { lines, .. }) = parse_mouse(line) else {
                panic!("{line} parses");
            };
            assert_eq!(lines, 1, "{line}");
        }
    }

    /// The four wheel actions each parse to their own [`WheelDir`], and the thumb
    /// buttons name themselves.
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
        // An unnamed device button is still rejected — no bogus report.
        assert!(parse_mouse("press thumb3 2 4").is_err());
    }
}

#[cfg(test)]
mod video_keys_ledger_tests {
    //! What the active-tab `send`/`feed`/`key` verbs build must reach the opt-in
    //! `video … keys` ledger, and the one path that structurally cannot be
    //! logged must be counted instead of silently dropped.

    use super::{feed_bytes, parse_key, send_bytes};
    use crate::input::InputEvent;
    use crate::{VideoInputSample, record_controller_video_attempts, video_sample_for_key};
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    /// Flagless `send` and `feed` are active-tab verbs delivered through the App
    /// seam as `InputEvent::KeySequence`, so `video … keys` logs them.
    #[test]
    fn video_keys_ledger_logs_the_send_and_feed_verbs() {
        // `send 'ls\n'`, the documented submit form: the literal `\n` becomes CR.
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

        // `feed 1b5b41` — ESC [ A, the escape hatch for the control bytes
        // `send` cannot carry.
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

        // Non-UTF-8 is still an attempt, never a panic or a drop: one replacement per
        // maximal invalid subpart, as from_utf8_lossy does.
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

        // The cap still truncates a large paste-shaped sequence exactly.
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

    /// Named keys (`esc`, arrows, F-keys) are recorded as samples, the three
    /// named keys with an unambiguous character keep their character
    /// representation, and bare modifiers are excluded.
    #[test]
    fn video_keys_ledger_names_keys_that_have_no_character() {
        for (verb, expect) in [
            ("up", VideoInputSample::Named(NamedKey::ArrowUp)),
            ("esc", VideoInputSample::Named(NamedKey::Escape)),
            ("backspace", VideoInputSample::Named(NamedKey::Backspace)),
            ("f1", VideoInputSample::Named(NamedKey::F1)),
            ("pagedown", VideoInputSample::Named(NamedKey::PageDown)),
            // The three with an unambiguous character keep it.
            ("enter", VideoInputSample::Char('\n')),
            ("tab", VideoInputSample::Char('\t')),
            ("space", VideoInputSample::Char(' ')),
        ] {
            let ev = parse_key(verb).unwrap_or_else(|| panic!("`key {verb}` parses"));
            let mut log = Vec::new();
            let _ = record_controller_video_attempts(&mut log, &ev, 7, 64);
            assert_eq!(log, vec![(7, expect)], "`key {verb}` must be an attempt");
        }

        // A bare modifier is not an attempt.
        assert_eq!(
            video_sample_for_key(&Key::Named(NamedKey::ShiftLeft)),
            None,
            "a bare Shift expresses no input intent"
        );
        assert_eq!(video_sample_for_key(&Key::Named(NamedKey::CapsLock)), None);

        // index.json shape: `ch` for characters, `key` for named, control characters
        // escaped.
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

    /// `unlogged_inputs` counts attempts the ledger would have recorded, so an event it
    /// never records (a cross-session `focus`, a bare modifier) adds nothing and
    /// `index.json` announces no measurement gap for it. Asserted on the pure classifier:
    /// the process-wide counter is shared with every test in this binary.
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

        // A real attempt on the same arm still counts: a classifier, not an off switch.
        assert_eq!(
            crate::video_attempt_count(&InputEvent::Text("ab".into())),
            2,
            "a cross-session paste of two chars IS two attempts this ledger missed"
        );

        // Raw bytes count per attempt, and an empty payload (a zero-length
        // `feed`) is not an attempt at all.
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
