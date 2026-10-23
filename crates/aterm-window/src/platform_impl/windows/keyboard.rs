// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/windows/keyboard.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

// Modified by Andrew Yates for the aterm project in 2026; see the repository NOTICE.

use std::char;
use std::ffi::OsString;
use std::mem::MaybeUninit;
use std::os::windows::ffi::OsStringExt;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Mutex, MutexGuard};

use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, GetKeyboardState, MapVirtualKeyExW, MAPVK_VK_TO_VSC_EX,
    MAPVK_VSC_TO_VK_EX, VIRTUAL_KEY, VK_ABNT_C2, VK_ADD, VK_CAPITAL, VK_CLEAR, VK_CONTROL,
    VK_DECIMAL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_F4, VK_HOME, VK_INSERT, VK_LCONTROL,
    VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_MULTIPLY, VK_NEXT, VK_NUMLOCK, VK_NUMPAD0,
    VK_NUMPAD1, VK_NUMPAD2, VK_NUMPAD3, VK_NUMPAD4, VK_NUMPAD5, VK_NUMPAD6, VK_NUMPAD7, VK_NUMPAD8,
    VK_NUMPAD9, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN,
    VK_SCROLL, VK_SHIFT, VK_SUBTRACT, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PeekMessageW, MSG, PM_NOREMOVE, WM_CHAR, WM_DEADCHAR, WM_KEYDOWN, WM_KEYFIRST, WM_KEYLAST,
    WM_KEYUP, WM_KILLFOCUS, WM_SETFOCUS, WM_SYSCHAR, WM_SYSDEADCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::keyboard::SmolStr;
use aterm_grapheme::GraphemeClusters;
use tracing::{trace, warn};

use crate::event::{ElementState, KeyEvent};
use crate::keyboard::{Key, KeyCode, KeyLocation, NamedKey, NativeKey, NativeKeyCode, PhysicalKey};
use crate::platform_impl::platform::event_loop::ProcResult;
use crate::platform_impl::platform::keyboard_layout::{
    Layout, LayoutCache, WindowsModifiers, LAYOUT_CACHE,
};
use crate::platform_impl::platform::KeyEventExtra;

pub type ExScancode = u16;

pub struct MessageAsKeyEvent {
    pub event: KeyEvent,
    pub is_synthetic: bool,
}

/// Stores information required to make `KeyEvent`s.
pub struct KeyEventBuilder {
    event_info: Mutex<Option<PartialKeyEventInfo>>,
    pending: PendingEventQueue<MessageAsKeyEvent>,
    /// Local PATCH (aterm): the Alt code being typed, and the character the last one composed.
    alt_code: Mutex<AltCode>,
    alt_code_commit: Mutex<Option<String>>,
}
impl Default for KeyEventBuilder {
    fn default() -> Self {
        KeyEventBuilder {
            event_info: Mutex::new(None),
            pending: Default::default(),
            alt_code: Mutex::new(AltCode::default()),
            alt_code_commit: Mutex::new(None),
        }
    }
}
impl KeyEventBuilder {
    /// Local PATCH (aterm): the character the last Alt code composed.
    pub(crate) fn take_alt_code_commit(&self) -> Option<String> {
        self.alt_code_commit.lock().unwrap().take()
    }

    /// LOCAL PATCH (aterm): a key-down for [`AltCode`], with the live Alt and Ctrl state.
    fn note_alt_code_key(&self, vkey: VIRTUAL_KEY) {
        let held = |vk: VIRTUAL_KEY| key_state(vk) < 0;
        let (alt, ctrl) = (held(VK_MENU), held(VK_CONTROL));
        self.alt_code.lock().unwrap().key_down(vkey, alt, ctrl);
    }

    /// LOCAL PATCH (aterm): a WM_CHAR no key-down claimed.
    fn alt_code_char(&self, hwnd: HWND, unit: u16) -> bool {
        if !self.alt_code.lock().unwrap().push(unit) {
            return false;
        }
        let more_char_coming =
            next_kbd_msg(hwnd).map(|m| matches!(m.message, WM_CHAR | WM_SYSCHAR)).unwrap_or(false);
        if !more_char_coming {
            let text = self.alt_code.lock().unwrap().finish();
            if text.is_some() {
                *self.alt_code_commit.lock().unwrap() = text;
            }
        }
        true
    }

