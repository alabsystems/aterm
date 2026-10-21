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
//! catalog to look past it, and nothing may: `latest` is the ONLY unmetered signal
//! that the publisher promoted a release. The download host serves a PRERELEASE's
//! assets exactly as a release's (measured: the same 302), and the publisher relies on
//! no client electing one — aterm's source release is a prerelease carrying its cut's
//! `aterm-appcast.toml`, and a cut retired after binding keeps it for good. An older
//! tag found past the head would also be judged under its OWN roster, never the head's
//! newer one, so a revocation published source-first would not reach the machines
//! behind it.
//!
//! A source-only head whose tag outranks the last one the ledger authorized for this
//! source is an APP BUILD IN FLIGHT ([`app_build_in_flight`]): the publication train
//! mints the release source-first and the app cut attaches its assets minutes later,
//! so the next checks come every `cadence::IN_FLIGHT_RETRY` (a minute) instead of a
//! whole interval, counted machine-wide on the check receipt ([`head_in_flight`]).
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

/// The channel head whose APP BUILD the check in progress found in flight
/// ([`app_build_in_flight`]) and, once the check receipt has counted it, how many
/// consecutive completed checks machine-wide have found it so. Set by
/// `check_and_stage_inner`, counted by [`check_and_stage`], read by the background
/// loop through [`head_in_flight`] to take the quick `cadence::IN_FLIGHT_RETRY` wait
/// instead of a whole interval. Cleared at every check's start.
static HEAD_IN_FLIGHT: std::sync::Mutex<Option<(String, u32)>> = std::sync::Mutex::new(None);

pub(crate) fn set_head_in_flight(value: Option<(String, u32)>) {
    *HEAD_IN_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
}

