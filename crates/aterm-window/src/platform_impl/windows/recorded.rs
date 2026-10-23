// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Recorded Windows keyboard and IME message replay through the real key path.

use std::collections::{HashMap, VecDeque};

use windows_sys::Win32::UI::Input::Ime::{GCS_COMPATTR, GCS_COMPSTR, GCS_RESULTSTR};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_ADD, VK_CAPITAL, VK_CONTROL, VK_DECIMAL, VK_DIVIDE, VK_LCONTROL, VK_LMENU, VK_LSHIFT,
    VK_MENU, VK_MULTIPLY, VK_NUMLOCK, VK_NUMPAD0, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_SCROLL,
    VK_SHIFT, VK_SUBTRACT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MSG, WM_CHAR, WM_DEADCHAR, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_STARTCOMPOSITION,
    WM_KEYDOWN, WM_KEYUP, WM_SYSCHAR, WM_SYSDEADCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::event::{Ime, KeyEvent};
use crate::keyboard::{Key, KeyCode, ModifiersState, NamedKey, NativeKey, SmolStr};

use super::event_loop::ProcResult;
use super::ime::{ime_events, CompositionSource};
use super::keyboard::{replay, KeyEventBuilder};
use super::keyboard_layout::{Layout, WindowsModifiers, LAYOUT_CACHE};
use super::window_state::ImeState;

/// One character key of a recorded layout: position, virtual key, levels, and Caps Lock.
pub struct LayoutKey {
    pub code: KeyCode,
    pub vk: u16,
    pub levels: [Option<Key>; 4],
    pub caps: bool,
}

/// A character at a level.
pub fn ch(text: &str) -> Option<Key> {
    Some(Key::Character(SmolStr::new(text)))
}

/// A dead key at a level.
pub fn dead(accent: char) -> Option<Key> {
    Some(Key::Dead(Some(accent)))
}

/// A letter: `lower`, its capital with Shift, Caps Lock shifting it, and the AltGr levels.
pub fn letter(code: KeyCode, vk: u16, lower: &str, altgr: [Option<Key>; 2]) -> LayoutKey {
    let [altgr, altgr_shift] = altgr;
    LayoutKey {
        code,
        vk,
        levels: [ch(lower), ch(&lower.to_uppercase()), altgr, altgr_shift],
        caps: true,
    }
}

/// A key Caps Lock leaves alone, with its four levels.
pub fn symbol(code: KeyCode, vk: u16, levels: [Option<Key>; 4]) -> LayoutKey {
    LayoutKey { code, vk, levels, caps: false }
}

/// The part of a keyboard layout the recordings press, as `ToUnicodeEx` answers for it.
pub struct RecordedLayout {
    /// The LANGID in the layout handle's low word (`0x0407`, de-DE).
    pub lang: u16,
    /// Distinguishes two recorded layouts of one language (US and US-International).
    pub variant: u16,
    /// Whether Ctrl+Alt (AltGr) types characters.
    pub has_alt_graph: bool,
    /// The NumLock-on keypad's decimal key.
    pub decimal: &'static str,
    pub keys: Vec<LayoutKey>,
}

impl RecordedLayout {
    /// Synthetic layout handle returned for `GetKeyboardLayout(0)` during replay.
    pub fn hkl(&self) -> u64 {
        ((0xA7E0 + u64::from(self.variant)) << 16) | u64::from(self.lang)
    }

