// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hardware-path key injection, for measuring keystroke latency.

/// A parsed hardware-key injection request: everything needed to build the `NSEvent` pair, with no AppKit types, so the grammar is unit-testable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HwKeySpec {
    /// macOS virtual keycode (`kVK_*`). winit's `create_key_event` derives the logical key from this through `UCKeyTranslate`, so it, not `chars`, decides which key the app sees.
    pub keycode: u16,
    /// `-[NSEvent characters]`: the text the layout produces with modifiers applied.
    pub chars: String,
    /// `-[NSEvent charactersIgnoringModifiers]`: winit dereferences this unconditionally (`.expect("expected characters to be non-null")`), so it is never empty here.
    pub chars_ignoring: String,
    /// Raw `NSEventModifierFlags` bits.
    pub flags: u64,
    /// How many press/release pairs to post.
    pub count: u32,
    /// Milliseconds between successive presses.
    pub interval_ms: u64,
    /// `shape=press`: build each event as a key press is, from a CoreGraphics keyboard event wrapped by `+[NSEvent eventWithCGEvent:]`, so AppKit reads its characters off the CURRENT layout (Option+b is `∫` on the U.S. layout, Option+e the acute dead key with empty characters) and the input method may compose it.
    pub press: bool,
}

/// `NSEventModifierFlags` bits, spelled out so the pure parser needs no AppKit.
const FLAG_SHIFT: u64 = 1 << 17;
const FLAG_CONTROL: u64 = 1 << 18;
const FLAG_OPTION: u64 = 1 << 19;
const FLAG_COMMAND: u64 = 1 << 20;
/// `NSEventModifierFlagFunction` — AppKit sets it on arrows and F-keys, and some
/// responders read it, so a synthetic arrow must carry it too.
const FLAG_FUNCTION: u64 = 1 << 23;

/// Upper bound on one `hwkey` invocation: the verb blocks while it paces the stream, so an
/// unbounded count would pin a control worker.
const MAX_COUNT: u32 = 5000;
/// Upper bound on the pacing interval, for the same reason.
const MAX_INTERVAL_MS: u64 = 1000;
/// Default pacing: 40 ms ≈ 25 keys/s, brisk typing. Not zero: a zero-interval burst measures
/// queue drain, not typing.
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
    123, 124, 125, 126, 115, 116, 117, 119, 121, 96, 97, 98, 99, 100, 101, 103, 109, 111, 118, 120,
    122,
];

