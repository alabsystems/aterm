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
//! | [`UPDATE_CHANNEL_PUBKEYS`] | release manifests, for clients that predate the roster | the cutting machine |
//! | [`APPLE_TEAM_ID`] | (not a key) the optional Developer-ID tier | Apple |
//!
//! # ONE ROOT
//!
//! There is exactly one trust root in this tree, and it is [`PAPER_MASTER_PUBKEYS`]. It
//! signs the machine roster; a machine on that roster signs the release appcast AND
//! atpkg's `index.toml` AND every `pkg-*.toml`. One thing written on paper, one document
//! to audit, one revocation that stops everything a stolen machine can publish.
//!
//! atpkg used to have a SECOND root (`PKG_ROOT_PUBKEY`, with its own delegation tier
//! under it). Both are retired and the constant itself is now removed — see the
//! retirement note above [`APPLE_TEAM_ID`].
//!
//! # ⚠ A correction, because this module doc used to be wrong
//!
//! It described a constant named `SIGNING_PUBKEYS` as "the ONLY signing anchor in the
//! tree" and said the atpkg delegation tier "was retired". Neither was true WHEN IT WAS
//! WRITTEN: no such constant ever existed, and atpkg's root → delegate → artifact chain
//! was live and tested. The second half has since come true for a different reason — the
//! delegation tier really is gone now, folded under the paper master — but the correction
//! is left standing, because the lesson is about describing the tree you have.
//!
//! # Why a two-level hierarchy is back
//!
//! The retired one-key record made a specific, correct argument: a delegation tier buys
//! revocation ONLY if the root is genuinely elsewhere, and here it was not — releases
//! are cut by hand, on one machine, by one person, so the "offline" root sat on the same
//! laptop as the online keys. That was paper isolation, and it was rightly retired.
//!
//! A master that is genuinely ON PAPER breaks that premise. It is present on no
//! computer, it is touched only to mint a machine key, and a thief who takes a laptop
//! gets a machine key that the master can revoke without them. So the tier now earns its
//! cost — and it fixes THEFT, which the record it supersedes explicitly called
//! unfixable. See `docs/SIGNING-KEY-DESIGN.md` for the full reversal.
//!
//! # Rotation
//!
//! [`UPDATE_CHANNEL_PUBKEYS`] is a LIST, not a single key, and clients accept any member.
//! That is load-bearing: replacing one key with another in a single release instantly
//! strands every client still holding the old one, and the keyset cannot be
//! retrofitted after a key is lost. Rotating means publishing a bridge release signed
//! by the outgoing key that carries both, then promoting the incoming key and
//! retiring the old one. See `docs/RELEASE-KEYS.md`.
//!
//! The same shape, and the same reasoning, applies to [`PAPER_MASTER_PUBKEYS`].

