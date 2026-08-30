// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Input-injection verbs + their parsers — the control protocol's "drive the
//! shell" surface: key/ctrl/send/feed/signal/mouse/paste/focus/resize/scroll/tab.
//! Moved verbatim from `control.rs` (behavior-preserving). The seam plumbing
//! (`post_input_reply`) and the cross-session `apply_scroll_intent` cluster stay
//! in `control.rs`; this module reaches them via `super::`.

use std::sync::{Arc, Mutex};

use aterm_core::grid::{MAX_GRID_COLS, MAX_GRID_ROWS};
use aterm_core::terminal::Terminal;
use aterm_session::Op;
use aterm_session::sink::SinkWriter;
use winit::event_loop::EventLoopProxy;

use super::post_input_reply;
use crate::input::{InputEvent, InputOutcome, ScrollIntent};
use crate::{TabAction, Wake, term_lock};

/// `scroll <up|down|top|bottom|N>` -> move the scrollback viewport and report
/// the new position as `OK <display_offset> <scrollback_lines>\n`. `up`/`down`
/// move one screen into/out of history; `top`/`bottom` jump; a signed integer
/// `N` moves N lines into history (negative = toward the live bottom). With no
/// argument it just reports the current position. After moving it nudges a
/// windowed session to repaint (no-op when headless).
pub(crate) fn cmd_scroll(
    term: &Arc<Mutex<Terminal>>,
    proxy: &EventLoopProxy<Wake>,
    rest: &str,
) -> String {
    // Parse to a tracking-agnostic ScrollIntent; the SEAM is the sole
    // `scroll_display`/`scroll_to_*` caller. `""` (just report position) maps to a
    // zero-line `By(0)` so the round-trip still reports the current offset.
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
    // Reply-bearing: the reply is sent AFTER the seam applied the scroll on the
    // main thread, so the position read below is NOT racy with the apply.
    // `scroll` is read-side view control (display_offset only) — audit class ReadScreen.
    match post_input_reply(proxy, Op::ReadScreen, vec![InputEvent::ScrollView(intent)]) {
        Ok(_) => {}
        Err(e) => return e,
    }
    let t = term_lock(term);
    let offset = t.grid().display_offset();
    let max = t.grid().scrollback_lines();
    format!("OK {offset} {max}\n")
}

/// `send <text>` -> write `<text>` to the PTY. The submit form on the line-framed
/// control protocol is the two-character literal `\n` (a backslash then `n`, e.g.
/// `aterm-ctl send 'ls\n'`): it is normalized to a single carriage-return `0x0d` —
/// the byte the Return key sends — so the shell's line editor runs the command. A
/// *real* trailing LF can't reach here over the socket (the request line is itself
/// `\n`-terminated, so the framing consumes it before `rest` is parsed); the
/// `strip_suffix('\n')` arm below is therefore defensive normalization for any
/// in-process caller, not the socket submit path. Only the trailing newline is
/// converted; embedded newlines in the body are passed through unchanged.
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

pub(crate) fn cmd_send(sink: &SinkWriter, rest: &str) -> String {
    let bytes = send_bytes(rest);
    write_pty(sink, &bytes);
    "OK\n".to_string()
}

/// Parse the optional trailing `mods=<list>` token (e.g. `mods=ctrl+shift`),
/// returning the modifier mask and the rest of the line with the token removed.
/// Additive: a verb line WITHOUT `mods=` parses to `Modifiers::empty()` and the
/// untouched line, so every existing caller stays byte-compatible.
pub(crate) fn take_mods(rest: &str) -> (aterm_types::keyboard::Modifiers, String) {
    use aterm_types::keyboard::Modifiers;
    let mut m = Modifiers::empty();
    let mut kept: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(list) = tok.strip_prefix("mods=") {
            for name in list.split(['+', ',']) {
                match name {
                    "shift" => m |= Modifiers::SHIFT,
                    "ctrl" | "control" => m |= Modifiers::CTRL,
                    "alt" | "option" => m |= Modifiers::ALT,
                    // `meta` is its OWN modifier (Kitty CSI-u bit 8), distinct from
                    // ALT — a controller can now drive a real Meta chord. Legacy /
                    // xterm encoders ignore META/HYPER so their bytes are unchanged;
                    // only the Kitty keyboard protocol gains the extra bit.
                    "meta" => m |= Modifiers::META,
                    "hyper" => m |= Modifiers::HYPER,
                    "super" | "cmd" | "command" => m |= Modifiers::SUPER,
                    _ => {}
                }
            }
        } else {
            kept.push(tok);
        }
    }
    (m, kept.join(" "))
}

/// Parse the optional trailing `type=<press|repeat|release>` token, returning the
/// event type (default `Press`) and the body with the token removed. ADDITIVE: a
/// line without `type=` yields `Press` and the untouched body. An unrecognized
/// value yields `None` so [`parse_key`] rejects the whole line rather than
/// silently defaulting. `down`/`up` are accepted aliases for `press`/`release`.
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

