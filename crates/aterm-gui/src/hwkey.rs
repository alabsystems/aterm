// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! HARDWARE-PATH key injection — the instrument the latency work was missing.
//!
//! # Why this exists
//!
//! The control socket's `key` verb posts a [`crate::Wake::Input`] to the main
//! thread, which applies an already-decoded `InputEvent` through the App input
//! seam. That is the right shape for driving a session, and it is byte-identical
//! to a human keystroke at the PTY. It is NOT the right shape for MEASURING one.
//!
//! `note_key_arrival_queued` — the NSEvent-queue BACKDATE that puts time a
//! keystroke spent waiting for a parked event loop inside `key->write` and
//! `input->present` — is called from exactly one site: the winit
//! `WindowEvent::KeyboardInput` arm. A `Wake::Input` arrives on a `CFRunLoopSource`,
//! not in the NSEvent queue, and it stamps its arrival at `note_input()`, i.e.
//! AFTER the event loop already dequeued it. So a socket-injected key is
//! structurally incapable of observing the single largest documented typing-stall
//! mechanism on macOS: a blocking `nextDrawable` parks the main thread and queues
//! keyDowns in the OS event queue behind it (`metrics.rs` prices that at up to
//! ~84 ms). Every latency number this project has published was driven by `ctl
//! key`, so every one of them is a measurement with that slice removed.
//!
//! # What this does instead
//!
//! It builds a real `NSEvent` keyDown/keyUp pair and hands it to
//! `-[NSApplication postEvent:atStart:]` — the app's OWN event queue, the same
//! queue WindowServer delivers a physical key into. From that moment on there is
//! no injected path at all: the run loop dequeues it, `-[NSApplication sendEvent:]`
//! routes it to the key window, winit's `WinitView keyDown:` runs
//! `interpretKeyEvents` and `create_key_event` on it, and the resulting
//! `WindowEvent::KeyboardInput` reaches the same arm with the same
//! `NSApp.currentEvent()` visible to `current_event_queue_age_ns()`. The event
//! carries a REAL `timestamp` (taken from the same `NSProcessInfo.systemUptime`
//! clock the age is computed against), so the backdate measures the true queue
//! residence rather than being discarded as a synthesized event (`timestamp <= 0`).
//!
//! # The one difference from a physical key, stated plainly
//!
//! The event is born inside this process instead of in WindowServer, so it never
//! crosses the mach port from the window server, and `CGEventSource`-level state
//! (key-repeat state, the HID idle clock) does not move. Everything the metrics
//! path can see — the arm, the dispatch, the queue residence, the key translation,
//! the modifier state, the PTY egress — is the same code on the same objects.
//!
//! Posting happens on the CONTROL thread, deliberately. Hopping to the main thread
//! first would make every injected key arrive at the same phase of the render loop
//! (just after a wake, just before the next park), which is exactly the phase
//! relationship the measurement is trying to sample fairly. `postEvent:atStart:` is
//! one of the few `NSApplication` methods Apple documents as safe from any thread —
//! it is the canonical secondary-thread wakeup — and it is how a key posted while
//! the main thread is parked inside `get_current_texture()` can WAIT in the queue,
//! which is the entire point.

/// A parsed hardware-key injection request: everything needed to build the
/// `NSEvent` pair, with no AppKit types, so the grammar is unit-testable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HwKeySpec {
    /// macOS virtual keycode (`kVK_*`). winit's `create_key_event` derives the
    /// logical key from THIS through `UCKeyTranslate`, so it, not `chars`, is what
    /// decides which key the app believes was pressed.
    pub keycode: u16,
    /// `-[NSEvent characters]`: the text the layout produces WITH modifiers applied.
    pub chars: String,
    /// `-[NSEvent charactersIgnoringModifiers]`: winit dereferences this
    /// unconditionally (`.expect("expected characters to be non-null")`), so it is
    /// never empty here.
    pub chars_ignoring: String,
    /// Raw `NSEventModifierFlags` bits.
    pub flags: u64,
    /// How many press/release pairs to post.
    pub count: u32,
    /// Milliseconds to sleep between successive presses. The default paces the
    /// stream like human typing rather than dumping a burst into the queue.
    pub interval_ms: u64,
}

