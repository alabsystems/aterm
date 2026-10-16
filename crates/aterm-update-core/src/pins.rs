// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The trust anchors, all in one file.
//!
//! Changing a value here is a reviewed commit: no environment variable, per-machine file or
//! build flag changes what a build trusts, so the source and its history say it. An empty
//! anchor is unpinned and inert: with nothing compiled in there is nothing to trust. A fork or
//! private channel commits its own anchors here.
//!
//! | Anchor | Signs | Where its secret half lives |
//! |---|---|---|
//! | [`PAPER_MASTER_PUBKEYS`] | the machine roster, which authorizes release manifests and atpkg's `index.toml` | on paper, on no computer |
//! | [`APPLE_TEAM_ID`] | (not a key) the optional Developer-ID tier | Apple |
//! | [`WINDOWS_SIGNING_PUBLISHER`] | (not a key) the optional Windows code-signing tier | the signing service |
//!
//! [`PAPER_MASTER_PUBKEYS`] is the one trust root: it signs the machine roster, and a rostered
//! machine signs the release appcast, atpkg's `index.toml` and every `pkg-*.toml`. The
//! delegation buys revocation because the root is elsewhere: a stolen laptop yields a machine
//! key the master can revoke. See `docs/RELEASE-KEYS.md`.
//!
//! `crates/aterm-gui/build.rs` compiles this file by `#[path]` to derive the Mach-O
//! `__aterm_upin` record from [`PAPER_MASTER_PUBKEYS`]. Keep it self-contained: no `use` and no
//! `crate::` path outside `#[cfg(test)]`.

/// Ed25519 public key(s) of the paper master: the offline root of the machine roster and the
/// one anchor that authorizes a release.
///
/// The master signs only `aterm-machines.toml`, the roster naming which machines may sign
/// releases and which are revoked (`roster`). Its secret half is 52 base32 characters on paper,
/// on no computer, typed in only to mint or revoke a machine key. A client authorizes a release
/// by the roster alone: also accepting some compiled-in key would let a revoked machine keep
/// publishing to every build that carried it.
///
/// An empty slice (a fork with no master of its own) removes the signature tier:
/// `roster::verify_roster` returns `Disabled` and trust is the channel repository over TLS, the
/// manifest sha256 and the codesign/Team-ID checks. An empty member is never legal: it reads
/// as armed and authorizes nobody.
///
/// A client accepts any member, so a planned rotation is append, wait out adoption, drop. A
/// lost paper is remedied only by a new binary carrying a new pin. Test vectors never land
/// here: any value in this file is the live root of trust.
///
/// Do not hand-edit the value: `atpkg-keys` writes it and re-reads the file to prove it landed.
///
/// 1. `atpkg-keys setup --id <id>`, once ever: generates the master, shows its phrase once on
///    `/dev/tty` and demands it retyped, writes the public key here, mints this machine's key
///    and creates the roster. It refuses if a master is already committed.
/// 2. Review and commit; the tool only edits the working tree.
/// 3. `atpkg-keys join --id <id>` on each later publishing machine (copy `aterm-machines.toml`
///    and its `.sig` there first): proves the phrase, mints locally, re-signs the roster.
/// 4. `atpkg-keys machine-revoke --id <id>` bumps `roster_seq` and re-signs the roster.
///
/// `roster_seq` is monotonic: a client refuses a generation below the highest it has seen
/// (`RosterReject::Rollback`), and `aterm_release::publish::roster_floor_covered` refuses a cut
/// carrying an older roster than the channel head's. The roster lives in gitignored `dist/`,
/// so after any `join` or `machine-revoke` copy it to every publishing machine before the next
/// cut.
pub const PAPER_MASTER_PUBKEYS: &[&str] = &[
    // The paper master, armed by `atpkg-keys setup --id m3`.
    "DtiLfpk0iUSrK1/LkyIVf+4C2eGjD2Myf4Sr/FCoMPQ=",
];

/// Whether the paper-master roster tier is armed for this build. An unpinned master
/// authorizes nothing, so callers skip the tier rather than read an absent roster as
/// permission.
#[must_use]
pub const fn roster_tier_armed() -> bool {
    !PAPER_MASTER_PUBKEYS.is_empty()
}

