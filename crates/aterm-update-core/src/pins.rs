// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE trust anchors. Every one of them, in one file.
//!
//! Changing a value here is a reviewed commit. There is no other way to change what
//! a build trusts — no environment variable, no per-machine file, no build flag.
//!
//! # Why these are constants and not `option_env!`
//!
//! These were compiled in from the build environment (`ATERM_UPDATE_PUBKEY`,
//! `ATERM_PKG_ROOTKEY`, `ATERM_EXPECTED_TEAM_ID`), exported into the child cargo
//! build by the release cutter. That made **what a binary trusts a property of the
//! shell that compiled it rather than of the source**, with three costs paid in
//! practice:
//!
//! * A locally built `atpkg` had an EMPTY root key and refused every install with
//!   `atpkg: disabled (no root key pinned or overridden)`. Same commit, same machine,
//!   different trust, and no diff anywhere to review.
//! * An unset variable is silent: `option_env!` yields `None`, the pin becomes `""`,
//!   and the consumer goes inert without saying so.
//! * "What does this build trust?" could not be answered by reading the repository.
//!
//! As constants the answer is a `git diff`, identical on every machine, and carried
//! in history like any other reviewed change.
//!
//! # Empty means unpinned, and unpinned means inert
//!
//! An empty anchor is the fail-closed default: with nothing compiled in there is
//! nothing to trust, so the consumer stays inert rather than accepting anything. A
//! fork or private channel commits its OWN anchors here — the same deliberate,
//! reviewable act as changing `update_channel`.
//!
//! # The anchors in this file
//!
//! | Anchor | Signs | Where its secret half lives |
//! |---|---|---|
//! | [`PAPER_MASTER_PUBKEYS`] | the machine roster — which authorizes BOTH release manifests and atpkg's `index.toml` | **on paper, on no computer** |
//! | [`APPLE_TEAM_ID`] | (not a key) the optional Developer-ID tier | Apple |
//! | [`WINDOWS_SIGNING_PUBLISHER`] | (not a key) the optional Windows code-signing tier | the signing service |
//!
//! # ONE ROOT
//!
//! There is exactly one trust root in this tree, and it is [`PAPER_MASTER_PUBKEYS`]. It
//! signs the machine roster; a machine on that roster signs the release appcast AND
//! atpkg's `index.toml` AND every `pkg-*.toml`. One thing written on paper, one document
//! to audit, one revocation that stops everything a stolen machine can publish.
//!
//! Two retired roots are gone for good: atpkg's second root (`PKG_ROOT_PUBKEY`, with its
//! own delegation tier) and the channel keyset (`UPDATE_CHANNEL_PUBKEYS`, "K1") that
//! builds older than v0.21.0 verified releases under before the roster existed. Those
//! installs are abandoned (owner ruling, 2026-09-23): every client from v0.21.0 on
//! authorizes a release by the master-signed roster alone and never consulted K1, so
//! nothing a current client needs was removed with it. K1's private half is simply the
//! roster machine `incumbent-head` now — revocable like any other.
//!
//! # Why a two-level hierarchy
//!
//! A delegation tier buys revocation ONLY if the root is genuinely elsewhere. A master
//! that is ON PAPER is: it is present on no computer, it is touched only to mint or
//! revoke a machine key, and a thief who takes a laptop gets a machine key that the
//! master can revoke without them. See `docs/SIGNING-KEY-DESIGN.md`.
//!
//! # Rotation
//!
//! [`PAPER_MASTER_PUBKEYS`] is a LIST, and clients accept any member: replacing one
//! master with another in a single release would strand every client still holding the
//! old one. Rotating means shipping a build that carries both, waiting out adoption,
//! then dropping the old one. Machines rotate without any of this — `atpkg-keys join`
//! and `machine-revoke` re-sign the roster, and no binary changes.