/// After a check: the head whose app build it found in flight, and how many
/// consecutive completed checks machine-wide (this one included) have found it so —
/// `None` when the last check did not, or recorded no receipt. The loop hands the
/// count to `Cadence::succeeded`.
#[must_use]
pub(crate) fn head_in_flight() -> Option<(String, u32)> {
    HEAD_IN_FLIGHT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Whether `head` — a channel head whose release carries no `aterm-appcast.toml` yet — is
/// an APP BUILD IN FLIGHT: a release tag this client installs from whose rank is above
/// `known`, the last tag the ledger authorized for this source, whichever build recorded it
/// (`status::authorized_tag`: every build on the machine gets the same answer, as the
/// machine-wide count needs), or any tag at all when none is. The publication train mints
/// the release source-first and the app cut attaches its assets minutes later (~19 minutes
/// on an ordinary promote). A head at or below `known` — GitHub's `latest` names the most
/// recently PUBLISHED release, which a re-published older tag can be — is no release this
/// client is waiting for, and neither is a tag it does not install from.
fn app_build_in_flight(head: &str, known: Option<&str>) -> bool {
    let Ok(TagKind::Candidate(rank)) = parse_numeric_tag(head) else {
        return false;
    };
    match known.map(parse_numeric_tag) {
        Some(Ok(TagKind::Candidate(known))) => rank > known,
        _ => true,
    }
}

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
/// ` · checks every 2 min`, and nothing about HOW it reads the channel: Settings
/// paints this sentence under "You're up to date.".
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
fn unreadable_explanation(source: &Source) -> String {
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
        "aterm cannot read its release channel github.com/{}/{}: it has no published \
         release, or the repository is private, renamed or gone. This machine stays on its \
         current version until the channel is fixed; if aterm moved, reinstall it from its \
         new location.{repointed}",
        source.owner, source.repo
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
/// reset to 0 (`VERSIONING.md`). `targo --unverified ship cut` derives it from
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
/// without loosening anything beside it. ARMED — as this tree is — the master-signed roster
/// is the SOLE authority over who may have signed the appcast.
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

/// The marker a refusal carries when it is THIS BUILD's trust anchor that cannot verify the
/// channel, rather than the channel that is broken: the pinned paper master cannot verify
/// the machine roster (a master rotation this build predates). That is permanent for the
/// build — the head is the one candidate and nothing falls back — so the persistent wording
/// must name the one remedy, a reinstall, instead of "fixed at the publisher". A forged
/// release lands on the same arms and gets the same advice; a reinstall from the
/// publisher's own site is right for it too.
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
    /// WHY the manifest was rejected, in the words the log got, so the ledger can
    /// tell a forged release from a revoked signer from a STRANDED client whose
    /// anchor predates a rotation and needs a reinstall ([`STALE_ANCHOR_KEY`]).
    rejection_reason: Option<String>,
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

impl AuthoritativeFetch {
    /// Refuse the candidate: WARN the reason and carry it to the ledger. Every
    /// rejection arm goes through here so the reason `health.toml` records is the
    /// one the log printed.
    fn refuse(&mut self, reason: String) {
        crate::warn(&reason);
        self.manifest_rejected = true;
        self.rejection_reason = Some(reason);
    }

    /// Refuse the selected release, after the chain verified it and before anything is
    /// staged, when its signed manifest does not bind it to the pointer's tag or its
    /// signer is in `floor`'s durable revocation set.
    fn refuse_unbound_or_revoked(
        &mut self,
        source: &Source,
        floor: &crate::manifest::Floor,
        web_tag: &str,
    ) {
        // THE URL CROSS-CHECK, after every signature has been verified and the version
        // bound, and before anything is staged: the signed manifest must name the very
        // container URL this client derived under the pointer's tag.
        if let Some((manifest, release, _)) = self.selected.as_ref()
            && let Err(error) = web_container_url_agrees(source, &release.tag_name, manifest)
        {
            let reason = format!("{error}; refusing authoritative {}", release.tag_name);
            self.selected = None;
            self.refuse(reason);
        }
        // THE DURABLE REVOCATION SET: a machine a roster this client
        // admitted once revoked is not trusted again, whatever a later roster lists — the
        // rule the Linux lane keeps (`revoked_machines`). Without it the floor would
        // ignore the machine's word while the check staged its build, and every surface
        // (which reads the same set) would then hide that stage and re-download it forever.
        if self.selected.is_some()
            && let Some(who) = &self.attribution
            && floor.revokes(Some(&who.machine_id))
        {
            let reason = format!(
                "authoritative {} was signed by machine {}, which a machine roster this client \
                 admitted has revoked; refusing it",
                web_tag, who.machine_id
            );
            self.selected = None;
            self.refuse(reason);
        }
    }

    /// What this fetch tells the durable floor, whether or not it selected a release:
    /// the admitted roster's generation and revocations, and the selected release's
    /// `min_build` with WHO asked for it, the machine the roster chain attributed the
    /// release to, so a revocation of that machine takes it back.
    fn floor_observation(&self) -> crate::manifest::FloorObservation<'_> {
        crate::manifest::FloorObservation {
            min_build: self
                .selected
                .as_ref()
                .and_then(|(manifest, _, _)| manifest.min_build)
                .unwrap_or(0),
            high_water: 0,
            roster_seq: self.observed_roster_seq.unwrap_or(0),
            machine: self.attribution.as_ref().map(|who| who.machine_id.as_str()),
            revoked: &self.observed_revocations,
        }
    }
}

/// The length of a detached Ed25519 signature, the only shape the roster and appcast
/// signature assets can have.
const ED25519_SIGNATURE_LEN: usize = 64;

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
    /// An asset could not be FETCHED, or what came back is not shaped like the asset at
    /// all (a signature that is not [`ED25519_SIGNATURE_LEN`] bytes). Nothing has been
    /// judged; retrying may work.
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
/// [`AuthoritativeFetch::observed_roster_seq`]). A malformed appcast signature is
/// judged after it too, so it cannot hide the observation.
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
    // A detached Ed25519 signature is exactly 64 bytes. A body of any other length is
    // not a signature that failed: it is something else answering the asset GET with a
    // 2xx (a TLS-inspecting proxy's block page, a captive portal), which `curl -f` calls
    // success. Nothing has been judged, so it is the transport arm — still a refusal,
    // and a retry or another network can clear it. Booked as `Refused` it read as a
    // stale trust anchor and told the person to reinstall, which changes nothing behind
    // the same proxy. A 64-byte signature that does not verify stays a verdict.
    // Each is judged where it is used: the roster's here, before the roster is verified;
    // the appcast's only once the roster's observation below has landed, so a malformed
    // appcast signature cannot suppress the floor ratchet or the revocations.
    let signature_shaped = |asset: &str, sig: &[u8]| {
        if sig.len() == ED25519_SIGNATURE_LEN {
            Ok(())
        } else {
            Err(Transport(format!(
                "fetch {asset}: the download is {} bytes, not a {ED25519_SIGNATURE_LEN}-byte \
                 signature (something other than the release host answered)",
                sig.len()
            )))
        }
    };
    signature_shaped(ROSTER_SIG_ASSET, &roster_sig)?;

    // (3)(4) Verify under the paper master, then parse — in that order, by construction.
    let verified =
        verify_roster(policy.master_pubkeys, roster_bytes, &roster_sig).map_err(|e| {
            // The stranded-client arm of the armed tier: a master rotation this build
            // predates (or a roster that is not the publisher's). The build cannot
            // recover by retrying; the wording names the reinstall.
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

    // (6) Deny-list before crypto, then the artifact signature, once it is one.
    signature_shaped("appcast signature", &appcast_sig)?;
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
                // once per release per process, at DEBUG.
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

/// Whether the roster generation that authorized this check's release has been
/// SUPERSEDED by the durable floor while the check was in flight.
///
/// `observed` is the sequence the chain admitted for this release — `None`, which can never
/// be superseded, whenever no master-verified roster reached `admit`: the UNARMED tier (a
/// fork with no pinned master), or an armed tier whose roster was unfetchable or refused.
/// This tree is armed, so `None` here is the second case, not the first. `floor_now` is a
/// FRESH read of the durable floor. This run's own ratchet write makes `floor_now >=
/// observed` in the quiescent case, so a strict `<` fires only when a CONCURRENT instance
/// recorded a newer generation — at which point staging an artifact authorized under the
/// older generation would act on authority this client already knows is withdrawn. The
/// refusal is transient: the next check re-runs under the advanced floor.
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
/// apply lane's standing answer about that very build, so a check never paints
/// "ready" over the apply lane's "did not apply: …". The ledger knows both facts;
/// the line says both.
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
/// separation is expressed: stage failures of a re-published artifact (same build,
/// new digest, which [`publishable_stage_covers`] does not cover) must not keep the
/// earlier, verified bundle on disk from applying.
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
    /// The window is a MOMENT's: staging could not finish, which
    /// says nothing about the bytes, and the line must not say it "failed".
    moment: bool,
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
        } else if self.moment {
            format!(
                "skipping re-stage of build {candidate_build} for another {}m (staging could \
                 not finish {} time(s) in a row; nothing is known against the build, and it is \
                 retried automatically, or at once when you check for updates)",
                self.retry_in_secs.div_ceil(60),
                self.attempts
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
/// A PERSON'S CHECK PASSES A MOMENT'S MEMO. A moment —
/// a full disk, a helper past its deadline — says nothing about the bytes, and the
/// memo throttles only the schedule's re-downloads while it keeps coming back. A
/// person who freed the space, or changed the network, and asked for a check IS
/// the news that it may have passed; holding them to the moment's schedule kept
/// the release from them for up to four hours after the cause was gone, while the
/// refusal told them to free some space and check. A verdict's memo and a
/// quarantine still hold against them: those are findings about the build, and a
/// person asking changes nothing about it.
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
    asked: Asked,
) -> Option<StageBackoff> {
    // The quarantine ledger first: a verdict no later stage event can overwrite
    // — `failed.toml` holds one artifact at a time.
    if crate::manifest::artifact_quarantined(staging, manifest.build_number, &manifest.sha256) {
        return Some(StageBackoff {
            retry_in_secs: 0,
            attempts: 1,
            quarantined: true,
            moment: false,
            applicable: Ready::read_publishable(staging)
                .filter(|ready| ready.build_number > current_build),
        });
    }
    let memo = crate::manifest::FailedMark::read(&staging.failed())?;
    if !memo.suppresses(manifest.build_number, &manifest.sha256, now)
        || (asked == Asked::Person && memo.moment && !memo.is_quarantine())
    {
        return None;
    }
    Some(StageBackoff {
        retry_in_secs: memo.retry_in_secs(now),
        attempts: memo.attempts.max(1),
        quarantined: memo.is_quarantine(),
        moment: memo.moment,
        applicable: Ready::read_publishable(staging)
            .filter(|ready| ready.build_number > current_build),
    })
}

/// The words a stage-lock refusal carries when its holder showed no sign of life
/// ([`take_stage_lock`]); the persistent notice reads its cause by them
/// ([`is_stopped_stage_holder_refusal`]). They are written into the ledger every
/// aterm on this Mac shares, and older builds read the cause by these same words,
/// so they never change.
pub(crate) const STOPPED_STAGE_HOLDER: &str =
    "holds the update stage lock and has shown no sign of life";

/// Whether a ledger error is [`take_stage_lock`]'s stopped-holder refusal.
pub(crate) fn is_stopped_stage_holder_refusal(error: &str) -> bool {
    error.contains(STOPPED_STAGE_HOLDER)
}

/// This check's hold on `stage.lock`, with the beat that tells a waiting sibling
/// the holder is alive. The beat is declared FIRST so it stops before the lock is
/// let go, and never stamps this pid into a lock another process then holds.
struct StageHold {
    _beat: crate::HolderBeat,
    _lock: aterm_update_core::FileLock,
}

/// Take the machine-wide stage lock (download → extract → publish), waiting at
/// most `wait`.
///
/// BOUNDED: a blocking wait would park the checker behind any holder.
///
/// AND THE HOLDER IS JUDGED. For an aterm STOPPED mid-download (a `SIGSTOP`, a
/// debugger), or one whose check HUNG there (a download stalled on a dead read, which
/// the window's watchdog retires but cannot unwind), "another aterm staging" never
/// comes: it keeps the lock and every sibling's check times out here cycle after
/// cycle.
///
/// The holder stamps the lock while its check is ALIVE — the rule
/// `checker.lock`'s holder keeps ([`crate::stage_holder_alive`]: its checker
/// generation current and its heartbeat fresh), not merely while the process
/// runs, so a hung holder looks as stuck as a stopped one. A waiter that runs out
/// of time reads the stamp: fresh is a sibling staging (the ordinary stand-down);
/// stale is a holder that did nothing for half the wait.
///
/// ONE STALE READING IS NOT A VERDICT (the mixed-version pairing). A holder of a
/// build older than this never stamps, so while it stages a large container on a
/// slow link its lock reads stale — with the mtime and pid of whichever newer
/// build stamped last, possibly this very process. So the first stale reading
/// stands down unbooked, and only a SECOND consecutive one — the very next check
/// that needs the lock — that finds the same stamp (nothing stamped in between —
/// [`StaleStageStamps`]) is booked as the
/// TYPED `pipeline` failure ([`STOPPED_STAGE_HOLDER`]) that `update status` and,
/// past `PERSISTENT_AFTER`, the persistent notice say. A pid the lock names is
/// never this process's own: an older holder leaves the last writer's there.
/// The lock cannot be taken from a live process, stopped or not; saying so is
/// what is left to do.
fn take_stage_lock(staging: &Staging, wait: std::time::Duration) -> Result<StageHold, String> {
    StaleStageStamps::note_stage_lock_reached();
    match aterm_update_core::FileLock::acquire_within(&staging.stage_lock, wait) {
        Ok(lock) => {
            StaleStageStamps::forget(&staging.stage_lock);
            crate::note_checker_lock_holder(&staging.stage_lock);
            let alive = crate::stage_holder_alive();
            Ok(StageHold {
                _beat: crate::HolderBeat::start_with(
                    staging.stage_lock.clone(),
                    wait / 4,
                    move || alive(),
                ),
                _lock: lock,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            let fresh = crate::holder_fresh(wait);
            if crate::checker_holder_progressing(&staging.stage_lock, fresh) {
                StaleStageStamps::forget(&staging.stage_lock);
                return Err(format!(
                    "stage lock: another aterm process has been staging an update for more \
                     than {} s; this check stands down and the next one reads what it staged",
                    wait.as_secs()
                ));
            }
            if !StaleStageStamps::seen_again(&staging.stage_lock) {
                return Err(format!(
                    "another aterm has held the update stage lock for over {} s without a sign \
                     of life (an older aterm shows none while it stages); the next check looks \
                     again",
                    wait.as_secs()
                ));
            }
            // No count of checks: every later check that finds the same stamp books
            // this again, so "across two checks" was true of the first only.
            let holder = crate::checker_lock_holder(&staging.stage_lock)
                .filter(|pid| *pid != std::process::id())
                .map_or_else(
                    || "another aterm".to_string(),
                    |pid| format!("another aterm (the lock names pid {pid})"),
                );
            let error = format!(
                "{holder} {STOPPED_STAGE_HOLDER} — it may be stopped or hung; no aterm on this \
                 Mac can stage an update until it is resumed or quits"
            );
            crate::health::Health::record_failure(&staging.health(), "pipeline", &error);
            Err(error)
        }
        Err(e) => Err(format!("stage lock: {e}")),
    }
}

/// The stage-lock stamps this process last found stale, per lock path: the memory
/// behind [`take_stage_lock`]'s two-reading rule. In process only — a relaunch
/// starts over, which costs one more unbooked reading, never a false one.
///
/// TWO READINGS MEANS TWO CONSECUTIVE ONES. An older build's slow staging never stamps, so
/// a first stale reading kept across checks that never needed the lock could pair, days
/// later, with a reading of the same mtime and book a false stopped holder. A check that
/// ends without reaching the stage lock ends the run ([`Self::check_ended`]), so the second
/// reading must come from the very next check that needs the lock.
struct StaleStageStamps;

/// A stale reading: `(lock path, the stamp read there, the run it belongs to)`.
type StaleReading = (std::path::PathBuf, std::time::SystemTime, u64);

/// [`StaleStageStamps`]' table.
static STALE_STAGE_STAMPS: std::sync::Mutex<Vec<StaleReading>> = std::sync::Mutex::new(Vec::new());

/// Per lock path, the run of consecutive stage-lock checks a reading belongs to:
/// advanced by every check that ends without reaching that stage lock.
static STAGE_LOCK_RUNS: std::sync::Mutex<Vec<(std::path::PathBuf, u64)>> =
    std::sync::Mutex::new(Vec::new());

thread_local! {
    /// Whether the check running on this thread has reached [`take_stage_lock`].
    static REACHED_STAGE_LOCK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl StaleStageStamps {
    /// Record the stale stamp `path` carries now, and say whether the previous
    /// stale reading of `path` found this SAME stamp in the SAME run of checks —
    /// nothing stamped in between, and no check in between that did without the
    /// lock. An unreadable stamp is never a second reading.
    fn seen_again(path: &std::path::Path) -> bool {
        let Ok(stamp) = std::fs::symlink_metadata(path).and_then(|meta| meta.modified()) else {
            Self::forget(path);
            return false;
        };
        let run = Self::run(path);
        let mut seen = STALE_STAGE_STAMPS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = seen.iter_mut().find(|(p, _, _)| p == path) {
            let again = entry.1 == stamp && entry.2 == run;
            entry.1 = stamp;
            entry.2 = run;
            return again;
        }
        seen.push((path.to_path_buf(), stamp, run));
        false
    }

    /// The holder of `path` showed life (or the lock was taken): the next stale
    /// reading starts over.
    fn forget(path: &std::path::Path) {
        STALE_STAGE_STAMPS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(p, _, _)| p != path);
    }

    /// A check on this thread is starting.
    fn check_started() {
        REACHED_STAGE_LOCK.with(|reached| reached.set(false));
    }

    /// The check on this thread reached the stage lock.
    fn note_stage_lock_reached() {
        REACHED_STAGE_LOCK.with(|reached| reached.set(true));
    }

    /// A check on this thread has ended: one that never reached the stage lock at
    /// `path` (the stage was covered, the release held, nothing new) ends the run, so
    /// no stale reading before it pairs with one after it.
    fn check_ended(path: &std::path::Path) {
        if REACHED_STAGE_LOCK.with(std::cell::Cell::get) {
            return;
        }
        let mut runs = STAGE_LOCK_RUNS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match runs.iter_mut().find(|(p, _)| p == path) {
            Some(entry) => entry.1 = entry.1.wrapping_add(1),
            None => runs.push((path.to_path_buf(), 1)),
        }
    }

    /// The run of checks at `path` a reading now belongs to.
    fn run(path: &std::path::Path) -> u64 {
        STAGE_LOCK_RUNS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(p, _)| p == path)
            .map_or(0, |(_, run)| *run)
    }
}

/// Retire the published stage of `build`, which the floor withdraws — a revoked
/// machine signed it, or the operator yanked it — the caller holds the apply lock
/// and has re-read that the stage is still `build`'s.
///
/// AND FORGET ITS PENDING CLOCK. The build is
/// withdrawn: its release can never be re-authorized by the revoked machine, so it
/// will never be applied, and a clock left running announced "build P has been
/// waiting on this machine … no apply attempt has been recorded" an hour later and
/// at every relaunch until a newer release shipped. Nothing on disk records the
/// revocation for the pending observation to read, but this code has the build in
/// hand; a clock another observation has re-keyed to another build is left alone
/// ([`crate::health::Health::forget_pending_update`]).
pub(crate) fn retire_revoked_stage(staging: &Staging, build: u64) {
    staging.retire_published();
    crate::health::Health::forget_pending_update(&staging.health(), build);
}

/// How [`book_stage_failure`] booked a stage that did not publish.
#[derive(Debug, PartialEq, Eq)]
enum StageBooked {
    /// HELD, not failed: the release was withdrawn (its signer revoked, a yank) or
    /// quarantined while this check downloaded it. The floor or the quarantine is its
    /// standing record, the next check refuses it before any download, and
    /// `status.toml` says why (`held: …`, the words carried here). A healthy end.
    Held(String),
    /// A MOMENT: staging could not finish. Booked on the moment schedule.
    Moment,
    /// A VERDICT on the bytes. Booked on the verdict schedule.
    Verdict,
}

/// Book a stage that did not publish after its container arrived — a digest that
/// could not be taken or did not match, an unpack, a verification, the publish —
/// or a container this machine could not store as it arrived
/// ([`book_download_failure`]), and say how it was booked. THE ONE BOOKING POINT
/// for everything past the download, so one rule decides moment from verdict for
/// all of it.
///
/// ONLY A VERDICT IS BOOKED AGAINST THE BUILD (the apply path's rule,
/// `install::stage_failure_is_a_verdict`). A
/// verification helper that ran past its deadline on a loaded machine, a helper
/// the kernel would not start just then, a copy past its launchd budget, an
/// `hdiutil attach` past its limit, the apply lock a sibling held through the
/// publish: each is a MOMENT ([`crate::is_passing_refusal`]) and says nothing about
/// the bytes, so it opens no verdict backoff against a healthy build.
///
/// BUT A MOMENT THAT NEVER PASSES IS BOUNDED, AND ESCALATES (L4, L5). A Gatekeeper
/// check a network filter always blocks would otherwise re-download the whole
/// container at every check of every aterm on the machine. So a moment is
/// memoized on the MOMENT schedule ([`crate::manifest::MOMENT_BACKOFF_SECS`]): the
/// first costs nothing — the next check tries again — and one that keeps coming
/// back widens to four hours at most, shared by every aterm on the Mac; a check a
/// person asks for passes it ([`stage_backoff`]). It also advances the `stage`
/// streak — the ledger's count of checks that could not make a stage — and past
/// `PERSISTENT_AFTER` (and the notice's thirty minutes) the persistent notice says
/// so, in words of its own: staging could not FINISH, never that the build will not
/// verify (`persistent_failure_notice` reads the passing key in the ledger's
/// error). The acquisition half of this check worked, which the ledger is told
/// first.
///
/// A stage refused because the artifact is QUARANTINED (a crash-loop revert
/// landed while this check was downloading), or WITHDRAWN (a sibling's
/// check recorded its signer's revocation or a yank), is no stage failure either:
/// it is [`StageBooked::Held`], and the check ends healthy.
fn book_stage_failure(
    staging: &Staging,
    current_build: u64,
    manifest: &Manifest,
    error: &str,
    now: u64,
) -> StageBooked {
    if crate::manifest::artifact_quarantined(staging, manifest.build_number, &manifest.sha256) {
        crate::health::Health::record_acquisition_success(&staging.health());
        let why = format!(
            "held: build {} is quarantined — it crash-looped on this machine and was \
             reverted; aterm will not install it",
            manifest.build_number
        );
        crate::status::record(staging, current_build, &why);
        return StageBooked::Held(why);
    }
    // WITHDRAWN WHILE IT DOWNLOADED: a sibling's check
    // recorded a revocation of its signer, or a yank, and the publish refused it. No
    // verdict on the bytes either — the floor is its standing record, and the next
    // check refuses the release before any download.
    if let Some(why) = crate::manifest::Floor::read(&staging.floor())
        .withdraws(manifest.build_number, manifest.machine_id.as_deref())
    {
        crate::health::Health::record_acquisition_success(&staging.health());
        let why = format!("held: {why}");
        crate::status::record(staging, current_build, &why);
        return StageBooked::Held(why);
    }
    if crate::is_passing_refusal(error) {
        let memo = crate::manifest::FailedMark::record_stage_moment(
            &staging.failed(),
            manifest.build_number,
            &manifest.sha256,
            now,
        );
        let next = match memo.retry_in_secs(now) {
            0 => "the next check tries again".to_string(),
            wait => format!(
                "it could not finish {} times in a row, so it is tried again in {} min, or \
                 at once when you check for updates",
                memo.attempts,
                wait.div_ceil(60)
            ),
        };
        crate::warn(&format!(
            "staging build {} could not finish just then: {error}; no verdict is booked \
             against it, and {next}",
            manifest.build_number
        ));
        crate::health::Health::record_acquisition_success(&staging.health());
        crate::health::Health::record_failure(&staging.health(), "stage", error);
        crate::status::record(
            staging,
            current_build,
            &format!(
                "deferred: staging build {} could not finish; {next}",
                manifest.build_number
            ),
        );
        return StageBooked::Moment;
    }
    crate::manifest::FailedMark::record_stage_failure(
        &staging.failed(),
        manifest.build_number,
        &manifest.sha256,
        now,
    );
    crate::health::Health::record_failure(&staging.health(), "stage", error);
    StageBooked::Verdict
}

/// End the check for a stage that did not publish with `error`: booked by
/// [`book_stage_failure`], a HELD release is a healthy end — `Ok(None)`, and a
/// download that put a row on a host's screen takes it down as withheld, never as
/// a failure that "tries again by itself" — and the rest is the
/// check's error.
fn end_failed_stage(
    staging: &Staging,
    current_build: u64,
    manifest: &Manifest,
    error: String,
) -> Result<Option<String>, String> {
    match book_stage_failure(
        staging,
        current_build,
        manifest,
        &error,
        crate::install::unix_now_secs(),
    ) {
        StageBooked::Held(why) => {
            if crate::progress::take_download_began() {
                crate::progress::report(crate::progress::Progress::Withheld { detail: why });
            }
            Ok(None)
        }
        StageBooked::Moment | StageBooked::Verdict => Err(error),
    }
}

/// The digest step: `None` when the container at `path` hashes (by `digest`) to
/// `expected`, else the check's end — the container reclaimed and the failure
/// booked at the one booking point ([`end_failed_stage`]): a mismatch is a verdict,
/// and a hash that did not come back is booked by what it was (a hash past its
/// deadline, or one whose thread the system would not start just then, answers
/// passing from [`container_sha256`]: a moment). `digest` is [`container_sha256`]
/// at [`CONTAINER_HASH_WAIT`] outside the tests.
fn check_container_digest(
    staging: &Staging,
    current_build: u64,
    manifest: &Manifest,
    path: &std::path::Path,
    container: &str,
    expected: &str,
    digest: impl FnOnce(&std::path::Path) -> Result<String, String>,
) -> Option<Result<Option<String>, String>> {
    let error = match digest(path) {
        Ok(got) if got.eq_ignore_ascii_case(expected) => return None,
        Ok(got) => format!("{container} sha256 mismatch: got {got}, manifest {expected}"),
        Err(e) => e,
    };
    let _ = std::fs::remove_file(path);
    Some(end_failed_stage(staging, current_build, manifest, error))
}

/// The bound on hashing the downloaded container ([`container_sha256`]). It reads
/// the whole file — well under a second for a release container, which the
/// download has just written, on any disk — so a hash still running after two
/// minutes is a read that is not coming back.
const CONTAINER_HASH_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// SHA-256 of the downloaded container, answered by `limit`: the checker thread holds
/// the check lane and `stage.lock`, so an unbounded read would block every aterm on
/// the Mac from staging. The hash runs in process, on the CPU's SHA-256 instructions
/// ([`aterm_update_core::sha256_file`]; a 47 MiB container in about 19 ms, where the
/// `shasum` it replaced took about 100), on a thread of its own: one still reading at
/// `limit` is left to finish on its own — a read wedged in the kernel, which no
/// signal would have ended in a helper either — and, like a thread the system would
/// not start just then, answers as a passing moment. A file that cannot be opened or
/// read is a verdict, as `shasum`'s exit was.
fn container_sha256(path: &std::path::Path, limit: std::time::Duration) -> Result<String, String> {
    let (answer, answered) = std::sync::mpsc::sync_channel(1);
    let owned = path.to_path_buf();
    let started = std::thread::Builder::new()
        .name("aterm-update-hash".into())
        .spawn(move || {
            let _ = answer.send(aterm_update_core::sha256_file(&owned));
        });
    if let Err(error) = started {
        return Err(crate::verify::mark_passing(format!(
            "could not start the container hash: {error}"
        )));
    }
    answered.recv_timeout(limit).unwrap_or_else(|_| {
        Err(crate::verify::mark_passing(format!(
            "the container hash did not finish within {}s; treating as a failure",
            limit.as_secs()
        )))
    })
}

/// Book a container download that failed with `error` (a rate limit is not one: it
/// defers), and answer how the check ends — its error, unless the release was held
/// while it downloaded ([`end_failed_stage`]).
///
/// A DOWNLOAD THIS MACHINE COULD NOT STORE IS NOT THE HOST'S (design law L4). curl's
/// write error onto a full volume
/// ([`aterm_update_core::download_error_is_local_write`]) is not a `pipeline` failure
/// but a passing moment of the stage lane ([`book_stage_failure`]): no verdict
/// against the release; the first costs nothing, a disk that stays full is
/// re-downloaded on the moment's schedule — which a check a person asks for passes
/// ([`stage_backoff`]), the action its words name
/// ([`crate::install::FREE_SPACE_REMEDY`]) — and it escalates in words that name it.
/// Every other failure is still the host's.
///
/// curl's write error is any local write or open failure (a permission, a read-only
/// volume, an I/O error), so the words say "is full" only when `free` — the
/// volume's free bytes, read before the partial download was removed — is under
/// [`crate::install::STAGE_FREE_SPACE_HEADROOM`]; otherwise they keep curl's cause.
fn book_download_failure(
    staging: &Staging,
    current_build: u64,
    manifest: &Manifest,
    container: &str,
    error: &str,
    free: Option<u64>,
) -> Result<Option<String>, String> {
    if aterm_update_core::download_error_is_local_write(error) {
        let room = free.map_or_else(String::new, |free| {
            format!(" ({} MiB free there)", free.div_ceil(1024 * 1024))
        });
        crate::warn(&format!(
            "{container} download could not be stored: {error}"
        ));
        let dir = staging.download.display();
        let full = free.is_some_and(|free| free < crate::install::STAGE_FREE_SPACE_HEADROOM);
        let error = crate::verify::mark_passing(if full {
            format!(
                "the disk holding {dir} is full{room}; {}",
                crate::install::FREE_SPACE_REMEDY
            )
        } else {
            // curl's own words, past its `curl: (N)` code.
            let cause = error
                .split_once("curl: (")
                .and_then(|(_, rest)| rest.split_once(") "))
                .map_or(error, |(_, words)| words.trim());
            format!("the update could not be written to {dir}{room}: {cause}")
        });
        return end_failed_stage(staging, current_build, manifest, error);
    }
    let error = format!("{container} download failed: {error}");
    let class = asset_failure_class();
    if class == "pipeline" {
        // The same note as the manifest leg: the download host did not serve it.
        crate::status::set_delivery_note(ASSET_BLOCKED_NOTE);
    }
    crate::health::Health::record_failure(&staging.health(), class, &error);
    Err(error)
}

/// The class a release-asset GET that failed after the channel answered books:
/// `pipeline` (the download host did not serve what the release names) — unless the
/// check began inside a wake's grace ([`crate::check_follows_wake`]). Then the
/// network is still coming up (Wi-Fi associating, a VPN or a portal), which is
/// `network`: weather, which neither starts a pipeline streak nor clears a standing
/// `manifest` or `stage` diagnosis, as a `pipeline` booking would.
fn asset_failure_class() -> &'static str {
    if crate::check_follows_wake() {
        "network"
    } else {
        "pipeline"
    }
}

/// The exact asset names a release carries for the updater. The roster chain looks
/// them up by name; the check DERIVES their URLs from them ([`web_release`]). The
/// appcast's is one constant with the dev build's look at the channel
/// ([`crate::dev_channel`]), so the two can never HEAD different names.
const APPCAST_ASSET: &str = crate::dev_channel::APPCAST_ASSET;
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
/// publisher's to point at an installable release, and an older tag's served appcast
/// is no proof it was ever promoted (a prerelease's is served too — see the module
/// docs).
fn record_head_without_app(staging: &Staging, current_build: u64, tag: &str) {
    crate::health::Health::record_success(&staging.health());
    let mut line =
        format!("channel head {tag} has no app manifest yet — nothing to install from it");
    if let Some(note) = unconfirmable_stage_note(
        staging,
        current_build,
        crate::install::unix_now_secs(),
        aterm_update_core::pins::roster_tier_armed(),
        "it installs once the channel head is an app release again",
    ) {
        line.push_str("; ");
        line.push_str(&note);
    }
    crate::status::record(staging, current_build, &line);
}

/// How long a stage waits under a roster this build's anchor cannot verify.
const STALE_ANCHOR_STAGE_WAIT: &str = "this build can never confirm it";

/// WHAT A CHECK THAT READ NO ROSTER SAYS ABOUT THE STAGE WAITING FOR ONE: `Some` when the
/// build staged here — publishable and newer than the running build — waits for its roster
/// confirmation at `now` (the swap's gate 4d, `Ready::roster_authorization_stale`), naming
/// the wait and `why` this check could not end it.
///
/// Gate 4d's own refusal says the stage "installs once the next update check
/// confirms it", which is true only of a check that can admit the channel head's
/// roster. Two channel states admit none, and the stage then waits as long as they
/// last: a head that is not an app release (an index cut or a source-only release at
/// the head: [`record_head_without_app`]), and a roster this build's trust anchor
/// cannot verify (a master rotation it predates: [`STALE_ANCHOR_KEY`]), where it
/// waits for good and only a reinstall moves the client (docs/RELEASE-KEYS.md,
/// *Rotating the master*). The status line names the state rather than leave the
/// update row promising a check that cannot come.
fn unconfirmable_stage_note(
    staging: &Staging,
    current_build: u64,
    now: u64,
    armed: bool,
    why: &str,
) -> Option<String> {
    let ready =
        Ready::read_publishable(staging).filter(|ready| ready.build_number > current_build)?;
    ready.roster_authorization_stale(now, armed)?;
    let since = ready.roster_confirmed_at.as_deref().map_or_else(
        || "never on this Mac".to_string(),
        |at| format!("last at {at}"),
    );
    Some(format!(
        "staged build {} is not installed: it waits to be confirmed against a current machine \
         roster ({since}), and {why}",
        ready.build_number
    ))
}

/// THE SOURCE-ONLY HEAD — the pointer named a canonical app tag whose appcast answers
/// 404 — said as a head with no app manifest yet ([`record_head_without_app`]), and,
/// when its tag outranks the last one the ledger authorized for this source, latched
/// as an APP BUILD IN FLIGHT ([`app_build_in_flight`]): its assets are minutes away, so
/// [`check_and_stage`] counts it on the receipt and the loop re-checks on the quick
/// ladder instead of a whole interval.
fn record_source_only_head(staging: &Staging, current_build: u64, source: &Source, tag: &str) {
    let known = crate::status::authorized_tag(staging, source);
    if app_build_in_flight(tag, known.as_deref()) {
        set_head_in_flight(Some((tag.to_string(), 0)));
    }
    record_head_without_app(staging, current_build, tag);
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
            crate::unreadable::announce(staging, current_build, &unreadable_explanation(source));
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
/// What is bound is the TAG and the DMG name, not the repository: the publisher signs `url`
/// under the compiled-in public channel's slug (`manifest_out.rs` `repo_slug` =
/// `[workspace.metadata.aterm] update_channel`, the slug `DEFAULT_OWNER`/`DEFAULT_REPO`
/// name), so an updater REPOINTED at a mirror that carries the
/// upstream-signed appcast would otherwise refuse every release as a `manifest`-class
/// failure blamed on a publisher who did nothing wrong. The repository half adds nothing to
/// (d): `pointer::parse_location` already scopes every GET after the HEAD to the source
/// repository. The URL's slug must be the source's or the compiled-in channel's
/// (GitHub-style case-insensitively); anything else is a manifest signed for a channel this
/// updater does not read.
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
    check_and_stage_for(current_build, source, Asked::Schedule)
}

/// Who asked for a check: the background schedule, or a PERSON (Check for Updates,
/// `aterm update check`, `aterm ctl update check` — every one is
/// [`crate::check_now`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    Schedule,
    Person,
}

/// [`check_and_stage`], asked for by `asked`. A person's check passes a MOMENT's
/// re-stage memo ([`stage_backoff`]); nothing else about the check differs.
pub(crate) fn check_and_stage_for(
    current_build: u64,
    source: &Source,
    asked: Asked,
) -> Result<Option<String>, String> {
    set_head_in_flight(None);
    if bundle::resolve().is_none() {
        return Ok(None);
    }
    RATE_LIMITED.store(false, Ordering::Relaxed);
    StaleStageStamps::check_started();
    let result = check_and_stage_inner(current_build, source, asked);
    if let Some(staging) = Staging::resolve() {
        StaleStageStamps::check_ended(&staging.stage_lock);
    }
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
    let found = head_in_flight().map(|(tag, _)| tag);
    set_head_in_flight(Staging::resolve().and_then(|staging| {
        stamp_receipt(
            &staging,
            current_build,
            source,
            &result,
            rate_limited(),
            found,
        )
    }));
    result
}

/// Stamp the check receipt for the check that just ended, when it owes one
/// ([`check_leaves_a_receipt`]), and answer the head in flight it counted: `found` —
/// the head whose app build the check found in flight — with the machine-wide count
/// of consecutive checks that have found it so, this one included. Only a completed
/// check that is not a deferral counts one: a host that asked us to slow down holds
/// to the deferral's own window, never the quick ladder. A failed check leaves the
/// receipt untouched, so it neither counts nor ends the run — the next completed
/// check that finds the same head carries the count on.
fn stamp_receipt(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    result: &Result<Option<String>, String>,
    deferred: bool,
    found: Option<String>,
) -> Option<(String, u32)> {
    if !check_leaves_a_receipt(result, deferred) {
        return None;
    }
    let found = found.filter(|_| result.is_ok() && !deferred);
    let checks =
        crate::check_receipt::record(staging, current_build, source, deferred, found.as_deref());
    found.filter(|_| checks > 0).map(|tag| (tag, checks))
}

/// Whether the check that just ended owes the shared check receipt a stamp.
///
/// ONLY A CHECK THAT REACHED THE CHANNEL LEAVES ONE. The receipt's
/// contract is its module doc's first line — "completed network checks have their
/// own receipt" — and every reader leans on it: `crate::checker_skip_for` skips
/// this interval's network check on it machine-wide, and `crate::last_check_at` /
/// `status.toml`'s `checked_at` advertise it as when this channel was last asked.
/// Stamped from a failing exit, a process would re-read its OWN failure stamp as
/// another process's completed check and advertise a last-check time for a check
/// that never reached GitHub. A failure is bounded by
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
        // THE STAGE'S ROSTER CONFIRMATION IS DUE: the head has not
        // moved, but the build waiting here was last confirmed against a freshly
        // fetched roster too long ago — the swap's gate 4d holds it past a day — so
        // the head is judged again, roster and all, and the floor's observation
        // confirms the stage or retires it (`observe_floor_and_withdraw_at`). A few
        // small GETs about once an hour while a newer stage waits (so the stamp a
        // quit leaves behind is never much older), and once after a launch that
        // found one stale.
        WebHead::Unchanged { tag }
            if stage_owes_roster_reconfirmation(
                staging,
                current_build,
                crate::install::unix_now_secs(),
                aterm_update_core::pins::roster_tier_armed(),
            ) =>
        {
            crate::debug(&format!(
                "re-reading the machine roster of channel head {tag}: the staged build's \
                 authorization is due for confirmation"
            ));
            Ok(Acquisition::Proceed(web_release(source, &tag)?))
        }
        WebHead::Unchanged { tag } => Ok(Acquisition::UpToDate { tag }),
        WebHead::New(candidate) => Ok(Acquisition::Proceed(candidate)),
    }
}

