// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! First-party libxkbcommon for aterm: the entry points the keyboard handling
//! calls, read at run time through `aterm-dl`, their types and constants, and
//! the keysyms it names ([`keysym`]).
//!
//! It replaces xkbcommon-dl and xkeysym on Linux
//! (docs/AUDIT-third-party-2026-10-15.md, linux-windowing LW.6; the winit
//! fork's Linux backend moves onto it at LW.7). Each row of [`Xkb`],
//! [`XkbCompose`] and [`XkbX11`] transcribes a prototype of libxkbcommon's
//! `xkbcommon.h`, `xkbcommon-compose.h` and `xkbcommon-x11.h`: the 26
//! keymap, state and keysym calls, the 9 compose calls and the 4 X11 calls
//! that aterm's key handling and its tests make. The C enums are their
//! `int`-sized values as `u32` constants.
//!
//! The libraries are `libxkbcommon.so.0` and `libxkbcommon-x11.so.0`, each
//! opened once per process and never closed.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

pub mod keysym;

/// `struct xkb_context`, opaque.
pub type XkbContext = c_void;
/// `struct xkb_keymap`, opaque.
pub type XkbKeymap = c_void;
/// `struct xkb_state`, opaque.
pub type XkbState = c_void;
/// `struct xkb_compose_table`, opaque.
pub type XkbComposeTable = c_void;
/// `struct xkb_compose_state`, opaque.
pub type XkbComposeState = c_void;
/// `xcb_connection_t`, opaque.
pub type XcbConnection = c_void;

/// `xkb_keysym_t`.
pub type Keysym = u32;
/// `xkb_keycode_t`.
pub type Keycode = u32;
/// `xkb_layout_index_t`.
pub type LayoutIndex = u32;
/// `xkb_level_index_t`.
pub type LevelIndex = u32;
/// `xkb_mod_index_t`.
pub type ModIndex = u32;
/// `xkb_mod_mask_t`.
pub type ModMask = u32;

/// `XKB_KEYCODE_INVALID`, `XKB_LAYOUT_INVALID`, `XKB_MOD_INVALID`: the
/// invalid index of each kind.
pub const INVALID: u32 = 0xffff_ffff;
/// `XKB_KEY_NoSymbol`.
pub const NO_SYMBOL: Keysym = 0;

/// The modifier names (`XKB_MOD_NAME_*`).
pub mod mod_name {
    use std::ffi::CStr;
    pub const SHIFT: &CStr = c"Shift";
    pub const CAPS: &CStr = c"Lock";
    pub const CTRL: &CStr = c"Control";
    pub const ALT: &CStr = c"Mod1";
    pub const NUM: &CStr = c"Mod2";
    pub const LOGO: &CStr = c"Mod4";
}

/// `enum xkb_context_flags`.
pub mod context_flags {
    pub const NO_FLAGS: u32 = 0;
    pub const NO_DEFAULT_INCLUDES: u32 = 1 << 0;
    pub const NO_ENVIRONMENT_NAMES: u32 = 1 << 1;
}

/// `enum xkb_keymap_compile_flags`: none is defined.
pub const KEYMAP_COMPILE_NO_FLAGS: u32 = 0;
/// `enum xkb_keymap_format`: `XKB_KEYMAP_FORMAT_TEXT_V1`.
pub const KEYMAP_FORMAT_TEXT_V1: u32 = 1;

/// `enum xkb_key_direction`.
pub mod key_direction {
    pub const UP: u32 = 0;
    pub const DOWN: u32 = 1;
}

/// `enum xkb_keysym_flags`.
pub mod keysym_flags {
    pub const NO_FLAGS: u32 = 0;
    pub const CASE_INSENSITIVE: u32 = 1 << 0;
}

/// `enum xkb_state_component`.
pub mod state_component {
    pub const MODS_DEPRESSED: u32 = 1 << 0;
    pub const MODS_LATCHED: u32 = 1 << 1;
    pub const MODS_LOCKED: u32 = 1 << 2;
    pub const MODS_EFFECTIVE: u32 = 1 << 3;
    pub const LAYOUT_DEPRESSED: u32 = 1 << 4;
    pub const LAYOUT_LATCHED: u32 = 1 << 5;
    pub const LAYOUT_LOCKED: u32 = 1 << 6;
    pub const LAYOUT_EFFECTIVE: u32 = 1 << 7;
    pub const LEDS: u32 = 1 << 8;
}

/// `enum xkb_compose_compile_flags` and `enum xkb_compose_state_flags`: none
/// is defined.
pub const COMPOSE_NO_FLAGS: u32 = 0;

/// `enum xkb_compose_status`.
pub mod compose_status {
    pub const NOTHING: u32 = 0;
    pub const COMPOSING: u32 = 1;
    pub const COMPOSED: u32 = 2;
    pub const CANCELLED: u32 = 3;
}

/// `enum xkb_compose_feed_result`.
pub mod compose_feed {
    pub const IGNORED: u32 = 0;
    pub const ACCEPTED: u32 = 1;
}

