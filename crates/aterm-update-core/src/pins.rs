// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
/// Ed25519 public key(s) of the paper master: the offline root of the machine roster and the
/// one anchor that authorizes a release.
///
/// Do not hand-edit the value or its layout: `atpkg-keys` writes this declaration and reads it
/// back in this shape (`pins_edit::read_anchor`).
pub const PAPER_MASTER_PUBKEYS: &[&str] = &[
    // The paper master, armed by `atpkg-keys setup --id m3`.
    "DtiLfpk0iUSrK1/LkyIVf+4C2eGjD2Myf4Sr/FCoMPQ=",
];

/// Whether the paper-master roster tier is armed for this build.
#[inline]
#[must_use]
pub const fn roster_tier_armed() -> bool {
    !PAPER_MASTER_PUBKEYS.is_empty()
}

/// Apple Developer Team ID for the optional Tier APPLE anchor.
pub const APPLE_TEAM_ID: &str = "A66A9P66Z7";

/// Windows code-signing publisher CN for the optional Tier WINDOWS anchor.
pub const WINDOWS_SIGNING_PUBLISHER: &str = "";

/// Whether an anchor string is active (non-empty).
#[inline]
#[must_use]
pub const fn anchor_active(anchor: &str) -> bool {
    !anchor.is_empty()
}

const _: () = assert!(!anchor_active("") && anchor_active("any-nonempty-value"));

#[cfg(test)]
#[path = "pins_tests.rs"]
mod tests;