/// Ed25519 public key(s) of the **paper MASTER** — the offline root of the machine
/// roster, and the ONE anchor that authorizes a release. ARMED in this tree since
/// 2026-08-15 (`atpkg-keys setup --id m3`).
///
/// # What this key is, and what it is not
///
/// The master signs exactly one kind of document: `aterm-machines.toml`, the roster that
/// names which machines may sign releases and which machines have been revoked
/// (`aterm_update_core::roster`). It signs no release, no package and no artifact. Its
/// secret half is written on paper as 52 base32 characters and exists on **no computer**
/// — it is typed in only to mint a machine key or to revoke one, and scrubbed
/// immediately after.
///
/// A client authorizes a release by the roster ALONE: the appcast must be signed by a
/// machine the master-signed roster names and has not revoked. There is no second key a
/// client also accepts — an OR of "roster or some compiled-in key" would let a revoked
/// machine keep publishing to every build that carried its key, forever.
///
/// # Empty means unpinned — the signature tier is ABSENT
///
/// An empty SLICE (a fork that has not committed a master of its own) removes the
/// signature tier: `roster::verify_roster` returns `Disabled`, the updater verifies no
/// appcast signature, and trust is the channel repository over TLS plus the manifest
/// sha256 and the codesign/Team-ID checks — the same shape as an empty [`APPLE_TEAM_ID`]
/// removing the Developer-ID tier without loosening anything beside it.
///
/// An empty STRING MEMBER is never legal: it would leave a non-empty anchor list that
/// authorizes nobody, which reads as "armed" and refuses everything. The tests below
/// refuse it.
///
/// # A LIST, so a planned rotation is possible
///
/// A client that accepts exactly one master cannot be told about a replacement by a
/// document it would refuse to verify. If the paper is lost or destroyed, the ONLY
/// remedy is a new binary carrying a new pin — so the list exists to make a planned
/// rotation (append → wait out adoption → drop) possible at all. The paper master is a
/// single point of total failure in both directions: photographed, the scheme is gone;
/// lost, no machine can ever be added or revoked again.
///
/// # ⚠ NEVER commit a real-looking key here
///
/// Any value in this file is the live root of trust for every user. Test vectors are
/// generated inside tests from obviously-synthetic seeds and must never land here.
///
/// # Machines — two commands, and the human types only the phrase
///
/// **Do not hand-edit the value below.** `atpkg-keys` writes it and re-reads this file to
/// prove the value it intended is the value now present.
///
/// 1. **Once ever, on the first machine:** `atpkg-keys setup --id <id>` generates the
///    paper master, shows its 52 characters ONCE on `/dev/tty`, demands them retyped from
///    the paper, writes the master's public key here, mints this machine's key and
///    creates the master-signed roster. It refuses if a master is already committed.
/// 2. **Review and commit.** The tool edits the working tree and stops; arming a trust
///    anchor is a reviewed act.
/// 3. **Every later publishing machine:** `atpkg-keys join --id <id>` proves the phrase
///    against the committed anchor and the existing roster, mints locally and re-signs
///    the roster. It edits no trust anchor. Copy `aterm-machines.toml` and its `.sig` to
///    the machine first — `join` refuses to start a second roster.
/// 4. **Revoke with `atpkg-keys machine-revoke --id <id>`**, which bumps `roster_seq`,
///    re-signs with the paper master, and refreshes the freshness window.
///
/// # ⚠ EVERY PUBLISHING MACHINE MUST HOLD A CURRENT ROSTER, not merely a valid one
///
/// `roster_seq` is a MONOTONIC channel counter: a client ratchets the highest generation
/// it has ever OBSERVED and refuses anything below it with `RosterReject::Rollback`.
/// Because the roster is not distributed with the repository (`dist/` is gitignored), a
/// machine that did not run the last `join`/`machine-revoke` holds a stale copy.
/// `aterm_release::publish::roster_floor_covered` refuses a cut that carries an older
/// generation than the published channel head's. The working rule: **after any `join` or
/// `machine-revoke`, copy the re-signed roster to every publishing machine before the
/// next cut from any of them.**
pub const PAPER_MASTER_PUBKEYS: &[&str] = &[
    // The PAPER MASTER — the offline root of the machine roster, armed by
    // `atpkg-keys setup --id m3` on 2026-08-15.
    // Its secret half is 52 base32 characters ON PAPER and exists on no
    // computer. It signs aterm-machines.toml and nothing else.
    "DtiLfpk0iUSrK1/LkyIVf+4C2eGjD2Myf4Sr/FCoMPQ=",
];

/// Whether the paper-master roster tier is ARMED for this build.
///
/// Fail-closed by construction: an unpinned master means the tier authorizes nothing, so
/// callers skip it rather than treating an absent roster as permission. This is the same
/// shape as [`APPLE_TEAM_ID`] — an empty anchor removes a tier, it never weakens the
/// tiers beside it.
#[must_use]
pub const fn roster_tier_armed() -> bool {
    !PAPER_MASTER_PUBKEYS.is_empty()
}

