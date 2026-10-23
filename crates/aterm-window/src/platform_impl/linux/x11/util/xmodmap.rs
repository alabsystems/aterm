// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/xmodmap.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::collections::HashSet;
use std::slice;

use super::ffi::{KeyCode as XKeyCode, XModifierKeymap};

const NUM_MODS: usize = 8;

/// Track which keys are modifiers, so we can properly replay them when they were filtered.
#[derive(Debug, Default)]
pub struct ModifierKeymap {
    modifiers: HashSet<XKeyCode>,
}

impl ModifierKeymap {
    pub fn new() -> ModifierKeymap {
        ModifierKeymap::default()
    }

    pub fn is_modifier(&self, keycode: XKeyCode) -> bool {
        self.modifiers.contains(&keycode)
    }

    pub fn reload_from_x_connection(&mut self, xconn: &super::XConnection) {
        unsafe {
            let keymap = (xconn.xlib.XGetModifierMapping)(xconn.display);

            if keymap.is_null() {
                return;
            }

            self.reset_from_x_keymap(&*keymap);

            (xconn.xlib.XFreeModifiermap)(keymap);
        }
    }

    fn reset_from_x_keymap(&mut self, keymap: &XModifierKeymap) {
        let keys_per_mod = keymap.max_keypermod as usize;

        let keys = unsafe {
            slice::from_raw_parts(keymap.modifiermap as *const _, keys_per_mod * NUM_MODS)
        };
        self.modifiers.clear();
        for key in keys {
            self.modifiers.insert(*key);
        }
    }
}