/// Whether the build staged here — publishable and newer than the running build, the
/// one a swap would install — needs its roster authorization confirmed again at `now`
/// ([`Ready::needs_roster_reconfirmation`]).
fn stage_owes_roster_reconfirmation(
    staging: &Staging,
    current_build: u64,
    now: u64,
    armed: bool,
) -> bool {
    armed
        && Ready::read_publishable(staging).is_some_and(|ready| {
            ready.build_number > current_build && ready.needs_roster_reconfirmation(now, armed)
        })
}

/// The download host did not serve an asset the release names (a filtering proxy, or a
/// publish missing an asset): the wording of the failure, and the `delivery=` note it
/// books (one whitespace-free token of the status line).
const ASSET_HOST: &str = "the download host";
const ASSET_BLOCKED_NOTE: &str = "blocked";

/// The recency floors' hold on a release of `build` (F5/F6), as the status line that
/// says it: below the operator floor (a yank), or below the high-water (a rollback).
fn floor_hold(build: u64, effective_min_build: u64, high_water: u64) -> Option<String> {
    if build < effective_min_build {
        return Some(format!(
            "held: latest build {build} is below the operator floor {effective_min_build}"
        ));
    }
    (build < high_water).then(|| {
        format!("held: latest build {build} is below high-water {high_water} (possible rollback)")
    })
}

