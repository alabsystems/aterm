// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure keyboard-event → PTY-bytes decision (K-1).
//!
//! The GUI used to hand-roll character encoding in `on_key`: `(c.to_uppercase()
//! as u8) & 0x1f` for Ctrl and a raw `ev.text` write otherwise. That bypassed
//! the engine's protocol, so Alt/Option lost its ESC prefix, Ctrl on non-alpha
//! keys and the Kitty char encoding (CSI-u, alternate/base-layout keys) all
//! diverged from `aterm_types::keyboard::encode_key`. This module routes EVERY
//! key through the engine encoder and exposes the decision as a PURE function so
//! it is unit-testable without a window or event loop.
//!
//! The winit→engine key map (Character / Named / numpad / base-layout) lives in
//! the `aterm-winit-keymap` crate (K-2) so the future native shell reuses the
//! same table. It is a CRATE and not a feature of `aterm-types` because a
//! workspace resolve unifies optional features: as a feature it linked AppKit
//! into every consumer of `aterm-types`, including the dependency-free
//! `aterm-ctl`.

use aterm_types::keyboard::{self, KeyEventType, KeyboardMode, Modifiers};
use winit::event::KeyEvent;
use winit::keyboard::{Key as WinitKey, ModifiersState, PhysicalKey};