    /// Builds the [`Layout`] that `LayoutCache::prepare_layout` would build.
    fn layout(&self) -> Layout {
        let mut keys = HashMap::new();
        for bits in 0..WindowsModifiers::FLAGS_END.bits() {
            let mods = WindowsModifiers::from_bits_retain(bits);
            let altgr = mods.contains(WindowsModifiers::CONTROL | WindowsModifiers::ALT);
            let mut map = HashMap::new();
            for key in &self.keys {
                let shift = mods.contains(WindowsModifiers::SHIFT)
                    ^ (key.caps && mods.contains(WindowsModifiers::CAPS_LOCK));
                let level = usize::from(shift) + if altgr { 2 } else { 0 };
                let value = key.levels[level].clone();
                map.insert(
                    key.code,
                    value.unwrap_or(Key::Unidentified(NativeKey::Windows(key.vk))),
                );
            }
            map.insert(KeyCode::AltLeft, Key::Named(NamedKey::Alt));
            map.insert(
                KeyCode::AltRight,
                Key::Named(if self.has_alt_graph { NamedKey::AltGraph } else { NamedKey::Alt }),
            );
            keys.insert(mods, map);
        }
        let mut numlock_on_keys = HashMap::new();
        for digit in 0..10u16 {
            numlock_on_keys
                .insert(VK_NUMPAD0 + digit, Key::Character(SmolStr::new(digit.to_string())));
        }
        for (vk, text) in [
            (VK_DECIMAL, self.decimal),
            (VK_MULTIPLY, "*"),
            (VK_ADD, "+"),
            (VK_SUBTRACT, "-"),
            (VK_DIVIDE, "/"),
        ] {
            numlock_on_keys.insert(vk, Key::Character(SmolStr::new(text)));
        }
        Layout {
            hkl: self.hkl(),
            numlock_on_keys,
            numlock_off_keys: HashMap::new(),
            keys,
            has_alt_graph: self.has_alt_graph,
        }
    }
}

/// What an input method's context answers while one IME message is handled.
#[derive(Clone, Debug, Default)]
pub struct Composition {
    /// `GCS_RESULTSTR`: the committed text.
    pub result: Option<String>,
    /// `GCS_COMPSTR`: the text being composed.
    pub composing: Option<String>,
    /// `GCS_COMPATTR`: one attribute per UTF-16 unit of `composing`.
    pub attrs: Vec<u8>,
    /// `GCS_CURSORPOS`, in UTF-16 units.
    pub cursor: i32,
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

impl CompositionSource for Composition {
    fn data(&self, gcs_mode: u32) -> Option<Vec<u8>> {
        match gcs_mode {
            GCS_RESULTSTR => Some(self.result.as_deref().map(utf16_bytes).unwrap_or_default()),
            GCS_COMPSTR => Some(self.composing.as_deref().map(utf16_bytes).unwrap_or_default()),
            GCS_COMPATTR => Some(self.attrs.clone()),
            _ => None,
        }
    }

    fn cursor(&self) -> i32 {
        self.cursor
    }
}

/// One recorded window message with its keyboard state and IME composition.
#[derive(Clone, Debug)]
pub struct Message {
    pub message: u32,
    pub wparam: usize,
    pub lparam: isize,
    pub keys: [u8; 256],
    pub composition: Composition,
}

impl Message {
    fn msg(&self) -> MSG {
        MSG {
            hwnd: std::ptr::null_mut(),
            message: self.message,
            wParam: self.wparam,
            lParam: self.lparam,
            time: 0,
            pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
        }
    }
}

/// Builds a Windows key/IME message sequence while tracking the keyboard state.
#[derive(Clone, Debug)]
pub struct Typist {
    keys: [u8; 256],
    messages: Vec<Message>,
    /// Last key press had Alt held without Ctrl (`WM_SYSCHAR`).
    sys: bool,
    last_lparam: isize,
}

impl Default for Typist {
    fn default() -> Self {
        Self::new()
    }
}

impl Typist {
    pub fn new() -> Self {
        Self { keys: [0; 256], messages: Vec::new(), sys: false, last_lparam: 1 }
    }

    /// Start with toggle key `vk` (Caps Lock, Num Lock) on.
    pub fn toggled(mut self, vk: u16) -> Self {
        self.keys[usize::from(vk)] |= 1;
        self
    }

    fn held(&self, vk: u16) -> bool {
        self.keys[usize::from(vk)] & 0x80 != 0
    }

