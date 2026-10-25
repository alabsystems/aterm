// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
// Derived from winit 0.30.13 src/keyboard.rs, modified by the aterm project in 2026 (see the repository NOTICE).

//! Keyboard key and modifier types.

// Variant names in `Key` and `KeyCode`, and their documentation, are derived from the W3C UI Events KeyboardEvent `key` (https://www.w3.org/TR/2017/CR-uievents-key-20170601/) and `code` (https://www.w3.org/TR/2017/CR-uievents-code-20170601/) specifications, Copyright © 2017 W3C® (MIT, ERCIM, Keio, Beihang), under the license below.

// Beginning of W3C License.
//
// License
//
// By obtaining and/or copying this work, you (the licensee) agree that you have read, understood,
// and will comply with the following terms and conditions.
//
// Permission to copy, modify, and distribute this work, with or without modification, for any
// purpose and without fee or royalty is hereby granted, provided that you include the following on
// ALL copies of the work or portions thereof, including modifications:
//
// - The full text of this NOTICE in a location viewable to users of the redistributed or derivative
//   work.
// - Any pre-existing intellectual property disclaimers, notices, or terms and conditions. If none
//   exist, the W3C Software and Document Short Notice should be included.
// - Notice of any changes or modifications, through a copyright statement on the new code or
//   document such as "This software or document includes material copied from or derived from
//   [title and URI of the W3C document]. Copyright © [YEAR] W3C® (MIT, ERCIM, Keio, Beihang)."
//
// Disclaimers
//
// THIS WORK IS PROVIDED "AS IS," AND COPYRIGHT HOLDERS MAKE NO REPRESENTATIONS OR WARRANTIES,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO, WARRANTIES OF MERCHANTABILITY OR FITNESS FOR
// ANY PARTICULAR PURPOSE OR THAT THE USE OF THE SOFTWARE OR DOCUMENT WILL NOT INFRINGE ANY THIRD
// PARTY PATENTS, COPYRIGHTS, TRADEMARKS OR OTHER RIGHTS.
//
// COPYRIGHT HOLDERS WILL NOT BE LIABLE FOR ANY DIRECT, INDIRECT, SPECIAL OR CONSEQUENTIAL DAMAGES
// ARISING OUT OF ANY USE OF THE SOFTWARE OR DOCUMENT.
//
// The name and trademarks of copyright holders may NOT be used in advertising or publicity
// pertaining to the work without specific, written prior permission. Title to copyright in this
// work will at all times remain with copyright holders.
//
// End of W3C License.

// Beginning of W3C Short Notice.
//
// winit: https://github.com/rust-windowing/winit
//
// Copyright © 2021 World Wide Web Consortium, (Massachusetts Institute of Technology, European
// Research Consortium for Informatics and Mathematics, Keio University, Beihang). All Rights
// Reserved. This work is distributed under the W3C® Software License [1] in the hope that it will
// be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
// FITNESS FOR A PARTICULAR PURPOSE.
//
// [1] http://www.w3.org/Consortium/Legal/copyright-software
//
// End of W3C Short Notice.

// LOCAL PATCH (aterm): the crate's own key-text type, not a re-export of the `smol_str` crate.
mod smol_str;
pub use self::smol_str::SmolStr;

/// Platform-native physical key identifier (scancode).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NativeKeyCode {
    Unidentified,
    /// An Android scancode.
    Android(u32),
    /// A macOS scancode.
    MacOS(u16),
    /// A Windows scancode.
    Windows(u16),
    /// An XKB keycode.
    Xkb(u32),
}