/// Apple Developer Team ID for the optional Tier APPLE anchor.
///
/// Empty skips the codesign/notarization anchor and leaves signature and hash verification
/// intact. A non-empty value is the whole switch; with no other code change it arms:
///
/// * the release pipeline's Developer-ID sign, notarize, staple and verify, or the cut fails
///   (`aterm-release/src/sign.rs`);
/// * the cut's self-check of `TeamIdentifier=`, a stapled ticket and a Gatekeeper pass on both
///   the `.app` and the `.dmg`;
/// * the in-app updater's refusal of any update not Developer-ID signed by this team
///   (`aterm-update`'s `PINNED_TEAM_ID` → `verify_bundle_policy`);
/// * `tools/install.sh`'s `subject.OU = "<TEAM>"` designated requirement.
///
/// The identity string (`Developer ID Application: <name> (<TEAMID>)`) is written nowhere: the
/// pipeline matches this anchor against `security find-identity -v -p codesigning`, so this
/// constant is the only place a Team ID appears in the tree.
///
/// Arming is a one-way door: a shipped binary with a non-empty value refuses every future
/// update not Developer-ID signed by this team, and clearing the value in source does not reach
/// it. It commits the owner to keeping the Developer Program membership and certificate alive
/// for as long as pinned clients exist.
///
/// # Arming Tier APPLE
///
/// 1. Install a **Developer ID Application** certificate and its private key in the login
///    keychain of the cutting machine. `security find-identity -v -p codesigning` should show
///    exactly one `Developer ID Application: <you> (<TEAMID>)` line; with two (a renewal
///    overlapping the incumbent), delete one or pin one in step 3. The pipeline refuses to
///    guess.
///
/// 2. **Store the notarytool credential once, outside the repo:**
///    ```text
///    xcrun notarytool store-credentials <profile-name> \
///        --apple-id <your-apple-id> \
///        --team-id <TEAMID> \
///        --password <app-specific-password>
///    ```
///    The app-specific password (minted at appleid.apple.com) is typed only here;
///    `--keychain-profile` keeps it out of the repository, the credentials profile and every
///    later argv.
///
/// 3. **Name that profile in the release-credentials file** that
///    `targo --unverified ship cut --release-credentials <path>` reads:
///    ```toml
///    notary_profile = "<profile-name>"          # from step 2
///    # only if step 1 left two matching certificates:
///    # signing_identity_sha1 = "<the 40-hex SHA-1 of the one to use>"
///    ```
///    A machine with no usable login keychain may use `notary_apple_id` + `notary_password`
///    instead; that puts a live secret in the file, so the loader refuses a profile readable by
///    group or other. There is no `team_id` key: notarytool always gets the Team ID from this
///    constant.
///
/// 4. **Set the value below** to the 10-character Team ID (public: it is in the `subject.OU` of
///    every Developer-ID signature), and update the armed value that
///    `the_shipped_anchor_is_armed_and_an_empty_anchor_still_resolves_inert`
///    (`crates/aterm-release/tests/it/apple_tier.rs`) asserts.
///
/// 5. **Cut a rehearsal first.** `--rehearse` and `--dry-run` sign and notarize for real.
// Armed on m3: two overlapping certificates, so its credentials profile pins
// signing_identity_sha1; the notarytool credential is keychain profile "notary".
pub const APPLE_TEAM_ID: &str = "A66A9P66Z7";

/// The Windows code-signing publisher for the optional Tier WINDOWS anchor, the twin of
/// [`APPLE_TEAM_ID`], read by `targo --unverified winsign` (`crates/aterm-winsign`, which says
/// why only a publicly trusted chain is a lane).
///
/// Empty (as committed): Tier WINDOWS is inactive and the tier refuses nothing. Set: every
/// shipped `aterm.exe` must be signed, timestamped, chain to a root the default Authenticode
/// policy trusts, and have a leaf issued to exactly this string. The value is the leaf's
/// subject Common Name as `signtool verify /v` prints it after `Issued to:` (an MSIX manifest's
/// `Publisher` is the full DN; this is its CN).
///
/// To arm it: obtain the identity (`apps/aterm-win/SIGNING.md`), sign a build while the anchor
/// is still empty, and commit the `signed by` value `targo --unverified winsign verify` prints:
/// the leaf, the last and most indented `Issued to:` in signtool's chain (the first is the root
/// CA, and pinning that would accept every exe under the root).
pub const WINDOWS_SIGNING_PUBLISHER: &str = "";

/// Whether an anchor is active, i.e. non-empty. No environment variable or other ambient
/// state can turn one off.
#[must_use]
pub const fn anchor_active(anchor: &str) -> bool {
    !anchor.is_empty()
}

// Fail-closed, checked at compile time: an empty anchor is never active.
const _: () = assert!(!anchor_active("") && anchor_active("any-nonempty-value"));

#[cfg(test)]
mod tests {
    use super::*;

    /// A master is a base64 Ed25519 public key (32 bytes, 44 chars with one `=`); this
    /// catches a truncated paste.
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
    /// DN and never padded: `targo --unverified winsign` compares it byte for byte.
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
    /// `roster_tier_armed()` while authorizing nobody.
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
        // A master rotation is append → promote → drop, not accumulate. Two is already a
        // rotation in flight; more than that means one was never retired.
        assert!(
            PAPER_MASTER_PUBKEYS.len() <= 2,
            "master keyset has {} members; a rotation window holds at most two",
            PAPER_MASTER_PUBKEYS.len()
        );
    }
}
