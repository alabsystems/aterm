// Copyright 2022-2023 John Nunley
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from xkeysym 0.2.1 (MIT OR Apache-2.0 OR Zlib, used under
// Apache-2.0) src/automatically_generated.rs: the names and values of the
// keysyms the winit fork's key handling names, taken from its `key` module,
// with each one's C name (libxkbcommon's) added by the aterm project in 2026.

//! The keysyms aterm's key handling names (the winit fork's Linux backend
//! maps exactly these to keys), with their values from the X11 protocol's
//! keysym list (keysymdef.h, XF86keysym.h, Sunkeysym.h) as libxkbcommon's
//! `xkbcommon-keysyms.h` repeats them. The Rust names are xkeysym 0.2.1's
//! (`XF86_AudioMute`, `SUN_Copy`, `_3270_Attn`), so the fork's call sites
//! read as they did; [`NAMES`] gives each one's C name, what
//! `xkb_keysym_from_name` takes.
//!
//! Held to xkeysym 0.2.1's values by `tests/recorded/`, and in the Linux
//! lane to libxkbcommon's own (`xkb_keysym_from_name` of every C name).

#![allow(
    non_upper_case_globals,
    reason = "the X11 keysym names, as xkeysym spells them"
)]

use crate::Keysym;

/// `XKB_KEY_3270_Attn`.
pub const _3270_Attn: Keysym = 0xfd0e;
/// `XKB_KEY_3270_CursorSelect`.
pub const _3270_CursorSelect: Keysym = 0xfd1c;
/// `XKB_KEY_3270_Enter`.
pub const _3270_Enter: Keysym = 0xfd1e;
/// `XKB_KEY_3270_EraseEOF`.
pub const _3270_EraseEOF: Keysym = 0xfd06;
/// `XKB_KEY_3270_ExSelect`.
pub const _3270_ExSelect: Keysym = 0xfd1b;
/// `XKB_KEY_3270_Play`.
pub const _3270_Play: Keysym = 0xfd16;
/// `XKB_KEY_3270_PrintScreen`.
pub const _3270_PrintScreen: Keysym = 0xfd1d;
/// `XKB_KEY_Alt_L`.
pub const Alt_L: Keysym = 0xffe9;
/// `XKB_KEY_Alt_R`.
pub const Alt_R: Keysym = 0xffea;
/// `XKB_KEY_BackSpace`.
pub const BackSpace: Keysym = 0xff08;
/// `XKB_KEY_Break`.
pub const Break: Keysym = 0xff6b;
/// `XKB_KEY_Cancel`.
pub const Cancel: Keysym = 0xff69;
/// `XKB_KEY_Caps_Lock`.
pub const Caps_Lock: Keysym = 0xffe5;
/// `XKB_KEY_Clear`.
pub const Clear: Keysym = 0xff0b;
/// `XKB_KEY_Codeinput`.
pub const Codeinput: Keysym = 0xff37;
/// `XKB_KEY_Control_L`.
pub const Control_L: Keysym = 0xffe3;
/// `XKB_KEY_Control_R`.
pub const Control_R: Keysym = 0xffe4;
/// `XKB_KEY_Delete`.
pub const Delete: Keysym = 0xffff;
/// `XKB_KEY_Down`.
pub const Down: Keysym = 0xff54;
/// `XKB_KEY_Eisu_Shift`.
pub const Eisu_Shift: Keysym = 0xff2f;
/// `XKB_KEY_Eisu_toggle`.
pub const Eisu_toggle: Keysym = 0xff30;
/// `XKB_KEY_End`.
pub const End: Keysym = 0xff57;
/// `XKB_KEY_Escape`.
pub const Escape: Keysym = 0xff1b;
/// `XKB_KEY_Execute`.
pub const Execute: Keysym = 0xff62;
/// `XKB_KEY_F1`.
pub const F1: Keysym = 0xffbe;
/// `XKB_KEY_F10`.
pub const F10: Keysym = 0xffc7;
/// `XKB_KEY_F11`.
pub const F11: Keysym = 0xffc8;
/// `XKB_KEY_F12`.
pub const F12: Keysym = 0xffc9;
/// `XKB_KEY_F13`.
pub const F13: Keysym = 0xffca;
/// `XKB_KEY_F14`.
pub const F14: Keysym = 0xffcb;
/// `XKB_KEY_F15`.
pub const F15: Keysym = 0xffcc;
/// `XKB_KEY_F16`.
pub const F16: Keysym = 0xffcd;
/// `XKB_KEY_F17`.
pub const F17: Keysym = 0xffce;
/// `XKB_KEY_F18`.
pub const F18: Keysym = 0xffcf;
/// `XKB_KEY_F19`.
pub const F19: Keysym = 0xffd0;
/// `XKB_KEY_F2`.
pub const F2: Keysym = 0xffbf;
/// `XKB_KEY_F20`.
pub const F20: Keysym = 0xffd1;
/// `XKB_KEY_F21`.
pub const F21: Keysym = 0xffd2;
/// `XKB_KEY_F22`.
pub const F22: Keysym = 0xffd3;
/// `XKB_KEY_F23`.
pub const F23: Keysym = 0xffd4;
/// `XKB_KEY_F24`.
pub const F24: Keysym = 0xffd5;
/// `XKB_KEY_F25`.
pub const F25: Keysym = 0xffd6;
/// `XKB_KEY_F26`.
pub const F26: Keysym = 0xffd7;
/// `XKB_KEY_F27`.
pub const F27: Keysym = 0xffd8;
/// `XKB_KEY_F28`.
pub const F28: Keysym = 0xffd9;
/// `XKB_KEY_F29`.
pub const F29: Keysym = 0xffda;
/// `XKB_KEY_F3`.
pub const F3: Keysym = 0xffc0;
/// `XKB_KEY_F30`.
pub const F30: Keysym = 0xffdb;
/// `XKB_KEY_F31`.
pub const F31: Keysym = 0xffdc;
/// `XKB_KEY_F32`.
pub const F32: Keysym = 0xffdd;
/// `XKB_KEY_F33`.
pub const F33: Keysym = 0xffde;
/// `XKB_KEY_F34`.
pub const F34: Keysym = 0xffdf;
/// `XKB_KEY_F35`.
pub const F35: Keysym = 0xffe0;
/// `XKB_KEY_F4`.
pub const F4: Keysym = 0xffc1;
/// `XKB_KEY_F5`.
pub const F5: Keysym = 0xffc2;
/// `XKB_KEY_F6`.
pub const F6: Keysym = 0xffc3;
/// `XKB_KEY_F7`.
pub const F7: Keysym = 0xffc4;
/// `XKB_KEY_F8`.
pub const F8: Keysym = 0xffc5;
/// `XKB_KEY_F9`.
pub const F9: Keysym = 0xffc6;
/// `XKB_KEY_Find`.
pub const Find: Keysym = 0xff68;
/// `XKB_KEY_Hankaku`.
pub const Hankaku: Keysym = 0xff29;
/// `XKB_KEY_Help`.
pub const Help: Keysym = 0xff6a;
/// `XKB_KEY_Henkan_Mode`.
pub const Henkan_Mode: Keysym = 0xff23;
/// `XKB_KEY_Hiragana`.
pub const Hiragana: Keysym = 0xff25;
/// `XKB_KEY_Hiragana_Katakana`.
pub const Hiragana_Katakana: Keysym = 0xff27;
/// `XKB_KEY_Home`.
pub const Home: Keysym = 0xff50;
/// `XKB_KEY_Hyper_L`.
pub const Hyper_L: Keysym = 0xffed;
/// `XKB_KEY_Hyper_R`.
pub const Hyper_R: Keysym = 0xffee;
/// `XKB_KEY_Insert`.
pub const Insert: Keysym = 0xff63;
/// `XKB_KEY_ISO_Enter`.
pub const ISO_Enter: Keysym = 0xfe34;
/// `XKB_KEY_ISO_First_Group`.
pub const ISO_First_Group: Keysym = 0xfe0c;
/// `XKB_KEY_ISO_Last_Group`.
pub const ISO_Last_Group: Keysym = 0xfe0e;
/// `XKB_KEY_ISO_Left_Tab`.
pub const ISO_Left_Tab: Keysym = 0xfe20;
/// `XKB_KEY_ISO_Level3_Latch`.
pub const ISO_Level3_Latch: Keysym = 0xfe04;
/// `XKB_KEY_ISO_Level3_Lock`.
pub const ISO_Level3_Lock: Keysym = 0xfe05;
/// `XKB_KEY_ISO_Level3_Shift`.
pub const ISO_Level3_Shift: Keysym = 0xfe03;
/// `XKB_KEY_ISO_Next_Group`.
pub const ISO_Next_Group: Keysym = 0xfe08;
/// `XKB_KEY_ISO_Prev_Group`.
pub const ISO_Prev_Group: Keysym = 0xfe0a;
/// `XKB_KEY_Kana_Lock`.
pub const Kana_Lock: Keysym = 0xff2d;
/// `XKB_KEY_Kana_Shift`.
pub const Kana_Shift: Keysym = 0xff2e;
/// `XKB_KEY_Kanji`.
pub const Kanji: Keysym = 0xff21;
/// `XKB_KEY_KP_0`.
pub const KP_0: Keysym = 0xffb0;
/// `XKB_KEY_KP_1`.
pub const KP_1: Keysym = 0xffb1;
/// `XKB_KEY_KP_2`.
pub const KP_2: Keysym = 0xffb2;
/// `XKB_KEY_KP_3`.
pub const KP_3: Keysym = 0xffb3;
/// `XKB_KEY_KP_4`.
pub const KP_4: Keysym = 0xffb4;
/// `XKB_KEY_KP_5`.
pub const KP_5: Keysym = 0xffb5;
/// `XKB_KEY_KP_6`.
pub const KP_6: Keysym = 0xffb6;
/// `XKB_KEY_KP_7`.
pub const KP_7: Keysym = 0xffb7;
/// `XKB_KEY_KP_8`.
pub const KP_8: Keysym = 0xffb8;
/// `XKB_KEY_KP_9`.
pub const KP_9: Keysym = 0xffb9;
/// `XKB_KEY_KP_Add`.
pub const KP_Add: Keysym = 0xffab;
/// `XKB_KEY_KP_Begin`.
pub const KP_Begin: Keysym = 0xff9d;
/// `XKB_KEY_KP_Decimal`.
pub const KP_Decimal: Keysym = 0xffae;
/// `XKB_KEY_KP_Delete`.
pub const KP_Delete: Keysym = 0xff9f;
/// `XKB_KEY_KP_Divide`.
pub const KP_Divide: Keysym = 0xffaf;
/// `XKB_KEY_KP_Down`.
pub const KP_Down: Keysym = 0xff99;
/// `XKB_KEY_KP_End`.
pub const KP_End: Keysym = 0xff9c;
/// `XKB_KEY_KP_Enter`.
pub const KP_Enter: Keysym = 0xff8d;
/// `XKB_KEY_KP_Equal`.
pub const KP_Equal: Keysym = 0xffbd;
/// `XKB_KEY_KP_F1`.
pub const KP_F1: Keysym = 0xff91;
/// `XKB_KEY_KP_F2`.
pub const KP_F2: Keysym = 0xff92;
/// `XKB_KEY_KP_F3`.
pub const KP_F3: Keysym = 0xff93;
/// `XKB_KEY_KP_F4`.
pub const KP_F4: Keysym = 0xff94;
/// `XKB_KEY_KP_Home`.
pub const KP_Home: Keysym = 0xff95;
/// `XKB_KEY_KP_Insert`.
pub const KP_Insert: Keysym = 0xff9e;
/// `XKB_KEY_KP_Left`.
pub const KP_Left: Keysym = 0xff96;
/// `XKB_KEY_KP_Multiply`.
pub const KP_Multiply: Keysym = 0xffaa;
/// `XKB_KEY_KP_Page_Down`.
pub const KP_Page_Down: Keysym = 0xff9b;
/// `XKB_KEY_KP_Page_Up`.
pub const KP_Page_Up: Keysym = 0xff9a;
/// `XKB_KEY_KP_Right`.
pub const KP_Right: Keysym = 0xff98;
/// `XKB_KEY_KP_Separator`.
pub const KP_Separator: Keysym = 0xffac;
/// `XKB_KEY_KP_Space`.
pub const KP_Space: Keysym = 0xff80;
/// `XKB_KEY_KP_Subtract`.
pub const KP_Subtract: Keysym = 0xffad;
/// `XKB_KEY_KP_Tab`.
pub const KP_Tab: Keysym = 0xff89;
/// `XKB_KEY_KP_Up`.
pub const KP_Up: Keysym = 0xff97;
/// `XKB_KEY_Left`.
pub const Left: Keysym = 0xff51;
/// `XKB_KEY_Menu`.
pub const Menu: Keysym = 0xff67;
/// `XKB_KEY_Meta_L`.
pub const Meta_L: Keysym = 0xffe7;
/// `XKB_KEY_Meta_R`.
pub const Meta_R: Keysym = 0xffe8;
/// `XKB_KEY_Mode_switch`.
pub const Mode_switch: Keysym = 0xff7e;
/// `XKB_KEY_Muhenkan`.
pub const Muhenkan: Keysym = 0xff22;
/// `XKB_KEY_Multi_key`.
pub const Multi_key: Keysym = 0xff20;
/// `XKB_KEY_MultipleCandidate`.
pub const MultipleCandidate: Keysym = 0xff3d;
/// `XKB_KEY_Num_Lock`.
pub const Num_Lock: Keysym = 0xff7f;
/// `XKB_KEY_Page_Down`.
pub const Page_Down: Keysym = 0xff56;
/// `XKB_KEY_Page_Up`.
pub const Page_Up: Keysym = 0xff55;
/// `XKB_KEY_Pause`.
pub const Pause: Keysym = 0xff13;
/// `XKB_KEY_PreviousCandidate`.
pub const PreviousCandidate: Keysym = 0xff3e;
/// `XKB_KEY_Print`.
pub const Print: Keysym = 0xff61;
/// `XKB_KEY_Redo`.
pub const Redo: Keysym = 0xff66;
/// `XKB_KEY_Return`.
pub const Return: Keysym = 0xff0d;
/// `XKB_KEY_Right`.
pub const Right: Keysym = 0xff53;
/// `XKB_KEY_Romaji`.
pub const Romaji: Keysym = 0xff24;
/// `XKB_KEY_Scroll_Lock`.
pub const Scroll_Lock: Keysym = 0xff14;
/// `XKB_KEY_Select`.
pub const Select: Keysym = 0xff60;
/// `XKB_KEY_Shift_L`.
pub const Shift_L: Keysym = 0xffe1;
/// `XKB_KEY_Shift_R`.
pub const Shift_R: Keysym = 0xffe2;
/// `XKB_KEY_SingleCandidate`.
pub const SingleCandidate: Keysym = 0xff3c;
/// `XKB_KEY_space`.
pub const space: Keysym = 0x20;
/// `XKB_KEY_SunAudioLowerVolume`.
pub const SUN_AudioLowerVolume: Keysym = 0x1005ff77;
/// `XKB_KEY_SunAudioMute`.
pub const SUN_AudioMute: Keysym = 0x1005ff78;
/// `XKB_KEY_SunAudioRaiseVolume`.
pub const SUN_AudioRaiseVolume: Keysym = 0x1005ff79;
/// `XKB_KEY_SunCopy`.
pub const SUN_Copy: Keysym = 0x1005ff72;
/// `XKB_KEY_SunCut`.
pub const SUN_Cut: Keysym = 0x1005ff75;
/// `XKB_KEY_SunOpen`.
pub const SUN_Open: Keysym = 0x1005ff73;
/// `XKB_KEY_SunPaste`.
pub const SUN_Paste: Keysym = 0x1005ff74;
/// `XKB_KEY_SunVideoLowerBrightness`.
pub const SUN_VideoLowerBrightness: Keysym = 0x1005ff7b;
/// `XKB_KEY_SunVideoRaiseBrightness`.
pub const SUN_VideoRaiseBrightness: Keysym = 0x1005ff7c;
/// `XKB_KEY_Super_L`.
pub const Super_L: Keysym = 0xffeb;
/// `XKB_KEY_Super_R`.
pub const Super_R: Keysym = 0xffec;
/// `XKB_KEY_Sys_Req`.
pub const Sys_Req: Keysym = 0xff15;
/// `XKB_KEY_Tab`.
pub const Tab: Keysym = 0xff09;
/// `XKB_KEY_Undo`.
pub const Undo: Keysym = 0xff65;
/// `XKB_KEY_Up`.
pub const Up: Keysym = 0xff52;
/// `XKB_KEY_XF86AudioCycleTrack`.
pub const XF86_AudioCycleTrack: Keysym = 0x1008ff9b;
/// `XKB_KEY_XF86AudioForward`.
pub const XF86_AudioForward: Keysym = 0x1008ff97;
/// `XKB_KEY_XF86AudioLowerVolume`.
pub const XF86_AudioLowerVolume: Keysym = 0x1008ff11;
/// `XKB_KEY_XF86AudioMute`.
pub const XF86_AudioMute: Keysym = 0x1008ff12;
/// `XKB_KEY_XF86AudioNext`.
pub const XF86_AudioNext: Keysym = 0x1008ff17;
/// `XKB_KEY_XF86AudioPause`.
pub const XF86_AudioPause: Keysym = 0x1008ff31;
/// `XKB_KEY_XF86AudioPlay`.
pub const XF86_AudioPlay: Keysym = 0x1008ff14;
/// `XKB_KEY_XF86AudioPrev`.
pub const XF86_AudioPrev: Keysym = 0x1008ff16;
/// `XKB_KEY_XF86AudioRaiseVolume`.
pub const XF86_AudioRaiseVolume: Keysym = 0x1008ff13;
/// `XKB_KEY_XF86AudioRandomPlay`.
pub const XF86_AudioRandomPlay: Keysym = 0x1008ff99;
/// `XKB_KEY_XF86AudioRecord`.
pub const XF86_AudioRecord: Keysym = 0x1008ff1c;
/// `XKB_KEY_XF86AudioRewind`.
pub const XF86_AudioRewind: Keysym = 0x1008ff3e;
/// `XKB_KEY_XF86AudioStop`.
pub const XF86_AudioStop: Keysym = 0x1008ff15;
/// `XKB_KEY_XF86Back`.
pub const XF86_Back: Keysym = 0x1008ff26;
/// `XKB_KEY_XF86Calculater`.
pub const XF86_Calculater: Keysym = 0x1008ff54;
/// `XKB_KEY_XF86Calculator`.
pub const XF86_Calculator: Keysym = 0x1008ff1d;
/// `XKB_KEY_XF86Calendar`.
pub const XF86_Calendar: Keysym = 0x1008ff20;
/// `XKB_KEY_XF86Close`.
pub const XF86_Close: Keysym = 0x1008ff56;
/// `XKB_KEY_XF86Copy`.
pub const XF86_Copy: Keysym = 0x1008ff57;
/// `XKB_KEY_XF86Cut`.
pub const XF86_Cut: Keysym = 0x1008ff58;
/// `XKB_KEY_XF86Eject`.
pub const XF86_Eject: Keysym = 0x1008ff2c;
/// `XKB_KEY_XF86Excel`.
pub const XF86_Excel: Keysym = 0x1008ff5c;
/// `XKB_KEY_XF86Favorites`.
pub const XF86_Favorites: Keysym = 0x1008ff30;
/// `XKB_KEY_XF86Forward`.
pub const XF86_Forward: Keysym = 0x1008ff27;
/// `XKB_KEY_XF86Hibernate`.
pub const XF86_Hibernate: Keysym = 0x1008ffa8;
/// `XKB_KEY_XF86HomePage`.
pub const XF86_HomePage: Keysym = 0x1008ff18;
/// `XKB_KEY_XF86LogOff`.
pub const XF86_LogOff: Keysym = 0x1008ff61;
/// `XKB_KEY_XF86Mail`.
pub const XF86_Mail: Keysym = 0x1008ff19;
/// `XKB_KEY_XF86MailForward`.
pub const XF86_MailForward: Keysym = 0x1008ff90;
/// `XKB_KEY_XF86MonBrightnessDown`.
pub const XF86_MonBrightnessDown: Keysym = 0x1008ff03;
/// `XKB_KEY_XF86MonBrightnessUp`.
pub const XF86_MonBrightnessUp: Keysym = 0x1008ff02;
/// `XKB_KEY_XF86Music`.
pub const XF86_Music: Keysym = 0x1008ff92;
/// `XKB_KEY_XF86MyComputer`.
pub const XF86_MyComputer: Keysym = 0x1008ff33;
/// `XKB_KEY_XF86MySites`.
pub const XF86_MySites: Keysym = 0x1008ff67;
/// `XKB_KEY_XF86New`.
pub const XF86_New: Keysym = 0x1008ff68;
/// `XKB_KEY_XF86Next_VMode`.
pub const XF86_Next_VMode: Keysym = 0x1008fe22;
/// `XKB_KEY_XF86Open`.
pub const XF86_Open: Keysym = 0x1008ff6b;
/// `XKB_KEY_XF86Paste`.
pub const XF86_Paste: Keysym = 0x1008ff6d;
/// `XKB_KEY_XF86Phone`.
pub const XF86_Phone: Keysym = 0x1008ff6e;
/// `XKB_KEY_XF86PowerDown`.
pub const XF86_PowerDown: Keysym = 0x1008ff21;
/// `XKB_KEY_XF86PowerOff`.
pub const XF86_PowerOff: Keysym = 0x1008ff2a;
/// `XKB_KEY_XF86Refresh`.
pub const XF86_Refresh: Keysym = 0x1008ff29;
/// `XKB_KEY_XF86Reload`.
pub const XF86_Reload: Keysym = 0x1008ff73;
/// `XKB_KEY_XF86Reply`.
pub const XF86_Reply: Keysym = 0x1008ff72;
/// `XKB_KEY_XF86Save`.
pub const XF86_Save: Keysym = 0x1008ff77;
/// `XKB_KEY_XF86ScreenSaver`.
pub const XF86_ScreenSaver: Keysym = 0x1008ff2d;
/// `XKB_KEY_XF86Search`.
pub const XF86_Search: Keysym = 0x1008ff1b;
/// `XKB_KEY_XF86Send`.
pub const XF86_Send: Keysym = 0x1008ff7b;
/// `XKB_KEY_XF86Sleep`.
pub const XF86_Sleep: Keysym = 0x1008ff2f;
/// `XKB_KEY_XF86Spell`.
pub const XF86_Spell: Keysym = 0x1008ff7c;
/// `XKB_KEY_XF86SplitScreen`.
pub const XF86_SplitScreen: Keysym = 0x1008ff7d;
/// `XKB_KEY_XF86Standby`.
pub const XF86_Standby: Keysym = 0x1008ff10;
/// `XKB_KEY_XF86Subtitle`.
pub const XF86_Subtitle: Keysym = 0x1008ff9a;
/// `XKB_KEY_XF86Suspend`.
pub const XF86_Suspend: Keysym = 0x1008ffa7;
/// `XKB_KEY_XF86Video`.
pub const XF86_Video: Keysym = 0x1008ff87;
/// `XKB_KEY_XF86WakeUp`.
pub const XF86_WakeUp: Keysym = 0x1008ff2b;
/// `XKB_KEY_XF86WebCam`.
pub const XF86_WebCam: Keysym = 0x1008ff8f;
/// `XKB_KEY_XF86Word`.
pub const XF86_Word: Keysym = 0x1008ff89;
/// `XKB_KEY_XF86WWW`.
pub const XF86_WWW: Keysym = 0x1008ff2e;
/// `XKB_KEY_XF86ZoomIn`.
pub const XF86_ZoomIn: Keysym = 0x1008ff8b;
/// `XKB_KEY_XF86ZoomOut`.
pub const XF86_ZoomOut: Keysym = 0x1008ff8c;
/// `XKB_KEY_Zenkaku`.
pub const Zenkaku: Keysym = 0xff28;
/// `XKB_KEY_Zenkaku_Hankaku`.
pub const Zenkaku_Hankaku: Keysym = 0xff2a;