/// `NSEventModifierFlags` bits, spelled out so the pure parser needs no AppKit.
const FLAG_SHIFT: u64 = 1 << 17;
const FLAG_CONTROL: u64 = 1 << 18;
const FLAG_OPTION: u64 = 1 << 19;
const FLAG_COMMAND: u64 = 1 << 20;
/// `NSEventModifierFlagFunction` — AppKit sets it on arrows and F-keys, and some
/// responders read it, so a synthetic arrow must carry it too.
const FLAG_FUNCTION: u64 = 1 << 23;

/// Upper bound on one `hwkey` invocation. The verb BLOCKS while it paces the
/// stream, so an unbounded count would pin a control worker indefinitely.
const MAX_COUNT: u32 = 5000;
/// Upper bound on the pacing interval, for the same reason.
const MAX_INTERVAL_MS: u64 = 1000;
/// Default pacing: 40 ms ≈ 25 keys/s, brisk human typing. Deliberately NOT zero —
/// a zero-interval burst measures queue drain, not typing.
const DEFAULT_INTERVAL_MS: u64 = 40;

/// A named key: `(name, keycode, characters)`. `characters` uses the private-use
/// function-key code points AppKit puts in `-[NSEvent characters]` for the
/// non-printing keys, and the C0/DEL control characters for the rest — the same
/// bytes a physical press produces.
const NAMED: &[(&str, u16, &str)] = &[
    ("enter", 36, "\r"),
    ("return", 36, "\r"),
    ("tab", 48, "\t"),
    ("space", 49, " "),
    ("backspace", 51, "\u{7f}"),
    ("delete", 117, "\u{f728}"),
    ("esc", 53, "\u{1b}"),
    ("escape", 53, "\u{1b}"),
    ("up", 126, "\u{f700}"),
    ("down", 125, "\u{f701}"),
    ("left", 123, "\u{f702}"),
    ("right", 124, "\u{f703}"),
    ("home", 115, "\u{f729}"),
    ("end", 119, "\u{f72b}"),
    ("pageup", 116, "\u{f72c}"),
    ("pagedown", 121, "\u{f72d}"),
    ("f1", 122, "\u{f704}"),
    ("f2", 120, "\u{f705}"),
    ("f3", 99, "\u{f706}"),
    ("f4", 118, "\u{f707}"),
    ("f5", 96, "\u{f708}"),
    ("f6", 97, "\u{f709}"),
    ("f7", 98, "\u{f70a}"),
    ("f8", 100, "\u{f70b}"),
    ("f9", 101, "\u{f70c}"),
    ("f10", 109, "\u{f70d}"),
    ("f11", 103, "\u{f70e}"),
    ("f12", 111, "\u{f70f}"),
];

/// The keys that AppKit marks with `NSEventModifierFlagFunction` on a real press.
const FUNCTION_FLAGGED: &[u16] = &[
    123, 124, 125, 126, // arrows
    115, 116, 117, 119, 121, // home/pageup/fwd-delete/end/pagedown
    96, 97, 98, 99, 100, 101, 103, 109, 111, 118, 120, 122, // F1..F12
];