    fn sideless(vk: u16) -> u16 {
        match vk {
            VK_LSHIFT | VK_RSHIFT => VK_SHIFT,
            VK_LCONTROL | VK_RCONTROL => VK_CONTROL,
            VK_LMENU | VK_RMENU => VK_MENU,
            other => other,
        }
    }

    fn sides(sideless: u16) -> Option<[u16; 2]> {
        match sideless {
            VK_SHIFT => Some([VK_LSHIFT, VK_RSHIFT]),
            VK_CONTROL => Some([VK_LCONTROL, VK_RCONTROL]),
            VK_MENU => Some([VK_LMENU, VK_RMENU]),
            _ => None,
        }
    }

    fn push(&mut self, message: u32, wparam: usize, lparam: isize, composition: Composition) {
        self.messages.push(Message { message, wparam, lparam, keys: self.keys, composition });
    }

    /// Press key `vk` (a repeat when it is already down).
    pub fn down(mut self, vk: u16, scan: u16, extended: bool) -> Self {
        let repeat = self.held(vk);
        let sideless = Self::sideless(vk);
        self.keys[usize::from(vk)] |= 0x80;
        self.keys[usize::from(sideless)] |= 0x80;
        if !repeat && matches!(vk, VK_CAPITAL | VK_NUMLOCK | VK_SCROLL) {
            self.keys[usize::from(vk)] ^= 1;
        }
        let alt = self.held(VK_MENU);
        let sys = alt && !self.held(VK_CONTROL);
        let lparam = 1
            | (((scan & 0xff) as isize) << 16)
            | (isize::from(extended) << 24)
            | (isize::from(alt) << 29)
            | (isize::from(repeat) << 30);
        self.sys = sys;
        self.last_lparam = lparam;
        self.push(
            if sys { WM_SYSKEYDOWN } else { WM_KEYDOWN },
            usize::from(sideless),
            lparam,
            Composition::default(),
        );
        self
    }

    /// Release key `vk`.
    pub fn up(mut self, vk: u16, scan: u16, extended: bool) -> Self {
        let sys = self.held(VK_MENU) && !self.held(VK_CONTROL);
        let sideless = Self::sideless(vk);
        self.keys[usize::from(vk)] &= !0x80;
        let other_side_down = Self::sides(sideless)
            .is_some_and(|sides| sides.iter().any(|&side| side != vk && self.held(side)));
        if sideless != vk && !other_side_down {
            self.keys[usize::from(sideless)] &= !0x80;
        }
        let alt = self.held(VK_MENU);
        let lparam = 1
            | (((scan & 0xff) as isize) << 16)
            | (isize::from(extended) << 24)
            | (isize::from(alt) << 29)
            | (1 << 30)
            | (1 << 31);
        self.push(
            if sys { WM_SYSKEYUP } else { WM_KEYUP },
            usize::from(sideless),
            lparam,
            Composition::default(),
        );
        self
    }

    /// The characters the last press typed, one `WM_CHAR` (or `WM_SYSCHAR`) per UTF-16 unit.
    pub fn chars(mut self, text: &str) -> Self {
        let message = if self.sys { WM_SYSCHAR } else { WM_CHAR };
        for unit in text.encode_utf16() {
            let lparam = self.last_lparam;
            self.push(message, usize::from(unit), lparam, Composition::default());
        }
        self
    }

    /// The last press was a dead key for `accent`: `WM_DEADCHAR` (or `WM_SYSDEADCHAR`).
    pub fn dead(mut self, accent: char) -> Self {
        let message = if self.sys { WM_SYSDEADCHAR } else { WM_DEADCHAR };
        let lparam = self.last_lparam;
        self.push(message, accent as usize, lparam, Composition::default());
        self
    }