impl std::fmt::Debug for NativeKeyCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use NativeKeyCode::{Android, MacOS, Unidentified, Windows, Xkb};
        let mut debug_tuple;
        match self {
            Unidentified => {
                debug_tuple = f.debug_tuple("Unidentified");
            },
            Android(code) => {
                debug_tuple = f.debug_tuple("Android");
                debug_tuple.field(&format_args!("0x{code:04X}"));
            },
            MacOS(code) => {
                debug_tuple = f.debug_tuple("MacOS");
                debug_tuple.field(&format_args!("0x{code:04X}"));
            },
            Windows(code) => {
                debug_tuple = f.debug_tuple("Windows");
                debug_tuple.field(&format_args!("0x{code:04X}"));
            },
            Xkb(code) => {
                debug_tuple = f.debug_tuple("Xkb");
                debug_tuple.field(&format_args!("0x{code:04X}"));
            },
        }
        debug_tuple.finish()
    }
}

/// Platform-native logical key identifier (keysym or virtual-key code).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NativeKey {
    Unidentified,
    /// An Android keycode.
    Android(u32),
    /// A macOS scancode.
    MacOS(u16),
    /// A Windows virtual-key code.
    Windows(u16),
    /// An XKB keysym.
    Xkb(u32),
    /// A web key value string.
    Web(SmolStr),
}

impl std::fmt::Debug for NativeKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use NativeKey::{Android, MacOS, Unidentified, Web, Windows, Xkb};
        let mut debug_tuple;
        match self {
            Unidentified => {
                debug_tuple = f.debug_tuple("Unidentified");
            },
            Android(code) => {
                debug_tuple = f.debug_tuple("Android");
                debug_tuple.field(&format_args!("0x{code:04X}"));
            },
            MacOS(code) => {
                debug_tuple = f.debug_tuple("MacOS");
                debug_tuple.field(&format_args!("0x{code:04X}"));
            },
            Windows(code) => {
                debug_tuple = f.debug_tuple("Windows");
                debug_tuple.field(&format_args!("0x{code:04X}"));
            },
            Xkb(code) => {
                debug_tuple = f.debug_tuple("Xkb");
                debug_tuple.field(&format_args!("0x{code:04X}"));
            },
            Web(code) => {
                debug_tuple = f.debug_tuple("Web");
                debug_tuple.field(code);
            },
        }
        debug_tuple.finish()
    }
}

impl From<NativeKeyCode> for NativeKey {
    #[inline]
    fn from(code: NativeKeyCode) -> Self {
        match code {
            NativeKeyCode::Unidentified => NativeKey::Unidentified,
            NativeKeyCode::Android(x) => NativeKey::Android(x),
            NativeKeyCode::MacOS(x) => NativeKey::MacOS(x),
            NativeKeyCode::Windows(x) => NativeKey::Windows(x),
            NativeKeyCode::Xkb(x) => NativeKey::Xkb(x),
        }
    }
}

impl PartialEq<NativeKey> for NativeKeyCode {
    #[allow(clippy::cmp_owned)]
    #[inline]
    fn eq(&self, rhs: &NativeKey) -> bool {
        NativeKey::from(*self) == *rhs
    }
}

impl PartialEq<NativeKeyCode> for NativeKey {
    #[inline]
    fn eq(&self, rhs: &NativeKeyCode) -> bool {
        rhs == self
    }
}

/// Represents the location of a physical key, either a known [`KeyCode`] or [`Self::Unidentified`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PhysicalKey {
    /// A known key code.
    Code(KeyCode),
    /// A key that cannot be translated to a [`KeyCode`].
    Unidentified(NativeKeyCode),
}

impl From<KeyCode> for PhysicalKey {
    #[inline]
    fn from(code: KeyCode) -> Self {
        PhysicalKey::Code(code)
    }
}

impl From<NativeKeyCode> for PhysicalKey {
    #[inline]
    fn from(code: NativeKeyCode) -> Self {
        PhysicalKey::Unidentified(code)
    }
}

impl PartialEq<KeyCode> for PhysicalKey {
    #[inline]
    fn eq(&self, rhs: &KeyCode) -> bool {
        match self {
            PhysicalKey::Code(ref code) => code == rhs,
            _ => false,
        }
    }
}

impl PartialEq<PhysicalKey> for KeyCode {
    #[inline]
    fn eq(&self, rhs: &PhysicalKey) -> bool {
        rhs == self
    }
}

