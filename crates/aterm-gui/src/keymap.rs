// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard event → engine key input: the pure decisions behind a key press.
//!
//! Every key goes through the engine encoder (`aterm_types::keyboard`); this module
//! builds the encoder's input (`build_key_input`) and answers the IME and lock-key
//! questions, testable without a window or event loop. The winit→engine key table
//! is the `aterm-winit-keymap` crate (a crate, not an `aterm-types` feature, because
//! workspace feature unification would link AppKit into every `aterm-types` user).

use aterm_types::keyboard::{self, KeyboardMode, Modifiers};
use aterm_window::event::KeyEvent;
use aterm_window::keyboard::{Key as WinitKey, ModifiersState};

/// Translate winit's [`ModifiersState`] into the engine's [`Modifiers`], Super/Cmd
/// included (the encoder uses it for the kitty/xterm modifier value and
/// `modifyOtherKeys`).
#[must_use]
pub(crate) fn modifiers_from_winit(mods: ModifiersState) -> Modifiers {
    let mut out = Modifiers::empty();
    if mods.shift_key() {
        out |= Modifiers::SHIFT;
    }
    if mods.control_key() {
        out |= Modifiers::CTRL;
    }
    if mods.alt_key() {
        out |= Modifiers::ALT;
    }
    if mods.super_key() {
        out |= Modifiers::SUPER;
    }
    out
}

/// The lock-key modifiers (Caps Lock / Num Lock) for the kitty modifier byte.
///
/// winit's [`ModifiersState`] carries only Shift/Ctrl/Alt/Super, so the kitty
/// `CAPS_LOCK`/`NUM_LOCK` bits (folded in by `Modifiers::kitty_encoded`) come from a
/// platform query: on macOS the kernel's HID system ([`hid_lock_state`]). Mac
/// hardware has no Num Lock, so only Caps Lock is reported.
///
/// The query must not go through WindowServer. AppKit's global modifier flags open
/// a SkyLight connection on first use, and WindowServer's main thread then runs a
/// synchronous TCC Input-Monitoring preflight that `readdir`s the caller's
/// executable directory; from a test binary in a huge `target/debug/deps` that
/// outran the WindowServer watchdog, which killed every GUI session on the machine.
/// Tests and headless instances do not even make this kernel read: they get
/// [`no_lock_modifiers`] through the injected `App::lock_modifiers` field (this
/// module's own test calls it once, deliberately).
#[cfg(target_os = "macos")]
#[must_use]
fn lock_modifiers() -> Modifiers {
    match hid_lock_state::caps_lock() {
        Some(true) => Modifiers::CAPS_LOCK,
        Some(false) | None => Modifiers::empty(),
    }
}

/// The lock-key source an `App` holds: the platform read ([`lock_modifiers`])
/// windowed, [`no_lock_modifiers`] headless. The platform read is private to this
/// module, so a headless-constructible path can reach it only through this choice.
pub(crate) fn lock_modifier_source(headless: bool) -> fn() -> Modifiers {
    if headless {
        no_lock_modifiers
    } else {
        lock_modifiers
    }
}

/// The lock-state source for headless instances and unit tests: always empty. A
/// headless `App` has no live keyboard, and a test must encode the same bytes
/// whatever the machine's Caps Lock; it also keeps a test binary off the platform
/// lock-key path (see [`lock_modifiers`]).
#[must_use]
pub(crate) fn no_lock_modifiers() -> Modifiers {
    Modifiers::empty()
}

/// Caps Lock from the kernel's HID system, the macOS lock-key source that never
/// involves WindowServer.
///
/// IOKit is a system framework (no crate on the dependency surface); five functions,
/// declared against `IOKitLib.h`/`hidsystem/IOHIDLib.h`. The user client is opened
/// once per process and cached (`io_connect_t` is a process-wide mach port); a failed
/// open is cached too and every read is `None`, so Caps Lock goes unreported. The
/// open (`kIOHIDParamConnectType`) and the read both succeed unentitled.
#[cfg(target_os = "macos")]
mod hid_lock_state {
    use std::ffi::{c_char, c_int, c_void};
    use std::sync::OnceLock;

    // SDK types (mach ports are 32-bit names; kern_return_t is an int).
    type MachPort = u32;
    type IoObject = MachPort;
    type IoService = IoObject;
    type IoConnect = IoObject;
    type KernReturn = c_int;

    /// `IOKitLib.h`: MACH_PORT_NULL selects the default main port.
    const IO_MAIN_PORT_DEFAULT: MachPort = 0;
    /// `hidsystem/IOHIDShared.h`: `kIOHIDParamConnectType = 1`.
    const IOHID_PARAM_CONNECT_TYPE: u32 = 1;
    /// `hidsystem/IOHIDParameter.h`: `kIOHIDCapsLockState = 0x1`.
    const IOHID_CAPS_LOCK_STATE: c_int = 0x0000_0001;
    /// `hidsystem/IOHIDShared.h`: `kIOHIDSystemClass "IOHIDSystem"`.
    const IOHID_SYSTEM_CLASS: &[u8] = b"IOHIDSystem\0";
    const KERN_SUCCESS: KernReturn = 0;

    unsafe extern "C" {
        /// libSystem's own task port (`mach/mach_init.h`); declared here rather
        /// than through `libc::mach_task_self`, which libc deprecates.
        static mach_task_self_: MachPort;
    }
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        /// Returns a CFMutableDictionaryRef, CONSUMED by `IOServiceGetMatchingService`.
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: MachPort, matching: *mut c_void) -> IoService;
        fn IOServiceOpen(
            service: IoService,
            owning_task: MachPort,
            connect_type: u32,
            connect: *mut IoConnect,
        ) -> KernReturn;
        fn IOObjectRelease(object: IoObject) -> KernReturn;
        fn IOHIDGetModifierLockState(
            handle: IoConnect,
            selector: c_int,
            state: *mut bool,
        ) -> KernReturn;
    }

    /// The process-wide user client, `None` once the open has failed.
    fn connection() -> Option<IoConnect> {
        static CONN: OnceLock<Option<IoConnect>> = OnceLock::new();
        *CONN.get_or_init(|| {
            // SAFETY: `IOServiceMatching` takes a NUL-terminated class name and
            // returns an owned dictionary that `IOServiceGetMatchingService`
            // consumes, so nothing leaks on either path; a NULL dictionary makes the
            // lookup return 0. `IOServiceOpen` writes the connect port only on
            // success; `mach_task_self_` is libSystem's cached task port. The
            // service ref is released after the open, which holds its own. All are
            // thread-safe kernel RPCs.
            unsafe {
                let matching = IOServiceMatching(IOHID_SYSTEM_CLASS.as_ptr().cast::<c_char>());
                if matching.is_null() {
                    return None;
                }
                let service = IOServiceGetMatchingService(IO_MAIN_PORT_DEFAULT, matching);
                if service == 0 {
                    return None;
                }
                let mut connect: IoConnect = 0;
                let kr = IOServiceOpen(
                    service,
                    mach_task_self_,
                    IOHID_PARAM_CONNECT_TYPE,
                    &raw mut connect,
                );
                IOObjectRelease(service);
                (kr == KERN_SUCCESS && connect != 0).then_some(connect)
            }
        })
    }

    /// `Some(state)` of the Caps Lock LED, `None` when the HID system is
    /// unreachable (no user client, or the read failed).
    #[must_use]
    pub(super) fn caps_lock() -> Option<bool> {
        let conn = connection()?;
        let mut state = false;
        // SAFETY: `conn` is a live user-client port from `connection()`; the
        // out-pointer is a valid `bool` for the call's duration; the selector is
        // the SDK constant. Synchronous kernel RPC, safe from any thread.
        let kr = unsafe { IOHIDGetModifierLockState(conn, IOHID_CAPS_LOCK_STATE, &raw mut state) };
        (kr == KERN_SUCCESS).then_some(state)
    }
}

/// Windows: the live toggle state from user32 `GetKeyState` (the low bit is the
/// toggled LED state for `VK_CAPITAL` / `VK_NUMLOCK`). Tiny FFI, as in
/// `clipboard_win`: user32 is on the approved list and std has no alternative.
#[cfg(windows)]
#[must_use]
fn lock_modifiers() -> Modifiers {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetKeyState(vkey: i32) -> i16;
    }
    const VK_CAPITAL: i32 = 0x14;
    const VK_NUMLOCK: i32 = 0x90;
    let mut out = Modifiers::empty();
    // SAFETY: GetKeyState is a pure per-thread keyboard-state query with no
    // preconditions; reading the toggle bit is valid from any thread.
    if unsafe { GetKeyState(VK_CAPITAL) } & 1 != 0 {
        out |= Modifiers::CAPS_LOCK;
    }
    // SAFETY: as above.
    if unsafe { GetKeyState(VK_NUMLOCK) } & 1 != 0 {
        out |= Modifiers::NUM_LOCK;
    }
    out
}

/// Fallback for [`lock_modifiers`] (not macOS, not Windows): winit exposes no
/// Caps/Num Lock LED state, so report none until a platform-specific source is
/// wired.
#[cfg(not(any(target_os = "macos", windows)))]
#[must_use]
fn lock_modifiers() -> Modifiers {
    Modifiers::empty()
}

/// Whether this press is a bare modifier or lock key (Shift, Control, Alt/Option,
/// Super/Command, Hyper, Meta, Caps/Num/Scroll Lock), which expresses no typing
/// intent.
///
/// The window half of the press-path snap (`cancel_press_scroll_motion`) is
/// lock-free and so lives outside the seam; it shares
/// `aterm_types::keyboard::is_modifier_or_lock_key` with the seam's
/// `PressClass::inert_modifier`, so a key cannot be inert on one side and disturbing
/// on the other. Reads `logical_key`, not `key_without_modifiers()`: on macOS the
/// modifier-state snapshot is still stale when the bare modifier's `KeyboardInput`
/// arrives.
#[must_use]
pub(crate) fn press_is_inert(ev: &KeyEvent) -> bool {
    if aterm_winit_keymap::map_logical_key(&ev.logical_key)
        .as_ref()
        .is_some_and(keyboard::is_modifier_or_lock_key)
    {
        return true;
    }
    // Modifier and lock keys the engine has no `NamedKey` for, so `map_logical_key`
    // returns `None`: `AltGraph`, `Fn`/`FnLock` and `Symbol`/`SymbolLock`. They send
    // no PTY bytes and never reach the seam (`build_key_input` returns `None`), but
    // without this arm resting a finger on AltGr would stop a momentum scroll that
    // Shift leaves running.
    matches!(
        &ev.logical_key,
        WinitKey::Named(
            aterm_window::keyboard::NamedKey::AltGraph
                | aterm_window::keyboard::NamedKey::Fn
                | aterm_window::keyboard::NamedKey::FnLock
                | aterm_window::keyboard::NamedKey::Symbol
                | aterm_window::keyboard::NamedKey::SymbolLock
        )
    )
}

/// Whether a direct key send must be suppressed because an IME composition (CJK,
/// dead key) is active: a non-empty `preedit` means the keystrokes belong to the
/// composer and the text arrives via `Ime::Commit`, so sending them too would
/// double the input.
#[must_use]
pub(crate) fn suppress_direct_send(preedit: &str) -> bool {
    !preedit.is_empty()
}