    /// A character Windows posts with no key of its own (an Alt code's, after Alt's release).
    pub fn posted(mut self, text: &str) -> Self {
        for unit in text.encode_utf16() {
            self.push(WM_CHAR, usize::from(unit), 1, Composition::default());
        }
        self
    }

    /// Appends an IME message (`WM_IME_*`) with its `lparam` flags and context composition.
    pub fn ime(mut self, message: u32, lparam: u32, composition: Composition) -> Self {
        self.push(message, 0, lparam as isize, composition);
        self
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// One event the window would send the application.
#[derive(Clone, Debug, PartialEq)]
pub enum Replayed {
    Modifiers(ModifiersState),
    Key(KeyEvent),
    Ime(Ime),
}

/// Ends the replay on this thread however the caller leaves.
struct Ending;

impl Drop for Ending {
    fn drop(&mut self) {
        replay::set(None);
    }
}

/// Replays `messages` on `layout` through the window's key path and returns the resulting events.
pub fn replay(layout: &RecordedLayout, messages: &[Message]) -> Vec<Replayed> {
    let hkl = layout.hkl();
    LAYOUT_CACHE.lock().unwrap().layouts.insert(hkl, layout.layout());
    let _ending = Ending;
    let builder = KeyEventBuilder::default();
    let mut out = Vec::new();
    let mut mods = ModifiersState::empty();
    let mut ime_state = ImeState::Disabled;
    for (at, message) in messages.iter().enumerate() {
        replay::set(Some(replay::Replay {
            queue: messages[at + 1..].iter().map(Message::msg).collect::<VecDeque<_>>(),
            keys: message.keys,
            hkl,
        }));
        if matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP) {
            let now = LAYOUT_CACHE.lock().unwrap().get_agnostic_mods();
            if now != mods {
                mods = now;
                out.push(Replayed::Modifiers(now));
            }
        }
        match message.message {
            WM_IME_STARTCOMPOSITION | WM_IME_COMPOSITION | WM_IME_ENDCOMPOSITION => {
                let events = ime_events(
                    message.message,
                    message.lparam,
                    true,
                    &mut ime_state,
                    &message.composition,
                );
                out.extend(events.into_iter().map(Replayed::Ime));
            },
            _ => {
                let mut result = ProcResult::DefWindowProc(message.wparam);
                let events = builder.process_message(
                    std::ptr::null_mut(),
                    message.message,
                    message.wparam,
                    message.lparam,
                    &mut result,
                );
                out.extend(events.into_iter().map(|event| Replayed::Key(event.event)));
                if let Some(text) = builder.take_alt_code_commit() {
                    out.push(Replayed::Ime(Ime::Commit(text)));
                }
            },
        }
    }
    out
}

/// Recorded keyboard layouts (`en-US`, `en-US` International, `de-DE`, `fr-FR`, `ja-JP`, `ko-KR`).
pub mod layouts {
    use super::{ch, dead, letter, symbol, LayoutKey, RecordedLayout};
    use crate::keyboard::KeyCode;

    fn us_letters() -> Vec<LayoutKey> {
        [
            (KeyCode::KeyA, 0x41, "a"),
            (KeyCode::KeyC, 0x43, "c"),
            (KeyCode::KeyE, 0x45, "e"),
            (KeyCode::KeyQ, 0x51, "q"),
            (KeyCode::KeyS, 0x53, "s"),
        ]
        .into_iter()
        .map(|(code, vk, lower)| letter(code, vk, lower, [None, None]))
        .collect()
    }

    /// US (`00000409`): no AltGr.
    pub fn en_us() -> RecordedLayout {
        let mut keys = us_letters();
        keys.extend([
            symbol(KeyCode::Digit2, 0x32, [ch("2"), ch("@"), None, None]),
            symbol(KeyCode::Digit6, 0x36, [ch("6"), ch("^"), None, None]),
            symbol(KeyCode::Digit7, 0x37, [ch("7"), ch("&"), None, None]),
            symbol(KeyCode::Slash, 0xBF, [ch("/"), ch("?"), None, None]),
            symbol(KeyCode::Quote, 0xDE, [ch("'"), ch("\""), None, None]),
            symbol(KeyCode::Space, 0x20, [ch(" "), ch(" "), None, None]),
        ]);
        RecordedLayout { lang: 0x0409, variant: 0, has_alt_graph: false, decimal: ".", keys }
    }

