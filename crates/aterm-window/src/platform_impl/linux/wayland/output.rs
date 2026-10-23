// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/wayland/output.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).
//! Modified by the aterm project in 2026; see the repository NOTICE. (A monitor is a
//! first-party `aterm_wayland::output::Output`, read as of its last `done`, where upstream read
//! smithay-client-toolkit's `OutputData`: docs/AUDIT-third-party-2026-10-15.md, LW.7. Its
//! position is `wl_output.geometry`'s, not xdg-output's logical one. aterm-gui reads a
//! monitor's position only to place a restored window and to cascade a new one, and on Wayland
//! both only feed `set_outer_position`, which a Wayland client cannot do.)

use aterm_wayland::output::{Output, OutputInfo};

use crate::dpi::{LogicalPosition, PhysicalPosition, PhysicalSize};

use super::event_loop::ActiveEventLoop;

impl ActiveEventLoop {
    #[inline]
    pub fn available_monitors(&self) -> impl Iterator<Item = MonitorHandle> {
        self.state.borrow().outputs.outputs().into_iter().map(MonitorHandle::new)
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct MonitorHandle {
    pub(crate) output: Output,
}

impl MonitorHandle {
    #[inline]
    pub(crate) fn new(output: Output) -> Self {
        Self { output }
    }

    fn info(&self) -> OutputInfo {
        self.output.info().unwrap_or_else(|| OutputInfo {
            global: self.output.global(),
            scale: 1,
            ..OutputInfo::default()
        })
    }

    #[inline]
    pub fn native_identifier(&self) -> u32 {
        self.output.global()
    }

    #[inline]
    pub fn size(&self) -> PhysicalSize<u32> {
        match self.info().current_mode() {
            Some(mode) => (mode.width as u32, mode.height as u32),
            _ => (0, 0),
        }
        .into()
    }

    #[inline]
    pub fn position(&self) -> PhysicalPosition<i32> {
        let info = self.info();
        LogicalPosition::<i32>::from((info.x, info.y)).to_physical(info.scale as f64)
    }

    #[inline]
    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        self.info().current_mode().map(|mode| mode.refresh as u32)
    }

    #[inline]
    pub fn scale_factor(&self) -> i32 {
        self.info().scale
    }
}

impl PartialEq for MonitorHandle {
    fn eq(&self, other: &Self) -> bool {
        self.native_identifier() == other.native_identifier()
    }
}

impl Eq for MonitorHandle {}

impl PartialOrd for MonitorHandle {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MonitorHandle {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.native_identifier().cmp(&other.native_identifier())
    }
}

impl std::hash::Hash for MonitorHandle {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.native_identifier().hash(state);
    }
}