    /// Call this function for every window message.
    pub(crate) fn process_message(
        &self,
        hwnd: HWND,
        msg_kind: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        result: &mut ProcResult,
    ) -> Vec<MessageAsKeyEvent> {
        enum MatchResult {
            Nothing,
            TokenToRemove(PendingMessageToken),
            MessagesToDispatch(Vec<MessageAsKeyEvent>),
        }

        let mut matcher = || -> MatchResult {
            match msg_kind {
                WM_SETFOCUS => {
                    let kbd_state = get_async_kbd_state();
                    let key_events = Self::synthesize_kbd_state(ElementState::Pressed, &kbd_state);
                    MatchResult::MessagesToDispatch(self.pending.complete_multi(key_events))
                },
                WM_KILLFOCUS => {
                    *self.alt_code.lock().unwrap() = AltCode::default();
                    let kbd_state = get_kbd_state();
                    let key_events = Self::synthesize_kbd_state(ElementState::Released, &kbd_state);
                    MatchResult::MessagesToDispatch(self.pending.complete_multi(key_events))
                },
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    if msg_kind == WM_SYSKEYDOWN && wparam as VIRTUAL_KEY == VK_F4 {
                        // This is handled in `event_loop.rs`
                        return MatchResult::Nothing;
                    }
                    self.note_alt_code_key(wparam as VIRTUAL_KEY);
                    let pending_token = self.pending.add_pending();
                    *result = ProcResult::Value(0);

                    let next_msg = next_kbd_msg(hwnd);

                    let mut layouts = LAYOUT_CACHE.lock().unwrap();
                    let mut finished_event_info = Some(PartialKeyEventInfo::from_message(
                        wparam,
                        lparam,
                        ElementState::Pressed,
                        &mut layouts,
                    ));
                    let mut event_info = self.event_info.lock().unwrap();
                    *event_info = None;
                    if let Some(next_msg) = next_msg {
                        let next_msg_kind = next_msg.message;
                        let next_belongs_to_this = !matches!(
                            next_msg_kind,
                            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP
                        );
                        if next_belongs_to_this {
                            // store the partial information, and add to it in the upcoming events
                            *event_info = finished_event_info.take();
                        } else {
                            let (_, layout) = layouts.get_current_layout();
                            let is_fake = {
                                let curr_event = finished_event_info.as_ref().unwrap();
                                is_current_fake(curr_event, next_msg, layout)
                            };
                            if is_fake {
                                finished_event_info = None;
                            }
                        }
                    }
                    if let Some(event_info) = finished_event_info {
                        let ev = event_info.finalize();
                        return MatchResult::MessagesToDispatch(self.pending.complete_pending(
                            pending_token,
                            MessageAsKeyEvent { event: ev, is_synthetic: false },
                        ));
                    }
                    MatchResult::TokenToRemove(pending_token)
                },
                WM_DEADCHAR | WM_SYSDEADCHAR => {
                    let pending_token = self.pending.add_pending();
                    *result = ProcResult::Value(0);
                    let event_info = self.event_info.lock().unwrap().take().unwrap();
                    let ev = event_info.finalize();
                    MatchResult::MessagesToDispatch(self.pending.complete_pending(
                        pending_token,
                        MessageAsKeyEvent { event: ev, is_synthetic: false },
                    ))
                },
                WM_CHAR | WM_SYSCHAR => {
                    let mut event_info = self.event_info.lock().unwrap();
                    if event_info.is_none() {
                        drop(event_info);
                        if self.alt_code_char(hwnd, wparam as u16) {
                            *result = ProcResult::Value(0);
                            return MatchResult::Nothing;
                        }
                        trace!(
                            "Received a CHAR message but no `event_info` was available. The \
                             message is probably IME, returning."
                        );
                        return MatchResult::Nothing;
                    }
                    let pending_token = self.pending.add_pending();
                    *result = ProcResult::Value(0);
                    let is_high_surrogate = (0xd800..=0xdbff).contains(&wparam);
                    let is_low_surrogate = (0xdc00..=0xdfff).contains(&wparam);

                    let is_utf16 = is_high_surrogate || is_low_surrogate;

                    if is_utf16 {
                        if let Some(ev_info) = event_info.as_mut() {
                            ev_info.utf16parts.push(wparam as u16);
                        }
                    } else {
                        let utf16parts = match event_info.as_mut() {
                            Some(ev_info) => &mut ev_info.utf16parts,
                            None => {
                                warn!("The event_info was None when it was expected to be some");
                                return MatchResult::TokenToRemove(pending_token);
                            },
                        };
                        let start_offset = utf16parts.len();
                        let new_size = utf16parts.len() + 2;
                        utf16parts.resize(new_size, 0);
                        if let Some(ch) = char::from_u32(wparam as u32) {
                            let encode_len = ch.encode_utf16(&mut utf16parts[start_offset..]).len();
                            let new_size = start_offset + encode_len;
                            utf16parts.resize(new_size, 0);
                        }
                    }
                    std::mem::drop(event_info);
                    let next_msg = next_kbd_msg(hwnd);
                    let more_char_coming = next_msg
                        .map(|m| matches!(m.message, WM_CHAR | WM_SYSCHAR))
                        .unwrap_or(false);
                    if more_char_coming {
                        // characters that need to be appended to this keyboard event
                        MatchResult::TokenToRemove(pending_token)
                    } else {
                        let mut event_info = self.event_info.lock().unwrap();
                        let mut event_info = match event_info.take() {
                            Some(ev_info) => ev_info,
                            None => {
                                warn!("The event_info was None when it was expected to be some");
                                return MatchResult::TokenToRemove(pending_token);
                            },
                        };
                        let mut layouts = LAYOUT_CACHE.lock().unwrap();
                        let kbd_state = get_kbd_state();
                        let mod_state = WindowsModifiers::active_modifiers(&kbd_state);

                        let (_, layout) = layouts.get_current_layout();
                        let ctrl_on = if layout.has_alt_graph {
                            let alt_on = mod_state.contains(WindowsModifiers::ALT);
                            !alt_on && mod_state.contains(WindowsModifiers::CONTROL)
                        } else {
                            mod_state.contains(WindowsModifiers::CONTROL)
                        };

                        if !ctrl_on {
                            event_info.text = PartialText::System(event_info.utf16parts.clone());
                        } else {
                            let mod_no_ctrl = mod_state.remove_only_ctrl();
                            let num_lock_on = kbd_state[VK_NUMLOCK as usize] & 1 != 0;
                            let vkey = event_info.vkey;
                            let physical_key = &event_info.physical_key;
                            let key = layout.get_key(mod_no_ctrl, num_lock_on, vkey, physical_key);
                            event_info.text = PartialText::Text(key.to_text().map(SmolStr::new));
                        }
                        let ev = event_info.finalize();
                        MatchResult::MessagesToDispatch(self.pending.complete_pending(
                            pending_token,
                            MessageAsKeyEvent { event: ev, is_synthetic: false },
                        ))
                    }
                },
                WM_KEYUP | WM_SYSKEYUP => {
                    let pending_token = self.pending.add_pending();
                    *result = ProcResult::Value(0);

                    let mut layouts = LAYOUT_CACHE.lock().unwrap();
                    let event_info = PartialKeyEventInfo::from_message(
                        wparam,
                        lparam,
                        ElementState::Released,
                        &mut layouts,
                    );
                    drop(layouts);
                    let next_msg = next_kbd_msg(hwnd);
                    let mut valid_event_info = Some(event_info);
                    if let Some(next_msg) = next_msg {
                        let mut layouts = LAYOUT_CACHE.lock().unwrap();
                        let (_, layout) = layouts.get_current_layout();
                        let is_fake = {
                            let event_info = valid_event_info.as_ref().unwrap();
                            is_current_fake(event_info, next_msg, layout)
                        };
                        if is_fake {
                            valid_event_info = None;
                        }
                    }
                    if let Some(event_info) = valid_event_info {
                        let event = event_info.finalize();
                        return MatchResult::MessagesToDispatch(self.pending.complete_pending(
                            pending_token,
                            MessageAsKeyEvent { event, is_synthetic: false },
                        ));
                    }
                    MatchResult::TokenToRemove(pending_token)
                },
                _ => MatchResult::Nothing,
            }
        };
        let matcher_result = matcher();
        match matcher_result {
            MatchResult::TokenToRemove(t) => self.pending.remove_pending(t),
            MatchResult::MessagesToDispatch(m) => m,
            MatchResult::Nothing => Vec::new(),
        }
    }

    #[allow(clippy::nonminimal_bool)]
    fn synthesize_kbd_state(
        key_state: ElementState,
        kbd_state: &[u8; 256],
    ) -> Vec<MessageAsKeyEvent> {
        let mut key_events = Vec::new();

        let mut layouts = LAYOUT_CACHE.lock().unwrap();
        let (locale_id, _) = layouts.get_current_layout();

        macro_rules! is_key_pressed {
            ($vk:expr) => {
                kbd_state[$vk as usize] & 0x80 != 0
            };
        }

        let caps_lock_on = kbd_state[VK_CAPITAL as usize] & 1 != 0;
        let num_lock_on = kbd_state[VK_NUMLOCK as usize] & 1 != 0;

        if is_key_pressed!(VK_CAPITAL) {
            let event = Self::create_synthetic(
                VK_CAPITAL,
                key_state,
                caps_lock_on,
                num_lock_on,
                super::handle::from_usize(locale_id as usize),
                &mut layouts,
            );
            if let Some(event) = event {
                key_events.push(event);
            }
        }
        let do_non_modifier = |key_events: &mut Vec<_>, layouts: &mut _| {
            for vk in 0..256 {
                match vk {
                    VK_CONTROL | VK_LCONTROL | VK_RCONTROL | VK_SHIFT | VK_LSHIFT | VK_RSHIFT
                    | VK_MENU | VK_LMENU | VK_RMENU | VK_CAPITAL => continue,
                    _ => (),
                }
                if !is_key_pressed!(vk) {
                    continue;
                }
                let event = Self::create_synthetic(
                    vk,
                    key_state,
                    caps_lock_on,
                    num_lock_on,
                    super::handle::from_usize(locale_id as usize),
                    layouts,
                );
                if let Some(event) = event {
                    key_events.push(event);
                }
            }
        };
        let do_modifier = |key_events: &mut Vec<_>, layouts: &mut _| {
            const CLEAR_MODIFIER_VKS: [VIRTUAL_KEY; 6] =
                [VK_LCONTROL, VK_LSHIFT, VK_LMENU, VK_RCONTROL, VK_RSHIFT, VK_RMENU];
            for vk in CLEAR_MODIFIER_VKS.iter() {
                if is_key_pressed!(*vk) {
                    let event = Self::create_synthetic(
                        *vk,
                        key_state,
                        caps_lock_on,
                        num_lock_on,
                        super::handle::from_usize(locale_id as usize),
                        layouts,
                    );
                    if let Some(event) = event {
                        key_events.push(event);
                    }
                }
            }
        };

        match key_state {
            ElementState::Pressed => {
                do_non_modifier(&mut key_events, &mut layouts);
                do_modifier(&mut key_events, &mut layouts);
            },
            ElementState::Released => {
                do_modifier(&mut key_events, &mut layouts);
                do_non_modifier(&mut key_events, &mut layouts);
            },
        }

        key_events
    }

    fn create_synthetic(
        vk: VIRTUAL_KEY,
        key_state: ElementState,
        caps_lock_on: bool,
        num_lock_on: bool,
        locale_id: HKL,
        layouts: &mut MutexGuard<'_, LayoutCache>,
    ) -> Option<MessageAsKeyEvent> {
        let scancode =
            unsafe { MapVirtualKeyExW(vk as u32, MAPVK_VK_TO_VSC_EX, mapping_layout(locale_id)) };
        if scancode == 0 {
            return None;
        }
        let scancode = scancode as ExScancode;
        let physical_key = scancode_to_physicalkey(scancode as u32);
        let mods =
            if caps_lock_on { WindowsModifiers::CAPS_LOCK } else { WindowsModifiers::empty() };
        let layout = layouts.layouts.get(&(super::handle::to_usize(locale_id) as u64)).unwrap();
        let logical_key = layout.get_key(mods, num_lock_on, vk, &physical_key);
        let key_without_modifiers =
            layout.get_key(WindowsModifiers::empty(), false, vk, &physical_key);
        let text = if key_state == ElementState::Pressed {
            logical_key.to_text().map(SmolStr::new)
        } else {
            None
        };
        let event_info = PartialKeyEventInfo {
            vkey: vk,
            logical_key: PartialLogicalKey::This(logical_key.clone()),
            key_without_modifiers,
            key_state,
            is_repeat: false,
            physical_key,
            location: get_location(scancode, locale_id),
            utf16parts: Vec::with_capacity(8),
            text: PartialText::Text(text.clone()),
        };

        let mut event = event_info.finalize();
        event.logical_key = logical_key;
        event.platform_specific.text_with_all_modifiers = text;
        Some(MessageAsKeyEvent { event, is_synthetic: true })
    }
}