/// Parse the optional trailing `base=<char>` token — the US-QWERTY base-layout
/// key fed to Kitty `REPORT_ALTERNATE_KEYS` (the 3rd CSI-u sub-field), so a
/// controller on a non-US layout can reproduce the byte a human on that layout
/// emits. ADDITIVE: no `base=` yields `None` (the existing behaviour). A `base=`
/// whose value is not exactly one char yields the parser `None`.
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
/// Covers the FULL `NamedKey` vocabulary the engine models — navigation, editing,
/// locks/system, F1–F35, numpad, modifier-side keys, and media/audio — so every
/// physical key a human can press is reachable by a controller. The original 25
/// tokens keep their exact spelling for byte-compatibility.
fn named_key_from_token(body: &str) -> Option<aterm_types::keyboard::NamedKey> {
    use aterm_types::keyboard::NamedKey as Nk;
    Some(match body {
        // --- original 25 (byte-identical spellings) ---
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
/// [`InputEvent::Key`]. Factored out of [`cmd_key`] so the additive grammar is
/// unit-testable WITHOUT an `EventLoopProxy` (the verb can't run headless — it
/// posts a `Wake::Input`). The SAME (Key, mods, base_layout, event_type) tuple a
/// human's named-key press builds, so the seam (the sole encoder caller) yields
/// byte-identical output incl. Kitty REPORT_ALTERNATE_KEYS. All trailing tokens
/// are ADDITIVE — a bare `key up` still parses to empty mods / Press / no base.
/// Returns `None` for an unknown key name or a malformed `type=`/`base=` value.
pub(crate) fn parse_key(rest: &str) -> Option<InputEvent> {
    use aterm_types::keyboard::Key;
    let (mut mods, body) = take_mods(rest);
    let (event_type, body) = take_event_type(&body)?;
    let (base_explicit, body) = take_base_layout(&body)?;
    // Inline modifier prefixes: `ctrl+u`, `alt+x`, `ctrl+shift+a`, ... The
    // prefixes are ADDITIVE with any trailing `mods=` token, so `ctrl+u` and
    // `u mods=ctrl` agree. After stripping them, a single residual character
    // (e.g. `u`) becomes a `Key::Character` event — the SAME (Key, mods) seam
    // `parse_ctrl` builds, so the encoder derives the control byte itself
    // (`ctrl+u` -> 0x15) rather than us hand-rolling it.
    let (prefix_mods, body) = take_inline_mods(body.trim());
    mods |= prefix_mods;
    let body = body.trim();
    let Some(named) = named_key_from_token(body) else {
        // Not a named key: a single residual character (after stripping inline
        // modifier prefixes) becomes a `Key::Character`. `ctrl+u` lands here as
        // `u` + CTRL, byte-identical to `parse_ctrl("u")` -> the encoder emits
        // 0x15. Lower-cased so `ctrl+U` == `ctrl+u`, matching `parse_ctrl`.
        let mut chars = body.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) => Some(InputEvent::Key {
                key: Key::Character(c.to_ascii_lowercase()),
                mods,
                base_layout: base_explicit,
                event_type,
            }),
            _ => None,
        };
    };
    Some(InputEvent::Key {
        key: Key::Named(named),
        mods,
        base_layout: base_explicit,
        event_type,
    })
}

/// Strip leading inline modifier prefixes (`ctrl+`, `alt+`, `shift+`, `super+`
/// and their aliases) from a `key` body, returning the accumulated modifier
/// mask and the remaining body. Mirrors the `mods=` alias table in
/// [`take_mods`] so `ctrl+u` and `u mods=ctrl` are equivalent. Only consumes a
/// prefix when a `+` follows a recognized modifier name, so a bare named key
/// like `up` (no `+`) is returned untouched.
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

/// Whether a well-formed `key` verb body is a PLAIN TYPED GLYPH press —
/// a Character/Space press with no Ctrl/Alt/Super — i.e. the same class the
/// input seams' mash exception leaves alone. Used by the control dispatcher to
/// skip the pre-parse licence fence for exactly this class: the injected key
/// stamps its own typed licence a moment later, and pre-clearing on every key
/// wiped the banked stamps of keys whose echoes were still in flight — the
/// flood-typing black gap, alive on the control path after the physical press
/// paths were fixed. Anything malformed, modified, named (Enter/Tab/nav), or a
/// release answers `false` and keeps the fence.
pub(crate) fn key_is_plain_typed_glyph(rest: &str) -> bool {
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    match parse_key(rest) {
        Some(InputEvent::Key {
            key,
            mods,
            event_type: KeyEventType::Press,
            ..
        }) => {
            !mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER)
                && match key {
                    Key::Character(c) => aterm_grapheme::char_width(c) > 0,
                    Key::Named(NamedKey::Space) => true,
                    _ => false,
                }
        }
        _ => false,
    }
}

/// `key <name> [mods=<list>]` -> build an [`InputEvent::Key`] and post it to the
/// seam (the SOLE encoder caller, under the CURRENT keyboard mode). See
/// [`parse_key`] for the grammar.
pub(crate) fn cmd_key(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    match parse_key(rest) {
        // Reply-bearing: OK means the seam APPLIED the event (bytes written),
        // not merely that it was enqueued. With no frontmost window the seam
        // drops the reply sender, so the caller gets ERR rather than a false OK.
        Some(ev) => input_reply_to_str(post_input_reply(proxy, Op::WriteInput, vec![ev])),
        None => "ERR usage: key <name> — enter tab esc space backspace delete \
                 up down left right home end pageup pagedown f1..f12 (opt +mods, e.g. ctrl+c)\n"
            .to_string(),
    }
}