impl PartialEq<NativeKeyCode> for PhysicalKey {
    #[inline]
    fn eq(&self, rhs: &NativeKeyCode) -> bool {
        match self {
            PhysicalKey::Unidentified(ref code) => code == rhs,
            _ => false,
        }
    }
}

impl PartialEq<PhysicalKey> for NativeKeyCode {
    #[inline]
    fn eq(&self, rhs: &PhysicalKey) -> bool {
        rhs == self
    }
}

/// Code representing the location of a physical key (`KeyboardEvent.code`).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyCode {
    Backquote,
    Backslash,
    BracketLeft,
    BracketRight,
    Comma,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Equal,
    /// Key between left Shift and Z on ISO keyboards.
    IntlBackslash,
    /// Key between `/` and right Shift on Japanese keyboards.
    IntlRo,
    /// Key between `=` and Backspace on Japanese/Russian keyboards.
    IntlYen,
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    Minus,
    Period,
    Quote,
    Semicolon,
    Slash,
    AltLeft,
    AltRight,
    Backspace,
    CapsLock,
    ContextMenu,
    ControlLeft,
    ControlRight,
    Enter,
    SuperLeft,
    SuperRight,
    ShiftLeft,
    ShiftRight,
    Space,
    Tab,
    Convert,
    KanaMode,
    /// HangulMode (Korean) or Kana (Mac Japanese).
    Lang1,
    /// Hanja (Korean) or Eisu (Mac Japanese).
    Lang2,
    /// Katakana (Japanese word-processing).
    Lang3,
    /// Hiragana (Japanese word-processing).
    Lang4,
    /// Zenkaku/Hankaku (Japanese word-processing).
    Lang5,
    NonConvert,
    /// Forward delete key (main-block Mac `Delete` is [`Self::Backspace`]).
    Delete,
    End,
    Help,
    Home,
    Insert,
    PageDown,
    PageUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    /// NumLock on PC, or numpad `Clear` on Mac.
    NumLock,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadAdd,
    NumpadBackspace,
    NumpadClear,
    NumpadClearEntry,
    NumpadComma,
    NumpadDecimal,
    NumpadDivide,
    NumpadEnter,
    NumpadEqual,
    NumpadHash,
    NumpadMemoryAdd,
    NumpadMemoryClear,
    NumpadMemoryRecall,
    NumpadMemoryStore,
    NumpadMemorySubtract,
    NumpadMultiply,
    NumpadParenLeft,
    NumpadParenRight,
    NumpadStar,
    NumpadSubtract,
    Escape,
    Fn,
    FnLock,
    PrintScreen,
    ScrollLock,
    Pause,
    BrowserBack,
    BrowserFavorites,
    BrowserForward,
    BrowserHome,
    BrowserRefresh,
    BrowserSearch,
    BrowserStop,
    Eject,
    LaunchApp1,
    LaunchApp2,
    LaunchMail,
    MediaPlayPause,
    MediaSelect,
    MediaStop,
    MediaTrackNext,
    MediaTrackPrevious,
    Power,
    Sleep,
    AudioVolumeDown,
    AudioVolumeMute,
    AudioVolumeUp,
    WakeUp,
    Meta,
    Hyper,
    Turbo,
    Abort,
    Resume,
    Suspend,
    Again,
    Copy,
    Cut,
    Find,
    Open,
    Paste,
    Props,
    Select,
    Undo,
    Hiragana,
    Katakana,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    F25,
    F26,
    F27,
    F28,
    F29,
    F30,
    F31,
    F32,
    F33,
    F34,
    F35,
}