/// LOCAL PATCH (aterm): Windows' Alt-code entry.
#[derive(Default)]
struct AltCode {
    armed: bool,
    units: Vec<u16>,
}

impl AltCode {
    fn key_down(&mut self, vkey: VIRTUAL_KEY, alt: bool, ctrl: bool) {
        let digit = (VK_NUMPAD0..=VK_NUMPAD9).contains(&vkey);
        let modifier = matches!(
            vkey,
            VK_MENU
                | VK_LMENU
                | VK_RMENU
                | VK_SHIFT
                | VK_LSHIFT
                | VK_RSHIFT
                | VK_CONTROL
                | VK_LCONTROL
                | VK_RCONTROL
        );
        if digit && alt && !ctrl {
            self.armed = true;
        } else if !modifier {
            *self = AltCode::default();
        }
    }

    /// Takes a WM_CHAR unit when armed; `false` leaves it to upstream's IME path.
    fn push(&mut self, unit: u16) -> bool {
        if self.armed {
            self.units.push(unit);
        }
        self.armed
    }

    /// The composed character, once its last unit is in; disarms.
    fn finish(&mut self) -> Option<String> {
        let text = String::from_utf16(&self.units).ok().filter(|text| !text.is_empty());
        *self = AltCode::default();
        text
    }
}