/// US-QWERTY virtual keycode for a printable ASCII character, unshifted form.
/// Returns `(keycode, needs_shift)`.
///
/// This is a LAYOUT ASSUMPTION and the only one in the module: it maps a character
/// the caller typed on the command line back to the physical key that produces it
/// on a US layout. winit then runs the REAL active layout over the keycode, so on a
/// non-US layout the app will faithfully report whatever that layout puts on that
/// physical key — which is correct behaviour for a hardware-path instrument (a
/// physical key is a physical key), just not what a caller naively expects.
#[must_use]
fn ansi_keycode(c: char) -> Option<(u16, bool)> {
    // Shifted ASCII first: fold to the unshifted character and demand shift.
    let (base, shift) = match c {
        'A'..='Z' => (c.to_ascii_lowercase(), true),
        '!' => ('1', true),
        '@' => ('2', true),
        '#' => ('3', true),
        '$' => ('4', true),
        '%' => ('5', true),
        '^' => ('6', true),
        '&' => ('7', true),
        '*' => ('8', true),
        '(' => ('9', true),
        ')' => ('0', true),
        '_' => ('-', true),
        '+' => ('=', true),
        '{' => ('[', true),
        '}' => (']', true),
        '|' => ('\\', true),
        ':' => (';', true),
        '"' => ('\'', true),
        '<' => (',', true),
        '>' => ('.', true),
        '?' => ('/', true),
        '~' => ('`', true),
        other => (other, false),
    };
    let code = match base {
        'a' => 0,
        's' => 1,
        'd' => 2,
        'f' => 3,
        'h' => 4,
        'g' => 5,
        'z' => 6,
        'x' => 7,
        'c' => 8,
        'v' => 9,
        'b' => 11,
        'q' => 12,
        'w' => 13,
        'e' => 14,
        'r' => 15,
        'y' => 16,
        't' => 17,
        '1' => 18,
        '2' => 19,
        '3' => 20,
        '4' => 21,
        '6' => 22,
        '5' => 23,
        '=' => 24,
        '9' => 25,
        '7' => 26,
        '-' => 27,
        '8' => 28,
        '0' => 29,
        ']' => 30,
        'o' => 31,
        'u' => 32,
        '[' => 33,
        'i' => 34,
        'p' => 35,
        'l' => 37,
        'j' => 38,
        '\'' => 39,
        'k' => 40,
        ';' => 41,
        '\\' => 42,
        ',' => 43,
        '/' => 44,
        'n' => 45,
        'm' => 46,
        '.' => 47,
        ' ' => 49,
        '`' => 50,
        _ => return None,
    };
    Some((code, shift))
}

/// Strip leading inline modifier prefixes (`ctrl+`, `alt+`, …), mirroring
/// [`crate::control_input::parse_key`]'s grammar so the two verbs read the same.
fn take_inline_mods(body: &str) -> (u64, &str) {
    let mut m = 0u64;
    let mut rest = body;
    while let Some(plus) = rest.find('+') {
        let bit = match &rest[..plus] {
            "shift" => FLAG_SHIFT,
            "ctrl" | "control" => FLAG_CONTROL,
            "alt" | "option" => FLAG_OPTION,
            "super" | "cmd" | "command" => FLAG_COMMAND,
            _ => break,
        };
        m |= bit;
        rest = &rest[plus + 1..];
    }
    (m, rest)
}

/// Parse `mods=<list>` / `count=<n>` / `interval=<ms>` trailing tokens, returning
/// them plus the residual body. Any unparsable token value is an error rather than
/// a silent default: a typo'd `count=1oo` that silently injected one key would
/// invalidate a whole measurement run without saying so.
fn take_tokens(rest: &str) -> Result<(u64, u32, u64, String), String> {
    let mut mods = 0u64;
    let mut count = 1u32;
    let mut interval = DEFAULT_INTERVAL_MS;
    let mut kept: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(list) = tok.strip_prefix("mods=") {
            for name in list.split(['+', ',']) {
                mods |= match name {
                    "shift" => FLAG_SHIFT,
                    "ctrl" | "control" => FLAG_CONTROL,
                    "alt" | "option" => FLAG_OPTION,
                    "super" | "cmd" | "command" => FLAG_COMMAND,
                    "" => 0,
                    other => return Err(format!("ERR hwkey: unknown modifier `{other}`\n")),
                };
            }
        } else if let Some(n) = tok.strip_prefix("count=") {
            count = n
                .parse::<u32>()
                .map_err(|_| format!("ERR hwkey: bad count `{n}`\n"))?;
            if count == 0 || count > MAX_COUNT {
                return Err(format!("ERR hwkey: count must be 1..={MAX_COUNT}\n"));
            }
        } else if let Some(n) = tok.strip_prefix("interval=") {
            interval = n
                .parse::<u64>()
                .map_err(|_| format!("ERR hwkey: bad interval `{n}`\n"))?;
            if interval > MAX_INTERVAL_MS {
                return Err(format!(
                    "ERR hwkey: interval must be 0..={MAX_INTERVAL_MS} ms\n"
                ));
            }
        } else {
            kept.push(tok);
        }
    }
    Ok((mods, count, interval, kept.join(" ")))
}

