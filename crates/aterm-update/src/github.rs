// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The DMG/.app-specific orchestration of a background update check: learn which
//! release is the channel head, and — if it is strictly newer than the running build —
//! download + verify its container and stage it. The portable plumbing it drives (the
//! redirect-refusing HEAD, the `curl` GETs of the download host) lives in
//! `aterm-update-core`.
//!
//! # One lane: the unmetered download host
//!
//! Every check, on every source, reads `github.com/<owner>/<repo>/releases/…` with no
//! credential and never touches `api.github.com` — not on a happy path, not on a failure
//! path. There is no budget on this host, so there is nothing to hold, reserve or fall
//! back to. A check is:
//!
//! 1. ONE unmetered `HEAD …/releases/latest/download/aterm-appcast.toml` with redirects
//!    refused ([`aterm_update_core::pointer`]). Its `Location` names the newest
//!    published release's tag — drafts and prereleases excluded by GitHub itself — and
//!    is accepted only when it is exactly the derived tag-specific URL of this
//!    repository under a canonical `vMAJOR.MINOR.PATCH` tag.
//! 2. If that tag is the one the ledger last AUTHORIZED (`latest_tag` in
//!    `status.toml`), the check is complete: "up to date", no other request.
//! 3. Otherwise every asset is fetched from the TAG-SPECIFIC
//!    `…/releases/download/<tag>/<name>` — never through `latest` again, so a pointer
//!    that moves mid-check cannot mix two releases — and meets: the master-signed
//!    machine roster ([`authorize_by_roster`], fail-closed), the manifest `version` ==
//!    the tag minus its `v`, the manifest's container `url` == the DERIVED download URL
//!    (the manifest field is cross-checked, never followed), then the build-number,
//!    `min_build`, high-water and roster-floor gates, the sha256 and the
//!    codesign/Team-ID verification on the staged bundle.
//!
//! A head that is not an app release — a release that carries no `aterm-appcast.toml`
//! (a source-only release) or a tag this client does not install from — is recorded as
//! exactly that ("channel head <tag> has no app manifest yet") and the check ends: the
//! publisher owns `latest`, and the next check reads the head again. Nothing lists the
//! catalog to look past it.
//!
//! The lane is not a trust decision. Artifact trust is the master-signed roster, the
//! pinned Team ID and the manifest sha256 — none of which the transport touches.

use std::sync::atomic::{AtomicBool, Ordering};

use aterm_update_core::tag::{TagError, TagKind};
use aterm_update_core::{HeadAnswer, HttpError};

use crate::manifest::{Manifest, Ready};
use crate::{Source, bundle, install, paths::Staging};

/// Whether the last check ended in a deferral — the download host asked us to slow
/// down (a 429 or a 5xx). Read by the background loop to LENGTHEN the next wait without
/// recording a failure: weather, not a broken updater.
static RATE_LIMITED: AtomicBool = AtomicBool::new(false);

/// Whether this process has already logged which channel it reads. Once per process: it
/// is a standing condition, not an event.
static ANNOUNCED_CHANNEL: AtomicBool = AtomicBool::new(false);

/// Whether the last check was cut short by a deferral.
#[must_use]
pub fn rate_limited() -> bool {
    RATE_LIMITED.load(Ordering::Relaxed)
}

/// Write a deferral to the status ledger: the plain deferred record (the loop's back-off
/// ladder applies; the host names no window to hold to), no `health.toml` entry — a
/// 429 or a 5xx is weather, not a fault.
fn record_deferral(staging: &Staging, current_build: u64, outcome: &str) {
    RATE_LIMITED.store(true, Ordering::Relaxed);
    crate::status::set_delivery_note("deferred");
    crate::status::record(staging, current_build, outcome);
}

/// Whether `source` is the compiled-in public channel. GitHub slugs are
/// case-insensitive, so a development build's `[update] owner = "Alabsystems"` is the
/// public channel too.
fn is_default_channel(source: &Source) -> bool {
    is_slug(source, crate::DEFAULT_OWNER, crate::DEFAULT_REPO)
}

/// Whether `source` names `owner/repo`, the way GitHub compares slugs (ASCII
/// case-insensitively).
fn is_slug(source: &Source, owner: &str, repo: &str) -> bool {
    source.owner.eq_ignore_ascii_case(owner) && source.repo.eq_ignore_ascii_case(repo)
}

/// Record that the channel head was READ. This is what clears the "this machine cannot
/// update" latch: reading the channel is the property that matters.
fn note_readable(source: &Source) {
    RATE_LIMITED.store(false, Ordering::Relaxed);
    crate::unreadable::clear();
    // Once per process, and at DEBUG: every aterm process (the window and each
    // terminal session) runs this loop, so at INFO it was one line per launch saying
    // what every launch before it had said.
    if !ANNOUNCED_CHANNEL.swap(true, Ordering::Relaxed) {
        crate::debug(&format!(
            "updating from github.com/{}/{} over its unmetered download host with no \
             credential and no GitHub API request, on a {}-minute interval",
            source.owner,
            source.repo,
            crate::cadence::INTERVAL_SECS / 60
        ));
    }
}

/// The note a HEALTHY status outcome ends with — how often this machine looks,
/// ` · checks every 10 min`, and nothing about HOW it reads the channel (2026-09-23
/// audit, GT-16: Settings paints this sentence under "You're up to date.", and the
/// lane-and-credential clause it once carried read as three lines of jargon there).
/// One cadence, no knob, so the note is a constant of [`crate::cadence::INTERVAL_SECS`].
fn cadence_note() -> String {
    format!(
        " \u{b7} checks every {} min",
        crate::cadence::INTERVAL_SECS / 60
    )
}

/// The one message an operator gets when this machine cannot read its release
/// channel — the evergreen pointer answered 404. It must survive being read months
/// later out of `status.toml`, so it names the consequence, every possible cause, and
/// the remedy.
///
/// GitHub renders "no published release", "private" and "does not exist" identically
/// on the web host, so all three are named. The channel is read with no credential, so
/// no credential is offered as a remedy. A REPOINTED source (a development build's
/// `[update] owner`/`repo`) adds the one cause only it can have.
fn unreadable_explanation(code: u16, source: &Source) -> String {
    let repointed = if is_default_channel(source) {
        String::new()
    } else {
        format!(
            " This updater is repointed away from the compiled-in channel github.com/{}/{} \
             — check `[update] owner`/`repo` in aterm's config (honoured by a development \
             build only).",
            crate::DEFAULT_OWNER,
            crate::DEFAULT_REPO
        )
    };
    format!(
        "aterm cannot read its release channel github.com/{}/{} (HTTP {code}): the channel \
         has no published release, or the repository is private, was renamed, or does not \
         exist. Updates are read with no credential, so this machine will NEVER receive an \
         update until the channel is repaired at github.com/{}/{}, or aterm is reinstalled \
         from the channel's new location.{repointed}",
        source.owner, source.repo, source.owner, source.repo
    )
}

/// A GitHub Release, SYNTHESIZED from the pointer's tag ([`web_release`]) with the
/// derived tag-specific URLs, so the roster chain and the staging tail consume one
/// shape.
#[derive(Clone, Debug)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

/// A release asset: its name, and the derived `…/releases/download/<tag>/<name>` its
/// bytes come from.
#[derive(Clone, Debug)]
struct Asset {
    name: String,
    url: String,
}

/// Classify the channel head's tag.
///
/// The grammar is [`aterm_update_core::tag::parse_release_tag`] — the SAME
/// function the publisher (`aterm-release/src/publish.rs`) classifies with, so
/// client and publisher cannot disagree about which releases are candidates.
/// Only the client's diagnostic wording is here: canonical three-component
/// `vMAJOR.MINOR.PATCH` is a candidate, two-component tags are the retired
/// scheme ([`TagKind::Legacy`]), anything else fails the check closed.
fn parse_numeric_tag(tag: &str) -> Result<TagKind, String> {
    aterm_update_core::tag::parse_release_tag(tag).map_err(|error| match error {
        TagError::Malformed => format!("update candidate tag {tag:?} is not numeric dotted vN.N.N"),
        TagError::Overflow => {
            format!("update candidate tag {tag:?} has an out-of-range numeric component")
        }
    })
}

/// The tag contract: a release the client will install is spelled exactly
/// `vMAJOR.MINOR.PATCH`, matching the workspace version with its DEV component
/// reset to 0 (`VERSIONING.md`). `cargo ship cut` derives it from
/// `[workspace.package] version`, so the shipped app, the published source
/// snapshot, and the tag are one number.
///
/// `numeric` has already been proved three-component by [`parse_numeric_tag`];
/// [`aterm_update_core::tag::canonical_version`] re-derives the string, which
/// pins the *spelling* too, so `v01.2.3` can never be admitted alongside
/// `v1.2.3`.
fn canonical_authority_version(tag: &str, numeric: &[u64]) -> Result<String, String> {
    aterm_update_core::tag::canonical_version(tag, numeric).ok_or_else(|| {
        format!("authoritative update tag {tag:?} is not canonical vMAJOR.MINOR.PATCH")
    })
}

fn unique_asset_index(release: &Release, name: &str) -> Result<Option<usize>, String> {
    let mut matches = release
        .assets
        .iter()
        .enumerate()
        .filter(|(_, asset)| asset.name == name)
        .map(|(index, _)| index);
    let first = matches.next();
    if matches.next().is_some() {
        return Err(format!(
            "release {} has duplicate assets named {name}; update metadata is ambiguous",
            release.tag_name
        ));
    }
    Ok(first)
}

/// Resolve the authenticated manifest's DMG to one canonical asset identity.
/// Carrying this index forward prevents a later first-match lookup from making
/// duplicate GitHub assets order-dependent. The exact filename also keeps an
/// operator-signed path-like name out of the local staging path.
fn authoritative_dmg_index(
    release: &Release,
    manifest: &Manifest,
    canonical_version: &str,
) -> Result<usize, String> {
    let expected = format!("aterm-{canonical_version}.dmg");
    if manifest.dmg != expected {
        return Err(format!(
            "authoritative update {} names noncanonical DMG {:?}; expected {expected:?}",
            release.tag_name, manifest.dmg
        ));
    }
    unique_asset_index(release, &manifest.dmg)?.ok_or_else(|| {
        format!(
            "authoritative update {} has no exact asset named {:?}",
            release.tag_name, manifest.dmg
        )
    })
}

/// Which container a stage unpacks. Both carry the same signed `aterm.app` and
/// are verified identically once extracted; the difference is only that the zip
/// needs no `hdiutil`, and therefore no live bootstrap context (see
/// [`crate::install::stage_from_zip`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Container {
    Zip,
    Dmg,
}

impl Container {
    /// Transcript/ledger wording — these strings reach the user through status
    /// lines and the health ledger.
    fn label(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Dmg => "DMG",
        }
    }
}

/// The one exact release asset this check will download and stage from: its
/// container kind, its already-proved-unique asset index, its file name (which
/// becomes the local scratch name), and the manifest digest its bytes must match.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StageArtifact {
    container: Container,
    asset_index: usize,
    name: String,
    sha256: String,
}

/// Resolve the manifest's optional zip container to one canonical asset identity.
///
/// `Ok(None)` = the manifest declares no usable zip (no name, or a name with no
/// digest to check the bytes against). `Err` = it declares one this release
/// cannot resolve — a noncanonical name, or an asset that is missing/duplicated.
/// Neither outcome is fatal to the check: the DMG identity is proved separately
/// and staging falls back to it, which is what keeps releases published before
/// zip staging installable.
fn authoritative_zip_artifact(
    release: &Release,
    manifest: &Manifest,
    canonical_version: &str,
) -> Result<Option<StageArtifact>, String> {
    let Some(zip) = manifest.zip.as_deref() else {
        return Ok(None);
    };
    let Some(sha256) = manifest.zip_sha256.as_deref() else {
        // A container name with no digest is not stageable — there would be
        // nothing to check the downloaded bytes against.
        return Ok(None);
    };
    let expected = format!("aterm-{canonical_version}-mac.zip");
    if zip != expected {
        return Err(format!(
            "authoritative update {} names noncanonical zip {zip:?}; expected {expected:?}",
            release.tag_name
        ));
    }
    let asset_index = unique_asset_index(release, zip)?.ok_or_else(|| {
        format!(
            "authoritative update {} has no exact asset named {zip:?}",
            release.tag_name
        )
    })?;
    Ok(Some(StageArtifact {
        container: Container::Zip,
        asset_index,
        name: zip.to_string(),
        sha256: sha256.to_string(),
    }))
}

/// Choose the container to stage from: the zip when the manifest carries a
/// resolvable one, else the DMG.
///
/// The DMG identity is proved FIRST and unconditionally, so a release that fails
/// that check is refused exactly as before and the fallback is always available.
/// Preferring the zip is an availability decision, not a trust one — both digests
/// come from the same (optionally signed) manifest and both bundles go through
/// the same codesign/sealed-identity gate after extraction.
fn select_stage_artifact(
    release: &Release,
    manifest: &Manifest,
    canonical_version: &str,
) -> Result<StageArtifact, String> {
    let dmg_index = authoritative_dmg_index(release, manifest, canonical_version)?;
    match authoritative_zip_artifact(release, manifest, canonical_version) {
        Ok(Some(zip)) => return Ok(zip),
        Ok(None) => {}
        // A declared-but-unresolvable zip is a publishing defect, not a reason to
        // stop updating: say so and take the DMG.
        Err(error) => crate::warn(&format!("{error}; staging from the DMG instead")),
    }
    Ok(StageArtifact {
        container: Container::Dmg,
        asset_index: dmg_index,
        name: manifest.dmg.clone(),
        sha256: manifest.sha256.clone(),
    })
}

/// The release the channel head names, synthesized under the derived URLs
/// ([`web_release`]): its canonical version (the fetched manifest's `version` must
/// equal it), the release, and where its appcast sits.
#[derive(Debug)]
struct AuthoritativeRelease {
    version: String,
    release: Release,
    manifest_index: usize,
}

/// Everything the master-signed machine roster tier needs to run, bundled so the client
/// path takes ONE parameter rather than three and so a caller cannot supply two of them
/// and forget the third.
///
/// With `master_pubkeys` empty (a fork with no master of its own) the signature tier is
/// ABSENT — the same shape as an empty `APPLE_TEAM_ID` removing the Developer-ID tier
/// without loosening anything beside it. ARMED — this tree, since 2026-08-15 — the
/// master-signed roster is the SOLE authority over who may have signed the appcast.
pub(crate) struct RosterPolicy<'a> {
    /// The pinned paper master(s) — `pins::PAPER_MASTER_PUBKEYS` in production. Empty
    /// means the tier is absent.
    pub master_pubkeys: &'a [&'a str],
    /// The highest `roster_seq` this client has ever durably recorded. THE replay defence
    /// for a client that has already seen a newer roster; worth nothing to a fresh
    /// install, which is what the roster's own `valid_until` is for.
    pub floor_seq: u64,
    /// Injected wall clock (unix seconds), so the freshness gate stays pure and every
    /// expiry case is testable without waiting for one.
    pub now_unix: i64,
    /// Re-read the DURABLE floor immediately before admission, closing the TOCTOU that
    /// `floor_seq` alone leaves open: that snapshot is taken before any network I/O, and
    /// a concurrent instance of this process may ratchet the durable floor past it while
    /// the roster assets download. Admission takes the max of the snapshot and this
    /// re-read, so a roster generation a concurrent check has already superseded is
    /// refused rather than admitted through a stale snapshot. `None` (tests that are not
    /// about the race) means the snapshot alone decides, which is never LOOSER than the
    /// snapshot — the hook can only raise the floor.
    pub floor_refresh: Option<&'a dyn Fn() -> u64>,
}

impl RosterPolicy<'static> {
    /// The tier switched off — the fixture every test that is not about the roster uses.
    /// `#[cfg(test)]` because production always builds its policy from
    /// `pins::PAPER_MASTER_PUBKEYS`.
    #[cfg(test)]
    pub(crate) const INERT: RosterPolicy<'static> = RosterPolicy {
        master_pubkeys: &[],
        floor_seq: 0,
        now_unix: 0,
        floor_refresh: None,
    };
}

/// The marker a refusal carries when it is THIS BUILD's trust anchor that cannot
/// verify the channel, rather than the channel that is broken (2026-09-14): the
/// pinned paper master cannot verify the machine roster (a master rotation this build
/// predates). That is permanent for the build — the head is the one candidate and
/// nothing falls back — so the persistent wording must name the one remedy, a
/// reinstall, instead of "fixed at the publisher". A forged release lands
/// on the same arms and gets the same advice; a reinstall from the publisher's own
/// site is right for it too.
pub const STALE_ANCHOR_KEY: &str = "cannot verify the channel's releases";

/// The remedy a stranded client is told, beside [`STALE_ANCHOR_KEY`].
pub const STALE_ANCHOR_REMEDY: &str = "reinstall aterm from the current release (tools/install.sh, \
     or drag it from the release DMG) to get a build whose anchor matches the channel";

/// Whether a `manifest`-class reason is the stranded-client refusal: the build's
/// anchor, not the publisher, is what has to change.
pub fn is_stale_anchor_refusal(reason: &str) -> bool {
    reason.contains(STALE_ANCHOR_KEY)
}

#[derive(Default)]
struct AuthoritativeFetch {
    /// Manifest, its release, and the already-proved unique canonical container
    /// (zip when the manifest carries a resolvable one, else the DMG).
    selected: Option<(Manifest, Release, StageArtifact)>,
    appcast_fetch_error: bool,
    /// The `appcast_fetch_error` was the MANIFEST ITSELF answering 404 on the download
    /// host: the release the pointer names exists but carries no `aterm-appcast.toml`
    /// (a source-only release). Not a pipeline fault: the check records "channel head
    /// <tag> has no app manifest yet" and ends, and the next check reads the head again.
    appcast_missing: bool,
    /// The `appcast_fetch_error` was a `github.com` 429 on an asset fetch, not a broken
    /// download: weather, and never a `pipeline`-class failure.
    asset_fetch_rate_limited: bool,
    manifest_rejected: bool,
    /// WHY the manifest was rejected, in the words the log got (2026-09-14). The
    /// health ledger used to book every rejection as the one fixed string
    /// "manifest(s) fetched but rejected (signature/parse)", so `health.toml`,
    /// `status.toml` and the pull-down could not tell a forged release from a
    /// revoked signer from a client whose compiled-in anchor simply predates a key
    /// or master rotation — and the last of those, the STRANDED client, was told
    /// the publisher was broken and never that a reinstall is its only way out
    /// ([`STALE_ANCHOR_KEY`]).
    rejection_reason: Option<String>,
    /// Candidate-manifest fetches only. Detached-signature downloads are a
    /// subordinate verification step and intentionally do not increment this.
    #[cfg(test)]
    manifest_fetch_attempts: u32,
    /// WHICH MACHINE SIGNED, when the roster tier is armed and the chain passed. `None`
    /// with an unpinned master (the tier is absent) and `None` on any rejection, because
    /// a rejection never produces a `selected` release either.
    attribution: Option<aterm_update_core::roster::Attribution>,
    /// The `roster_seq` of the master-verified roster that passed ADMISSION (the replay
    /// floor and the freshness window), for the caller to ratchet into the durable floor.
    ///
    /// Set on OBSERVATION — the moment `admit` passes — NOT on successful artifact
    /// authorization. The difference is the whole replay defence: a seq-10 roster that
    /// REVOKES the appcast's signer refuses the release, and if the floor only ratcheted
    /// on acceptance, a replayed still-fresh seq-9 roster would then re-authorize the very
    /// machine the owner just revoked. Having SEEN generation 10, this client must refuse
    /// 9 forever, whether or not it went on to install anything.
    observed_roster_seq: Option<u64>,
    /// The machine IDS the admitted roster REVOKES, carried out on the same
    /// observation as [`Self::observed_roster_seq`] and for the same reason.
    ///
    /// The seq says a newer generation was seen; this says what that generation
    /// withdrew. The caller needs it to retract an already-staged build, which is a
    /// question only this lane can answer: revocation is a LIST inside the roster
    /// document, and the apply lane holds nothing but the floor NUMBER.
    observed_revocations: Vec<String>,
}

/// Unix seconds now, for the roster's freshness and per-machine expiry gates.
///
/// The fallback is the OPPOSITE of `install::unix_now_secs`, and deliberately so. That
/// one returns 0 on a broken clock because zero makes every retry deadline look passed,
/// which is the safe direction for a retry budget. Here zero would read as 1970 — before
/// every conceivable `valid_until` — so a lapsed roster would be ACCEPTED. A clock we
/// cannot read must fail CLOSED, so this returns `i64::MAX`, which makes every window
/// look expired and refuses the update.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(i64::MAX, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl AuthoritativeFetch {
    /// Refuse the candidate: WARN the reason and carry it to the ledger. Every
    /// rejection arm goes through here so the reason `health.toml` records is the
    /// one the log printed.
    fn refuse(&mut self, reason: String) {
        crate::warn(&reason);
        self.manifest_rejected = true;
        self.rejection_reason = Some(reason);
    }
}

/// Why the roster chain did not produce an attribution — split into the two classes the
/// health ledger must never confuse.
///
/// Both are REFUSALS; neither is ever a fallthrough. The distinction is only about what
/// the operator is told, and it matters because the wordings are not interchangeable:
/// a `pipeline`-class transport failure is postponed-and-will-retry, while a `manifest`
/// -class refusal escalates to "this Mac cannot install any release until that is fixed
/// at the publisher". Once the roster is the SOLE authority, a flaky network fetching
/// `aterm-machines.toml` would otherwise accuse the publisher of shipping a bad release.
enum RosterFailure {
    /// An asset could not be FETCHED. Nothing has been judged; retrying may work.
    Transport(String),
    /// The chain RAN and refused: no roster, a bad master signature, a stale or
    /// rolled-back generation, a revoked machine, an unauthorized signer.
    Refused(String),
}