    /// United States-International (`00020409`): dead keys and AltGr accented letters.
    pub fn en_us_intl() -> RecordedLayout {
        let keys = vec![
            letter(KeyCode::KeyA, 0x41, "a", [ch("á"), ch("Á")]),
            letter(KeyCode::KeyE, 0x45, "e", [ch("é"), ch("É")]),
            letter(KeyCode::KeyQ, 0x51, "q", [ch("ä"), ch("Ä")]),
            letter(KeyCode::KeyS, 0x53, "s", [ch("ß"), ch("§")]),
            symbol(KeyCode::Quote, 0xDE, [dead('\''), dead('"'), None, None]),
            symbol(KeyCode::Backquote, 0xC0, [dead('`'), dead('~'), None, None]),
            symbol(KeyCode::Digit6, 0x36, [ch("6"), dead('^'), None, None]),
            symbol(KeyCode::Space, 0x20, [ch(" "), ch(" "), None, None]),
        ];
        RecordedLayout { lang: 0x0409, variant: 1, has_alt_graph: true, decimal: ".", keys }
    }

    /// German (`00000407`): QWERTZ, AltGr, `^` and the accent key dead, a `,` keypad decimal.
    pub fn de_de() -> RecordedLayout {
        let keys = vec![
            letter(KeyCode::KeyA, 0x41, "a", [None, None]),
            letter(KeyCode::KeyE, 0x45, "e", [ch("€"), None]),
            letter(KeyCode::KeyQ, 0x51, "q", [ch("@"), None]),
            letter(KeyCode::KeyS, 0x53, "s", [None, None]),
            // QWERTZ: the US Y position types z, the US Z position y.
            letter(KeyCode::KeyY, 0x5A, "z", [None, None]),
            letter(KeyCode::KeyZ, 0x59, "y", [None, None]),
            symbol(KeyCode::Digit2, 0x32, [ch("2"), ch("\""), ch("²"), None]),
            symbol(KeyCode::Digit7, 0x37, [ch("7"), ch("/"), ch("{"), None]),
            symbol(KeyCode::Digit8, 0x38, [ch("8"), ch("("), ch("["), None]),
            symbol(KeyCode::Digit0, 0x30, [ch("0"), ch("="), ch("}"), None]),
            symbol(KeyCode::Minus, 0xDB, [ch("ß"), ch("?"), ch("\\"), None]),
            symbol(KeyCode::Backquote, 0xDC, [dead('^'), ch("°"), None, None]),
            symbol(KeyCode::Equal, 0xDD, [dead('´'), dead('`'), None, None]),
            symbol(KeyCode::BracketRight, 0xBB, [ch("+"), ch("*"), ch("~"), None]),
            symbol(KeyCode::IntlBackslash, 0xE2, [ch("<"), ch(">"), ch("|"), None]),
            symbol(KeyCode::Space, 0x20, [ch(" "), ch(" "), None, None]),
        ];
        RecordedLayout { lang: 0x0407, variant: 0, has_alt_graph: true, decimal: ",", keys }
    }

