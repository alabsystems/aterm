// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What an instance keeps of its desk across launches: the quit's layout
//! (`session.toml`), the crash journal and its claim at the next launch, the
//! deferred restore pass, and the supervisor host's relaunch of the agents a
//! restore hands it. A window keeps all of it; a headless instance keeps none.
//!
//! Under the [`HEADLESS_RESTORE_SEAM`] development seam ([`aterm_types::dev_seam!`]),
//! which the real-Claude lab (`crates/aterm-lab`) uses to check a cold restore
//! against a real Claude Code, a headless instance keeps its desk as a window does,
//! except that nothing waits for a first present and every quit keeps its agents (a
//! logout's Apple Event, [`crate::system_quit`], never reaches a headless instance).
//! A shipped binary reads no seam.

use std::sync::OnceLock;

/// The development seam that gives a headless instance a window's desk
/// ([`Desk::LabHeadless`]). Any value turns it on; a window ignores it.
pub(crate) const HEADLESS_RESTORE_SEAM: &str = "ATERM_DEBUG_HEADLESS_RESTORE";

/// What this instance keeps of its desk ([module docs](self)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Desk {
    /// A window's: the quit writes its layout (with its agents only when the
    /// system asked for the quit), a crash journal is kept and claimed, the
    /// restore pass waits for the first present, and restored agents are
    /// relaunched.
    Window,
    /// A headless instance's: none of it.
    Headless,
    /// A headless instance under [`HEADLESS_RESTORE_SEAM`]: a window's, with
    /// no first present to wait for and every quit keeping its agents.
    LabHeadless,
}

impl Desk {
    /// This process's desk: `headless` is `--headless`; the seam is read once per
    /// process and counts only under a private state root
    /// ([`aterm_types::dirs::runs_under_private_state_root`]), since otherwise a
    /// headless instance would consume and overwrite the person's own `session.toml`
    /// and crash journals.
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

    /// The desk of an instance that is `headless` or not, with the seam `seam`.
    const fn new(headless: bool, seam: bool) -> Self {
        match (headless, seam) {
            (false, _) => Self::Window,
            (true, false) => Self::Headless,
            (true, true) => Self::LabHeadless,
        }
    }

    /// Whether this instance keeps a desk at all: its quit writes the layout,
    /// it journals and claims crash journals, and its host relaunches the
    /// agents a restore hands it. Without the seam: `!headless`.
    pub(crate) const fn kept(self) -> bool {
        !matches!(self, Self::Headless)
    }

    /// Whether the first frame counts as on glass for the restore pass and the
    /// journal's capture: `first_present_done`, or always where there is no
    /// glass to wait for.
    pub(crate) const fn on_glass(self, first_present_done: bool) -> bool {
        first_present_done || matches!(self, Self::LabHeadless)
    }

    /// Whether the quit's layout carries the agents the desk holds:
    /// `system_asked` ([`crate::system_quit::asked`]), or always under the seam.
    pub(crate) const fn quit_keeps_agents(self, system_asked: bool) -> bool {
        system_asked || matches!(self, Self::LabHeadless)
    }
}

#[cfg(test)]
mod tests {
    use super::Desk;

    /// Without the seam each gate reduces to `!headless`, `first_present_done` and
    /// `system_quit::asked()`, for every input.
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

    /// The seam gives a headless instance a window's desk, without the glass
    /// and with every quit keeping its agents — and never touches a window.
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