/// PURE parser for `hwkey <name> [mods=<list>] [count=<n>] [interval=<ms>]`.
///
/// Kept free of AppKit so the whole grammar — including the character→keycode
/// table a measurement's fidelity rests on — is exercised by `#[test]` on every
/// platform, not only when someone runs the verb by hand on a Mac.
pub(crate) fn parse_hwkey(rest: &str) -> Result<HwKeySpec, String> {
    let (tok_mods, count, interval_ms, body) = take_tokens(rest)?;
    let (prefix_mods, body) = take_inline_mods(body.trim());
    let mut flags = tok_mods | prefix_mods;
    let body = body.trim();
    if body.is_empty() {
        return Err(usage());
    }

    let lower = body.to_ascii_lowercase();
    if let Some(&(_, keycode, chars)) = NAMED.iter().find(|(n, _, _)| *n == lower) {
        if FUNCTION_FLAGGED.contains(&keycode) {
            flags |= FLAG_FUNCTION;
        }
        return Ok(HwKeySpec {
            keycode,
            chars: chars.to_string(),
            chars_ignoring: chars.to_string(),
            flags,
            count,
            interval_ms,
        });
    }

    let mut chars_iter = body.chars();
    let (Some(c), None) = (chars_iter.next(), chars_iter.next()) else {
        return Err(usage());
    };
    let Some((keycode, needs_shift)) = ansi_keycode(c) else {
        return Err(usage());
    };
    if needs_shift {
        flags |= FLAG_SHIFT;
    }
    // `charactersIgnoringModifiers` heeds SHIFT only (that is exactly what AppKit
    // reports), so an explicitly shifted letter must carry its uppercase form.
    let ignoring = if flags & FLAG_SHIFT != 0 {
        c.to_ascii_uppercase().to_string()
    } else {
        c.to_ascii_lowercase().to_string()
    };
    // `characters` heeds everything: Control folds an ASCII letter to its C0
    // control byte, which is what a physical Ctrl-chord press delivers.
    let chars = if flags & FLAG_CONTROL != 0 && c.is_ascii_alphabetic() {
        let ctrl = (c.to_ascii_uppercase() as u8) & 0x1f;
        (ctrl as char).to_string()
    } else {
        ignoring.clone()
    };
    Ok(HwKeySpec {
        keycode,
        chars,
        chars_ignoring: ignoring,
        flags,
        count,
        interval_ms,
    })
}

fn usage() -> String {
    "ERR usage: hwkey <char|name> [mods=<list>] [count=<n>] [interval=<ms>] — \
     posts a REAL NSEvent into this app's own OS event queue so the key takes the \
     winit hardware path (macOS only). Names: enter tab esc space backspace delete \
     up down left right home end pageup pagedown f1..f12\n"
        .to_string()
}

