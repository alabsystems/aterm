// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Keyboard event → engine key input: the pure decisions behind a key press.

use aterm_types::keyboard::{self, KeyboardMode, Modifiers};
use aterm_window::event::KeyEvent;
use aterm_window::keyboard::{Key as WinitKey, ModifiersState};

/// Translate winit's [`ModifiersState`] into the engine's [`Modifiers`], Super/Cmd included (the encoder uses it for the kitty/xterm modifier value and `modifyOtherKeys`).
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
#[cfg(target_os = "macos")]
#[must_use]
fn lock_modifiers() -> Modifiers {
    match hid_lock_state::caps_lock() {
        Some(true) => Modifiers::CAPS_LOCK,
        Some(false) | None => Modifiers::empty(),
    }
}

/// The lock-key source an `App` holds: the platform read ([`lock_modifiers`]) windowed, [`no_lock_modifiers`] headless.
pub(crate) fn lock_modifier_source(headless: bool) -> fn() -> Modifiers {
    if headless {
        no_lock_modifiers
    } else {
        lock_modifiers
    }
}

/// The lock-state source for headless instances and unit tests: always empty.
#[must_use]
pub(crate) const fn no_lock_modifiers() -> Modifiers {
    Modifiers::empty()
}

/// Caps Lock from the kernel's HID system, the macOS lock-key source that never
/// involves WindowServer.
#[cfg(target_os = "macos")]
mod hid_lock_state {
    use std::ffi::{c_char, c_int, c_void};
    use std::sync::OnceLock;

    type MachPort = u32;
    type IoObject = MachPort;
    type IoService = IoObject;
    type IoConnect = IoObject;
    type KernReturn = c_int;

    const IO_MAIN_PORT_DEFAULT: MachPort = 0;
    const IOHID_PARAM_CONNECT_TYPE: u32 = 1;
    const IOHID_CAPS_LOCK_STATE: c_int = 0x0000_0001;
    const IOHID_SYSTEM_CLASS: &[u8] = b"IOHIDSystem\0";
    const KERN_SUCCESS: KernReturn = 0;

    unsafe extern "C" {
        static mach_task_self_: MachPort;
    }
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
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

    /// `Some(state)` of the Caps Lock LED, `None` when the HID system is unreachable (no user client, or the read failed).
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
/// toggled LED state for `VK_CAPITAL` / `VK_NUMLOCK`).
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

/// Whether this press is a bare modifier or lock key (Shift, Control, Alt/Option, Super/Command, Hyper, Meta, Caps/Num/Scroll Lock), which expresses no typing intent.
#[must_use]
pub(crate) fn press_is_inert(ev: &KeyEvent) -> bool {
    if aterm_winit_keymap::map_logical_key(&ev.logical_key)
        .as_ref()
        .is_some_and(keyboard::is_modifier_or_lock_key)
    {
        return true;
    }
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

/// Whether a direct key send must be suppressed because an IME composition (CJK, dead key) is active.
#[must_use]
pub(crate) const fn suppress_direct_send(preedit: &str) -> bool {
    !preedit.is_empty()
}

/// Whether an input method's committed text (`Ime::Commit`) reaches the PTY as a paste rather than as typed keys.
#[must_use]
pub(crate) fn committed_text_is_paste(text: &str) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(only), None) => only.is_control() && !only.is_ascii(),
        _ => text.chars().any(char::is_control),
    }
}

/// Encode committed composition text (`Ime::Commit`) for the PTY.
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

/// The engine-neutral fields of an [`InputEvent::Key`](crate::input::InputEvent::Key) for a press.
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
        return Some((key, mods, base_layout, None));
    }
    let key = aterm_winit_keymap::map_logical_key(&ev.key_without_modifiers())?;
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

/// Character the active layout types for a press at its Shift/lock level when it differs from [`keyboard::typed_character`].
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
/// never the lock's.
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

/// [`build_key_input`] on Windows, where `KeyEventExtModifierSupplement` exists but must not be used for the ENCODE path.
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
/// code (Alt, 0, 2, 3, 3 types `é`).
#[cfg(any(windows, test))]
fn windows_alt_code_digit(
    logical_key: &WinitKey,
    location: aterm_window::keyboard::KeyLocation,
    mods: Modifiers,
) -> bool {
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

/// What an auto-repeat of a press that went out as text types, where that is not the press's own text.
#[cfg(windows)]
#[must_use]
pub(crate) fn text_press_repeat(ev: &KeyEvent, mods: Modifiers) -> Option<String> {
    windows_text_repeat(&ev.logical_key, ev.text.as_deref(), mods)
}

/// [`text_press_repeat`] off Windows, where a repeat replays the press.
#[cfg(not(windows))]
#[must_use]
pub(crate) const fn text_press_repeat(_ev: &KeyEvent, _mods: Modifiers) -> Option<String> {
    None
}

/// [`text_press_repeat`]'s answer from the press's parts: the logical key's own
/// character, which winit reports for the second of the two.
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
/// Shift, 2 = Ctrl, 4 = Alt).
#[cfg(any(windows, test))]
const SHIFT_STATE_CTRL_ALT: u8 = 0b0000_0110;

/// The Windows half of [`build_key_input`], pure so the layout cases are testable
/// without a window: `logical_key` is the layout-composed key, `base_key`
/// `key_without_modifiers()`, `base_layout` the physical key's US-QWERTY identity,
/// and `layout_shift_state` answers `VkKeyScanExW`'s "which modifiers does this
/// character need on the current layout?".
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
/// needs Ctrl+Alt.
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
/// return), as for every non-BMP character (the API takes one UTF-16 unit).
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
    // SAFETY: both are pure keyboard-layout queries with no preconditions — `GetKeyboardLayout(0)`
    // names the calling thread and returns a borrowed HKL (nothing to free), which `VkKeyScanExW`
    // only reads.
    let scan = unsafe { VkKeyScanExW(unit, GetKeyboardLayout(0)) };
    if scan == -1 {
        return None;
    }
    u8::try_from((scan >> 8) & 0xff).ok()
}

