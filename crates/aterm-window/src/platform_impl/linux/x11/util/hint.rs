// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/util/hint.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

#[derive(Debug)]
pub enum StateOperation {
    Remove = 0, // _NET_WM_STATE_REMOVE
    Add = 1,    // _NET_WM_STATE_ADD
}

impl From<bool> for StateOperation {
    fn from(op: bool) -> Self {
        if op {
            StateOperation::Add
        } else {
            StateOperation::Remove
        }
    }
}

pub struct MotifHints {
    hints: MwmHints,
}

struct MwmHints {
    flags: u32,
    functions: u32,
    decorations: u32,
    input_mode: u32,
    status: u32,
}

mod mwm {
    // Motif WM hints are obsolete, but still widely supported.
    // https://stackoverflow.com/a/1909708
    //
    // LOCAL PATCH (aterm), 2026-10-21 (WC.8): only the decorations hint is written; the
    // function hints left with `set_enabled_buttons` and `set_resizable` on Linux.
    pub const MWM_HINTS_DECORATIONS: u32 = 1 << 1;
}

impl MotifHints {
    pub fn new() -> MotifHints {
        MotifHints {
            hints: MwmHints { flags: 0, functions: 0, decorations: 0, input_mode: 0, status: 0 },
        }
    }

    pub fn set_decorations(&mut self, decorations: bool) {
        self.hints.flags |= mwm::MWM_HINTS_DECORATIONS;
        self.hints.decorations = decorations as u32;
    }
}

impl Default for MotifHints {
    fn default() -> Self {
        Self::new()
    }
}

impl XConnection {
    pub fn get_motif_hints(&self, window: xproto::Window) -> MotifHints {
        let atoms = self.atoms();
        let motif_hints = atoms[_MOTIF_WM_HINTS];

        let mut hints = MotifHints::new();

        if let Ok(props) = self.get_property::<u32>(window, motif_hints, motif_hints) {
            hints.hints.flags = props.first().cloned().unwrap_or(0);
            hints.hints.functions = props.get(1).cloned().unwrap_or(0);
            hints.hints.decorations = props.get(2).cloned().unwrap_or(0);
            hints.hints.input_mode = props.get(3).cloned().unwrap_or(0);
            hints.hints.status = props.get(4).cloned().unwrap_or(0);
        }

        hints
    }

    #[allow(clippy::unnecessary_cast)]
    pub fn set_motif_hints(
        &self,
        window: xproto::Window,
        hints: &MotifHints,
    ) -> Result<VoidCookie<'_>, X11Error> {
        let atoms = self.atoms();
        let motif_hints = atoms[_MOTIF_WM_HINTS];

        let hints_data: [u32; 5] = [
            hints.hints.flags as u32,
            hints.hints.functions as u32,
            hints.hints.decorations as u32,
            hints.hints.input_mode as u32,
            hints.hints.status as u32,
        ];

        self.change_property(
            window,
            motif_hints,
            motif_hints,
            xproto::PropMode::Replace,
            &hints_data,
        )
    }
}
