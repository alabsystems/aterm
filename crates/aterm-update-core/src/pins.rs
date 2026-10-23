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
mod tests {
    use super::*;

    /// A master is a base64 Ed25519 public key (32 bytes, 44 chars with one `=`); this
    #[test]
    fn the_master_is_a_well_formed_base64_ed25519_key() {
        for k in PAPER_MASTER_PUBKEYS {
            assert_eq!(k.len(), 44, "not a 44-char base64 Ed25519 key: {k}");
            assert!(k.ends_with('='), "missing base64 padding: {k}");
            assert!(
                k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='),
                "non-base64 character: {k}"
            );
        }
    }

    /// The Windows anchor is the leaf's subject CN as `signtool verify /v` prints it, never a
    #[test]
    fn the_windows_anchor_is_a_bare_common_name_or_empty() {
        let a = WINDOWS_SIGNING_PUBLISHER;
        assert_eq!(a, a.trim(), "no leading/trailing whitespace");
        assert!(!a.contains('\n') && !a.contains('\r'));
        assert!(
            !a.starts_with("CN=") && !a.contains(", O=") && !a.contains(",O="),
            "commit the CN as signtool prints it, not the distinguished name"
        );
    }

    /// Only an empty slice unpins the master: an empty member would read as armed to
    #[test]
    fn the_master_keyset_has_no_empty_members_and_no_duplicates() {
        for (i, key) in PAPER_MASTER_PUBKEYS.iter().enumerate() {
            assert!(
                !key.is_empty(),
                "PAPER_MASTER_PUBKEYS[{i}] is empty — use an empty SLICE to unpin, never \
                 an empty member"
            );
            assert!(
                !PAPER_MASTER_PUBKEYS[..i].contains(key),
                "duplicate master key: {key}"
            );
        }
        assert!(
            PAPER_MASTER_PUBKEYS.len() <= 2,
            "master keyset has {} members; a rotation window holds at most two",
            PAPER_MASTER_PUBKEYS.len()
        );
    }
}