/// Ed25519 public key(s) of the **paper MASTER** — the offline root of the machine
/// roster. ARMED in this tree since 2026-08-15 (`atpkg-keys setup --id m3`).
///
/// # What this key is, and what it is not
///
/// The master signs exactly one kind of document: `aterm-machines.toml`, the roster that
/// names which machines may sign releases and which machines have been revoked
/// (`aterm_update_core::roster`). It signs no release, no package and no artifact. The
/// owner's decision is that its secret half is written on paper as 52 base32
/// characters and exists on **no computer** — it is typed in only to mint a machine key
/// or to revoke one, and scrubbed immediately after.
///
/// That is the whole reason the tier is worth its cost. A delegation hierarchy whose
/// "offline" root actually sits on the release laptop buys nothing, which is exactly why
/// the previous one was retired. A root on paper is genuinely elsewhere, so a stolen
/// machine key can be revoked by an authority the thief does not have.
///
/// # Empty means unpinned means INERT — and inert means it grants nothing
///
/// An empty SLICE is the fail-closed default (what shipped before v0.21.0; forks start
/// there): the roster tier is absent, `roster::verify_roster` returns `Disabled` for
/// every input, and no machine is ever authorized by it. It never means "accept
/// anything" — with the tier absent, the
/// [`UPDATE_CHANNEL_PUBKEYS`] gate is the authority and is untouched, exactly as it is
/// now. THIS VALUE IS THE SWITCH between the two: see § ARMED, THE ROSTER REPLACES
/// [`UPDATE_CHANNEL_PUBKEYS`] AS THE AUTHORITY, below.
///
/// An empty STRING MEMBER is never legal, for the same reason it is illegal in the
/// channel keyset: it would leave a non-empty anchor list that authorizes nobody, which
/// reads as "armed" to the tier check and refuses everything. The tests below refuse it.
///
/// # A LIST, for the same reason the channel keyset is one
///
/// A client that accepts exactly one master cannot be told about a replacement by a
/// document it would refuse to verify. If the paper is lost or destroyed, the ONLY
/// remedy is a new binary carrying a new pin — so the keyset exists to make a planned
/// master rotation (append → wait out adoption → promote → drop) possible at all. That
/// is a real limit worth stating plainly: the paper master is a single point of total
/// failure in both directions. Photographed, the scheme is gone; lost, no machine can
/// ever be added or revoked again.
///
/// # ⚠ NEVER commit a real-looking key here
///
/// Any value in this file is the live root of trust for every user. Test vectors are
/// generated inside tests from obviously-synthetic seeds and must never land here.
///
/// # ACTIVATION — two commands, and the human types only the phrase
///
/// **Do not hand-edit the value below.** `atpkg-keys` writes it, and that is the point:
/// every manual step this checklist used to list was a chance to paste the wrong 44
/// characters into the file that decides what the whole fleet trusts. The tool derives the
/// key it writes from the master it just generated, writes it here surgically, and then
/// RE-READS this file to prove the value it intended is the value now present.
///
/// 1. **On the FIRST machine, once ever:**
///    ```text
///    atpkg-keys setup --id m3
///    ```
///    It generates the paper master and shows the 52 characters ONCE, **on the
///    terminal itself** (`/dev/tty`, not stdout — a redirect cannot capture them and a
///    discarded stream cannot swallow them), storing them nowhere. **Write them on paper —
///    twice, in two places**, because there is no `k`-of-`n` and no recovery. A short
///    public FINGERPRINT (`a3f2-9c1b`) is shown beside them; write that down too. It then
///    demands the phrase RETYPED FROM THE PAPER — a shown-but-never-copied master arms
///    nothing (added after the 2026-08-14 ceremony failure, where exactly that happened) —
///    and only on a match writes the master's public key into the constant below,
///    mints this machine's keypair to a `0600` file in `$HOME`, and creates the
///    master-signed roster — naming the INCUMBENT keyset head as its first machine (see
///    step 5; `--head-id` names it, default `incumbent-head`) and this machine second.
///
///    It does **not** add the minted key to [`UPDATE_CHANNEL_PUBKEYS`], and that is
///    deliberate: the roster is what authorizes the machine, while a keyset entry would
///    be a grant no `machine-revoke` could ever take back from a client that shipped
///    with it.
///
///    It runs only where there is a terminal to show a phrase on, and refuses otherwise:
///    a master generated into a pipe is either a permanent plaintext copy of the fleet's
///    root key or a master nobody ever saw.
///
///    `setup` REFUSES if a master is already committed here, and tells you to use `join`.
///    Generating a second master silently would strand the first — whose secret half is on
///    paper and exists nowhere else.
///
/// 2. **Re-prove the paper any later day** with `atpkg-keys master-check`: the same
///    no-echo prompt, the fingerprint printed back. `setup` already proved the
///    transcription once, at arming time, before anything was written.
///
/// 3. **Review and commit.** The tool does NOT `git commit` and does NOT `git push`:
///    arming a trust anchor is a reviewed act, so it edits the working tree, prints
///    exactly what is and is not now true, and stops. Read `git diff` over this file, and
///    delete the tripwire assertions the tool names in its closing output — they exist so
///    this step cannot be taken by accident.
///
/// 4. **On EVERY LATER publishing machine:**
///    ```text
///    atpkg-keys join --id m11
///    ```
///    It reads the phrase from `/dev/tty` (never argv, never an environment variable,
///    never stdin, never a file — there is no flag that takes it, and the whole family of
///    spellings is refused BY NAME so a phrase typed on a command line is reported as
///    compromised rather than quietly ignored), PROVES it against the committed anchor
///    above AND against the existing roster the real master signed, and only then mints
///    locally and re-signs the roster. It edits NO trust anchor at all. A mistyped phrase
///    is refused with nothing written. Only machines that PUBLISH need a key; a machine
///    that merely builds does not.
///
///    **This is the act the whole tier exists for, and it needs no release.** Once the
///    re-signed roster is published on a release, the new machine can cut for every
///    roster-aware client — see step 5 for the one audience it cannot reach.
///
///    **Copy `aterm-machines.toml` and its `.sig` to the machine first.** The roster is
///    published as a release asset and lives under `dist/`, which is gitignored, so a fresh
///    checkout does not have one — and `join` REFUSES to run without it rather than
///    starting a second roster. Two rosters signed by the same master at the same sequence
///    de-authorize each other's machines, invisibly to the monotonic ratchet, and a client
///    that meets a release it cannot attribute has no fallback.
///
/// 5. **Serve the installed base until it has rolled over.** Shipped clients verify the
///    appcast against their own compiled-in [`UPDATE_CHANNEL_PUBKEYS`], know nothing about
///    a roster, and — because release selection yields exactly ONE candidate with no
///    fallback to an older release — a client that meets a release it cannot verify is not
///    delayed, it is WEDGED there permanently. So the FIRST roster must name the existing
///    channel key as its first machine, and until the fleet has adopted a roster-aware
///    build, releases must be cut only from that machine. **`setup` does that seeding
///    itself**, because it has to: arming the master changes the cutter's authorization
///    check from "the signing key IS `UPDATE_CHANNEL_PUBKEYS[0]`" to "the roster names it",
///    so a first roster without the incumbent would leave the one machine every shipped
///    client can verify unable to cut at all. The keyset's other, ACCEPT-ONLY members are
///    deliberately NOT rostered: nobody on this machine holds their private halves, and
///    listing a key nobody can use only widens the set a thief could aim at.
///
///    The incumbent's own release-credentials profile must set `machine_id` to the roster
///    id it is seeded under (`--head-id`, default `incumbent-head`). `setup` writes
///    `~/.aterm/machine.toml` naming the box it ran on, the cutter falls back to that file
///    when the profile is silent, and a declared id that contradicts the roster refuses the
///    cut — so without that one line the SAFE path is the one that fails.
///
///    A machine `setup`/`join` just minted CAN cut for every roster-aware client the moment
///    the re-signed roster is published — that is the point — but a cut from it is refused
///    by `cargo ship cut` unless the command carries `--strand-pre-roster-clients`, which
///    asserts that no pre-roster client is left. The tools say so in their closing output
///    rather than leaving it to be discovered. See `docs/SIGNING-KEY-DESIGN.md` § The
///    bridge.
///
/// 6. **Revoke with `atpkg-keys machine-revoke --id <id>`**, which bumps `roster_seq`,
///    re-signs with the paper master, and refreshes the freshness window.
///
/// # ⚠ ARMED, THE ROSTER REPLACES [`UPDATE_CHANNEL_PUBKEYS`] AS THE AUTHORITY
///
/// The two anchors are not two gates that both must pass. Which one decides is chosen by
/// THIS anchor, and by nothing else:
///
/// * **empty master** — the compiled-in keyset decides, exactly as it always has. This is
///   every build shipped BEFORE v0.21.0 (the first armed release, 2026-08-15), and
///   `fetch_authoritative_release`'s unarmed branch is the old code rather than a
///   re-expression of it.
/// * **armed master** — the master-signed roster decides, and it decides ALONE. The keyset
///   is not consulted and cannot refuse a machine the roster authorized. That is what makes
///   adding a machine a LOCAL act — mint, roster, publish — instead of one that needs a
///   release cut from a machine that can already sign, which is the ceremony the owner
///   asked to remove.
///
/// It is deliberately not an OR of the two. Accepting "keyset member OR rostered" would
/// mean a machine the owner had REVOKED kept publishing to every client whose build happens
/// to carry its key, forever, because a shipped key cannot be un-shipped. Revocation is the
/// one thing this tier exists to provide, so an OR would buy compatibility by giving up the
/// reason for the tier.
///
/// # ⚠ WHAT THE KEYSET IS FOR ONCE THE MASTER IS ARMED
///
/// It is the allowance held by clients that PREDATE the roster — nothing more, and nothing
/// less. Those clients are real, they verify the appcast under their own compiled-in
/// keyset, they know nothing about a roster, and `select_authoritative_release` gives them
/// exactly ONE candidate with no fallback to an older release. So a release signed by a key
/// they do not hold does not make them miss an update: it makes them never update again,
/// silently, with a reinstall as the only remedy.
///
/// **And "the keyset in this tree" is NOT "the keyset they hold".** This slice is what the
/// NEXT build will carry. Step 1 of the rotation above appends a key precisely so a FUTURE
/// build can ship it, so at the moment of appending a non-head member is in this file and
/// in nobody's installed build — K2 was exactly that from 2026-08-12 until it was dropped
/// unused on 2026-08-15. Index 0 is the only member a shipped build is known to carry,
/// because promotion TO index 0 (step 3) is the reviewed commit in which the operator
/// asserts the adoption window has closed.
///
/// That obligation is the PRODUCER's, because the producer is what chooses the signing key,
/// and it is enforced as an operator assertion rather than a guess:
/// `aterm_release::publish::channel_signature_policy` REFUSES a cut whose signing key is
/// not `UPDATE_CHANNEL_PUBKEYS[0]` unless the command carries
/// `--strand-pre-roster-clients`, and prints an unmissable warning when it does. It names
/// an accept-only member as such rather than calling it a stranger, but it refuses it just
/// the same. No program can know whether a pre-roster client is still out there; the
/// operator can.
///
/// So arming the master changes WHO MAY SIGN (the roster, not this keyset) without changing
/// WHO CAN VERIFY: the head-equality rule the single-key cutter has always enforced survives
/// intact as the pre-roster obligation, and is discharged by an assertion rather than by a
/// silent widening.
///
/// With the master unpinned the producer demands that same rule as an absolute
/// (`committed_channel_signature_policy`, equality with `UPDATE_CHANNEL_PUBKEYS[0]`, no flag
/// and no roster) and behaves byte-identically to the single-key cutter — the empty anchor
/// changes nothing about what ships.
///
/// # ⚠ EVERY PUBLISHING MACHINE MUST HOLD A CURRENT ROSTER, not merely a valid one
///
/// This is the operational rule arming the tier adds, and it is not obvious from the
/// steps above. `roster_seq` is a MONOTONIC channel counter, and the client treats it as
/// one: it ratchets the highest generation it has ever OBSERVED into durable state —
/// whether or not it staged the release — and refuses anything below that with
/// `RosterReject::Rollback`, before any artifact crypto. Because release selection yields
/// exactly one candidate with no fallback, a rolled-back head does not delay those
/// clients; it stops them updating at all, silently, while the cut reports success.
///
/// The roster is NOT distributed with the repository — `atpkg-keys`' default path is
/// `dist/aterm-machines.toml` and `/dist/` is gitignored — so every machine that did not
/// run the mint holds a hand-copied roster, and holding a stale-but-unexpired one is the
/// steady state rather than the accident. Concretely: revoking a machine on m3 bumps the
/// generation and publishes it; m11, still on its previous copy, would pass freshness,
/// pass the deny-list, and publish a release the whole updated fleet refuses. The same
/// staleness is what would let a machine revoked at generation N authorize itself from
/// its own copy at N−1 — the producer-side deny-list is only ever as current as the
/// least-updated cutter.
///
/// `aterm_release::publish::roster_floor_covered` closes both directions by reading the
/// generation out of the published channel head's own manifest and refusing a cut that
/// carries an older one — pre-claim, and again under the release lease at lock,
/// selfcheck, preflip and flip, exactly where the `min_build` ratchet is enforced. So the
/// working rule when arming is: **after any `join` or `machine-revoke`, copy the
/// re-signed roster to every publishing machine before the next cut from any of them.**
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