/// A [`Key::Named`] logical key value (`KeyboardEvent.key`).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NamedKey {
    Alt,
    AltGraph,
    CapsLock,
    Control,
    Fn,
    FnLock,
    NumLock,
    ScrollLock,
    Shift,
    Symbol,
    SymbolLock,
    Meta,
    Hyper,
    /// Windows Logo or Apple Command (`⌘`) key.
    Super,
    Enter,
    Tab,
    Space,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    End,
    /// Start-of-content Home key (for mobile home screen, see [`Self::GoHome`]).
    Home,
    PageDown,
    PageUp,
    /// Delete character before cursor (labeled `Delete` on macOS keyboards).
    Backspace,
    Clear,
    Copy,
    CrSel,
    Cut,
    /// Forward delete character after cursor (`Fn+Delete` on macOS keyboards).
    Delete,
    EraseEof,
    ExSel,
    Insert,
    Paste,
    Redo,
    Undo,
    Accept,
    Again,
    Attn,
    Cancel,
    ContextMenu,
    Escape,
    Execute,
    Find,
    Help,
    Pause,
    Play,
    Props,
    Select,
    ZoomIn,
    ZoomOut,
    BrightnessDown,
    BrightnessUp,
    Eject,
    LogOff,
    Power,
    PowerOff,
    PrintScreen,
    Hibernate,
    Standby,
    WakeUp,
    AllCandidates,
    Alphanumeric,
    CodeInput,
    Compose,
    Convert,
    FinalMode,
    GroupFirst,
    GroupLast,
    GroupNext,
    GroupPrevious,
    ModeChange,
    NextCandidate,
    NonConvert,
    PreviousCandidate,
    Process,
    SingleCandidate,
    HangulMode,
    HanjaMode,
    JunjaMode,
    Eisu,
    Hankaku,
    Hiragana,
    HiraganaKatakana,
    KanaMode,
    KanjiMode,
    Katakana,
    Romaji,
    Zenkaku,
    ZenkakuHankaku,
    Soft1,
    Soft2,
    Soft3,
    Soft4,
    ChannelDown,
    ChannelUp,
    Close,
    MailForward,
    MailReply,
    MailSend,
    MediaClose,
    MediaFastForward,
    MediaPause,
    MediaPlay,
    MediaPlayPause,
    MediaRecord,
    MediaRewind,
    MediaStop,
    MediaTrackNext,
    MediaTrackPrevious,
    New,
    Open,
    Print,
    Save,
    SpellCheck,
    Key11,
    Key12,
    AudioBalanceLeft,
    AudioBalanceRight,
    AudioBassBoostDown,
    AudioBassBoostToggle,
    AudioBassBoostUp,
    AudioFaderFront,
    AudioFaderRear,
    AudioSurroundModeNext,
    AudioTrebleDown,
    AudioTrebleUp,
    AudioVolumeDown,
    AudioVolumeUp,
    AudioVolumeMute,
    MicrophoneToggle,
    MicrophoneVolumeDown,
    MicrophoneVolumeUp,
    MicrophoneVolumeMute,
    SpeechCorrectionList,
    SpeechInputToggle,
    LaunchApplication1,
    LaunchApplication2,
    LaunchCalendar,
    LaunchContacts,
    LaunchMail,
    LaunchMediaPlayer,
    LaunchMusicPlayer,
    LaunchPhone,
    LaunchScreenSaver,
    LaunchSpreadsheet,
    LaunchWebBrowser,
    LaunchWebCam,
    LaunchWordProcessor,
    BrowserBack,
    BrowserFavorites,
    BrowserForward,
    BrowserHome,
    BrowserRefresh,
    BrowserSearch,
    BrowserStop,
    AppSwitch,
    Call,
    Camera,
    CameraFocus,
    EndCall,
    GoBack,
    GoHome,
    HeadsetHook,
    LastNumberRedial,
    Notification,
    MannerMode,
    VoiceDial,
    TV,
    TV3DMode,
    TVAntennaCable,
    TVAudioDescription,
    TVAudioDescriptionMixDown,
    TVAudioDescriptionMixUp,
    TVContentsMenu,
    TVDataService,
    TVInput,
    TVInputComponent1,
    TVInputComponent2,
    TVInputComposite1,
    TVInputComposite2,
    TVInputHDMI1,
    TVInputHDMI2,
    TVInputHDMI3,
    TVInputHDMI4,
    TVInputVGA1,
    TVMediaContext,
    TVNetwork,
    TVNumberEntry,
    TVPower,
    TVRadioService,
    TVSatellite,
    TVSatelliteBS,
    TVSatelliteCS,
    TVSatelliteToggle,
    TVTerrestrialAnalog,
    TVTerrestrialDigital,
    TVTimer,
    AVRInput,
    AVRPower,
    ColorF0Red,
    ColorF1Green,
    ColorF2Yellow,
    ColorF3Blue,
    ColorF4Grey,
    ColorF5Brown,
    ClosedCaptionToggle,
    Dimmer,
    DisplaySwap,
    DVR,
    Exit,
    FavoriteClear0,
    FavoriteClear1,
    FavoriteClear2,
    FavoriteClear3,
    FavoriteRecall0,
    FavoriteRecall1,
    FavoriteRecall2,
    FavoriteRecall3,
    FavoriteStore0,
    FavoriteStore1,
    FavoriteStore2,
    FavoriteStore3,
    Guide,
    GuideNextDay,
    GuidePreviousDay,
    Info,
    InstantReplay,
    Link,
    ListProgram,
    LiveContent,
    Lock,
    MediaApps,
    MediaAudioTrack,
    MediaLast,
    MediaSkipBackward,
    MediaSkipForward,
    MediaStepBackward,
    MediaStepForward,
    MediaTopMenu,
    NavigateIn,
    NavigateNext,
    NavigateOut,
    NavigatePrevious,
    NextFavoriteChannel,
    NextUserProfile,
    OnDemand,
    Pairing,
    PinPDown,
    PinPMove,
    PinPToggle,
    PinPUp,
    PlaySpeedDown,
    PlaySpeedReset,
    PlaySpeedUp,
    RandomToggle,
    RcLowBattery,
    RecordSpeedNext,
    RfBypass,
    ScanChannelsToggle,
    ScreenModeNext,
    Settings,
    SplitScreenToggle,
    STBInput,
    STBPower,
    Subtitle,
    Teletext,
    VideoModeNext,
    Wink,
    ZoomToggle,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    F25,
    F26,
    F27,
    F28,
    F29,
    F30,
    F31,
    F32,
    F33,
    F34,
    F35,
}

