// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/common/xkb/keymap.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! XKB keymap compilation and keycode/keysym translation.

use std::ffi::CStr;
use std::ops::Deref;
use std::ptr::{self, NonNull};

#[cfg(x11_platform)]
use aterm_xkb::XcbConnection as xcb_connection_t;

use aterm_xkb::{
    self as xkb, mod_name, Keycode as xkb_keycode_t, Keysym as xkb_keysym_t,
    LayoutIndex as xkb_layout_index_t, ModIndex as xkb_mod_index_t, XkbKeymap as xkb_keymap,
};

use crate::keyboard::{Key, KeyCode, KeyLocation, NamedKey, NativeKey, NativeKeyCode, PhysicalKey};
#[cfg(x11_platform)]
use crate::platform_impl::common::xkb::xkbxh;
use crate::platform_impl::common::xkb::{xkbh, XkbContext};

/// Map the raw X11-style keycode to the `KeyCode` enum.
pub fn raw_keycode_to_physicalkey(keycode: u32) -> PhysicalKey {
    scancode_to_physicalkey(keycode.saturating_sub(8))
}

/// Map the linux scancode to Keycode.
pub fn scancode_to_physicalkey(scancode: u32) -> PhysicalKey {
    PhysicalKey::Code(match scancode {
        0 => return PhysicalKey::Unidentified(NativeKeyCode::Xkb(0)),
        1 => KeyCode::Escape,
        2 => KeyCode::Digit1,
        3 => KeyCode::Digit2,
        4 => KeyCode::Digit3,
        5 => KeyCode::Digit4,
        6 => KeyCode::Digit5,
        7 => KeyCode::Digit6,
        8 => KeyCode::Digit7,
        9 => KeyCode::Digit8,
        10 => KeyCode::Digit9,
        11 => KeyCode::Digit0,
        12 => KeyCode::Minus,
        13 => KeyCode::Equal,
        14 => KeyCode::Backspace,
        15 => KeyCode::Tab,
        16 => KeyCode::KeyQ,
        17 => KeyCode::KeyW,
        18 => KeyCode::KeyE,
        19 => KeyCode::KeyR,
        20 => KeyCode::KeyT,
        21 => KeyCode::KeyY,
        22 => KeyCode::KeyU,
        23 => KeyCode::KeyI,
        24 => KeyCode::KeyO,
        25 => KeyCode::KeyP,
        26 => KeyCode::BracketLeft,
        27 => KeyCode::BracketRight,
        28 => KeyCode::Enter,
        29 => KeyCode::ControlLeft,
        30 => KeyCode::KeyA,
        31 => KeyCode::KeyS,
        32 => KeyCode::KeyD,
        33 => KeyCode::KeyF,
        34 => KeyCode::KeyG,
        35 => KeyCode::KeyH,
        36 => KeyCode::KeyJ,
        37 => KeyCode::KeyK,
        38 => KeyCode::KeyL,
        39 => KeyCode::Semicolon,
        40 => KeyCode::Quote,
        41 => KeyCode::Backquote,
        42 => KeyCode::ShiftLeft,
        43 => KeyCode::Backslash,
        44 => KeyCode::KeyZ,
        45 => KeyCode::KeyX,
        46 => KeyCode::KeyC,
        47 => KeyCode::KeyV,
        48 => KeyCode::KeyB,
        49 => KeyCode::KeyN,
        50 => KeyCode::KeyM,
        51 => KeyCode::Comma,
        52 => KeyCode::Period,
        53 => KeyCode::Slash,
        54 => KeyCode::ShiftRight,
        55 => KeyCode::NumpadMultiply,
        56 => KeyCode::AltLeft,
        57 => KeyCode::Space,
        58 => KeyCode::CapsLock,
        59 => KeyCode::F1,
        60 => KeyCode::F2,
        61 => KeyCode::F3,
        62 => KeyCode::F4,
        63 => KeyCode::F5,
        64 => KeyCode::F6,
        65 => KeyCode::F7,
        66 => KeyCode::F8,
        67 => KeyCode::F9,
        68 => KeyCode::F10,
        69 => KeyCode::NumLock,
        70 => KeyCode::ScrollLock,
        71 => KeyCode::Numpad7,
        72 => KeyCode::Numpad8,
        73 => KeyCode::Numpad9,
        74 => KeyCode::NumpadSubtract,
        75 => KeyCode::Numpad4,
        76 => KeyCode::Numpad5,
        77 => KeyCode::Numpad6,
        78 => KeyCode::NumpadAdd,
        79 => KeyCode::Numpad1,
        80 => KeyCode::Numpad2,
        81 => KeyCode::Numpad3,
        82 => KeyCode::Numpad0,
        83 => KeyCode::NumpadDecimal,
        85 => KeyCode::Lang5,
        86 => KeyCode::IntlBackslash,
        87 => KeyCode::F11,
        88 => KeyCode::F12,
        89 => KeyCode::IntlRo,
        90 => KeyCode::Lang3,
        91 => KeyCode::Lang4,
        92 => KeyCode::Convert,
        93 => KeyCode::KanaMode,
        94 => KeyCode::NonConvert,
        96 => KeyCode::NumpadEnter,
        97 => KeyCode::ControlRight,
        98 => KeyCode::NumpadDivide,
        99 => KeyCode::PrintScreen,
        100 => KeyCode::AltRight,
        102 => KeyCode::Home,
        103 => KeyCode::ArrowUp,
        104 => KeyCode::PageUp,
        105 => KeyCode::ArrowLeft,
        106 => KeyCode::ArrowRight,
        107 => KeyCode::End,
        108 => KeyCode::ArrowDown,
        109 => KeyCode::PageDown,
        110 => KeyCode::Insert,
        111 => KeyCode::Delete,
        113 => KeyCode::AudioVolumeMute,
        114 => KeyCode::AudioVolumeDown,
        115 => KeyCode::AudioVolumeUp,
        117 => KeyCode::NumpadEqual,
        119 => KeyCode::Pause,
        121 => KeyCode::NumpadComma,
        122 => KeyCode::Lang1,
        123 => KeyCode::Lang2,
        124 => KeyCode::IntlYen,
        125 => KeyCode::SuperLeft,
        126 => KeyCode::SuperRight,
        127 => KeyCode::ContextMenu,
        163 => KeyCode::MediaTrackNext,
        164 => KeyCode::MediaPlayPause,
        165 => KeyCode::MediaTrackPrevious,
        166 => KeyCode::MediaStop,
        183 => KeyCode::F13,
        184 => KeyCode::F14,
        185 => KeyCode::F15,
        186 => KeyCode::F16,
        187 => KeyCode::F17,
        188 => KeyCode::F18,
        189 => KeyCode::F19,
        190 => KeyCode::F20,
        191 => KeyCode::F21,
        192 => KeyCode::F22,
        193 => KeyCode::F23,
        194 => KeyCode::F24,
        240 => return PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
        _ => return PhysicalKey::Unidentified(NativeKeyCode::Xkb(scancode)),
    })
}