/// Ed25519 public keys any of which may sign — a release manifest for the public
/// update channel, or an atpkg `index.toml` / `pkg-*.toml`. One anchor, both
/// consumers.
///
/// ORDER IS A CONTRACT. Index 0 is the key THIS build signs with. Every other member
/// is accepted-but-never-signed-with, and is either an **incoming** key being
/// pre-seeded into clients ahead of a rotation, or an **outgoing** key inside its
/// retirement window. Verification accepts any member.
///
/// The only workable rotation order follows from that, because a client can only
/// learn a new key from a release it already accepts:
///
/// 1. append K2 as a NON-head member, and ship — clients now accept K1 and K2
/// 2. wait out the adoption window
/// 3. promote K2 to index 0, so new releases are signed with K2
/// 4. drop K1 once the window has closed
///
/// Empty slice = unpinned: signature verification is skipped and the channel is
/// unauthenticated (forks and private channels).
///
/// An EMPTY STRING MEMBER is never legal — it is not "unpinned", it is a brick:
/// `update_channel_signing_pubkey()` would return `""` (so the build stamps itself
/// unpinned) while this slice is non-empty (so the client demands a signature and
/// then rejects every one). The tests below refuse it.
pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[
    // K1 — `aterm-update-v2`. HEAD: the one key pre-roster clients (builds
    // older than v0.21.0) verify. Roster-aware clients authorize by the
    // master-signed roster alone; K1 serves the pre-roster installed base and
    // leaves the tree when the operator asserts that base is gone — the same
    // reviewed act that must first rework the embedded-pin seam
    // (`expected_embedded_update_pin` vs build.rs's UNPINNED sentinel), which
    // today refuses every signed cut from an empty-keyset tree.
    "cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=",
    // K2 (`aterm-update-v3`) was DROPPED 2026-08-15, never having signed
    // anything: it was pre-seeded for a channel-key rotation the armed roster
    // tier made obsolete (machines rotate by join/revoke now, no keyset edit).
    // Its private half at ~/.aterm/update-v3.key on its minting machine is
    // dead material.
];

