// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Portable presentation values for macOS bundle posture. This module performs
//! no path or bundle probes and grants no replacement authority; native probing
//! remains in `bundle`, while all-host message builders can render these facts.

/// WHERE a running copy is installed from, and therefore what it can and cannot
/// do for the person using it. `resolve_from` answers the updater's question —
/// "may I replace this?" — as a bare `Option`, which is the right shape for the
/// updater and the wrong shape for a human: "no" is the same value whether the
/// copy is a dev build or a download the user never moved out of their Downloads
/// folder, and only one of those is something they can fix.
///
/// This is that same judgement, kept as a REASON. It is what a first-open doctor
/// reports, and it is deliberately pure — the classification is a function of the
/// path alone, so every case below is a unit test rather than a situation someone
/// has to reproduce by hand on a fresh Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPosture {
    /// A normal install. The bundle can be replaced in place, so self-update
    /// works and the shell shims can be written beside it.
    Installed,
    /// Running straight out of a mounted disk image (`/Volumes/…`) — the user
    /// double-clicked `aterm.app` inside the DMG window instead of dragging it
    /// to Applications. The volume is read-only and disappears on eject.
    MountedImage,
    /// Gatekeeper is running this copy from a randomized, read-only path (App
    /// Translocation), which is what happens to a quarantined app launched
    /// WITHOUT being moved first — typically unzipped and opened from
    /// `~/Downloads`. The copy the user can see is not the copy that is running.
    Translocated,
    /// Not a `…/<name>.app/Contents/MacOS/<exe>` layout at all: a `cargo run` or
    /// a `target/release` binary. Nothing is wrong; there is simply no bundle.
    NotABundle,
}

impl InstallPosture {
    /// Whether this copy can replace itself — the updater's question, answered
    /// from the same classification the human-facing text uses, so the two can
    /// never drift into disagreeing about the same install.
    #[must_use]
    pub fn can_update(self) -> bool {
        matches!(self, Self::Installed)
    }

    /// Whether moving the app to `/Applications` is what fixes it. False for a
    /// dev build, where nothing is broken and the advice would be wrong.
    #[must_use]
    pub fn wants_move_to_applications(self) -> bool {
        matches!(self, Self::MountedImage | Self::Translocated)
    }

    /// One line naming what is true, in the user's terms rather than the
    /// updater's. No trailing period: callers put this in a sentence.
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::Installed => "aterm is installed and can keep itself up to date",
            Self::MountedImage => "aterm is running from the disk image, not from Applications",
            Self::Translocated => {
                "macOS is running aterm from a temporary read-only copy, because it was opened \
                 without being moved first"
            }
            Self::NotABundle => "aterm is running as a plain binary, not from an app bundle",
        }
    }

    /// What the person should DO, or `None` when there is nothing to fix. The
    /// two consequences are named because they are both invisible otherwise:
    /// the copy silently cannot update itself, and it silently does not put
    /// `aterm` on the PATH of a new shell.
    #[must_use]
    pub fn remedy(self) -> Option<&'static str> {
        match self {
            Self::Installed | Self::NotABundle => None,
            Self::MountedImage => Some(
                "Drag aterm to Applications and open it from there. Until then it cannot update \
                 itself, and it cannot add `aterm` to your PATH.",
            ),
            Self::Translocated => Some(
                "Move aterm into Applications and open it again. Until then it cannot update \
                 itself, and it cannot add `aterm` to your PATH.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::InstallPosture;

    #[test]
    fn portable_posture_values_preserve_bundle_advice() {
        for (posture, can_update, move_app) in [
            (InstallPosture::Installed, true, false),
            (InstallPosture::MountedImage, false, true),
            (InstallPosture::Translocated, false, true),
            (InstallPosture::NotABundle, false, false),
        ] {
            assert_eq!(posture.can_update(), can_update);
            assert_eq!(posture.wants_move_to_applications(), move_app);
            assert_eq!(posture.remedy().is_some(), move_app);
            assert!(!posture.summary().is_empty());
            assert!(!posture.summary().ends_with('.'));
            if let Some(remedy) = posture.remedy() {
                assert!(remedy.contains("Applications"));
                assert!(remedy.contains("update itself"));
                assert!(remedy.contains("PATH"));
            }
        }
    }
}