pub fn keysym_to_key(keysym: u32) -> Key {
    use aterm_xkb::keysym as keysyms;
    Key::Named(match keysym {
        keysyms::BackSpace => NamedKey::Backspace,
        keysyms::Tab => NamedKey::Tab,
        keysyms::Clear => NamedKey::Clear,
        keysyms::Return => NamedKey::Enter,
        keysyms::Pause => NamedKey::Pause,
        keysyms::Scroll_Lock => NamedKey::ScrollLock,
        keysyms::Sys_Req => NamedKey::PrintScreen,
        keysyms::Escape => NamedKey::Escape,
        keysyms::Delete => NamedKey::Delete,

        keysyms::Multi_key => NamedKey::Compose,
        keysyms::Codeinput => NamedKey::CodeInput,
        keysyms::SingleCandidate => NamedKey::SingleCandidate,
        keysyms::MultipleCandidate => NamedKey::AllCandidates,
        keysyms::PreviousCandidate => NamedKey::PreviousCandidate,

        keysyms::Kanji => NamedKey::KanjiMode,
        keysyms::Muhenkan => NamedKey::NonConvert,
        keysyms::Henkan_Mode => NamedKey::Convert,
        keysyms::Romaji => NamedKey::Romaji,
        keysyms::Hiragana => NamedKey::Hiragana,
        keysyms::Hiragana_Katakana => NamedKey::HiraganaKatakana,
        keysyms::Zenkaku => NamedKey::Zenkaku,
        keysyms::Hankaku => NamedKey::Hankaku,
        keysyms::Zenkaku_Hankaku => NamedKey::ZenkakuHankaku,
        keysyms::Kana_Lock => NamedKey::KanaMode,
        keysyms::Kana_Shift => NamedKey::KanaMode,
        keysyms::Eisu_Shift => NamedKey::Alphanumeric,
        keysyms::Eisu_toggle => NamedKey::Alphanumeric,

        keysyms::Home => NamedKey::Home,
        keysyms::Left => NamedKey::ArrowLeft,
        keysyms::Up => NamedKey::ArrowUp,
        keysyms::Right => NamedKey::ArrowRight,
        keysyms::Down => NamedKey::ArrowDown,
        keysyms::Page_Up => NamedKey::PageUp,
        keysyms::Page_Down => NamedKey::PageDown,
        keysyms::End => NamedKey::End,

        keysyms::Select => NamedKey::Select,
        keysyms::Print => NamedKey::PrintScreen,
        keysyms::Execute => NamedKey::Execute,
        keysyms::Insert => NamedKey::Insert,
        keysyms::Undo => NamedKey::Undo,
        keysyms::Redo => NamedKey::Redo,
        keysyms::Menu => NamedKey::ContextMenu,
        keysyms::Find => NamedKey::Find,
        keysyms::Cancel => NamedKey::Cancel,
        keysyms::Help => NamedKey::Help,
        keysyms::Break => NamedKey::Pause,
        keysyms::Mode_switch => NamedKey::ModeChange,
        keysyms::Num_Lock => NamedKey::NumLock,

        keysyms::KP_Tab => NamedKey::Tab,
        keysyms::KP_Enter => NamedKey::Enter,
        keysyms::KP_F1 => NamedKey::F1,
        keysyms::KP_F2 => NamedKey::F2,
        keysyms::KP_F3 => NamedKey::F3,
        keysyms::KP_F4 => NamedKey::F4,
        keysyms::KP_Home => NamedKey::Home,
        keysyms::KP_Left => NamedKey::ArrowLeft,
        keysyms::KP_Up => NamedKey::ArrowUp,
        keysyms::KP_Right => NamedKey::ArrowRight,
        keysyms::KP_Down => NamedKey::ArrowDown,
        keysyms::KP_Page_Up => NamedKey::PageUp,
        keysyms::KP_Page_Down => NamedKey::PageDown,
        keysyms::KP_End => NamedKey::End,
        keysyms::KP_Insert => NamedKey::Insert,
        keysyms::KP_Delete => NamedKey::Delete,

        keysyms::F1 => NamedKey::F1,
        keysyms::F2 => NamedKey::F2,
        keysyms::F3 => NamedKey::F3,
        keysyms::F4 => NamedKey::F4,
        keysyms::F5 => NamedKey::F5,
        keysyms::F6 => NamedKey::F6,
        keysyms::F7 => NamedKey::F7,
        keysyms::F8 => NamedKey::F8,
        keysyms::F9 => NamedKey::F9,
        keysyms::F10 => NamedKey::F10,
        keysyms::F11 => NamedKey::F11,
        keysyms::F12 => NamedKey::F12,
        keysyms::F13 => NamedKey::F13,
        keysyms::F14 => NamedKey::F14,
        keysyms::F15 => NamedKey::F15,
        keysyms::F16 => NamedKey::F16,
        keysyms::F17 => NamedKey::F17,
        keysyms::F18 => NamedKey::F18,
        keysyms::F19 => NamedKey::F19,
        keysyms::F20 => NamedKey::F20,
        keysyms::F21 => NamedKey::F21,
        keysyms::F22 => NamedKey::F22,
        keysyms::F23 => NamedKey::F23,
        keysyms::F24 => NamedKey::F24,
        keysyms::F25 => NamedKey::F25,
        keysyms::F26 => NamedKey::F26,
        keysyms::F27 => NamedKey::F27,
        keysyms::F28 => NamedKey::F28,
        keysyms::F29 => NamedKey::F29,
        keysyms::F30 => NamedKey::F30,
        keysyms::F31 => NamedKey::F31,
        keysyms::F32 => NamedKey::F32,
        keysyms::F33 => NamedKey::F33,
        keysyms::F34 => NamedKey::F34,
        keysyms::F35 => NamedKey::F35,

        keysyms::Shift_L => NamedKey::Shift,
        keysyms::Shift_R => NamedKey::Shift,
        keysyms::Control_L => NamedKey::Control,
        keysyms::Control_R => NamedKey::Control,
        keysyms::Caps_Lock => NamedKey::CapsLock,

        keysyms::Alt_L => NamedKey::Alt,
        keysyms::Alt_R => NamedKey::Alt,
        keysyms::Super_L => NamedKey::Super,
        keysyms::Super_R => NamedKey::Super,
        keysyms::Hyper_L => NamedKey::Hyper,
        keysyms::Hyper_R => NamedKey::Hyper,

        keysyms::ISO_Level3_Shift => NamedKey::AltGraph,
        keysyms::ISO_Level3_Latch => NamedKey::AltGraph,
        keysyms::ISO_Level3_Lock => NamedKey::AltGraph,
        keysyms::ISO_Next_Group => NamedKey::GroupNext,
        keysyms::ISO_Prev_Group => NamedKey::GroupPrevious,
        keysyms::ISO_First_Group => NamedKey::GroupFirst,
        keysyms::ISO_Last_Group => NamedKey::GroupLast,
        keysyms::ISO_Left_Tab => NamedKey::Tab,
        keysyms::ISO_Enter => NamedKey::Enter,

        keysyms::_3270_EraseEOF => NamedKey::EraseEof,
        keysyms::_3270_Attn => NamedKey::Attn,
        keysyms::_3270_Play => NamedKey::Play,
        keysyms::_3270_ExSelect => NamedKey::ExSel,
        keysyms::_3270_CursorSelect => NamedKey::CrSel,
        keysyms::_3270_PrintScreen => NamedKey::PrintScreen,
        keysyms::_3270_Enter => NamedKey::Enter,

        keysyms::space => NamedKey::Space,

        keysyms::XF86_MonBrightnessUp => NamedKey::BrightnessUp,
        keysyms::XF86_MonBrightnessDown => NamedKey::BrightnessDown,

        keysyms::XF86_Standby => NamedKey::Standby,
        keysyms::XF86_AudioLowerVolume => NamedKey::AudioVolumeDown,
        keysyms::XF86_AudioRaiseVolume => NamedKey::AudioVolumeUp,
        keysyms::XF86_AudioPlay => NamedKey::MediaPlay,
        keysyms::XF86_AudioStop => NamedKey::MediaStop,
        keysyms::XF86_AudioPrev => NamedKey::MediaTrackPrevious,
        keysyms::XF86_AudioNext => NamedKey::MediaTrackNext,
        keysyms::XF86_HomePage => NamedKey::BrowserHome,
        keysyms::XF86_Mail => NamedKey::LaunchMail,
        keysyms::XF86_Search => NamedKey::BrowserSearch,
        keysyms::XF86_AudioRecord => NamedKey::MediaRecord,

        keysyms::XF86_Calculator => NamedKey::LaunchApplication2,
        keysyms::XF86_Calendar => NamedKey::LaunchCalendar,
        keysyms::XF86_PowerDown => NamedKey::Power,

        keysyms::XF86_Back => NamedKey::BrowserBack,
        keysyms::XF86_Forward => NamedKey::BrowserForward,
        keysyms::XF86_Refresh => NamedKey::BrowserRefresh,
        keysyms::XF86_PowerOff => NamedKey::Power,
        keysyms::XF86_WakeUp => NamedKey::WakeUp,
        keysyms::XF86_Eject => NamedKey::Eject,
        keysyms::XF86_ScreenSaver => NamedKey::LaunchScreenSaver,
        keysyms::XF86_WWW => NamedKey::LaunchWebBrowser,
        keysyms::XF86_Sleep => NamedKey::Standby,
        keysyms::XF86_Favorites => NamedKey::BrowserFavorites,
        keysyms::XF86_AudioPause => NamedKey::MediaPause,
        keysyms::XF86_MyComputer => NamedKey::LaunchApplication1,
        keysyms::XF86_AudioRewind => NamedKey::MediaRewind,

        keysyms::XF86_Calculater => NamedKey::LaunchApplication2, // Nice typo, libxkbcommon :)
        keysyms::XF86_Close => NamedKey::Close,
        keysyms::XF86_Copy => NamedKey::Copy,
        keysyms::XF86_Cut => NamedKey::Cut,
        keysyms::XF86_Excel => NamedKey::LaunchSpreadsheet,
        keysyms::XF86_LogOff => NamedKey::LogOff,
        keysyms::XF86_MySites => NamedKey::BrowserFavorites,
        keysyms::XF86_New => NamedKey::New,
        keysyms::XF86_Open => NamedKey::Open,
        keysyms::XF86_Paste => NamedKey::Paste,
        keysyms::XF86_Phone => NamedKey::LaunchPhone,
        keysyms::XF86_Reply => NamedKey::MailReply,
        keysyms::XF86_Reload => NamedKey::BrowserRefresh,
        keysyms::XF86_Save => NamedKey::Save,
        keysyms::XF86_Send => NamedKey::MailSend,
        keysyms::XF86_Spell => NamedKey::SpellCheck,
        keysyms::XF86_SplitScreen => NamedKey::SplitScreenToggle,
        keysyms::XF86_Video => NamedKey::LaunchMediaPlayer,
        keysyms::XF86_Word => NamedKey::LaunchWordProcessor,
        keysyms::XF86_ZoomIn => NamedKey::ZoomIn,
        keysyms::XF86_ZoomOut => NamedKey::ZoomOut,

        keysyms::XF86_WebCam => NamedKey::LaunchWebCam,
        keysyms::XF86_MailForward => NamedKey::MailForward,
        keysyms::XF86_Music => NamedKey::LaunchMusicPlayer,

        keysyms::XF86_AudioForward => NamedKey::MediaFastForward,
        keysyms::XF86_AudioRandomPlay => NamedKey::RandomToggle,
        keysyms::XF86_Subtitle => NamedKey::Subtitle,
        keysyms::XF86_AudioCycleTrack => NamedKey::MediaAudioTrack,
        keysyms::XF86_Suspend => NamedKey::Standby,
        keysyms::XF86_Hibernate => NamedKey::Hibernate,
        keysyms::XF86_AudioMute => NamedKey::AudioVolumeMute,

        keysyms::XF86_Next_VMode => NamedKey::VideoModeNext,

        keysyms::SUN_Copy => NamedKey::Copy,
        keysyms::SUN_Open => NamedKey::Open,
        keysyms::SUN_Paste => NamedKey::Paste,
        keysyms::SUN_Cut => NamedKey::Cut,

        keysyms::SUN_AudioLowerVolume => NamedKey::AudioVolumeDown,
        keysyms::SUN_AudioMute => NamedKey::AudioVolumeMute,
        keysyms::SUN_AudioRaiseVolume => NamedKey::AudioVolumeUp,
        keysyms::SUN_VideoLowerBrightness => NamedKey::BrightnessDown,
        keysyms::SUN_VideoRaiseBrightness => NamedKey::BrightnessUp,
        0 => return Key::Unidentified(NativeKey::Unidentified),
        _ => return Key::Unidentified(NativeKey::Xkb(keysym)),
    })
}