/// Apple Developer **Team ID** for the optional Tier APPLE anchor.
///
/// Empty does NOT disable the updater — it skips the codesign/notarization anchor,
/// leaving signature + hash verification intact. Non-empty is a promise the release
/// pipeline must keep: a Developer-ID-signed AND notarized artifact.
///
/// # This one line is the whole switch
///
/// The producer and every consumer are already wired to it, so a non-empty value
/// here simultaneously arms four things and needs no other code change:
///
/// * the release pipeline resolves a Developer-ID certificate, signs, notarizes,
///   staples and verifies — or fails the cut (`aterm-release/src/sign.rs`);
/// * the cut's self-check demands `TeamIdentifier=`, a stapled ticket and a
///   Gatekeeper pass on BOTH the `.app` and the `.dmg`;
/// * the in-app updater refuses any update not Developer-ID signed by this exact
///   team (`aterm-update`'s `PINNED_TEAM_ID` → `verify_bundle_policy`);
/// * `tools/install.sh` builds a `subject.OU = "<TEAM>"` designated requirement
///   and refuses a non-Dev-ID bundle.
///
/// The identity STRING (`Developer ID Application: <name> (<TEAMID>)`) is
/// deliberately NOT written down anywhere: the pipeline derives it by matching
/// this anchor against `security find-identity -v -p codesigning`. That keeps
/// this constant the only place a Team ID appears in the entire tree, which is
/// the property that makes "what does this build trust?" answerable by a `git
/// diff` rather than by auditing a keychain.
///
/// # ⚠ This is a one-way door for anyone already running a pinned build
///
/// A shipped binary carrying a non-empty value refuses every FUTURE update that
/// is not Developer-ID signed by this team. Turning the anchor back off in
/// source does not reach a binary already in the field: those clients are
/// stranded with no update path if the Developer Program membership lapses or
/// the certificate expires without replacement. Setting this is a commitment to
/// keep that membership alive for as long as pinned clients exist, and the first
/// pinned release must itself be a genuinely notarized cut.
///
/// # ACTIVATION CHECKLIST — turning Tier APPLE on
///
/// Do these in order. Steps 1–3 are one-time setup on the cutting machine and
/// change nothing about what ships; step 4 is the reviewed commit that flips it.
///
/// 1. **Hold an active Apple Developer Program membership**, and install a
///    **Developer ID Application** certificate *and its private key* in the
///    login keychain of the machine that cuts releases. Verify with
///    `security find-identity -v -p codesigning` — you want EXACTLY ONE line
///    reading `Developer ID Application: <you> (<TEAMID>)`. If two appear (a
///    renewal overlapping the incumbent), either delete the superseded one in
///    Keychain Access or continue to step 3's optional key; the pipeline refuses
///    to guess between them.
///
/// 2. **Store the notarytool credential once, outside the repo:**
///    ```text
///    xcrun notarytool store-credentials <profile-name> \
///        --apple-id <your-apple-id> \
///        --team-id <TEAMID> \
///        --password <app-specific-password>
///    ```
///    The app-specific password is minted at appleid.apple.com. It is typed into
///    this command once and never enters the repository, the credentials profile,
///    or any argv thereafter — `--keychain-profile` is why. `<profile-name>` is a
///    label you choose; nothing else derives meaning from it.
///
/// 3. **Name that profile in the release-credentials file** — the same 0600 file
///    `cargo ship cut --release-credentials <path>` already reads for the Ed25519
///    signing key. Add one line:
///    ```toml
///    notary_profile = "<profile-name>"          # from step 2
///    # only if step 1 left two matching certificates:
///    # signing_identity_sha1 = "<the 40-hex SHA-1 of the one to use>"
///    ```
///    A machine with no usable login keychain may instead use the headless
///    fallback `notary_apple_id` + `notary_password`; it puts a live secret in
///    that file, which is why the loader refuses any profile readable by group or
///    other. Note there is no `team_id` key: the Team ID notarytool receives
///    always comes from THIS constant, so the two can never disagree.
///
/// 4. **Set the value below** to your 10-character Team ID (it is public — it
///    already appears in the `subject.OU` of every Developer-ID signature you
///    ship). This step was taken on 2026-08-15, and taking it DELETED the tripwire
///    assertion this checklist used to send the operator to remove
///    (`the_shipped_anchor_is_unset_so_the_tier_is_inert`, in
///    `crates/aterm-release/tests/apple_tier.rs`) — the instruction outlived the test it
///    named, which is now defined nowhere in the tree. Its replacement,
///    `the_shipped_anchor_is_armed_and_an_empty_anchor_still_resolves_inert`, asserts the
///    ARMED value directly, so a fork re-arming this anchor updates that assertion rather
///    than deleting one. One reviewed diff.
///
/// 5. **Cut a rehearsal first.** `--rehearse` and `--dry-run` deliberately sign
///    and notarize for real, so the rehearsal is a true proof that the whole path
///    works — and the first real pinned release is not the first time it runs.
// ARMED 2026-08-15 per the checklist above: membership paid, the Developer ID
// Application certificate + key live in the cutting machine's (m3's) login keychain
// (two overlapping certs, so the credentials profile pins signing_identity_sha1),
// and the notarytool credential is stored under keychain profile "notary".
pub const APPLE_TEAM_ID: &str = "A66A9P66Z7";