    /// French (`0000040C`): AZERTY, the digits on Shift, `^` and AltGr's `~` dead.
    pub fn fr_fr() -> RecordedLayout {
        let keys = vec![
            // AZERTY: the US Q position types a, the US A position q.
            letter(KeyCode::KeyQ, 0x41, "a", [None, None]),
            letter(KeyCode::KeyA, 0x51, "q", [None, None]),
            letter(KeyCode::KeyE, 0x45, "e", [ch("€"), None]),
            letter(KeyCode::KeyU, 0x55, "u", [None, None]),
            symbol(KeyCode::Digit1, 0x31, [ch("&"), ch("1"), None, None]),
            symbol(KeyCode::Digit2, 0x32, [ch("é"), ch("2"), dead('~'), None]),
            symbol(KeyCode::Digit3, 0x33, [ch("\""), ch("3"), ch("#"), None]),
            symbol(KeyCode::Digit5, 0x35, [ch("("), ch("5"), ch("["), None]),
            symbol(KeyCode::Digit0, 0x30, [ch("à"), ch("0"), ch("@"), None]),
            symbol(KeyCode::Minus, 0xDB, [ch(")"), ch("°"), ch("]"), None]),
            symbol(KeyCode::BracketLeft, 0xDD, [dead('^'), dead('¨'), None, None]),
            symbol(KeyCode::Space, 0x20, [ch(" "), ch(" "), None, None]),
        ];
        RecordedLayout { lang: 0x040C, variant: 0, has_alt_graph: true, decimal: ".", keys }
    }

    /// Japanese (`00000411`) under the Microsoft IME, whose keys arrive as `VK_PROCESSKEY`.
    pub fn ja_jp() -> RecordedLayout {
        RecordedLayout {
            lang: 0x0411,
            variant: 0,
            has_alt_graph: false,
            decimal: ".",
            keys: us_letters(),
        }
    }

    /// Korean (`00000412`) under the Microsoft IME.
    pub fn ko_kr() -> RecordedLayout {
        RecordedLayout {
            lang: 0x0412,
            variant: 0,
            has_alt_graph: false,
            decimal: ".",
            keys: us_letters(),
        }
    }
}

/// Physical keys `(virtual_key, scan_code, extended)` used by the recordings.
pub mod keys {
    pub type Physical = (u16, u16, bool);
    pub const LCTRL: Physical = (0xA2, 0x1D, false);
    pub const LSHIFT: Physical = (0xA0, 0x2A, false);
    pub const LALT: Physical = (0xA4, 0x38, false);
    /// AltGr: the right Alt key (extended).
    pub const RALT: Physical = (0xA5, 0x38, true);
    pub const A: Physical = (0x41, 0x1E, false);
    pub const C: Physical = (0x43, 0x2E, false);
    pub const E: Physical = (0x45, 0x12, false);
    pub const Q: Physical = (0x51, 0x10, false);
    pub const S: Physical = (0x53, 0x1F, false);
    pub const N: Physical = (0x4E, 0x31, false);
    pub const DIGIT2: Physical = (0x32, 0x03, false);
    pub const DIGIT7: Physical = (0x37, 0x08, false);
    pub const DIGIT0: Physical = (0x30, 0x0B, false);
    pub const SPACE: Physical = (0x20, 0x39, false);
    pub const ENTER: Physical = (0x0D, 0x1C, false);
    pub const CAPS: Physical = (0x14, 0x3A, false);
    pub const NUMPAD0: Physical = (0x60, 0x52, false);
    pub const NUMPAD2: Physical = (0x62, 0x50, false);
    pub const NUMPAD3: Physical = (0x63, 0x51, false);
    /// The keypad's decimal key (`VK_DECIMAL`), `,` on a German layout.
    pub const NUMPAD_DECIMAL: Physical = (0x6E, 0x53, false);
    /// US-International's `'` (a dead key there).
    pub const QUOTE: Physical = (0xDE, 0x28, false);
    /// de-DE's `^` (VK_OEM_5, left of 1).
    pub const DE_CIRCUMFLEX: Physical = (0xDC, 0x29, false);
    /// de-DE's `ß` (VK_OEM_4, right of 0).
    pub const DE_SHARP_S: Physical = (0xDB, 0x0C, false);
    /// fr-FR's `^` (VK_OEM_6, right of P).
    pub const FR_CIRCUMFLEX: Physical = (0xDD, 0x1A, false);
    /// fr-FR's `a` (on the US Q position).
    pub const FR_A: Physical = (0x41, 0x10, false);
    /// fr-FR's `&`/`1` key.
    pub const FR_1: Physical = (0x31, 0x02, false);
    /// A key an input method took (`VK_PROCESSKEY`), on the US N position.
    pub const PROCESS_N: Physical = (0xE5, 0x31, false);
    /// A key an input method took, on Space.
    pub const PROCESS_SPACE: Physical = (0xE5, 0x39, false);
    /// A key an input method took, on Enter.
    pub const PROCESS_ENTER: Physical = (0xE5, 0x1C, false);
    /// `VK_PACKET`: text `SendInput` (an on-screen keyboard, the emoji panel) types, scan 0.
    pub const PACKET: Physical = (0xE7, 0, false);
}

impl Typist {
    /// [`Typist::down`] of a [`keys`] key.
    pub fn press(self, (vk, scan, extended): keys::Physical) -> Self {
        self.down(vk, scan, extended)
    }