pub fn keysym_location(keysym: u32) -> KeyLocation {
    use aterm_xkb::keysym as keysyms;
    match keysym {
        keysyms::Shift_L
        | keysyms::Control_L
        | keysyms::Meta_L
        | keysyms::Alt_L
        | keysyms::Super_L
        | keysyms::Hyper_L => KeyLocation::Left,
        keysyms::Shift_R
        | keysyms::Control_R
        | keysyms::Meta_R
        | keysyms::Alt_R
        | keysyms::Super_R
        | keysyms::Hyper_R => KeyLocation::Right,
        keysyms::KP_0
        | keysyms::KP_1
        | keysyms::KP_2
        | keysyms::KP_3
        | keysyms::KP_4
        | keysyms::KP_5
        | keysyms::KP_6
        | keysyms::KP_7
        | keysyms::KP_8
        | keysyms::KP_9
        | keysyms::KP_Space
        | keysyms::KP_Tab
        | keysyms::KP_Enter
        | keysyms::KP_F1
        | keysyms::KP_F2
        | keysyms::KP_F3
        | keysyms::KP_F4
        | keysyms::KP_Home
        | keysyms::KP_Left
        | keysyms::KP_Up
        | keysyms::KP_Right
        | keysyms::KP_Down
        | keysyms::KP_Page_Up
        | keysyms::KP_Page_Down
        | keysyms::KP_End
        | keysyms::KP_Begin
        | keysyms::KP_Insert
        | keysyms::KP_Delete
        | keysyms::KP_Equal
        | keysyms::KP_Multiply
        | keysyms::KP_Add
        | keysyms::KP_Separator
        | keysyms::KP_Subtract
        | keysyms::KP_Decimal
        | keysyms::KP_Divide => KeyLocation::Numpad,
        _ => KeyLocation::Standard,
    }
}