/// Every constant above as (Rust name, C name, value), in value order.
pub static NAMES: &[(&str, &str, Keysym)] = &[
    ("space", "space", space),
    ("_3270_EraseEOF", "3270_EraseEOF", _3270_EraseEOF),
    ("_3270_Attn", "3270_Attn", _3270_Attn),
    ("_3270_Play", "3270_Play", _3270_Play),
    ("_3270_ExSelect", "3270_ExSelect", _3270_ExSelect),
    (
        "_3270_CursorSelect",
        "3270_CursorSelect",
        _3270_CursorSelect,
    ),
    ("_3270_PrintScreen", "3270_PrintScreen", _3270_PrintScreen),
    ("_3270_Enter", "3270_Enter", _3270_Enter),
    ("ISO_Level3_Shift", "ISO_Level3_Shift", ISO_Level3_Shift),
    ("ISO_Level3_Latch", "ISO_Level3_Latch", ISO_Level3_Latch),
    ("ISO_Level3_Lock", "ISO_Level3_Lock", ISO_Level3_Lock),
    ("ISO_Next_Group", "ISO_Next_Group", ISO_Next_Group),
    ("ISO_Prev_Group", "ISO_Prev_Group", ISO_Prev_Group),
    ("ISO_First_Group", "ISO_First_Group", ISO_First_Group),
    ("ISO_Last_Group", "ISO_Last_Group", ISO_Last_Group),
    ("ISO_Left_Tab", "ISO_Left_Tab", ISO_Left_Tab),
    ("ISO_Enter", "ISO_Enter", ISO_Enter),
    ("BackSpace", "BackSpace", BackSpace),
    ("Tab", "Tab", Tab),
    ("Clear", "Clear", Clear),
    ("Return", "Return", Return),
    ("Pause", "Pause", Pause),
    ("Scroll_Lock", "Scroll_Lock", Scroll_Lock),
    ("Sys_Req", "Sys_Req", Sys_Req),
    ("Escape", "Escape", Escape),
    ("Multi_key", "Multi_key", Multi_key),
    ("Kanji", "Kanji", Kanji),
    ("Muhenkan", "Muhenkan", Muhenkan),
    ("Henkan_Mode", "Henkan_Mode", Henkan_Mode),
    ("Romaji", "Romaji", Romaji),
    ("Hiragana", "Hiragana", Hiragana),
    ("Hiragana_Katakana", "Hiragana_Katakana", Hiragana_Katakana),
    ("Zenkaku", "Zenkaku", Zenkaku),
    ("Hankaku", "Hankaku", Hankaku),
    ("Zenkaku_Hankaku", "Zenkaku_Hankaku", Zenkaku_Hankaku),
    ("Kana_Lock", "Kana_Lock", Kana_Lock),
    ("Kana_Shift", "Kana_Shift", Kana_Shift),
    ("Eisu_Shift", "Eisu_Shift", Eisu_Shift),
    ("Eisu_toggle", "Eisu_toggle", Eisu_toggle),
    ("Codeinput", "Codeinput", Codeinput),
    ("SingleCandidate", "SingleCandidate", SingleCandidate),
    ("MultipleCandidate", "MultipleCandidate", MultipleCandidate),
    ("PreviousCandidate", "PreviousCandidate", PreviousCandidate),
    ("Home", "Home", Home),
    ("Left", "Left", Left),
    ("Up", "Up", Up),
    ("Right", "Right", Right),
    ("Down", "Down", Down),
    ("Page_Up", "Page_Up", Page_Up),
    ("Page_Down", "Page_Down", Page_Down),
    ("End", "End", End),
    ("Select", "Select", Select),
    ("Print", "Print", Print),
    ("Execute", "Execute", Execute),
    ("Insert", "Insert", Insert),
    ("Undo", "Undo", Undo),
    ("Redo", "Redo", Redo),
    ("Menu", "Menu", Menu),
    ("Find", "Find", Find),
    ("Cancel", "Cancel", Cancel),
    ("Help", "Help", Help),
    ("Break", "Break", Break),
    ("Mode_switch", "Mode_switch", Mode_switch),
    ("Num_Lock", "Num_Lock", Num_Lock),
    ("KP_Space", "KP_Space", KP_Space),
    ("KP_Tab", "KP_Tab", KP_Tab),
    ("KP_Enter", "KP_Enter", KP_Enter),
    ("KP_F1", "KP_F1", KP_F1),
    ("KP_F2", "KP_F2", KP_F2),
    ("KP_F3", "KP_F3", KP_F3),
    ("KP_F4", "KP_F4", KP_F4),
    ("KP_Home", "KP_Home", KP_Home),
    ("KP_Left", "KP_Left", KP_Left),
    ("KP_Up", "KP_Up", KP_Up),
    ("KP_Right", "KP_Right", KP_Right),
    ("KP_Down", "KP_Down", KP_Down),
    ("KP_Page_Up", "KP_Page_Up", KP_Page_Up),
    ("KP_Page_Down", "KP_Page_Down", KP_Page_Down),
    ("KP_End", "KP_End", KP_End),
    ("KP_Begin", "KP_Begin", KP_Begin),
    ("KP_Insert", "KP_Insert", KP_Insert),
    ("KP_Delete", "KP_Delete", KP_Delete),
    ("KP_Multiply", "KP_Multiply", KP_Multiply),
    ("KP_Add", "KP_Add", KP_Add),
    ("KP_Separator", "KP_Separator", KP_Separator),
    ("KP_Subtract", "KP_Subtract", KP_Subtract),
    ("KP_Decimal", "KP_Decimal", KP_Decimal),
    ("KP_Divide", "KP_Divide", KP_Divide),
    ("KP_0", "KP_0", KP_0),
    ("KP_1", "KP_1", KP_1),
    ("KP_2", "KP_2", KP_2),
    ("KP_3", "KP_3", KP_3),
    ("KP_4", "KP_4", KP_4),
    ("KP_5", "KP_5", KP_5),
    ("KP_6", "KP_6", KP_6),
    ("KP_7", "KP_7", KP_7),
    ("KP_8", "KP_8", KP_8),
    ("KP_9", "KP_9", KP_9),
    ("KP_Equal", "KP_Equal", KP_Equal),
    ("F1", "F1", F1),
    ("F2", "F2", F2),
    ("F3", "F3", F3),
    ("F4", "F4", F4),
    ("F5", "F5", F5),
    ("F6", "F6", F6),
    ("F7", "F7", F7),
    ("F8", "F8", F8),
    ("F9", "F9", F9),
    ("F10", "F10", F10),
    ("F11", "F11", F11),
    ("F12", "F12", F12),
    ("F13", "F13", F13),
    ("F14", "F14", F14),
    ("F15", "F15", F15),
    ("F16", "F16", F16),
    ("F17", "F17", F17),
    ("F18", "F18", F18),
    ("F19", "F19", F19),
    ("F20", "F20", F20),
    ("F21", "F21", F21),
    ("F22", "F22", F22),
    ("F23", "F23", F23),
    ("F24", "F24", F24),
    ("F25", "F25", F25),
    ("F26", "F26", F26),
    ("F27", "F27", F27),
    ("F28", "F28", F28),
    ("F29", "F29", F29),
    ("F30", "F30", F30),
    ("F31", "F31", F31),
    ("F32", "F32", F32),
    ("F33", "F33", F33),
    ("F34", "F34", F34),
    ("F35", "F35", F35),
    ("Shift_L", "Shift_L", Shift_L),
    ("Shift_R", "Shift_R", Shift_R),
    ("Control_L", "Control_L", Control_L),
    ("Control_R", "Control_R", Control_R),
    ("Caps_Lock", "Caps_Lock", Caps_Lock),
    ("Meta_L", "Meta_L", Meta_L),
    ("Meta_R", "Meta_R", Meta_R),
    ("Alt_L", "Alt_L", Alt_L),
    ("Alt_R", "Alt_R", Alt_R),
    ("Super_L", "Super_L", Super_L),
    ("Super_R", "Super_R", Super_R),
    ("Hyper_L", "Hyper_L", Hyper_L),
    ("Hyper_R", "Hyper_R", Hyper_R),
    ("Delete", "Delete", Delete),
    ("SUN_Copy", "SunCopy", SUN_Copy),
    ("SUN_Open", "SunOpen", SUN_Open),
    ("SUN_Paste", "SunPaste", SUN_Paste),
    ("SUN_Cut", "SunCut", SUN_Cut),
    (
        "SUN_AudioLowerVolume",
        "SunAudioLowerVolume",
        SUN_AudioLowerVolume,
    ),
    ("SUN_AudioMute", "SunAudioMute", SUN_AudioMute),
    (
        "SUN_AudioRaiseVolume",
        "SunAudioRaiseVolume",
        SUN_AudioRaiseVolume,
    ),
    (
        "SUN_VideoLowerBrightness",
        "SunVideoLowerBrightness",
        SUN_VideoLowerBrightness,
    ),
    (
        "SUN_VideoRaiseBrightness",
        "SunVideoRaiseBrightness",
        SUN_VideoRaiseBrightness,
    ),
    ("XF86_Next_VMode", "XF86Next_VMode", XF86_Next_VMode),
    (
        "XF86_MonBrightnessUp",
        "XF86MonBrightnessUp",
        XF86_MonBrightnessUp,
    ),
    (
        "XF86_MonBrightnessDown",
        "XF86MonBrightnessDown",
        XF86_MonBrightnessDown,
    ),
    ("XF86_Standby", "XF86Standby", XF86_Standby),
    (
        "XF86_AudioLowerVolume",
        "XF86AudioLowerVolume",
        XF86_AudioLowerVolume,
    ),
    ("XF86_AudioMute", "XF86AudioMute", XF86_AudioMute),
    (
        "XF86_AudioRaiseVolume",
        "XF86AudioRaiseVolume",
        XF86_AudioRaiseVolume,
    ),
    ("XF86_AudioPlay", "XF86AudioPlay", XF86_AudioPlay),
    ("XF86_AudioStop", "XF86AudioStop", XF86_AudioStop),
    ("XF86_AudioPrev", "XF86AudioPrev", XF86_AudioPrev),
    ("XF86_AudioNext", "XF86AudioNext", XF86_AudioNext),
    ("XF86_HomePage", "XF86HomePage", XF86_HomePage),
    ("XF86_Mail", "XF86Mail", XF86_Mail),
    ("XF86_Search", "XF86Search", XF86_Search),
    ("XF86_AudioRecord", "XF86AudioRecord", XF86_AudioRecord),
    ("XF86_Calculator", "XF86Calculator", XF86_Calculator),
    ("XF86_Calendar", "XF86Calendar", XF86_Calendar),
    ("XF86_PowerDown", "XF86PowerDown", XF86_PowerDown),
    ("XF86_Back", "XF86Back", XF86_Back),
    ("XF86_Forward", "XF86Forward", XF86_Forward),
    ("XF86_Refresh", "XF86Refresh", XF86_Refresh),
    ("XF86_PowerOff", "XF86PowerOff", XF86_PowerOff),
    ("XF86_WakeUp", "XF86WakeUp", XF86_WakeUp),
    ("XF86_Eject", "XF86Eject", XF86_Eject),
    ("XF86_ScreenSaver", "XF86ScreenSaver", XF86_ScreenSaver),
    ("XF86_WWW", "XF86WWW", XF86_WWW),
    ("XF86_Sleep", "XF86Sleep", XF86_Sleep),
    ("XF86_Favorites", "XF86Favorites", XF86_Favorites),
    ("XF86_AudioPause", "XF86AudioPause", XF86_AudioPause),
    ("XF86_MyComputer", "XF86MyComputer", XF86_MyComputer),
    ("XF86_AudioRewind", "XF86AudioRewind", XF86_AudioRewind),
    ("XF86_Calculater", "XF86Calculater", XF86_Calculater),
    ("XF86_Close", "XF86Close", XF86_Close),
    ("XF86_Copy", "XF86Copy", XF86_Copy),
    ("XF86_Cut", "XF86Cut", XF86_Cut),
    ("XF86_Excel", "XF86Excel", XF86_Excel),
    ("XF86_LogOff", "XF86LogOff", XF86_LogOff),
    ("XF86_MySites", "XF86MySites", XF86_MySites),
    ("XF86_New", "XF86New", XF86_New),
    ("XF86_Open", "XF86Open", XF86_Open),
    ("XF86_Paste", "XF86Paste", XF86_Paste),
    ("XF86_Phone", "XF86Phone", XF86_Phone),
    ("XF86_Reply", "XF86Reply", XF86_Reply),
    ("XF86_Reload", "XF86Reload", XF86_Reload),
    ("XF86_Save", "XF86Save", XF86_Save),
    ("XF86_Send", "XF86Send", XF86_Send),
    ("XF86_Spell", "XF86Spell", XF86_Spell),
    ("XF86_SplitScreen", "XF86SplitScreen", XF86_SplitScreen),
    ("XF86_Video", "XF86Video", XF86_Video),
    ("XF86_Word", "XF86Word", XF86_Word),
    ("XF86_ZoomIn", "XF86ZoomIn", XF86_ZoomIn),
    ("XF86_ZoomOut", "XF86ZoomOut", XF86_ZoomOut),
    ("XF86_WebCam", "XF86WebCam", XF86_WebCam),
    ("XF86_MailForward", "XF86MailForward", XF86_MailForward),
    ("XF86_Music", "XF86Music", XF86_Music),
    ("XF86_AudioForward", "XF86AudioForward", XF86_AudioForward),
    (
        "XF86_AudioRandomPlay",
        "XF86AudioRandomPlay",
        XF86_AudioRandomPlay,
    ),
    ("XF86_Subtitle", "XF86Subtitle", XF86_Subtitle),
    (
        "XF86_AudioCycleTrack",
        "XF86AudioCycleTrack",
        XF86_AudioCycleTrack,
    ),
    ("XF86_Suspend", "XF86Suspend", XF86_Suspend),
    ("XF86_Hibernate", "XF86Hibernate", XF86_Hibernate),
];