/// `enum xkb_x11_setup_xkb_extension_flags`: none is defined.
pub const X11_SETUP_NO_FLAGS: u32 = 0;
/// `XKB_X11_MIN_MAJOR_XKB_VERSION`, `XKB_X11_MIN_MINOR_XKB_VERSION`.
pub const X11_MIN_XKB_VERSION: (u16, u16) = (1, 0);

/// `struct xkb_rule_names`: each field null for the default.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RuleNames {
    pub rules: *const c_char,
    pub model: *const c_char,
    pub layout: *const c_char,
    pub variant: *const c_char,
    pub options: *const c_char,
}

aterm_dl::table! {
    /// libxkbcommon: contexts, keymaps, states and keysyms.
    pub struct Xkb {
        context_new = c"xkb_context_new": unsafe extern "C" fn(u32) -> *mut XkbContext;
        context_unref = c"xkb_context_unref": unsafe extern "C" fn(*mut XkbContext);
        keymap_new_from_names = c"xkb_keymap_new_from_names":
            unsafe extern "C" fn(*mut XkbContext, *const RuleNames, u32) -> *mut XkbKeymap;
        keymap_new_from_string = c"xkb_keymap_new_from_string":
            unsafe extern "C" fn(*mut XkbContext, *const c_char, u32, u32) -> *mut XkbKeymap;
        keymap_new_from_buffer = c"xkb_keymap_new_from_buffer": unsafe extern "C" fn(
            *mut XkbContext,
            *const c_char,
            usize,
            u32,
            u32,
        ) -> *mut XkbKeymap;
        keymap_unref = c"xkb_keymap_unref": unsafe extern "C" fn(*mut XkbKeymap);
        keymap_mod_get_index = c"xkb_keymap_mod_get_index":
            unsafe extern "C" fn(*mut XkbKeymap, *const c_char) -> ModIndex;
        keymap_key_repeats = c"xkb_keymap_key_repeats":
            unsafe extern "C" fn(*mut XkbKeymap, Keycode) -> c_int;
        keymap_key_get_syms_by_level = c"xkb_keymap_key_get_syms_by_level": unsafe extern "C" fn(
            *mut XkbKeymap,
            Keycode,
            LayoutIndex,
            LevelIndex,
            *mut *const Keysym,
        ) -> c_int;
        keymap_num_layouts = c"xkb_keymap_num_layouts":
            unsafe extern "C" fn(*mut XkbKeymap) -> LayoutIndex;
        keymap_layout_get_name = c"xkb_keymap_layout_get_name":
            unsafe extern "C" fn(*mut XkbKeymap, LayoutIndex) -> *const c_char;
        state_new = c"xkb_state_new": unsafe extern "C" fn(*mut XkbKeymap) -> *mut XkbState;
        state_unref = c"xkb_state_unref": unsafe extern "C" fn(*mut XkbState);
        state_update_mask = c"xkb_state_update_mask": unsafe extern "C" fn(
            *mut XkbState,
            ModMask,
            ModMask,
            ModMask,
            LayoutIndex,
            LayoutIndex,
            LayoutIndex,
        ) -> u32;
        state_update_key = c"xkb_state_update_key":
            unsafe extern "C" fn(*mut XkbState, Keycode, u32) -> u32;
        state_key_get_one_sym = c"xkb_state_key_get_one_sym":
            unsafe extern "C" fn(*mut XkbState, Keycode) -> Keysym;
        state_key_get_utf8 = c"xkb_state_key_get_utf8":
            unsafe extern "C" fn(*mut XkbState, Keycode, *mut c_char, usize) -> c_int;
        state_key_get_utf32 = c"xkb_state_key_get_utf32":
            unsafe extern "C" fn(*mut XkbState, Keycode) -> u32;
        state_key_get_layout = c"xkb_state_key_get_layout":
            unsafe extern "C" fn(*mut XkbState, Keycode) -> LayoutIndex;
        state_mod_name_is_active = c"xkb_state_mod_name_is_active":
            unsafe extern "C" fn(*mut XkbState, *const c_char, u32) -> c_int;
        state_serialize_mods = c"xkb_state_serialize_mods":
            unsafe extern "C" fn(*mut XkbState, u32) -> ModMask;
        state_serialize_layout = c"xkb_state_serialize_layout":
            unsafe extern "C" fn(*mut XkbState, u32) -> LayoutIndex;
        keysym_to_utf8 = c"xkb_keysym_to_utf8":
            unsafe extern "C" fn(Keysym, *mut c_char, usize) -> c_int;
        keysym_to_utf32 = c"xkb_keysym_to_utf32": unsafe extern "C" fn(Keysym) -> u32;
        keysym_from_name = c"xkb_keysym_from_name":
            unsafe extern "C" fn(*const c_char, u32) -> Keysym;
        keysym_get_name = c"xkb_keysym_get_name":
            unsafe extern "C" fn(Keysym, *mut c_char, usize) -> c_int;
    }
}