/// The Windows code-signing **publisher** for the optional Tier WINDOWS anchor —
/// the Windows twin of [`APPLE_TEAM_ID`], read by `cargo winsign`
/// (`crates/aterm-winsign`).
///
/// # This one line is the whole switch
///
/// Empty (as committed) → Tier WINDOWS is INACTIVE: `cargo winsign sign` signs
/// when a lane is configured and reports honestly when none is, and nothing
/// fails a build over a missing or self-signed signature. Set → ACTIVE: every
/// shipped `aterm.exe` must be signed, timestamped, chain to a root the default
/// Authenticode policy trusts, and its leaf certificate must be issued to
/// EXACTLY this string — or `cargo winsign` refuses. The value is the leaf's
/// subject Common Name as `signtool verify /v` prints it after `Issued to:`
/// (the `Publisher` an MSIX manifest carries is the full DN; this is its CN).
///
/// # Why a signature at all, and why not a self-signed one
///
/// Windows 11 with Smart App Control ON runs an unsigned exe only while the
/// reputation service happens to allow it, and Code Integrity refuses it
/// (event 3077, "did not meet the Enterprise signing level requirements")
/// whenever that changes — measured on 2026-09-15 on an installed
/// `aterm-gui.exe` that had run the day before. Only a publicly trusted chain
/// clears that policy: Azure Trusted Signing (the lane Microsoft recommends for
/// exactly this) or a CA-issued code-signing certificate. A self-signed
/// certificate, however widely trusted locally, does not, so it is not a lane.
///
/// # ACTIVATION CHECKLIST — turning Tier WINDOWS on
///
/// 1. Obtain the identity (apps/aterm-win/SIGNING.md walks both lanes).
/// 2. Sign a build with it while the anchor is still empty and read the
///    `Issued to:` line `cargo winsign verify` prints back — THAT string, byte for
///    byte, is the value to commit here.
/// 3. Commit it. From that commit on, an unsigned or wrongly signed Windows
///    exe is refused by the tool, never shipped by accident.
pub const WINDOWS_SIGNING_PUBLISHER: &str = "";

/// Whether an anchor is active. Fail-closed: an empty anchor is never active.
///
/// Unlike the `pin_active` it replaces, this takes NO opt-out environment variable.
/// A build either has an anchor or it does not, and no ambient state can turn one
/// off — which is the entire point of moving these into source.
#[must_use]
pub const fn anchor_active(anchor: &str) -> bool {
    !anchor.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anchors are base64 Ed25519 public keys: 32 bytes -> 44 chars with one `=`.
    /// Catches a truncated paste, which would otherwise fail closed at runtime with
    /// no hint that the VALUE, not the state, is wrong.
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

    #[test]
    fn empty_anchor_is_never_active() {
        assert!(!anchor_active(""));
        assert!(anchor_active("any-nonempty-value"));
    }

    /// The Windows anchor is the leaf certificate's subject CN exactly as
    /// `signtool verify /v` prints it after `Issued to:` — never a full DN
    /// (`CN=…, O=…`), never padded. `cargo winsign` compares it byte for byte,
    /// so a DN or stray whitespace would refuse every correctly signed build.
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

    // (The unset-anchor tripwire that stood here was deleted 2026-08-15 as part of the
    // arming commit, exactly as its own doc prescribed: `atpkg-keys setup --id m3` wrote
    // the anchor, the tripwire went red, and the reviewed diff removed it.)

    /// The brick rule for the master: only an empty
    /// SLICE means unpinned. A non-empty list containing an empty member would read as
    /// ARMED to `roster_tier_armed()` while authorizing nobody, so every release would be
    /// refused with no diff explaining why.
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

    /// `roster_tier_armed()` is derived from the anchor, never separately edited — the
    /// drift that a second constant would allow is unrepresentable.
    #[test]
    fn the_tier_switch_is_the_anchor_itself() {
        assert_eq!(roster_tier_armed(), !PAPER_MASTER_PUBKEYS.is_empty());
    }
}