/// Record what this check OBSERVED in the durable floor, then withdraw a published
/// stage the floor no longer admits, and answer the floor the rest of the check
/// judges by (the one just written — a revocation may have LOWERED it).
///
/// Split out of the check so the rule is a test: the check itself needs the network.
///
/// `Err` (a passing refusal) when the floor is there but could not be read back: the
/// check ends there, judging nothing by a floor it never saw. The
/// stage a revocation in hand withdraws is still retired first.
///
/// AND A STAGE THE ROSTER STILL ADMITS IS CONFIRMED: when this
/// check admitted a roster (`observation.roster_seq`, nonzero only then) and the floor
/// it wrote withdraws nothing on disk, the stage's [`Ready::roster_confirmed_at`]
/// moves to `roster_read_at` ([`crate::install::confirm_roster_authorization`]) — the
/// one thing the swap's gate 4d waits for. `roster_read_at` (RFC3339) is when this
/// check began fetching the roster it judged by, never a later instant: the window
/// bounds how long ago a roster was read.
///
/// The `Check` of `NativeUpdateRosterFreshness`: a check that
/// admitted a roster retires the stage it revokes and confirms the one it trusts.
/// Tier-1: `tests::roster_freshness_conformance`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateRosterFreshness",
        action = "Check",
        project = "aterm_update::roster_freshness_projection"
    )
)]
fn observe_floor_and_withdraw_at(
    staging: &Staging,
    current_build: u64,
    floor_before: crate::manifest::Floor,
    observation: &crate::manifest::FloorObservation<'_>,
    roster_read_at: &str,
) -> Result<crate::manifest::Floor, String> {
    // Remember the authoritative release's operator floor immediately (even if we do
    // not stage). The persisted floor remains monotonic across checks.
    // The same call ratchets the roster sequence. Doing it here — on OBSERVATION, not on
    // successful staging — is what makes the replay defence work: a client that merely
    // SAW roster generation n must refuse n-1 forever after, whether or not it went on to
    // install anything from that release. `observed_roster_seq` carries that observation
    // out of the chain even when the chain then REFUSED the release (a roster that
    // revokes the release's signer is admitted, observed here, and only then refuses),
    // so the ratchet is genuinely observation-driven and not acceptance-driven.
    //
    // And it records what that roster REVOKES, which takes back every floor the revoked
    // machines raised: a stolen key's release
    // with a huge `min_build`, or a huge build this client staged, held every genuine
    // release after the revocation "below the operator floor" forever.
    crate::manifest::Floor::observe_and_write(&staging.floor(), observation);
    // THE FLOOR THIS CHECK JUDGES BY is the one just written: a revocation may have
    // LOWERED it, and the snapshot taken before the fetch would go on holding the
    // release a revoked machine's floor was holding back.
    let floor = crate::manifest::Floor::read_for_gate(&staging.floor());
    if let Ok(floor) = &floor
        && (floor.min_build < floor_before.min_build || floor.high_water < floor_before.high_water)
    {
        crate::warn(&format!(
            "the machine roster revoked {:?}; the update floors it had raised are withdrawn \
             (min_build {} -> {}, high_water {} -> {})",
            floor.revoked_machines,
            floor_before.min_build,
            floor.min_build,
            floor_before.high_water,
            floor.high_water
        ));
    }
    // A STAGE THE FLOOR NO LONGER ADMITS IS WITHDRAWN HERE. The ratchet above records
    // what was SEEN; this acts on what it SAYS, for the stage already on disk: a
    // machine the roster now revokes (roster generation `n+1`), or a build the
    // operator has yanked (`min_build`).
    //
    // A stage is an authorization made earlier, and nothing revisited it: a build
    // staged at 10:00 by a machine revoked at 10:30 was applied anyway (in-session,
    // or at the next launch), and only a separate `min_build` yank could have
    // stopped a withdrawn machine's artifact. A yanked stage, in turn, stayed
    // announced as "verified and ready to apply" until an apply parked every reader
    // for it and the successor's gate 4b refused it.
    //
    // IT IS DECIDED HERE, NOT IN THE APPLY LANE ALONE. Revocation is a LIST, and this
    // is the only place the roster document is in hand; the apply lane holds the
    // floor, whose durable revocation set is what this check just recorded. A
    // roster-GENERATION comparison there is not merely weaker but WRONG — a manifest
    // attributed under an older generation than the roster asset is the ordinary
    // post-join steady state, which `authorize_by_roster` deliberately admits, so
    // gating on it retires good stages forever (see `install`, gate 4c).
    //
    // Matching on `machine_id` is what makes this exact: the id sits inside the
    // manifest's SIGNED bytes and the roster maps ids to keys, so a genuine
    // signature by one machine cannot be relabelled as another's. The stage is
    // read WITHOUT the floor ([`Ready::read_published_bundle`]): the stage the
    // floor now hides from every other reader is exactly the one to find.
    let withdrawal = |staged: &Ready| {
        floor
            .as_ref()
            .ok()
            .and_then(|floor| floor.withdraws(staged.build_number, staged.machine_id.as_deref()))
            .or_else(|| {
                // The floor write above can fail (a full disk); the roster in hand
                // still says who it revokes.
                revocation_withdraws_stage(observation.revoked, staged.machine_id.as_deref()).then(
                    || {
                        format!(
                            "build {} was signed by machine {}, which the machine roster \
                             has revoked",
                            staged.build_number,
                            staged.machine_id.as_deref().unwrap_or("?")
                        )
                    },
                )
            })
    };
    if let Some(staged) = Ready::read_published_bundle(staging)
        && let Some(why) = withdrawal(&staged)
    {
        // UNDER THE APPLY LOCK, like every other retirement of the published stage.
        // `apply_staged_if_ready` in a concurrently LAUNCHING instance verifies the
        // staged `.app` and then renames it into place under `apply_lock`; a
        // `remove_dir_all` racing that rename would gut the tree it was in the
        // middle of installing (and, fd-relative, keep unlinking inside the same
        // inode after the rename — the live install), which the boot sentinel would
        // then read as a crash loop and revert with the build poisoned. Lock order
        // is respected (nothing is held here; the stage lock is taken later), and a
        // boot apply that wins the lock first reads the same floor at its own gates
        // (4b, 4c) and retires the stage itself — the re-read below then finds
        // nothing to retire.
        // Bounded: this is the checker thread, and a wedged apply-lock holder would
        // park it here for good. The `Err` arm already retries.
        match aterm_update_core::FileLock::acquire_within(
            &staging.apply_lock,
            crate::install::BACKGROUND_LOCK_WAIT,
        ) {
            Ok(_apply_lock) => {
                if Ready::read_published_bundle(staging)
                    .is_some_and(|still| still.build_number == staged.build_number)
                {
                    crate::warn(&format!("staged {why}; discarding it"));
                    retire_revoked_stage(staging, staged.build_number);
                    let note = format!("held: staged {why}");
                    // Carried onto every later record of THIS check (status.toml is one
                    // overwritten line): the sentence survives the check's own terminal
                    // outcome, and `aterm ctl update status` really does say so.
                    crate::status::set_check_note(note.clone());
                    crate::status::record(staging, current_build, &note);
                }
            }
            Err(error) => crate::warn(&format!(
                "staged {why}, but the apply lock could not be taken to retire it ({error}); \
                 no surface announces it, and the next check retries"
            )),
        }
    }
    // THE CONFIRMATION. A roster admitted by THIS check — fetched
    // just now, master-verified, no older than any generation this Mac has seen —
    // whose revocations (and every one the floor remembers) leave the stage on disk
    // standing has confirmed it: the swap may install it for another day. A floor
    // that could not be read confirms nothing, and nor does a check that admitted no
    // roster (the unchanged head's pointer read, a fetch that failed).
    if observation.roster_seq > 0
        && floor.is_ok()
        && let Some(staged) = Ready::read_published_bundle(staging)
        && withdrawal(&staged).is_none()
        && let Err(error) =
            crate::install::confirm_roster_authorization(staging, &staged, roster_read_at)
    {
        crate::warn(&format!(
            "could not record that staged build {} was confirmed against the machine \
             roster ({error}); the next check that reads the roster tries again",
            staged.build_number
        ));
    }
    floor
}