/// `hwkey <char|name> [mods=…] [count=…] [interval=…]` -> inject the key through
/// the OS EVENT QUEUE instead of the control seam.
///
/// The deliberately different spelling from [`cmd_key`] is the point: the two
/// verbs are not interchangeable and must never be confused in a measurement.
/// `key` posts a decoded `InputEvent` to the main thread over a `CFRunLoopSource`
/// and the arrival is stamped inside the handler, so the key is BORN already
/// dequeued — no socket-injected key has ever measured OS-level key queueing.
/// `hwkey` builds a real `NSEvent` and posts it into this application's own event
/// queue, from THIS (control) thread, so it is dequeued, routed and translated by
/// the same code a physical keypress runs, including the
/// `note_key_arrival_queued` backdate that prices a parked event loop.
///
/// Reply honesty: `OK posted=<n>` means `n` press events were handed to the OS
/// event queue, NOT that any of them reached the PTY. That is a weaker claim than
/// `key`'s `OK` (which means the seam applied the event), and it is the strongest
/// claim this path can truthfully make — the whole point is that delivery is the
/// OS's business from the post onward. Read the result through `metrics`
/// (`n_key_write` only ever moves on the hardware path) or `text`.
pub(crate) fn cmd_hwkey(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let spec = match crate::hwkey::parse_hwkey(rest) {
        Ok(s) => s,
        Err(e) => return e,
    };
    // ONE main-thread round trip for the window number, then nothing else touches
    // the main thread: the posting loop must be able to run — and the posted keys
    // must be able to wait — while that thread is parked inside `nextDrawable`.
    let window_number = match crate::control::control_media::call_main(proxy, |reply| {
        Wake::HwKeyTarget { reply }
    }) {
        Ok(n) => n,
        Err(e) => return format!("ERR hwkey: {e}\n"),
    };
    match crate::hwkey::post(&spec, window_number) {
        Ok(n) => format!("OK posted={n}\n"),
        Err(e) => e,
    }
}

/// Map a reply-bearing input outcome to a verb reply line. `Ok` (applied) and
/// `RangeRejected` (out-of-range geometry — not relevant to key/mouse/paste, but
/// handled for completeness) become OK / ERR; an `Err` (event loop closed / no
/// window) is already a full `ERR …\n` string.
pub(super) fn input_reply_to_str(r: Result<InputOutcome, String>) -> String {
    match r {
        Ok(InputOutcome::Ok) => "OK\n".to_string(),
        Ok(InputOutcome::RangeRejected) => "ERR out of range\n".to_string(),
        Ok(InputOutcome::WriteFailed) => "ERR write failed\n".to_string(),
        Err(e) => e,
    }
}

/// PURE parser for `ctrl <letter>` -> a Control-modified character key. Factored
/// out of [`cmd_ctrl`] for headless unit-testing. The seam encodes it (under the
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
        event_type: aterm_types::keyboard::KeyEventType::Press,
    })
}

/// `ctrl <letter>` -> a Control-modified character key posted to the seam. See
/// [`parse_ctrl`]. Reply-bearing like `key` (not fire-and-forget): `OK` means the
/// seam APPLIED the event; with no frontmost window the dropped reply sender
/// yields `ERR`, so `ctrl c` never reports a false `OK` for an event that went
/// nowhere — the same honesty the byte-identical `key ctrl+c` already had.
pub(crate) fn cmd_ctrl(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    match parse_ctrl(rest) {
        Some(ev) => input_reply_to_str(post_input_reply(proxy, Op::WriteInput, vec![ev])),
        None => "ERR usage: ctrl <single-letter>\n".to_string(),
    }
}