/// US-QWERTY virtual keycode for a printable ASCII character: `(keycode, needs_shift)`.
#[must_use]
const fn ansi_keycode(c: char) -> Option<(u16, bool)> {
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

/// Parse `mods=<list>` / `count=<n>` / `interval=<ms>` trailing tokens, returning them plus the
/// residual body.
fn take_tokens(rest: &str) -> Result<(u64, u32, u64, bool, String), String> {
    let mut mods = 0u64;
    let mut count = 1u32;
    let mut interval = DEFAULT_INTERVAL_MS;
    let mut press = false;
    let mut kept: Vec<&str> = Vec::new();
    for tok in rest.split_whitespace() {
        if let Some(shape) = tok.strip_prefix("shape=") {
            press = match shape {
                "press" => true,
                "built" => false,
                other => {
                    return Err(format!(
                        "ERR hwkey: unknown shape `{other}` (press, built)\n"
                    ));
                }
            };
        } else if let Some(list) = tok.strip_prefix("mods=") {
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
    Ok((mods, count, interval, press, kept.join(" ")))
}

/// PURE parser for `hwkey <name> [mods=<list>] [count=<n>] [interval=<ms>] [shape=press|built]`.
pub(crate) fn parse_hwkey(rest: &str) -> Result<HwKeySpec, String> {
    let (tok_mods, count, interval_ms, press, body) = take_tokens(rest)?;
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
            press,
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
    let ignoring = if flags & FLAG_SHIFT != 0 {
        c.to_ascii_uppercase().to_string()
    } else {
        c.to_ascii_lowercase().to_string()
    };
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
        press,
    })
}

fn usage() -> String {
    "ERR usage: hwkey <char|name> [mods=<list>] [count=<n>] [interval=<ms>] \
     [shape=press|built] — posts a REAL NSEvent into this app's own OS event queue so \
     the key takes the winit hardware path (macOS only); shape=press builds it from a \
     CoreGraphics key event, as a key press is, so the layout gives its characters and \
     the input method may compose it. Names: enter tab esc space backspace delete \
     up down left right home end pageup pagedown f1..f12\n"
        .to_string()
}

/// Post the request as real `NSEvent`s into this application's event queue; the number of presses posted.
#[cfg(target_os = "macos")]
pub(crate) fn post(spec: &HwKeySpec, window_number: i64) -> Result<u32, String> {
    use aterm_objc::{Bool, CGPoint, Id, Sel, autoreleasepool, class, sel};

    use crate::appkit;

    /// `NSEvent.h` — `NSEventTypeKeyDown = 10`, `NSEventTypeKeyUp = 11`.
    const NS_EVENT_TYPE_KEY_DOWN: usize = 10;
    const NS_EVENT_TYPE_KEY_UP: usize = 11;

    if window_number == 0 {
        return Err("ERR hwkey: no frontmost AppKit window to post into\n".to_string());
    }
    if crate::platform::Windowed::get().is_none() {
        return Err("ERR hwkey: a headless instance has no AppKit event queue\n".to_string());
    }
    let (Some(chars), Some(ignoring)) = (
        appkit::nsstring(&spec.chars),
        appkit::nsstring(&spec.chars_ignoring),
    ) else {
        return Err("ERR hwkey: could not build the key strings\n".to_string());
    };
    let flags = spec.flags as usize;

    // SAFETY: `+sharedApplication` is `-(id)`; the caller resolved a live window number on the main thread first, so `NSApp` exists and this returns the singleton rather than constructing it off-main.
    let app = unsafe { appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication)) };
    if app.is_null() {
        return Err("ERR hwkey: no shared NSApplication\n".to_string());
    }

    let mut posted = 0u32;
    for i in 0..spec.count {
        if i > 0 && spec.interval_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(spec.interval_ms));
        }
        let _posting = crate::askpass::PROMPT_INPUT
            .posting_permit()
            .map_err(|()| "ERR hwkey: a sudo password sheet is up\n".to_string())?;
        autoreleasepool(|_| {
            // SAFETY: `+processInfo` is `-(id)`, an immortal singleton, and `-systemUptime` is `-(NSTimeInterval)`, a plain scalar getter.
            let ts = unsafe {
                let info = appkit::send_id(class(c"NSProcessInfo").as_id(), sel!(processInfo));
                if info.is_null() {
                    return Err("ERR hwkey: no NSProcessInfo\n".to_string());
                }
                appkit::send_f64(info, sel!(systemUptime))
            };
            // SAFETY: the AppKit event factory with a non-nil characters pair (winit dereferences both unconditionally) and a nil graphics context, exactly as upstream winit's `replace_event` builds a synthetic key event (aterm-window no longer carries it).
            let built = |kind: usize| unsafe {
                let make: unsafe extern "C-unwind" fn(
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
                ) -> Id = aterm_objc::checked_msg();
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
            let key_event = |kind: usize| {
                if spec.press {
                    press_event(spec.keycode, kind == NS_EVENT_TYPE_KEY_DOWN, spec.flags)
                } else {
                    built(kind)
                }
            };
            let down = key_event(NS_EVENT_TYPE_KEY_DOWN);
            if down.is_null() {
                return Err("ERR hwkey: NSEvent construction failed\n".to_string());
            }
            // SAFETY: `-postEvent:atStart:` is `-(void)(NSEvent *, BOOL)` on the shared application with a live event; see the thread-safety note.
            unsafe {
                let post: unsafe extern "C-unwind" fn(Id, Sel, Id, Bool) =
                    aterm_objc::checked_msg();
                post(app, sel!(postEvent:atStart:), down, Bool::NO);
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

/// A key-down (`down`) or key-up `NSEvent` built as a key press is: a CoreGraphics
/// keyboard event for `keycode` with `flags` (`CGEventFlags` share the bit positions of
/// `NSEventModifierFlags`), wrapped by `+[NSEvent eventWithCGEvent:]`, so its
/// `characters` are the current layout's for that key and modifiers and its timestamp is
/// the CoreGraphics event's own (the same seconds-since-boot clock).
#[cfg(target_os = "macos")]
fn press_event(keycode: u16, down: bool, flags: u64) -> aterm_objc::Id {
    use std::ffi::c_void;

    use aterm_objc::{Id, Sel, class, sel};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            keycode: u16,
            keydown: bool,
        ) -> *mut c_void;
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }
    // SAFETY: the CoreGraphics prototypes above are the headers'; a null source is documented.
    unsafe {
        let cg = CGEventCreateKeyboardEvent(std::ptr::null(), keycode, down);
        if cg.is_null() {
            return Id::NIL;
        }
        CGEventSetFlags(cg, flags);
        let wrap: unsafe extern "C-unwind" fn(Id, Sel, *mut c_void) -> Id =
            aterm_objc::checked_msg();
        let event = wrap(class(c"NSEvent").as_id(), sel!(eventWithCGEvent:), cg);
        CFRelease(cg);
        event
    }
}

/// Non-macOS: there is no NSEvent queue to post into, and falling back to the socket path would hide the very stall this verb measures, so it refuses.
#[cfg(not(target_os = "macos"))]
pub(crate) fn post(_spec: &HwKeySpec, _window_number: i64) -> Result<u32, String> {
    Err("ERR hwkey: hardware-path injection is implemented on macOS only\n".to_string())
}

#[cfg(test)]
mod tests {
    use super::{FLAG_CONTROL, FLAG_FUNCTION, FLAG_SHIFT, MAX_COUNT, parse_hwkey};

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
    /// is what the keyboard does.
    #[test]
    fn an_uppercase_letter_shifts_the_same_physical_key() {
        for (plain, shifted) in [("x", "X"), ("1", "!")] {
            let lower = parse_hwkey(plain).unwrap();
            let upper = parse_hwkey(shifted).unwrap();
            assert_eq!(upper.keycode, lower.keycode, "{shifted}");
            assert_eq!(upper.flags, FLAG_SHIFT, "{shifted}");
            assert_eq!(upper.chars_ignoring, shifted);
        }
    }

    /// Control chords carry the C0 byte in `characters` (what AppKit delivers) and
    /// the bare letter in `charactersIgnoringModifiers` (what winit reads for the
    /// logical key).
    #[test]
    fn a_control_chord_carries_the_c0_byte_and_the_bare_letter() {
        let a = parse_hwkey("ctrl+u").unwrap();
        let b = parse_hwkey("u mods=ctrl").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.flags, FLAG_CONTROL);
        assert_eq!(a.chars, "\u{15}");
        assert_eq!(a.chars_ignoring, "u");
    }

    /// Named keys resolve to their AppKit keycode and private-use character, and the ones AppKit
    /// marks as function keys carry that flag.
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

    /// winit unwraps `charactersIgnoringModifiers` with `.expect(...)`, so an empty string there
    /// would panic on dispatch: no accepted spelling produces one.
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

    /// Pacing tokens are additive and bounded; out-of-range or malformed values are refused,
    /// never clamped or defaulted.
    #[test]
    fn pacing_tokens_are_additive_and_bounded() {
        let s = parse_hwkey("x count=200 interval=25").unwrap();
        assert_eq!(s.count, 200);
        assert_eq!(s.interval_ms, 25);
        assert_eq!(s.keycode, parse_hwkey("x").unwrap().keycode);

        assert!(parse_hwkey("x count=0").is_err());
        assert!(parse_hwkey(&format!("x count={}", MAX_COUNT + 1)).is_err());
        assert!(parse_hwkey("x count=1oo").is_err());
        assert!(parse_hwkey("x interval=100000").is_err());
        assert!(parse_hwkey("x mods=bogus").is_err());
    }

    /// `shape=press` asks for the key-press shape and changes nothing else about the
    /// key; the default is the built shape, and an unknown shape is refused.
    #[test]
    fn the_press_shape_is_asked_for_by_name_and_keeps_the_key() {
        let built = parse_hwkey("b mods=alt").unwrap();
        assert!(!built.press);
        let press = parse_hwkey("b mods=alt shape=press").unwrap();
        assert!(press.press);
        assert_eq!((press.keycode, press.flags), (11, super::FLAG_OPTION));
        assert_eq!(press.chars_ignoring, built.chars_ignoring);
        assert_eq!(parse_hwkey("b mods=alt shape=built").unwrap(), built);
        assert!(parse_hwkey("b shape=cg").is_err());
    }

    /// Unknown / multi-character bodies are refused rather than guessed at.
    #[test]
    fn a_malformed_body_is_refused() {
        assert!(parse_hwkey("").is_err());
        assert!(parse_hwkey("   ").is_err());
        assert!(parse_hwkey("notakey").is_err());
        assert!(parse_hwkey("ab").is_err());
        assert!(parse_hwkey("ctrl+").is_err());
    }
}