enum PartialText {
    System(Vec<u16>),
    Text(Option<SmolStr>),
}

enum PartialLogicalKey {
    /// Use the text provided by the WM_CHAR messages and report that as a `Character` variant.
    TextOr(Key),

    /// Use the value directly provided by this variant.
    This(Key),
}

struct PartialKeyEventInfo {
    vkey: VIRTUAL_KEY,
    key_state: ElementState,
    is_repeat: bool,
    physical_key: PhysicalKey,
    location: KeyLocation,
    logical_key: PartialLogicalKey,

    key_without_modifiers: Key,

    /// The UTF-16 code units of the text that was produced by the keypress event.
    utf16parts: Vec<u16>,

    text: PartialText,
}

impl PartialKeyEventInfo {
    fn from_message(
        wparam: WPARAM,
        lparam: LPARAM,
        state: ElementState,
        layouts: &mut MutexGuard<'_, LayoutCache>,
    ) -> Self {
        const NO_MODS: WindowsModifiers = WindowsModifiers::empty();

        let (_, layout) = layouts.get_current_layout();
        let lparam_struct = destructure_key_lparam(lparam);
        let vkey = wparam as VIRTUAL_KEY;
        let scancode = if lparam_struct.scancode == 0 {
            unsafe {
                MapVirtualKeyExW(
                    vkey as u32,
                    MAPVK_VK_TO_VSC_EX,
                    mapping_layout(super::handle::from_usize(layout.hkl as usize)),
                ) as u16
            }
        } else {
            new_ex_scancode(lparam_struct.scancode, lparam_struct.extended)
        };
        let physical_key = scancode_to_physicalkey(scancode as u32);
        let location = get_location(scancode, super::handle::from_usize(layout.hkl as usize));

        let kbd_state = get_kbd_state();
        let mods = WindowsModifiers::active_modifiers(&kbd_state);
        let mods_without_ctrl = mods.remove_only_ctrl();
        let num_lock_on = kbd_state[VK_NUMLOCK as usize] & 1 != 0;

        let code_as_key = if mods.contains(WindowsModifiers::CONTROL) {
            match physical_key {
                PhysicalKey::Code(KeyCode::NumLock) => Some(Key::Named(NamedKey::NumLock)),
                PhysicalKey::Code(KeyCode::Pause) => Some(Key::Named(NamedKey::Pause)),
                _ => None,
            }
        } else {
            None
        };

        let preliminary_logical_key =
            layout.get_key(mods_without_ctrl, num_lock_on, vkey, &physical_key);
        let key_is_char = matches!(preliminary_logical_key, Key::Character(_));
        let is_pressed = state == ElementState::Pressed;

        let logical_key = if let Some(key) = code_as_key.clone() {
            PartialLogicalKey::This(key)
        } else if is_pressed && key_is_char && !mods.contains(WindowsModifiers::CONTROL) {
            // dead keys to have an effect on the character reported by `logical_key`.
            PartialLogicalKey::TextOr(preliminary_logical_key)
        } else {
            PartialLogicalKey::This(preliminary_logical_key)
        };
        let key_without_modifiers = if let Some(key) = code_as_key {
            key
        } else {
            match layout.get_key(NO_MODS, false, vkey, &physical_key) {
                Key::Dead(k) => {
                    if let Some(ch) = k {
                        let mut utf8 = [0; 4];
                        let s = ch.encode_utf8(&mut utf8);
                        Key::Character(SmolStr::new(s))
                    } else {
                        Key::Unidentified(NativeKey::Unidentified)
                    }
                },
                key => key,
            }
        };

        PartialKeyEventInfo {
            vkey,
            key_state: state,
            logical_key,
            key_without_modifiers,
            is_repeat: lparam_struct.is_repeat,
            physical_key,
            location,
            utf16parts: Vec::with_capacity(8),
            text: PartialText::System(Vec::new()),
        }
    }