/// Project one observed stage onto the bounded `NativeUpdateRosterFreshness`
/// machine: ticks of half the window (twelve hours of
/// [`crate::manifest::ROSTER_FRESHNESS_SECS`]), the tick its confirmation was read
/// at (`confirmed`), when the owner's revocation of its signer was
/// published (`revoked_at`, `None`: not yet), whether its marker and bundle are
/// on disk, and whether a lane installed it and how late (`swapped`: `None` not
/// yet, `Some(late)` once).
///
/// Compiled only for proof/test builds. Every input is an OBSERVATION of the real
/// staging root and of the real gate's answer, never a second copy of either rule.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn roster_freshness_projection(
    now: i64,
    confirmed: i64,
    revoked_at: Option<i64>,
    staged: bool,
    swapped: Option<bool>,
) -> std::collections::BTreeMap<&'static str, i64> {
    std::collections::BTreeMap::from([
        ("now", now),
        ("conf", confirmed),
        ("revoked", i64::from(revoked_at.is_some())),
        ("rev_at", revoked_at.unwrap_or(0)),
        ("staged", i64::from(staged)),
        ("swapped", i64::from(swapped.is_some())),
        ("late", i64::from(swapped == Some(true))),
    ])
}

/// A publishable stage strictly newer than the running build, as the check loop's
/// `Some` answer: "a build is staged and can be applied" — reported so the apply lane
/// arms even on a check that fetched nothing.
fn applicable_stage(staging: &Staging, current_build: u64) -> Option<String> {
    Ready::read_publishable(staging)
        .filter(|ready| ready.build_number > current_build)
        .map(|ready| ready.version)
}