/// Whether an input method's committed text (`Ime::Commit`) reaches the PTY as a
/// paste rather than as typed keys: when it holds a control character (C0, DEL or
/// C1, Unicode's `Cc`), unless the whole commit is one C0 or DEL character.
///
/// Fail closed (`docs/AUDIT-boundaries-2026-10-09.md`, L2): a commit is text the
/// person did not type key by key. A clipboard-history input method (fcitx5's
/// Clipboard addon) commits an entry a program's OSC 52 write can plant, and
/// dictation commits a whole phrase; typed, a line break would run the line and an
/// ESC end a bracketed paste early. As a paste (`App::deliver_paste`) it is
/// sanitized, bracketed when the program asked, and checked by the multi-line guard
/// under the guard's own rule. A dictated "new line" pastes too, on purpose: a
/// misheard line must not run silently. Printable commits (CJK, accents, emoji)
/// keep the typed-key path byte for byte.
///
/// A lone C0 or DEL is one key: X11 commits a key that way while an XIM preedit is
/// open (winit sends a key event only for `keycode != 0 && !is_composing`, so Return
/// arrives as the commit `"\r"`, Tab `"\t"`, BackSpace `"\x08"`), and Wayland's
/// `commit_string` is unfiltered too. Pasted, Return would not run the line, Tab
/// would not complete and the sanitizer would drop BackSpace. One character cannot
/// carry planted text plus a line break (two) or `ESC [ 201 ~` (six). A lone C1
/// still pastes: no key types one.
#[must_use]
pub(crate) fn committed_text_is_paste(text: &str) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(only), None) => only.is_control() && !only.is_ascii(),
        _ => text.chars().any(char::is_control),
    }
}

/// Encode committed composition text (`Ime::Commit`) for the PTY. An input method
/// brings only commits with no control character here ([`committed_text_is_paste`]
/// routes the rest to the paste path), but the encoder takes whatever it is handed
/// (the on-key text fallback, a controller's typed turn).
///
/// - Legacy and non-report-all modes: each character is a `Character` key through
///   the engine, so the commit goes out as plain UTF-8.
/// - Kitty `REPORT_ALL_KEYS_AS_ESC` + `REPORT_ASSOCIATED_TEXT`: one key-0 event with
///   the codepoints colon-joined in the text field, `ESC[0;1;cp1:cp2:…u`, the spec's
///   keyless-text form ("If no known key is associated with the text the key number
///   0 must be used"). Control codepoints are dropped (kitty suppresses text that
///   begins with one); an all-control commit emits nothing.
/// - `REPORT_ALL_KEYS_AS_ESC` alone: one keyless event (`ESC[0u`), since no field
///   can carry the text and an IME commit has no physical key. A fabricated key per
///   codepoint would pair with no release and can surface as literal bracket-number
///   text in a client's fallback decoder.
#[must_use]
pub(crate) fn encode_committed_text(text: &str, mode: KeyboardMode) -> Vec<u8> {
    if mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC) {
        let mut out = Vec::new();
        for c in text.chars().filter(|c| !c.is_control()) {
            if mode.contains(KeyboardMode::REPORT_ASSOCIATED_TEXT) {
                if out.is_empty() {
                    out.extend_from_slice(b"\x1b[0;1;");
                } else {
                    out.push(b':');
                }
                out.extend_from_slice((c as u32).to_string().as_bytes());
            } else if out.is_empty() {
                out.extend_from_slice(b"\x1b[0");
            }
        }
        if !out.is_empty() {
            out.push(b'u');
        }
        return out;
    }
    let mut out = Vec::new();
    for c in text.chars() {
        out.extend_from_slice(&keyboard::encode_key(
            &keyboard::Key::Character(c),
            Modifiers::empty(),
            mode,
        ));
    }
    out
}

/// The engine-neutral fields of an [`InputEvent::Key`](crate::input::InputEvent::Key)
/// for a press: `(Key, Modifiers, base_layout, layout_text)`, the last the layout's
/// own character for a text press ([`layout_text`]). The seam (`App::input`) is the
/// only caller of the encoder (`encode_key_with_text`), so a human key and a
/// `key`/`ctrl` verb that build the same fields send the same bytes, including the
/// kitty `REPORT_ALTERNATE_KEYS` field `base_layout` carries. Bare modifiers map
/// (winit's sideless `Shift`/`Control`/… become `ShiftLeft`/`ControlLeft`/…) and the
/// encoder decides what they produce. `None` only for keys with no engine mapping (a
/// dead key, `Key::Unidentified`).
///
/// The keypad is asked first, on every platform. winit has no keypad key (KP_5 is
/// `Character("5")`, KP_Enter `Named(Enter)`; only `location` says keypad), so
/// [`aterm_winit_keymap::map_numpad_key`] resolves it from `location` and
/// `logical_key` (which carries NumLock). On Linux that also fixes the digits:
/// `key_without_modifiers()` there is xkb's level-0 keysym, the NumLock-off symbol.
///
/// With no mode set a keypad key types what the main block would. Under DECKPAM
/// (set by `smkx`, so vim, less and tmux) the digits send xterm's `SS3 p..y` and
/// KP_Enter `SS3 M`; vim maps unmapped `<k0>`..`<k9>` back to digits, `less` does
/// not. NumLock does not override the mode (see `encode_numpad_named_legacy`).
/// Kitty disambiguate reports the keypad's own `CSI 57399..57427 u`; a kitty mode
/// without it folds to the main-block twin unless DECKPAM is also set.
///
/// The keybinding lookup, the tab-menu chord and every overlay read the
/// MAIN-BLOCK key a keypad press stands for, so a binding on Enter still fires
/// for KP_Enter and one on `1` fires for a NumLock-on KP_1: the lookup through
/// [`crate::app_input::base_logical_key`] (which takes `logical_key` on the
/// keypad, not the level-0 keysym), the tab menu through
/// `tab_menu::nav_for_key`, and the rest through `InputEvent::keypad_folded`.
/// The two consumers that read the BUILT key for its meaning rather than its
/// bytes — the native pages (`App::native_input_event`, the other caller of
/// this function's result) and the seam's press classifier (`classify_press` /
/// `is_plain_enter`) — fold it the same way, so KP_5 still types `5` into a
/// Settings field and KP_Enter still submits a turn.
// `key_without_modifiers()` (the `KeyEventExtModifierSupplement` trait) is
// available on macOS AND on the Linux X11/Wayland backends, so BOTH map the
// layout BASE key — Alt+<key>/AltGr/Shifted-compose all encode the unshifted key
// the engine expects (mirrors `app_input::base_logical_key`). Without this, X11
// users got the layout-composed/shifted glyph in the PTY encoding.
//
// …BUT THE BASE KEY IS NOT WHAT A TEXT PRESS TYPES on any layout but US. The
// engine re-derives Shift from a US table and the window used to drop the
// OS's answer, so German Shift+2 wrote '@' (the layout types '"'), AZERTY's
// digits were unreachable, Russian Shift+ф wrote 'ф' and Caps Lock + a wrote
// 'a'. [`layout_text`] carries the layout's character beside the base key;
// the encoder writes it for the text press, sends it after ESC for an Alt
// chord, takes a Ctrl chord's control byte from it, and keeps the base key
// for the kitty key code. `option_composes` is
// [`crate::platform::option_composes`]: an Option chord's text is then the
// composed glyph, not the layout's character.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[must_use]
pub(crate) fn build_key_input(
    ev: &KeyEvent,
    mods: Modifiers,
    option_composes: bool,
) -> Option<(keyboard::Key, Modifiers, Option<char>, Option<char>)> {
    use aterm_window::platform::modifier_supplement::KeyEventExtModifierSupplement;
    let base_layout = aterm_winit_keymap::base_layout_key_for(ev.physical_key);
    if let Some(key) =
        aterm_winit_keymap::map_numpad_key(ev.physical_key, &ev.logical_key, ev.location)
    {
        // A keypad key never composes (see `encode_key_with_layout`).
        return Some((key, mods, base_layout, None));
    }
    let key = aterm_winit_keymap::map_logical_key(&ev.key_without_modifiers())?;
    // A chord's letter takes Shift's case, never the lock's: on X11 and Wayland xkb
    // cancels Shift against Caps Lock, so the chord reads the press without the lock
    // (`text_ignoring_lock`, which is `text` on macOS).
    let text = if mods.intersects(Modifiers::CTRL | Modifiers::ALT) {
        ev.text_ignoring_lock()
    } else {
        ev.text.as_deref()
    };
    let text = layout_text(text, &key, mods, option_composes);
    Some((
        aterm_winit_keymap::sided_modifier(key, ev.location),
        mods,
        base_layout,
        text,
    ))
}