    /// [`Typist::up`] of a [`keys`] key.
    pub fn release(self, (vk, scan, extended): keys::Physical) -> Self {
        self.up(vk, scan, extended)
    }

    /// Press `key`, post `text` for it, release it.
    pub fn tap(self, key: keys::Physical, text: &str) -> Self {
        self.press(key).chars(text).release(key)
    }
}

#[cfg(test)]
mod tests {
    use super::keys::*;
    use super::*;
    use crate::event::ElementState;

    fn presses(events: &[Replayed]) -> Vec<(Key, Option<String>)> {
        events
            .iter()
            .filter_map(|event| match event {
                Replayed::Key(key) if key.state == ElementState::Pressed => {
                    Some((key.logical_key.clone(), key.text.as_deref().map(str::to_owned)))
                },
                _ => None,
            })
            .collect()
    }

    fn character(text: &str) -> Key {
        Key::Character(SmolStr::new(text))
    }

    #[test]
    fn altgr_drops_its_fake_ctrl_and_types_the_character_bare() {
        let typed = Typist::new().press(LCTRL).press(RALT).tap(Q, "@").release(LCTRL).release(RALT);
        let events = replay(&layouts::de_de(), typed.messages());
        assert_eq!(
            presses(&events),
            [(Key::Named(NamedKey::AltGraph), None), (character("@"), Some("@".to_owned())),],
            "{events:#?}"
        );
        let at = events
            .iter()
            .position(|e| matches!(e, Replayed::Key(k) if k.logical_key == character("@")))
            .unwrap();
        let mods_then = events[..at]
            .iter()
            .rev()
            .find_map(|e| match e {
                Replayed::Modifiers(m) => Some(*m),
                _ => None,
            })
            .unwrap();
        assert_eq!(mods_then, ModifiersState::empty(), "{events:#?}");
    }

    #[test]
    fn a_dead_key_composes_with_the_next_key() {
        let typed = Typist::new()
            .press(DE_CIRCUMFLEX)
            .dead('^')
            .release(DE_CIRCUMFLEX)
            .tap(E, "ê")
            .press(DE_CIRCUMFLEX)
            .dead('^')
            .release(DE_CIRCUMFLEX)
            .tap(SPACE, "^");
        let events = replay(&layouts::de_de(), typed.messages());
        assert_eq!(presses(&events), [
            (Key::Dead(Some('^')), None),
            (character("ê"), Some("ê".to_owned())),
            (Key::Dead(Some('^')), None),
            (Key::Named(NamedKey::Space), Some("^".to_owned())),
        ]);
        let typed = Typist::new().press(QUOTE).dead('\'').release(QUOTE).tap(S, "'s");
        let events = replay(&layouts::en_us_intl(), typed.messages());
        assert_eq!(presses(&events), [
            (Key::Dead(Some('\'')), None),
            (character("s"), Some("'s".to_owned()))
        ]);
    }