/// The check's snapshot of the floor, or the refusal that ends the check: passing,
/// since nothing is admitted past a floor that was not read and the next check
/// reads it again ([`crate::manifest::Floor::read_for_gate`]) — and BOOKED,
/// `pipeline` class.
///
/// Failing closed is right for a moment (`EMFILE` at a Dock launch's descriptor
/// limit, one `EIO`), but a floor that STAYS unreadable — root-owned after a `sudo`
/// run, a broken ACL, a failing disk — ends every check at this line, before any
/// other booking, and `commit_observation` can never rewrite it: all updates stop
/// for good. Unbooked, the only trace was `status.toml`'s "update check failed" and a
/// de-duplicated log line. Booked, a streak of [`crate::PERSISTENT_AFTER`] checks
/// raises the persistent notice, which names the file and the remedy.
fn floor_snapshot(staging: &Staging) -> Result<crate::manifest::Floor, String> {
    crate::manifest::Floor::read_for_gate(&staging.floor()).inspect_err(|error| {
        crate::health::Health::record_failure(&staging.health(), "pipeline", error);
    })
}

/// What one check runs against besides the network: the folder the installed bundle sits
/// in, the staging layout, the pointer transport, the roster anchor and the Team ID a
/// staged bundle must be signed by. [`check_and_stage_inner`] resolves the shipping ones;
/// the end-to-end test supplies a scratch world, so the whole lane runs over real curl
/// against a channel signed by a scratch master.
struct CheckWorld<'a> {
    installed_parent: Option<&'a std::path::Path>,
    staging: Staging,
    head: HeadFetch<'a>,
    master_pubkeys: &'a [&'a str],
    team: &'a str,
}