/// Post the request as real `NSEvent`s into this application's event queue.
///
/// `window_number` is the target window's `-[NSWindow windowNumber]`, resolved on
/// the main thread by the caller: `-[NSApplication sendEvent:]` routes a key event
/// by the number baked into it, so a zero here would be dispatched to no window at
/// all and the whole measurement would silently record nothing.
///
/// Returns the number of press events posted.
#[cfg(target_os = "macos")]
pub(crate) fn post(spec: &HwKeySpec, window_number: i64) -> Result<u32, String> {
    use aterm_objc::{Bool, CGPoint, Id, Sel, autoreleasepool, class, sel};

    use crate::appkit;

    /// `NSEvent.h` — `NSEventTypeKeyDown = 10`, `NSEventTypeKeyUp = 11`.
    /// Values, not names: `NSEventType` is an `NS_ENUM(NSUInteger, …)` in a
    /// header, so there is no symbol to link (see `appkit::consts`).
    const NS_EVENT_TYPE_KEY_DOWN: usize = 10;
    const NS_EVENT_TYPE_KEY_UP: usize = 11;

    if window_number == 0 {
        return Err("ERR hwkey: no frontmost AppKit window to post into\n".to_string());
    }
    let (Some(chars), Some(ignoring)) = (
        appkit::nsstring(&spec.chars),
        appkit::nsstring(&spec.chars_ignoring),
    ) else {
        return Err("ERR hwkey: could not build the key strings\n".to_string());
    };
    // `NSEventModifierFlags` is an `NSUInteger` bitmask; every flag bit used
    // here is below 1<<24, so the cast is exact on every macOS target.
    let flags = spec.flags as usize;

    // SAFETY: `+sharedApplication` is `-(id)` and is called only after the event
    // loop has already created it on the main thread (the caller resolved a live
    // window number through the main thread first, which cannot happen before
    // `NSApp` exists), so this returns the existing singleton and does not
    // construct AppKit's application object off-main. The returned object is
    // retained by the runtime for the process lifetime, so it is borrowed here.
    let app = unsafe { appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) };
    if app.is_null() {
        return Err("ERR hwkey: no shared NSApplication\n".to_string());
    }

    let mut posted = 0u32;
    for i in 0..spec.count {
        if i > 0 && spec.interval_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(spec.interval_ms));
        }
        // One pool PER KEY, not one around the loop: `count=5000` would otherwise
        // hold five thousand autoreleased NSEvents alive for the whole run, and a
        // control thread with no pool at all makes Cocoa log-and-leak every one of
        // them. Draining per key keeps a long paced burst flat.
        autoreleasepool(|_| {
            // The timestamp is read HERE, immediately before the post, on the same
            // seconds-since-boot clock `current_event_queue_age_ns` subtracts it
            // from. That is what makes the backdate honest: the age it computes is
            // the real interval this event spent sitting in the queue, exactly as
            // for a key the window server delivered.
            //
            // SAFETY: `+processInfo` is `-(id)`, an immortal singleton, and
            // `-systemUptime` is `-(NSTimeInterval)`, a plain scalar getter.
            let ts = unsafe {
                let info = appkit::send_id(class(c"NSProcessInfo").as_id(), sel!(processInfo));
                if info.is_null() {
                    return Err("ERR hwkey: no NSProcessInfo\n".to_string());
                }
                appkit::send_f64(info, sel!(systemUptime))
            };
            // THE WIDEST SEND IN THE TREE: ten arguments plus the implicit
            // `self`/`_cmd`, i.e. twelve of `MsgFn`'s sixteen parameters — and a
            // 16-byte `NSPoint` BY VALUE in the second argument slot, which is
            // where a wrong prototype would shift every later argument by a
            // register on both ABIs. Written out once, as the crate's rule
            // requires; there is no shared helper for a shape used once.
            //
            // SAFETY: the AppKit event factory with a non-nil characters pair
            // (winit dereferences both unconditionally) and a nil graphics
            // context, exactly as winit's own `replace_event` builds a
            // synthetic key event. The returned event is AUTORELEASED into this
            // per-key pool. `postEvent:atStart:` is documented thread-safe — it
            // is AppKit's sanctioned way for a secondary thread to hand work to
            // the main run loop — and posting from this thread is the POINT: it
            // lets the event arrive, and wait, while the main thread is parked
            // in `nextDrawable`.
            let key_event = |kind: usize| unsafe {
                let make: unsafe extern "C" fn(
                    Id,
                    Sel,
                    usize,
                    CGPoint,
                    usize,
                    f64,
                    isize,
                    Id,
                    Id,
                    Id,
                    Bool,
                    u16,
                ) -> Id = aterm_objc::msg();
                make(
                    class(c"NSEvent").as_id(),
                    sel!(
                        keyEventWithType:location:modifierFlags:timestamp:windowNumber:
                        context:characters:charactersIgnoringModifiers:isARepeat:keyCode:
                    ),
                    kind,
                    CGPoint { x: 0.0, y: 0.0 },
                    flags,
                    ts,
                    window_number as isize,
                    Id::NIL,
                    chars.id(),
                    ignoring.id(),
                    Bool::NO,
                    spec.keycode,
                )
            };
            let down = key_event(NS_EVENT_TYPE_KEY_DOWN);
            if down.is_null() {
                return Err("ERR hwkey: NSEvent construction failed\n".to_string());
            }
            // SAFETY: `-postEvent:atStart:` is `-(void)(NSEvent *, BOOL)` on the
            // shared application with a live event; see the thread-safety note.
            unsafe {
                let post: unsafe extern "C" fn(Id, Sel, Id, Bool) = aterm_objc::msg();
                post(app, sel!(postEvent:atStart:), down, Bool::NO);
                // The release too: a real key always produces one, and leaving
                // the press unmatched would drift AppKit's notion of the
                // pressed-key set across a long run. winit maps it to
                // `ElementState::Released`, which the metrics arm ignores
                // (released keys do not echo), so it costs a dispatch and
                // changes no measurement.
                let up = key_event(NS_EVENT_TYPE_KEY_UP);
                if !up.is_null() {
                    post(app, sel!(postEvent:atStart:), up, Bool::NO);
                }
            }
            Ok(())
        })?;
        posted += 1;
    }
    Ok(posted)
}