aterm_dl::table! {
    /// libxkbcommon's compose (dead keys and the Compose key), from the same
    /// library.
    pub struct XkbCompose {
        table_new_from_locale = c"xkb_compose_table_new_from_locale":
            unsafe extern "C" fn(*mut XkbContext, *const c_char, u32) -> *mut XkbComposeTable;
        table_unref = c"xkb_compose_table_unref": unsafe extern "C" fn(*mut XkbComposeTable);
        state_new = c"xkb_compose_state_new":
            unsafe extern "C" fn(*mut XkbComposeTable, u32) -> *mut XkbComposeState;
        state_unref = c"xkb_compose_state_unref": unsafe extern "C" fn(*mut XkbComposeState);
        state_feed = c"xkb_compose_state_feed":
            unsafe extern "C" fn(*mut XkbComposeState, Keysym) -> u32;
        state_reset = c"xkb_compose_state_reset": unsafe extern "C" fn(*mut XkbComposeState);
        state_get_status = c"xkb_compose_state_get_status":
            unsafe extern "C" fn(*mut XkbComposeState) -> u32;
        state_get_utf8 = c"xkb_compose_state_get_utf8":
            unsafe extern "C" fn(*mut XkbComposeState, *mut c_char, usize) -> c_int;
        state_get_one_sym = c"xkb_compose_state_get_one_sym":
            unsafe extern "C" fn(*mut XkbComposeState) -> Keysym;
    }
}

aterm_dl::table! {
    /// libxkbcommon-x11: a keymap and state read from an X server's XKB.
    pub struct XkbX11 {
        setup_xkb_extension = c"xkb_x11_setup_xkb_extension": unsafe extern "C" fn(
            *mut XcbConnection,
            u16,
            u16,
            u32,
            *mut u16,
            *mut u16,
            *mut u8,
            *mut u8,
        ) -> c_int;
        get_core_keyboard_device_id = c"xkb_x11_get_core_keyboard_device_id":
            unsafe extern "C" fn(*mut XcbConnection) -> i32;
        keymap_new_from_device = c"xkb_x11_keymap_new_from_device":
            unsafe extern "C" fn(*mut XkbContext, *mut XcbConnection, i32, u32) -> *mut XkbKeymap;
        state_new_from_device = c"xkb_x11_state_new_from_device":
            unsafe extern "C" fn(*mut XkbKeymap, *mut XcbConnection, i32) -> *mut XkbState;
    }
}

static XKB_SONAMES: &[&CStr] = &[c"libxkbcommon.so.0", c"libxkbcommon.so"];
static XKB_X11_SONAMES: &[&CStr] = &[c"libxkbcommon-x11.so.0", c"libxkbcommon-x11.so"];

fn xkb_library() -> Result<&'static aterm_dl::Library, aterm_dl::Error> {
    static LIB: OnceLock<Result<aterm_dl::Library, aterm_dl::Error>> = OnceLock::new();
    LIB.get_or_init(|| aterm_dl::Library::open(XKB_SONAMES))
        .as_ref()
        .map_err(Clone::clone)
}

/// libxkbcommon's core table, loaded once per process.
pub fn xkb() -> Result<&'static Xkb, aterm_dl::Error> {
    static T: OnceLock<Result<Xkb, aterm_dl::Error>> = OnceLock::new();
    T.get_or_init(|| {
        // SAFETY: every row transcribes xkbcommon.h.
        xkb_library().and_then(|lib| unsafe { Xkb::load(lib) })
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// libxkbcommon's compose table, loaded once per process.
pub fn compose() -> Result<&'static XkbCompose, aterm_dl::Error> {
    static T: OnceLock<Result<XkbCompose, aterm_dl::Error>> = OnceLock::new();
    T.get_or_init(|| {
        // SAFETY: every row transcribes xkbcommon-compose.h.
        xkb_library().and_then(|lib| unsafe { XkbCompose::load(lib) })
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// libxkbcommon-x11's table, loaded once per process.
pub fn x11() -> Result<&'static XkbX11, aterm_dl::Error> {
    static T: OnceLock<Result<XkbX11, aterm_dl::Error>> = OnceLock::new();
    T.get_or_init(|| {
        let lib = aterm_dl::Library::open(XKB_X11_SONAMES)?;
        // SAFETY: every row transcribes xkbcommon-x11.h.
        unsafe { XkbX11::load(&lib) }
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// A keysym's text through libxkbcommon (`xkb_keysym_to_utf8`), or `None`
/// for one without text.
pub fn keysym_utf8(xkb: &Xkb, sym: Keysym) -> Option<String> {
    let mut buf = [0u8; 16];
    // SAFETY: a buffer of its own length; libxkbcommon NUL-terminates.
    let n = unsafe { (xkb.keysym_to_utf8)(sym, buf.as_mut_ptr().cast(), buf.len()) };
    let n = usize::try_from(n).ok().filter(|&n| n > 1)?;
    String::from_utf8(buf.get(..n - 1)?.to_vec()).ok()
}