/// `feed <hex>` -> write raw bytes (decoded from a hex string, whitespace
/// allowed) straight to the PTY. The escape hatch for control/binary bytes the
/// line-delimited `send` verb can't carry: `feed 03` = Ctrl-C, `feed 1b5b41` =
/// ESC[A, `feed 0a` = a real newline. Replies `OK <n> bytes\n` or `ERR bad hex`.
pub(crate) fn feed_bytes(rest: &str) -> Result<Vec<u8>, &'static str> {
    let hex: String = rest.chars().filter(|c| !c.is_whitespace()).collect();
    if !hex.len().is_multiple_of(2) {
        return Err("ERR bad hex (odd length)\n");
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let h = hex.as_bytes();
    let mut i = 0;
    while i < h.len() {
        let hi = (h[i] as char).to_digit(16);
        let lo = (h[i + 1] as char).to_digit(16);
        let (Some(hi), Some(lo)) = (hi, lo) else {
            return Err("ERR bad hex\n");
        };
        bytes.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Ok(bytes)
}

pub(crate) fn cmd_feed(sink: &SinkWriter, rest: &str) -> String {
    let bytes = match feed_bytes(rest) {
        Ok(bytes) => bytes,
        Err(error) => return error.to_string(),
    };
    let n = bytes.len();
    write_pty(sink, &bytes);
    format!("OK {n} bytes\n")
}

/// `signal <name>` -> deliver a job-control signal to the PTY's CURRENT
/// foreground process group (via `tcgetpgrp` on the master + `killpg`).
/// `name` is one of `int`/`c`, `quit`, `tstp`/`z`, `hup`, `term`, `kill`.
/// This makes Ctrl-C/Ctrl-\\/Ctrl-Z effects deliverable and testable regardless
/// of the line discipline / launch context (which may not generate them).
/// POSIX-only: on Windows (no process groups / killpg for a ConPTY) the verb
/// stays in the table but honestly replies
/// `ERR signal unsupported on this platform`.
#[cfg(unix)]
pub(crate) fn cmd_signal(master: i32, rest: &str) -> String {
    let sig = match rest.trim() {
        "int" | "c" | "sigint" => libc::SIGINT,
        "quit" | "sigquit" => libc::SIGQUIT,
        "tstp" | "z" | "sigtstp" => libc::SIGTSTP,
        "hup" | "sighup" => libc::SIGHUP,
        "term" | "sigterm" => libc::SIGTERM,
        "kill" | "sigkill" => libc::SIGKILL,
        other => return format!("ERR unknown signal: {other}\n"),
    };
    let pgrp = unsafe { libc::tcgetpgrp(master) };
    if pgrp <= 0 {
        return "ERR no foreground process group\n".to_string();
    }
    let rc = unsafe { libc::killpg(pgrp, sig) };
    if rc == 0 {
        format!("OK signalled pgrp {pgrp}\n")
    } else {
        "ERR killpg failed\n".to_string()
    }
}

/// Windows arm of the `signal` verb: kept in the verb table so the surface is
/// total, but job-control signals have no ConPTY analogue — reply with an
/// honest error (callers wanting Ctrl-C semantics can `feed 03`, which the
/// ConPTY host cooks into a console Ctrl-C for the foreground app).
#[cfg(windows)]
pub(crate) fn cmd_signal(master: i32, rest: &str) -> String {
    let _ = (master, rest);
    "ERR signal unsupported on this platform\n".to_string()
}

const MOUSE_USAGE: &str = "ERR usage: mouse <press|release|move|wheelup|wheeldown|wheelleft|wheelright> <left|middle|right|back|forward> <row> <col> [mods=..] [count=N] [side=left|right] [block=0|1] [lines=N]\n";

/// Upper bound on a single `mouse wheelup|wheeldown` verb's `lines=N`. The seam
/// emits ONE wheel report per line under a tracking app, so an unbounded count
/// would let one verb line flood the PTY; 512 covers a large flick (many screens)
/// while keeping the burst bounded. The seam separately clamps `lines >= 1`.
///
/// Defined as the seam's own [`crate::input::MAX_WHEEL_BURST`] rather than a second
/// 512: the seam has a SECOND per-event burst to bound now (the DEC-1007 alt-scroll
/// arrows, whose count is the platform's lines-per-detent and so is no longer
/// bounded by `lines` alone), and the two ceilings must not drift apart.
const MAX_WHEEL_LINES: i32 = crate::input::MAX_WHEEL_BURST;

/// PURE parser for the `mouse` verb -> an engine-neutral mouse [`InputEvent`].
/// Factored out of [`cmd_mouse`] so the additive `mods=`/`count=`/`side=`/`block=`
/// grammar is unit-testable without an `EventLoopProxy`. Returns `Err(usage/err
/// string)` for a malformed line, `Ok(event)` otherwise.
///
/// Grammar: `mouse <action> <button> <row> <col> [mods=..] [count=N]
/// [side=left|right] [block=0|1] [lines=N]`. `action` is `press|release|move|
/// wheelup|wheeldown|wheelleft|wheelright`; `button` is
/// `left|middle|right|back|forward` (ignored for the wheel actions); `row`/`col`
/// are 0-based. The horizontal wheel pair and the thumb buttons (audits I7/I8)
/// exist here for the same reason the rest do: a controller must be able to
/// drive every gesture a hand can, or the seam's source-blindness is only a
/// claim.
///
/// The additive tokens carry the data that closes the human/controller
/// divergences: `mods=` the real modifier mask (kills a), `count=` the click
/// depth 1..=3 (kills b), `side=` the cell-half (kills i), `block=1` the
/// rectangular-selection intent for a single-click press (the same intent a human
/// encodes from a held Alt, carried as DATA so the seam never reads ambient
/// modifier state), `lines=N` the wheel-notch count for the wheel actions
/// (default 1, clamped to `1..=MAX_WHEEL_LINES`) so one verb line scrolls N lines
/// the way a human's single trackpad flick banks N — instead of one socket round
/// trip per notch. `lines=` is ignored by the non-wheel actions.
pub(crate) fn parse_mouse(rest: &str) -> Result<InputEvent, String> {
    use aterm_core::selection::SelectionSide;
    use aterm_types::mouse::{ALT_MASK, CTRL_MASK, MouseButton, SHIFT_MASK, WheelDir};
    let mut action = "";
    let mut mods: u8 = 0;
    let mut click_count: u8 = 1;
    let mut side = SelectionSide::Left;
    let mut block = false;
    let mut wheel_lines: i32 = 1;
    // POSITIONAL tokens (in order), interpreted per-action below: this keeps
    // press/release/wheel as `<button> <row> <col>` (byte-compatible with the
    // pre-Phase-0.5 grammar) AND lets `move` be EITHER `<row> <col>` (no-button
    // hover, code 3) OR `<button> <row> <col>` (held-button drag).
    let mut positional: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(list) = tok.strip_prefix("mods=") {
            for name in list.split(['+', ',']) {
                match name {
                    "shift" => mods |= SHIFT_MASK,
                    "alt" | "option" | "meta" => mods |= ALT_MASK,
                    "ctrl" | "control" => mods |= CTRL_MASK,
                    _ => {}
                }
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
            // Wheel-notch count for wheelup/wheeldown. A non-positive or
            // non-numeric value falls back to 1; the magnitude is clamped so one
            // verb line cannot flood the PTY with reports.
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
        // `move` with two positionals = no-button hover (code 3); with three =
        // `<button> <row> <col>` held-button drag (kills divergence c at the verb).
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
            // `button` is ignored for the wheel actions but still required as a
            // positional (byte-compatible with the old `<button> <row> <col>` form).
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
                    // policy is a control-authority decision applied by cmd_mouse
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
                // The wheel's four directions share one arm: only `dir` differs,
                // and the `_` catch-all is `wheeldown` (the outer match already
                // fenced the action set, so no other string can reach here).
                wheel => InputEvent::Wheel {
                    dir: match wheel {
                        "wheelup" => WheelDir::Up,
                        "wheelleft" => WheelDir::Left,
                        "wheelright" => WheelDir::Right,
                        _ => WheelDir::Down,
                    },
                    lines: wheel_lines,
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
/// [block=0|1]` -> BUILD an engine-neutral mouse [`InputEvent`] (via [`parse_mouse`])
/// and post it to the seam, which reads the CURRENT mouse mode ONCE and either
/// emits the `encode_mouse_*` report (tracking ON) or runs the local selection
/// gesture (tracking OFF).
///
/// Phase 0.5 CONTRACT CHANGE (divergences a/b/d/i): the old `OK (mouse off)`
/// short-circuit is GONE — a tracking-OFF press/release now runs the SAME
/// selection machinery as the human (not a no-op), and `mods`/`count`/`side`/
/// `block` are carried as data instead of hard-coded. The verb returns `OK\n`
/// (fire-and-forget) once the batch is posted.
///
/// DRAG CONVERGENCE (divergence c) — SCOPE: one `mouse move` verb line posts ONE
/// `MouseMove`, so a controller that wants intermediate motion reports under a
/// tracking app issues a `press` then N `move`s then a `release` as separate verb
/// lines (the seam reports each, identical to the human's per-pixel `MouseMove`s).
/// A single-line `press→N×move→release` BATCH grammar is deliberately deferred —
/// the seam already supports a batched `Wake::Input` (A.2.3), so it is additive
/// and out of scope for this convergence commit.
pub(crate) fn cmd_mouse(proxy: &EventLoopProxy<Wake>, scope: super::Scope, rest: &str) -> String {
    match parse_mouse(rest) {
        // Reply-bearing: OK means the seam ran (report emitted or local fallback
        // applied), not merely enqueued. No window ⇒ ERR, not a false OK.
        //
        // COPY-ON-SELECT FENCE: a scoped (non-Owner) WriteInput edge can inject a
        // select+release gesture that, on the active tab, would auto-copy the
        // selected (attacker-chosen or on-screen) text to the SYSTEM clipboard —
        // exfil past the `copy` → `ClipboardWrite` fence. We stamp the policy on the
        // event HERE (the control-authority layer, the only place `Scope` is known)
        // so `finish_selection` skips the pbcopy/PRIMARY side-effect for THIS
        // gesture. The selection itself is still made (viewport nav is not exfil);
        // only the automatic clipboard write is suppressed. Owner scope and a real
        // human gesture leave the flag `false`, so their copy-on-select is
        // unaffected. This is a clipboard SIDE-EFFECT, never a PTY byte, so it does
        // NOT gate `seam_egress` / violate `bytes_human_eq_controller`.
        Ok(ev) => {
            let ev = apply_copy_on_select_policy(scope, ev);
            input_reply_to_str(post_input_reply(proxy, Op::WriteInput, vec![ev]))
        }
        Err(e) => e,
    }
}

/// PURE control-authority decision: whether a `mouse`-verb gesture from `scope`
/// must have its copy-on-select CLIPBOARD side-effect suppressed. Only a NON-OWNER
/// (scoped-edge) gesture is suppressed; the Owner god token (in-session / owner
/// automation) is exempt, matching the `front_drive_escalation` Owner
/// carve-out. Split out pure so the policy is unit-testable without an event loop
/// (mirrors `update_is_owner_only_subcmd` / `front_drive_escalation`).
pub(crate) fn scope_suppresses_copy_on_select(scope: super::Scope) -> bool {
    !matches!(scope, super::Scope::Owner)
}

/// Stamp the scoped-edge copy-on-select suppression onto a mouse `InputEvent` (a
/// no-op for Owner scope, and for non-`MouseButton` events which never trigger the
/// copy-on-select side-effect). Applied in [`cmd_mouse`] AFTER the scope-blind
/// [`parse_mouse`], so the pure parser output stays byte/struct-identical to human
/// input and the suppression rides ONLY the release that settles the selection.
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

/// `paste <text>` -> write `<text>` to the PTY exactly as if the user pasted
/// it: [`Terminal::format_paste`] strips control bytes that could escape the
/// guards (ESC, C1 controls), converts line breaks to CR, and wraps the body
/// in the bracketed-paste guards `ESC[200~` ... `ESC[201~` when the app has
/// enabled bracketed paste (DECSET 2004). The text is the rest of the line
/// taken literally; a literal trailing `\n` (backslash + n) becomes a line
/// break (sent as CR, like a real paste) so a paste can end in one. For raw
/// unsanitized bytes use `feed`/`send` instead.
pub(crate) fn cmd_paste(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    // The seam runs `format_paste` (bracketing + sanitize) under the lock and the
    // snap-to-bottom, converging with the human Cmd-V path. Reply-bearing so OK
    // means the paste reached the PTY (no window ⇒ ERR, not a false OK).
    input_reply_to_str(post_input_reply(
        proxy,
        Op::WriteInput,
        vec![InputEvent::Paste(paste_text(rest))],
    ))
}

/// The `paste` verb's text transform: a literal trailing `\n` (backslash + n)
/// becomes a real line break (sent as CR by `format_paste`). Pure, so the
/// bracketing/sanitize bytes stay unit-testable without an event loop.
pub(crate) fn paste_text(rest: &str) -> String {
    match rest.strip_suffix("\\n") {
        Some(head) => format!("{head}\n"),
        None => rest.to_string(),
    }
}

/// `focus <in|out>` -> drive DEC 1004 focus reporting (kills divergence j: a
/// controller-only session can now satisfy a focus-tracking app's oracle). The
/// seam emits ESC[I / ESC[O when the app enabled focus reporting, byte-identical
/// to the human `on_focus` egress. `in`/`1`/`true` = focus-in. Reply-bearing like
/// `key` (not fire-and-forget): `OK` means the seam ran `on_focus`; with no
/// frontmost window the dropped reply sender yields `ERR`, so the self path never
/// reports a false `OK` for focus that went nowhere.
pub(crate) fn cmd_focus(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    match parse_focus(rest) {
        Some(focused) => input_reply_to_str(post_input_reply(
            proxy,
            Op::WriteInput,
            vec![InputEvent::Focus(focused)],
        )),
        None => "ERR usage: focus <in|out>\n".to_string(),
    }
}

/// PURE parser for the `focus` verb's `in/out` argument, factored out of
/// [`cmd_focus`] so the self (active-tab) and cross-session paths build the SAME
/// [`InputEvent::Focus`] from the SAME grammar. `in`/`1`/`true`/`focus` => focus-in.
pub(crate) fn parse_focus(rest: &str) -> Option<bool> {
    match rest.trim() {
        "in" | "1" | "true" | "focus" => Some(true),
        "out" | "0" | "false" | "blur" => Some(false),
        _ => None,
    }
}

/// Parse a `tab <arg>` request into the [`TabAction`] it drives (the PURE part, so
/// it is unit-testable without an event loop). Grammar: `new` opens a tab, a
/// 0-based integer `<N>` selects tab N, `next`/`prev` cycle. `None` (an unknown /
/// missing arg) maps the caller to the usage error.
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

/// `tab new | <N> | next | prev` -> DRIVE the FRONT window's native tabs and reply
/// `OK <active_index> <tab_count>`.
///
/// MAIN-THREAD HOP (mirrors [`cmd_chrome`]): mutating `App` (its tabs) may ONLY
/// happen on the event loop, but this runs on a background control thread. So we
/// parse the action, post [`Wake::TabCmd`] with a one-shot reply channel, and BLOCK
/// on the reply; the main thread resolves `self.frontmost_window`, applies the
/// action via the SAME command paths the keyboard/menu use (`open_tab` / `switch_tab`
/// / `cycle_tab`), and sends back the resulting `(active, count)`. `new` reuses the
/// new-tab path; the native toolbar segments then re-track via `App::sync_window`.
pub(crate) fn cmd_tab(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let Some(action) = parse_tab(rest) else {
        return "ERR usage: tab <new|N|next|prev|close [N]|move <from> <to>>\n".to_string();
    };
    match super::control_media::call_main(proxy, |reply| Wake::TabCmd { action, reply }) {
        Ok((active, count)) => format!("OK {active} {count}\n"),
        Err(error) => format!("ERR tab command failed: {error}\n"),
    }
}

/// `hover <on|off>` — toggle the drag-and-drop drop-target highlight on the
/// frontmost window. Testing/automation of the drop affordance (a real drag drives
/// the same flag); also lets `aterm-ctl image` capture the highlight headlessly.
/// Resolved on the main thread ([`Wake::SetDragHover`]) since it targets the front
/// window. Replies `OK`, `ERR no window`, or a usage error.
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

/// Parse + range-check a `resize <r> <c>` request (the PURE part, so it is unit
/// testable without an event loop). Returns the validated `(rows, cols)` or the
/// exact error string the verb replies with.
///
/// Requests outside `1..=MAX_GRID_ROWS`/`MAX_GRID_COLS` are rejected with
/// `ERR out of range` rather than silently clamped, so a caller learns its
/// requested size was not applied.
pub(crate) fn parse_resize(rest: &str) -> Result<(u16, u16), String> {
    let mut it = rest.split_whitespace();
    let (Some(rs), Some(cs)) = (it.next(), it.next()) else {
        return Err("ERR usage: resize <r> <c>\n".to_string());
    };
    let (Ok(r), Ok(c)) = (rs.parse::<u16>(), cs.parse::<u16>()) else {
        return Err("ERR bad args\n".to_string());
    };
    if !(1..=MAX_GRID_ROWS).contains(&r) || !(1..=MAX_GRID_COLS).contains(&c) {
        return Err("ERR out of range\n".to_string());
    }
    Ok((r, c))
}

/// `resize <r> <c>` -> resize the engine grid, the PTY, AND the GUI (RES-1).
///
/// The main thread is the SOLE geometry owner (`App.rows/cols`, the framebuffer,
/// the window). Resizing the term + PTY here directly — as the verb used to —
/// left `App` stale and sent no repaint, so a follow-up `image`/`dims` (which
/// read `App`/the framebuffer) disagreed with the engine. So the verb now ONLY
/// validates and forwards an `InputEvent::Resize` (in a `Wake::Input`) to the main
/// thread, which applies the term + PTY + window resize and requests a redraw in
/// one owner. A dropped proxy (event loop gone) means the GUI is shutting down:
/// report it.
///
/// RES-1: the verb sets `echo_to_window: true` so the seam ALSO asks the window to
/// match the new grid pixel size (the verb has no window event of its own). The
/// interactive winit `Resized` path uses `echo_to_window: false` (the window is
/// already that size). `echo_to_window` is a transport flag, NOT a `Source` branch.
pub(crate) fn cmd_resize(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    // `resize px <w> <h>` — resize the WINDOW, in physical pixels, and let the grid
    // follow from the platform's `Resized` exactly as an edge drag does.
    //
    // The cell form below cannot reach the drag path: it applies the grid first and
    // echoes the pixel size after, so the window event arrives with the columns
    // already correct and the live-resize throttle sees no reflow. That left the
    // width-throttle arms — coalescing, the trailing settle, the leading-edge apply
    // — drivable only by a hand on the window edge, and therefore unmeasurable.
    // Fire several of these back to back to reproduce a drag's event pressure.
    if let Some(px) = rest.trim().strip_prefix("px") {
        let mut it = px.split_whitespace();
        let (Some(ws), Some(hs)) = (it.next(), it.next()) else {
            return "ERR usage: resize px <w> <h>\n".to_string();
        };
        let (Ok(w), Ok(h)) = (ws.parse::<u32>(), hs.parse::<u32>()) else {
            return "ERR bad args\n".to_string();
        };
        return match post_input_reply(
            proxy,
            Op::WriteInput,
            vec![InputEvent::ResizeWindowPx {
                width: w,
                height: h,
            }],
        ) {
            Ok(InputOutcome::RangeRejected) => "ERR out of range\n".to_string(),
            Ok(_) => "OK\n".to_string(),
            Err(e) => e,
        };
    }
    // Range-check up front (keeps the precise `ERR out of range` / usage strings),
    // then post a reply-bearing Resize through the seam. The seam re-clamps and
    // reports `RangeRejected` if somehow out of range — but a valid request here
    // returns `Ok`, so the contract is unchanged for existing callers.
    let (r, c) = match parse_resize(rest) {
        Ok(rc) => rc,
        Err(e) => return e,
    };
    match post_input_reply(
        proxy,
        Op::WriteInput,
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

/// Funnel all control-verb bytes through the active session's single SinkWriter
/// (whole-frame atomicity vs the GUI keyboard path + reader-thread replies). Drops
/// a closed-peer error like the legacy writer did. Used ONLY by the audited raw
/// hatch (`send`/`feed`); the human-vocabulary verbs go through the seam instead.
pub(crate) fn write_pty(sink: &SinkWriter, data: &[u8]) {
    // Control-driven input measures the same input→present slice a keystroke
    // does, so a driven smoke can assert on typing latency.
    crate::metrics::note_input();
    // This write NEVER passes the App input seam, so an in-flight
    // `video ... keys` recording structurally cannot log it. Count the attempts
    // it carries: the recording reports the total as `unlogged_inputs`, which is
    // what stops an empty ledger from being indistinguishable from a quiet
    // screen.
    crate::note_unseamed_control_bytes(data);
    let _ = sink.write_frame(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `lines=N` reaches `InputEvent::Wheel.lines` for the wheel actions: absent it
    /// defaults to 1 (the pre-existing one-notch behaviour), an explicit count is
    /// carried through, and the magnitude is clamped to `1..=MAX_WHEEL_LINES` with a
    /// non-positive / non-numeric value falling back to 1.
    #[test]
    fn parse_mouse_wheel_lines() {
        let Ok(InputEvent::Wheel { lines, dir, .. }) = parse_mouse("wheeldown left 2 4 lines=8")
        else {
            panic!("wheeldown lines=8 parses");
        };
        assert_eq!(lines, 8);
        assert_eq!(dir, aterm_types::mouse::WheelDir::Down);
        // No token: the default stays 1 (byte-compatible with the old grammar).
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
    /// buttons name themselves (audits I7/I8). Without the horizontal pair a
    /// controller could not drive — or regression-test — buttons 66/67 at all.
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
    //! The documented key→photon recipe, pinned at the VERB boundary: what the
    //! active-tab `send`/`feed`/`key` verbs build must reach the opt-in
    //! `video … keys` ledger, and the one path that structurally cannot be
    //! logged must be COUNTED instead of silently dropped.
    //!
    //! These live beside the verb parsers (rather than beside the recording) so
    //! the assertions are driven by the REAL `send_bytes`/`feed_bytes`/`parse_key`
    //! a socket request goes through — the only untested link left between a
    //! verb and the ledger is the single `control.rs` dispatch arm, which the
    //! on-glass recording in this change's report exercises end to end.

    use super::{feed_bytes, parse_key, send_bytes, write_pty};
    use crate::input::InputEvent;
    use crate::{VideoInputSample, record_controller_video_attempts, video_sample_for_key};
    use aterm_session::sink::SinkWriter;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    /// `video … keys` claims to log "socket input driven through the ACTIVE-TAB
    /// verbs", and flagless `send`/`feed` ARE active-tab verbs: `control.rs`
    /// routes both through `front_routed_input` as an `InputEvent::KeySequence`.
    /// The ledger's classifier had no `KeySequence` arm, so every take driven
    /// the documented way produced an EMPTY `inputs[]` — a measuring instrument
    /// reporting zero indistinguishably from one that measured zero.
    #[test]
    fn video_keys_ledger_logs_the_send_and_feed_verbs() {
        // `send 'ls\n'` — the submit form the recipe documents. The trailing
        // literal `\n` normalizes to CR, the byte Return sends.
        let bytes = send_bytes("ls\\n");
        assert_eq!(bytes, b"ls\r".to_vec(), "the verb's bytes, not a stand-in");
        let mut log = Vec::new();
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

        // NON-UTF-8 is still an attempt, never a panic and never a drop: one
        // replacement per maximal invalid subpart, matching from_utf8_lossy.
        let raw = [b'a', 0xff, 0xfe, b'b'];
        let mut lossy = Vec::new();
        record_controller_video_attempts(
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
        record_controller_video_attempts(
            &mut capped,
            &InputEvent::KeySequence(b"abcdefgh".to_vec()),
            400,
            3,
        );
        assert_eq!(capped.len(), 3, "the bounded ledger is still bounded");
    }

    /// A terminal take is driven with `esc`, the arrows and the F-keys, and the
    /// old `char`-only ledger dropped every one of them: `video 3 keys` +
    /// `ctl key up` five times produced `inputs: []`. Named keys are now
    /// SAMPLES, and the three named keys with an unambiguous character keep
    /// their character representation. Bare modifiers stay out (they express no
    /// intent and would only add noise to the correlation).
    #[test]
    fn video_keys_ledger_names_keys_that_have_no_character() {
        for (verb, expect) in [
            ("up", VideoInputSample::Named(NamedKey::ArrowUp)),
            ("esc", VideoInputSample::Named(NamedKey::Escape)),
            ("backspace", VideoInputSample::Named(NamedKey::Backspace)),
            ("f1", VideoInputSample::Named(NamedKey::F1)),
            ("pagedown", VideoInputSample::Named(NamedKey::PageDown)),
            // The character-bearing three keep their historical shape.
            ("enter", VideoInputSample::Char('\n')),
            ("tab", VideoInputSample::Char('\t')),
            ("space", VideoInputSample::Char(' ')),
        ] {
            let ev = parse_key(verb).unwrap_or_else(|| panic!("`key {verb}` parses"));
            let mut log = Vec::new();
            record_controller_video_attempts(&mut log, &ev, 7, 64);
            assert_eq!(log, vec![(7, expect)], "`key {verb}` must be an attempt");
        }

        // A bare modifier is not an attempt.
        assert_eq!(
            video_sample_for_key(&Key::Named(NamedKey::ShiftLeft)),
            None,
            "a bare Shift expresses no input intent"
        );
        assert_eq!(video_sample_for_key(&Key::Named(NamedKey::CapsLock)), None);

        // index.json shape: `ch` for characters, `key` for named — both valid
        // JSON fragments, with control characters escaped.
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

    /// The genuinely-unloggable path must be COUNTED, not silently ignored:
    /// `write_pty` is the control-thread egress `cmd_send`/`cmd_feed` take for a
    /// BACKGROUND target, and it never reaches the App input seam the ledger
    /// hooks. A recording can only stay honest by reporting how many of them
    /// happened during the take.
    ///
    /// The assertion is `>=` deliberately: the counter is process-wide and other
    /// tests in this binary drive the same verbs concurrently, so they can only
    /// ADD. Neutering the `note_unseamed_control_bytes()` call site makes this
    /// delta 0 and the test fails.
    ///
    /// The unit is the LEDGER's unit — one per attempt that would have become an
    /// `inputs[]` row — so `write_pty(b"abc")` counts THREE, matching what the
    /// same three bytes would have logged had they been front-routed. A reader
    /// comparing `inputs_logged` with `unlogged_inputs` is comparing like with
    /// like.
    #[test]
    fn control_thread_input_egress_is_counted_for_the_video_ledger() {
        let sink = SinkWriter::new(-1);
        let before = crate::unseamed_control_inputs();
        const DRIVES: u64 = 12;
        for _ in 0..DRIVES {
            write_pty(&sink, b"x");
        }
        let after = crate::unseamed_control_inputs();
        assert!(
            after.saturating_sub(before) >= DRIVES,
            "{DRIVES} control-thread egresses must be countable; saw {} \
             (an uncounted bypass is exactly the silent zero this instrument must not produce)",
            after.saturating_sub(before)
        );

        // Per-ATTEMPT, not per-call: `send hey` at a background target is three
        // attempts the ledger did not get, and must be reported as three.
        let before = crate::unseamed_control_inputs();
        write_pty(&sink, &send_bytes("hey"));
        assert!(
            crate::unseamed_control_inputs().saturating_sub(before) >= 3,
            "a 3-byte background `send` is 3 unlogged attempts, not 1"
        );
    }

    /// The disclosure must not cry wolf. `unlogged_inputs` is the count of
    /// attempts the ledger WOULD have recorded, so an event it never records
    /// anyway must contribute nothing: a cross-session `focus` reaching the same
    /// control-thread arm as `key` would otherwise push the count above zero and
    /// make `index.json` announce a MEASUREMENT GAP for a take that had none —
    /// the silent zero's mistake pointed the other way.
    ///
    /// Asserted on the PURE classifier, not on the process-wide counter: that
    /// counter is shared with every other test in this binary (the sibling
    /// wiring test above drives fifteen egresses through it), so an exact
    /// before/after assertion on it is a race, not a proof. This one is exact
    /// because it shares no state.
    #[test]
    fn unloggable_counter_ignores_events_the_ledger_never_records() {
        for quiet in [
            InputEvent::Focus(true),
            InputEvent::Focus(false),
            InputEvent::Key {
                key: Key::Named(NamedKey::ShiftLeft),
                mods: Modifiers::default(),
                base_layout: None,
                event_type: KeyEventType::Press,
            },
        ] {
            assert_eq!(
                crate::video_attempt_count(&quiet),
                0,
                "{quiet:?} is not an input attempt and must not be reported as an unlogged one"
            );
        }

        // A real attempt on the same arm still counts, so the gate is a
        // classifier and not an off switch.
        assert_eq!(
            crate::video_attempt_count(&InputEvent::Text("ab".into())),
            2,
            "a cross-session paste of two chars IS two attempts this ledger missed"
        );

        // The raw-byte arm agrees, per attempt, and an empty payload (a
        // zero-length `feed`) is not an attempt at all.
        assert_eq!(crate::video_attempt_count_bytes(b""), 0);
        assert_eq!(crate::video_attempt_count_bytes(&send_bytes("hey")), 3);
    }
}