    fn finalize(self) -> KeyEvent {
        let mut char_with_all_modifiers = None;
        if !self.utf16parts.is_empty() {
            let os_string = OsString::from_wide(&self.utf16parts);
            if let Ok(string) = os_string.into_string() {
                char_with_all_modifiers = Some(SmolStr::new(string));
            }
        }

        let mut text = None;
        match self.text {
            PartialText::System(wide) => {
                if !wide.is_empty() {
                    let os_string = OsString::from_wide(&wide);
                    if let Ok(string) = os_string.into_string() {
                        text = Some(SmolStr::new(string));
                    }
                }
            },
            PartialText::Text(s) => {
                text = s.map(SmolStr::new);
            },
        }

        let logical_key = match self.logical_key {
            PartialLogicalKey::TextOr(fallback) => match text.as_ref() {
                Some(s) => {
                    // records them.
                    if s.graphemes().count() > 1 {
                        fallback
                    } else {
                        Key::Character(s.clone())
                    }
                },
                None => Key::Unidentified(NativeKey::Windows(self.vkey)),
            },
            PartialLogicalKey::This(v) => v,
        };

        KeyEvent {
            physical_key: self.physical_key,
            logical_key,
            text,
            location: self.location,
            state: self.key_state,
            repeat: self.is_repeat,
            platform_specific: KeyEventExtra {
                text_with_all_modifiers: char_with_all_modifiers,
                key_without_modifiers: self.key_without_modifiers,
            },
        }
    }
}

#[derive(Debug, Copy, Clone)]
struct KeyLParam {
    pub scancode: u8,
    pub extended: bool,

    /// This is `previous_state XOR transition_state`.
    pub is_repeat: bool,
}

fn destructure_key_lparam(lparam: LPARAM) -> KeyLParam {
    let previous_state = (lparam >> 30) & 0x01;
    let transition_state = (lparam >> 31) & 0x01;
    KeyLParam {
        scancode: ((lparam >> 16) & 0xff) as u8,
        extended: ((lparam >> 24) & 0x01) != 0,
        is_repeat: (previous_state ^ transition_state) != 0,
    }
}

#[inline]
fn new_ex_scancode(scancode: u8, extended: bool) -> ExScancode {
    (scancode as u16) | (if extended { 0xe000 } else { 0 })
}

#[inline]
fn ex_scancode_from_lparam(lparam: LPARAM) -> ExScancode {
    let lparam = destructure_key_lparam(lparam);
    new_ex_scancode(lparam.scancode, lparam.extended)
}

/// Local PATCH (aterm): what the key path reads from Windows.
#[cfg(any(test, debug_assertions))]
pub(crate) mod replay {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use windows_sys::Win32::UI::WindowsAndMessaging::{MSG, WM_KEYFIRST, WM_KEYLAST};

    /// The replay's view of Windows while one message is handled.
    pub(crate) struct Replay {
        /// The messages still to come, the next first.
        pub(crate) queue: VecDeque<MSG>,
        /// `GetKeyboardState` as of the message being handled.
        pub(crate) keys: [u8; 256],
        /// What `GetKeyboardLayout(0)` answers.
        pub(crate) hkl: u64,
    }

    thread_local! {
        static REPLAY: RefCell<Option<Replay>> = const { RefCell::new(None) };
    }

    /// Install (or, with `None`, end) this thread's replay.
    pub(crate) fn set(replay: Option<Replay>) {
        REPLAY.with(|cell| *cell.borrow_mut() = replay);
    }

    /// `PeekMessageW(.., WM_KEYFIRST, WM_KEYLAST, PM_NOREMOVE)` while replaying.
    pub(crate) fn next_key_message() -> Option<Option<MSG>> {
        REPLAY.with(|cell| {
            cell.borrow().as_ref().map(|replay| {
                replay
                    .queue
                    .iter()
                    .find(|msg| (WM_KEYFIRST..=WM_KEYLAST).contains(&msg.message))
                    .copied()
            })
        })
    }

    /// `GetKeyboardState` while replaying.
    pub(crate) fn keys() -> Option<[u8; 256]> {
        REPLAY.with(|cell| cell.borrow().as_ref().map(|replay| replay.keys))
    }

    /// `GetKeyboardLayout(0)` while replaying.
    pub(crate) fn layout() -> Option<u64> {
        REPLAY.with(|cell| cell.borrow().as_ref().map(|replay| replay.hkl))
    }
}

/// Local PATCH (aterm): `GetKeyState(vk)`.
pub(crate) fn key_state(vk: VIRTUAL_KEY) -> i16 {
    #[cfg(any(test, debug_assertions))]
    if let Some(keys) = replay::keys() {
        let state = keys[vk as usize];
        return (if state & 0x80 != 0 { i16::MIN } else { 0 }) | i16::from(state & 1);
    }
    unsafe { GetKeyState(vk as i32) }
}

/// Local PATCH (aterm): the layout `MapVirtualKeyExW` maps a scan code through.
pub(crate) fn mapping_layout(hkl: HKL) -> HKL {
    #[cfg(any(test, debug_assertions))]
    if replay::layout().is_some() {
        return unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout(0) };
    }
    hkl
}

/// Gets the keyboard state as reported by messages that have been removed from the event queue.
fn get_kbd_state() -> [u8; 256] {
    #[cfg(any(test, debug_assertions))]
    if let Some(keys) = replay::keys() {
        return keys;
    }
    unsafe {
        let mut kbd_state: MaybeUninit<[u8; 256]> = MaybeUninit::uninit();
        GetKeyboardState(kbd_state.as_mut_ptr() as *mut u8);
        kbd_state.assume_init()
    }
}