/// Translate winit's [`ModifiersState`] into the engine's [`Modifiers`].
///
/// Super/Cmd is carried through (the old inline path dropped it); the engine's
/// encoder uses it for the Kitty/xterm modifier value and for `modifyOtherKeys`.
#[must_use]
pub fn modifiers_from_winit(mods: ModifiersState) -> Modifiers {
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

/// The lock-key modifiers (Caps Lock / Num Lock) for the Kitty modifier byte.
///
/// winit's portable [`ModifiersState`] carries only Shift/Ctrl/Alt/Super, so the
/// Kitty `CAPS_LOCK`/`NUM_LOCK` bits (which `Modifiers::kitty_encoded` folds into
/// the reported value) must come from a platform query. On macOS we ask the
/// kernel's HID system directly (`IOHIDGetModifierLockState` on the IOHIDSystem
/// user client — see [`hid_lock_state`]); macOS hardware has no Num Lock, so only
/// Caps Lock is reported. Off macOS this is empty until a platform lock-state
/// source is wired (winit exposes none).
///
/// LAW (the 2026-08-17 WindowServer watchdog incident): this query MUST NOT go
/// through WindowServer. It used to read AppKit's global modifier flags, whose
/// first call lazily opens a SkyLight connection and asks WindowServer for the
/// event shmem; WindowServer's MAIN THREAD then runs a synchronous TCC
/// Input-Monitoring preflight for the caller's code identity, and tccd's
/// identify step `readdir`s the executable's parent directory. From a unit test
/// binary in a 1.1-million-entry `target/debug/deps` that scan outran the 40 s
/// watchdog: WindowServer was killed and every GUI session on the machine died.
/// A kernel round-trip has no such path; and no test's or headless instance's
/// `App` reaches even that — they get [`no_lock_modifiers`] through the injected
/// `App::lock_modifiers` field. (This module's own test does call it once,
/// deliberately: a real, kernel-only round-trip.)
#[cfg(target_os = "macos")]
#[must_use]
pub fn lock_modifiers() -> Modifiers {
    match hid_lock_state::caps_lock() {
        Some(true) => Modifiers::CAPS_LOCK,
        Some(false) | None => Modifiers::empty(),
    }
}

/// The lock-state source for HEADLESS instances and unit tests: always empty.
///
/// A headless `App` has no window and therefore no live keyboard whose LEDs
/// could matter, and a unit test must encode the same bytes on every machine
/// regardless of the operator's Caps Lock — so neither may consult the platform.
/// This is also the guarantee that keeps a test binary from ever reaching
/// WindowServer through the lock-key path (see [`lock_modifiers`]).
#[must_use]
pub fn no_lock_modifiers() -> Modifiers {
    Modifiers::empty()
}

/// Caps Lock straight from the kernel's HID system — the one lock-key source
/// on macOS that never involves WindowServer.
///
/// Tiny-FFI posture, same as the Windows `GetKeyState` path below: IOKit is a
/// system framework (no crate on the dependency surface), five functions, all
/// declared against the SDK's `IOKitLib.h`/`hidsystem/IOHIDLib.h`. The user
/// client is opened ONCE per process and cached (`io_connect_t` is a mach port,
/// valid process-wide, and `IOHIDGetModifierLockState` is a plain synchronous
/// call on it); if the open fails the failure is cached too and every read is
/// `None` — Caps Lock then simply isn't reported in the Kitty modifier byte,
/// which is the same degraded truth the non-macOS/non-Windows fallback ships.
/// Measured on macOS 26.5.1: `IOServiceOpen(kIOHIDParamConnectType)` and the
/// read both return `KERN_SUCCESS` unentitled, and the value tracks the LED
/// (set via `IOHIDSetModifierLockState`, read back 1, restored, read back 0).
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
            // consumes (its documented contract), so nothing leaks on either
            // path; a NULL dictionary makes the lookup return 0. `IOServiceOpen`
            // writes the connect port only on success; `mach_task_self()` is
            // our own task. The service ref is released after the open, which
            // holds its own reference. `mach_task_self_` is libSystem's cached
            // task port. All calls are thread-safe kernel RPCs.
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

/// Windows: the live toggle state via user32 `GetKeyState` — the low-order bit
/// of the returned SHORT is the toggled (LED) state for `VK_CAPITAL` /
/// `VK_NUMLOCK`. Same tiny-FFI posture as `clipboard_win` (user32 is on the
/// approved list; there is no std alternative).
#[cfg(windows)]
#[must_use]
pub fn lock_modifiers() -> Modifiers {
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
pub fn lock_modifiers() -> Modifiers {
    Modifiers::empty()
}

/// The PURE key-encoding decision (K-1): given a winit key event, the live
/// modifiers, and the terminal's current [`KeyboardMode`], return the bytes to
/// write to the PTY — or `None` when the event maps to no terminal sequence
/// (an unencodable key, or a bare modifier press).
///
/// All encoding is delegated to `aterm_types::keyboard::encode_key_with_layout`,
/// so Ctrl/Alt/Shift, the legacy vs Kitty vs `modifyOtherKeys` selection, and
/// the alternate/base-layout key reporting are exactly the engine's protocol —
/// no `& 0x1f`, no raw-text passthrough. The `base_layout_key` (US-QWERTY
/// equivalent of the physical key) is supplied for the Kitty
/// `REPORT_ALTERNATE_KEYS` enhancement.
///
/// `logical_key` is the key to encode: pass `key_without_modifiers()` so a
/// composed character (Option+a → "å") is NOT what gets encoded — Alt must
/// produce the ESC-prefixed base key, not the composed glyph.
///
/// Phase 0.5: the GUI no longer calls this directly (the seam owns encoding via
/// `build_key_input` + the engine encoder). It is retained as the documented,
/// unit-tested pure decision (the `keymap::tests` module exercises it).
#[cfg_attr(not(test), allow(dead_code))]
#[must_use]
pub fn encode_key_event(
    logical_key: &WinitKey,
    physical_key: PhysicalKey,
    mods: Modifiers,
    mode: KeyboardMode,
) -> Option<Vec<u8>> {
    let key = aterm_winit_keymap::map_logical_key(logical_key)?;
    // base_layout_key only matters for Character keys under REPORT_ALTERNATE_KEYS;
    // the engine ignores it otherwise, so deriving it unconditionally is harmless.
    let base_layout = aterm_winit_keymap::base_layout_key_for(physical_key);
    let bytes =
        keyboard::encode_key_with_layout(&key, mods, mode, KeyEventType::Press, base_layout);
    if bytes.is_empty() { None } else { Some(bytes) }
}

/// SELECTION CUSTODY (R1): whether this winit press is a bare MODIFIER or LOCK
/// key — Shift, Control, Alt/Option, Super/Command, Hyper, Meta, Caps/Num/Scroll
/// Lock — and so expresses no typing intent.
///
/// The window half of the press-path snap (`cancel_press_scroll_motion`) is
/// deliberately lock-free and therefore lives OUTSIDE the seam, so it needs its
/// own call site answering the question the seam answers with
/// `PressClass::inert_modifier`. Both resolve to the same authority —
/// `aterm_types::keyboard::is_modifier_or_lock_key` — so a key cannot be inert
/// on one side of the seam and disturbing on the other.
///
/// Reads `logical_key`, NOT `key_without_modifiers()`: the modifier identity is
/// what `logical_key` carries, and the answer must not depend on the platform's
/// modifier-state snapshot (on macOS that snapshot is still stale when the bare
/// modifier's `KeyboardInput` is delivered).
#[must_use]
pub fn press_is_inert(ev: &KeyEvent) -> bool {
    if aterm_winit_keymap::map_logical_key(&ev.logical_key)
        .as_ref()
        .is_some_and(keyboard::is_modifier_or_lock_key)
    {
        return true;
    }
    // The modifier and lock keys winit reports that the ENGINE has no `NamedKey`
    // variant for, so `map_logical_key` returns `None` for them: `AltGraph` (the
    // AltGr of every European layout), macOS/laptop `Fn`/`FnLock`, and
    // `Symbol`/`SymbolLock`. They are modifiers by any honest reading — you hold
    // them to reach another key — and they produce no PTY bytes.
    //
    // The TERMINAL half is already safe for them without this arm: unmapped keys
    // return `None` from `build_key_input`, fall to `on_key`'s un-encodable tail,
    // and never reach the seam at all. The WINDOW half is not, and that is the
    // whole reason this arm exists — without it, resting a finger on AltGr kills
    // an in-flight momentum scroll while Shift (which IS engine-mapped) leaves it
    // running. Same key class, opposite behaviour, for no reason a user could
    // ever infer.
    matches!(
        &ev.logical_key,
        WinitKey::Named(
            winit::keyboard::NamedKey::AltGraph
                | winit::keyboard::NamedKey::Fn
                | winit::keyboard::NamedKey::FnLock
                | winit::keyboard::NamedKey::Symbol
                | winit::keyboard::NamedKey::SymbolLock
        )
    )
}

/// IME-1: whether a direct key send must be SUPPRESSED because an IME
/// composition (CJK / dead key) is currently active.
///
/// `preedit` is the marked text being composed; a non-empty preedit means the
/// keystrokes belong to the composer and the resulting text will arrive via
/// `Ime::Commit` — sending them directly too would double-input. When the
/// preedit is empty (no composition), ASCII typing proceeds normally.
#[must_use]
pub fn suppress_direct_send(preedit: &str) -> bool {
    !preedit.is_empty()
}

/// IME-1: encode committed composition text (`Ime::Commit`) for the PTY.
///
/// Legacy / non-report-all modes: each character is encoded as a `Character` key
/// through the engine (NOT a raw `& 0x1f` byte), so committed CJK/dead-key text
/// goes out exactly as typed text (plain UTF-8). Returns the concatenated bytes
/// (empty for empty input).
///
/// Kitty `REPORT_ALL_KEYS_AS_ESC` + `REPORT_ASSOCIATED_TEXT`: the whole commit is
/// ONE key-0 event with every codepoint colon-joined in the text field —
/// `ESC[0;1;cp1:cp2:…u` (mods subfield 1 = none) — the spec's canonical
/// keyless-text form: "If no known key is associated with the text the key number
/// 0 must be used." The old per-char `encode_key(Character(c))` FABRICATED a key
/// NUMBER per codepoint ("日本" arrived as `ESC[26085u ESC[26412u`), and under
/// `REPORT_EVENT_TYPES` no release ever followed — an orphan press per character.
/// Control-code codepoints are dropped (kitty suppresses text that begins with a
/// control character); a commit of only control codes emits nothing.
///
/// Kitty `REPORT_ALL_KEYS_AS_ESC` without `REPORT_ASSOCIATED_TEXT`: the commit is
/// represented by one keyless event (`ESC[0u`). There is no protocol field in
/// which to carry the committed text when the application opted out of associated
/// text. Inventing one key event per Unicode codepoint is incorrect: an IME commit
/// has no corresponding physical key, and those fabricated numeric CSI-u packets
/// can be exposed as literal bracket-number text by a client that falls back while
/// decoding input.
#[must_use]
pub fn encode_committed_text(text: &str, mode: KeyboardMode) -> Vec<u8> {
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

/// Phase 0.5 (A.2 divergence f/h): the keymap demoted to a BUILDER. Instead of
/// returning PTY bytes (which made `on_key` a second encoder caller), it yields
/// the engine-neutral `(Key, Modifiers, base_layout)` triple for an
/// [`InputEvent::Key`](crate::input::InputEvent::Key). The SEAM (`App::input`) is
/// the sole caller of `encode_key_with_layout`, so a human key and a `key`/`ctrl`
/// verb that build the SAME triple produce byte-identical output — including the
/// Kitty `REPORT_ALTERNATE_KEYS` 3rd field carried by `base_layout`. Bare
/// modifiers DO map (the winit-keymap table canonicalizes winit's sideless
/// `Shift`/`Control`/… to the engine's `ShiftLeft`/`ControlLeft`/… variants); the
/// ENCODER decides what they produce — reported only under kitty
/// `REPORT_ALL_KEYS_AS_ESC`, encoded to nothing otherwise. `None` only for keys
/// with no engine mapping at all (a dead key / `Key::Unidentified`).
// `key_without_modifiers()` (the `KeyEventExtModifierSupplement` trait) is
// available on macOS AND on the Linux X11/Wayland backends, so BOTH map the
// layout BASE key — Alt+<key>/AltGr/Shifted-compose all encode the unshifted key
// the engine expects (mirrors `app_input::base_logical_key`). Without this, X11
// users got the layout-composed/shifted glyph in the PTY encoding.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[must_use]
pub fn build_key_input(
    ev: &KeyEvent,
    mods: Modifiers,
) -> Option<(keyboard::Key, Modifiers, Option<char>)> {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    let key = aterm_winit_keymap::map_logical_key(&ev.key_without_modifiers())?;
    Some((
        key,
        mods,
        aterm_winit_keymap::base_layout_key_for(ev.physical_key),
    ))
}

/// Fallback for [`build_key_input`] on platforms WITHOUT
/// `KeyEventExtModifierSupplement` (web/…) — and DELIBERATELY on Windows, where
/// the extension exists but must not be used for the ENCODE path: the Windows
/// backend filters Ctrl/Alt out of `ModifiersState` while AltGr is held, so
/// `key_without_modifiers()` would encode the layout BASE key ('q') with no
/// modifiers instead of the AltGr-composed glyph ('@' on de-DE) the app expects.
/// The binding-LOOKUP path (`app_input::base_logical_key`) does use the
/// extension on Windows; only PTY encoding keeps the composed `logical_key` —
/// except as the last-resort rescue for a Ctrl+Alt chord the layout composed
/// into nothing, which [`windows_key_input`] documents.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[must_use]
pub fn build_key_input(
    ev: &KeyEvent,
    mods: Modifiers,
) -> Option<(keyboard::Key, Modifiers, Option<char>)> {
    windows_key_input(
        &ev.logical_key,
        &key_without_modifiers(ev),
        aterm_winit_keymap::base_layout_key_for(ev.physical_key),
        mods,
        layout_shift_state,
    )
}

/// The layout-BASE key of a press, for the Ctrl+Alt rescue in
/// [`windows_key_input`]. Windows implements `KeyEventExtModifierSupplement`
/// (macOS and the Linux backends use it as their whole encode path); the
/// remaining `not(macos/linux)` targets — web — do not, and there the rescue
/// simply has no second key to try.
#[cfg(windows)]
fn key_without_modifiers(ev: &KeyEvent) -> WinitKey {
    use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
    ev.key_without_modifiers()
}

#[cfg(all(not(windows), not(any(target_os = "macos", target_os = "linux"))))]
fn key_without_modifiers(ev: &KeyEvent) -> WinitKey {
    ev.logical_key.clone()
}

/// `VkKeyScanExW`'s shift-state bits for "this character needs Ctrl AND Alt" —
/// i.e. AltGr. (`winuser.h`: 1 = Shift, 2 = Ctrl, 4 = Alt.)
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const SHIFT_STATE_CTRL_ALT: u8 = 0b0000_0110;

/// The Windows half of [`build_key_input`], as a PURE decision so the layout
/// cases below are unit-testable without a window, an event loop, or a German
/// keyboard. `logical_key` is the layout-COMPOSED key, `base_key` is
/// `key_without_modifiers()`, `base_layout` the US-QWERTY identity of the
/// physical key, and `layout_shift_state` answers `VkKeyScanExW`'s question
/// "which modifiers does this character need on the CURRENT layout?".
///
/// Two Windows-only layout facts drive it, both flowing from ONE winit
/// behaviour: `WindowsModifiers::remove_only_ctrl` keeps CONTROL in the layout
/// lookup whenever ALT is also down, so a Ctrl+Alt press is resolved against
/// the layout's AltGr table (`keyboard.rs:506`).
///
/// 1. ALTGR COMPOSED AS LEFTCTRL+LEFTALT ENCODES A CONTROL BYTE. winit clears
///    Ctrl/Alt from `ModifiersState` only while the RIGHT Alt is physically down
///    (`keyboard_layout.rs:277`), but Windows equally accepts LCtrl+LAlt as
///    AltGr — that is in fact how winit DETECTS AltGr. So on de-DE, LCtrl+LAlt+8
///    arrives as `Character('[')` with CTRL|ALT still set and the engine emits
///    ESC ESC (`ctrl_character('[')` = 0x1B) instead of a `[`; LCtrl+LAlt+Q
///    ('@') emits ESC NUL. Measured against the shipped layout DLLs with
///    `ToUnicodeEx`: de-DE Ctrl+Alt+{7,8,Q} → `{ [ @`, US-QWERTY Ctrl+Alt+
///    {7,8,A,Q} → nothing at all. So the strip below asks the LAYOUT (one
///    `VkKeyScanExW`, no cache, no 256-key scan) whether the character the user
///    actually got is one that needs Ctrl+Alt: on US-QWERTY no character ever
///    answers yes, which is what makes US unreachable by this path. The RAlt
///    path is untouched — winit already delivered it with no modifiers.
///
/// 2. A GENUINE Ctrl+Alt CHORD RESOLVES TO NOTHING. The same AltGr-first lookup
///    means Ctrl+Alt+A is resolved against a shift state that no layout defines
///    for letters (measured: `ToUnicodeEx` returns 0 on both US and de-DE), so
///    winit reports `Key::Unidentified` and the chord encoded to ZERO bytes on
///    Windows while macOS/Linux emit ESC ^A. When the composed key maps to
///    nothing and both Ctrl and Alt are held, fall back to the base key — the
///    same key macOS/Linux encode from — so the chord keeps its control
///    sequence. Nothing else can reach this arm: without ALT, winit already
///    drops CONTROL from the lookup and hands back the base key itself.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[must_use]
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

/// Whether `logical_key` is a character the CURRENT layout composes with AltGr —
/// the proof that the live Ctrl+Alt bits are AltGr's spelling and not a chord.
///
/// Deliberately narrow: a single-codepoint, non-control character that is NOT
/// merely the key's own base identity, and that `VkKeyScanExW` says is reachable
/// only with Ctrl+Alt held on this layout. A control codepoint is exactly what a
/// real chord would produce, so it can never launder itself through here.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
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

/// `VkKeyScanExW`'s shift state for `c` on the CALLING THREAD's keyboard layout
/// (`GetKeyboardLayout(0)`), or `None` when the layout cannot type it at all
/// (the documented -1 return) — which is also the answer for every non-BMP
/// character, since the API takes one UTF-16 unit.
///
/// Same tiny-FFI posture as [`lock_modifiers`] above: user32 is on the approved
/// list, both calls are pure per-thread queries, and there is no std
/// alternative. One call per Ctrl+Alt press, so no cache is warranted.
#[cfg(windows)]
fn layout_shift_state(c: char) -> Option<u8> {
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

/// No layout to ask off Windows (web): nothing is AltGr-composed there.
#[cfg(all(not(windows), not(any(target_os = "macos", target_os = "linux"))))]
fn layout_shift_state(_c: char) -> Option<u8> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{KeyCode, NamedKey as WinitNamed, SmolStr};

    fn ch(c: &str) -> WinitKey {
        WinitKey::Character(SmolStr::new(c))
    }

    /// Alt+a must emit the ESC-prefixed base key (ESC a), NOT the macOS
    /// Option-composed "å". We pass the layout base key ('a'); the engine
    /// prefixes ESC for ALT. This is THE K-1 regression: the old GUI wrote
    /// `ev.text`, which on macOS is the composed glyph.
    #[test]
    fn alt_a_is_esc_prefixed_not_composed() {
        let bytes = encode_key_event(
            &ch("a"),
            PhysicalKey::Code(KeyCode::KeyA),
            Modifiers::ALT,
            KeyboardMode::empty(),
        )
        .expect("alt+a encodes");
        assert_eq!(
            bytes,
            vec![0x1b, b'a'],
            "Alt+a must be ESC a, not composed å"
        );
    }

    /// Ctrl+Space => NUL (0x00). The old `& 0x1f` branch only fired for ASCII
    /// alphabetics, so Ctrl+Space fell through to a raw " " write — wrong.
    #[test]
    fn ctrl_space_is_nul() {
        let bytes = encode_key_event(
            &WinitKey::Named(WinitNamed::Space),
            PhysicalKey::Code(KeyCode::Space),
            Modifiers::CTRL,
            KeyboardMode::empty(),
        )
        .expect("ctrl+space encodes");
        assert_eq!(bytes, vec![0x00], "Ctrl+Space must be NUL");
    }

    /// Ctrl+\\ => FS (0x1c). The old `& 0x1f` branch ignored non-alpha keys, so
    /// Ctrl+\\ produced a raw backslash instead of the control byte.
    #[test]
    fn ctrl_backslash_is_fs() {
        let bytes = encode_key_event(
            &ch("\\"),
            PhysicalKey::Code(KeyCode::Backslash),
            Modifiers::CTRL,
            KeyboardMode::empty(),
        )
        .expect("ctrl+backslash encodes");
        assert_eq!(bytes, vec![0x1c], "Ctrl+\\ must be FS (0x1c)");
    }

    /// Under DISAMBIGUATE_ESC_CODES (Kitty) a *plain* printable key still emits
    /// its text byte — disambiguate only escapes genuinely ambiguous keys (Esc,
    /// Ctrl/Alt combos). Over-escaping `a` to `ESC [ 97 u` was a confirmed
    /// defect that corrupted ordinary typing.
    #[test]
    fn printable_under_disambiguate_is_plain_text() {
        let bytes = encode_key_event(
            &ch("a"),
            PhysicalKey::Code(KeyCode::KeyA),
            Modifiers::empty(),
            KeyboardMode::DISAMBIGUATE_ESC_CODES,
        )
        .expect("a encodes");
        assert_eq!(
            bytes, b"a",
            "plain printable under Kitty disambiguate must be its text byte"
        );
    }

    /// Existing Ctrl-C / Ctrl-D byte output is UNCHANGED by the rewrite: the
    /// classic control bytes 0x03 / 0x04 still reach the PTY.
    #[test]
    fn ctrl_c_and_ctrl_d_unchanged() {
        let c = encode_key_event(
            &ch("c"),
            PhysicalKey::Code(KeyCode::KeyC),
            Modifiers::CTRL,
            KeyboardMode::empty(),
        )
        .expect("ctrl+c encodes");
        assert_eq!(c, vec![0x03], "Ctrl-C must stay 0x03");
        let d = encode_key_event(
            &ch("d"),
            PhysicalKey::Code(KeyCode::KeyD),
            Modifiers::CTRL,
            KeyboardMode::empty(),
        )
        .expect("ctrl+d encodes");
        assert_eq!(d, vec![0x04], "Ctrl-D must stay 0x04");
    }

    /// A plain printable key with no modifiers and no Kitty mode writes the
    /// literal byte — ordinary ASCII typing still works after the rewrite.
    #[test]
    fn plain_ascii_writes_literal() {
        let bytes = encode_key_event(
            &ch("a"),
            PhysicalKey::Code(KeyCode::KeyA),
            Modifiers::empty(),
            KeyboardMode::empty(),
        )
        .expect("a encodes");
        assert_eq!(bytes, vec![b'a']);
    }

    /// Regression (the "Shift doesn't work" report): Shift+<symbol> must encode
    /// the SHIFTED glyph. On macOS the GUI hands the engine the UNSHIFTED base key
    /// (`key_without_modifiers()` → '2') plus the SHIFT modifier, so the engine's
    /// legacy encoder is solely responsible for producing '@'. It used to
    /// `to_ascii_uppercase` the base key, which no-ops on digits/symbols, so every
    /// shifted symbol was lost. Letters masked the gap (they DO uppercase), which
    /// is why it slipped past the unit tests twice.
    #[test]
    fn shift_symbol_encodes_shifted_glyph() {
        let bytes = encode_key_event(
            &ch("2"),
            PhysicalKey::Code(KeyCode::Digit2),
            Modifiers::SHIFT,
            KeyboardMode::empty(),
        )
        .expect("shift+2 encodes");
        assert_eq!(bytes, b"@", "Shift+2 must emit '@', not '2'");
    }

    /// A bare modifier press (Shift alone) encodes to nothing in legacy mode.
    #[test]
    fn bare_modifier_press_is_none() {
        assert_eq!(
            encode_key_event(
                &WinitKey::Named(WinitNamed::Shift),
                PhysicalKey::Code(KeyCode::ShiftLeft),
                Modifiers::SHIFT,
                KeyboardMode::empty(),
            ),
            None,
            "a bare Shift press must produce no bytes in legacy mode"
        );
    }

    /// The Windows lock-state query returns ONLY the two lock bits (whatever
    /// the host's live toggle state is — a real GetKeyState round-trip, callable
    /// without a window or event loop).
    #[cfg(windows)]
    #[test]
    fn lock_modifiers_reports_only_lock_bits() {
        let locks = lock_modifiers();
        assert!(
            (Modifiers::CAPS_LOCK | Modifiers::NUM_LOCK).contains(locks),
            "lock_modifiers must set nothing beyond CAPS_LOCK/NUM_LOCK, got {locks:?}"
        );
    }

    /// The two layouts the Windows arm below is pinned against, as
    /// `VkKeyScanExW` actually answers on the shipped layout DLLs (measured
    /// 2026-08-22 against `00000407`/`00000409`; 1 = Shift, 2 = Ctrl, 4 = Alt,
    /// `None` = the layout cannot type this character at all).
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn de_de_shift_state(c: char) -> Option<u8> {
        match c {
            // AltGr characters: 0x0637 '{', 0x0638 '[', 0x0651 '@', 0x0645 '€'.
            '{' | '[' | '@' | '€' | '}' | ']' | '\\' | '|' | '~' => Some(0b0000_0110),
            'a'..='z' | '0'..='9' => Some(0),
            'A'..='Z' => Some(1),
            _ => None,
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn us_qwerty_shift_state(c: char) -> Option<u8> {
        match c {
            // 0x01DB '{', 0x00DB '[', 0x0132 '@' — Shift or nothing, never 6.
            '{' | '@' | 'A'..='Z' | '}' | '|' | '~' => Some(1),
            '[' | 'a'..='z' | '0'..='9' | ']' | '\\' => Some(0),
            // 0xFFFF: no US key types '€'.
            _ => None,
        }
    }

    /// AltGr spelled the way Windows equally accepts and compact/RDP keyboards
    /// force — LEFT Ctrl + LEFT Alt — must type the composed character, not a
    /// control byte. winit only clears Ctrl/Alt for the RIGHT Alt, so before the
    /// strip de-DE AltGr+8 ('[') encoded ESC ESC and AltGr+Q ('@') encoded
    /// ESC NUL: a German user typing a brace got a control code.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn de_de_altgr_as_left_ctrl_alt_types_the_character() {
        for (composed, digit, base, want) in [
            ("{", KeyCode::Digit7, '7', &b"{"[..]),
            ("[", KeyCode::Digit8, '8', &b"["[..]),
            ("@", KeyCode::KeyQ, 'q', &b"@"[..]),
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
            let bytes = encode_key_event(
                &ch(composed),
                PhysicalKey::Code(digit),
                mods,
                KeyboardMode::empty(),
            )
            .expect("the composed character encodes");
            assert_eq!(bytes, want, "de-DE AltGr must type {composed}");
        }
    }

    /// The RIGHT-Alt spelling is winit's own path: it already cleared Ctrl/Alt
    /// from `ModifiersState`, so the strip has nothing to do and must not
    /// disturb the character.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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

    /// The guard on the strip: a GENUINE Ctrl+Alt+letter chord keeps its
    /// modifiers and still encodes as a control sequence (ESC ^A). Both shapes
    /// are covered — the layout that composes nothing for the chord (measured:
    /// `ToUnicodeEx` returns 0 for Ctrl+Alt+A on US AND de-DE, so winit reports
    /// `Unidentified` and the chord used to encode NO bytes at all on Windows),
    /// and a layout that hands back a plain letter, which `VkKeyScanExW` reports
    /// as needing no modifiers and so can never look like AltGr.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn genuine_ctrl_alt_letter_still_encodes_a_control_sequence() {
        let unidentified = WinitKey::Unidentified(winit::keyboard::NativeKey::Unidentified);
        for logical in [&unidentified, &ch("a")] {
            let (key, mods, _) = windows_key_input(
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
            let bytes = encode_key_event(
                &ch("a"),
                PhysicalKey::Code(KeyCode::KeyA),
                mods,
                KeyboardMode::empty(),
            )
            .expect("Ctrl+Alt+A encodes");
            assert_eq!(bytes, b"\x1b\x01", "Ctrl+Alt+A must stay ESC ^A");
        }
    }

    /// US-QWERTY is structurally out of reach of the strip: no character on it
    /// needs Ctrl+Alt, so `VkKeyScanExW` never answers 6 and nothing is ever
    /// stripped — including the '{' that US types with plain Shift.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn us_qwerty_is_unaffected() {
        // The same composed '{' a de-DE AltGr press produces, on a layout that
        // reaches it with Shift: the modifiers must survive untouched.
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
        // And the ordinary shifted press stays exactly as it was.
        let (key, mods, _) = windows_key_input(
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
            encode_key_event(
                &ch("{"),
                PhysicalKey::Code(KeyCode::BracketLeft),
                mods,
                KeyboardMode::empty(),
            )
            .expect("Shift+[ encodes"),
            b"{"
        );
    }

    /// The macOS lock-state query is a real IOKit HID round-trip (kernel, never
    /// WindowServer — see `hid_lock_state`), callable without a window or event
    /// loop, and reports ONLY Caps Lock (macOS has no Num Lock). It is allowed
    /// to find the HID system unreachable (a sandbox) — then it reports nothing.
    #[cfg(target_os = "macos")]
    #[test]
    fn lock_modifiers_reports_only_caps_lock_from_the_hid_system() {
        let locks = lock_modifiers();
        assert!(
            Modifiers::CAPS_LOCK.contains(locks),
            "lock_modifiers must set nothing beyond CAPS_LOCK, got {locks:?}"
        );
        // Both reads agree with each other (the connection is cached, the value
        // is the same LED) — a second call is not a second open.
        assert_eq!(locks, lock_modifiers());
        if let Some(on) = hid_lock_state::caps_lock() {
            assert_eq!(on, locks.contains(Modifiers::CAPS_LOCK));
        }
    }

    /// The headless/test source is inert on every platform: this is what keeps a
    /// unit test's encoded bytes machine-independent, and what keeps a test
    /// binary off every platform lock-key path (2026-08-17).
    #[test]
    fn no_lock_modifiers_is_empty() {
        assert!(no_lock_modifiers().is_empty());
    }

    /// winit ModifiersState → engine Modifiers carries Super/Cmd through
    /// (the old inline path dropped it).
    #[test]
    fn super_modifier_carried_through() {
        let m = modifiers_from_winit(ModifiersState::SUPER);
        assert!(m.contains(Modifiers::SUPER));
    }

    /// IME-1: while a composition is active (non-empty preedit), direct key
    /// sends are SUPPRESSED so the composing keystrokes don't double-input; with
    /// no composition (empty preedit) they proceed.
    #[test]
    fn composition_suppresses_direct_send() {
        assert!(
            suppress_direct_send("か"),
            "an active preedit must suppress direct key sends"
        );
        assert!(
            !suppress_direct_send(""),
            "no composition (empty preedit) must NOT suppress direct sends"
        );
    }

    /// IME-1: a committed composition is encoded as ordinary text through the
    /// engine (each char a `Character` key), NOT a raw `& 0x1f` byte. ASCII
    /// commits round-trip to their bytes; multi-byte CJK to their UTF-8.
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

    /// Under kitty REPORT_ALL_KEYS_AS_ESC + REPORT_ASSOCIATED_TEXT (+ event types,
    /// the mode a full-protocol app negotiates) an IME commit is ONE key-0 event
    /// carrying every codepoint colon-joined in the text field — the spec's
    /// keyless-text form. The old per-char path fabricated a key NUMBER per
    /// codepoint (`ESC[26085u ESC[26412u`), and under REPORT_EVENT_TYPES no
    /// release ever paired with those fabricated presses.
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

    /// Key-0 commits FILTER control-code codepoints (kitty suppresses text that
    /// begins with a control character); a commit of only control codes emits
    /// nothing at all — never a payload-less `ESC[0;1;u`.
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
}