fn check_and_stage_inner(
    current_build: u64,
    source: &Source,
    asked: Asked,
) -> Result<Option<String>, String> {
    // Only stage for a real installed bundle (a dev build has nothing to swap).
    let Some(installed) = bundle::resolve() else {
        return Ok(None);
    };
    let staging = Staging::resolve().ok_or("could not resolve Updates dir")?;
    // The pointer transport, INJECTED rather than called directly, so the request cost
    // is measurable in a test.
    let mut head = aterm_update_core::head_no_redirect;
    let world = CheckWorld {
        installed_parent: installed.app_root.parent(),
        staging,
        head: &mut head,
        master_pubkeys: aterm_update_core::pins::PAPER_MASTER_PUBKEYS,
        team: crate::effective_team_id(),
    };
    check_and_stage_in(world, current_build, source, asked)
}

/// [`check_and_stage_inner`] in `world`.
fn check_and_stage_in(
    world: CheckWorld<'_>,
    current_build: u64,
    source: &Source,
    asked: Asked,
) -> Result<Option<String>, String> {
    let CheckWorld {
        installed_parent,
        staging,
        head,
        master_pubkeys,
        team,
    } = world;
    // Recursive copy cleanup runs in this checker worker. Both staging and
    // cross-volume apply leave isolated attempts on their destination volume.
    install::reap_abandoned_copy_attempts(&staging.staged_dir());
    if let Some(parent) = installed_parent {
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
    // from there would erase the failures its own attempt is about to add, keeping
    // `failing_applies` at 1. Every other launch heals on its first check.
    if !crate::is_uncommitted_handoff_candidate() {
        crate::health::Health::expire_stale_apply_streak(&staging.health(), current_build);
    }
    // Persisted monotonic recency floor (operator yank + rollback guard, F5/F6). A
    // floor that could not be read ends the check, passing and booked: judged
    // as "no floor", a yanked or rolled-back release would be authorized, staged and
    // announced on that read.
    let floor = floor_snapshot(&staging)?;

    let authoritative = match acquire(&staging, current_build, source, head)? {
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
            // carry: "up to date" beside `staged_build = <newer>` would read as
            // current.
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
        |url: &str, max_bytes: u64| aterm_update_core::download_bytes(url, max_bytes);
    // The roster tier's inputs, resolved from the anchor and this client's durable state.
    // `PAPER_MASTER_PUBKEYS` is ARMED, so the master-signed roster decides
    // who may have signed the appcast this check accepts, and every field below is
    // load-bearing.
    //
    // `floor_seq` is a snapshot read before the (network) head fetch above, so it can be
    // stale by the time a roster is admitted; `floor_refresh` re-reads the durable floor
    // at the admission point itself, closing the check-vs-ratchet TOCTOU between two
    // concurrent app instances.
    let floor_path = staging.floor();
    // A re-read that failed keeps the snapshot: the max below only tightens.
    let floor_refresh = || crate::manifest::Floor::read(&floor_path).roster_seq;
    let roster_policy = RosterPolicy {
        master_pubkeys,
        floor_seq: floor.roster_seq,
        now_unix: aterm_update_core::roster::now_unix(),
        floor_refresh: Some(&floor_refresh),
    };
    // WHEN THIS CHECK READ THE ROSTER, taken before the
    // fetch that reads it: the stage this check confirms, or publishes after its
    // download, is stamped with this instant, never a later one. A download may run
    // for hours, and a stamp taken at the publish added all of it to the window a
    // revocation can hide in. An unreadable clock is empty, which stamps nothing
    // (the swap reads that as unconfirmed).
    let roster_read_at = crate::install::now_rfc3339();
    let mut fetched =
        fetch_authoritative_release(Some(authoritative), &mut download, &roster_policy);
    // THE SOURCE-ONLY HEAD: the pointer named a release whose appcast answers 404. The
    // head is not an app release yet; say so and end the check, healthy. The tag is not
    // recorded as authorized, so the next check reads it again and the app cut that
    // attaches its appcast is picked up then.
    if fetched.appcast_missing {
        record_source_only_head(&staging, current_build, source, &web_tag);
        return Ok(None);
    }
    fetched.refuse_unbound_or_revoked(source, &floor, &web_tag);
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
    let floor = observe_floor_and_withdraw_at(
        &staging,
        current_build,
        floor,
        &fetched.floor_observation(),
        &roster_read_at,
    )?;
    let best = fetched.selected;
    let seen_min_build = best
        .as_ref()
        .and_then(|(manifest, _, _)| manifest.min_build)
        .unwrap_or(0);
    let effective_min_build = floor.min_build.max(seen_min_build);

    let Some((manifest, release, artifact)) = best else {
        if appcast_fetch_error && asset_fetch_rate_limited {
            // The manifest/roster/signature GET met the host's throttle — the same
            // verdict a throttled pointer HEAD gets one request earlier, and for the
            // same reason no `record_failure`: a saturated check must not book a
            // persistent `pipeline` streak and fire the "download pipeline is likely
            // broken" notice at a perfectly healthy machine.
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
            // readable — a `pipeline`-class failure, or `network` inside a wake's grace
            // ([`asset_failure_class`]). The ledger decides the honest wording: a
            // persistent streak (`Health::persistent_class`: the count and the span)
            // is not called "deferred".
            let host = ASSET_HOST;
            let class = asset_failure_class();
            if class == "pipeline" {
                crate::status::set_delivery_note(ASSET_BLOCKED_NOTE);
            }
            let h = crate::health::Health::record_failure(
                &staging.health(),
                class,
                &format!("release manifests exist but could not be fetched from {host}"),
            );
            if h.persistent_class()
                .is_some_and(|(class, _)| class == "pipeline")
            {
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
                    if class == "pipeline" {
                        h.pipeline_failures
                    } else {
                        h.network_failures
                    }
                )
            }
        } else if manifest_rejected {
            // Manifests were FETCHED but rejected (unsigned / bad signature /
            // unparseable / a version or URL that does not bind to the tag): the
            // pipeline works; the release side (or an attacker) is the problem. Its
            // own class — it must not clear a streak. The ledger gets the reason the
            // log printed, and a STRANDED client — one whose own anchor
            // cannot verify the channel — is told to reinstall, not to wait for the
            // publisher.
            let reason = rejection_reason
                .as_deref()
                .unwrap_or("manifest(s) fetched but rejected (signature/parse)");
            let h = crate::health::Health::record_failure(&staging.health(), "manifest", reason);
            // A stage waiting for its roster confirmation cannot get it from a build whose
            // anchor cannot verify the roster: said, so the update row does not promise a
            // check that cannot come.
            let stage_note = is_stale_anchor_refusal(reason)
                .then(|| {
                    unconfirmable_stage_note(
                        &staging,
                        current_build,
                        crate::install::unix_now_secs(),
                        aterm_update_core::pins::roster_tier_armed(),
                        STALE_ANCHOR_STAGE_WAIT,
                    )
                })
                .flatten()
                .map_or_else(String::new, |note| format!("; {note}"));
            let line = if h
                .persistent_class()
                .is_some_and(|(class, _)| class == "manifest")
            {
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
            };
            line + &stage_note
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
    // not an input to an acquisition decision.
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
    if let Some(held) = floor_hold(manifest.build_number, effective_min_build, floor.high_water) {
        crate::health::Health::record_success(&staging.health());
        crate::status::record(&staging, current_build, &held);
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
        // only waiting to be applied". Every `aterm` session process runs this
        // checker, so a SIBLING often wins the race to publish; `None` here would
        // leave the GUI's apply lane unarmed until a relaunch (`arm` dedups).
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
        asked,
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
    //
    // BOUNDED: a blocking wait would park the checker thread behind any holder, including
    // one stopped mid-download. A holder past the bound is another process staging (the
    // next check reads what it staged) or one stopped mid-stage, which `take_stage_lock`
    // tells apart by the holder's beat and books as the typed failure it is.
    let _stage_lock = take_stage_lock(&staging, crate::install::BACKGROUND_LOCK_WAIT)?;
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
        crate::manifest::Floor::read_for_gate(&staging.floor())?.roster_seq,
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
    // LIVE PROGRESS for a host that shows it (the aterm window's band row): a
    // sibling poller stats the growing `.part`, and a HEAD of the same derived URL,
    // run beside the download and never before it, gives the total its fill and ETA
    // are drawn from (design ruling 226; the digest below still proves the bytes).
    // The poller is joined on drop, so it cannot outlive the download it watches.
    let download_watch = crate::progress::watch_download(&part, &manifest.version, &asset.url);
    // A failed download is a `pipeline`-class ledger entry: the asset provably
    // exists (the release names it) but could not be fetched — unless it was this
    // machine that could not store it (`book_download_failure`).
    let downloaded = aterm_update_core::download_to(
        &asset.url,
        &part,
        aterm_update_core::RELEASE_ASSET_DOWNLOAD_BOUND,
    );
    drop(download_watch);
    if let Err(e) = downloaded {
        // Read before the partial download goes, so a full disk still reads as one.
        let free = crate::install::free_bytes_at(&staging.download);
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
        return book_download_failure(&staging, current_build, &manifest, container, &e, free);
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

    // Integrity: SHA-256 must equal the manifest's digest FOR THIS CONTAINER. Every
    // exit here is booked: a machine that can never hash a download must still reach
    // the persistent-failure notice.
    //
    // BOUNDED, AND BOOKED BY THE ONE RULE: the hash has a deadline
    // ([`container_sha256`]), and a hash past it or one whose thread the system would
    // not start just then is a moment, booked as one at the one booking point.
    if let Some(end) = check_container_digest(
        &staging,
        current_build,
        &manifest,
        &container_path,
        container,
        &artifact.sha256,
        |path| container_sha256(path, CONTAINER_HASH_WAIT),
    ) {
        return end;
    }

    // Unpack, verify (codesign/team-id/spctl), publish the ready marker. On a
    // post-download stage failure (verification etc.) memoize this build+sha so we
    // don't re-download it next cycle, and reclaim the container (F17). The memo is
    // keyed on the MANIFEST digest, not this container's, so the two paths share one
    // retry budget for one candidate build.
    let staged = match artifact.container {
        Container::Zip => {
            install::stage_from_zip(&staging, &container_path, &manifest, team, &roster_read_at)
        }
        Container::Dmg => {
            install::stage_from_dmg(&staging, &container_path, &manifest, team, &roster_read_at)
        }
    };
    if let Err(e) = staged {
        let _ = std::fs::remove_file(&container_path);
        return end_failed_stage(&staging, current_build, &manifest, e);
    }
    // The verified bundle is the artifact now; reclaim the container and clear the
    // backoff memo — never a quarantine verdict, which a success of this build says
    // nothing about.
    let _ = std::fs::remove_file(&container_path);
    crate::manifest::FailedMark::clear_backoff(&staging.failed());
    // Terminal healthy outcome: this check exercised the WHOLE pipeline (manifest,
    // container, verify, stage) successfully — clear every failure streak.
    crate::health::Health::record_success(&staging.health());
    // Raise the high-water to the build we just staged: a later attempt to roll us
    // back below it is refused above (F6). ATTRIBUTED to the machine that signed it,
    // so a revocation of that machine takes it back: a
    // stolen key's staged build 9_000_000_000 otherwise held every genuine release
    // after the revocation "below high-water" for good.
    crate::manifest::Floor::observe_and_write(
        &staging.floor(),
        &crate::manifest::FloorObservation {
            high_water: manifest.build_number,
            machine: manifest.machine_id.as_deref(),
            ..crate::manifest::FloorObservation::default()
        },
    );
    // …and remember the tag, so the next check is one HEAD.
    crate::status::set_latest_tag(&web_tag, source, current_build, manifest.build_number);

    // NOT "applies on next launch": the in-session apply lane owns that decision,
    // and when it runs no relaunch happens at all. This is a record of what the
    // stager did, not advice. A refused apply overwrites it through
    // `record_apply_refusal` (a guarantee of those call sites, not this one), so
    // read it as "the last thing the STAGER knew".
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
#[path = "github_tests.rs"]
mod tests;