/// Non-macOS: there is no NSEvent queue to post into, and the queue-age backdate
/// this instrument exists to exercise is `None` on every other platform anyway
/// (`platform::current_event_queue_age_ns`). Refusing is the honest answer; a
/// silent fallback to the socket path would reintroduce precisely the
/// measurement blindness this verb was built to remove.
#[cfg(not(target_os = "macos"))]
pub(crate) fn post(_spec: &HwKeySpec, _window_number: i64) -> Result<u32, String> {
    Err("ERR hwkey: hardware-path injection is implemented on macOS only\n".to_string())
}

#[cfg(test)]
mod tests {
    use super::{FLAG_COMMAND, FLAG_CONTROL, FLAG_FUNCTION, FLAG_SHIFT, MAX_COUNT, parse_hwkey};

    /// A bare letter is the unshifted US-QWERTY key for it, with no modifiers and
    /// both character strings equal — the shape AppKit reports for a plain press.
    #[test]
    fn a_plain_letter_is_its_unshifted_physical_key() {
        let s = parse_hwkey("x").expect("x parses");
        assert_eq!(s.keycode, 7, "kVK_ANSI_X");
        assert_eq!(s.chars, "x");
        assert_eq!(s.chars_ignoring, "x");
        assert_eq!(s.flags, 0);
        assert_eq!(s.count, 1);
    }

    /// SHIFT is DERIVED from the character, not demanded from the caller: `hwkey X`
    /// must be the same physical key as `hwkey x` plus the shift flag, because that
    /// is what the keyboard does. Getting this wrong would post keycode-for-`x`
    /// with no shift and quietly measure the wrong keystroke.
    #[test]
    fn an_uppercase_letter_shifts_the_same_physical_key() {
        let lower = parse_hwkey("x").unwrap();
        let upper = parse_hwkey("X").unwrap();
        assert_eq!(upper.keycode, lower.keycode);
        assert_eq!(upper.flags, FLAG_SHIFT);
        assert_eq!(upper.chars_ignoring, "X");
    }

    /// A shifted PUNCTUATION character folds to its unshifted key too.
    #[test]
    fn shifted_punctuation_folds_to_its_base_key() {
        let plain = parse_hwkey("1").unwrap();
        let bang = parse_hwkey("!").unwrap();
        assert_eq!(bang.keycode, plain.keycode);
        assert_eq!(bang.flags, FLAG_SHIFT);
    }