/// Run the master-signed machine-roster chain for one candidate release, returning WHICH
/// machine signed its appcast.
///
/// With the master ARMED this is the ONLY thing that authorizes a release.
///
/// Cheapest-first and fail-closed throughout — the ordering IS the design, so it is worth
/// reading as a list:
///
/// 1. both roster assets must be present on the release. Free and structural: no roster
///    means no authority, and an armed anchor never degrades to "unsigned is fine".
/// 2. download them, under tight caps. A roster is a few hundred bytes per machine and a
///    detached Ed25519 signature is exactly 64, so 64 KiB and 4 KiB are ceilings rather
///    than fits.
/// 3. verify the roster under the pinned paper master. THE FIRST CRYPTO.
/// 4. parse it — only from `VerifiedRoster`, which has no public constructor, so parsing
///    unverified roster bytes does not type-check.
/// 5. `admit`: the durable `roster_seq` floor, then the freshness window. Both cheap,
///    both before any artifact crypto.
/// 6. `authorize_appcast`: revoked and expired machines are removed from the candidate
///    set FIRST — so a revoked machine's perfectly valid signature is never even checked —
///    and the survivors verify the appcast. THE SECOND CRYPTO.
///
/// Every error is a refusal. There is no path through here that returns "accept anyway".
///
/// `observed_roster_seq` is the OBSERVATION out-parameter: it is set the moment a
/// master-verified roster passes `admit`, before — and regardless of — the appcast
/// authorization that follows. The caller ratchets it into the durable floor even when
/// this function then refuses the release, because a roster that revokes the release's
/// signer is exactly the generation the floor must remember (see
/// [`AuthoritativeFetch::observed_roster_seq`]).
fn authorize_by_roster(
    candidate: &AuthoritativeRelease,
    appcast: &[u8],
    policy: &RosterPolicy<'_>,
    download: &mut impl FnMut(&str, u64) -> Result<Vec<u8>, String>,
    observed_roster_seq: &mut Option<u64>,
    observed_revocations: &mut Vec<String>,
) -> Result<aterm_update_core::roster::Attribution, RosterFailure> {
    use RosterFailure::{Refused, Transport};
    use aterm_update_core::roster::{ROSTER_ASSET, ROSTER_SIG_ASSET, Roster, verify_roster};

    // (1) Structural, free: the appcast signature and both roster assets are present.
    let signature_index = unique_asset_index(&candidate.release, APPCAST_SIG_ASSET)
        .map_err(|e| Refused(format!("locate the appcast signature: {e}")))?
        .ok_or_else(|| {
            Refused(format!(
                "the paper master is pinned but {} carries no appcast signature",
                candidate.release.tag_name
            ))
        })?;
    let roster_index = unique_asset_index(&candidate.release, ROSTER_ASSET)
        .map_err(|e| Refused(format!("locate {ROSTER_ASSET}: {e}")))?
        .ok_or_else(|| {
            Refused(format!(
                "the paper master is pinned but {} carries no {ROSTER_ASSET}",
                candidate.release.tag_name
            ))
        })?;
    let roster_sig_index = unique_asset_index(&candidate.release, ROSTER_SIG_ASSET)
        .map_err(|e| Refused(format!("locate {ROSTER_SIG_ASSET}: {e}")))?
        .ok_or_else(|| {
            Refused(format!(
                "{} carries a machine roster with no master signature",
                candidate.release.tag_name
            ))
        })?;

    // (2) Bounded transport. A failure here has judged NOTHING — see [`RosterFailure`].
    let roster_bytes = download(&candidate.release.assets[roster_index].url, 65_536)
        .map_err(|e| Transport(format!("fetch {ROSTER_ASSET}: {e}")))?;
    let roster_sig = download(&candidate.release.assets[roster_sig_index].url, 4096)
        .map_err(|e| Transport(format!("fetch {ROSTER_SIG_ASSET}: {e}")))?;
    let appcast_sig = download(&candidate.release.assets[signature_index].url, 4096)
        .map_err(|e| Transport(format!("fetch appcast signature: {e}")))?;

    // (3)(4) Verify under the paper master, then parse — in that order, by construction.
    let verified =
        verify_roster(policy.master_pubkeys, roster_bytes, &roster_sig).map_err(|e| {
            // The stranded-client arm of the armed tier: a master rotation this build
            // predates (or a roster that is not the publisher's). The build cannot
            // recover by retrying; the wording names the reinstall (2026-09-14).
            Refused(format!(
                "machine roster did not verify under the pinned master ({e:?}); this \
                 build's trust anchor {STALE_ANCHOR_KEY} (a rotation it predates, or a \
                 roster that is not the publisher's) — {STALE_ANCHOR_REMEDY}"
            ))
        })?;
    if verified.master_index() != 0 {
        // Never a rejection: a hit on a non-head master is a rotation in flight. Saying so
        // makes a STALLED rotation visible instead of silent until updates stop. At INFO,
        // not WARN: the roster VERIFIED under a pinned master, so this is rotation
        // visibility, not verification degradation — an unverifiable roster is the
        // `Refused` right above.
        crate::log(&format!(
            "the machine roster was signed by master key #{}, not the current one — a \
             master rotation is in progress or incomplete",
            verified.master_index()
        ));
    }
    let roster = Roster::parse(&verified)
        .map_err(|e| Refused(format!("machine roster is unusable ({e:?})")))?;

    // (5) Replay floor, then freshness. The floor is RE-READ here when the caller
    // provides a reader, because `policy.floor_seq` is a snapshot taken before the
    // downloads above and a concurrent instance may have ratcheted the durable floor
    // past it in the meantime — admitting against the stale snapshot would accept a
    // roster generation that instance has already superseded. The max keeps the hook
    // strictly tightening: it can only raise the floor, never lower it.
    let floor_seq = policy
        .floor_refresh
        .map_or(policy.floor_seq, |read| read().max(policy.floor_seq));
    roster
        .admit(floor_seq, policy.now_unix)
        .map_err(|e| Refused(format!("machine roster refused ({e:?})")))?;
    // THE OBSERVATION RATCHET. This roster is master-verified and admitted, so its
    // generation has been SEEN — recorded here, before the appcast authorization,
    // so a refusal below (a revoked signer, above all) still advances the floor.
    *observed_roster_seq = Some(roster.roster_seq);
    // Observed on the SAME event, so a roster that refuses this release below still
    // tells the caller whom it withdrew.
    observed_revocations.clone_from(&roster.revoked);

    // (6) Deny-list before crypto, then the artifact signature.
    roster
        .authorize_appcast(appcast, &appcast_sig, policy.now_unix)
        .map_err(|e| {
            Refused(format!(
                "no machine on the roster signed this release ({e:?})"
            ))
        })
}

/// Fetch and validate the one candidate the channel head names. Nothing older is ever
/// downloaded: a head that cannot be authorized ends the check.
fn fetch_authoritative_release(
    candidate: Option<AuthoritativeRelease>,
    download: &mut impl FnMut(&str, u64) -> Result<Vec<u8>, String>,
    roster: &RosterPolicy<'_>,
) -> AuthoritativeFetch {
    let mut fetched = AuthoritativeFetch::default();
    let Some(candidate) = candidate else {
        return fetched;
    };
    let manifest_url = candidate.release.assets[candidate.manifest_index]
        .url
        .clone();

    #[cfg(test)]
    {
        fetched.manifest_fetch_attempts = 1;
    }
    let bytes = match download(&manifest_url, 5_000_000) {
        Ok(bytes) => bytes,
        Err(error) => {
            fetched.appcast_missing = aterm_update_core::download_error_is_not_found(&error);
            // A 404 is the source-only release's ordinary shape: the check records the
            // head as having no app manifest yet ([`record_head_without_app`]), healthy,
            // and reads it again next time. Anything else here is a fetch that failed.
            if fetched.appcast_missing {
                crate::debug(&format!("fetch appcast: {error}"));
            } else {
                crate::warn(&format!("fetch appcast: {error}"));
            }
            fetched.appcast_fetch_error = true;
            fetched.asset_fetch_rate_limited =
                aterm_update_core::download_error_is_rate_limit(&error);
            return fetched;
        }
    };
    // WHO IS ALLOWED TO HAVE SIGNED THIS APPCAST — decided by the PAPER MASTER ANCHOR
    // alone. Unpinned (a fork with no master of its own), the signature tier is absent.
    // Armed — this tree — the master-signed roster decides, and it decides ALONE: there is
    // no compiled-in key it could fall back to, so a revoked machine stays revoked on
    // every build, and adding a machine is a LOCAL act (mint, roster, publish).
    //
    // The chain runs here, over the RAW bytes and before the parse below, which is the
    // only correct place for it: the identity claims INSIDE the appcast are not bound to
    // anything until something has verified the bytes that carry them. The cheap identity
    // cross-check happens immediately after the parse (`bind`, below).
    //
    // The observed sequence lands in `fetched` on EVERY arm, including the refusals: it
    // was set the moment a master-verified roster passed admission, and the caller's
    // ratchet must advance on that observation even when the release itself is refused
    // (see `AuthoritativeFetch::observed_roster_seq`).
    let attribution = if roster.master_pubkeys.is_empty() {
        None
    } else {
        let mut observed_roster_seq = None;
        let mut observed_revocations = Vec::new();
        let authorized = authorize_by_roster(
            &candidate,
            &bytes,
            roster,
            download,
            &mut observed_roster_seq,
            &mut observed_revocations,
        );
        fetched.observed_roster_seq = observed_roster_seq;
        fetched.observed_revocations = observed_revocations;
        match authorized {
            Ok(who) => Some(who),
            // FAIL CLOSED. An attacker who could suppress the roster assets must not
            // downgrade an armed client to anything weaker.
            Err(RosterFailure::Transport(error)) => {
                crate::warn(&format!(
                    "{error}; refusing authoritative {}",
                    candidate.release.tag_name
                ));
                fetched.appcast_fetch_error = true;
                fetched.asset_fetch_rate_limited =
                    aterm_update_core::download_error_is_rate_limit(&error);
                return fetched;
            }
            Err(RosterFailure::Refused(error)) => {
                fetched.refuse(format!(
                    "{error}; refusing authoritative {}",
                    candidate.release.tag_name
                ));
                return fetched;
            }
        }
    };

    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            fetched.refuse(format!(
                "authoritative {} appcast is not UTF-8: {error}",
                candidate.release.tag_name
            ));
            return fetched;
        }
    };
    match Manifest::parse(&text) {
        Ok(manifest) if manifest.version == candidate.version => {
            // THE IDENTITY BIND, over bytes that are authenticated by the time it runs.
            // A genuine signature by one machine cannot be relabelled as another (the id
            // is inside the signed bytes), and a machine cannot claim someone else's id
            // (the roster maps id to key). Both directions are string compares.
            if let Some(who) = &attribution
                && let Err(reject) = who.bind(manifest.machine_id.as_deref(), manifest.roster_seq)
            {
                fetched.refuse(format!(
                    "authoritative {} verified under machine {} but its own attribution \
                     does not agree ({reject:?}); refusing",
                    candidate.release.tag_name, who.machine_id
                ));
                return fetched;
            }
            if let Some(who) = &attribution {
                // ATTRIBUTION, recorded where a human reads it: the owner asked to be able
                // to track which computer does what. The durable record is the status
                // file's (`check_and_stage` writes it beside the release); the log line is
                // once per release per process, at DEBUG (2026-09-23): the same fact was
                // logged on every check that read the release and again at every launch —
                // a hundred lines for one release in the owner's log.
                if crate::first_in_process(&format!("signer {} {who}", candidate.release.tag_name))
                {
                    crate::debug(&format!(
                        "authoritative {} was signed by machine {who}",
                        candidate.release.tag_name
                    ));
                }
                fetched.attribution = Some(who.clone());
            }
            match select_stage_artifact(&candidate.release, &manifest, &candidate.version) {
                Ok(artifact) => {
                    fetched.selected = Some((manifest, candidate.release, artifact));
                }
                Err(error) => fetched.refuse(error),
            }
        }
        Ok(manifest) => fetched.refuse(format!(
            "authoritative {} carries manifest version {:?}, expected {:?}",
            candidate.release.tag_name, manifest.version, candidate.version
        )),
        Err(error) => fetched.refuse(format!(
            "parse authoritative {} appcast: {error}",
            candidate.release.tag_name
        )),
    }
    fetched
}

/// Whether the roster generation that authorized this check's release has been
/// SUPERSEDED by the durable floor while the check was in flight.
///
/// `observed` is the sequence the chain admitted for this release — `None`, which can
/// never be superseded, whenever no master-verified roster reached `admit`: the UNARMED
/// tier (a fork with no pinned master), or an armed tier whose roster was unfetchable or
/// refused. This tree has been armed since 2026-08-15, so `None` here is the second case,
/// not the first. `floor_now` is a FRESH read of the
/// durable floor. This run's own ratchet write makes `floor_now >= observed` in the
/// quiescent case, so a strict `<` fires only when a CONCURRENT instance recorded a newer
/// generation — at which point staging an artifact authorized under the older generation
/// would act on authority this client already knows is withdrawn. The refusal is
/// transient: the next check re-runs under the advanced floor.
/// Whether an admitted roster's revocations withdraw the machine that authorized the
/// staged build.
///
/// Split out so the decision is testable — the surrounding check lane fetches over the
/// network — and so the `None` case is stated once: a marker that records NO machine
/// predates the attribution fields, and an unnamed machine can never be matched against
/// a revocation list. It is left alone rather than retired, because "I cannot tell who
/// signed this" is not evidence of withdrawal, and guessing costs a re-download on every
/// check forever.
fn revocation_withdraws_stage(revocations: &[String], staged_machine: Option<&str>) -> bool {
    staged_machine.is_some_and(|machine| revocations.iter().any(|id| id == machine))
}

fn roster_authority_superseded(observed: Option<u64>, floor_now: u64) -> bool {
    observed.is_some_and(|seq| seq < floor_now)
}

/// Whether a fully published local stage makes this release download redundant.
/// Both the optimistic pre-lock check and the authoritative under-lock re-check
/// call this exact predicate. For the same build, bind the marker back to the
/// selected manifest's commit and DMG digest; a strictly newer publishable stage
/// already supersedes the selected release.
fn publishable_stage_covers(staging: &Staging, manifest: &Manifest) -> bool {
    Ready::read_publishable(staging).is_some_and(|ready| {
        ready.build_number > manifest.build_number
            || (ready.build_number == manifest.build_number
                && ready.dmg_sha256.eq_ignore_ascii_case(&manifest.sha256)
                && ready.commit.as_deref().is_some_and(|ready_commit| {
                    manifest.commit.as_deref().is_some_and(|manifest_commit| {
                        ready_commit
                            .trim()
                            .eq_ignore_ascii_case(manifest_commit.trim())
                    })
                }))
    })
}

/// Record the "a verified stage already covers this candidate" decision in
/// `status.toml`.
///
/// WHAT WENT WRONG: both [`publishable_stage_covers`] short-circuits returned
/// `Ok(None)` having written NOTHING. The attribution note ("authoritative release
/// signed by machine m3") is written earlier in the SAME check, before the staging
/// decision — and with the paper master armed that note lands on every cycle. So on a
/// machine holding a pending, verified stage — which is its steady state until the
/// apply lane runs — the last decision in `status.toml` was a note about who SIGNED a
/// release, and the one fact an operator reading this file was after, that a build is
/// already on disk waiting to apply, appeared nowhere. Every other terminal outcome of
/// the check records its decision; these two were the hole.
fn record_covered_stage_status(staging: &Staging, current_build: u64, manifest: &Manifest) {
    // Re-read the marker rather than plumbing a value out of the predicate: the marker
    // IS the authority for what is on disk, and this read is local and cheap. It can
    // legitimately have vanished since the predicate ran (a concurrent retire), and in
    // that case we still record a decision — naming the candidate instead of inventing
    // a stage — because leaving the previous line standing is the very failure above.
    let msg = match Ready::read_publishable(staging) {
        Some(ready) => covered_stage_line(
            staging,
            current_build,
            &ready.version,
            ready.build_number,
            manifest.build_number,
        ),
        None => format!(
            "a verified stage already covers release build {}",
            manifest.build_number
        ),
    };
    crate::status::record(staging, current_build, &msg);
}

/// The one-line verdict for a stage the check found already in place — WITH the
/// apply lane's standing answer about that very build (2026-09-14). The check
/// lane used to write "verified and ready to apply" over the apply lane's
/// "did not apply: …" every half hour: for the ~6 h between two stand-downs on
/// 2026-09-14 the ledger's outcome alternated back to "ready" twenty times
/// while the apply lane had exhausted its budget on that build. The ledger
/// knows both facts; the line says both.
fn covered_stage_line(
    staging: &Staging,
    current_build: u64,
    version: &str,
    staged_build: u64,
    release_build: u64,
) -> String {
    let mut msg = format!("staged {version} (build {staged_build}) — verified and ready to apply");
    // The release build is news only when it is not the staged one.
    if release_build != staged_build {
        msg.push_str(&format!(
            "; release build {release_build} needs no download"
        ));
    }
    let ledger = crate::health::Health::read(&staging.health());
    if ledger.last_apply_failure_target_build == staged_build
        && ledger.apply_failures_for_target > 0
    {
        msg.push_str(&format!(
            "; it failed to install {} time(s): {}",
            ledger.apply_failures_for_target, ledger.last_apply_error
        ));
    } else if ledger.apply_refusal_applies_to(current_build)
        && !ledger.last_apply_refusal.is_empty()
    {
        msg.push_str(&format!(
            "; the last install was refused: {}",
            ledger.last_apply_refusal
        ));
    }
    msg
}

