// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Portable presentation values for macOS bundle posture: no path or bundle probes and no
//! replacement authority. Probing stays in `bundle`; any host's messages can render these.

/// Where a running copy is installed from, kept as a reason. `bundle::resolve_from`
/// answers only "may I replace this?", and its "no" is the same for a dev build and for a
/// download left in `~/Downloads`, though only the latter is the user's to fix.
/// `bundle::posture_from` classifies from the path alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPosture {
    /// Replaceable in place: self-update works and the shell shims go beside it.
    Installed,
    /// Running from a mounted volume (`/Volumes/…`): usually opened inside the DMG
    /// window, but an install on a second drive looks the same; neither is replaced.
    MountedImage,
    /// Gatekeeper runs this copy from a randomized read-only path (App Translocation):
    /// a quarantined app opened without being moved, typically from `~/Downloads`.
    Translocated,
    /// Not a `…/<name>.app/Contents/MacOS/<exe>` layout: a plain dev binary such as
    /// `target/release/…`. Nothing is wrong; there is no bundle.
    NotABundle,
}

impl InstallPosture {
    /// One line, in the user's terms. No trailing period: callers put it in a sentence.
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            Self::Installed => "aterm is installed and can keep itself up to date",
            Self::MountedImage => "aterm is running from a mounted volume, not from Applications",
            Self::Translocated => {
                "macOS is running aterm from a temporary read-only copy, because it was opened \
                 without being moved first"
            }
            Self::NotABundle => "aterm is running as a plain binary, not from an app bundle",
        }
    }

    /// What the person should do, or `None` when there is nothing to fix. It names both
    /// consequences, which are otherwise invisible: no self-update and no `aterm` on PATH.
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