    /// Control chords carry the C0 byte in `characters` (what AppKit delivers) and
    /// the bare letter in `charactersIgnoringModifiers` (what winit reads for the
    /// logical key). Inline `ctrl+` and trailing `mods=ctrl` must agree.
    #[test]
    fn a_control_chord_carries_the_c0_byte_and_the_bare_letter() {
        let a = parse_hwkey("ctrl+u").unwrap();
        let b = parse_hwkey("u mods=ctrl").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.flags, FLAG_CONTROL);
        assert_eq!(a.chars, "\u{15}");
        assert_eq!(a.chars_ignoring, "u");
    }

    /// Named keys resolve to their AppKit keycode and private-use character, and
    /// the ones AppKit marks as function keys carry that flag — an arrow without it
    /// is not what a real arrow press looks like.
    #[test]
    fn named_keys_match_appkit_codes_and_function_flag() {
        let enter = parse_hwkey("enter").unwrap();
        assert_eq!(enter.keycode, 36);
        assert_eq!(enter.chars, "\r");
        assert_eq!(
            enter.flags & FLAG_FUNCTION,
            0,
            "Return is not a function key"
        );

        let up = parse_hwkey("up").unwrap();
        assert_eq!(up.keycode, 126);
        assert_eq!(up.chars, "\u{f700}");
        assert_eq!(up.flags & FLAG_FUNCTION, FLAG_FUNCTION);

        assert_eq!(parse_hwkey("esc").unwrap(), parse_hwkey("escape").unwrap());
        assert_eq!(
            parse_hwkey("enter").unwrap(),
            parse_hwkey("return").unwrap()
        );
    }

    /// winit dereferences `charactersIgnoringModifiers` with `.expect(...)`, so an
    /// empty string there would abort the host process on dispatch. No accepted
    /// spelling may produce one.
    #[test]
    fn no_accepted_key_yields_empty_character_strings() {
        for spelling in [
            "a",
            "Z",
            "0",
            "!",
            "space",
            "enter",
            "tab",
            "esc",
            "backspace",
            "delete",
            "up",
            "down",
            "left",
            "right",
            "home",
            "end",
            "pageup",
            "pagedown",
            "f1",
            "f12",
            "ctrl+c",
            "cmd+v",
            "shift+alt+q",
        ] {
            let s = parse_hwkey(spelling).unwrap_or_else(|e| panic!("{spelling}: {e}"));
            assert!(!s.chars.is_empty(), "{spelling} had empty characters");
            assert!(
                !s.chars_ignoring.is_empty(),
                "{spelling} had empty charactersIgnoringModifiers"
            );
        }
    }

    /// Pacing tokens are additive and bounded. A silently-clamped or
    /// silently-defaulted count would corrupt a measurement without reporting it,
    /// so out-of-range values are refused instead.
    #[test]
    fn pacing_tokens_are_additive_and_bounded() {
        let s = parse_hwkey("x count=200 interval=25").unwrap();
        assert_eq!(s.count, 200);
        assert_eq!(s.interval_ms, 25);
        assert_eq!(s.keycode, parse_hwkey("x").unwrap().keycode);

        assert_eq!(
            parse_hwkey("x").unwrap().interval_ms,
            40,
            "human-ish default"
        );
        assert!(parse_hwkey("x count=0").is_err());
        assert!(parse_hwkey(&format!("x count={}", MAX_COUNT + 1)).is_err());
        assert!(parse_hwkey("x count=1oo").is_err());
        assert!(parse_hwkey("x interval=100000").is_err());
        assert!(parse_hwkey("x mods=bogus").is_err());
    }

    /// Unknown / multi-character bodies are refused rather than guessed at.
    #[test]
    fn a_malformed_body_is_refused() {
        assert!(parse_hwkey("").is_err());
        assert!(parse_hwkey("   ").is_err());
        assert!(parse_hwkey("notakey").is_err());
        assert!(parse_hwkey("ab").is_err());
        // A modifier prefix with nothing after it is not a key.
        assert!(parse_hwkey("ctrl+").is_err());
    }

    /// Modifier spelling aliases match the `key` verb's table, so a driver that
    /// knows one verb's grammar can move to the other without surprises.
    #[test]
    fn modifier_aliases_match_the_key_verb() {
        assert_eq!(
            parse_hwkey("cmd+v").unwrap().flags & FLAG_COMMAND,
            FLAG_COMMAND
        );
        assert_eq!(
            parse_hwkey("command+v").unwrap(),
            parse_hwkey("super+v").unwrap()
        );
        assert_eq!(
            parse_hwkey("control+c").unwrap(),
            parse_hwkey("ctrl+c").unwrap()
        );
        assert_eq!(
            parse_hwkey("option+x").unwrap(),
            parse_hwkey("alt+x").unwrap()
        );
    }
}
