// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/wm.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use std::sync::Mutex;

use aterm_x11::proto::atom;

use super::*;

pub const MOVERESIZE_TOPLEFT: isize = 0;
pub const MOVERESIZE_TOP: isize = 1;
pub const MOVERESIZE_TOPRIGHT: isize = 2;
pub const MOVERESIZE_RIGHT: isize = 3;
pub const MOVERESIZE_BOTTOMRIGHT: isize = 4;
pub const MOVERESIZE_BOTTOM: isize = 5;
pub const MOVERESIZE_BOTTOMLEFT: isize = 6;
pub const MOVERESIZE_LEFT: isize = 7;
pub const MOVERESIZE_MOVE: isize = 8;

static SUPPORTED_HINTS: Mutex<Vec<xproto::Atom>> = Mutex::new(Vec::new());
static WM_NAME: Mutex<Option<String>> = Mutex::new(None);

pub fn hint_is_supported(hint: xproto::Atom) -> bool {
    (*SUPPORTED_HINTS.lock().unwrap()).contains(&hint)
}

pub fn wm_name_is_one_of(names: &[&str]) -> bool {
    if let Some(ref name) = *WM_NAME.lock().unwrap() {
        names.contains(&name.as_str())
    } else {
        false
    }
}

impl XConnection {
    pub fn update_cached_wm_info(&self, root: xproto::Window) {
        *SUPPORTED_HINTS.lock().unwrap() = self.get_supported_hints(root);
        *WM_NAME.lock().unwrap() = self.get_wm_name(root);
    }

    fn get_supported_hints(&self, root: xproto::Window) -> Vec<xproto::Atom> {
        let atoms = self.atoms();
        let supported_atom = atoms[_NET_SUPPORTED];
        self.get_property(root, supported_atom, atom::ATOM)
            .unwrap_or_else(|_| Vec::with_capacity(0))
    }

    #[allow(clippy::useless_conversion)]
    fn get_wm_name(&self, root: xproto::Window) -> Option<String> {
        let atoms = self.atoms();
        let check_atom = atoms[_NET_SUPPORTING_WM_CHECK];
        let wm_name_atom = atoms[_NET_WM_NAME];

        let root_window_wm_check = {
            let result = self.get_property::<xproto::Window>(root, check_atom, atom::WINDOW);

            let wm_check = result.ok().and_then(|wm_check| wm_check.first().cloned());

            wm_check?
        };

        let child_window_wm_check = {
            let result = self.get_property::<xproto::Window>(
                root_window_wm_check.into(),
                check_atom,
                atom::WINDOW,
            );

            let wm_check = result.ok().and_then(|wm_check| wm_check.first().cloned());

            wm_check?
        };

        if root_window_wm_check != child_window_wm_check {
            return None;
        }

        let wm_name = {
            let atoms = self.atoms();
            let utf8_string_atom = atoms[UTF8_STRING];

            let result =
                self.get_property(root_window_wm_check.into(), wm_name_atom, utf8_string_atom);

            let no_utf8 = if let Err(ref err) = result {
                err.is_actual_property_type(atom::STRING)
            } else {
                false
            };

            if no_utf8 {
                self.get_property(root_window_wm_check.into(), wm_name_atom, atom::STRING)
            } else {
                result
            }
        }
        .ok();

        wm_name.and_then(|wm_name| String::from_utf8(wm_name).ok())
    }
}