/// Gets the current keyboard state regardless of whether the corresponding keyboard events have
/// been removed from the event queue.
#[allow(clippy::uninit_assumed_init)]
fn get_async_kbd_state() -> [u8; 256] {
    #[cfg(any(test, debug_assertions))]
    if let Some(keys) = replay::keys() {
        return keys;
    }
    unsafe {
        let mut kbd_state: [u8; 256] = [0; 256];
        for (vk, state) in kbd_state.iter_mut().enumerate() {
            let vk = vk as VIRTUAL_KEY;
            let async_state = GetAsyncKeyState(vk as i32);
            let is_down = (async_state & (1 << 15)) != 0;
            *state = if is_down { 0x80 } else { 0 };

            if matches!(vk, VK_CAPITAL | VK_NUMLOCK | VK_SCROLL) {
                let toggle_state = GetKeyState(vk as i32);
                let is_active = (toggle_state & 1) != 0;
                *state |= u8::from(is_active);
            }
        }
        kbd_state
    }
}

/// On windows, AltGr == Ctrl + Alt
fn is_current_fake(curr_info: &PartialKeyEventInfo, next_msg: MSG, layout: &Layout) -> bool {
    let curr_is_ctrl =
        matches!(curr_info.logical_key, PartialLogicalKey::This(Key::Named(NamedKey::Control)));
    if layout.has_alt_graph {
        let next_code = ex_scancode_from_lparam(next_msg.lParam);
        let next_is_altgr = next_code == 0xe038;
        if curr_is_ctrl && next_is_altgr {
            return true;
        }
    }
    false
}

enum PendingMessage<T> {
    Incomplete,
    Complete(T),
}
struct IdentifiedPendingMessage<T> {
    token: PendingMessageToken,
    msg: PendingMessage<T>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingMessageToken(u32);

/// While processing keyboard events, we sometimes need to call `PeekMessageW` (`next_msg`).
pub struct PendingEventQueue<T> {
    pending: Mutex<Vec<IdentifiedPendingMessage<T>>>,
    next_id: AtomicU32,
}
impl<T> PendingEventQueue<T> {
    /// Add a new pending event to the "pending queue".
    pub fn add_pending(&self) -> PendingMessageToken {
        let token = self.next_token();
        let mut pending = self.pending.lock().unwrap();
        pending.push(IdentifiedPendingMessage { token, msg: PendingMessage::Incomplete });
        token
    }

    /// Returns all finished pending events.
    pub fn complete_pending(&self, token: PendingMessageToken, msg: T) -> Vec<T> {
        let mut pending = self.pending.lock().unwrap();
        let mut target_is_first = false;
        for (i, pending_msg) in pending.iter_mut().enumerate() {
            if pending_msg.token == token {
                pending_msg.msg = PendingMessage::Complete(msg);
                if i == 0 {
                    target_is_first = true;
                }
                break;
            }
        }
        if target_is_first {
            // then we can empty the queue, and dispatch all of the messages.
            Self::drain_pending(&mut *pending)
        } else {
            Vec::new()
        }
    }

    pub fn complete_multi(&self, msgs: Vec<T>) -> Vec<T> {
        let mut pending = self.pending.lock().unwrap();
        if pending.is_empty() {
            return msgs;
        }
        pending.reserve(msgs.len());
        for msg in msgs {
            pending.push(IdentifiedPendingMessage {
                token: self.next_token(),
                msg: PendingMessage::Complete(msg),
            });
        }
        Vec::new()
    }

    /// Returns all finished pending events.
    pub fn remove_pending(&self, token: PendingMessageToken) -> Vec<T> {
        let mut pending = self.pending.lock().unwrap();
        let mut was_first = false;
        if let Some(m) = pending.first() {
            if m.token == token {
                was_first = true;
            }
        }
        pending.retain(|m| m.token != token);
        if was_first {
            Self::drain_pending(&mut *pending)
        } else {
            Vec::new()
        }
    }

    fn drain_pending(pending: &mut Vec<IdentifiedPendingMessage<T>>) -> Vec<T> {
        pending
            .drain(..)
            .map(|m| match m.msg {
                PendingMessage::Complete(msg) => msg,
                PendingMessage::Incomplete => {
                    panic!(
                        "Found an incomplete pending message when collecting messages. This \
                         indicates a bug in winit."
                    )
                },
            })
            .collect()
    }

