// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/common/xkb/state.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

//! Modified by the aterm project in 2026; see the repository NOTICE. (`SmolStr` from
//! `crate::keyboard`. libxkbcommon's entry points are first-party aterm-xkb's, where upstream
//! used xkbcommon-dl's: docs/AUDIT-third-party-2026-10-15.md, LW.7.)
//!
//! XKB state.

use std::ffi::CStr;
use std::ptr::NonNull;

use crate::keyboard::SmolStr;
#[cfg(x11_platform)]
use aterm_xkb::XcbConnection as xcb_connection_t;
use aterm_xkb::{
    self as xkb, mod_name, state_component, Keycode as xkb_keycode_t, Keysym as xkb_keysym_t,
    LayoutIndex as xkb_layout_index_t, XkbState as xkb_state,
};

use crate::platform_impl::common::xkb::keymap::XkbKeymap;
#[cfg(x11_platform)]
use crate::platform_impl::common::xkb::xkbxh;
use crate::platform_impl::common::xkb::{make_string_with, xkbh};

#[derive(Debug)]
pub struct XkbState {
    state: NonNull<xkb_state>,
    modifiers: ModifiersState,
}

impl XkbState {
    #[cfg(wayland_platform)]
    pub fn new_wayland(keymap: &XkbKeymap) -> Option<Self> {
        let state = NonNull::new(unsafe { (xkbh().state_new)(keymap.as_ptr()) })?;
        Some(Self::new_inner(state))
    }

    #[cfg(x11_platform)]
    pub fn new_x11(xcb: *mut xcb_connection_t, keymap: &XkbKeymap) -> Option<Self> {
        let state = unsafe {
            (xkbxh().state_new_from_device)(keymap.as_ptr(), xcb, keymap._core_keyboard_id)
        };
        let state = NonNull::new(state)?;
        Some(Self::new_inner(state))
    }

    fn new_inner(state: NonNull<xkb_state>) -> Self {
        let modifiers = ModifiersState::default();
        let mut this = Self { state, modifiers };
        this.reload_modifiers();
        this
    }

    pub fn get_one_sym_raw(&mut self, keycode: xkb_keycode_t) -> xkb_keysym_t {
        unsafe { (xkbh().state_key_get_one_sym)(self.state.as_ptr(), keycode) }
    }

    pub fn layout(&mut self, key: xkb_keycode_t) -> xkb_layout_index_t {
        unsafe { (xkbh().state_key_get_layout)(self.state.as_ptr(), key) }
    }

    #[cfg(x11_platform)]
    pub fn depressed_modifiers(&mut self) -> xkb::ModMask {
        unsafe {
            (xkbh().state_serialize_mods)(self.state.as_ptr(), state_component::MODS_DEPRESSED)
        }
    }

    #[cfg(x11_platform)]
    pub fn latched_modifiers(&mut self) -> xkb::ModMask {
        unsafe { (xkbh().state_serialize_mods)(self.state.as_ptr(), state_component::MODS_LATCHED) }
    }

    #[cfg(x11_platform)]
    pub fn locked_modifiers(&mut self) -> xkb::ModMask {
        unsafe { (xkbh().state_serialize_mods)(self.state.as_ptr(), state_component::MODS_LOCKED) }
    }

    pub fn get_utf8_raw(
        &mut self,
        keycode: xkb_keycode_t,
        scratch_buffer: &mut Vec<u8>,
    ) -> Option<SmolStr> {
        make_string_with(scratch_buffer, |ptr, len| unsafe {
            (xkbh().state_key_get_utf8)(self.state.as_ptr(), keycode, ptr, len)
        })
    }

    pub fn modifiers(&self) -> ModifiersState {
        self.modifiers
    }

    pub fn update_modifiers(
        &mut self,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        depressed_group: u32,
        latched_group: u32,
        locked_group: u32,
    ) {
        let mask = unsafe {
            (xkbh().state_update_mask)(
                self.state.as_ptr(),
                mods_depressed,
                mods_latched,
                mods_locked,
                depressed_group,
                latched_group,
                locked_group,
            )
        };

        if mask & state_component::MODS_EFFECTIVE != 0 {
            // Effective value of mods have changed, we need to update our state.
            self.reload_modifiers();
        }
    }

    /// Reload the modifiers.
    fn reload_modifiers(&mut self) {
        self.modifiers.ctrl = self.mod_name_is_active(mod_name::CTRL);
        self.modifiers.alt = self.mod_name_is_active(mod_name::ALT);
        self.modifiers.shift = self.mod_name_is_active(mod_name::SHIFT);
        self.modifiers.caps_lock = self.mod_name_is_active(mod_name::CAPS);
        self.modifiers.logo = self.mod_name_is_active(mod_name::LOGO);
        self.modifiers.num_lock = self.mod_name_is_active(mod_name::NUM);
    }

    /// Check if the modifier is active within xkb.
    fn mod_name_is_active(&mut self, name: &CStr) -> bool {
        unsafe {
            (xkbh().state_mod_name_is_active)(
                self.state.as_ptr(),
                name.as_ptr(),
                state_component::MODS_EFFECTIVE,
            ) > 0
        }
    }
}

impl Drop for XkbState {
    fn drop(&mut self) {
        unsafe {
            (xkbh().state_unref)(self.state.as_ptr());
        }
    }
}

/// Represents the current state of the keyboard modifiers
///
/// Each field of this struct represents a modifier and is `true` if this modifier is active.
///
/// For some modifiers, this means that the key is currently pressed, others are toggled
/// (like caps lock).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ModifiersState {
    /// The "control" key
    pub ctrl: bool,
    /// The "alt" key
    pub alt: bool,
    /// The "shift" key
    pub shift: bool,
    /// The "Caps lock" key
    pub caps_lock: bool,
    /// The "logo" key
    ///
    /// Also known as the "windows" key on most keyboards
    pub logo: bool,
    /// The "Num lock" key
    pub num_lock: bool,
}

impl From<ModifiersState> for crate::keyboard::ModifiersState {
    fn from(mods: ModifiersState) -> crate::keyboard::ModifiersState {
        let mut to_mods = crate::keyboard::ModifiersState::empty();
        to_mods.set(crate::keyboard::ModifiersState::SHIFT, mods.shift);
        to_mods.set(crate::keyboard::ModifiersState::CONTROL, mods.ctrl);
        to_mods.set(crate::keyboard::ModifiersState::ALT, mods.alt);
        to_mods.set(crate::keyboard::ModifiersState::SUPER, mods.logo);
        to_mods
    }
}
