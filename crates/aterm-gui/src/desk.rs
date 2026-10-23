// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-instance desk persistence and restore policy across window, headless, and lab runs.

use std::sync::OnceLock;

/// Development seam that gives a headless instance under a private state root a window's desk.
pub(crate) const HEADLESS_RESTORE_SEAM: &str = "ATERM_DEBUG_HEADLESS_RESTORE";

/// Desk persistence and restore policy for this instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Desk {
    /// Windowed instance: persists layout and crash journals and relaunches restored agents.
    Window,
    /// Headless instance: keeps no desk state.
    Headless,
    /// Headless instance under [`HEADLESS_RESTORE_SEAM`]: keeps a window's desk without glass wait.
    LabHeadless,
}

impl Desk {
    /// Resolves this process's desk policy from `headless` and [`HEADLESS_RESTORE_SEAM`].
    pub(crate) fn of(headless: bool) -> Self {
        static SEAM: OnceLock<bool> = OnceLock::new();
        Self::new(
            headless,
            *SEAM.get_or_init(|| {
                aterm_types::dev_seam!(HEADLESS_RESTORE_SEAM).is_some()
                    && aterm_types::dirs::runs_under_private_state_root()
            }),
        )
    }

    const fn new(headless: bool, seam: bool) -> Self {
        match (headless, seam) {
            (false, _) => Self::Window,
            (true, false) => Self::Headless,
            (true, true) => Self::LabHeadless,
        }
    }

    /// Returns whether this instance persists layout, claims journals, and relaunches agents.
    pub(crate) const fn kept(self) -> bool {
        !matches!(self, Self::Headless)
    }

    /// Returns whether the first frame counts as presented for restore and journal capture.
    pub(crate) const fn on_glass(self, first_present_done: bool) -> bool {
        first_present_done || matches!(self, Self::LabHeadless)
    }

    /// Returns whether a quit layout preserves running agents.
    pub(crate) const fn quit_keeps_agents(self, system_asked: bool) -> bool {
        system_asked || matches!(self, Self::LabHeadless)
    }
}

#[cfg(test)]
mod tests {
    use super::Desk;

    #[test]
    fn without_the_seam_every_gate_is_todays() {
        for headless in [false, true] {
            let desk = Desk::new(headless, false);
            assert_ne!(desk, Desk::LabHeadless);
            assert_eq!(desk.kept(), !headless);
            for flag in [false, true] {
                assert_eq!(desk.on_glass(flag), flag);
                assert_eq!(desk.quit_keeps_agents(flag), flag);
            }
        }
    }

    #[test]
    fn the_seam_reaches_a_headless_instance_only() {
        assert_eq!(Desk::new(false, true), Desk::Window);
        let lab = Desk::new(true, true);
        assert_eq!(lab, Desk::LabHeadless);
        assert!(lab.kept());
        assert!(lab.on_glass(false));
        assert!(lab.quit_keeps_agents(false));
    }
}