    fn next_token(&self) -> PendingMessageToken {
        let id = self.next_id.fetch_add(1, Relaxed);
        PendingMessageToken(id)
    }
}
impl<T> Default for PendingEventQueue<T> {
    fn default() -> Self {
        PendingEventQueue { pending: Mutex::new(Vec::new()), next_id: AtomicU32::new(0) }
    }
}

/// Warning: Calls `PeekMessageW` internally.
pub fn next_kbd_msg(hwnd: HWND) -> Option<MSG> {
    #[cfg(any(test, debug_assertions))]
    if let Some(next) = replay::next_key_message() {
        return next;
    }
    unsafe {
        let mut next_msg = MaybeUninit::uninit();
        let peek_retval =
            PeekMessageW(next_msg.as_mut_ptr(), hwnd, WM_KEYFIRST, WM_KEYLAST, PM_NOREMOVE);
        (peek_retval != 0).then(|| next_msg.assume_init())
    }
}

fn get_location(scancode: ExScancode, hkl: HKL) -> KeyLocation {
    const ABNT_C2: VIRTUAL_KEY = VK_ABNT_C2 as VIRTUAL_KEY;

    let extension = 0xe000;
    let extended = (scancode & extension) == extension;
    let vkey = unsafe {
        MapVirtualKeyExW(scancode as u32, MAPVK_VSC_TO_VK_EX, mapping_layout(hkl)) as VIRTUAL_KEY
    };

    match vkey {
        VK_LSHIFT | VK_LCONTROL | VK_LMENU | VK_LWIN => KeyLocation::Left,
        VK_RSHIFT | VK_RCONTROL | VK_RMENU | VK_RWIN => KeyLocation::Right,
        VK_RETURN if extended => KeyLocation::Numpad,
        VK_INSERT | VK_DELETE | VK_END | VK_DOWN | VK_NEXT | VK_LEFT | VK_CLEAR | VK_RIGHT
        | VK_HOME | VK_UP | VK_PRIOR => {
            if extended {
                KeyLocation::Standard
            } else {
                KeyLocation::Numpad
            }
        },
        VK_NUMPAD0 | VK_NUMPAD1 | VK_NUMPAD2 | VK_NUMPAD3 | VK_NUMPAD4 | VK_NUMPAD5
        | VK_NUMPAD6 | VK_NUMPAD7 | VK_NUMPAD8 | VK_NUMPAD9 | VK_DECIMAL | VK_DIVIDE
        | VK_MULTIPLY | VK_SUBTRACT | VK_ADD | ABNT_C2 => KeyLocation::Numpad,
        _ => KeyLocation::Standard,
    }
}

pub(crate) fn scancode_to_physicalkey(scancode: u32) -> PhysicalKey {
    PhysicalKey::Code(match scancode {
        0x0029 => KeyCode::Backquote,
        0x002b => KeyCode::Backslash,
        0x000e => KeyCode::Backspace,
        0x001a => KeyCode::BracketLeft,
        0x001b => KeyCode::BracketRight,
        0x0033 => KeyCode::Comma,
        0x000b => KeyCode::Digit0,
        0x0002 => KeyCode::Digit1,
        0x0003 => KeyCode::Digit2,
        0x0004 => KeyCode::Digit3,
        0x0005 => KeyCode::Digit4,
        0x0006 => KeyCode::Digit5,
        0x0007 => KeyCode::Digit6,
        0x0008 => KeyCode::Digit7,
        0x0009 => KeyCode::Digit8,
        0x000a => KeyCode::Digit9,
        0x000d => KeyCode::Equal,
        0x0056 => KeyCode::IntlBackslash,
        0x0073 => KeyCode::IntlRo,
        0x007d => KeyCode::IntlYen,
        0x001e => KeyCode::KeyA,
        0x0030 => KeyCode::KeyB,
        0x002e => KeyCode::KeyC,
        0x0020 => KeyCode::KeyD,
        0x0012 => KeyCode::KeyE,
        0x0021 => KeyCode::KeyF,
        0x0022 => KeyCode::KeyG,
        0x0023 => KeyCode::KeyH,
        0x0017 => KeyCode::KeyI,
        0x0024 => KeyCode::KeyJ,
        0x0025 => KeyCode::KeyK,
        0x0026 => KeyCode::KeyL,
        0x0032 => KeyCode::KeyM,
        0x0031 => KeyCode::KeyN,
        0x0018 => KeyCode::KeyO,
        0x0019 => KeyCode::KeyP,
        0x0010 => KeyCode::KeyQ,
        0x0013 => KeyCode::KeyR,
        0x001f => KeyCode::KeyS,
        0x0014 => KeyCode::KeyT,
        0x0016 => KeyCode::KeyU,
        0x002f => KeyCode::KeyV,
        0x0011 => KeyCode::KeyW,
        0x002d => KeyCode::KeyX,
        0x0015 => KeyCode::KeyY,
        0x002c => KeyCode::KeyZ,
        0x000c => KeyCode::Minus,
        0x0034 => KeyCode::Period,
        0x0028 => KeyCode::Quote,
        0x0027 => KeyCode::Semicolon,
        0x0035 => KeyCode::Slash,
        0x0038 => KeyCode::AltLeft,
        0xe038 => KeyCode::AltRight,
        0x003a => KeyCode::CapsLock,
        0xe05d => KeyCode::ContextMenu,
        0x001d => KeyCode::ControlLeft,
        0xe01d => KeyCode::ControlRight,
        0x001c => KeyCode::Enter,
        0xe05b => KeyCode::SuperLeft,
        0xe05c => KeyCode::SuperRight,
        0x002a => KeyCode::ShiftLeft,
        0x0036 => KeyCode::ShiftRight,
        0x0039 => KeyCode::Space,
        0x000f => KeyCode::Tab,
        0x0079 => KeyCode::Convert,
        0x0072 => KeyCode::Lang1,
        0xe0f2 => KeyCode::Lang1,
        0x0071 => KeyCode::Lang2,
        0xe0f1 => KeyCode::Lang2,
        0x0070 => KeyCode::KanaMode,
        0x007b => KeyCode::NonConvert,
        0xe053 => KeyCode::Delete,
        0xe04f => KeyCode::End,
        0xe047 => KeyCode::Home,
        0xe052 => KeyCode::Insert,
        0xe051 => KeyCode::PageDown,
        0xe049 => KeyCode::PageUp,
        0xe050 => KeyCode::ArrowDown,
        0xe04b => KeyCode::ArrowLeft,
        0xe04d => KeyCode::ArrowRight,
        0xe048 => KeyCode::ArrowUp,
        0xe045 => KeyCode::NumLock,
        0x0052 => KeyCode::Numpad0,
        0x004f => KeyCode::Numpad1,
        0x0050 => KeyCode::Numpad2,
        0x0051 => KeyCode::Numpad3,
        0x004b => KeyCode::Numpad4,
        0x004c => KeyCode::Numpad5,
        0x004d => KeyCode::Numpad6,
        0x0047 => KeyCode::Numpad7,
        0x0048 => KeyCode::Numpad8,
        0x0049 => KeyCode::Numpad9,
        0x004e => KeyCode::NumpadAdd,
        0x007e => KeyCode::NumpadComma,
        0x0053 => KeyCode::NumpadDecimal,
        0xe035 => KeyCode::NumpadDivide,
        0xe01c => KeyCode::NumpadEnter,
        0x0059 => KeyCode::NumpadEqual,
        0x0037 => KeyCode::NumpadMultiply,
        0x004a => KeyCode::NumpadSubtract,
        0x0001 => KeyCode::Escape,
        0x003b => KeyCode::F1,
        0x003c => KeyCode::F2,
        0x003d => KeyCode::F3,
        0x003e => KeyCode::F4,
        0x003f => KeyCode::F5,
        0x0040 => KeyCode::F6,
        0x0041 => KeyCode::F7,
        0x0042 => KeyCode::F8,
        0x0043 => KeyCode::F9,
        0x0044 => KeyCode::F10,
        0x0057 => KeyCode::F11,
        0x0058 => KeyCode::F12,
        0x0064 => KeyCode::F13,
        0x0065 => KeyCode::F14,
        0x0066 => KeyCode::F15,
        0x0067 => KeyCode::F16,
        0x0068 => KeyCode::F17,
        0x0069 => KeyCode::F18,
        0x006a => KeyCode::F19,
        0x006b => KeyCode::F20,
        0x006c => KeyCode::F21,
        0x006d => KeyCode::F22,
        0x006e => KeyCode::F23,
        0x0076 => KeyCode::F24,
        0xe037 => KeyCode::PrintScreen,
        0x0054 => KeyCode::PrintScreen,
        0x0046 => KeyCode::ScrollLock,
        0x0045 => KeyCode::Pause,
        0xe046 => KeyCode::Pause,
        0xe06a => KeyCode::BrowserBack,
        0xe066 => KeyCode::BrowserFavorites,
        0xe069 => KeyCode::BrowserForward,
        0xe032 => KeyCode::BrowserHome,
        0xe067 => KeyCode::BrowserRefresh,
        0xe065 => KeyCode::BrowserSearch,
        0xe068 => KeyCode::BrowserStop,
        0xe06b => KeyCode::LaunchApp1,
        0xe021 => KeyCode::LaunchApp2,
        0xe06c => KeyCode::LaunchMail,
        0xe022 => KeyCode::MediaPlayPause,
        0xe06d => KeyCode::MediaSelect,
        0xe024 => KeyCode::MediaStop,
        0xe019 => KeyCode::MediaTrackNext,
        0xe010 => KeyCode::MediaTrackPrevious,
        0xe05e => KeyCode::Power,
        0xe02e => KeyCode::AudioVolumeDown,
        0xe020 => KeyCode::AudioVolumeMute,
        0xe030 => KeyCode::AudioVolumeUp,
        _ => return PhysicalKey::Unidentified(NativeKeyCode::Windows(scancode as u16)),
    })
}

#[cfg(test)]
mod alt_code_tests {
    use super::AltCode;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_LMENU, VK_MENU, VK_NUMPAD0, VK_NUMPAD2, VK_NUMPAD3, VK_RETURN, VK_SHIFT,
    };