    #[test]
    fn an_alt_code_commits_its_character() {
        let mut typed = Typist::new().toggled(0x90).press(LALT);
        for digit in [NUMPAD0, NUMPAD2, NUMPAD3, NUMPAD3] {
            typed = typed.press(digit).release(digit);
        }
        let typed = typed.release(LALT).posted("é");
        let events = replay(&layouts::en_us(), typed.messages());
        let digits: Vec<_> = presses(&events).into_iter().skip(1).collect();
        assert_eq!(
            digits,
            [0x60, 0x62, 0x63, 0x63]
                .map(|vk| (Key::Unidentified(NativeKey::Windows(vk)), None))
                .to_vec()
        );
        assert_eq!(events.last(), Some(&Replayed::Ime(Ime::Commit("é".to_owned()))));
    }

    #[test]
    fn an_input_methods_result_is_committed_once() {
        let converted = "日本語";
        let typed = Typist::new()
            .ime(WM_IME_STARTCOMPOSITION, 0, Composition::default())
            .press(PROCESS_N)
            .ime(WM_IME_COMPOSITION, GCS_COMPSTR | GCS_COMPATTR, Composition {
                composing: Some("に".into()),
                attrs: vec![0],
                cursor: 1,
                ..Composition::default()
            })
            .release(N)
            .press(PROCESS_SPACE)
            .ime(WM_IME_COMPOSITION, GCS_COMPSTR | GCS_COMPATTR, Composition {
                composing: Some(converted.into()),
                attrs: vec![1, 1, 1],
                cursor: 3,
                ..Composition::default()
            })
            .release(SPACE)
            .press(PROCESS_ENTER)
            .ime(WM_IME_COMPOSITION, GCS_RESULTSTR, Composition {
                result: Some(converted.into()),
                ..Composition::default()
            })
            .ime(WM_IME_ENDCOMPOSITION, 0, Composition {
                result: Some(converted.into()),
                ..Composition::default()
            })
            .release(ENTER);
        let events = replay(&layouts::ja_jp(), typed.messages());
        let ime: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Replayed::Ime(ime) => Some(ime.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(ime, [
            Ime::Enabled,
            Ime::Preedit("に".into(), Some((3, 3))),
            Ime::Preedit(converted.into(), Some((0, converted.len()))),
            Ime::Preedit(String::new(), None),
            Ime::Commit(converted.into()),
            Ime::Disabled,
        ]);
        assert!(
            presses(&events)
                .iter()
                .all(|(key, text)| *key == Key::Named(NamedKey::Process) && text.is_none()),
            "{events:#?}"
        );
    }

    #[test]
    fn the_hangul_ime_commits_once() {
        let typed = Typist::new()
            .ime(WM_IME_STARTCOMPOSITION, 0, Composition::default())
            .ime(WM_IME_COMPOSITION, GCS_COMPSTR, Composition {
                composing: Some("한".into()),
                cursor: 1,
                ..Composition::default()
            })
            .ime(WM_IME_ENDCOMPOSITION, 0, Composition {
                result: Some("한".into()),
                ..Composition::default()
            })
            .ime(WM_IME_COMPOSITION, GCS_RESULTSTR, Composition {
                result: Some("한".into()),
                ..Composition::default()
            });
        let events = replay(&layouts::ko_kr(), typed.messages());
        let commits = events.iter().filter(|e| matches!(e, Replayed::Ime(Ime::Commit(_)))).count();
        assert_eq!(commits, 1, "{events:#?}");
    }

    #[test]
    fn a_packet_key_carries_its_text() {
        let typed = Typist::new().tap(PACKET, "😀");
        let events = replay(&layouts::en_us(), typed.messages());
        assert_eq!(presses(&events), [(
            Key::Unidentified(NativeKey::Windows(0xE7)),
            Some("😀".to_owned())
        )]);
    }
}