/// The single key new releases are signed WITH — always `UPDATE_CHANNEL_PUBKEYS[0]`.
///
/// Verification must use [`UPDATE_CHANNEL_PUBKEYS`] (accept any); only the cutter,
/// which produces exactly one signature, cares which key is current.
#[must_use]
pub const fn update_channel_signing_pubkey() -> &'static str {
    if UPDATE_CHANNEL_PUBKEYS.is_empty() {
        ""
    } else {
        UPDATE_CHANNEL_PUBKEYS[0]
    }
}

// **RETIRED AND REMOVED.** atpkg's index used to be anchored by a second root here
// (`PKG_ROOT_PUBKEY`, whose secret half lived at `~/.config/atpkg/root.key` — on a
// computer). It is gone: atpkg's index is authorized by the SAME master-signed machine
// roster as app releases ([`PAPER_MASTER_PUBKEYS`]), so there is ONE root and one
// revocation story. Two roots meant two deny-lists and no single document an owner
// could revoke to stop everything a stolen machine could publish.
//
// The last producer-side readers — `aterm-release`'s DMG seed-sealing gate and a test
// borrowing it as "a second key" — were deleted with the seedpack lane, whose CLIENT
// half had already been removed (`ba832933`): a sealed seed was dead weight no shipped
// client would read. The old key's value survives in git history; anyone who finds a
// `~/.config/atpkg/root.key` on an old machine should delete it — it signs nothing any
// shipped or buildable client will accept.

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
///    ship), and delete the tripwire assertion in
///    `crates/aterm-release/tests/apple_tier.rs`
///    (`the_shipped_anchor_is_unset_so_the_tier_is_inert`), which exists to make
///    this step impossible to take by accident. One reviewed diff.
///
/// 5. **Cut a rehearsal first.** `--rehearse` and `--dry-run` deliberately sign
///    and notarize for real, so the rehearsal is a true proof that the whole path
///    works — and the first real pinned release is not the first time it runs.
// ARMED 2026-08-15 per the checklist above: membership paid, the Developer ID
// Application certificate + key live in the cutting machine's (m3's) login keychain
// (two overlapping certs, so the credentials profile pins signing_identity_sha1),
// and the notarytool credential is stored under keychain profile "notary".
pub const APPLE_TEAM_ID: &str = "A66A9P66Z7";

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

    /// The signing key is the head of the keyset — not a second, separately edited
    /// constant that could silently disagree with it.
    #[test]
    fn signing_key_is_the_head_of_the_keyset() {
        assert_eq!(update_channel_signing_pubkey(), UPDATE_CHANNEL_PUBKEYS[0]);
        assert!(
            !update_channel_signing_pubkey().is_empty(),
            "the public channel is pinned; an empty signing key would silently unpin it"
        );
    }

    /// A keyset with duplicates means a rotation was recorded wrong: the outgoing
    /// key was re-added rather than retired, so retiring it later would be a no-op.
    #[test]
    fn keyset_has_no_duplicates() {
        for (i, key) in UPDATE_CHANNEL_PUBKEYS.iter().enumerate() {
            assert!(
                !UPDATE_CHANNEL_PUBKEYS[..i].contains(key),
                "duplicate key in the rotation set: {key}"
            );
        }
    }

    /// An empty keyset member is a BRICK, not "unpinned": the build would stamp
    /// itself unpinned (`update_channel_signing_pubkey()` == "") while the client
    /// sees a non-empty keyset, demands a signature, and rejects every one. Only the
    /// whole slice being empty means unpinned.
    #[test]
    fn keyset_has_no_empty_members() {
        for (i, key) in UPDATE_CHANNEL_PUBKEYS.iter().enumerate() {
            assert!(
                !key.is_empty(),
                "UPDATE_CHANNEL_PUBKEYS[{i}] is empty — use an empty SLICE to unpin, \
                 never an empty member"
            );
        }
    }

    /// A keyset is a rotation window, not an accumulator. An unbounded list means an
    /// old key was never retired, which is the failure rotation exists to avoid.
    #[test]
    fn keyset_is_bounded() {
        assert!(
            UPDATE_CHANNEL_PUBKEYS.len() <= 4,
            "keyset has {} members; retire outgoing keys instead of accumulating them",
            UPDATE_CHANNEL_PUBKEYS.len()
        );
    }

    /// Anchors are base64 Ed25519 public keys: 32 bytes -> 44 chars with one `=`.
    /// Catches a truncated paste, which would otherwise fail closed at runtime with
    /// no hint that the VALUE, not the state, is wrong.
    #[test]
    fn anchors_are_well_formed_base64_ed25519() {
        // NOTE the asymmetry: an empty ANCHOR is legal (that tier is inert), but an
        // empty keyset MEMBER is not — see `keyset_has_no_empty_members`.
        let check = |k: &str, what: &str| {
            if k.is_empty() {
                return; // unpinned is legal for a whole anchor
            }
            assert_eq!(k.len(), 44, "{what}: not a 44-char base64 Ed25519 key: {k}");
            assert!(k.ends_with('='), "{what}: missing base64 padding: {k}");
            assert!(
                k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='),
                "{what}: non-base64 character: {k}"
            );
        };
        for key in UPDATE_CHANNEL_PUBKEYS {
            check(key, "UPDATE_CHANNEL_PUBKEYS");
        }
        for key in PAPER_MASTER_PUBKEYS {
            check(key, "PAPER_MASTER_PUBKEYS");
        }
    }

    #[test]
    fn empty_anchor_is_never_active() {
        assert!(!anchor_active(""));
        assert!(anchor_active("any-nonempty-value"));
    }

    // (The unset-anchor tripwire that stood here was deleted 2026-08-15 as part of the
    // arming commit, exactly as its own doc prescribed: `atpkg-keys setup --id m3` wrote
    // the anchor, the tripwire went red, and the reviewed diff removed it.)

    /// The same brick rule the channel keyset has, applied to the master: only an empty
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

    /// The master and the channel key are DIFFERENT roots doing different jobs: the
    /// master signs the roster, a machine key signs releases. Listing the same key as
    /// both would collapse the hierarchy back into one key while looking like a
    /// hierarchy — the paper master would be on the release machine, which is precisely
    /// the premise that made the previous tier worthless.
    #[test]
    fn the_master_is_never_also_a_channel_signing_key() {
        for master in PAPER_MASTER_PUBKEYS {
            assert!(
                !UPDATE_CHANNEL_PUBKEYS.contains(master),
                "the paper master must not also be a channel signing key: {master}"
            );
        }
    }
}