    /// Alt, 0, 2, 3, 3 on the keypad, then Alt's release posts `é` as one WM_CHAR: the
    /// code takes it and hands it over whole, once.
    #[test]
    fn an_alt_code_hands_over_its_character() {
        let mut code = AltCode::default();
        code.key_down(VK_MENU, true, false);
        for digit in [VK_NUMPAD0, VK_NUMPAD2, VK_NUMPAD3, VK_NUMPAD3] {
            code.key_down(digit, true, false);
        }
        assert!(code.push(0x00e9));
        assert_eq!(code.finish().as_deref(), Some("é"));
        assert!(!code.push(0x00e9), "one character per code");
    }

    /// A surrogate pair arrives as two WM_CHARs and leaves as one character.
    #[test]
    fn a_surrogate_pair_is_one_character() {
        let mut code = AltCode::default();
        code.key_down(VK_NUMPAD2, true, false);
        assert!(code.push(0xd83d));
        assert!(code.push(0xde00));
        assert_eq!(code.finish().as_deref(), Some("😀"));
    }

    /// Nothing arms without Alt, Ctrl+Alt (AltGr) does not, and a key that is not a
    /// modifier disarms: those WM_CHARs stay upstream's (an IME's, or nobody's).
    #[test]
    fn only_alt_and_a_keypad_digit_arm_it() {
        let mut code = AltCode::default();
        code.key_down(VK_NUMPAD2, false, false);
        assert!(!code.push(u16::from(b'2')));
        code.key_down(VK_NUMPAD2, true, true);
        assert!(!code.push(u16::from(b'2')), "Ctrl+Alt is AltGr");
        code.key_down(VK_NUMPAD2, true, false);
        code.key_down(VK_SHIFT, true, false);
        code.key_down(VK_LMENU, true, false);
        assert!(code.push(0x00e9), "modifiers leave it armed");
        let _ = code.finish();
        code.key_down(VK_NUMPAD2, true, false);
        code.key_down(VK_RETURN, true, false);
        assert!(!code.push(u16::from(b'\r')), "another key disarms it");
    }
}