/// Logical meaning of a keypress (`KeyboardEvent.key`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key<Str = SmolStr> {
    /// A named non-character action key.
    Named(NamedKey),

    /// A character string produced by the keypress under the active layout.
    Character(Str),

    /// A key that cannot be translated to another variant.
    Unidentified(NativeKey),

    /// A dead key, with its character when available.
    Dead(Option<char>),
}

impl From<NamedKey> for Key {
    #[inline]
    fn from(action: NamedKey) -> Self {
        Key::Named(action)
    }
}

impl From<NativeKey> for Key {
    #[inline]
    fn from(code: NativeKey) -> Self {
        Key::Unidentified(code)
    }
}

impl<Str> PartialEq<NamedKey> for Key<Str> {
    #[inline]
    fn eq(&self, rhs: &NamedKey) -> bool {
        match self {
            Key::Named(ref a) => a == rhs,
            _ => false,
        }
    }
}

impl<Str: PartialEq<str>> PartialEq<str> for Key<Str> {
    #[inline]
    fn eq(&self, rhs: &str) -> bool {
        match self {
            Key::Character(ref s) => s == rhs,
            _ => false,
        }
    }
}

impl<Str: PartialEq<str>> PartialEq<&str> for Key<Str> {
    #[inline]
    fn eq(&self, rhs: &&str) -> bool {
        self == *rhs
    }
}

impl<Str> PartialEq<NativeKey> for Key<Str> {
    #[inline]
    fn eq(&self, rhs: &NativeKey) -> bool {
        match self {
            Key::Unidentified(ref code) => code == rhs,
            _ => false,
        }
    }
}

impl<Str> PartialEq<Key<Str>> for NativeKey {
    #[inline]
    fn eq(&self, rhs: &Key<Str>) -> bool {
        rhs == self
    }
}