#[derive(Debug)]
pub struct XkbKeymap {
    keymap: NonNull<xkb_keymap>,
    _mods_indices: ModsIndices,
    pub _core_keyboard_id: i32,
}

impl XkbKeymap {
    /// Compile an XKB v1 text keymap received from the compositor.
    #[cfg(wayland_platform)]
    pub fn from_text(context: &XkbContext, text: &[u8]) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        // SAFETY: A live context, and `text.len()` readable bytes.
        let keymap = unsafe {
            let keymap = (xkbh().keymap_new_from_buffer)(
                (*context).as_ptr(),
                text.as_ptr().cast(),
                text.len(),
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            );
            NonNull::new(keymap)?
        };

        Some(Self::new_inner(keymap, 0))
    }

    #[cfg(x11_platform)]
    pub fn from_x11_keymap(
        context: &XkbContext,
        xcb: *mut xcb_connection_t,
        core_keyboard_id: i32,
    ) -> Option<Self> {
        let keymap = unsafe {
            (xkbxh().keymap_new_from_device)(
                context.as_ptr(),
                xcb,
                core_keyboard_id,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
        };
        let keymap = NonNull::new(keymap)?;
        Some(Self::new_inner(keymap, core_keyboard_id))
    }

    fn new_inner(keymap: NonNull<xkb_keymap>, _core_keyboard_id: i32) -> Self {
        let mods_indices = ModsIndices {
            shift: mod_index_for_name(keymap, mod_name::SHIFT),
            caps: mod_index_for_name(keymap, mod_name::CAPS),
            ctrl: mod_index_for_name(keymap, mod_name::CTRL),
            alt: mod_index_for_name(keymap, mod_name::ALT),
            num: mod_index_for_name(keymap, mod_name::NUM),
            mod3: mod_index_for_name(keymap, c"Mod3"),
            logo: mod_index_for_name(keymap, mod_name::LOGO),
            mod5: mod_index_for_name(keymap, c"Mod5"),
        };

        Self { keymap, _mods_indices: mods_indices, _core_keyboard_id }
    }

    pub fn mods_indices(&self) -> ModsIndices {
        self._mods_indices
    }

    /// A keymap a test compiled itself (from rule names).
    #[cfg(test)]
    pub fn from_raw_for_test(keymap: NonNull<xkb_keymap>) -> Self {
        Self::new_inner(keymap, 0)
    }

    pub fn first_keysym_by_level(
        &mut self,
        layout: xkb_layout_index_t,
        keycode: xkb_keycode_t,
    ) -> xkb_keysym_t {
        unsafe {
            let mut keysyms = ptr::null();
            let count = (xkbh().keymap_key_get_syms_by_level)(
                self.keymap.as_ptr(),
                keycode,
                layout,
                0,
                &mut keysyms,
            );

            if count == 1 {
                *keysyms
            } else {
                0
            }
        }
    }

    /// Check whether the given key repeats.
    pub fn key_repeats(&mut self, keycode: xkb_keycode_t) -> bool {
        unsafe { (xkbh().keymap_key_repeats)(self.keymap.as_ptr(), keycode) == 1 }
    }
}