/// The character the user's ACTIVE layout types for a press at its Shift and
/// lock level, Ctrl and Alt aside, when it is not what the engine would derive
/// from the unshifted `key` on its own ([`keyboard::typed_character`]: the US
/// Shift table, Caps Lock capitals).
///
/// `text` is winit's text for the press (`KeyEvent::text`): xkb's level-and-
/// lock-resolved keysym on X11/Wayland, where neither Ctrl nor Alt changes the
/// level; on macOS AppKit's `-charactersIgnoringModifiers` for a Ctrl chord
/// (the backend reads a Control or Command chord without them), and for an
/// Option chord when the window treats Option as Alt
/// ([`crate::platform::option_as_alt_for`]), `-characters` otherwise. So the
/// field serves a text press, an Alt chord (ESC + the layout's character:
/// German Alt+Shift+2 is `ESC "`, not the US table's `ESC @`) and a Ctrl chord
/// (German Ctrl+Shift+ß is `?`'s DEL). Not when `option_composes`, where an
/// Option chord's text is what Option composed (Option+a, "å"), and never for
/// Super, Meta or Hyper, which the engine encodes from the base key. One
/// printable character only: a dead key's empty text, a multi-character
/// commit and a control code carry nothing.
///
/// `None` whenever the derivation already agrees (US Shift+a is 'A'), so a
/// window press and the controller's `key a mods=shift` stay the same event
/// on a US layout, and the field is set exactly when it changes the bytes:
/// German Shift+2 (`"`), AZERTY Shift+& (`1`), Russian Shift+ф (`Ф`), Caps
/// Lock (whose state winit's modifiers never carry; macOS adds it only later,
/// and the Linux backends not at all) and, on X11/Wayland, a level-3 (AltGr)
/// character or an xkb compose result, neither of which winit reports as a
/// modifier. On Windows `key` is the composed glyph rather than the unshifted
/// key ([`windows_composed_text`]); the same comparison holds.
///
/// On a Ctrl or Alt chord whose text is the key's letter in either case, the
/// text is that letter in Shift's case, never the lock's ([`chord_letter`]):
/// the lock's case (Caps Lock, or xkb's Shift+Lock level) is dropped, as the
/// encoder's own chord rule and kitty's legacy encoding ignore the lock
/// modifiers, and Shift's capital is the layout's, which the US table cannot
/// derive for a letter outside it. So Caps Lock + Alt+b stays ESC b, Turkish
/// Caps Lock + Alt+ı is ESC ı, Russian Alt+Shift+и is ESC И, and Ctrl+c under
/// modifyOtherKeys level 2 still reports 'c'. The lock's state is not needed
/// here. Where xkb cancelled Shift against the lock (Turkish Caps Lock +
/// Alt+Shift+i typed `i`), the caller passes a chord the press's text without
/// the lock (winit's `text_ignoring_lock`, [`build_key_input`]), so the chord
/// is ESC İ, as without the lock.
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
#[must_use]
pub(crate) fn layout_text(
    text: Option<&str>,
    key: &keyboard::Key,
    mods: Modifiers,
    option_composes: bool,
) -> Option<char> {
    let keyboard::Key::Character(base) = key else {
        return None;
    };
    if mods.intersects(Modifiers::SUPER | Modifiers::META | Modifiers::HYPER)
        || (option_composes && mods.contains(Modifiers::ALT) && !mods.contains(Modifiers::CTRL))
    {
        return None;
    }
    let mut chars = text?.chars();
    let (Some(glyph), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let glyph = if mods.intersects(Modifiers::CTRL | Modifiers::ALT) {
        chord_letter(glyph, *base, mods.contains(Modifiers::SHIFT))
    } else {
        glyph
    };
    (!glyph.is_control() && glyph != keyboard::typed_character(*base, mods)).then_some(glyph)
}

/// The character a chord on key `base` names when the layout typed `glyph`: when `glyph` is
/// `base`'s letter in either case ([`same_letter`]), that letter in Shift's case (`shift`),
/// never the lock's. Shift's capital is `glyph` when the layout typed a capital (Turkish
/// Shift+i is 'İ'), else `base`'s own one-character capital. Any other `glyph` (German
/// Shift+2's '"') is itself.
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
fn chord_letter(glyph: char, base: char, shift: bool) -> char {
    if !same_letter(glyph, base) {
        return glyph;
    }
    if !shift {
        return base;
    }
    if glyph != base && glyph.is_uppercase() {
        return glyph;
    }
    let mut capital = base.to_uppercase();
    match (capital.next(), capital.next()) {
        (Some(capital), None) => capital,
        _ => glyph,
    }
}

/// Whether `a` and `b` are one character or one letter in two cases: by Unicode's default
/// lowercase or uppercase, which between them also pair Turkish 'I' with 'ı', plus the
/// Turkic dotted pair 'İ' and 'i', which neither default mapping takes to the other.
#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
fn same_letter(a: char, b: char) -> bool {
    a == b
        || a.to_lowercase().eq(b.to_lowercase())
        || a.to_uppercase().eq(b.to_uppercase())
        || matches!((a, b), ('İ', 'i') | ('i', 'İ'))
}

/// [`build_key_input`] on Windows, where `KeyEventExtModifierSupplement` exists
/// but must not be used for the ENCODE path: the Windows
/// backend filters Ctrl/Alt out of `ModifiersState` while AltGr is held, so
/// `key_without_modifiers()` would encode the layout BASE key ('q') with no
/// modifiers instead of the AltGr-composed glyph ('@' on de-DE) the app expects.
/// The binding-LOOKUP path (`app_input::base_logical_key`) does use the
/// extension on Windows; only PTY encoding keeps the composed `logical_key` —
/// except as the last-resort rescue for a Ctrl+Alt chord the layout composed
/// into nothing, which [`windows_key_input`] documents. The keypad is asked
/// first here too, ahead of that rescue: a keypad key is never AltGr-composed,
/// so the strip and the Ctrl+Alt fallback never see one.
///
/// The key is the COMPOSED one, Shift already applied, so the layout's own
/// text rides beside it only where the engine's US Shift table would type
/// something else ([`windows_composed_text`]), and a Shift chord whose glyph
/// the layout already shifted drops the SHIFT bit the encoder would apply a
/// second time. Two presses build no key: an Alt-code digit
/// ([`windows_alt_code_digit`]), and a dead key that did not compose, whose
/// two characters reach the PTY as the text fallback ([`windows_text_commit`]).
/// `_option_composes` is unread: no Windows modifier composes like macOS's
/// Option.
#[cfg(windows)]
#[must_use]
pub(crate) fn build_key_input(
    ev: &KeyEvent,
    mods: Modifiers,
    _option_composes: bool,
) -> Option<(keyboard::Key, Modifiers, Option<char>, Option<char>)> {
    let base_layout = aterm_winit_keymap::base_layout_key_for(ev.physical_key);
    if windows_alt_code_digit(&ev.logical_key, ev.location, mods) {
        return None;
    }
    if let Some(key) =
        aterm_winit_keymap::map_numpad_key(ev.physical_key, &ev.logical_key, ev.location)
    {
        return Some((key, mods, base_layout, None));
    }
    if windows_text_commit(&ev.logical_key, ev.text.as_deref(), mods) {
        return None;
    }
    windows_key_input(
        &ev.logical_key,
        &key_without_modifiers(ev),
        base_layout,
        mods,
        layout_shift_state,
    )
    .map(|(key, mods, base)| {
        let (mods, text) = windows_composed_text(&ev.logical_key, &key, mods, ev.text.as_deref());
        (
            aterm_winit_keymap::sided_modifier(key, ev.location),
            mods,
            base,
            text,
        )
    })
}

/// A NumLock-on keypad digit pressed with Alt alone: one digit of a Windows Alt
/// code (Alt, 0, 2, 3, 3 types `é`). Windows composes the character itself and
/// posts it when Alt is released, which the window delivers as an input-method
/// commit; the digits type nothing. Windows posts no character for the digits
/// themselves, so winit names each `Unidentified(VK_NUMPADn)`, and keypad 5 then
/// read as the NumLock-off centre key and sent Alt+KP_Begin; a `Character`
/// digit (a controller's, or a layout that does post one) would have sent
/// Meta-digit, PSReadLine's digit argument. With NumLock off the keypad digits
/// are navigation keys, and Alt+keypad-arrow is pane focus.
///
/// Pure, and compiled for every test build — see [`windows_key_input`].
#[cfg(any(windows, test))]
fn windows_alt_code_digit(
    logical_key: &WinitKey,
    location: aterm_window::keyboard::KeyLocation,
    mods: Modifiers,
) -> bool {
    /// `VK_NUMPAD0`..`VK_NUMPAD9` (winuser.h).
    const VK_NUMPAD_DIGITS: std::ops::RangeInclusive<u16> = 0x60..=0x69;
    let digit = match logical_key {
        WinitKey::Character(s) => matches!(s.as_bytes(), [b'0'..=b'9']),
        WinitKey::Unidentified(aterm_window::keyboard::NativeKey::Windows(vk)) => {
            VK_NUMPAD_DIGITS.contains(vk)
        }
        _ => false,
    };
    location == aterm_window::keyboard::KeyLocation::Numpad
        && digit
        && mods & (Modifiers::SHIFT | Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER)
            == Modifiers::ALT
}

/// Whether a press goes out as its text because a dead key changed what it
/// types, which its key does not say:
///
/// * A dead key that did not compose with the key after it: US-International
///   `'` then `s` types `'s`. Windows posts both characters for the second
///   press, and winit, finding two graphemes, reports the plain `s` as its
///   logical key, so encoding the key alone typed `its` for `it's`.
/// * A dead key then Space, the way to type the accent itself: German `^` then
///   Space types `^`, US-International `'` then Space `'`. Windows posts the
///   accent for the Space press, which winit still names `Space`, so encoding
///   the key typed a space and the accent was lost (found on recorded key
///   messages, `keymap_win_replay_tests`).
///
/// The text carries what Windows typed, and with no chord modifier held the
/// caller's text fallback sends it. A composed press (`'` then `e`) is one
/// character on its own key and takes the key path.
///
/// Pure, and compiled for every test build — see [`windows_key_input`].
#[cfg(any(windows, test))]
fn windows_text_commit(logical_key: &WinitKey, text: Option<&str>, mods: Modifiers) -> bool {
    let chord = Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER;
    let Some(text) = text.filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
    else {
        return false;
    };
    !mods.intersects(chord)
        && match logical_key {
            WinitKey::Character(_) => text.chars().nth(1).is_some(),
            WinitKey::Named(aterm_window::keyboard::NamedKey::Space) => text != " ",
            _ => false,
        }
}

/// What an auto-repeat of a press that went out as text types, where that is not
/// the press's own text: `None` when a repeat replays the press. A dead key that
/// did not compose ([`windows_text_commit`]) types both characters once, and the
/// press clears the dead key, so Windows' own repeats post the key's character
/// alone: `'` then a held `s` types `'sss`, not `'s's's`, and `^` then a held
/// Space `^  `.
#[cfg(windows)]
#[must_use]
pub(crate) fn text_press_repeat(ev: &KeyEvent, mods: Modifiers) -> Option<String> {
    windows_text_repeat(&ev.logical_key, ev.text.as_deref(), mods)
}

/// [`text_press_repeat`] off Windows, where a repeat replays the press.
#[cfg(not(windows))]
#[must_use]
pub(crate) fn text_press_repeat(_ev: &KeyEvent, _mods: Modifiers) -> Option<String> {
    None
}

/// [`text_press_repeat`]'s answer from the press's parts: the logical key's own
/// character, which winit reports for the second of the two.
///
/// Pure, and compiled for every test build — see [`windows_key_input`].
#[cfg(any(windows, test))]
fn windows_text_repeat(
    logical_key: &WinitKey,
    text: Option<&str>,
    mods: Modifiers,
) -> Option<String> {
    if !windows_text_commit(logical_key, text, mods) {
        return None;
    }
    match logical_key {
        WinitKey::Character(own) => Some(own.to_string()),
        WinitKey::Named(aterm_window::keyboard::NamedKey::Space) => Some(" ".to_owned()),
        _ => None,
    }
}

/// The modifiers and layout text a Windows press carries, given the COMPOSED
/// key ([`windows_key_input`]) and winit's text for the press.
///
/// The engine derives Shift from the US table ([`keyboard::typed_character`]),
/// which leaves every glyph the US layout's Shift produces as it is (`?` stays
/// `?`), so a US press needs nothing. Other layouts shift to glyphs the US table
/// shifts again: de-DE Shift+7 is `/`, which typed `?`, and AZERTY Shift+& is
/// `1`, which typed `!`, so an AZERTY user could not type a digit.
///
/// * A text press (no Ctrl, Alt or Super) carries the layout's character
///   ([`layout_text`]) when the derivation differs, and also whenever Shift is
///   held over a lowercase glyph: Caps Lock + Shift types lowercase on Windows,
///   and the encoder, which adds Caps Lock's bit after this, would capitalise it.
/// * A chord keeps the composed glyph and loses the SHIFT bit when the US table
///   would shift that glyph again (de-DE Alt+Shift+7 is `ESC /`, not `ESC ?`).
///   Letters keep it: Shift on a letter is the chord's own (Alt+Shift+O).
///
/// Pure, and compiled for every test build — see [`windows_key_input`].
#[cfg(any(windows, test))]
fn windows_composed_text(
    logical_key: &WinitKey,
    key: &keyboard::Key,
    mods: Modifiers,
    text: Option<&str>,
) -> (Modifiers, Option<char>) {
    let keyboard::Key::Character(glyph) = key else {
        return (mods, None);
    };
    let chord =
        Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER | Modifiers::META | Modifiers::HYPER;
    if !mods.intersects(chord) {
        let caps_inverted = mods.contains(Modifiers::SHIFT)
            && text.is_some_and(|text| {
                let mut chars = text.chars();
                matches!((chars.next(), chars.next()), (Some(c), None) if c.is_lowercase())
            });
        let text = layout_text(text, key, mods, false).or_else(|| {
            caps_inverted
                .then(|| text.and_then(|t| t.chars().next()))
                .flatten()
        });
        return (mods, text);
    }
    // Only the composed glyph is already shifted: the Ctrl+Alt rescue hands
    // back the unshifted base key, whose Shift is the chord's.
    let composed = matches!(logical_key, WinitKey::Character(s) if s.chars().eq([*glyph]));
    let reshifted = composed
        && mods.contains(Modifiers::SHIFT)
        && !glyph.is_alphabetic()
        && keyboard::shifted_character(*glyph, mods) != Some(*glyph);
    if reshifted {
        (mods & !Modifiers::SHIFT, None)
    } else {
        (mods, None)
    }
}

/// The layout base key of a press, for the Ctrl+Alt rescue in [`windows_key_input`].
#[cfg(windows)]
fn key_without_modifiers(ev: &KeyEvent) -> WinitKey {
    use aterm_window::platform::modifier_supplement::KeyEventExtModifierSupplement;
    ev.key_without_modifiers()
}

/// `VkKeyScanExW`'s shift-state bits for Ctrl+Alt, i.e. AltGr (`winuser.h`: 1 =
/// Shift, 2 = Ctrl, 4 = Alt). A header constant, so compiled for tests too.
#[cfg(any(windows, test))]
const SHIFT_STATE_CTRL_ALT: u8 = 0b0000_0110;

/// The Windows half of [`build_key_input`], pure so the layout cases are testable
/// without a window: `logical_key` is the layout-composed key, `base_key`
/// `key_without_modifiers()`, `base_layout` the physical key's US-QWERTY identity,
/// and `layout_shift_state` answers `VkKeyScanExW`'s "which modifiers does this
/// character need on the current layout?".
///
/// winit's `remove_only_ctrl` keeps CONTROL in the layout lookup whenever ALT is
/// also down, so a Ctrl+Alt press resolves against the layout's AltGr table:
///
/// 1. AltGr as LCtrl+LAlt. winit clears Ctrl/Alt only while the right Alt is down,
///    but Windows accepts LCtrl+LAlt as AltGr too, so de-DE LCtrl+LAlt+8 arrives as
///    `Character('[')` with CTRL|ALT set and would encode ESC ESC. When the layout
///    says the character needs Ctrl+Alt, the bits are stripped; no US-QWERTY
///    character does, so US never takes this path.
/// 2. A genuine Ctrl+Alt chord. Ctrl+Alt+A resolves to a shift state no layout
///    defines for letters, so winit reports `Unidentified`; falling back to the
///    base key gives the ESC ^A macOS and Linux send. Without ALT, winit already
///    drops CONTROL and hands back the base key.
///
/// COMPILED FOR WINDOWS AND FOR EVERY TEST BUILD, and that is deliberate. This
/// function is named for the platform whose behaviour it encodes, not for a
/// platform API it calls: it takes `layout_shift_state` as a PARAMETER precisely
/// so the layout cases are decidable without a window — the only real FFI,
/// [`layout_shift_state`], is separately `#[cfg(windows)]` and stays that way.
/// While a `not(any(macos, linux))` gate sat here, the body and its four
/// de-DE/AltGr regression tests were read by NO compiler in this repository: the
/// host suite gated them out on both machines this team owns. `test` in the cfg
/// is what keeps them compiled and run on every host, where nothing calls this
/// outside `mod tests`.
#[must_use]
#[cfg(any(windows, test))]
fn windows_key_input(
    logical_key: &WinitKey,
    base_key: &WinitKey,
    base_layout: Option<char>,
    mods: Modifiers,
    layout_shift_state: impl Fn(char) -> Option<u8>,
) -> Option<(keyboard::Key, Modifiers, Option<char>)> {
    let ctrl_alt = mods.contains(Modifiers::CTRL | Modifiers::ALT);
    if ctrl_alt && altgr_composed(logical_key, base_layout, &layout_shift_state) {
        let key = aterm_winit_keymap::map_logical_key(logical_key)?;
        return Some((key, mods & !(Modifiers::CTRL | Modifiers::ALT), base_layout));
    }
    match aterm_winit_keymap::map_logical_key(logical_key) {
        Some(key) => Some((key, mods, base_layout)),
        None if ctrl_alt => Some((
            aterm_winit_keymap::map_logical_key(base_key)?,
            mods,
            base_layout,
        )),
        None => None,
    }
}

/// Whether `logical_key` is a character the current layout composes with AltGr,
/// proving the live Ctrl+Alt bits are AltGr's spelling rather than a chord: one
/// non-control codepoint, not the key's own base identity, that `VkKeyScanExW` says
/// needs Ctrl+Alt. A control codepoint is what a real chord produces, so it never
/// qualifies.
#[cfg(any(windows, test))]
fn altgr_composed(
    logical_key: &WinitKey,
    base_layout: Option<char>,
    layout_shift_state: impl Fn(char) -> Option<u8>,
) -> bool {
    let WinitKey::Character(s) = logical_key else {
        return false;
    };
    let mut chars = s.chars();
    let Some(c) = chars.next() else { return false };
    if chars.next().is_some() || c.is_control() || Some(c) == base_layout {
        return false;
    }
    layout_shift_state(c).is_some_and(|state| state & SHIFT_STATE_CTRL_ALT == SHIFT_STATE_CTRL_ALT)
}

/// `VkKeyScanExW`'s shift state for `c` on the calling thread's layout
/// (`GetKeyboardLayout(0)`), or `None` when the layout cannot type it (the -1
/// return), as for every non-BMP character (the API takes one UTF-16 unit). One
/// call per Ctrl+Alt press, so no cache.
#[cfg(windows)]
fn layout_shift_state(c: char) -> Option<u8> {
    #[cfg(test)]
    if let Some(layout) = recorded_layout_shift_state::get() {
        return layout(c);
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetKeyboardLayout(idThread: u32) -> *mut core::ffi::c_void;
        fn VkKeyScanExW(ch: u16, dwhkl: *mut core::ffi::c_void) -> i16;
    }
    let unit = u16::try_from(u32::from(c)).ok()?;
    // SAFETY: both are pure keyboard-layout queries with no preconditions —
    // `GetKeyboardLayout(0)` names the calling thread and returns a borrowed
    // HKL (nothing to free), which `VkKeyScanExW` only reads. Called on the UI
    // thread, whose layout is the one that composed the press.
    let scan = unsafe { VkKeyScanExW(unit, GetKeyboardLayout(0)) };
    if scan == -1 {
        return None;
    }
    u8::try_from((scan >> 8) & 0xff).ok()
}

/// Test-only: the layout [`layout_shift_state`] answers for on this thread, so a recorded
/// German or French key sequence (`keymap_win_replay_tests`) is decided against that layout's
/// `VkKeyScanExW` table instead of the test machine's.
#[cfg(all(test, windows))]
pub(crate) mod recorded_layout_shift_state {
    use std::cell::Cell;

    /// A layout's `VkKeyScanExW` shift-state answer for a character.
    pub(crate) type ShiftState = fn(char) -> Option<u8>;

    thread_local! {
        static LAYOUT: Cell<Option<ShiftState>> = const { Cell::new(None) };
    }

    /// Answer `VkKeyScanExW`'s shift state from `layout` on this thread (`None`: from Windows).
    pub(crate) fn set(layout: Option<ShiftState>) {
        LAYOUT.with(|cell| cell.set(layout));
    }

    pub(super) fn get() -> Option<ShiftState> {
        LAYOUT.with(Cell::get)
    }
}

/// Recorded Windows key messages on German, French, US and US-International layouts and
/// through Japanese and Korean input methods, replayed through winit's real key path
/// (`aterm_window::platform::windows::recorded`) and then through `App::on_key` and the IME arms, down
/// to the bytes the program reads.
#[cfg(all(test, windows))]
#[path = "keymap_win_replay_tests.rs"]
mod win_replay_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_types::keyboard::KeyEventType;
    use aterm_window::keyboard::{KeyCode, NamedKey as WinitNamed, PhysicalKey, SmolStr};

    fn ch(c: &str) -> WinitKey {
        WinitKey::Character(SmolStr::new(c))
    }

    /// The bytes the shipping encode path writes for one standard-location press:
    /// [`build_key_input`] into the engine's encoder. `None` when nothing is sent.
    fn encode_press(
        logical: WinitKey,
        code: KeyCode,
        mods: Modifiers,
        mode: KeyboardMode,
    ) -> Option<Vec<u8>> {
        let ev = press_at(code, logical, aterm_window::keyboard::KeyLocation::Standard);
        let (key, mods, base, text) = build_key_input(&ev, mods, false)?;
        let bytes =
            keyboard::encode_key_with_text(&key, mods, mode, KeyEventType::Press, base, text);
        (!bytes.is_empty()).then_some(bytes)
    }

    /// The bytes the shipping encode path writes for one press, each row a
    /// regression the rewrite fixed or kept:
    ///
    /// * Alt+a is ESC a, NOT the macOS Option-composed "å" — THE K-1
    ///   regression: the old GUI wrote `ev.text`, the composed glyph;
    /// * Ctrl+Space is NUL and Ctrl+\\ is FS — the old `& 0x1f` branch only
    ///   fired for ASCII alphabetics, so both fell through to a raw byte;
    /// * a plain printable under kitty DISAMBIGUATE is its text byte —
    ///   over-escaping `a` to `ESC [ 97 u` corrupted ordinary typing;
    /// * Ctrl-C / Ctrl-D stay 0x03 / 0x04, plain ASCII writes its literal byte,
    ///   and a bare Shift press writes nothing in legacy mode.
    #[test]
    fn a_press_encodes_to_its_terminal_bytes() {
        let none = KeyboardMode::empty();
        #[expect(clippy::type_complexity, reason = "a test's case table")]
        let cases: [(
            &str,
            WinitKey,
            KeyCode,
            Modifiers,
            KeyboardMode,
            Option<&[u8]>,
        ); 8] = [
            (
                "alt+a",
                ch("a"),
                KeyCode::KeyA,
                Modifiers::ALT,
                none,
                Some(b"\x1ba"),
            ),
            (
                "ctrl+space",
                WinitKey::Named(WinitNamed::Space),
                KeyCode::Space,
                Modifiers::CTRL,
                none,
                Some(b"\x00"),
            ),
            (
                "ctrl+\\",
                ch("\\"),
                KeyCode::Backslash,
                Modifiers::CTRL,
                none,
                Some(b"\x1c"),
            ),
            (
                "a under disambiguate",
                ch("a"),
                KeyCode::KeyA,
                Modifiers::empty(),
                KeyboardMode::DISAMBIGUATE_ESC_CODES,
                Some(b"a"),
            ),
            (
                "ctrl+c",
                ch("c"),
                KeyCode::KeyC,
                Modifiers::CTRL,
                none,
                Some(b"\x03"),
            ),
            (
                "ctrl+d",
                ch("d"),
                KeyCode::KeyD,
                Modifiers::CTRL,
                none,
                Some(b"\x04"),
            ),
            (
                "a",
                ch("a"),
                KeyCode::KeyA,
                Modifiers::empty(),
                none,
                Some(b"a"),
            ),
            (
                "bare shift",
                WinitKey::Named(WinitNamed::Shift),
                KeyCode::ShiftLeft,
                Modifiers::SHIFT,
                none,
                None,
            ),
        ];
        for (case, logical, code, mods, mode, want) in cases {
            assert_eq!(
                encode_press(logical, code, mods, mode).as_deref(),
                want,
                "{case}"
            );
        }
    }

    /// Shift+<symbol> encodes the shifted glyph: AppKit hands over the unshifted base
    /// key ('2') plus SHIFT and the engine derives '@'. On a US layout that agrees
    /// with the text, so no layout text rides and the press equals a controller's
    /// `key 2 mods=shift`.
    #[test]
    fn shift_symbol_encodes_shifted_glyph() {
        let ev = mac_press(KeyCode::Digit2, "@", "2", "@");
        let (key, mods, base, text) = build_key_input(&ev, Modifiers::SHIFT, false).expect("maps");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            assert_eq!(key, keyboard::Key::Character('2'));
            assert_eq!(text, None, "US Shift+2 is the engine's own '@'");
        }
        let bytes = keyboard::encode_key_with_text(
            &key,
            mods,
            KeyboardMode::empty(),
            KeyEventType::Press,
            base,
            text,
        );
        assert_eq!(bytes, b"@", "Shift+2 must emit '@', not '2'");
    }

    /// The Windows lock-state query (a real `GetKeyState` round-trip) returns only
    /// the two lock bits.
    #[cfg(windows)]
    #[test]
    fn lock_modifiers_reports_only_lock_bits() {
        let locks = lock_modifiers();
        assert!(
            (Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK).contains(locks),
            "lock_modifiers must set nothing beyond CAPS_LOCK/NUM_LOCK, got {locks:?}"
        );
    }

    /// `VkKeyScanExW`'s answers on the shipped de-DE (`00000407`) and US
    /// (`00000409`) layout DLLs (1 = Shift, 2 = Ctrl, 4 = Alt, `None` = untypeable).
    /// No platform `cfg`, so the AltGr tests below run on every host.
    fn de_de_shift_state(c: char) -> Option<u8> {
        match c {
            // AltGr characters: 0x0637 '{', 0x0638 '[', 0x0651 '@', 0x0645 '€'.
            '{' | '[' | '@' | '€' | '}' | ']' | '\\' | '|' | '~' => Some(0b0000_0110),
            'a'..='z' | '0'..='9' => Some(0),
            'A'..='Z' => Some(1),
            _ => None,
        }
    }

    fn us_qwerty_shift_state(c: char) -> Option<u8> {
        match c {
            // 0x01DB '{', 0x00DB '[', 0x0132 '@' — Shift or nothing, never 6.
            '{' | '@' | 'A'..='Z' | '}' | '|' | '~' => Some(1),
            '[' | 'a'..='z' | '0'..='9' | ']' | '\\' => Some(0),
            // 0xFFFF: no US key types '€'.
            _ => None,
        }
    }

    /// AltGr spelled LCtrl+LAlt (which Windows accepts and compact/RDP keyboards
    /// force) types the composed character, not a control byte: winit clears
    /// Ctrl/Alt only for the right Alt.
    #[test]
    fn de_de_altgr_as_left_ctrl_alt_types_the_character() {
        for (composed, base, want) in [
            ("{", '7', &b"{"[..]),
            ("[", '8', &b"["[..]),
            ("@", 'q', &b"@"[..]),
        ] {
            let (key, mods, base_layout) = windows_key_input(
                &ch(composed),
                &ch(&base.to_string()),
                Some(base),
                Modifiers::CTRL | Modifiers::ALT,
                de_de_shift_state,
            )
            .expect("an AltGr-composed character maps");
            assert_eq!(
                key,
                keyboard::Key::Character(composed.chars().next().unwrap())
            );
            assert_eq!(
                mods,
                Modifiers::empty(),
                "AltGr's Ctrl+Alt spelling must not survive into the encoding of {composed}"
            );
            assert_eq!(base_layout, Some(base));
            let bytes = keyboard::encode_key_with_layout(
                &key,
                mods,
                KeyboardMode::empty(),
                KeyEventType::Press,
                base_layout,
            );
            assert_eq!(bytes, want, "de-DE AltGr must type {composed}");
        }
    }

    /// The right-Alt spelling is winit's own path: it already cleared Ctrl/Alt
    /// from `ModifiersState`, so the strip leaves the character alone.
    #[test]
    fn de_de_right_alt_altgr_is_unchanged() {
        let (key, mods, _) = windows_key_input(
            &ch("{"),
            &ch("7"),
            Some('7'),
            Modifiers::empty(),
            de_de_shift_state,
        )
        .expect("the RAlt-composed character maps");
        assert_eq!(key, keyboard::Key::Character('{'));
        assert_eq!(mods, Modifiers::empty());
    }

    /// A genuine Ctrl+Alt+letter chord keeps its modifiers and encodes ESC ^A, both
    /// when the layout composes nothing (`ToUnicodeEx` returns 0 for Ctrl+Alt+A on US
    /// and de-DE, so winit reports `Unidentified`) and when it hands back a plain
    /// letter, which needs no modifiers and so never looks like AltGr.
    #[test]
    fn genuine_ctrl_alt_letter_still_encodes_a_control_sequence() {
        let unidentified = WinitKey::Unidentified(aterm_window::keyboard::NativeKey::Unidentified);
        for logical in [&unidentified, &ch("a")] {
            let (key, mods, base_layout) = windows_key_input(
                logical,
                &ch("a"),
                Some('a'),
                Modifiers::CTRL | Modifiers::ALT,
                de_de_shift_state,
            )
            .expect("a genuine Ctrl+Alt chord still resolves a key");
            assert_eq!(key, keyboard::Key::Character('a'));
            assert_eq!(
                mods,
                Modifiers::CTRL | Modifiers::ALT,
                "a real chord must keep its modifiers"
            );
            let bytes = keyboard::encode_key_with_layout(
                &key,
                mods,
                KeyboardMode::empty(),
                KeyEventType::Press,
                base_layout,
            );
            assert_eq!(bytes, b"\x1b\x01", "Ctrl+Alt+A must stay ESC ^A");
        }
    }

    /// US-QWERTY is structurally out of reach of the strip: no character on it
    /// needs Ctrl+Alt, so `VkKeyScanExW` never answers 6 and nothing is ever
    /// stripped — including the '{' that US types with plain Shift.
    #[test]
    fn us_qwerty_is_unaffected() {
        // The '{' a de-DE AltGr press composes, on a layout that reaches it with Shift.
        let (_, mods, _) = windows_key_input(
            &ch("{"),
            &ch("["),
            Some('['),
            Modifiers::CTRL | Modifiers::ALT,
            us_qwerty_shift_state,
        )
        .expect("the chord resolves");
        assert_eq!(
            mods,
            Modifiers::CTRL | Modifiers::ALT,
            "US-QWERTY has no AltGr; nothing may be stripped"
        );
        // The ordinary Shift+[ is untouched.
        let (key, mods, base_layout) = windows_key_input(
            &ch("{"),
            &ch("["),
            Some('['),
            Modifiers::SHIFT,
            us_qwerty_shift_state,
        )
        .expect("Shift+[ resolves");
        assert_eq!(key, keyboard::Key::Character('{'));
        assert_eq!(mods, Modifiers::SHIFT);
        assert_eq!(
            keyboard::encode_key_with_layout(
                &key,
                mods,
                KeyboardMode::empty(),
                KeyEventType::Press,
                base_layout,
            ),
            b"{"
        );
    }

    /// The bytes a Windows press writes, from the pure halves of the Windows
    /// [`build_key_input`]: `logical` is winit's composed key, `text` its text,
    /// `locks` the lock bits `on_key` adds after the build. Runs on every host.
    fn windows_bytes(
        logical: &str,
        text: &str,
        base: char,
        mods: Modifiers,
        locks: Modifiers,
        mode: KeyboardMode,
    ) -> Vec<u8> {
        let logical = ch(logical);
        let (key, mods, base_layout) =
            windows_key_input(&logical, &ch(&base.to_string()), Some(base), mods, |_| None)
                .expect("the press maps");
        let (mods, text) = windows_composed_text(&logical, &key, mods, Some(text));
        keyboard::encode_key_with_text(
            &key,
            mods | locks,
            mode,
            KeyEventType::Press,
            base_layout,
            text,
        )
    }

    /// The audit's `layout-shift-reapplied`: Windows hands the encoder the
    /// glyph the layout already shifted, and the US Shift table shifted it
    /// again. de-DE Shift+7 typed `?` for `/` and Shift+0 typed `+` for `=`;
    /// AZERTY Shift+& typed `!` for `1`, so no digit could be typed.
    #[test]
    fn windows_non_us_shift_types_the_layouts_character() {
        let shift = Modifiers::SHIFT;
        let none = Modifiers::empty();
        let disambiguate = KeyboardMode::DISAMBIGUATE_ESC_CODES;
        for mode in [
            KeyboardMode::empty(),
            KeyboardMode::WIN32_INPUT,
            disambiguate,
        ] {
            for (logical, base, want) in [
                ("/", '7', "/"),
                ("=", '0', "="),
                (";", ',', ";"),
                ("'", '\\', "'"),
                ("`", '=', "`"),
                ("1", '1', "1"),
                ("2", '2', "2"),
                ("Ф", 'a', "Ф"),
            ] {
                assert_eq!(
                    windows_bytes(logical, logical, base, shift, none, mode),
                    want.as_bytes(),
                    "Shift+{base} composed {logical:?} under {mode:?}"
                );
            }
        }
        // US: the derivation already agrees, so no layout text rides along and
        // the press is the event a controller's `key shift+/` builds.
        for (logical, base) in [("?", '/'), ("A", 'a'), ("@", '2'), ("{", '[')] {
            let key = keyboard::Key::Character(logical.chars().next().unwrap());
            assert_eq!(
                windows_composed_text(&ch(logical), &key, shift, Some(logical)),
                (shift, None),
                "US Shift+{base}"
            );
        }
    }

    /// Caps Lock + Shift + a letter types lowercase on Windows (the layout
    /// lookup applies both), and the encoder, which adds Caps Lock's bit after
    /// the build, capitalised it again (the audit's `caps-shift-inverted`).
    #[test]
    fn windows_caps_lock_with_shift_types_lowercase() {
        let caps = Modifiers::CAPS_LOCK;
        let mode = KeyboardMode::WIN32_INPUT;
        assert_eq!(
            windows_bytes("a", "a", 'a', Modifiers::SHIFT, caps, mode),
            b"a"
        );
        assert_eq!(
            windows_bytes("ф", "ф", 'a', Modifiers::SHIFT, caps, mode),
            "ф".as_bytes(),
            "a letter the US table leaves alone is still Caps Lock's to invert"
        );
        // Caps Lock alone still types the capital it always did.
        assert_eq!(
            windows_bytes("A", "A", 'a', Modifiers::empty(), caps, mode),
            b"A"
        );
    }

    /// A chord keeps the composed glyph: the SHIFT that composed it is dropped
    /// when the US table would shift the glyph again (de-DE Alt+Shift+7 is
    /// `ESC /`, not `ESC ?`). Letters, US glyphs and the Ctrl+Alt rescue's
    /// UNSHIFTED base key keep their Shift.
    #[test]
    fn windows_chords_keep_the_composed_glyph() {
        let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
        let none = KeyboardMode::empty();
        assert_eq!(
            windows_bytes("/", "/", '7', alt_shift, Modifiers::empty(), none),
            b"\x1b/"
        );
        assert_eq!(
            windows_bytes("1", "1", '1', alt_shift, Modifiers::empty(), none),
            b"\x1b1",
            "AZERTY Alt+Shift+&"
        );
        assert_eq!(
            windows_bytes("?", "?", '/', alt_shift, Modifiers::empty(), none),
            b"\x1b?",
            "US Alt+Shift+/"
        );
        let o = keyboard::Key::Character('O');
        assert_eq!(
            windows_composed_text(&ch("O"), &o, alt_shift, Some("O")),
            (alt_shift, None),
            "Alt+Shift+O keeps its Shift"
        );
        // The Ctrl+Alt rescue: the layout composed nothing, the key is the
        // unshifted base, and Shift is the chord's own.
        let unidentified = WinitKey::Unidentified(aterm_window::keyboard::NativeKey::Unidentified);
        let all = Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT;
        let (key, mods, _) = windows_key_input(
            &unidentified,
            &ch("7"),
            Some('7'),
            all,
            us_qwerty_shift_state,
        )
        .expect("the rescue maps");
        assert_eq!(key, keyboard::Key::Character('7'));
        assert_eq!(
            windows_composed_text(&unidentified, &key, mods, None),
            (all, None)
        );
    }

    /// The audit's `dead-key-noncomposing-lost`: US-International `'` then `s`
    /// posts both characters, and winit reports the plain `s` as the key, so
    /// `it's` typed `its`. The press builds no key and its text goes out as
    /// typed; a composed press and every chord keep the key path.
    #[test]
    fn windows_dead_key_that_did_not_compose_types_both_characters() {
        let none = Modifiers::empty();
        assert!(windows_text_commit(&ch("s"), Some("'s"), none));
        assert!(windows_text_commit(&ch("S"), Some("´S"), Modifiers::SHIFT));
        assert!(windows_text_commit(&ch("´"), Some("´´"), none));
        assert!(!windows_text_commit(&ch("é"), Some("é"), none));
        assert!(!windows_text_commit(&ch("s"), Some("s"), none));
        assert!(!windows_text_commit(&ch("s"), Some("'s"), Modifiers::CTRL));
        assert!(!windows_text_commit(&ch("s"), Some("'s"), Modifiers::ALT));
        assert!(!windows_text_commit(
            &WinitKey::Named(WinitNamed::Enter),
            Some("´\r"),
            none
        ));
    }

    /// A dead key then Space types the accent (German `^` then Space is `^`,
    /// US-International `'` then Space `'`): Windows posts it for the Space
    /// press, which winit still names `Space`, and the key path typed a space.
    /// The press goes out as its text; a plain Space, and Space in a chord, keep
    /// the key path. Held, it repeats spaces, as Windows' own repeat does.
    #[test]
    fn windows_dead_key_then_space_types_the_accent() {
        let space = WinitKey::Named(WinitNamed::Space);
        let none = Modifiers::empty();
        assert!(windows_text_commit(&space, Some("^"), none));
        assert!(windows_text_commit(&space, Some("'"), Modifiers::SHIFT));
        assert!(
            !windows_text_commit(&space, Some(" "), none),
            "a plain Space"
        );
        assert!(!windows_text_commit(&space, None, none));
        assert!(!windows_text_commit(&space, Some("^"), Modifiers::CTRL));
        assert!(!windows_text_commit(&space, Some("^"), Modifiers::ALT));
        assert_eq!(
            windows_text_repeat(&space, Some("^"), none).as_deref(),
            Some(" ")
        );
        assert_eq!(windows_text_repeat(&space, Some(" "), none), None);
    }

    /// A held key after a dead key that did not compose repeats the key's own
    /// character, as Windows' repeat posts it (`'sss`, not `'s's's`). Negative
    /// control: a press that took the key path, a chord and a plain one-character
    /// text press replay themselves.
    #[test]
    fn windows_dead_key_that_did_not_compose_repeats_the_key_alone() {
        let none = Modifiers::empty();
        assert_eq!(
            windows_text_repeat(&ch("s"), Some("'s"), none).as_deref(),
            Some("s")
        );
        assert_eq!(
            windows_text_repeat(&ch("S"), Some("´S"), Modifiers::SHIFT).as_deref(),
            Some("S")
        );
        assert_eq!(windows_text_repeat(&ch("é"), Some("é"), none), None);
        assert_eq!(windows_text_repeat(&ch("s"), Some("s"), none), None);
        assert_eq!(
            windows_text_repeat(&ch("s"), Some("'s"), Modifiers::CTRL),
            None
        );
    }

    /// The audit's `alt-numpad-codes`: Alt + a NumLock-on keypad digit is one
    /// digit of an Alt code, which Windows composes itself; it types nothing.
    /// Windows posts no character for the digit, so winit names it
    /// `Unidentified(VK_NUMPADn)`, and keypad 5 read as Alt+KP_Begin. Every
    /// other keypad press keeps its key.
    #[test]
    fn windows_alt_code_digits_type_nothing() {
        use aterm_window::keyboard::{KeyLocation, NativeKey};
        let pad = KeyLocation::Numpad;
        let alt = Modifiers::ALT;
        let vk = |code: u16| WinitKey::Unidentified(NativeKey::Windows(code));
        assert!(windows_alt_code_digit(&vk(0x60), pad, alt));
        assert!(windows_alt_code_digit(&vk(0x65), pad, alt), "keypad 5");
        assert!(windows_alt_code_digit(
            &vk(0x69),
            pad,
            alt | Modifiers::NUM_LOCK
        ));
        assert!(windows_alt_code_digit(&ch("2"), pad, alt));
        // Not a digit of an Alt code: no Alt, a chord, NumLock off (VK_CLEAR,
        // an arrow), an operator, the main row.
        assert!(!windows_alt_code_digit(&ch("5"), pad, Modifiers::empty()));
        assert!(!windows_alt_code_digit(
            &ch("5"),
            pad,
            alt | Modifiers::CTRL
        ));
        assert!(!windows_alt_code_digit(
            &ch("5"),
            pad,
            alt | Modifiers::SHIFT
        ));
        assert!(!windows_alt_code_digit(&vk(0x0c), pad, alt));
        assert!(!windows_alt_code_digit(
            &WinitKey::Named(WinitNamed::ArrowLeft),
            pad,
            alt
        ));
        assert!(!windows_alt_code_digit(&ch("+"), pad, alt));
        assert!(!windows_alt_code_digit(
            &ch("5"),
            KeyLocation::Standard,
            alt
        ));
    }

    /// The whole Windows builder on real winit events: the same three cases as
    /// the pure halves above, through `build_key_input` itself.
    #[cfg(windows)]
    #[test]
    fn windows_build_key_input_carries_the_layouts_text() {
        use aterm_window::keyboard::KeyLocation;
        let press = |code, logical: &str, text: &str| {
            KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                ch(logical),
                Some(SmolStr::new(text)),
                KeyLocation::Standard,
                aterm_window::event::ElementState::Pressed,
                false,
            )
        };
        let de_7 = press(KeyCode::Digit7, "/", "/");
        let (key, mods, _, text) = build_key_input(&de_7, Modifiers::SHIFT, false).expect("maps");
        assert_eq!(
            (key, mods, text),
            (keyboard::Key::Character('/'), Modifiers::SHIFT, Some('/'))
        );
        let dead = press(KeyCode::KeyS, "s", "'s");
        assert!(build_key_input(&dead, Modifiers::empty(), false).is_none());
        let kp = KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::Numpad2),
            ch("2"),
            None,
            KeyLocation::Numpad,
            aterm_window::event::ElementState::Pressed,
            false,
        );
        assert!(build_key_input(&kp, Modifiers::ALT, false).is_none());
        assert!(build_key_input(&kp, Modifiers::empty(), false).is_some());
    }

    /// The macOS lock-state query is a real IOKit HID round-trip (kernel only, see
    /// `hid_lock_state`) and reports only Caps Lock, or nothing when the HID system
    /// is unreachable (a sandbox).
    #[cfg(target_os = "macos")]
    #[test]
    fn lock_modifiers_reports_only_caps_lock_from_the_hid_system() {
        let locks = lock_modifiers();
        assert!(
            Modifiers::CAPS_LOCK.contains(locks),
            "lock_modifiers must set nothing beyond CAPS_LOCK, got {locks:?}"
        );
        // A second read, through the cached connection, agrees.
        assert_eq!(locks, lock_modifiers());
        if let Some(on) = hid_lock_state::caps_lock() {
            assert_eq!(on, locks.contains(Modifiers::CAPS_LOCK));
        }
    }

    /// `modifiers_from_winit` carries Super/Cmd through.
    #[test]
    fn super_modifier_carried_through() {
        let m = modifiers_from_winit(ModifiersState::SUPER);
        assert!(m.contains(Modifiers::SUPER));
    }

    /// A committed composition is encoded as ordinary text: ASCII to its bytes, CJK
    /// to its UTF-8.
    #[test]
    fn commit_sends_committed_text() {
        // ASCII commit (e.g. an accented dead-key result reduced to ASCII).
        assert_eq!(
            encode_committed_text("hi", KeyboardMode::empty()),
            b"hi".to_vec()
        );
        // CJK commit: UTF-8 of "日本" goes out as typed text.
        assert_eq!(
            encode_committed_text("日本", KeyboardMode::empty()),
            "日本".as_bytes().to_vec()
        );
        // Empty commit is empty.
        assert!(encode_committed_text("", KeyboardMode::empty()).is_empty());
    }

    /// C0, DEL and C1 (Unicode `Cc`) make a commit a paste, except a lone C0 or DEL
    /// character (the key X11 commits while an XIM preedit is open). Printable text,
    /// joiners, the no-break space and the line separator (`Zl`, which no shell reads
    /// as a line end) are typed.
    #[test]
    fn a_commit_is_a_paste_when_a_control_character_is_not_a_lone_key() {
        for paste in [
            "ls\n",
            "a\u{85}",
            "\u{9b}",
            "\u{85}",
            "\r\n",
            "\n\n",
            "\x1b\x1b",
            "a\t",
            "\x1b[201~",
        ] {
            assert!(
                committed_text_is_paste(paste),
                "{paste:?} is a paste: a C1 character, or a control beside more text"
            );
        }
        for key in ["\n", "\r", "\t", "\x08", "\x1b", "\x7f", "\x03", "\0"] {
            assert!(
                !committed_text_is_paste(key),
                "{key:?} alone is one key's worth and stays typed"
            );
        }
        for typed in ["", "日本", "é", "🙂", " ", "\u{200d}", "\u{2028}", "\u{a0}"] {
            assert!(!committed_text_is_paste(typed), "{typed:?} is typed text");
        }
    }

    /// Under kitty report-all + associated text (+ event types, the mode a
    /// full-protocol app negotiates) an IME commit is one key-0 event with every
    /// codepoint colon-joined in the text field.
    #[test]
    fn commit_under_report_all_with_text_is_one_key0_event() {
        let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC
            | KeyboardMode::REPORT_ASSOCIATED_TEXT
            | KeyboardMode::REPORT_EVENT_TYPES;
        // '日' = U+65E5 = 26085, '本' = U+672C = 26412.
        assert_eq!(
            encode_committed_text("日本", mode),
            b"\x1b[0;1;26085:26412u".to_vec(),
            "one key-0 event for the whole commit, codepoints colon-joined"
        );
        // A single-codepoint commit is the same form without the joiner.
        assert_eq!(
            encode_committed_text("日", mode),
            b"\x1b[0;1;26085u".to_vec()
        );
    }

    /// Key-0 commits drop control-code codepoints; an all-control commit emits
    /// nothing, never a payload-less `ESC[0;1;u`.
    #[test]
    fn commit_key0_filters_control_codepoints() {
        let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
        assert_eq!(
            encode_committed_text("\u{8}日", mode),
            b"\x1b[0;1;26085u".to_vec(),
            "control codepoints are dropped from the text field"
        );
        assert!(
            encode_committed_text("\u{8}\u{1b}", mode).is_empty(),
            "an all-control commit emits nothing"
        );
    }

    /// REPORT_ALL_KEYS_AS_ESC without REPORT_ASSOCIATED_TEXT has no legal field in
    /// which to carry the IME text. Emit Kitty's one keyless event rather than
    /// fabricating a physical key identity for every Unicode codepoint.
    #[test]
    fn commit_under_report_all_without_text_is_one_keyless_event() {
        let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC;
        assert_eq!(
            encode_committed_text("日本", mode),
            b"\x1b[0u".to_vec(),
            "one IME commit is one keyless event"
        );
        assert!(
            encode_committed_text("\u{8}\u{1b}", mode).is_empty(),
            "an all-control commit remains silent"
        );
    }

    /// A real winit press for the seam. `location` is what says keypad, so
    /// the event carries it exactly as the desktop backends deliver it; a
    /// `Character` logical key carries its text, as a press does.
    fn press_at(
        code: KeyCode,
        logical: WinitKey,
        location: aterm_window::keyboard::KeyLocation,
    ) -> KeyEvent {
        let text = match &logical {
            WinitKey::Character(s) => Some(s.clone()),
            _ => None,
        };
        KeyEvent::synthetic_for_test(
            PhysicalKey::Code(code),
            logical,
            text,
            location,
            aterm_window::event::ElementState::Pressed,
            false,
        )
    }

    /// The bytes the seam writes for the triple `build_key_input` builds.
    fn seam_bytes(ev: &KeyEvent, mode: KeyboardMode) -> Vec<u8> {
        let (key, mods, base, text) =
            build_key_input(ev, Modifiers::empty(), false).expect("the key maps");
        keyboard::encode_key_with_text(&key, mods, mode, KeyEventType::Press, base, text)
    }

    /// A physical KP_5 resolves to `Numpad5`: `5` in legacy mode, DECKPAM's `SS3 u`
    /// and kitty's `CSI 57404 u` where those modes ask.
    #[test]
    fn numpad_digit_reaches_the_numpad_encoder() {
        use aterm_types::keyboard::NamedKey;
        use aterm_window::keyboard::KeyLocation;
        let ev = press_at(KeyCode::Numpad5, ch("5"), KeyLocation::Numpad);
        let (key, _, _, _) = build_key_input(&ev, Modifiers::empty(), false).expect("KP_5 maps");
        assert_eq!(key, keyboard::Key::Named(NamedKey::Numpad5));
        assert_eq!(seam_bytes(&ev, KeyboardMode::empty()), b"5");
        assert_eq!(seam_bytes(&ev, KeyboardMode::APP_KEYPAD), b"\x1bOu");
        assert_eq!(
            seam_bytes(&ev, KeyboardMode::DISAMBIGUATE_ESC_CODES),
            b"\x1b[57404u"
        );
    }

    /// NumLock off with Shift held, xkb's keypad type yields the digit
    /// (`Character("5")` at `Numpad`, SHIFT set), and it types `5`: on the keypad Shift
    /// picks a level and, as in xterm, only cancels DECKPAM. A kitty reporting flag
    /// must not run the main row's shift table over it (`%`).
    #[test]
    fn shifted_keypad_digit_types_its_digit_never_the_main_rows_glyph() {
        use aterm_types::keyboard::NamedKey;
        use aterm_window::keyboard::KeyLocation;
        let ev = press_at(KeyCode::Numpad5, ch("5"), KeyLocation::Numpad);
        let (key, mods, base, _) =
            build_key_input(&ev, Modifiers::SHIFT, false).expect("KP_5 maps");
        assert_eq!(key, keyboard::Key::Named(NamedKey::Numpad5));
        assert!(
            mods.contains(Modifiers::SHIFT),
            "the seam keeps the SHIFT bit"
        );
        let bytes =
            |mode| keyboard::encode_key_with_layout(&key, mods, mode, KeyEventType::Press, base);
        assert_eq!(
            bytes(KeyboardMode::REPORT_EVENT_TYPES),
            b"5",
            "a kitty reporting flag must not turn KP_5 into `%`"
        );
        assert_eq!(bytes(KeyboardMode::empty()), b"5");
        assert_eq!(
            bytes(KeyboardMode::APP_KEYPAD),
            b"5",
            "SHIFT cancels application keypad mode, as xterm has it"
        );
        assert_eq!(
            bytes(KeyboardMode::DISAMBIGUATE_ESC_CODES),
            b"\x1b[57404;2u",
            "disambiguate still reports the keypad's own key, SHIFT bit included"
        );
    }

    /// KP_Enter is NumpadEnter — CR in legacy mode, `SS3 M` under DECKPAM,
    /// `CSI 57414 u` under kitty disambiguate — so an application can tell it
    /// from Return.
    #[test]
    fn numpad_enter_is_distinguishable_from_enter() {
        use aterm_types::keyboard::NamedKey;
        use aterm_window::keyboard::KeyLocation;
        let ev = press_at(
            KeyCode::NumpadEnter,
            WinitKey::Named(WinitNamed::Enter),
            KeyLocation::Numpad,
        );
        let (key, _, _, _) =
            build_key_input(&ev, Modifiers::empty(), false).expect("KP_Enter maps");
        assert_eq!(key, keyboard::Key::Named(NamedKey::NumpadEnter));
        assert_eq!(seam_bytes(&ev, KeyboardMode::empty()), b"\r");
        assert_eq!(seam_bytes(&ev, KeyboardMode::APP_KEYPAD), b"\x1bOM");
        assert_eq!(
            seam_bytes(&ev, KeyboardMode::DISAMBIGUATE_ESC_CODES),
            b"\x1b[57414u"
        );
    }

    /// NumLock off, keypad 1 is KP_End: the main block's End bytes in legacy mode
    /// (kitty's legacy fold) and its own `CSI 57424 u` under disambiguate.
    #[test]
    fn numlock_off_keypad_navigation_keeps_its_keypad_identity() {
        use aterm_types::keyboard::NamedKey;
        use aterm_window::keyboard::KeyLocation;
        let ev = press_at(
            KeyCode::Numpad1,
            WinitKey::Named(WinitNamed::End),
            KeyLocation::Numpad,
        );
        let (key, _, _, _) = build_key_input(&ev, Modifiers::empty(), false).expect("KP_End maps");
        assert_eq!(key, keyboard::Key::Named(NamedKey::NumpadEnd));
        assert_eq!(seam_bytes(&ev, KeyboardMode::empty()), b"\x1b[F");
        assert_eq!(
            seam_bytes(&ev, KeyboardMode::DISAMBIGUATE_ESC_CODES),
            b"\x1b[57424u"
        );
    }

    /// The negative control: the main block is untouched. `5` above `R` stays
    /// `Character('5')` and types `5` under DECKPAM and disambiguate alike; the
    /// main Return stays `Enter` and CR under DECKPAM.
    #[test]
    fn main_row_digit_is_unchanged() {
        use aterm_types::keyboard::NamedKey;
        use aterm_window::keyboard::KeyLocation;
        let five = press_at(KeyCode::Digit5, ch("5"), KeyLocation::Standard);
        let (key, _, _, _) = build_key_input(&five, Modifiers::empty(), false).expect("5 maps");
        assert_eq!(key, keyboard::Key::Character('5'));
        assert_eq!(seam_bytes(&five, KeyboardMode::APP_KEYPAD), b"5");
        assert_eq!(
            seam_bytes(&five, KeyboardMode::DISAMBIGUATE_ESC_CODES),
            b"5"
        );
        let enter = press_at(
            KeyCode::Enter,
            WinitKey::Named(WinitNamed::Enter),
            KeyLocation::Standard,
        );
        let (key, _, _, _) =
            build_key_input(&enter, Modifiers::empty(), false).expect("Enter maps");
        assert_eq!(key, keyboard::Key::Named(NamedKey::Enter));
        assert_eq!(seam_bytes(&enter, KeyboardMode::APP_KEYPAD), b"\r");
    }

    /// A winit press whose modifier-supplement base key differs from `logical_key`,
    /// as on every X11/Wayland keypad press.
    fn press_at_with_base(
        code: KeyCode,
        logical: WinitKey,
        base: WinitKey,
        location: aterm_window::keyboard::KeyLocation,
    ) -> KeyEvent {
        let text = match &logical {
            WinitKey::Character(s) => Some(s.clone()),
            _ => None,
        };
        KeyEvent::synthetic_for_test_with_base(
            PhysicalKey::Code(code),
            logical,
            base,
            text,
            location,
            aterm_window::event::ElementState::Pressed,
            false,
        )
    }

    /// On X11/Wayland `key_without_modifiers()` is xkb's level-0 keysym, and the
    /// keypad type holds the NumLock-off symbol there (`<KP1> { [ KP_End, KP_1 ] }`),
    /// so a NumLock-on KP_1 has a level-0 `Named(End)`. `map_numpad_key` reads
    /// `logical_key`, which carries NumLock, so the digit types its digit; and
    /// `base_logical_key` reads the same key, so bindings and encoder agree.
    #[test]
    fn numlock_on_keypad_digit_is_not_the_level_zero_keysym() {
        use aterm_types::keyboard::NamedKey;
        use aterm_window::keyboard::KeyLocation;
        let ev = press_at_with_base(
            KeyCode::Numpad1,
            ch("1"),
            WinitKey::Named(WinitNamed::End),
            KeyLocation::Numpad,
        );
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        {
            use aterm_window::platform::modifier_supplement::KeyEventExtModifierSupplement;
            assert_eq!(
                ev.key_without_modifiers(),
                WinitKey::Named(WinitNamed::End),
                "the harness really does hand the builder a divergent base key"
            );
        }
        let (key, _, _, _) = build_key_input(&ev, Modifiers::empty(), false).expect("KP_1 maps");
        assert_eq!(key, keyboard::Key::Named(NamedKey::Numpad1));
        assert_eq!(
            seam_bytes(&ev, KeyboardMode::empty()),
            b"1",
            "a NumLock-ON keypad 1 types 1, not End's ESC [ F"
        );
        assert_eq!(seam_bytes(&ev, KeyboardMode::APP_KEYPAD), b"\x1bOq");
        // Level 0 of the decimal key is KP_Delete.
        let dot = press_at_with_base(
            KeyCode::NumpadDecimal,
            ch("."),
            WinitKey::Named(WinitNamed::Delete),
            KeyLocation::Numpad,
        );
        assert_eq!(seam_bytes(&dot, KeyboardMode::empty()), b".");
        // The binding lookup reads the same key the encoder does.
        assert_eq!(crate::app_input::base_logical_key(&ev), ch("1"));
        let kp3 = press_at_with_base(
            KeyCode::Numpad3,
            ch("3"),
            WinitKey::Named(WinitNamed::PageDown),
            KeyLocation::Numpad,
        );
        assert_eq!(crate::app_input::base_logical_key(&kp3), ch("3"));
        // NumLock off, the level-0 symbol is the key: End, for encoder and bindings.
        let off = press_at_with_base(
            KeyCode::Numpad1,
            WinitKey::Named(WinitNamed::End),
            WinitKey::Named(WinitNamed::End),
            KeyLocation::Numpad,
        );
        assert_eq!(seam_bytes(&off, KeyboardMode::empty()), b"\x1b[F");
        assert_eq!(
            crate::app_input::base_logical_key(&off),
            WinitKey::Named(WinitNamed::End)
        );
        // The main block's base key stays the layout base.
        let shifted = press_at_with_base(KeyCode::Digit1, ch("!"), ch("1"), KeyLocation::Standard);
        assert_eq!(crate::app_input::base_logical_key(&shifted), ch("1"));
    }

    /// The NumLock-off centre key, which no platform names (xkb's `KP_Begin` is
    /// `Unidentified`, Windows says `Clear`), is settled by its physical code and
    /// sends xterm's `CSI E` (`SS3 E` under DECKPAM), never a digit.
    #[test]
    fn numlock_off_keypad_centre_is_the_letter_form_never_a_digit() {
        use aterm_types::keyboard::NamedKey;
        use aterm_window::keyboard::{KeyLocation, NativeKey};
        // xkb keysym KP_Begin (0xFF9D), which winit maps to no `Key` variant.
        let ev = press_at(
            KeyCode::Numpad5,
            WinitKey::Unidentified(NativeKey::Xkb(0xff9d)),
            KeyLocation::Numpad,
        );
        let (key, _, _, _) =
            build_key_input(&ev, Modifiers::empty(), false).expect("KP_Begin maps");
        assert_eq!(key, keyboard::Key::Named(NamedKey::NumpadBegin));
        assert_eq!(
            seam_bytes(&ev, KeyboardMode::empty()),
            b"\x1b[E",
            "the centre key must never type a digit into the shell"
        );
        assert_eq!(seam_bytes(&ev, KeyboardMode::APP_KEYPAD), b"\x1bOE");
        assert_eq!(
            seam_bytes(&ev, KeyboardMode::DISAMBIGUATE_ESC_CODES),
            b"\x1b[E"
        );
    }

    /// A press as AppKit builds it: `base` is `key_without_modifiers()`
    /// (UCKeyTranslate with no modifiers), `logical` and `text` are
    /// `-characters` under the live modifiers and lock state.
    fn mac_press(code: KeyCode, logical: &str, base: &str, text: &str) -> KeyEvent {
        KeyEvent::synthetic_for_test_with_base(
            PhysicalKey::Code(code),
            ch(logical),
            ch(base),
            (!text.is_empty()).then(|| SmolStr::new(text)),
            aterm_window::keyboard::KeyLocation::Standard,
            aterm_window::event::ElementState::Pressed,
            false,
        )
    }

    /// The bytes the seam writes for a macOS-shaped press under `mods`.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn mac_bytes(ev: &KeyEvent, mods: Modifiers, mode: KeyboardMode) -> Vec<u8> {
        let (key, mods, base, text) = build_key_input(ev, mods, false).expect("the key maps");
        keyboard::encode_key_with_text(&key, mods, mode, KeyEventType::Press, base, text)
    }

    /// Bug hunt 8: the window carries the layout's character only when the US
    /// derivation from the unshifted key would type something else — so a US
    /// press stays the event a controller builds — for a text press and, since
    /// , a Ctrl or Alt chord.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn layout_text_rides_a_press_only_when_the_layout_differs() {
        let text = |ev: &KeyEvent, mods| build_key_input(ev, mods, false).expect("maps").3;
        // US: the engine already agrees.
        assert_eq!(
            text(&mac_press(KeyCode::KeyA, "a", "a", "a"), Modifiers::empty()),
            None
        );
        assert_eq!(
            text(&mac_press(KeyCode::KeyA, "A", "a", "A"), Modifiers::SHIFT),
            None
        );
        assert_eq!(
            text(&mac_press(KeyCode::Digit2, "@", "2", "@"), Modifiers::SHIFT),
            None
        );
        // Caps Lock (not a winit modifier), German, AZERTY, Russian.
        assert_eq!(
            text(&mac_press(KeyCode::KeyA, "A", "a", "A"), Modifiers::empty()),
            Some('A')
        );
        assert_eq!(
            text(
                &mac_press(KeyCode::Digit2, "\"", "2", "\""),
                Modifiers::SHIFT
            ),
            Some('"')
        );
        assert_eq!(
            text(&mac_press(KeyCode::Digit1, "1", "&", "1"), Modifiers::SHIFT),
            Some('1')
        );
        assert_eq!(
            text(&mac_press(KeyCode::KeyA, "Ф", "ф", "Ф"), Modifiers::SHIFT),
            Some('Ф')
        );
        // A Ctrl or Alt chord carries the layout's character too (German
        // Ctrl+Shift+2 and Option+Shift+2 read '"' without Ctrl or Option),
        // but not when it agrees with the derivation (Alt+a's 'a'), nor an
        // Option chord whose text Option composed, nor a Super chord, nor a
        // dead key's empty text.
        let de_2 = mac_press(KeyCode::Digit2, "\"", "2", "\"");
        assert_eq!(text(&de_2, Modifiers::CTRL | Modifiers::SHIFT), Some('"'));
        assert_eq!(text(&de_2, Modifiers::ALT | Modifiers::SHIFT), Some('"'));
        assert_eq!(
            text(&mac_press(KeyCode::KeyA, "a", "a", "a"), Modifiers::ALT),
            None
        );
        let alt_a = mac_press(KeyCode::KeyA, "å", "a", "å");
        let composes = |ev: &KeyEvent, mods| build_key_input(ev, mods, true).expect("maps").3;
        assert_eq!(
            composes(&alt_a, Modifiers::ALT),
            None,
            "Option composed 'å'"
        );
        assert_eq!(
            composes(&de_2, Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT),
            Some('"'),
            "a Ctrl chord's text is read without Option"
        );
        assert_eq!(text(&de_2, Modifiers::SUPER | Modifiers::SHIFT), None);
        assert_eq!(
            text(&mac_press(KeyCode::KeyE, "e", "e", ""), Modifiers::empty()),
            None
        );
        // A chord's letter in the lock's case carries nothing (Caps Lock +
        // Alt+b, Caps Lock + Ctrl+c, Russian Caps Lock + Alt+и, and xkb's
        // Shift+Lock level, which types the small letter), while a text press
        // keeps the lock's capital (above).
        let caps_b = mac_press(KeyCode::KeyB, "B", "b", "B");
        assert_eq!(text(&caps_b, Modifiers::ALT), None);
        assert_eq!(
            text(&mac_press(KeyCode::KeyC, "C", "c", "C"), Modifiers::CTRL),
            None
        );
        assert_eq!(
            text(&mac_press(KeyCode::KeyB, "И", "и", "И"), Modifiers::ALT),
            None
        );
        assert_eq!(
            text(
                &mac_press(KeyCode::KeyB, "b", "b", "b"),
                Modifiers::CTRL | Modifiers::SHIFT
            ),
            None
        );
        // Every case pair, Turkish Q's two included (Caps Lock + Alt+ı reads
        // 'I', Caps Lock + Alt+i 'İ'), and Shift's capital where the US table
        // derives none (Russian Alt+Shift+и, German Alt+Shift+ü).
        let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
        for (press, mods, want, why) in [
            (("I", "ı"), Modifiers::ALT, None, "tr Caps Lock + Alt+ı"),
            (("İ", "i"), Modifiers::ALT, None, "tr Caps Lock + Alt+i"),
            (("I", "ı"), alt_shift, Some('I'), "tr Alt+Shift+ı"),
            (("İ", "i"), alt_shift, Some('İ'), "tr Alt+Shift+i"),
            (("И", "и"), alt_shift, Some('И'), "ru Alt+Shift+и"),
            (("Ü", "ü"), alt_shift, Some('Ü'), "de Alt+Shift+ü"),
            (
                ("и", "и"),
                alt_shift,
                Some('И'),
                "ru Shift under Caps Lock (xkb)",
            ),
            (("ẞ", "ß"), Modifiers::ALT, None, "de Caps Lock + Alt+ß"),
        ] {
            let (glyph, base) = press;
            let ev = mac_press(KeyCode::KeyI, glyph, base, glyph);
            assert_eq!(text(&ev, mods), want, "{why}");
        }
    }

    /// Caps Lock + A and Shift on a non-US layout type the layout's character, in
    /// legacy mode and every kitty mode that keeps text as text; kitty's report-all
    /// forms keep the layout's unshifted key as the key code.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn caps_lock_and_non_us_shift_type_the_layouts_character() {
        let disambiguate = KeyboardMode::DISAMBIGUATE_ESC_CODES;
        let alternates = disambiguate | KeyboardMode::REPORT_ALTERNATE_KEYS;
        for mode in [KeyboardMode::empty(), disambiguate, alternates] {
            let caps_a = mac_press(KeyCode::KeyA, "A", "a", "A");
            assert_eq!(
                mac_bytes(&caps_a, Modifiers::empty(), mode),
                b"A",
                "{mode:?}"
            );
            let de_2 = mac_press(KeyCode::Digit2, "\"", "2", "\"");
            assert_eq!(mac_bytes(&de_2, Modifiers::SHIFT, mode), b"\"", "{mode:?}");
            let de_7 = mac_press(KeyCode::Digit7, "/", "7", "/");
            assert_eq!(mac_bytes(&de_7, Modifiers::SHIFT, mode), b"/", "{mode:?}");
            let azerty_1 = mac_press(KeyCode::Digit1, "1", "&", "1");
            assert_eq!(
                mac_bytes(&azerty_1, Modifiers::SHIFT, mode),
                b"1",
                "{mode:?}"
            );
            let azerty_2 = mac_press(KeyCode::Digit2, "2", "é", "2");
            assert_eq!(
                mac_bytes(&azerty_2, Modifiers::SHIFT, mode),
                b"2",
                "{mode:?}"
            );
            let ru_f = mac_press(KeyCode::KeyA, "Ф", "ф", "Ф");
            assert_eq!(mac_bytes(&ru_f, Modifiers::SHIFT, mode), "Ф".as_bytes());
        }
        let report_all_text =
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ASSOCIATED_TEXT;
        let de_7 = mac_press(KeyCode::Digit7, "/", "7", "/");
        assert_eq!(
            mac_bytes(&de_7, Modifiers::SHIFT, report_all_text),
            b"\x1b[55;2;47u",
            "kitty >24: key 7, text '/'"
        );
        let report_all_alternates =
            KeyboardMode::REPORT_ALL_KEYS_AS_ESC | KeyboardMode::REPORT_ALTERNATE_KEYS;
        let de_2 = mac_press(KeyCode::Digit2, "\"", "2", "\"");
        assert_eq!(
            mac_bytes(&de_2, Modifiers::SHIFT, report_all_alternates),
            b"\x1b[50:34;2u",
            "kitty >12: the shifted key is the layout's '\"'"
        );
    }

    /// A CHORD UNDER XKB'S CAPS LOCK TAKES SHIFT'S CASE. On X11 and
    /// Wayland xkb cancels Shift against Caps Lock on an alphabetic key, so Turkish Caps
    /// Lock + Alt+Shift+i arrives with the text `i`, and the chord took `i`'s default
    /// capital, ESC `I`, where Alt+Shift+i without the lock is ESC `İ`. The window reads a
    /// chord's letter from the press read again without the lock (winit's
    /// `text_ignoring_lock`, which xkb answers `İ` there: the winit fork's
    /// `a_press_under_caps_lock_is_read_again_without_it`). Controls: the same press
    /// without the lock, and the text press under the lock, which keeps the lock's case.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_xkb_chord_under_caps_lock_takes_shifts_capital() {
        let none = KeyboardMode::empty();
        let alt_shift = Modifiers::ALT | Modifiers::SHIFT;
        let locked = mac_press(KeyCode::Quote, "i", "i", "i")
            .with_text_ignoring_lock_for_test(Some(SmolStr::new("İ")));
        assert_eq!(mac_bytes(&locked, alt_shift, none), "\x1bİ".as_bytes());
        let unlocked = mac_press(KeyCode::Quote, "İ", "i", "İ");
        assert_eq!(mac_bytes(&unlocked, alt_shift, none), "\x1bİ".as_bytes());
        let caps_alt = mac_press(KeyCode::Quote, "İ", "i", "İ")
            .with_text_ignoring_lock_for_test(Some(SmolStr::new("i")));
        assert_eq!(mac_bytes(&caps_alt, Modifiers::ALT, none), b"\x1bi");
        let caps_text = mac_press(KeyCode::Quote, "İ", "i", "İ")
            .with_text_ignoring_lock_for_test(Some(SmolStr::new("i")));
        assert_eq!(
            mac_bytes(&caps_text, Modifiers::empty(), none),
            "İ".as_bytes()
        );
    }

    /// Ctrl on a Cyrillic layout: the base key 'с' has no control byte, so the
    /// physical key's US letter names it; kitty keeps reporting the layout's key.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn ctrl_on_a_cyrillic_layout_sends_the_control_byte() {
        let es = mac_press(KeyCode::KeyC, "с", "с", "с");
        let ve = mac_press(KeyCode::KeyD, "в", "в", "в");
        let none = KeyboardMode::empty();
        assert_eq!(mac_bytes(&es, Modifiers::CTRL, none), [0x03]);
        assert_eq!(mac_bytes(&ve, Modifiers::CTRL, none), [0x04]);
        assert_eq!(
            mac_bytes(&es, Modifiers::CTRL, KeyboardMode::DISAMBIGUATE_ESC_CODES),
            b"\x1b[1089;5u"
        );
    }
}
