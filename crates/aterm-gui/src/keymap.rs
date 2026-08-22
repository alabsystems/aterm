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
//! `aterm_types::keyboard` (the `winit-keymap` feature, K-2) so the future
//! native shell reuses the same table.

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
        fn IOHIDGetModifierLockState(handle: IoConnect, selector: c_int, state: *mut bool) -> KernReturn;
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
    let key = keyboard::map_logical_key(logical_key)?;
    // base_layout_key only matters for Character keys under REPORT_ALTERNATE_KEYS;
    // the engine ignores it otherwise, so deriving it unconditionally is harmless.
    let base_layout = keyboard::base_layout_key_for(physical_key);
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
    if keyboard::map_logical_key(&ev.logical_key)
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
    let key = keyboard::map_logical_key(&ev.key_without_modifiers())?;
    Some((key, mods, keyboard::base_layout_key_for(ev.physical_key)))
}

/// Fallback for [`build_key_input`] on platforms WITHOUT
/// `KeyEventExtModifierSupplement` (web/…) — and DELIBERATELY on Windows, where
/// the extension exists but must not be used for the ENCODE path: the Windows
/// backend filters Ctrl/Alt out of `ModifiersState` while AltGr is held, so
/// `key_without_modifiers()` would encode the layout BASE key ('q') with no
/// modifiers instead of the AltGr-composed glyph ('@' on de-DE) the app expects.
/// The binding-LOOKUP path (`app_input::base_logical_key`) does use the
/// extension on Windows; only PTY encoding keeps the composed `logical_key`.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[must_use]
pub fn build_key_input(
    ev: &KeyEvent,
    mods: Modifiers,
) -> Option<(keyboard::Key, Modifiers, Option<char>)> {
    let key = keyboard::map_logical_key(&ev.logical_key)?;
    Some((key, mods, keyboard::base_layout_key_for(ev.physical_key)))
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