/// Test-only: thread-local layout [`layout_shift_state`] queries (`keymap_win_replay_tests`).
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

    fn de_de_shift_state(c: char) -> Option<u8> {
        match c {
            '{' | '[' | '@' | '€' | '}' | ']' | '\\' | '|' | '~' => Some(0b0000_0110),
            'a'..='z' | '0'..='9' => Some(0),
            'A'..='Z' => Some(1),
            _ => None,
        }
    }

    fn us_qwerty_shift_state(c: char) -> Option<u8> {
        match c {
            '{' | '@' | 'A'..='Z' | '}' | '|' | '~' => Some(1),
            '[' | 'a'..='z' | '0'..='9' | ']' | '\\' => Some(0),
            _ => None,
        }
    }

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

    #[test]
    fn us_qwerty_is_unaffected() {
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
        for (logical, base) in [("?", '/'), ("A", 'a'), ("@", '2'), ("{", '[')] {
            let key = keyboard::Key::Character(logical.chars().next().unwrap());
            assert_eq!(
                windows_composed_text(&ch(logical), &key, shift, Some(logical)),
                (shift, None),
                "US Shift+{base}"
            );
        }
    }

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
        assert_eq!(
            windows_bytes("A", "A", 'a', Modifiers::empty(), caps, mode),
            b"A"
        );
    }

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
        assert_eq!(locks, lock_modifiers());
        if let Some(on) = hid_lock_state::caps_lock() {
            assert_eq!(on, locks.contains(Modifiers::CAPS_LOCK));
        }
    }

    #[test]
    fn super_modifier_carried_through() {
        let m = modifiers_from_winit(ModifiersState::SUPER);
        assert!(m.contains(Modifiers::SUPER));
    }

    #[test]
    fn commit_sends_committed_text() {
        assert_eq!(
            encode_committed_text("hi", KeyboardMode::empty()),
            b"hi".to_vec()
        );
        assert_eq!(
            encode_committed_text("日本", KeyboardMode::empty()),
            "日本".as_bytes().to_vec()
        );
        assert!(encode_committed_text("", KeyboardMode::empty()).is_empty());
    }

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

    #[test]
    fn commit_under_report_all_with_text_is_one_key0_event() {
        let mode = KeyboardMode::REPORT_ALL_KEYS_AS_ESC
            | KeyboardMode::REPORT_ASSOCIATED_TEXT
            | KeyboardMode::REPORT_EVENT_TYPES;
        assert_eq!(
            encode_committed_text("日本", mode),
            b"\x1b[0;1;26085:26412u".to_vec(),
            "one key-0 event for the whole commit, codepoints colon-joined"
        );
        assert_eq!(
            encode_committed_text("日", mode),
            b"\x1b[0;1;26085u".to_vec()
        );
    }

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

    fn seam_bytes(ev: &KeyEvent, mode: KeyboardMode) -> Vec<u8> {
        let (key, mods, base, text) =
            build_key_input(ev, Modifiers::empty(), false).expect("the key maps");
        keyboard::encode_key_with_text(&key, mods, mode, KeyEventType::Press, base, text)
    }

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
        let dot = press_at_with_base(
            KeyCode::NumpadDecimal,
            ch("."),
            WinitKey::Named(WinitNamed::Delete),
            KeyLocation::Numpad,
        );
        assert_eq!(seam_bytes(&dot, KeyboardMode::empty()), b".");
        assert_eq!(crate::app_input::base_logical_key(&ev), ch("1"));
        let kp3 = press_at_with_base(
            KeyCode::Numpad3,
            ch("3"),
            WinitKey::Named(WinitNamed::PageDown),
            KeyLocation::Numpad,
        );
        assert_eq!(crate::app_input::base_logical_key(&kp3), ch("3"));
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
        let shifted = press_at_with_base(KeyCode::Digit1, ch("!"), ch("1"), KeyLocation::Standard);
        assert_eq!(crate::app_input::base_logical_key(&shifted), ch("1"));
    }

    #[test]
    fn numlock_off_keypad_centre_is_the_letter_form_never_a_digit() {
        use aterm_types::keyboard::NamedKey;
        use aterm_window::keyboard::{KeyLocation, NativeKey};
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
    /// capital, ESC `I`, where Alt+Shift+i without the lock is ESC `İ`.
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