impl Drop for XkbKeymap {
    fn drop(&mut self) {
        unsafe {
            (xkbh().keymap_unref)(self.keymap.as_ptr());
        };
    }
}

impl Deref for XkbKeymap {
    type Target = NonNull<xkb_keymap>;

    fn deref(&self) -> &Self::Target {
        &self.keymap
    }
}

/// Modifier index in the keymap.
#[cfg_attr(not(x11_platform), allow(dead_code))]
#[derive(Default, Debug, Clone, Copy)]
pub struct ModsIndices {
    pub shift: Option<xkb_mod_index_t>,
    pub caps: Option<xkb_mod_index_t>,
    pub ctrl: Option<xkb_mod_index_t>,
    pub alt: Option<xkb_mod_index_t>,
    pub num: Option<xkb_mod_index_t>,
    pub mod3: Option<xkb_mod_index_t>,
    pub logo: Option<xkb_mod_index_t>,
    pub mod5: Option<xkb_mod_index_t>,
}

fn mod_index_for_name(keymap: NonNull<xkb_keymap>, name: &CStr) -> Option<xkb_mod_index_t> {
    unsafe {
        let mod_index = (xkbh().keymap_mod_get_index)(keymap.as_ptr(), name.as_ptr());
        if mod_index == xkb::INVALID {
            None
        } else {
            Some(mod_index)
        }
    }
}