impl Key<SmolStr> {
    /// Converts `Key::Character(SmolStr)` to `Key::Character(&str)`.
    #[inline]
    pub fn as_ref(&self) -> Key<&str> {
        match self {
            Key::Named(a) => Key::Named(*a),
            Key::Character(ch) => Key::Character(ch.as_str()),
            Key::Dead(d) => Key::Dead(*d),
            Key::Unidentified(u) => Key::Unidentified(u.clone()),
        }
    }
}

impl NamedKey {
    /// Converts a named key to its approximate textual equivalent.
    ///
    /// ```
    /// use aterm_window::keyboard::NamedKey;
    ///
    /// assert_eq!(NamedKey::Enter.to_text(), Some("\r"));
    /// assert_eq!(NamedKey::F20.to_text(), None);
    /// ```
    #[inline]
    pub const fn to_text(&self) -> Option<&'static str> {
        match self {
            NamedKey::Enter => Some("\r"),
            NamedKey::Backspace => Some("\x08"),
            NamedKey::Tab => Some("\t"),
            NamedKey::Space => Some(" "),
            NamedKey::Escape => Some("\x1b"),
            _ => None,
        }
    }
}

impl Key {
    /// Converts a key to its approximate textual equivalent.
    ///
    /// ```
    /// use aterm_window::keyboard::{Key, NamedKey};
    ///
    /// assert_eq!(Key::Character("a".into()).to_text(), Some("a"));
    /// assert_eq!(Key::Named(NamedKey::Enter).to_text(), Some("\r"));
    /// assert_eq!(Key::Named(NamedKey::F20).to_text(), None);
    /// ```
    #[inline]
    pub fn to_text(&self) -> Option<&str> {
        match self {
            Key::Named(action) => action.to_text(),
            Key::Character(ch) => Some(ch.as_str()),
            _ => None,
        }
    }
}

/// Physical location of a key on the keyboard (see [`KeyEvent::location`](crate::event::KeyEvent::location)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyLocation {
    /// The key is in its standard location (or its location cannot be identified).
    Standard,
    /// The key is on the left side of the keyboard.
    Left,
    /// The key is on the right side of the keyboard.
    Right,
    /// The key is on the numeric keypad.
    Numpad,
}

aterm_types::bitflags! {
    /// Active keyboard modifier flags.
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ModifiersState: u32 {
        /// The Shift key.
        const SHIFT = 0b100;
        /// The Control key.
        const CONTROL = 0b100 << 3;
        /// The Alt (Option) key.
        const ALT = 0b100 << 6;
        /// The Windows or Command (`⌘`) key.
        const SUPER = 0b100 << 9;
    }
}

impl ModifiersState {
    /// Returns `true` if the shift key is pressed.
    #[inline]
    pub const fn shift_key(&self) -> bool {
        self.intersects(Self::SHIFT)
    }

    /// Returns `true` if the control key is pressed.
    #[inline]
    pub const fn control_key(&self) -> bool {
        self.intersects(Self::CONTROL)
    }

    /// Returns `true` if the alt key is pressed.
    #[inline]
    pub const fn alt_key(&self) -> bool {
        self.intersects(Self::ALT)
    }

    /// Returns `true` if the super key is pressed.
    #[inline]
    pub const fn super_key(&self) -> bool {
        self.intersects(Self::SUPER)
    }
}

/// Press state of an individual modifier key.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifiersKeyState {
    /// The modifier key is pressed.
    Pressed,
    /// The state of the modifier key is unknown.
    #[default]
    Unknown,
}

aterm_types::bitflags! {
    #[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub(crate) struct ModifiersKeys: u8 {
        const LSHIFT   = 0b0000_0001;
        const RSHIFT   = 0b0000_0010;
        const LCONTROL = 0b0000_0100;
        const RCONTROL = 0b0000_1000;
        const LALT     = 0b0001_0000;
        const RALT     = 0b0010_0000;
        const LSUPER   = 0b0100_0000;
        const RSUPER   = 0b1000_0000;
    }
}