/// The download path's counterpart to `install::sweep_stale_mounts` /
/// `install::sweep_stale_extracts`: reclaim container scratch a previously-killed run
/// leaked. Every removal on this path is keyed to the CURRENT artifact's name, and
/// those names are version-keyed (`aterm-<version>-mac.zip{.part}`), so a partial —
/// or a fully-downloaded container abandoned in the window between the finalize
/// rename and the post-stage removal — for a version the channel has since moved past
/// is unreachable by every other code path, forever. That is up to
/// `RELEASE_ASSET_DOWNLOAD_BOUND` of the single largest file in the pipeline sitting
/// in the user's Application Support, and `Staging::retire_published` deliberately
/// never touches `download/`, so this is the only place it can be reclaimed.
///
/// Deleting every regular file (rather than sparing the current names) is what makes
/// it a sweep and subsumes the pre-download `remove_file` it replaced: the pipeline
/// always downloads into `{name}.part` and renames over the container, so no path
/// ever reuses bytes already in this dir. Directories are left alone — nothing puts
/// one here, and a recursive delete is not a risk worth taking for scratch.
///
/// Callers MUST hold `staging.stage_lock`: the staging critical section is this
/// directory's only writer, and the apply/retire lane must keep its hands off it.
fn sweep_download_scratch(staging: &Staging) {
    let Ok(entries) = std::fs::read_dir(&staging.download) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// An OPEN stage-failure retry window, and what it is allowed to stop.
///
/// The window in `failed.toml` exists for exactly one purpose: stop us
/// re-DOWNLOADING (up to 512 MB of) bytes that already refused to become a verified
/// bundle. It says nothing about a bundle that is ALREADY downloaded, verified,
/// extracted and published — applying that costs no bandwidth, is not the failure
/// the memo recorded, and is counted and remedied separately
/// (`Health::apply_failures`, cleared only by a real apply, escalating on its own
/// streak). The two failures must not share a timer, and this type is where that
/// separation is expressed.
///
/// THE REGRESSION: a machine with `staged_build=1785510971`,
/// `staged_version=0.10.0`, `relaunch_ready=true` and `failing_applies=0` reported
/// `skipping build 1785510971 for another 1387m (failed to stage 4 time(s))`. Four
/// stage failures of the release's CURRENT artifact (the shape that produces this:
/// a re-publish of the same build under a new digest, which
/// [`publishable_stage_covers`] no longer covers) had opened the 24 h re-download
/// window — and that window was also refusing to apply the earlier, verified 0.10.0
/// bundle that was sitting on disk marked ready.
#[derive(Debug)]
struct StageBackoff {
    /// Seconds until the candidate named by the manifest may be downloaded again.
    /// Meaningless when `quarantined` — that window never opens.
    retry_in_secs: u64,
    /// Consecutive stage failures recorded for that candidate (at least 1).
    attempts: u32,
    /// The candidate is QUARANTINED: it was applied, crash-looped, and was reverted.
    /// This is not a timed backoff and must not be reported as one — nothing on this
    /// machine will retry it; the channel has to offer something else.
    quarantined: bool,
    /// A published local stage that is strictly newer than the running build, if
    /// one exists. The backoff never gates this: the check reports it so the apply
    /// lane runs on it this cycle.
    applicable: Option<Ready>,
}

impl StageBackoff {
    /// The operator-facing line for an open window. It names the lane actually
    /// being skipped — the RE-STAGE — and then says, separately, whether an apply
    /// is being skipped along with it. The old wording ("skipping build N for
    /// another 1387m") named neither, so it read as "nothing is happening" while
    /// sitting beside a `staged_build` that was ready the whole time.
    fn status_line(&self, candidate_build: u64) -> String {
        let restage = if self.quarantined {
            // A quarantine has no clock, and saying "retrying automatically" about one
            // would be a lie an operator could only discover by waiting forever.
            format!(
                "build {candidate_build} is quarantined: it was applied, failed to start \
                 cleanly, and was reverted — this machine will not retry it (a newer build, \
                 or a re-publish under a different digest, clears it)"
            )
        } else {
            format!(
                "skipping re-stage of build {candidate_build} for another {}m (failed to stage \
                 {} time(s); retrying automatically, or re-publish to retry now)",
                self.retry_in_secs.div_ceil(60),
                self.attempts
            )
        };
        match &self.applicable {
            None => format!("{restage}; no verified stage to apply"),
            Some(ready) => format!(
                "{restage}; NOT skipping apply: staged {} (build {}) is verified and \
                 ready to apply",
                ready.version, ready.build_number
            ),
        }
    }
}

/// Read the stage-failure memo and decide what it may stop for this candidate.
/// `None` means no window is open for it (absent memo, another artifact, or the
/// deadline has passed) — download and stage as usual.
///
/// `applicable` is gated by [`Ready::read_publishable`], the same local read the
/// status and apply surfaces share: the marker must carry a canonical identity AND
/// name a real published bundle whose sealed `Info.plist` rebinds to it. Full
/// codesign/Team-ID re-verification stays where it has always been — under the
/// apply lock on the apply path itself, which is the authority — so a backed-off
/// check never spawns a verification helper per cycle to answer this.
fn stage_backoff(
    staging: &Staging,
    manifest: &Manifest,
    current_build: u64,
    now: u64,
) -> Option<StageBackoff> {
    let memo = crate::manifest::FailedMark::read(&staging.failed())?;
    if !memo.suppresses(manifest.build_number, &manifest.sha256, now) {
        return None;
    }
    Some(StageBackoff {
        retry_in_secs: memo.retry_in_secs(now),
        attempts: memo.attempts.max(1),
        quarantined: memo.is_quarantine(),
        applicable: Ready::read_publishable(staging)
            .filter(|ready| ready.build_number > current_build),
    })
}

/// The exact asset names a release carries for the updater. The roster chain looks
/// them up by name; the check DERIVES their URLs from them ([`web_release`]).
const APPCAST_ASSET: &str = "aterm-appcast.toml";
const APPCAST_SIG_ASSET: &str = "aterm-appcast.toml.sig";

// ---------------------------------------------------------------------------------
// THE CHECK
// ---------------------------------------------------------------------------------

/// The injected pointer transport: `url → (status, Location)`. Production passes
/// [`aterm_update_core::head_no_redirect`]; tests pass a counting fake, which is what
/// makes "zero `api.github.com` requests" a measurement rather than a claim.
type HeadFetch<'a> = &'a mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>;

/// What the evergreen pointer said the channel head is, relative to what this ledger
/// last authorized.
enum WebHead {
    /// The pointer names the tag this ledger last authorized: nothing to fetch.
    Unchanged { tag: String },
    /// A tag this ledger has not authorized (or nothing was recorded): the synthesized
    /// candidate whose every asset URL is the derived tag-specific one.
    New(AuthoritativeRelease),
    /// A NON-failure end, already recorded — the channel has no published release
    /// (announced, loud), the host asked us to wait (deferred), or the head is not an
    /// app release. The check is over.
    Ended,
}

/// The names of every asset a check may fetch for release `tag`, in the index order
/// [`web_release`] fixes: appcast, its signature, the roster, its signature, the DMG,
/// the zip. Names, not URLs — the URLs are derived from these under the strict charset,
/// so a name is the only thing a fetch can be addressed by.
fn web_asset_names(version: &str) -> [String; 6] {
    [
        APPCAST_ASSET.to_string(),
        APPCAST_SIG_ASSET.to_string(),
        aterm_update_core::roster::ROSTER_ASSET.to_string(),
        aterm_update_core::roster::ROSTER_SIG_ASSET.to_string(),
        format!("aterm-{version}.dmg"),
        format!("aterm-{version}-mac.zip"),
    ]
}

/// The candidate for the pointer's `tag`: every asset's URL the DERIVED
/// `…/releases/download/<tag>/<name>` (a pure function of the trusted source and the
/// pointer's tag — never a server string) and the canonical version the tag names,
/// which the fetched manifest's `version` must then equal.
fn web_release(source: &Source, tag: &str) -> Result<AuthoritativeRelease, String> {
    let TagKind::Candidate(numeric) = parse_numeric_tag(tag)? else {
        return Err(format!(
            "the channel head {tag:?} is a retired two-component release tag"
        ));
    };
    let version = canonical_authority_version(tag, &numeric)?;
    let mut assets = Vec::with_capacity(6);
    for name in web_asset_names(&version) {
        let url =
            aterm_update_core::cdn::release_download_url(&source.owner, &source.repo, tag, &name)
                .ok_or_else(|| format!("no download URL can be derived for {name:?} of {tag}"))?;
        assets.push(Asset { name, url });
    }
    Ok(AuthoritativeRelease {
        version,
        release: Release {
            tag_name: tag.to_string(),
            assets,
        },
        manifest_index: 0,
    })
}

/// Record that the channel head is not an app release — a release that carries no
/// `aterm-appcast.toml` (a source-only release), or a tag this client does not install
/// from — and end the check. A HEALTHY outcome: the channel was read and nothing is
/// broken on this machine. The tag is NOT recorded as authorized, so the next check
/// reads the head again and picks up the app cut the moment the publisher's release
/// carries it. No listing is consulted to look past the head: `latest` is the
/// publisher's to point at an installable release.
fn record_head_without_app(staging: &Staging, current_build: u64, tag: &str) {
    crate::health::Health::record_success(&staging.health());
    crate::status::record(
        staging,
        current_build,
        &format!("channel head {tag} has no app manifest yet — nothing to install from it"),
    );
}

/// ONE unmetered HEAD, and the decision it yields: a redirect to a canonical tag is the
/// head; the ledger's `latest_tag` decides whether anything else is fetched; a 404 is
/// the loud standing state; a 429 or 5xx is a deferral; a redirect to a tag this client
/// does not install from is a head with no app release; a transport failure is the
/// historical `network`-class failure.
///
/// `known_tag` is what `status.toml` recorded as last authorized. The comparison is
/// string equality on tags this function already proved canonical, so `v0.74.0`
/// recorded and `v0.74.0` pointed at is "unchanged", and nothing else is.
fn resolve_web_head(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    known_tag: Option<&str>,
    head: HeadFetch<'_>,
) -> Result<WebHead, String> {
    use aterm_update_core::pointer::{self, PointerError};
    let pointer = pointer::resolve_with(
        &source.owner,
        &source.repo,
        APPCAST_ASSET,
        &pointer::canonical_app_tag,
        head,
    );
    let pointer = match pointer {
        Ok(pointer) => pointer,
        Err(PointerError::NoRelease { .. }) => {
            crate::unreadable::announce(
                staging,
                current_build,
                &unreadable_explanation(404, source),
            );
            return Ok(WebHead::Ended);
        }
        Err(error @ PointerError::Transient { .. }) => {
            record_deferral(
                staging,
                current_build,
                &format!("update check deferred: {error}"),
            );
            return Ok(WebHead::Ended);
        }
        // This repository, this asset name, a safe tag this client does not install
        // from (an index cut published as a normal release): the channel was read, and
        // its head is not an app release.
        Err(PointerError::OtherTag { tag }) => {
            note_readable(source);
            record_head_without_app(staging, current_build, &tag);
            return Ok(WebHead::Ended);
        }
        // A refused redirect, an unexpected status, an unsafe source, a transport
        // failure: the historical `network`-class failure. A refusal in particular is
        // NOT interpreted — the pointer named something this client will not follow,
        // and the check ends without a request to it.
        Err(error) => {
            let message = error.to_string();
            crate::health::Health::record_failure(&staging.health(), "network", &message);
            return Err(message);
        }
    };
    note_readable(source);
    if known_tag == Some(pointer.tag.as_str()) {
        return Ok(WebHead::Unchanged { tag: pointer.tag });
    }
    let candidate = web_release(source, &pointer.tag)?;
    // The pointer's own `Location` and the candidate's derived appcast URL are the same
    // string by construction (`parse_location` proved it); assert the construction
    // rather than trust it, because every later GET is addressed by the derived one.
    debug_assert_eq!(candidate.release.assets[0].url, pointer.location);
    Ok(WebHead::New(candidate))
}

/// THE URL CROSS-CHECK: the signed manifest names its container's
/// download URL, and that URL must name THE TAG the pointer chose and THE CONTAINER
/// the manifest itself names. The manifest field is never followed — the derived URL
/// is what is downloaded — so this binds the signed bytes to the tag (invariant (d):
/// tag == `v<version>` == the tag in the manifest's URL), and a re-published appcast
/// copied onto another tag cannot be elected under it.
///
/// What is bound is the TAG and the DMG name, not the repository: the publisher signs
/// `url` under the compiled-in public channel's slug (`manifest_out.rs` `repo_slug` =
/// `[workspace.metadata.aterm] update_channel`, the key `build.rs` stamps into
/// `DEFAULT_OWNER`/`DEFAULT_REPO`), so an updater REPOINTED at a mirror
/// that carries the upstream-signed appcast would otherwise refuse every release as a
/// `manifest`-class failure blamed on a publisher who did nothing wrong (2026-09-04
/// review). The repository half adds nothing to (d): `pointer::parse_location` already
/// scopes every GET after the HEAD to the source repository. The URL's slug must be the
/// source's or the compiled-in channel's (GitHub-style case-insensitively); anything
/// else is a manifest signed for a channel this updater does not read.
fn web_container_url_agrees(source: &Source, tag: &str, manifest: &Manifest) -> Result<(), String> {
    let Some(url) = manifest.url.as_deref() else {
        return Err(format!(
            "authoritative {tag} carries no container `url`; the updater requires the \
             signed manifest to name its own download URL"
        ));
    };
    let refuse = |why: &str| {
        format!(
            "authoritative {tag} names container URL {url:?}, {why}; refusing a manifest \
             whose URL does not bind it to the tag the channel head points at"
        )
    };
    // `https://github.com/<owner>/<repo>/releases/download/<tag>/<dmg>`, every segment
    // under the strict predicate the derived URLs are built with.
    let Some(rest) = url.strip_prefix("https://github.com/") else {
        return Err(refuse("which is not a github.com release download URL"));
    };
    let parts: Vec<&str> = rest.split('/').collect();
    let [owner, repo, "releases", "download", url_tag, dmg] = parts.as_slice() else {
        return Err(refuse("which is not a release download URL"));
    };
    if !parts
        .iter()
        .all(|p| aterm_update_core::cdn::path_segment_safe(p))
    {
        return Err(refuse("which is not URL-safe"));
    }
    if *url_tag != tag {
        return Err(refuse(&format!("which names tag {url_tag:?}, not {tag}")));
    }
    if *dmg != manifest.dmg {
        return Err(refuse(&format!(
            "which names container {dmg:?}, not the manifest's {:?}",
            manifest.dmg
        )));
    }
    let names_default_channel = owner.eq_ignore_ascii_case(crate::DEFAULT_OWNER)
        && repo.eq_ignore_ascii_case(crate::DEFAULT_REPO);
    if !(is_slug(source, owner, repo) || names_default_channel) {
        return Err(refuse(&format!(
            "which names repository {owner}/{repo} — neither this updater's source \
             github.com/{}/{} nor the compiled-in channel github.com/{}/{}",
            source.owner,
            source.repo,
            crate::DEFAULT_OWNER,
            crate::DEFAULT_REPO
        )));
    }
    Ok(())
}

/// How a check's acquisition ended.
#[derive(Debug)]
enum Acquisition {
    /// Proceed to authorization and staging with the head's candidate.
    Proceed(AuthoritativeRelease),
    /// The steady state: the pointer names the last authorized tag.
    UpToDate { tag: String },
    /// A non-failure end, already recorded.
    Ended,
}

/// Background check + stage. Returns `Some(version)` when a strictly-newer
/// verified build IS staged and applicable — usually because this call staged it,
/// but also when one was already published and only the RE-stage is backed off
/// (see [`StageBackoff`]) — or `None` when nothing newer is available / the updater
/// is idle. The caller turns `Some` into the apply lane's cue, so the answer is
/// deliberately about STATE ("a build is staged and can be applied") rather than
/// about this call's activity. Errors are transient/operational (network, parse)
/// and are logged by the caller.
///
/// The running build's *version* is deliberately not a parameter: it plays no
/// part in the decision. Selection is by numeric release tag
/// ([`canonical_authority_version`]) and the downgrade gate is `current_build`
/// against the manifest's `build_number`. Reintroducing a version comparison
/// here would be wrong even under the single `MAJOR.MINOR.0` scheme: the patch
/// slot is always 0 and a dev build carries the commit sha in SemVer build
/// metadata (`0.5.0+g<sha>`), so a dev build and the release it should install
/// compare EQUAL on the numeric triple — a version test could not tell them
/// apart, and build metadata is explicitly not ordered (`VERSIONING.md`).
pub fn check_and_stage(current_build: u64, source: &Source) -> Result<Option<String>, String> {
    if bundle::resolve().is_none() {
        return Ok(None);
    }
    RATE_LIMITED.store(false, Ordering::Relaxed);
    let result = check_and_stage_inner(current_build, source);
    // EVERY exit writes status, including the failing ones. The eight `Err` paths
    // below all returned without recording, so `status.toml` kept advertising the
    // last HEALTHY outcome — "staged X — verified and ready to apply", or "up to
    // date" — while the machine was in fact failing every check. An operator (and
    // the GUI, which renders this text) then read a stale success as the current
    // state, which is worse than no status at all: it is confidently wrong.
    if let Err(error) = &result
        && let Some(staging) = Staging::resolve()
    {
        crate::status::record(
            &staging,
            current_build,
            &format!("update check failed: {error}"),
        );
    }
    // THE LIVE CHANNEL IS ANSWERED THE SAME WAY. A check that reported a download
    // (`progress::watch_download`) put something on the host's screen; every exit
    // after that point must take it down with the truth — the `Staged` and
    // `Deferred` arms report inline, and this is the one place all eight failure
    // exits reach. A check that never downloaded reported nothing and owes nothing
    // (the routine no-op check must not raise a bar to say it failed to find work).
    let began = crate::progress::take_download_began();
    if began && let Err(error) = &result {
        crate::progress::report(crate::progress::Progress::Failed {
            detail: error.clone(),
        });
    }
    if check_leaves_a_receipt(&result, rate_limited())
        && let Some(staging) = Staging::resolve()
    {
        crate::check_receipt::record(&staging, current_build, source, rate_limited());
    }
    result
}

/// Whether the check that just ended owes the shared check receipt a stamp.
///
/// ONLY A CHECK THAT REACHED THE CHANNEL LEAVES ONE (2026-09-15). The receipt's
/// contract is its module doc's first line — "completed network checks have their
/// own receipt" — and every reader leans on it: `crate::checker_skip_for` skips
/// this interval's network check on it machine-wide, and `crate::last_check_at` /
/// `status.toml`'s `checked_at` advertise it as when this channel was last asked.
/// Stamping it from the failing exits made a process re-read its OWN failure stamp
/// as "another aterm process completed this interval's update check" (measured
/// 2026-09-13, with no second aterm process on the machine) and advertised a
/// last-check time for a check that never reached GitHub. A failure is bounded by
/// each process's own backoff ladder (`cadence::Cadence::failed`), which is what
/// that ladder is for.
///
/// A DEFERRAL IS NOT A FAILURE and still records: a 429 or a 5xx from the download
/// host ends the check with `Ok` and books `record_deferral`, and the receipt then
/// widens every sibling's window so the whole machine retreats, not one process.
fn check_leaves_a_receipt(
    result: &Result<Option<String>, String>,
    recorded_a_deferral: bool,
) -> bool {
    result.is_ok() || recorded_a_deferral
}

/// The acquisition half of [`check_and_stage_inner`]: learn the channel head and elect
/// its candidate. The pointer transport is injected, so the request cost is measurable
/// without a network.
fn acquire(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    head: HeadFetch<'_>,
) -> Result<Acquisition, String> {
    // The recorded tag is trusted only when the ledger still describes THIS build's
    // verdict on THIS source (`status::latest_tag`).
    let known_tag = crate::status::latest_tag(staging, current_build, source);
    match resolve_web_head(staging, current_build, source, known_tag.as_deref(), head)? {
        WebHead::Ended => Ok(Acquisition::Ended),
        WebHead::Unchanged { tag } => Ok(Acquisition::UpToDate { tag }),
        WebHead::New(candidate) => Ok(Acquisition::Proceed(candidate)),
    }
}

/// The download host did not serve an asset the release names (a filtering proxy, or a
/// publish missing an asset): the wording of the failure, and the `delivery=` note it
/// books (one whitespace-free token of the status line).
const ASSET_HOST: &str = "the download host";
const ASSET_BLOCKED_NOTE: &str = "blocked";

/// A publishable stage strictly newer than the running build, as the check loop's
/// `Some` answer: "a build is staged and can be applied" — reported so the apply lane
/// arms even on a check that fetched nothing.
fn applicable_stage(staging: &Staging, current_build: u64) -> Option<String> {
    Ready::read_publishable(staging)
        .filter(|ready| ready.build_number > current_build)
        .map(|ready| ready.version)
}

fn check_and_stage_inner(current_build: u64, source: &Source) -> Result<Option<String>, String> {
    // Only stage for a real installed bundle (a dev build has nothing to swap).
    let Some(installed) = bundle::resolve() else {
        return Ok(None);
    };
    let staging = Staging::resolve().ok_or("could not resolve Updates dir")?;
    // Recursive copy cleanup runs in this checker worker. Both staging and
    // cross-volume apply leave isolated attempts on their destination volume.
    install::reap_abandoned_copy_attempts(&staging.staged_dir());
    if let Some(parent) = installed.app_root.parent() {
        install::reap_abandoned_copy_attempts(parent);
    }
    crate::status::clear_check_note();
    // A surviving apply streak recorded by a DIFFERENT build is proven stale
    // — the machine moved by SOME means (channel, manual install, boot swap)
    // — so every check heals it here rather than letting `update status`
    // present `persistent=true` on an up-to-date install forever (see
    // [`crate::health::Health::expire_stale_apply_streak`]).
    // …unless this process is an UNCOMMITTED HANDOFF CANDIDATE. A candidate spawns
    // its own background check before it has taken over, and expiring the streak
    // from there erased the very failures its own attempt was about to add —
    // `failing_applies` could never pass 1, so an update that downloads and verifies
    // but never starts stayed invisible (round-4 audit). Every other launch heals on
    // its first check, exactly as before.
    if !crate::is_uncommitted_handoff_candidate() {
        crate::health::Health::expire_stale_apply_streak(&staging.health(), current_build);
    }
    // Persisted monotonic recency floor (operator yank + rollback guard, F5/F6).
    let floor = crate::manifest::Floor::read(&staging.floor());

    // The pointer transport, INJECTED rather than called directly, so the request cost
    // is measurable in a test.
    let mut head = aterm_update_core::head_no_redirect;
    let authoritative = match acquire(&staging, current_build, source, &mut head)? {
        Acquisition::Proceed(candidate) => candidate,
        Acquisition::Ended => return Ok(None),
        Acquisition::UpToDate { tag } => {
            // THE STEADY STATE, on one request. The pointer names the tag this ledger
            // last authorized, so nothing is fetched and nothing is re-judged: the
            // release was accepted or declined on a previous check under the same
            // gates, and its required local stage still covers that decision. A terminal
            // healthy outcome — the channel was read.
            crate::health::Health::record_success(&staging.health());
            // A strictly newer build already staged is the fact this line must
            // carry (2026-09-14): "up to date" beside `staged_build = <newer>`
            // and six failed applies told an operator the machine was current.
            let stage = applicable_stage(&staging, current_build);
            let line = match (&stage, Ready::read_publishable(&staging)) {
                (Some(_), Some(ready)) => format!(
                    "{}; channel head {tag} unchanged",
                    covered_stage_line(
                        &staging,
                        current_build,
                        &ready.version,
                        ready.build_number,
                        ready.build_number,
                    ),
                ),
                _ => format!("up to date (channel head {tag}){}", cadence_note()),
            };
            crate::status::record(&staging, current_build, &line);
            // …still answering `Some` for a stage a sibling won the race to publish,
            // so this process's apply lane arms too (see the covered arms below).
            return Ok(stage);
        }
    };
    let web_tag = authoritative.release.tag_name.clone();
    // Every asset rides the derived download-host URL, with no credential.
    let mut download =
        |url: &str, max_bytes: u64| aterm_update_core::download_bytes(url, None, max_bytes);
    // The roster tier's inputs, resolved from the anchor and this client's durable state.
    // `PAPER_MASTER_PUBKEYS` is ARMED (2026-08-15), so the master-signed roster decides
    // who may have signed the appcast this check accepts, and every field below is
    // load-bearing.
    //
    // `floor_seq` is a snapshot read before the (network) head fetch above, so it can be
    // stale by the time a roster is admitted; `floor_refresh` re-reads the durable floor
    // at the admission point itself, closing the check-vs-ratchet TOCTOU between two
    // concurrent app instances.
    let floor_path = staging.floor();
    let floor_refresh = || crate::manifest::Floor::read(&floor_path).roster_seq;
    let roster_policy = RosterPolicy {
        master_pubkeys: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        floor_seq: floor.roster_seq,
        now_unix: unix_now(),
        floor_refresh: Some(&floor_refresh),
    };
    let mut fetched =
        fetch_authoritative_release(Some(authoritative), &mut download, &roster_policy);
    // THE SOURCE-ONLY HEAD: the pointer named a release whose appcast answers 404. The
    // head is not an app release yet; say so and end the check, healthy. The tag is not
    // recorded as authorized, so the next check reads it again and the app cut that
    // attaches its appcast is picked up then.
    if fetched.appcast_missing {
        record_head_without_app(&staging, current_build, &web_tag);
        return Ok(None);
    }
    // THE URL CROSS-CHECK, after every signature has been verified and the version
    // bound, and before anything is staged: the signed manifest must name the very
    // container URL this client derived under the pointer's tag.
    if let Some((manifest, release, _)) = fetched.selected.as_ref()
        && let Err(error) = web_container_url_agrees(source, &release.tag_name, manifest)
    {
        let reason = format!("{error}; refusing authoritative {}", release.tag_name);
        fetched.selected = None;
        fetched.refuse(reason);
    }
    // ATTRIBUTION, recorded where a human will find it later: the updater's own status
    // file, beside the release it describes. The owner's requirement is "I can track
    // which computer does what", and for the client half this is the record. It is
    // written before the staging decision because knowing WHO signed the release a
    // machine saw is useful whether or not that machine went on to install it.
    if let Some(who) = &fetched.attribution {
        crate::status::record(
            &staging,
            current_build,
            &format!("authoritative release signed by machine {who}"),
        );
    }
    let appcast_fetch_error = fetched.appcast_fetch_error;
    let asset_fetch_rate_limited = fetched.asset_fetch_rate_limited;
    let manifest_rejected = fetched.manifest_rejected;
    let rejection_reason = fetched.rejection_reason.take();
    let observed_roster_seq = fetched.observed_roster_seq;
    let best = fetched.selected;
    let seen_min_build = best
        .as_ref()
        .and_then(|(manifest, _, _)| manifest.min_build)
        .unwrap_or(0);

    // Remember the authoritative release's operator floor immediately (even if we do
    // not stage). The persisted floor remains monotonic across checks.
    // The same call ratchets the roster sequence. Doing it here — on OBSERVATION, not on
    // successful staging — is what makes the replay defence work: a client that merely
    // SAW roster generation n must refuse n-1 forever after, whether or not it went on to
    // install anything from that release. `observed_roster_seq` carries that observation
    // out of the chain even when the chain then REFUSED the release (a roster that
    // revokes the release's signer is admitted, observed here, and only then refuses),
    // so the ratchet is genuinely observation-driven and not acceptance-driven.
    crate::manifest::Floor::bump_and_write(
        &staging.floor(),
        seen_min_build,
        0,
        observed_roster_seq.unwrap_or(0),
    );
    // A REVOCATION NOW REACHES AN ALREADY-STAGED BUILD. The ratchet above records
    // that a newer generation was SEEN; this acts on what that generation SAYS.
    //
    // A stage is an authorization made earlier, and nothing revisited it: a build
    // staged at 10:00 by a machine revoked at 10:30 was applied anyway (in-session,
    // or at the next launch), and only a separate `min_build` yank could have
    // stopped a withdrawn machine's artifact.
    //
    // IT BELONGS HERE, NOT IN THE APPLY LANE. Revocation is a LIST, and this is the
    // only place the roster document is in hand; the apply lane holds a floor NUMBER,
    // and the obvious comparison there is not merely weaker but WRONG — a manifest
    // attributed under an older generation than the roster asset is the ordinary
    // post-join steady state, which `authorize_by_roster` deliberately admits, so
    // gating on it retires good stages forever (see `install`, gate 4c).
    //
    // Matching on `machine_id` is what makes this exact: the id sits inside the
    // manifest's SIGNED bytes and the roster maps ids to keys, so a genuine
    // signature by one machine cannot be relabelled as another's.
    if !fetched.observed_revocations.is_empty()
        && let Some(staged) = Ready::read_publishable(&staging)
        && let Some(machine) = staged.machine_id.as_deref()
        && revocation_withdraws_stage(&fetched.observed_revocations, Some(machine))
    {
        // UNDER THE APPLY LOCK, like every other retirement of the published stage.
        // `apply_staged_if_ready` in a concurrently LAUNCHING instance verifies the
        // staged `.app` and then renames it into place under `apply_lock`; a
        // `remove_dir_all` racing that rename would gut the tree it was in the
        // middle of installing (and, fd-relative, keep unlinking inside the same
        // inode after the rename — the live install), which the boot sentinel would
        // then read as a crash loop and revert with the build poisoned. Lock order
        // is respected (nothing is held here; the stage lock is taken later), and a
        // boot apply that wins the lock first simply consumes the marker — the
        // re-read below then finds nothing to retire, which is the honest outcome:
        // an installed build from a revoked machine is `min_build`'s to yank.
        match aterm_update_core::FileLock::acquire(&staging.apply_lock) {
            Ok(_apply_lock) => {
                if Ready::read_publishable(&staging)
                    .is_some_and(|still| still.build_number == staged.build_number)
                {
                    crate::warn(&format!(
                        "staged build {} was authorized by machine {machine:?}, which \
                         roster generation {} revokes; discarding it",
                        staged.build_number,
                        observed_roster_seq.unwrap_or(0)
                    ));
                    staging.retire_published();
                    let note = format!(
                        "held: staged build {} was signed by machine {machine}, which the \
                         machine roster has revoked",
                        staged.build_number
                    );
                    // Carried onto every later record of THIS check (status.toml is one
                    // overwritten line): the sentence survives the check's own terminal
                    // outcome, and `aterm ctl update status` really does say so.
                    crate::status::set_check_note(note.clone());
                    crate::status::record(&staging, current_build, &note);
                }
            }
            Err(error) => crate::warn(&format!(
                "staged build {} is signed by revoked machine {machine:?} but the apply \
                 lock could not be taken to retire it ({error}); the next check retries",
                staged.build_number
            )),
        }
    }
    let effective_min_build = floor.min_build.max(seen_min_build);

    let Some((manifest, release, artifact)) = best else {
        if appcast_fetch_error && asset_fetch_rate_limited {
            // The manifest/roster/signature GET met the host's throttle — the same
            // verdict a throttled pointer HEAD gets one request earlier, and for the
            // same reason no `record_failure`: a saturated check must not book a
            // persistent `pipeline` streak and fire the "download pipeline is likely
            // broken" notice at a perfectly healthy machine (2026-08-19 audit).
            record_deferral(
                &staging,
                current_build,
                "update check deferred: GitHub rate limit hit while fetching a release \
                 asset — backing off, will retry on the next check",
            );
            return Ok(None);
        }
        let msg = if appcast_fetch_error {
            // Manifests exist but could not be downloaded while the channel head was
            // readable — a `pipeline`-class failure. The ledger decides the honest
            // wording: a streak ≥ PERSISTENT_AFTER is not called "deferred".
            let host = ASSET_HOST;
            crate::status::set_delivery_note(ASSET_BLOCKED_NOTE);
            let h = crate::health::Health::record_failure(
                &staging.health(),
                "pipeline",
                &format!("release manifests exist but could not be fetched from {host}"),
            );
            if h.pipeline_failures >= crate::PERSISTENT_AFTER {
                format!(
                    "FAILING ({} consecutive checks since {}): release manifests exist \
                     but cannot be downloaded from {host} — this build's download \
                     pipeline is likely broken",
                    h.pipeline_failures,
                    h.class_since("pipeline")
                )
            } else {
                format!(
                    "update check deferred: a release manifest could not be fetched \
                     from {host} (attempt {} — will retry)",
                    h.pipeline_failures
                )
            }
        } else if manifest_rejected {
            // Manifests were FETCHED but rejected (unsigned / bad signature /
            // unparseable / a version or URL that does not bind to the tag): the
            // pipeline works; the release side (or an attacker) is the problem. Its
            // own class — it must not clear a streak. The ledger gets the reason the
            // log printed (2026-09-14), and a STRANDED client — one whose own anchor
            // cannot verify the channel — is told to reinstall, not to wait for the
            // publisher.
            let reason = rejection_reason
                .as_deref()
                .unwrap_or("manifest(s) fetched but rejected (signature/parse)");
            let h = crate::health::Health::record_failure(&staging.health(), "manifest", reason);
            if h.manifest_failures >= crate::PERSISTENT_AFTER {
                if is_stale_anchor_refusal(reason) {
                    format!(
                        "FAILING ({} consecutive checks since {}): {reason}",
                        h.manifest_failures,
                        h.class_since("manifest")
                    )
                } else {
                    format!(
                        "FAILING ({} consecutive checks since {}): {reason} — this machine \
                         cannot install any release until that is fixed at the publisher",
                        h.manifest_failures,
                        h.class_since("manifest")
                    )
                }
            } else {
                format!("no stageable release: {reason}")
            }
        } else {
            // The check itself ran fine (the head was read, nothing carries a
            // manifest): clear any stale failure streak so health reflects THIS check.
            crate::health::Health::record_success(&staging.health());
            String::from("no release carries an update manifest")
        };
        crate::status::record(&staging, current_build, &msg);
        return Ok(None);
    };
    // NOTE: no `record_success` yet — the container download/verify/stage below is
    // still part of this check's pipeline. Success is recorded only at the terminal
    // healthy outcomes ("up to date" / "staged"), so a download-only breakage ACCRUES
    // a streak instead of being reset every cycle by its own check's manifest fetch.

    // Downgrade gate: never stage an older-or-equal build. A terminal healthy
    // outcome — the whole pipeline this check exercised worked — and the one that
    // records `latest_tag`, so the next check stops at the HEAD.
    //
    // DELIBERATELY NOT GATED ON THE BUNDLE AT THIS PATH. Suppressing the download
    // when the installed bundle already carries this build would save one redundant
    // fetch on a publisher's own machine — and would strand every machine whose
    // newer bundle cannot be VERIFIED (mid-notarization, a broken seal, a yanked
    // build), because the activation lane refuses it while the check lane would no
    // longer acquire the release it could actually install. An unverified plist is
    // not an input to an acquisition decision (2026-08-19 round-4 skeptics).
    if manifest.build_number <= current_build {
        crate::health::Health::record_success(&staging.health());
        crate::status::set_latest_tag(&web_tag, source, current_build, manifest.build_number);
        crate::status::record(
            &staging,
            current_build,
            &format!(
                "up to date (latest release build {}){}",
                manifest.build_number,
                cadence_note()
            ),
        );
        return Ok(None);
    }

    // Recency floors (F5/F6): refuse a genuine build below the operator floor (yank),
    // or below our high-water (an attacker re-pointing the newest release at an older
    // genuine build cannot roll a client that has already advanced back down).
    //
    // BOTH holds are TERMINAL HEALTHY outcomes and must clear the acquisition streaks,
    // for exactly the reason the downgrade gate immediately above does: everything this
    // check exercised — the channel head, the appcast fetch, the signature/roster
    // admission — WORKED, and the only reason it stops here is a deliberate policy
    // decision about the build it found. Returning without `record_success` left the
    // network/pipeline/manifest streaks standing, and a machine parked under a yank
    // floor stays parked for days, so ordinary non-consecutive blips accumulated check
    // after check until one crossed PERSISTENT_AFTER and fired "your update pipeline is
    // likely broken" at a machine whose pipeline had just run end to end in front of it.
    // (`latest_tag` is deliberately NOT recorded: a floor can move under the same tag,
    // and re-judging costs unmetered requests only.)
    if manifest.build_number < effective_min_build {
        crate::health::Health::record_success(&staging.health());
        crate::status::record(
            &staging,
            current_build,
            &format!(
                "held: latest build {} is below the operator floor {}",
                manifest.build_number, effective_min_build
            ),
        );
        return Ok(None);
    }
    if manifest.build_number < floor.high_water {
        crate::health::Health::record_success(&staging.health());
        crate::status::record(
            &staging,
            current_build,
            &format!(
                "held: latest build {} is below high-water {} (possible rollback)",
                manifest.build_number, floor.high_water
            ),
        );
        return Ok(None);
    }

    // If a newer build is already staged, don't re-download it. This is still a
    // TERMINAL HEALTHY outcome — the head and the manifest were fetched and accepted;
    // the only step skipped is a download whose bytes we already have — so clear the
    // acquisition streaks. Omitting that let non-consecutive pipeline/manifest blips
    // accumulate for the whole life of a pending stage and cross PERSISTENT_AFTER,
    // firing "your update pipeline is likely broken" at a machine whose only state is
    // a stage waiting to apply.
    if publishable_stage_covers(&staging, &manifest) {
        crate::health::Health::record_success(&staging.health());
        crate::status::set_latest_tag(&web_tag, source, current_build, manifest.build_number);
        record_covered_stage_status(&staging, current_build, &manifest);
        // ANSWER `Some`, exactly as the check loop's contract says: "the check
        // also answers `Some` for a build that was already published and is
        // only waiting to be applied". These two covered arms answered `None`,
        // so `on_staged` never fired for a stage some SIBLING process won the
        // race to publish — and every `aterm` session process runs this
        // checker, so on a daily driver the GUI lost that race about half the
        // time per release and its in-session apply lane never armed: the
        // staged build sat "verified and ready" until a relaunch (2026-09-01
        // audit; the GUI side is idempotent — `arm` dedups the armed build).
        return Ok(Ready::read_publishable(&staging).map(|ready| ready.version));
    }

    // If this exact build already failed to stage, don't re-download the (up to
    // 2 GiB) container every interval; a re-publish under the same build with a
    // different sha256 (or any newer build) clears the memo (F17). The memo is keyed
    // on the manifest's `sha256` for both containers, so a build is one candidate
    // however its bytes arrive.
    //
    // The window gates THIS — the re-stage — and nothing else. An already-published,
    // strictly-newer stage is applied from disk, so it is not the thing the memo is
    // throttling; skipping it here is how a ready 0.10.0 was held for 1387 minutes by
    // a failed re-publish of the same build (see [`StageBackoff`]).
    if let Some(backoff) = stage_backoff(
        &staging,
        &manifest,
        current_build,
        crate::install::unix_now_secs(),
    ) {
        // A backed-off re-stage is still a terminal healthy end to the ACQUISITION half
        // of this check: the head, the appcast and its authorization all worked, and we
        // stop only because a memo says these exact bytes already refused to stage.
        // Recording nothing left the network/pipeline/manifest streaks standing for the
        // whole life of the memo — up to 24 h, and a quarantine's window never opens at
        // all — so unrelated blips accumulated to PERSISTENT_AFTER and reported a broken
        // pipeline on a machine whose pipeline demonstrably ran every cycle.
        //
        // DELIBERATELY NOT `record_success`: that also zeroes `stage_failures`. The
        // backoff itself lives in `failed.toml`, so clearing the ledger streak would not
        // re-open the window — it would do something worse. A machine that fails to
        // stage interleaves failed checks with backed-off ones, so a `record_success`
        // here would reset the stage streak between every pair of failures and it could
        // never reach PERSISTENT_AFTER: the one class whose escalation says "the bytes
        // arrive and will not become a bundle" would be silenced by the very backoff it
        // caused. `record_acquisition_success` clears the acquisition classes and their
        // clocks and preserves `stage_failures`/`stage_since`, exactly the way
        // `record_success` already preserves the apply streak.
        crate::health::Health::record_acquisition_success(&staging.health());
        crate::status::record(
            &staging,
            current_build,
            &backoff.status_line(manifest.build_number),
        );
        // Reporting the staged version is what drives the apply lane (the caller's
        // `on_staged` hook), so a download backoff cannot strand a build that only
        // needs applying.
        return Ok(backoff.applicable.map(|ready| ready.version));
    }

    // Serialize the staging critical section (download → extract → publish) across
    // processes so two app instances can't clobber the shared download/staged
    // scratch. Separate from the apply lock so this (possibly long) download never
    // blocks a starting instance's apply path.
    let _stage_lock = aterm_update_core::FileLock::acquire(&staging.stage_lock)
        .map_err(|e| format!("stage lock: {e}"))?;
    // Re-check under the lock: another instance may have just staged this build.
    // Same terminal-healthy reasoning as the pre-lock check above — and the same
    // `Some` answer, so the sibling's freshly-won stage arms THIS process's
    // apply lane too.
    if publishable_stage_covers(&staging, &manifest) {
        crate::health::Health::record_success(&staging.health());
        crate::status::set_latest_tag(&web_tag, source, current_build, manifest.build_number);
        record_covered_stage_status(&staging, current_build, &manifest);
        return Ok(Ready::read_publishable(&staging).map(|ready| ready.version));
    }
    // Under the same lock, re-read the roster floor: a concurrent instance may have
    // observed a newer roster generation after this check's admission. Nothing signed
    // under a superseded generation may be staged (see [`roster_authority_superseded`]).
    if roster_authority_superseded(
        observed_roster_seq,
        crate::manifest::Floor::read(&staging.floor()).roster_seq,
    ) {
        // Terminal healthy: acquisition ran end to end and a CONCURRENT instance simply
        // ratcheted the roster generation under us. Losing that benign race is not a
        // pipeline failure, and without a success record the streaks from earlier blips
        // survived it — on a machine running two app instances this hold is common
        // enough to keep a stale streak alive and eventually push it past
        // PERSISTENT_AFTER. The refusal is transient; the next check re-runs under the
        // advanced floor.
        crate::health::Health::record_success(&staging.health());
        crate::status::record(
            &staging,
            current_build,
            "held: a newer machine-roster generation was recorded during this check; \
             re-checking under it next cycle",
        );
        return Ok(None);
    }

    // Download the exact unique same-release container identity already proven
    // while accepting the authoritative manifest — the zip when the release
    // carries one, else the DMG. No order-dependent asset lookup is permitted
    // after this point. The URL is the DERIVED one for the pointer's tag (the
    // manifest's own `url` was cross-checked above and is never followed).
    let asset = &release.assets[artifact.asset_index];
    let container = artifact.container.label();

    let part = staging.download.join(format!("{}.part", artifact.name));
    let container_path = staging.download.join(&artifact.name);
    sweep_download_scratch(&staging);
    // LIVE PROGRESS for a host that shows it (the aterm window's status bar): a
    // sibling poller stats the growing `.part` (the download host declares no size up
    // front, which the host renders as "unknown"). Joined on drop, so it cannot
    // outlive the download it watches.
    let download_watch = crate::progress::watch_download(&part, &manifest.version);
    // A failed download is a `pipeline`-class ledger entry: the asset provably
    // exists (the release names it) but could not be fetched.
    let downloaded = aterm_update_core::download_to(
        &asset.url,
        None,
        &part,
        aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND,
    );
    drop(download_watch);
    if let Err(e) = downloaded {
        let _ = std::fs::remove_file(&part);
        // The container's own 429 is the same weather as the manifest's, one
        // request later in the check: deferred, not a pipeline failure.
        if aterm_update_core::download_error_is_rate_limit(&e) {
            let note = format!(
                "update check deferred: GitHub rate limit hit while downloading the \
                 {container} — backing off, will retry on the next check"
            );
            record_deferral(&staging, current_build, &note);
            // Answer the live channel too: the bar opened on the first byte and
            // a deferral is its honest end (the wrapper reports only `Err`s).
            if crate::progress::take_download_began() {
                crate::progress::report(crate::progress::Progress::Deferred { detail: note });
            }
            return Ok(None);
        }
        // The same note as the manifest leg: the download host did not serve it.
        crate::status::set_delivery_note(ASSET_BLOCKED_NOTE);
        crate::health::Health::record_failure(
            &staging.health(),
            "pipeline",
            &format!("{container} download failed: {e}"),
        );
        return Err(format!("{container} download failed: {e}"));
    }

    // The container arrived; everything from here to the publish is one "verifying"
    // phase on the live channel: digest, extract, codesign/Gatekeeper, the
    // atomic stage.
    crate::progress::report(crate::progress::Progress::Verifying {
        version: manifest.version.clone(),
    });
    // Atomically name it final. From here failures are `stage`-class in the health
    // ledger: the bytes ARRIVED; the artifact (or local disk) is the problem, not the
    // download pipeline. (Size is proved by the sha256 below.)
    if let Err(e) = std::fs::rename(&part, &container_path) {
        let msg = format!("finalize download: {e}");
        crate::health::Health::record_failure(&staging.health(), "stage", &msg);
        return Err(msg);
    }

    // Integrity: SHA-256 must equal the manifest's digest FOR THIS CONTAINER.
    //
    // A bare `?` here was the ONE exit in this function that recorded nothing at all.
    // A missing or non-executable `shasum` therefore left `failing=0 persistent=false`
    // forever — and `Health::is_persistent()` is the sole gate on the "aterm
    // auto-update is failing" notice, so the machine that could never hash a download
    // was also the one machine guaranteed never to say so.
    let got = match aterm_update_core::sha256_file(&container_path) {
        Ok(got) => got,
        Err(e) => {
            let _ = std::fs::remove_file(&container_path);
            crate::manifest::FailedMark::record_stage_failure(
                &staging.failed(),
                manifest.build_number,
                &manifest.sha256,
                crate::install::unix_now_secs(),
            );
            crate::health::Health::record_failure(&staging.health(), "stage", &e);
            return Err(e);
        }
    };
    if !got.eq_ignore_ascii_case(&artifact.sha256) {
        let _ = std::fs::remove_file(&container_path);
        let msg = format!(
            "{container} sha256 mismatch: got {got}, manifest {}",
            artifact.sha256
        );
        // Same budget as every other bytes-arrived failure — see the size arm above.
        crate::manifest::FailedMark::record_stage_failure(
            &staging.failed(),
            manifest.build_number,
            &manifest.sha256,
            crate::install::unix_now_secs(),
        );
        crate::health::Health::record_failure(&staging.health(), "stage", &msg);
        return Err(msg);
    }

    // Unpack, verify (codesign/team-id/spctl), publish the ready marker. On a
    // post-download stage failure (verification etc.) memoize this build+sha so we
    // don't re-download it next cycle, and reclaim the container (F17). The memo is
    // keyed on the MANIFEST digest, not this container's, so the two paths share one
    // retry budget for one candidate build.
    let staged = match artifact.container {
        Container::Zip => install::stage_from_zip(
            &staging,
            &container_path,
            &manifest,
            crate::effective_team_id(),
        ),
        Container::Dmg => install::stage_from_dmg(
            &staging,
            &container_path,
            &manifest,
            crate::effective_team_id(),
        ),
    };
    if let Err(e) = staged {
        crate::manifest::FailedMark::record_stage_failure(
            &staging.failed(),
            manifest.build_number,
            &manifest.sha256,
            crate::install::unix_now_secs(),
        );
        let _ = std::fs::remove_file(&container_path);
        crate::health::Health::record_failure(&staging.health(), "stage", &e);
        return Err(e);
    }
    // The verified bundle is the artifact now; reclaim the container and clear the memo.
    let _ = std::fs::remove_file(&container_path);
    crate::manifest::FailedMark::clear(&staging.failed());
    // Terminal healthy outcome: this check exercised the WHOLE pipeline (manifest,
    // container, verify, stage) successfully — clear every failure streak.
    crate::health::Health::record_success(&staging.health());
    // Raise the high-water to the build we just staged (never lowered): a later attempt
    // to roll us back below it is refused above (F6).
    crate::manifest::Floor::bump_and_write(&staging.floor(), 0, manifest.build_number, 0);
    // …and remember the tag, so the next check is one HEAD.
    crate::status::set_latest_tag(&web_tag, source, current_build, manifest.build_number);

    // NOT "applies on next launch". The stager has no idea whether it does: the
    // in-session apply lane owns that decision, is on by default, and when it
    // runs no relaunch happens at all. Emitting the relaunch advice
    // unconditionally made this line ADVICE rather than a record — and when the
    // apply lane then refused silently, that advice was the only thing an
    // operator could see, so "quit aterm" looked like the answer. This says what
    // the stager actually did. When an apply is refused AND that refusal reaches
    // `record_apply_refusal`, this line is overwritten with the reason — every
    // refusal funnel now does so, but the guarantee lives in those call sites,
    // not here, so read this as "the last thing the STAGER knew", never as proof
    // that no apply has been attempted since.
    crate::status::record(
        &staging,
        current_build,
        &format!(
            "staged {} (build {}) — verified and ready to apply",
            manifest.version, manifest.build_number
        ),
    );
    // The live channel's good end. Only a check that downloaded says so — an
    // already-covered stage returns `Ok(Some)` far above without touching it.
    if crate::progress::take_download_began() {
        crate::progress::report(crate::progress::Progress::Staged {
            version: manifest.version.clone(),
            build: manifest.build_number,
        });
    }
    Ok(Some(manifest.version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    /// Standard padded Base64 — the shipped encoder (`aterm_codec::base64`), held
    /// byte-identical to the retired `base64` package's `general_purpose::STANDARD`
    /// by `crates/aterm-codec/tests/base64_oracle.rs`.
    fn b64(raw: &[u8]) -> String {
        aterm_codec::base64::encode(raw).expect("test key material is far below MAX_INPUT_LEN")
    }

    fn test_source() -> Source {
        Source {
            owner: "alabsystems".into(),
            repo: "aterm".into(),
        }
    }

    /// A healthy outcome says how often it checks, in words, and never how it reads
    /// the channel (2026-09-23 audit, GT-16) — `lane=` and the log carry those.
    #[test]
    fn the_healthy_note_says_how_often_and_nothing_about_how() {
        let note = cadence_note();
        assert_eq!(
            note,
            format!(
                " \u{b7} checks every {} min",
                crate::cadence::INTERVAL_SECS / 60
            )
        );
        for jargon in ["lane", "rung", "token", "API", "unmetered", "credential"] {
            assert!(!note.contains(jargon), "{jargon}: {note}");
        }
    }

    /// The standing "cannot read the channel" wording must survive being read months
    /// later out of `status.toml`: the consequence, every cause GitHub renders
    /// identically, and the remedy — never a bare "idle", which an operator cannot tell
    /// apart from "no updates available", and never a credential, which no lane reads.
    #[test]
    fn an_unreadable_channel_is_loud_and_actionable_not_idle() {
        let source = test_source();
        let text = unreadable_explanation(404, &source);
        assert!(text.contains("NEVER receive an update"), "{text}");
        assert!(text.contains("github.com/alabsystems/aterm"), "{text}");
        assert!(text.contains("HTTP 404"), "{text}");
        // Every cause the web host cannot distinguish.
        assert!(
            text.contains("no published release"),
            "cause 1 missing: {text}"
        );
        assert!(text.contains("private"), "cause 2 missing: {text}");
        assert!(text.contains("does not exist"), "cause 3 missing: {text}");
        assert!(
            text.contains("reinstalled"),
            "the remedy is missing: {text}"
        );
        assert!(
            !text.contains("token") && !text.contains("ATERM_"),
            "no credential and no environment knob is ever a remedy: {text}"
        );
        assert!(
            !text.contains("repointed"),
            "the public channel has no repoint to check: {text}"
        );
        // A REPOINTED source (a development build) adds the one cause only it can have.
        let overridden = Source {
            owner: "someone-else".to_string(),
            repo: "private-aterm".to_string(),
        };
        let text = unreadable_explanation(404, &overridden);
        assert!(
            text.contains("github.com/someone-else/private-aterm"),
            "{text}"
        );
        assert!(text.contains("`[update] owner`/`repo`"), "{text}");
        assert!(!text.contains("token"), "{text}");
    }

    /// Reading the channel — with no credential at all — clears the stranded state and
    /// the deferral latch the background loop reads.
    #[test]
    fn a_readable_channel_clears_the_strand_and_the_deferral() {
        let _serialized = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let source = test_source();
        RATE_LIMITED.store(true, Ordering::Relaxed);
        note_readable(&source);
        assert!(!rate_limited(), "a completed read clears the backoff latch");
        assert!(!crate::unreadable::is_stranded());
    }

    /// The candidate the check builds for this one release, shaped the way
    /// [`web_release`] shapes it (the web lane synthesizes exactly one) — for tests
    /// that exercise the roster chain over hand-built releases.
    fn candidate_for(releases: Vec<Release>) -> Result<Option<AuthoritativeRelease>, String> {
        let [release] = <[Release; 1]>::try_from(releases).expect("exactly one release");
        let version = release.tag_name.trim_start_matches('v').to_string();
        let manifest_index = unique_asset_index(&release, APPCAST_ASSET)?.expect("an appcast");
        Ok(Some(AuthoritativeRelease {
            version,
            release,
            manifest_index,
        }))
    }

    fn candidate_manifest() -> Manifest {
        Manifest {
            schema: 1,
            version: "0.54.0".into(),
            build_number: 54,
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            sha256: "ab".repeat(32),
            dmg: "aterm-0.54.0.dmg".into(),
            url: None,
            zip: None,
            zip_sha256: None,
            min_build: None,
            machine_id: None,
            roster_seq: None,
            changelog: None,
        }
    }

    fn release_with_appcast(tag: &str, url: &str) -> Release {
        let version = tag.strip_prefix('v').unwrap_or(tag);
        Release {
            tag_name: tag.into(),
            assets: vec![
                Asset {
                    name: "aterm-appcast.toml".into(),
                    url: url.into(),
                },
                Asset {
                    name: format!("aterm-{version}.dmg"),
                    url: format!("{url}-dmg"),
                },
            ],
        }
    }

    fn release_with_signed_appcast(tag: &str, manifest_url: &str, signature_url: &str) -> Release {
        let version = tag.strip_prefix('v').unwrap_or(tag);
        Release {
            tag_name: tag.into(),
            assets: vec![
                Asset {
                    name: "aterm-appcast.toml".into(),
                    url: manifest_url.into(),
                },
                Asset {
                    name: "aterm-appcast.toml.sig".into(),
                    url: signature_url.into(),
                },
                Asset {
                    name: format!("aterm-{version}.dmg"),
                    url: format!("{manifest_url}-dmg"),
                },
            ],
        }
    }

    fn manifest_bytes(version: &str, build_number: u64, min_build: u64) -> Vec<u8> {
        manifest_bytes_with_dmg(
            version,
            build_number,
            min_build,
            &format!("aterm-{version}.dmg"),
        )
    }

    fn manifest_bytes_with_dmg(
        version: &str,
        build_number: u64,
        min_build: u64,
        dmg: &str,
    ) -> Vec<u8> {
        format!(
            "schema = 1\nversion = \"{version}\"\nbuild_number = {build_number}\n\
             sha256 = \"{}\"\ndmg = {dmg:?}\nmin_build = {min_build}\n",
            "ab".repeat(32),
        )
        .into_bytes()
    }

    #[test]
    fn authoritative_manifest_version_must_equal_canonical_tag() {
        let authoritative =
            candidate_for(vec![release_with_appcast("v0.10.0", "mismatched-highest")])
                .unwrap()
                .unwrap();
        let mut urls = Vec::new();
        let mut download = |url: &str, _max_bytes: u64| {
            urls.push(url.to_string());
            match url {
                "mismatched-highest" => Ok(manifest_bytes("0.9.0", 10, 0)),
                unexpected => panic!("unexpected asset fetch: {unexpected}"),
            }
        };
        let fetched =
            fetch_authoritative_release(Some(authoritative), &mut download, &RosterPolicy::INERT);
        assert!(fetched.selected.is_none());
        assert!(fetched.manifest_rejected);
        assert_eq!(urls, ["mismatched-highest"]);
        assert_eq!(fetched.manifest_fetch_attempts, 1);
    }

    /// The zip is PREFERRED whenever the manifest carries a resolvable one —
    /// that preference IS the fix: `hdiutil attach` cannot work in the orphaned
    /// post-handoff process that most needs to stage, and `ditto` can. Every
    /// other shape must still fall back to the DMG rather than stop updating,
    /// because that is what keeps already-published releases installable.
    #[test]
    fn stage_selection_prefers_the_zip_and_falls_back_to_the_dmg() {
        const VERSION: &str = "0.54.0";
        let zip_name = format!("aterm-{VERSION}-mac.zip");
        let zip_digest = "cd".repeat(32);
        let push_zip_asset = |release: &mut Release| {
            release.assets.push(Asset {
                name: zip_name.clone(),
                url: "https://api.github.com/repos/o/r/releases/assets/9".into(),
            });
        };

        // A manifest with no zip — every release published before zip staging —
        // selects the DMG, exactly as it always did.
        let dmg_only_release = release_with_appcast("v0.54.0", "https://example/appcast");
        let dmg_only = candidate_manifest();
        let chosen = select_stage_artifact(&dmg_only_release, &dmg_only, VERSION).unwrap();
        assert_eq!(chosen.container, Container::Dmg);
        assert_eq!(chosen.name, "aterm-0.54.0.dmg");
        assert_eq!(chosen.sha256, dmg_only.sha256);
        assert_eq!(
            dmg_only_release.assets[chosen.asset_index].name,
            chosen.name
        );

        // Manifest and release both carry the zip: the zip wins, carrying ITS
        // digest (not the DMG's) as the bytes-must-match value.
        let mut zip_release = dmg_only_release.clone();
        push_zip_asset(&mut zip_release);
        let mut zipped = candidate_manifest();
        zipped.zip = Some(zip_name.clone());
        zipped.zip_sha256 = Some(zip_digest.clone());
        let chosen = select_stage_artifact(&zip_release, &zipped, VERSION).unwrap();
        assert_eq!(chosen.container, Container::Zip);
        assert_eq!(chosen.name, zip_name);
        assert_eq!(chosen.sha256, zip_digest);
        assert_eq!(zip_release.assets[chosen.asset_index].name, chosen.name);

        // Declared but unusable, four ways — each falls back, none refuses.
        let mut no_digest = zipped.clone();
        no_digest.zip_sha256 = None;
        let mut noncanonical = zipped.clone();
        noncanonical.zip = Some("aterm-mac.zip".into());
        let mut duplicate_zip_release = zip_release.clone();
        push_zip_asset(&mut duplicate_zip_release);
        for (label, release, manifest) in [
            (
                "no digest to check the bytes against",
                &zip_release,
                &no_digest,
            ),
            ("release carries no such asset", &dmg_only_release, &zipped),
            ("noncanonical zip name", &zip_release, &noncanonical),
            (
                "ambiguous duplicate assets",
                &duplicate_zip_release,
                &zipped,
            ),
        ] {
            let chosen = select_stage_artifact(release, manifest, VERSION)
                .unwrap_or_else(|error| panic!("{label} must still update: {error}"));
            assert_eq!(chosen.container, Container::Dmg, "{label}");
            assert_eq!(chosen.sha256, manifest.sha256, "{label}");
        }

        // The DMG identity proof stays unconditional: a manifest that names a
        // noncanonical DMG is refused outright even when its zip is perfect.
        let mut bad_dmg = zipped.clone();
        bad_dmg.dmg = "aterm.dmg".into();
        let error = select_stage_artifact(&zip_release, &bad_dmg, VERSION)
            .expect_err("a noncanonical DMG must refuse the whole release");
        assert!(error.contains("noncanonical DMG"), "{error}");
    }

    fn write_ready(staging: &Staging, build: u64, commit: &str, digest: &str) {
        let ready = Ready {
            build_number: build,
            version: format!("0.0.{build}"),
            commit: Some(commit.into()),
            dmg_sha256: digest.into(),
            team_id: "T".into(),
            staged_at: String::new(),
            changelog: None,
            machine_id: None,
            roster_seq: None,
        };
        std::fs::write(&staging.ready, ready.to_toml().unwrap()).unwrap();
    }

    fn write_bundle_identity(staging: &Staging, build: u64, commit: &str) {
        let contents = staging.staged_app.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<plist><dict><key>CFBundleVersion</key><string>{build}</string>\
                 <key>ATermGitCommit</key><string>{commit}</string></dict></plist>"
            ),
        )
        .unwrap();
    }

    #[test]
    fn corrupt_high_ready_and_deleted_stage_cannot_suppress_restage() {
        let staging = Staging::scratch("publishable");
        let manifest = candidate_manifest();
        std::fs::create_dir_all(&staging.staged_app).unwrap();

        // NEGATIVE CONTROL: an enormous parseable marker carries no canonical
        // artifact identity, so it must never permanently bypass the download.
        write_ready(&staging, 9_999, "bad", &"cd".repeat(32));
        assert!(
            !publishable_stage_covers(&staging, &manifest),
            "corrupt high Ready must fall through to download/restage"
        );

        let canonical_commit = manifest.commit.as_deref().unwrap();
        write_ready(&staging, 9_999, canonical_commit, &"cd".repeat(32));
        assert!(
            !publishable_stage_covers(&staging, &manifest),
            "canonical high Ready plus an empty app directory must still restage"
        );

        // Exact metadata without the release bundle identity is still incomplete.
        write_ready(
            &staging,
            manifest.build_number,
            canonical_commit,
            &manifest.sha256,
        );
        assert!(
            !publishable_stage_covers(&staging, &manifest),
            "exact Ready with missing Info.plist must force restage"
        );

        // A complete exact stage is the positive control for the short-circuit.
        write_bundle_identity(&staging, manifest.build_number, canonical_commit);
        assert!(publishable_stage_covers(&staging, &manifest));

        // Marker metadata without the published directory is not a stage. Both
        // pre-lock and post-lock call this predicate, so either observation falls
        // through to a fresh download/stage transaction.
        std::fs::remove_dir_all(&staging.staged_app).unwrap();
        assert!(
            !publishable_stage_covers(&staging, &manifest),
            "deleted staged_app must force download/restage"
        );

        // Same-build metadata from another artifact cannot suppress this release.
        std::fs::create_dir_all(&staging.staged_app).unwrap();
        write_ready(
            &staging,
            manifest.build_number,
            canonical_commit,
            &"ef".repeat(32),
        );
        assert!(
            !publishable_stage_covers(&staging, &manifest),
            "same build with another digest must be restaged"
        );

        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// A pending, verified stage must SAY SO. Both `publishable_stage_covers`
    /// short-circuits used to return having recorded nothing, while the attribution note
    /// ("authoritative release signed by machine m3") is written earlier in the same
    /// check — so `status.toml` on a machine holding a ready build showed the SIGNER of a
    /// release it had already staged as its last decision, and the fact an operator was
    /// actually looking for appeared nowhere.
    #[test]
    fn a_stage_that_already_covers_the_candidate_records_the_staged_decision() {
        let staging = Staging::scratch("covered-status");
        let manifest = candidate_manifest();
        let commit = manifest.commit.as_deref().unwrap();
        write_ready(&staging, manifest.build_number, commit, &manifest.sha256);
        write_bundle_identity(&staging, manifest.build_number, commit);
        assert!(
            publishable_stage_covers(&staging, &manifest),
            "precondition: this stage covers the candidate"
        );

        // The signer note is what the real check writes just before the staging
        // decision, so it is the line the decision has to overwrite.
        crate::status::record(&staging, 1, "authoritative release signed by machine m3");
        record_covered_stage_status(&staging, 1, &manifest);
        let text = std::fs::read_to_string(&staging.status).expect("status written");
        assert!(
            text.contains("verified and ready to apply"),
            "the staged decision must be the last thing recorded: {text}"
        );
        assert!(
            text.contains(&format!("build {}", manifest.build_number)),
            "the staged build must be named: {text}"
        );
        assert!(
            !text.contains("signed by machine"),
            "the attribution note must not survive as the last decision: {text}"
        );

        // The marker can vanish between the predicate and the record (a concurrent
        // retire). Even then the arm records a DECISION rather than leaving the previous
        // line standing, which is the whole failure this exists to prevent.
        std::fs::remove_file(&staging.ready).unwrap();
        record_covered_stage_status(&staging, 1, &manifest);
        let text = std::fs::read_to_string(&staging.status).expect("status written");
        assert!(
            text.contains("already covers release build"),
            "the fallback must still record a decision: {text}"
        );

        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// THE regression this split exists for: a DOWNLOAD backoff must never hold an
    /// already-staged build hostage.
    ///
    /// The observed machine had `staged_build=1785510971`, `staged_version=0.10.0`,
    /// `relaunch_ready=true` and `failing_applies=0`, and still reported "skipping
    /// build 1785510971 for another 1387m (failed to stage 4 time(s))": a re-publish
    /// of the same build under a NEW digest had failed to stage four times, and its
    /// 24 h re-download window was also refusing to apply a bundle that was already
    /// downloaded, verified, extracted and marked ready. Different failures, different
    /// remedies — they must not share a timer.
    /// The health half of the same arm, pinned separately because it is the half that
    /// silently mis-reported healthy machines: a backed-off check must clear the
    /// ACQUISITION streaks (the list, appcast and authorization all worked, so unrelated
    /// blips must not accumulate toward PERSISTENT_AFTER for the whole 24 h life of the
    /// memo) and must NOT clear `stage_failures` — a machine that keeps failing to stage
    /// interleaves failures with backed-off checks, so resetting that streak here would
    /// mean the one class whose escalation says "the bytes arrive and will not become a
    /// bundle" could never reach PERSISTENT_AFTER.
    #[test]
    fn a_backed_off_check_clears_the_acquisition_streaks_but_never_the_stage_streak() {
        let staging = Staging::scratch("backoff-health");
        let ledger = staging.health();
        crate::health::Health::record_failure(&ledger, "network", "dns");
        crate::health::Health::record_failure(&ledger, "pipeline", "asset fetch failed");
        crate::health::Health::record_failure(&ledger, "manifest", "bad signature");
        crate::health::Health::record_failure(&ledger, "stage", "sha256 mismatch");

        let h = crate::health::Health::record_acquisition_success(&ledger);
        assert_eq!(h.network_failures, 0, "network streak must clear");
        assert_eq!(h.pipeline_failures, 0, "pipeline streak must clear");
        assert_eq!(h.manifest_failures, 0, "manifest streak must clear");
        assert_eq!(
            h.stage_failures, 1,
            "the stage streak is the one this arm must preserve"
        );

        let _ = std::fs::remove_dir_all(&staging.root);
    }

    #[test]
    fn a_stage_backoff_throttles_the_restage_never_an_already_staged_newer_build() {
        use crate::manifest::{FailedMark, RETRY_BACKOFF_SECS};

        let staging = Staging::scratch("stage-backoff-vs-apply");
        let manifest = candidate_manifest();
        let canonical_commit = manifest.commit.as_deref().unwrap().to_string();
        let running = manifest.build_number - 1;
        const NOW: u64 = 1_000_000;

        // Four consecutive stage failures of this candidate: the widest window.
        for _ in 0..4 {
            FailedMark::record_stage_failure(
                &staging.failed(),
                manifest.build_number,
                &manifest.sha256,
                NOW,
            );
        }

        // Nothing staged: the window stops the re-download, and there is no apply to
        // skip — the status line has to say exactly that.
        let backoff = stage_backoff(&staging, &manifest, running, NOW).expect("window open");
        assert_eq!(backoff.attempts, 4);
        assert_eq!(backoff.retry_in_secs, RETRY_BACKOFF_SECS[3]);
        assert!(backoff.applicable.is_none());
        let line = backoff.status_line(manifest.build_number);
        assert!(line.starts_with("skipping re-stage of build "), "{line}");
        assert!(line.contains("no verified stage to apply"), "{line}");

        // Now reproduce the observed machine: a published, locally verified stage for
        // a build strictly newer than the running one, while that SAME window is open.
        // Its digest differs from the manifest's (the re-publish), so the download
        // path really is still backed off — this is not the covered-stage shortcut.
        write_ready(
            &staging,
            manifest.build_number,
            &canonical_commit,
            &"ef".repeat(32),
        );
        write_bundle_identity(&staging, manifest.build_number, &canonical_commit);
        assert!(
            !publishable_stage_covers(&staging, &manifest),
            "the staged digest is not the manifest's, so the re-stage is genuinely due"
        );
        // A stage failure throttles DOWNLOADING; it must never gate APPLYING a bundle
        // that is already downloaded, verified, extracted and marked ready. Today's
        // logic returned early here and the ready build went unoffered.
        let backoff = stage_backoff(&staging, &manifest, running, NOW).expect("window open");
        let applicable = backoff.applicable.as_ref().expect("apply is not gated");
        assert_eq!(applicable.build_number, manifest.build_number);
        let line = backoff.status_line(manifest.build_number);
        assert!(line.starts_with("skipping re-stage of build "), "{line}");
        assert!(line.contains("NOT skipping apply"), "{line}");

        // A marker that is not STRICTLY newer is residue, not an update: still nothing
        // to apply.
        let residue = stage_backoff(&staging, &manifest, manifest.build_number, NOW);
        assert!(
            residue.expect("window open").applicable.is_none(),
            "a marker for the running build is not an applicable update"
        );

        // And the window governs only its own lane's deadline: past it, the re-stage
        // resumes on its own.
        let past_deadline = NOW + RETRY_BACKOFF_SECS[3];
        assert!(
            stage_backoff(&staging, &manifest, running, past_deadline).is_none(),
            "an expired window throttles nothing"
        );

        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// A QUARANTINED BUILD NEVER COMES BACK, AND THE LINE MUST NOT PROMISE A TIMER.
    ///
    /// The crash-loop revert wrote its poison with `retry_after = 0` meaning "forever",
    /// but `suppresses` — the only reader — treats a zero deadline as "already elapsed",
    /// which is also what a pre-budget legacy marker means. So the poison was written and
    /// then ignored: the very next check re-downloaded and re-applied the build that had
    /// just crash-looped, straight back into the loop the poison exists to break. This
    /// pins BOTH halves: the quarantine suppresses at any future time, and the old
    /// permanent-shaped marker still does not (so the flag is what is load-bearing, not
    /// the zero deadline).
    #[test]
    fn a_quarantined_build_is_skipped_forever_and_the_status_line_says_so() {
        use crate::manifest::FailedMark;

        let staging = Staging::scratch("stage-backoff-quarantine");
        let manifest = candidate_manifest();
        let running = manifest.build_number - 1;
        const NOW: u64 = 1_000_000;
        const A_DECADE: u64 = 10 * 365 * 24 * 60 * 60;

        // PRE-FIX CONTROL: the identity-only marker (the shape the poison used to take)
        // opens no window at all.
        FailedMark::record(&staging.failed(), manifest.build_number, &manifest.sha256);
        assert!(
            stage_backoff(&staging, &manifest, running, NOW).is_none(),
            "a zero deadline reads as elapsed — this is exactly why the poison was inert"
        );

        FailedMark::record_quarantine(&staging.failed(), manifest.build_number, &manifest.sha256);
        let backoff = stage_backoff(&staging, &manifest, running, NOW).expect("quarantined");
        assert!(backoff.quarantined);
        let line = backoff.status_line(manifest.build_number);
        assert!(line.contains("is quarantined"), "{line}");
        assert!(
            !line.contains("retrying automatically") && !line.contains("for another "),
            "a quarantine has no clock; the line must not imply one: {line}"
        );
        assert!(
            line.contains("will not retry it"),
            "and must say plainly that nothing here will: {line}"
        );

        // No deadline can lapse it.
        assert!(
            stage_backoff(&staging, &manifest, running, NOW + A_DECADE).is_some(),
            "a quarantine does not expire"
        );

        // THE ESCAPES: a different build, and a re-publish of the same build under a
        // different digest, both miss the memo's key and stage normally.
        let newer = Manifest {
            build_number: manifest.build_number + 1,
            ..candidate_manifest()
        };
        assert!(
            stage_backoff(&staging, &newer, running, NOW).is_none(),
            "a newer build is not the quarantined artifact"
        );
        let republished = Manifest {
            sha256: "cd".repeat(32),
            ..candidate_manifest()
        };
        assert_ne!(
            republished.sha256, manifest.sha256,
            "the re-publish fixture must actually differ or it proves nothing"
        );
        assert!(
            stage_backoff(&staging, &republished, running, NOW).is_none(),
            "a re-publish under a different digest is not the quarantined artifact"
        );

        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// Every removal on the download path is keyed to the CURRENT artifact's name, and
    /// those names carry the version — so a `.part` (or a whole container abandoned
    /// between the finalize rename and the post-stage removal) for a version the channel
    /// has moved past used to be unreachable by every code path, forever. The sweep is
    /// the only reclaim, and it runs under the stage lock before the next download.
    #[test]
    fn download_scratch_from_a_version_the_channel_moved_past_is_reclaimed() {
        let staging = Staging::scratch("download-sweep");

        // Two abandoned versions plus the current one: a killed transfer's `.part` and
        // a fully-downloaded container that was never staged.
        let stale_part = staging.download.join("aterm-0.52.0-mac.zip.part");
        let stale_container = staging.download.join("aterm-0.53.0-mac.zip");
        let current_part = staging.download.join("aterm-0.54.0-mac.zip.part");
        for leftover in [&stale_part, &stale_container, &current_part] {
            std::fs::write(leftover, b"abandoned bytes").unwrap();
        }
        // A directory is not scratch we own; the sweep must not recurse into one.
        let bystander = staging.download.join("not-ours");
        std::fs::create_dir_all(bystander.join("keep")).unwrap();

        sweep_download_scratch(&staging);

        assert!(
            !stale_part.exists(),
            "a killed transfer's part file for a superseded version has no other reclaimer"
        );
        assert!(
            !stale_container.exists(),
            "an abandoned full container for a superseded version has no other reclaimer"
        );
        assert!(
            !current_part.exists(),
            "the sweep subsumes the pre-download remove_file it replaced"
        );
        assert!(
            bystander.join("keep").is_dir(),
            "the sweep removes regular files only, never a directory tree"
        );

        // Idempotent, and silent on a download dir that does not exist yet (a first
        // check on a fresh machine reaches it before anything has created the dir).
        sweep_download_scratch(&staging);
        let _ = std::fs::remove_dir_all(&staging.root);
        sweep_download_scratch(&staging);
    }

    // -----------------------------------------------------------------------
    // The machine-roster tier, on the real client transport path.
    //
    // The chain's own gates are proved in `aterm_update_core::roster`; what these
    // exercise is the WIRING — that the assets are demanded, fetched, sequenced in the
    // documented order, that attribution comes back out, and that every refusal is a
    // refusal rather than a fallthrough.
    // -----------------------------------------------------------------------

    /// Obviously synthetic seeds, distinct from `SIGNING_SEED` so a mix-up between the
    /// channel key and a machine key cannot pass by coincidence.
    const MASTER_SEED_FIXTURE: [u8; 32] = [0xA7; 32];
    const M3_SEED_FIXTURE: [u8; 32] = [0xB7; 32];

    /// 2026-08-04T00:00:00Z.
    const ROSTER_NOW: i64 = 1_785_801_600;

    fn release_with_roster(tag: &str) -> Release {
        let version = tag.strip_prefix('v').unwrap_or(tag);
        let mut release = release_with_signed_appcast(tag, "m-url", "sig-url");
        release.assets.push(Asset {
            name: "aterm-machines.toml".into(),
            url: "roster-url".into(),
        });
        release.assets.push(Asset {
            name: "aterm-machines.toml.sig".into(),
            url: "roster-sig-url".into(),
        });
        assert!(
            release
                .assets
                .iter()
                .any(|a| a.name == format!("aterm-{version}.dmg"))
        );
        release
    }

    /// An appcast carrying the two attribution keys, signed by `machine`.
    fn attributed_manifest(machine_id: &str, roster_seq: u64) -> Vec<u8> {
        let mut text = String::from_utf8(manifest_bytes("0.10.0", 10, 0)).unwrap();
        text.push_str(&format!(
            "machine_id = {machine_id:?}\nroster_seq = {roster_seq}\n"
        ));
        text.into_bytes()
    }

    /// The full owner side: a master-signed roster listing m3, plus everything the client
    /// needs to check it.
    struct RosterFixture {
        master_pub: String,
        roster: Vec<u8>,
        roster_sig: Vec<u8>,
        manifest: Vec<u8>,
        manifest_sig: Vec<u8>,
        machine_pub: String,
        seq: u64,
    }

    fn roster_fixture(revoke_m3: bool) -> RosterFixture {
        let master = Ed25519KeyPair::from_seed_unchecked(&MASTER_SEED_FIXTURE).unwrap();
        let m3 = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE).unwrap();
        let machine_pub = b64(m3.public_key().as_ref());
        let seq = 4u64;
        let roster = aterm_update_core::roster::Roster {
            schema: 1,
            roster_seq: seq,
            valid_until: "2027-02-01T00:00:00Z".into(),
            machines: vec![aterm_update_core::roster::Machine {
                id: "m3".into(),
                pubkey: machine_pub.clone(),
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            }],
            revoked: if revoke_m3 { vec!["m3".into()] } else { vec![] },
        };
        let roster_bytes = roster.to_toml().unwrap().into_bytes();
        let manifest = attributed_manifest("m3", seq);
        RosterFixture {
            master_pub: b64(master.public_key().as_ref()),
            roster_sig: master.sign(&roster_bytes).as_ref().to_vec(),
            roster: roster_bytes,
            manifest_sig: m3.sign(&manifest).as_ref().to_vec(),
            manifest,
            machine_pub,
            seq,
        }
    }

    /// THE CLIENT HAPPY PATH: the roster assets are fetched, the chain passes, the release
    /// is selected, and the attribution names the machine that signed it.
    #[test]
    fn an_armed_master_accepts_a_rostered_release_and_reports_which_machine_signed() {
        let f = roster_fixture(false);
        let selected = candidate_for(vec![release_with_roster("v0.10.0")])
            .unwrap()
            .unwrap();
        let master = [f.master_pub.as_str()];
        let mut urls = Vec::new();
        let mut download = |url: &str, _max: u64| {
            urls.push(url.to_string());
            match url {
                "m-url" => Ok(f.manifest.clone()),
                "sig-url" => Ok(f.manifest_sig.clone()),
                "roster-url" => Ok(f.roster.clone()),
                "roster-sig-url" => Ok(f.roster_sig.clone()),
                other => Err(format!("unexpected fetch {other}")),
            }
        };
        let _ = crate::log_capture::take();
        let fetched = fetch_authoritative_release(
            Some(selected),
            &mut download,
            &RosterPolicy {
                master_pubkeys: &master,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: None,
            },
        );
        assert!(
            fetched.selected.is_some(),
            "the chain must accept this release"
        );
        let who = fetched.attribution.expect("attribution is reported");
        assert_eq!(who.machine_id, "m3");
        assert_eq!(who.pubkey_b64, f.machine_pub);
        assert_eq!(who.roster_seq, f.seq);
        assert_eq!(
            fetched.observed_roster_seq,
            Some(f.seq),
            "the accepted sequence must reach the caller so the durable floor ratchets"
        );
        assert!(
            urls.contains(&"roster-url".to_string())
                && urls.contains(&"roster-sig-url".to_string()),
            "the roster and its master signature must actually be fetched: {urls:?}"
        );
        // The LOG contract of an accept: one DEBUG attribution line, nothing at WARN —
        // and not again for the same release in the same process (2026-09-23: a
        // hundred lines for one release in the owner's log).
        let lines = crate::log_capture::take();
        assert!(
            lines
                .iter()
                .all(|(level, _)| *level != aterm_log::Level::Warn),
            "an accepted release must log nothing at WARN: {lines:?}"
        );
        let attributed: Vec<_> = lines
            .iter()
            .filter(|(_, msg)| msg.contains("was signed by machine"))
            .collect();
        assert_eq!(
            attributed.len(),
            1,
            "attribution is stated exactly once: {lines:?}"
        );
        assert_eq!(
            attributed[0].0,
            aterm_log::Level::Debug,
            "the note is DEBUG: {lines:?}"
        );
        let mut download_again = |url: &str, _max: u64| match url {
            "m-url" => Ok(f.manifest.clone()),
            "sig-url" => Ok(f.manifest_sig.clone()),
            "roster-url" => Ok(f.roster.clone()),
            "roster-sig-url" => Ok(f.roster_sig.clone()),
            other => Err(format!("unexpected fetch {other}")),
        };
        let again = fetch_authoritative_release(
            Some(
                candidate_for(vec![release_with_roster("v0.10.0")])
                    .unwrap()
                    .unwrap(),
            ),
            &mut download_again,
            &RosterPolicy {
                master_pubkeys: &master,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: None,
            },
        );
        assert!(again.selected.is_some(), "the release is still accepted");
        let lines = crate::log_capture::take();
        assert!(
            !lines
                .iter()
                .any(|(_, msg)| msg.contains("was signed by machine")),
            "once per release per process: {lines:?}"
        );
    }

    /// A RELEASE WITH NO ROSTER is refused under an armed master — structurally, before
    /// any roster crypto. An armed anchor never degrades to "unsigned is fine".
    #[test]
    fn an_armed_master_refuses_a_release_that_carries_no_roster() {
        let f = roster_fixture(false);
        // The plain signed release: appcast + signature, no roster assets.
        let selected = candidate_for(vec![release_with_signed_appcast(
            "v0.10.0", "m-url", "sig-url",
        )])
        .unwrap()
        .unwrap();
        let master = [f.master_pub.as_str()];
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(f.manifest.clone()),
            "sig-url" => Ok(f.manifest_sig.clone()),
            other => panic!("nothing else may be fetched, got {other}"),
        };
        let fetched = fetch_authoritative_release(
            Some(selected),
            &mut download,
            &RosterPolicy {
                master_pubkeys: &master,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: None,
            },
        );
        assert!(fetched.selected.is_none());
        assert!(fetched.manifest_rejected);
        assert!(fetched.attribution.is_none());
    }

    /// A REVOKED MACHINE is refused on the real path, though its signature is genuine.
    /// This is the whole point of the tier.
    #[test]
    fn a_revoked_machine_is_refused_on_the_client_transport_path() {
        let f = roster_fixture(true);
        let selected = candidate_for(vec![release_with_roster("v0.10.0")])
            .unwrap()
            .unwrap();
        let master = [f.master_pub.as_str()];
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(f.manifest.clone()),
            "sig-url" => Ok(f.manifest_sig.clone()),
            "roster-url" => Ok(f.roster.clone()),
            "roster-sig-url" => Ok(f.roster_sig.clone()),
            other => Err(format!("unexpected fetch {other}")),
        };
        let _ = crate::log_capture::take();
        let fetched = fetch_authoritative_release(
            Some(selected),
            &mut download,
            &RosterPolicy {
                master_pubkeys: &master,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: None,
            },
        );
        assert!(
            fetched.selected.is_none(),
            "a revoked machine must not publish"
        );
        assert!(fetched.manifest_rejected);
        assert!(fetched.attribution.is_none());
        // The contrast that keeps the rotation note's INFO demotion honest: an actual
        // refusal — the roster does NOT authorize the signer — still surfaces at WARN.
        let lines = crate::log_capture::take();
        assert!(
            lines
                .iter()
                .any(|(level, msg)| *level == aterm_log::Level::Warn
                    && msg.contains("refusing authoritative")),
            "a roster refusal is verification degradation and must WARN: {lines:?}"
        );
    }

    /// A REPLAYED PRE-REVOCATION ROSTER is refused by the durable floor, and a STALE one
    /// by the freshness window. Both are checked before any artifact crypto, and both are
    /// driven here through the real transport path.
    #[test]
    fn a_rolled_back_or_lapsed_roster_is_refused_on_the_client_transport_path() {
        let f = roster_fixture(false);
        let master = [f.master_pub.as_str()];
        let refuse_with = |floor_seq: u64, now_unix: i64| {
            let selected = candidate_for(vec![release_with_roster("v0.10.0")])
                .unwrap()
                .unwrap();
            let mut download = |url: &str, _max: u64| match url {
                "m-url" => Ok(f.manifest.clone()),
                "sig-url" => Ok(f.manifest_sig.clone()),
                "roster-url" => Ok(f.roster.clone()),
                "roster-sig-url" => Ok(f.roster_sig.clone()),
                other => Err(format!("unexpected fetch {other}")),
            };
            fetch_authoritative_release(
                Some(selected),
                &mut download,
                &RosterPolicy {
                    master_pubkeys: &master,
                    floor_seq,
                    now_unix,
                    floor_refresh: None,
                },
            )
        };
        // A client that has durably seen sequence 5 refuses this seq-4 roster forever.
        let rolled_back = refuse_with(f.seq + 1, ROSTER_NOW);
        assert!(rolled_back.selected.is_none() && rolled_back.manifest_rejected);
        assert_eq!(
            rolled_back.observed_roster_seq, None,
            "a rolled-back roster failed admission: not an observation, moves no floor"
        );
        // Past `valid_until`, the same roster is refused even with no floor at all — the
        // only defence a fresh install has.
        let lapsed = refuse_with(0, 1_900_000_000);
        assert!(lapsed.selected.is_none() && lapsed.manifest_rejected);
        assert_eq!(
            lapsed.observed_roster_seq, None,
            "a stale roster failed admission: not an observation, moves no floor"
        );
        // Negative control: at the same sequence and inside the window it is accepted, so
        // the two refusals above are the gates and not a broken fixture.
        assert!(refuse_with(f.seq, ROSTER_NOW).selected.is_some());
    }

    /// A GENUINE SIGNATURE WITH A MISMATCHED LABEL is refused after the parse. The bytes
    /// verify under m3's key, but they claim to come from `m99`, and attribution follows
    /// the key.
    #[test]
    fn a_release_whose_declared_machine_disagrees_with_the_signer_is_refused() {
        let master = Ed25519KeyPair::from_seed_unchecked(&MASTER_SEED_FIXTURE).unwrap();
        let m3 = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE).unwrap();
        let machine_pub = b64(m3.public_key().as_ref());
        let roster = aterm_update_core::roster::Roster {
            schema: 1,
            roster_seq: 4,
            valid_until: "2027-02-01T00:00:00Z".into(),
            machines: vec![aterm_update_core::roster::Machine {
                id: "m3".into(),
                pubkey: machine_pub.clone(),
                added_at: String::new(),
                not_after: None,
            }],
            revoked: vec![],
        };
        let roster_bytes = roster.to_toml().unwrap().into_bytes();
        let roster_sig = master.sign(&roster_bytes).as_ref().to_vec();
        // m3 signs bytes that CLAIM to be m99's.
        let lying = attributed_manifest("m99", 4);
        let lying_sig = m3.sign(&lying).as_ref().to_vec();

        let master_pub = b64(master.public_key().as_ref());
        let masters = [master_pub.as_str()];
        let selected = candidate_for(vec![release_with_roster("v0.10.0")])
            .unwrap()
            .unwrap();
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(lying.clone()),
            "sig-url" => Ok(lying_sig.clone()),
            "roster-url" => Ok(roster_bytes.clone()),
            "roster-sig-url" => Ok(roster_sig.clone()),
            other => Err(format!("unexpected fetch {other}")),
        };
        let fetched = fetch_authoritative_release(
            Some(selected),
            &mut download,
            &RosterPolicy {
                master_pubkeys: &masters,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: None,
            },
        );
        assert!(
            fetched.selected.is_none() && fetched.manifest_rejected,
            "a signature cannot be relabelled onto another machine's identity"
        );
    }

    /// AN UNPINNED MASTER (a fork with no master of its own) REMOVES THE SIGNATURE TIER:
    /// only the appcast is fetched — no signature, no roster — no attribution is
    /// produced, and the release proceeds on the channel repository, the sha256 and the
    /// codesign checks. Exercised with a synthetic empty anchor; this tree is armed.
    #[test]
    fn an_unpinned_master_never_touches_the_signature_or_the_roster() {
        let f = roster_fixture(false);
        let selected = candidate_for(vec![release_with_roster("v0.10.0")])
            .unwrap()
            .unwrap();
        let mut urls = Vec::new();
        let mut download = |url: &str, _max: u64| {
            urls.push(url.to_string());
            match url {
                "m-url" => Ok(f.manifest.clone()),
                other => panic!("an absent tier must fetch nothing else, got {other}"),
            }
        };
        let fetched =
            fetch_authoritative_release(Some(selected), &mut download, &RosterPolicy::INERT);
        assert!(fetched.selected.is_some());
        assert!(fetched.attribution.is_none());
        assert_eq!(fetched.observed_roster_seq, None);
        assert_eq!(urls, ["m-url"]);
    }

    // -----------------------------------------------------------------------
    // THE ROSTER IS THE AUTHORITY.
    //
    // Everything below drives the REAL transport path; the chain's own gates are
    // proved in `aterm_update_core::roster`.
    // -----------------------------------------------------------------------

    /// A second master, obviously synthetic and distinct from everything above so a
    /// mix-up cannot pass by coincidence.
    const OTHER_MASTER_FIXTURE: [u8; 32] = [0xA8; 32];

    /// Everything the owner publishes for one release, with every placement under the
    /// caller's control — which machine is on the roster, which is revoked, which one
    /// signed, and at which generation. The placements ARE the subject of these tests.
    struct Chain {
        master_pub: String,
        roster: Vec<u8>,
        roster_sig: Vec<u8>,
        manifest: Vec<u8>,
        manifest_sig: Vec<u8>,
    }

    fn pub_b64(seed: &[u8; 32]) -> String {
        b64(Ed25519KeyPair::from_seed_unchecked(seed)
            .unwrap()
            .public_key()
            .as_ref())
    }

    fn chain(
        machines: &[(&str, [u8; 32])],
        revoked: &[&str],
        signer: (&str, [u8; 32]),
        seq: u64,
        master_seed: &[u8; 32],
        claimed_seq: u64,
    ) -> Chain {
        let master = Ed25519KeyPair::from_seed_unchecked(master_seed).unwrap();
        let roster = aterm_update_core::roster::Roster {
            schema: 1,
            roster_seq: seq,
            valid_until: "2027-02-01T00:00:00Z".into(),
            machines: machines
                .iter()
                .map(|(id, seed)| aterm_update_core::roster::Machine {
                    id: (*id).to_string(),
                    pubkey: pub_b64(seed),
                    added_at: "2026-08-04T00:00:00Z".into(),
                    not_after: None,
                })
                .collect(),
            revoked: revoked.iter().map(|s| (*s).to_string()).collect(),
        };
        let roster_bytes = roster.to_toml().unwrap().into_bytes();
        let manifest = attributed_manifest(signer.0, claimed_seq);
        let signing = Ed25519KeyPair::from_seed_unchecked(&signer.1).unwrap();
        Chain {
            master_pub: b64(master.public_key().as_ref()),
            roster_sig: master.sign(&roster_bytes).as_ref().to_vec(),
            roster: roster_bytes,
            manifest_sig: signing.sign(&manifest).as_ref().to_vec(),
            manifest,
        }
    }

    /// Drive the real path over a [`Chain`] with an explicit master policy. Returns both
    /// the verdict and the URLs that were actually fetched.
    fn run_chain(
        c: &Chain,
        masters: &[&str],
        floor_seq: u64,
        now_unix: i64,
    ) -> (AuthoritativeFetch, Vec<String>) {
        let selected = candidate_for(vec![release_with_roster("v0.10.0")])
            .unwrap()
            .unwrap();
        let mut urls = Vec::new();
        let mut download = |url: &str, _max: u64| {
            urls.push(url.to_string());
            match url {
                "m-url" => Ok(c.manifest.clone()),
                "sig-url" => Ok(c.manifest_sig.clone()),
                "roster-url" => Ok(c.roster.clone()),
                "roster-sig-url" => Ok(c.roster_sig.clone()),
                other => Err(format!("unexpected fetch {other}")),
            }
        };
        let fetched = fetch_authoritative_release(
            Some(selected),
            &mut download,
            &RosterPolicy {
                master_pubkeys: masters,
                floor_seq,
                now_unix,
                floor_refresh: None,
            },
        );
        (fetched, urls)
    }

    /// THE OBSERVATION-RATCHET SEMANTICS, pinned end to end so the code and its comment
    /// can never again disagree about when the floor moves. "A client that merely SAW
    /// roster generation n must refuse n-1 forever after" means the observation is
    /// reported on ADMISSION — not on successful artifact authorization — and exactly on
    /// admission: a roster that never passed `admit` (stale, rolled back, unverifiable)
    /// has NOT been observed and moves nothing.
    ///
    /// MUTATION: move the `observed_roster_seq` assignment back inside the `Ok(who)` arm
    /// of `authorize_by_roster`'s caller (the pre-fix code) and the first half fails; make
    /// it fire before `admit` and the second half fails.
    #[test]
    fn the_roster_floor_ratchets_on_observation_not_on_acceptance() {
        // ACT 1 — the attack the ratchet exists for. Generation 10 revokes m3; m3 itself
        // signed the release. The release is refused AND generation 10 is observed.
        let revoking = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &["m3"],
            ("m3", M3_SEED_FIXTURE),
            10,
            &MASTER_SEED_FIXTURE,
            10,
        );
        let masters_owned = revoking.master_pub.clone();
        let masters = [masters_owned.as_str()];
        let (saw_revocation, _) = run_chain(&revoking, &masters, 9, ROSTER_NOW);
        assert!(saw_revocation.selected.is_none() && saw_revocation.manifest_rejected);
        assert_eq!(
            saw_revocation.observed_roster_seq,
            Some(10),
            "observing the revoking generation must be reported for the durable ratchet"
        );

        // ACT 2 — the replay, against the floor ACT 1's observation produced. The seq-9
        // roster still lists m3 and is still inside its freshness window; with the floor
        // at 10 it must be refused, and it is NOT an observation (admit failed).
        let pre_revocation = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            9,
            &MASTER_SEED_FIXTURE,
            9,
        );
        let floor_after_observation = saw_revocation.observed_roster_seq.unwrap();
        let (replayed, _) = run_chain(
            &pre_revocation,
            &masters,
            floor_after_observation,
            ROSTER_NOW,
        );
        assert!(
            replayed.selected.is_none() && replayed.manifest_rejected,
            "the replayed pre-revocation roster must be refused by the observed floor"
        );
        assert_eq!(
            replayed.observed_roster_seq, None,
            "a roster that failed admission was never observed and must not move the floor"
        );

        // NEGATIVE CONTROL — the same seq-9 roster is accepted below the floor ACT 1
        // produced, so ACT 2's refusal is the ratchet's doing and not a broken fixture.
        let (accepted, _) = run_chain(&pre_revocation, &masters, 9, ROSTER_NOW);
        assert!(accepted.selected.is_some());
        assert_eq!(accepted.observed_roster_seq, Some(9));
    }

    /// THE ADMISSION-TIME FLOOR RE-READ (the check-vs-ratchet TOCTOU): the policy's
    /// `floor_seq` snapshot is taken before any network I/O, and a concurrent instance
    /// can ratchet the durable floor while the roster assets download. When the caller
    /// provides `floor_refresh`, admission must consult the RE-READ value, so a roster
    /// generation a concurrent check has already superseded is refused even though the
    /// stale snapshot would admit it.
    ///
    /// MUTATION: drop the `floor_refresh` consultation in `authorize_by_roster` (admit
    /// against `policy.floor_seq` alone — the pre-fix code) and the refusal below flips
    /// to an acceptance.
    #[test]
    fn admission_rereads_the_durable_floor_a_concurrent_check_may_have_ratcheted() {
        let c = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            9,
            &MASTER_SEED_FIXTURE,
            9,
        );
        let masters = [c.master_pub.as_str()];
        let run_with_refresh = |refreshed_floor: u64| {
            let selected = candidate_for(vec![release_with_roster("v0.10.0")])
                .unwrap()
                .unwrap();
            let mut download = |url: &str, _max: u64| match url {
                "m-url" => Ok(c.manifest.clone()),
                "sig-url" => Ok(c.manifest_sig.clone()),
                "roster-url" => Ok(c.roster.clone()),
                "roster-sig-url" => Ok(c.roster_sig.clone()),
                other => Err(format!("unexpected fetch {other}")),
            };
            // The concurrent actor's ratchet, visible only through the re-read: the
            // snapshot below stays at 0, exactly as in the race.
            let refresh = move || refreshed_floor;
            fetch_authoritative_release(
                Some(selected),
                &mut download,
                &RosterPolicy {
                    master_pubkeys: &masters,
                    floor_seq: 0,
                    now_unix: ROSTER_NOW,
                    floor_refresh: Some(&refresh),
                },
            )
        };
        // The other instance recorded generation 10 mid-download: this seq-9 roster is
        // superseded and must be refused, stale snapshot notwithstanding.
        let raced = run_with_refresh(10);
        assert!(
            raced.selected.is_none() && raced.manifest_rejected,
            "admission must honour the re-read floor, not the pre-download snapshot"
        );
        assert_eq!(raced.observed_roster_seq, None);
        // NEGATIVE CONTROL: with the durable floor still at 9, the same chain is
        // admitted — so the refusal above is the re-read's doing.
        let quiet = run_with_refresh(9);
        assert!(quiet.selected.is_some());
        assert_eq!(quiet.observed_roster_seq, Some(9));
    }

    /// The under-stage-lock half of the same defence: whether an already-authorized
    /// release must be dropped because the durable floor advanced past its generation
    /// while the check was in flight. Strictly `<` — this run's own ratchet write makes
    /// the floor EQUAL in the quiescent case, and an inert tier (no observation) can
    /// never be superseded.
    /// A REVOCATION MUST REACH AN ALREADY-STAGED BUILD, and must reach nothing else.
    ///
    /// The stage lane refuses a release whose signer the roster withdrew, but a build
    /// already on disk was authorized earlier and nothing revisited it: staged at 10:00
    /// by a machine revoked at 10:30, applied anyway (in-session, or at the next launch).
    #[test]
    fn a_revoked_signer_withdraws_the_stage_it_authorized_and_no_other() {
        let revoked = vec!["m11".to_string(), "m19".to_string()];
        assert!(revocation_withdraws_stage(&revoked, Some("m11")));
        assert!(revocation_withdraws_stage(&revoked, Some("m19")));
        // The machine that signed this stage is still on the roster.
        assert!(!revocation_withdraws_stage(&revoked, Some("m3")));
        // Nothing revoked at all is the overwhelmingly common case.
        assert!(!revocation_withdraws_stage(&[], Some("m3")));
        // A marker written before the attribution fields existed names no machine. It
        // must be LEFT ALONE: "I cannot tell who signed this" is not evidence of
        // withdrawal, and retiring on it would re-download on every check forever —
        // the same never-updates shape the apply-lane seq gate had to be removed for.
        assert!(!revocation_withdraws_stage(&revoked, None));
    }

    #[test]
    fn a_release_is_held_when_its_roster_generation_was_superseded_mid_check() {
        // Quiescent: our own write put the floor at our generation.
        assert!(!roster_authority_superseded(Some(9), 9));
        // Raced: a concurrent instance recorded 10 — the release's authority is stale.
        assert!(roster_authority_superseded(Some(9), 10));
        // Inert tier: nothing was observed, nothing can be superseded.
        assert!(!roster_authority_superseded(None, u64::MAX));
    }

    /// A STRANDED CLIENT IS TOLD TO REINSTALL, NOT TO WAIT FOR THE PUBLISHER
    /// (2026-09-14, audit LT-3).
    ///
    /// One refusal is permanent for the BUILD rather than for the channel: the pinned
    /// paper master cannot verify the roster (a master rotation this build predates).
    /// The head is the one candidate and nothing falls back, so retrying changes nothing
    /// and no release the publisher cuts will ever verify here. The refusal used to reach
    /// the ledger as the fixed "manifest(s) fetched but rejected (signature/parse)" and
    /// the persistent line blamed the publisher; now the reason itself travels
    /// ([`AuthoritativeFetch::rejection_reason`]) and the anchor arm carries
    /// [`STALE_ANCHOR_KEY`] and the reinstall remedy. The OTHER roster refusals (a lapsed
    /// or rolled-back generation) are the publisher's to fix and must NOT carry the key —
    /// a reinstall would change nothing for them.
    #[test]
    fn a_stale_anchor_names_the_reinstall_and_a_publisher_fault_does_not() {
        // (A) A roster signed by a master this build does not pin.
        let good = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        let masters = [good.master_pub.as_str()];
        let wrong_master = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &OTHER_MASTER_FIXTURE,
            4,
        );
        let (forged, _) = run_chain(&wrong_master, &masters, 0, ROSTER_NOW);
        let reason = forged.rejection_reason.as_deref().expect("reason");
        assert!(
            is_stale_anchor_refusal(reason) && reason.contains(STALE_ANCHOR_REMEDY),
            "an unverifiable roster is the stranded client's arm: {reason}"
        );
        assert!(
            reason.contains("v0.10.0"),
            "the reason still names the release it refused: {reason}"
        );

        // (B) The publisher's faults keep the publisher wording: a lapsed roster and a
        //     rolled-back one are refused with a reason that carries NO reinstall.
        let (lapsed, _) = run_chain(&good, &masters, 0, 1_900_000_000);
        let reason = lapsed.rejection_reason.as_deref().expect("reason");
        assert!(
            lapsed.manifest_rejected && !is_stale_anchor_refusal(reason),
            "a lapsed roster is the publisher's to refresh, not a reinstall: {reason}"
        );
        let (replayed, _) = run_chain(&good, &masters, 5, ROSTER_NOW);
        let reason = replayed.rejection_reason.as_deref().expect("reason");
        assert!(
            replayed.manifest_rejected && !is_stale_anchor_refusal(reason),
            "a rolled-back roster is a replay, not a stale anchor: {reason}"
        );
        // And a verified, accepted release carries no reason at all.
        let (accepted, _) = run_chain(&good, &masters, 0, ROSTER_NOW);
        assert!(accepted.selected.is_some() && accepted.rejection_reason.is_none());
    }

    /// A ROSTER ASSET THAT WILL NOT DOWNLOAD is a TRANSPORT failure, not a publisher
    /// error — and it still refuses.
    ///
    /// The distinction is what the operator is told. `manifest_rejected` escalates to
    /// "this Mac cannot install any release until that is fixed at the publisher", which
    /// is a false accusation for a flaky network — and now that the roster is the sole
    /// authority, every armed client's fetch of it is on that path.
    ///
    /// It is also the ATTACKER'S branch: dropping or stalling the `aterm-machines.toml`
    /// fetch must never produce anything but a refusal. The closure below serves a good
    /// appcast and a good appcast signature, so the only thing missing is the roster.
    #[test]
    fn a_roster_that_cannot_be_fetched_is_reported_as_transport_and_still_refuses() {
        let c = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        // Control: the same chain with its roster served is accepted, so the refusal
        // below is about the missing roster and nothing else.
        let masters_control = [c.master_pub.as_str()];
        let (control, _) = run_chain(&c, &masters_control, 0, ROSTER_NOW);
        assert!(
            control.selected.is_some(),
            "precondition: the served chain passes"
        );
        let masters = [c.master_pub.as_str()];
        let selected = candidate_for(vec![release_with_roster("v0.10.0")])
            .unwrap()
            .unwrap();
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(c.manifest.clone()),
            // Served, deliberately. See the doc comment.
            "sig-url" => Ok(c.manifest_sig.clone()),
            "roster-url" => Err("connection reset".to_string()),
            other => Err(format!("unexpected fetch {other}")),
        };
        let fetched = fetch_authoritative_release(
            Some(selected),
            &mut download,
            &RosterPolicy {
                master_pubkeys: &masters,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: None,
            },
        );
        assert!(fetched.selected.is_none(), "no roster, no release");
        assert!(
            fetched.attribution.is_none(),
            "nothing may be accepted, attributed or not"
        );
        assert!(
            fetched.appcast_fetch_error,
            "a fetch failure is a pipeline-class failure"
        );
        assert!(
            !fetched.manifest_rejected,
            "a network failure must not accuse the publisher of shipping a bad release"
        );

        // THE SAME SUPPRESSION, ONE ASSET OVER. The roster body arrives and its master
        // signature does not — the other half an attacker can withhold independently, and
        // a second place a fallback could be bolted on.
        let selected = candidate_for(vec![release_with_roster("v0.10.0")])
            .unwrap()
            .unwrap();
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(c.manifest.clone()),
            "sig-url" => Ok(c.manifest_sig.clone()),
            "roster-url" => Ok(c.roster.clone()),
            "roster-sig-url" => Err("connection reset".to_string()),
            other => Err(format!("unexpected fetch {other}")),
        };
        let fetched = fetch_authoritative_release(
            Some(selected),
            &mut download,
            &RosterPolicy {
                master_pubkeys: &masters,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: None,
            },
        );
        assert!(
            fetched.selected.is_none() && fetched.attribution.is_none(),
            "a roster with no master signature authorizes nothing"
        );
        assert!(fetched.appcast_fetch_error);
        assert!(!fetched.manifest_rejected);
    }

    /// THE RATCHET AND THE BIND, on the transport path, under the new authority.
    ///
    /// `roster_seq` appears in two documents: the roster's own generation, and the copy
    /// inside the signed appcast. The appcast may not claim a generation NEWER than the
    /// roster that authorized it — that is what stops an old roster being paired with a
    /// new release. It MAY claim an older one (2026-08-18): the roster travels as an
    /// asset on the channel head, and a join attaches the new pair to releases attributed
    /// under the previous generation — the steady state of a multi-machine channel, and
    /// verifying under a newer roster is strictly stronger. And the accepted generation
    /// must reach the caller, or the durable floor never advances and the replay defence
    /// is inert.
    #[test]
    fn the_roster_generation_must_agree_between_the_roster_and_the_signed_manifest() {
        // The roster is at generation 6; the appcast claims 7 — inside its own signed
        // bytes, so this is a genuine signature over a claim the roster cannot back:
        // an OLD roster presented with a NEWER release.
        let lying = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            6,
            &MASTER_SEED_FIXTURE,
            7,
        );
        let masters = [lying.master_pub.as_str()];
        let (refused, _) = run_chain(&lying, &masters, 0, ROSTER_NOW);
        assert!(
            refused.selected.is_none() && refused.manifest_rejected,
            "an appcast may not name a roster generation newer than the one that \
             authorized it"
        );
        // The roster is at generation 6; the appcast was attributed under 5 — a release
        // published before a join, now carrying the newer pair: ADMITTED, and the floor
        // ratchets to the roster's generation (6), not the manifest's.
        let redressed = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            6,
            &MASTER_SEED_FIXTURE,
            5,
        );
        let (admitted, _) = run_chain(&redressed, &masters, 0, ROSTER_NOW);
        assert!(
            admitted.selected.is_some(),
            "a newer roster paired with an older release is the post-join steady state"
        );
        assert_eq!(admitted.observed_roster_seq, Some(6));

        // Truthful, same everything else: accepted, and the generation is handed back for
        // the durable floor to ratchet.
        let honest = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            6,
            &MASTER_SEED_FIXTURE,
            6,
        );
        let (ok, _) = run_chain(&honest, &masters, 0, ROSTER_NOW);
        assert!(ok.selected.is_some());
        assert_eq!(ok.observed_roster_seq, Some(6));
        // ...and the floor it just advanced past now refuses that same generation's
        // predecessor forever.
        let older = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            5,
            &MASTER_SEED_FIXTURE,
            5,
        );
        let (rolled, _) = run_chain(&older, &masters, 6, ROSTER_NOW);
        assert!(rolled.selected.is_none() && rolled.manifest_rejected);
    }

    // ---------------------------------------------------------------------------
    // THE WEB LANE, MEASURED.
    //
    // Both transports are injected — the token lane's LIST fetcher and the web lane's
    // HEAD — and the asset downloader is a closure, so every request a check makes is
    // COUNTED here rather than claimed. On the web lane the LIST fake panics: "zero
    // `api.github.com` requests" is then a fact the compiler and the test runner
    // jointly enforce, on the happy path and on every failure path.
    // ---------------------------------------------------------------------------

    /// The release the fixture channel's pointer names.
    const WEB_TAG: &str = "v0.10.0";

    /// The DERIVED tag-specific URL of `name` on the fixture channel.
    fn tag_url(tag: &str, name: &str) -> String {
        aterm_update_core::cdn::release_download_url("alabsystems", "aterm", tag, name).unwrap()
    }

    /// The DERIVED tag-specific URL prefix (`…/releases/download/<tag>/`) every GET of
    /// `tag` must carry. (The builder refuses an empty name, so it is taken off a
    /// placeholder.)
    fn tag_prefix(tag: &str) -> String {
        let url = tag_url(tag, "x");
        url[..url.len() - 1].to_string()
    }

    /// The one evergreen URL a web-lane check is allowed to HEAD.
    fn evergreen_url() -> String {
        aterm_update_core::pointer::latest_download_url("alabsystems", "aterm", APPCAST_ASSET)
            .unwrap()
    }

    /// GitHub's measured answer to the evergreen HEAD: a 302 whose `Location` is the
    /// tag-specific appcast URL of `tag`.
    fn redirect_to(tag: &str) -> Result<HeadAnswer, HttpError> {
        Ok(HeadAnswer {
            code: 302,
            location: Some(tag_url(tag, APPCAST_ASSET)),
        })
    }

    /// The owner side of one web-lane release: a master-signed roster naming m3, and an
    /// appcast for [`WEB_TAG`] signed by m3 whose `url` field is `container_url` — the
    /// derived DMG URL by default, or whatever a test wants to bind against.
    struct WebChannel {
        master_pub: String,
        appcast: Vec<u8>,
        appcast_sig: Vec<u8>,
        roster: Vec<u8>,
        roster_sig: Vec<u8>,
    }

    fn web_channel(container_url: Option<&str>) -> WebChannel {
        let master = Ed25519KeyPair::from_seed_unchecked(&MASTER_SEED_FIXTURE).unwrap();
        let m3 = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE).unwrap();
        let machine_pub = b64(m3.public_key().as_ref());
        let roster = aterm_update_core::roster::Roster {
            schema: 1,
            roster_seq: 4,
            valid_until: "2027-02-01T00:00:00Z".into(),
            machines: vec![aterm_update_core::roster::Machine {
                id: "m3".into(),
                pubkey: machine_pub,
                added_at: "2026-08-04T00:00:00Z".into(),
                not_after: None,
            }],
            revoked: vec![],
        };
        let roster = roster.to_toml().unwrap().into_bytes();
        let mut appcast = String::from_utf8(manifest_bytes("0.10.0", 10, 0)).unwrap();
        if let Some(url) = container_url {
            appcast.push_str(&format!("url = {url:?}\n"));
        }
        appcast.push_str("machine_id = \"m3\"\nroster_seq = 4\n");
        let appcast = appcast.into_bytes();
        WebChannel {
            master_pub: b64(master.public_key().as_ref()),
            appcast_sig: m3.sign(&appcast).as_ref().to_vec(),
            appcast,
            roster_sig: master.sign(&roster).as_ref().to_vec(),
            roster,
        }
    }

    impl WebChannel {
        /// The pinned-master policy every armed client runs, over `masters` (the
        /// caller's `[channel.master_pub.as_str()]` — a borrow the policy outlives).
        fn policy<'a>(&self, masters: &'a [&'a str]) -> RosterPolicy<'a> {
            RosterPolicy {
                master_pubkeys: masters,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: None,
            }
        }

        /// Serve the four assets of [`WEB_TAG`] by their DERIVED URL — nothing else
        /// exists, exactly like the real download host.
        fn serve(&self, url: &str) -> Result<Vec<u8>, String> {
            let base = tag_prefix(WEB_TAG);
            let Some(name) = url.strip_prefix(base.as_str()) else {
                return Err(format!("HTTP 404: no such asset {url}"));
            };
            match name {
                APPCAST_ASSET => Ok(self.appcast.clone()),
                APPCAST_SIG_ASSET => Ok(self.appcast_sig.clone()),
                aterm_update_core::roster::ROSTER_ASSET => Ok(self.roster.clone()),
                aterm_update_core::roster::ROSTER_SIG_ASSET => Ok(self.roster_sig.clone()),
                _ => Err(format!("HTTP 404: no such asset {url}")),
            }
        }
    }

    /// The build every web-lane fixture check runs as.
    const WEB_BUILD: u64 = 5;

    /// Run the web lane's acquisition against a pointer that answers `answer`, with the
    /// LIST fake armed. `known` writes a ledger that authorized that tag AS THIS BUILD,
    /// AGAINST THE FIXTURE SOURCE (the only ledger the shortcut may trust). Returns the
    /// outcome and every URL the HEAD transport was asked.
    fn acquire_web(
        staging: &Staging,
        known: Option<&str>,
        answer: impl FnMut() -> Result<HeadAnswer, HttpError>,
    ) -> (Result<Acquisition, String>, Vec<String>) {
        if let Some(tag) = known {
            write_ledger(staging, tag, WEB_BUILD, "alabsystems/aterm");
        } else {
            let _ = std::fs::remove_file(&staging.status);
        }
        acquire_web_from(staging, &test_source(), answer)
    }

    /// A ledger that authorized `tag` while running `build`, against `source`.
    fn write_ledger(staging: &Staging, tag: &str, build: u64, source: &str) {
        std::fs::write(
            &staging.status,
            format!(
                "schema = 1\noutcome = \"x\"\ncurrent_build = {build}\nlatest_tag = {tag:?}\n\
                 latest_source = {source:?}\nlatest_authorized_build = {build}\n\
                 latest_release_build = {build}\n"
            ),
        )
        .unwrap();
    }

    /// [`acquire_web`] over whatever ledger is on disk, as `source`.
    fn acquire_web_from(
        staging: &Staging,
        source: &Source,
        mut answer: impl FnMut() -> Result<HeadAnswer, HttpError>,
    ) -> (Result<Acquisition, String>, Vec<String>) {
        let mut heads = Vec::new();
        let mut head = |url: &str| {
            heads.push(url.to_string());
            answer()
        };
        let outcome = acquire(staging, WEB_BUILD, source, &mut head);
        (outcome, heads)
    }

    /// **THE measurement.** A web-lane check whose pointer has MOVED costs ONE HEAD of
    /// the evergreen URL plus FOUR GETs — appcast, its signature, the roster, its
    /// signature — every one addressed by the DERIVED tag-specific URL, none through
    /// `latest` again, none to `api.github.com`, none with a credential; the armed
    /// roster chain runs on those bytes unchanged and the manifest's `url` binds to
    /// the pointer's tag.
    #[test]
    fn a_web_lane_check_makes_zero_api_requests_and_only_tag_specific_gets() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("zero-api");
        let source = test_source();
        let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
        let masters = [channel.master_pub.as_str()];
        let (outcome, heads) = acquire_web(&staging, None, || redirect_to(WEB_TAG));
        assert_eq!(
            heads,
            vec![evergreen_url()],
            "one HEAD, of the evergreen URL"
        );
        let Ok(Acquisition::Proceed(candidate)) = outcome else {
            panic!("a moved pointer proceeds to acquisition: {outcome:?}");
        };
        assert_eq!(candidate.version, "0.10.0");
        assert_eq!(candidate.release.tag_name, WEB_TAG);
        for asset in &candidate.release.assets {
            assert_eq!(
                asset.url,
                tag_url(WEB_TAG, &asset.name),
                "every asset URL is the derived tag-specific one"
            );
            assert!(!aterm_update_core::cdn::is_api_host(&asset.url));
        }

        let mut gets: Vec<String> = Vec::new();
        let mut download = |url: &str, _max: u64| {
            gets.push(url.to_string());
            channel.serve(url)
        };
        let fetched =
            fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
        let (manifest, release, _) = fetched
            .selected
            .as_ref()
            .expect("the armed chain accepts the rostered release over the web lane");
        assert_eq!(
            fetched
                .attribution
                .as_ref()
                .map(|who| who.machine_id.as_str()),
            Some("m3")
        );
        assert_eq!(fetched.observed_roster_seq, Some(4));
        web_container_url_agrees(&source, &release.tag_name, manifest)
            .expect("the manifest's url names the derived container URL under the tag");

        // The request ledger.
        let prefix = tag_prefix(WEB_TAG);
        assert_eq!(
            gets.len(),
            4,
            "appcast, its sig, the roster, its sig: {gets:?}"
        );
        assert_eq!(gets[0], tag_url(WEB_TAG, APPCAST_ASSET));
        for url in &gets {
            assert!(url.starts_with(&prefix), "not tag-specific: {url}");
            assert!(
                !url.contains("/releases/latest/"),
                "through `latest`: {url}"
            );
            assert!(!aterm_update_core::cdn::is_api_host(url), "metered: {url}");
        }
        let mut names: Vec<&str> = gets.iter().map(|u| &u[prefix.len()..]).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                APPCAST_ASSET,
                APPCAST_SIG_ASSET,
                aterm_update_core::roster::ROSTER_ASSET,
                aterm_update_core::roster::ROSTER_SIG_ASSET,
            ]
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// THE STEADY STATE: a pointer naming the tag this ledger last authorized ends the
    /// check after its one HEAD — no GET, no API. Any OTHER recorded tag, or none, is
    /// re-judged.
    #[test]
    fn the_steady_state_is_one_head_when_the_pointer_names_the_authorized_tag() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("steady");
        let (outcome, heads) = acquire_web(&staging, Some(WEB_TAG), || redirect_to(WEB_TAG));
        assert_eq!(heads, vec![evergreen_url()]);
        let Ok(Acquisition::UpToDate { tag }) = outcome else {
            panic!("an unchanged pointer is the steady state: {outcome:?}");
        };
        assert_eq!(tag, WEB_TAG);
        assert_eq!(
            crate::status::latest_tag(&staging, WEB_BUILD, &test_source()).as_deref(),
            Some(WEB_TAG)
        );
        // A pointer that moved past the recorded tag — or a ledger with none — proceeds.
        for known in [Some("v0.9.0"), None] {
            let (outcome, heads) = acquire_web(&staging, known, || redirect_to(WEB_TAG));
            assert_eq!(heads.len(), 1);
            assert!(
                matches!(outcome, Ok(Acquisition::Proceed(_))),
                "{known:?}: {outcome:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    #[test]
    fn an_unchanged_web_head_refetches_a_signed_release_after_its_stage_is_lost() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("web-lost-stage");
        let source = test_source();
        let mut channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
        // The generic transport fixture omits commit identity. This publication
        // test needs one, covered by the same real detached signature.
        channel
            .appcast
            .extend_from_slice(b"commit = \"0123456789abcdef0123456789abcdef01234567\"\n");
        let signer = Ed25519KeyPair::from_seed_unchecked(&M3_SEED_FIXTURE).unwrap();
        channel.appcast_sig = signer.sign(&channel.appcast).as_ref().to_vec();
        let masters = [channel.master_pub.as_str()];
        crate::status::clear_check_note();
        let (outcome, _) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
        let Ok(Acquisition::Proceed(candidate)) = outcome else {
            panic!("the first check must acquire its authority: {outcome:?}");
        };
        let fetched = fetch_authoritative_release(
            Some(candidate),
            &mut |url, _| channel.serve(url),
            &channel.policy(&masters),
        );
        let (manifest, _, _) = fetched.selected.expect("the real signature chain accepts");
        let commit = manifest.commit.as_deref().expect("signed identity");
        write_ready(&staging, manifest.build_number, commit, &manifest.sha256);
        write_bundle_identity(&staging, manifest.build_number, commit);
        // The ledger the check writes after this stage, written directly rather than
        // through the process-global carry (`set_latest_tag` + `record`), which a
        // concurrently running apply-lane test may clear between the two calls.
        std::fs::write(
            &staging.status,
            format!(
                "schema = 1\noutcome = \"verified stage published\"\ncurrent_build = \
                 {WEB_BUILD}\nlatest_tag = {WEB_TAG:?}\nlatest_source = \"alabsystems/aterm\"\n\
                 latest_authorized_build = {WEB_BUILD}\nlatest_release_build = {}\n",
                manifest.build_number
            ),
        )
        .unwrap();
        let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
        assert_eq!(heads, vec![evergreen_url()]);
        assert!(matches!(outcome, Ok(Acquisition::UpToDate { .. })));

        // Publication invalidates a previous marker before committing the next
        // one. A failed commit or interrupted writer leaves exactly this state,
        // with no call to retire_published to clear the cached tag.
        std::fs::remove_file(&staging.ready).unwrap();
        assert!(Ready::read_publishable(&staging).is_none());
        let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
        assert_eq!(heads, vec![evergreen_url()]);
        let Ok(Acquisition::Proceed(candidate)) = outcome else {
            panic!("the unchanged head must retry the lost stage: {outcome:?}");
        };
        let mut gets = Vec::new();
        let recovered = fetch_authoritative_release(
            Some(candidate),
            &mut |url, _| {
                gets.push(url.to_string());
                channel.serve(url)
            },
            &channel.policy(&masters),
        );
        assert_eq!(
            recovered.selected.unwrap().0.build_number,
            manifest.build_number
        );
        assert_eq!(
            gets.len(),
            4,
            "recovery repeats the complete signature chain"
        );
        assert!(gets.iter().all(|url| url.starts_with(&tag_prefix(WEB_TAG))));

        // Historical negative control: trusting the bare tag still takes the
        // shortcut and would never reach those four verification fetches.
        assert!(matches!(
            resolve_web_head(&staging, WEB_BUILD, &source, Some(WEB_TAG), &mut |_| {
                redirect_to(WEB_TAG)
            },),
            Ok(WebHead::Unchanged { .. })
        ));
        crate::status::clear_check_note();
        let _ = std::fs::remove_dir_all(staging.root);
    }

    /// Every way the pointer can fail ends the check WITHOUT an API request: a 404 is
    /// the loud standing state (announced, stranded), a 429/5xx is a deferral (no
    /// health entry), a refused redirect or a transport failure is the historical
    /// `network`-class failure — and a refusal is never interpreted, so nothing is
    /// fetched from wherever it pointed.
    #[test]
    fn every_web_lane_failure_path_ends_without_an_api_request() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("failures");
        crate::status::clear_check_note();

        // 404: stranded, and the ledger says why.
        crate::unreadable::clear();
        let (outcome, heads) = acquire_web(&staging, None, || {
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        });
        assert_eq!(heads.len(), 1);
        assert!(matches!(outcome, Ok(Acquisition::Ended)), "{outcome:?}");
        assert!(crate::unreadable::is_stranded());
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(text.contains("cannot read its release channel"), "{text}");
        assert!(text.contains("HTTP 404"), "{text}");
        assert!(
            !staging.health().exists(),
            "a standing state is not a fault"
        );
        crate::unreadable::clear();

        // 429 / 5xx: deferred, no health entry, the latch set for the loop.
        for code in [429u16, 503] {
            let (outcome, heads) = acquire_web(&staging, None, || {
                Ok(HeadAnswer {
                    code,
                    location: None,
                })
            });
            assert_eq!(heads.len(), 1);
            assert!(
                matches!(outcome, Ok(Acquisition::Ended)),
                "{code}: {outcome:?}"
            );
            assert!(rate_limited(), "{code}: the loop lengthens its wait");
            let text = std::fs::read_to_string(&staging.status).unwrap();
            assert!(text.contains("deferred"), "{code}: {text}");
            assert!(!staging.health().exists(), "{code}: weather is not a fault");
        }

        // A refused redirect (another repository, the same asset name) and a transport
        // failure: `network`-class, and NOTHING is fetched from the refused location.
        let elsewhere =
            "https://github.com/someone/aterm/releases/download/v0.10.0/aterm-appcast.toml";
        for (label, answer) in [
            (
                "refused",
                Ok(HeadAnswer {
                    code: 302,
                    location: Some(elsewhere.to_string()),
                }),
            ),
            (
                "transport",
                Err(HttpError::Transport("curl HEAD x failed".into())),
            ),
            (
                "unexpected",
                Ok(HeadAnswer {
                    code: 200,
                    location: None,
                }),
            ),
        ] {
            let before = crate::health::Health::read(&staging.health()).network_failures;
            let (outcome, heads) = acquire_web(&staging, None, || answer.clone());
            assert_eq!(heads.len(), 1, "{label}");
            let Err(error) = outcome else {
                panic!("{label}: expected a failure, got {outcome:?}");
            };
            if label == "refused" {
                assert!(error.contains("refuses to follow"), "{error}");
                assert!(
                    !error.contains("someone/aterm"),
                    "a refusal never echoes the server's string: {error}"
                );
            }
            assert_eq!(
                crate::health::Health::read(&staging.health()).network_failures,
                before + 1,
                "{label}: booked as network"
            );
        }
        note_readable(&test_source());
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// Invariant (d): the pointer alone can never choose what is installed. The tag it
    /// names must equal `v` + the signed manifest's `version` AND the tag inside the
    /// signed manifest's `url`; a manifest that fails either bind is REJECTED, and the
    /// manifest's URL is never the one downloaded.
    #[test]
    fn the_pointers_tag_the_manifest_version_and_the_manifest_url_must_agree() {
        let source = test_source();
        let derived = tag_url(WEB_TAG, "aterm-0.10.0.dmg");
        let manifest = |url: Option<&str>| {
            let mut text = String::from_utf8(manifest_bytes("0.10.0", 10, 0)).unwrap();
            if let Some(url) = url {
                text.push_str(&format!("url = {url:?}\n"));
            }
            Manifest::parse(&text).unwrap()
        };
        web_container_url_agrees(&source, WEB_TAG, &manifest(Some(&derived))).unwrap();
        for wrong in [
            // The same appcast copied onto another tag.
            Some(tag_url("v0.9.0", "aterm-0.10.0.dmg")),
            // Another repository's copy of the container.
            Some(
                "https://github.com/someone/aterm/releases/download/v0.10.0/aterm-0.10.0.dmg"
                    .into(),
            ),
            // The evergreen alias, which would let a moving pointer choose.
            Some("https://github.com/alabsystems/aterm/releases/latest/download/aterm.dmg".into()),
            // No URL at all: the web lane requires the bind.
            None,
        ] {
            let error = web_container_url_agrees(&source, WEB_TAG, &manifest(wrong.as_deref()))
                .expect_err("must refuse");
            assert!(
                error.contains("refusing") || error.contains("requires"),
                "{wrong:?}: {error}"
            );
        }

        // The version half, on the real fetch path: a pointer at v0.11.0 whose appcast
        // says 0.10.0 is refused as a manifest defect, never staged.
        let channel = web_channel(Some(&derived));
        let masters = [channel.master_pub.as_str()];
        let candidate = web_release(&source, "v0.11.0").unwrap();
        let mut gets = Vec::new();
        let mut download = |url: &str, _max: u64| {
            gets.push(url.to_string());
            // Serve WEB_TAG's bytes under v0.11.0's derived names.
            let name = url.rsplit('/').next().unwrap().to_string();
            channel.serve(&tag_url(WEB_TAG, &name))
        };
        let fetched =
            fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
        assert!(fetched.selected.is_none());
        assert!(
            fetched.manifest_rejected,
            "version 0.10.0 does not bind to v0.11.0"
        );
        assert!(
            gets.iter().all(|u| u.starts_with(&tag_prefix("v0.11.0"))),
            "every GET was addressed by the pointer's tag: {gets:?}"
        );
    }

    /// A pointer that moves DURING a check cannot mix two releases: the HEAD is asked
    /// exactly once, and every GET is addressed by the tag that one answer named.
    #[test]
    fn a_pointer_that_moves_mid_check_cannot_mix_two_releases() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("moving");
        let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
        let masters = [channel.master_pub.as_str()];
        let mut calls = 0;
        let (outcome, heads) = acquire_web(&staging, None, || {
            calls += 1;
            redirect_to(if calls == 1 { WEB_TAG } else { "v0.11.0" })
        });
        assert_eq!(heads.len(), 1, "the pointer is consulted once");
        let Ok(Acquisition::Proceed(candidate)) = outcome else {
            panic!("{outcome:?}");
        };
        let mut gets = Vec::new();
        let mut download = |url: &str, _max: u64| {
            gets.push(url.to_string());
            channel.serve(url)
        };
        let fetched =
            fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
        assert!(fetched.selected.is_some());
        assert!(
            gets.iter().all(|u| u.starts_with(&tag_prefix(WEB_TAG))),
            "{gets:?}"
        );
        assert!(gets.iter().all(|u| !u.contains("v0.11.0")), "{gets:?}");
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// Invariant (c) on the web lane: a roster that cannot be FETCHED refuses the
    /// release as a transport failure — there is nothing else a client could accept it on.
    #[test]
    fn a_roster_that_cannot_be_fetched_on_the_web_lane_refuses_as_transport() {
        let source = test_source();
        let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
        let masters = [channel.master_pub.as_str()];
        let candidate = web_release(&source, WEB_TAG).unwrap();
        let mut gets = Vec::new();
        let mut download = |url: &str, _max: u64| {
            gets.push(url.to_string());
            if url.ends_with(aterm_update_core::roster::ROSTER_ASSET) {
                return Err("HTTP 404: the release does not carry this asset".to_string());
            }
            channel.serve(url)
        };
        let fetched =
            fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
        assert!(fetched.selected.is_none(), "refused");
        assert!(
            fetched.appcast_fetch_error,
            "transport, not a manifest verdict"
        );
        assert!(!fetched.manifest_rejected);
        assert!(!fetched.asset_fetch_rate_limited);
        assert!(fetched.attribution.is_none());
        assert!(
            gets.iter().all(|u| !aterm_update_core::cdn::is_api_host(u)),
            "{gets:?}"
        );
        // …and a web-host 429 on the same leg is the deferrable weather, still refused.
        let mut download = |url: &str, _max: u64| {
            if url.ends_with(aterm_update_core::roster::ROSTER_ASSET) {
                return Err("rate limited (HTTP 429) fetching an asset".to_string());
            }
            channel.serve(url)
        };
        let candidate = web_release(&source, WEB_TAG).unwrap();
        let fetched =
            fetch_authoritative_release(Some(candidate), &mut download, &channel.policy(&masters));
        assert!(fetched.selected.is_none() && fetched.appcast_fetch_error);
        assert!(fetched.asset_fetch_rate_limited);
    }

    /// THE STEADY-STATE SHORTCUT IS BOUND TO THIS MACHINE'S DECISION. A ledger naming
    /// the pointer's tag is "up to date" only when it was written by THIS build against
    /// THIS source; recorded by another build (a manual downgrade, an applied stage) or
    /// against another repository (a repointed updater), the release is re-judged —
    /// one HEAD and the tag-specific GETs, still zero API. And a RETIRED stage clears
    /// the tag, so the next check re-fetches and re-stages rather than answering "up to
    /// date" on a build that is gone.
    #[test]
    fn the_shortcut_is_bound_to_the_recording_build_and_source_and_cleared_by_retirement() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("shortcut-binding");
        let source = test_source();
        // The same tag, recorded by another build: re-judged.
        for other in [WEB_BUILD - 1, WEB_BUILD + 1] {
            write_ledger(&staging, WEB_TAG, other, "alabsystems/aterm");
            let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
            assert_eq!(heads, vec![evergreen_url()]);
            assert!(
                matches!(outcome, Ok(Acquisition::Proceed(_))),
                "build {other}: {outcome:?}"
            );
        }
        // The same tag, recorded against another repository: re-judged.
        write_ledger(&staging, WEB_TAG, WEB_BUILD, "example/mirror");
        let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
        assert_eq!(heads.len(), 1);
        assert!(
            matches!(outcome, Ok(Acquisition::Proceed(_))),
            "{outcome:?}"
        );
        // A ledger that recorded no source (an earlier build of this design): re-judged.
        std::fs::write(
            &staging.status,
            format!("schema = 1\ncurrent_build = {WEB_BUILD}\nlatest_tag = {WEB_TAG:?}\n"),
        )
        .unwrap();
        let (outcome, _) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
        assert!(
            matches!(outcome, Ok(Acquisition::Proceed(_))),
            "{outcome:?}"
        );
        // This build, this source: the steady state — written the way the check writes
        // it, through `set_latest_tag` + `record`.
        crate::status::clear_check_note();
        crate::status::set_latest_tag(WEB_TAG, &source, WEB_BUILD, WEB_BUILD);
        crate::status::record(&staging, WEB_BUILD, "staged 0.10.0 (build 10)");
        let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
        assert_eq!(heads.len(), 1);
        assert!(
            matches!(outcome, Ok(Acquisition::UpToDate { .. })),
            "{outcome:?}"
        );
        // RETIREMENT ("so the next check re-stages"): the unchanged pointer now
        // proceeds to a full re-fetch — and every request is still off the API.
        staging.retire_published();
        let (outcome, heads) = acquire_web_from(&staging, &source, || redirect_to(WEB_TAG));
        assert_eq!(heads, vec![evergreen_url()]);
        let Ok(Acquisition::Proceed(candidate)) = outcome else {
            panic!("a retired stage forces a re-fetch: {outcome:?}");
        };
        assert_eq!(candidate.release.tag_name, WEB_TAG);
        for asset in &candidate.release.assets {
            assert!(!aterm_update_core::cdn::is_api_host(&asset.url));
        }
        crate::status::clear_check_note();
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// THE URL BIND ON A REPOINTED SOURCE. The publisher signs `url` under the
    /// compiled-in channel's slug, so an updater repointed at a mirror
    /// carrying the upstream-signed appcast must accept it — the bind is on the TAG and
    /// the DMG, which invariant (d) needs, not on the repository, which
    /// `parse_location` already scopes. Any other slug, tag or container is refused, and
    /// so is a manifest with no `url` at all.
    #[test]
    fn the_url_bind_accepts_the_source_or_the_compiled_in_channel_and_nothing_else() {
        let mirror = Source {
            owner: "example".into(),
            repo: "mirror".into(),
        };
        let with_url = |url: Option<&str>| Manifest {
            url: url.map(str::to_string),
            ..candidate_manifest()
        };
        let dmg = candidate_manifest().dmg;
        let tag = "v0.6.0";
        let upstream = tag_url(tag, &dmg);
        web_container_url_agrees(&mirror, tag, &with_url(Some(&upstream)))
            .expect("the upstream-signed appcast is accepted on a mirror");
        web_container_url_agrees(&test_source(), tag, &with_url(Some(&upstream)))
            .expect("…and on the compiled-in channel");
        let own =
            aterm_update_core::cdn::release_download_url("example", "mirror", tag, &dmg).unwrap();
        web_container_url_agrees(&mirror, tag, &with_url(Some(&own)))
            .expect("a mirror's own re-signed appcast is accepted too");
        let cased =
            aterm_update_core::cdn::release_download_url("Example", "MIRROR", tag, &dmg).unwrap();
        web_container_url_agrees(&mirror, tag, &with_url(Some(&cased)))
            .expect("slugs compare case-insensitively, as GitHub does");
        for (why, bad) in [
            (
                "another repository",
                aterm_update_core::cdn::release_download_url("someone", "else", tag, &dmg).unwrap(),
            ),
            ("another tag", tag_url("v0.7.0", &dmg)),
            ("another container", tag_url(tag, "aterm-0.7.0.dmg")),
            (
                "another host",
                format!("https://evil.example/alabsystems/aterm/releases/download/{tag}/{dmg}"),
            ),
            (
                "the evergreen URL",
                aterm_update_core::pointer::latest_download_url("alabsystems", "aterm", &dmg)
                    .unwrap(),
            ),
            ("a query", format!("{upstream}?x=1")),
            ("an extra segment", format!("{upstream}/x")),
        ] {
            let error =
                web_container_url_agrees(&mirror, tag, &with_url(Some(&bad))).expect_err(why);
            assert!(error.contains("refusing"), "{why}: {error}");
        }
        assert!(
            web_container_url_agrees(&mirror, tag, &with_url(None))
                .unwrap_err()
                .contains("no container `url`")
        );
    }

    /// A FAILED CHECK IS NOT A COMPLETED ONE (2026-09-15). The receipt is what the
    /// machine-wide checker gate reads to skip an interval, and what `checked_at`
    /// reports as the last time this channel was asked. A check that ended in a
    /// transport error asked nobody: stamping it made the failing process re-read
    /// its own stamp and log "another aterm process completed this interval's
    /// update check" (nineteen times in forty minutes on 2026-09-13, one process,
    /// one DNS failure).
    #[test]
    fn only_a_check_that_reached_the_channel_leaves_a_receipt() {
        assert!(
            check_leaves_a_receipt(&Ok(None), false),
            "up to date is a completed check and must dedup the siblings"
        );
        assert!(
            check_leaves_a_receipt(&Ok(Some("0.86.0".into())), false),
            "a staged build is a completed check"
        );
        assert!(
            !check_leaves_a_receipt(
                &Err("dns error: nodename nor servname provided".into()),
                false
            ),
            "a resolver failure reached no channel: it must not stamp the receipt \
             every checker on this machine reads as this interval's completed check"
        );
        assert!(
            check_leaves_a_receipt(&Err("the release host closed the connection".into()), true),
            "a recorded deferral's machine-wide retreat is kept even when a later leg \
             failed"
        );
    }

    /// THE PUBLISHED v0.90.0 RELEASE VERIFIES UNDER THE COMMITTED PAPER MASTER ALONE.
    ///
    /// These are the exact bytes the channel serves for v0.90.0 (appcast, its signature,
    /// the machine roster and its master signature — `tests/fixtures/v0.90.0`), signed by
    /// roster machine m3, whose key was never in any compiled-in keyset. The client below
    /// is THIS tree's: `pins::PAPER_MASTER_PUBKEYS` and nothing else. So a current-format
    /// release verifies with K1 retired, which is what every client from v0.21.0 on has
    /// done all along. Negative controls: one flipped appcast byte, or a master this
    /// build does not pin, refuses.
    #[test]
    fn a_published_release_verifies_under_the_committed_master_alone() {
        const APPCAST: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-appcast.toml");
        const APPCAST_SIG: &[u8] =
            include_bytes!("../tests/fixtures/v0.90.0/aterm-appcast.toml.sig");
        const ROSTER: &[u8] = include_bytes!("../tests/fixtures/v0.90.0/aterm-machines.toml");
        const ROSTER_SIG: &[u8] =
            include_bytes!("../tests/fixtures/v0.90.0/aterm-machines.toml.sig");
        // 2026-09-21T20:00:00Z, just after the release; the roster is valid far past it.
        const PUBLISHED_AT: i64 = 1_790_020_800;
        let tag = "v0.90.0";
        let serve = |appcast: &'static [u8]| {
            move |url: &str, _max: u64| -> Result<Vec<u8>, String> {
                let name = url.rsplit('/').next().unwrap_or_default();
                match name {
                    APPCAST_ASSET => Ok(appcast.to_vec()),
                    APPCAST_SIG_ASSET => Ok(APPCAST_SIG.to_vec()),
                    "aterm-machines.toml" => Ok(ROSTER.to_vec()),
                    "aterm-machines.toml.sig" => Ok(ROSTER_SIG.to_vec()),
                    other => Err(format!("HTTP 404: no such asset {other}")),
                }
            }
        };
        let policy = |masters: &'static [&'static str]| RosterPolicy {
            master_pubkeys: masters,
            floor_seq: 0,
            now_unix: PUBLISHED_AT,
            floor_refresh: None,
        };
        let fetched = fetch_authoritative_release(
            Some(web_release(&test_source(), tag).unwrap()),
            &mut serve(APPCAST),
            &policy(aterm_update_core::pins::PAPER_MASTER_PUBKEYS),
        );
        let (manifest, release, _) = fetched.selected.as_ref().unwrap_or_else(|| {
            panic!(
                "the published release must verify: {:?}",
                fetched.rejection_reason
            )
        });
        assert_eq!(manifest.version, "0.90.0");
        assert_eq!(manifest.build_number, 1_790_019_739);
        web_container_url_agrees(&test_source(), &release.tag_name, manifest)
            .expect("the signed url binds to the tag");
        let who = fetched.attribution.as_ref().expect("attributed");
        assert_eq!(who.machine_id, "m3");
        assert_eq!(fetched.observed_roster_seq, Some(4));

        // Negative control 1: one flipped byte of the appcast refuses.
        let mut tampered = APPCAST.to_vec();
        let last = tampered.len() - 2;
        tampered[last] ^= 0x01;
        let tampered: &'static [u8] = Vec::leak(tampered);
        let refused = fetch_authoritative_release(
            Some(web_release(&test_source(), tag).unwrap()),
            &mut serve(tampered),
            &policy(aterm_update_core::pins::PAPER_MASTER_PUBKEYS),
        );
        assert!(refused.selected.is_none() && refused.manifest_rejected);

        // Negative control 2: a master this build does not pin refuses the roster.
        let stranger = pub_b64(&OTHER_MASTER_FIXTURE);
        let stranger: &'static [&'static str] = Vec::leak(vec![&*String::leak(stranger)]);
        let refused = fetch_authoritative_release(
            Some(web_release(&test_source(), tag).unwrap()),
            &mut serve(APPCAST),
            &policy(stranger),
        );
        assert!(refused.selected.is_none() && refused.manifest_rejected);
    }

    /// A HEAD THAT IS NOT AN APP RELEASE is said, once per check, as a healthy outcome
    /// — and nothing looks past it. Both shapes: a tag this client does not install from
    /// (`OtherTag`, decided by the pointer alone), and a canonical tag whose appcast
    /// answers 404 (a source-only release). One HEAD, no API, no failure streak, no
    /// strand, and the tag is NOT recorded as authorized so the next check reads it
    /// again. Negative control: the same head WITH its appcast proceeds.
    #[test]
    fn a_head_that_is_not_an_app_release_ends_the_check_healthy_and_unauthorized() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("non-app-head");
        crate::status::clear_check_note();
        crate::unreadable::clear();
        let index_tag = "atpkg-index-44";
        let (outcome, heads) = acquire_web(&staging, None, || {
            Ok(HeadAnswer {
                code: 302,
                location: Some(tag_url(index_tag, APPCAST_ASSET)),
            })
        });
        assert_eq!(heads, vec![evergreen_url()], "one HEAD and nothing else");
        assert!(matches!(outcome, Ok(Acquisition::Ended)), "{outcome:?}");
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(
            text.contains("channel head atpkg-index-44 has no app manifest yet"),
            "{text}"
        );
        assert!(!text.contains("latest_tag"), "never authorized: {text}");
        let h = crate::health::Health::read(&staging.health());
        assert_eq!(h.acquisition_failures(), 0, "not a failure");
        assert!(!crate::unreadable::is_stranded(), "not a strand");

        // The source-only shape: the tag is an app tag, its appcast 404s.
        let fetched = fetch_authoritative_release(
            Some(web_release(&test_source(), WEB_TAG).unwrap()),
            &mut |url: &str, _max: u64| Err(format!("HTTP 404: no such asset {url}")),
            &RosterPolicy::INERT,
        );
        assert!(fetched.selected.is_none() && fetched.appcast_missing);
        record_head_without_app(&staging, WEB_BUILD, WEB_TAG);
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(
            text.contains("channel head v0.10.0 has no app manifest yet"),
            "{text}"
        );
        assert!(
            crate::status::latest_tag(&staging, WEB_BUILD, &test_source()).is_none(),
            "a head with no app manifest is never the authorized tag"
        );
        // Negative control: an app tag proceeds to acquisition.
        let (outcome, _) = acquire_web(&staging, None, || redirect_to(WEB_TAG));
        assert!(
            matches!(outcome, Ok(Acquisition::Proceed(_))),
            "{outcome:?}"
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    // -----------------------------------------------------------------------------
    // CHECK-CHANNEL AUDIT, 2026-09-14. Failing tests; see also
    // `crate::check_channel_audit_tests`.
    // -----------------------------------------------------------------------------

    /// THE COVERED-STAGE LINE ERASES THE STANDING APPLY VERDICT EVERY CHECK. On
    /// 2026-09-14 the apply lane failed the staged v0.85.0 six times over ~8 h and
    /// stood down for ~6 h between attempts; every 30 minutes in between the check
    /// lane found the stage already covering the candidate and rewrote `status.toml`'s
    /// one `outcome` line to "staged 0.85.0 (build …) — verified and ready to apply;
    /// release build … needs no download" — over the apply lane's "staged build did
    /// not apply: …". An operator reading the file (or the update screen, which
    /// renders it) in those hours saw a build that was "ready to apply" and nothing
    /// about the attempt that had just refused it. The check lane knows the apply
    /// lane's answer — `Health::last_apply_failure_target_build` names this very
    /// artifact — and its steady-state line must carry it rather than overwrite it.
    #[test]
    fn check_channel_audit_a_covered_stage_line_keeps_the_standing_apply_verdict() {
        let staging = Staging::scratch("covered-vs-apply");
        let manifest = candidate_manifest();
        let commit = manifest.commit.as_deref().unwrap();
        write_ready(&staging, manifest.build_number, commit, &manifest.sha256);
        write_bundle_identity(&staging, manifest.build_number, commit);
        assert!(
            publishable_stage_covers(&staging, &manifest),
            "precondition"
        );
        let running = 1;
        // The apply lane's terminal verdict on THIS artifact, exactly as
        // `crate::record_apply_failure` leaves both ledgers.
        let reason = "PreparationFailed: the installed bundle at /Applications/aterm.app \
                      cannot be the rollback source the swap installs";
        crate::health::Health::record_apply_failure(
            &staging.health(),
            running,
            manifest.build_number,
            reason,
        );
        crate::status::record(
            &staging,
            running,
            &format!("staged build did not apply: {reason}"),
        );
        // The next check, an interval later, finds the stage covering the candidate.
        record_covered_stage_status(&staging, running, &manifest);
        let text = std::fs::read_to_string(&staging.status).expect("status written");
        let outcome = text
            .parse::<aterm_toml::Value>()
            .ok()
            .and_then(|v| {
                v.get("outcome")
                    .and_then(aterm_toml::Value::as_str)
                    .map(str::to_string)
            })
            .expect("an outcome line");
        assert!(
            outcome.contains("did not apply") || outcome.contains("rollback source"),
            "the covered-stage line must carry the standing apply verdict on the very \
             build it calls ready, not erase it: {outcome}"
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }
}
