// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The macOS (DMG/.app) update check: learn the channel head and, if it is strictly newer
//! than the running build, download, verify and stage its container. The portable plumbing
//! (the redirect-refusing HEAD, the `curl` GETs) is `aterm-update-core`.
//!
//! Every check reads `github.com/<owner>/<repo>/releases/…` with no credential and never
//! touches `api.github.com`:
//!
//! 1. One unmetered `HEAD …/releases/latest/download/aterm-appcast.toml`, redirects refused
//!    ([`aterm_update_core::pointer`]). Its `Location` names the newest published release's
//!    tag and is accepted only as this repository's derived URL under a canonical
//!    `vMAJOR.MINOR.PATCH` tag.
//! 2. If that tag is the one the ledger last authorized (`latest_tag` in `status.toml`),
//!    the check is done.
//! 3. Otherwise every asset is fetched from the tag-specific
//!    `…/releases/download/<tag>/<name>` (never `latest` again, so a moving pointer cannot
//!    mix releases) and must pass the master-signed roster ([`authorize_by_roster`]),
//!    manifest `version` == the tag, the manifest's container `url` == the derived URL
//!    (cross-checked, never followed), the build-number, `min_build`, high-water and
//!    roster-floor gates, the sha256, and codesign/Team ID on the staged bundle.
//!
//! A head that is not an app release (no `aterm-appcast.toml`, or a tag this client does
//! not install from) is recorded as such and the check ends. Nothing looks past `latest`,
//! the only unmetered sign that the publisher promoted a release: the host serves a
//! prerelease's assets like a release's, aterm's source release is a prerelease carrying
//! its cut's appcast, and an older tag would be judged under its own older roster, missing
//! a revocation published source-first. A source-only head that outranks the last
//! authorized tag is an app build in flight ([`app_build_in_flight`]): the next checks
//! come on `cadence::IN_FLIGHT_RETRY`, counted machine-wide ([`head_in_flight`]).
//!
//! The lane is not a trust decision. Artifact trust is the master-signed roster, the
//! pinned Team ID and the manifest sha256 — none of which the transport touches.

use std::sync::atomic::{AtomicBool, Ordering};

use aterm_update_core::tag::{TagError, TagKind};
use aterm_update_core::{HeadAnswer, HttpError};

use crate::manifest::{Manifest, Ready};
use crate::{Source, bundle, install, paths::Staging};

/// Whether the last check ended in a deferral (the download host asked us to slow down
/// with a 429 or a 5xx). The background loop lengthens the next wait on it without
/// recording a failure: weather, not a broken updater.
static RATE_LIMITED: AtomicBool = AtomicBool::new(false);

/// The channel head whose app build the check in progress found in flight
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

/// Whether `head`, a channel head with no `aterm-appcast.toml` yet, is an app build in
/// flight: a tag this client installs from that outranks `known` (the last tag the ledger
/// authorized for this source, whichever build recorded it, so every build on the machine
/// agrees), or any such tag when none is known. The train publishes source first and the
/// app cut attaches its assets minutes later. A head at or below `known` (GitHub's `latest`
/// is the most recently published, which can be an older tag) is not awaited.
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

/// Record that the channel head was read, which clears the "this machine cannot update"
/// latch.
fn note_readable(source: &Source) {
    RATE_LIMITED.store(false, Ordering::Relaxed);
    crate::unreadable::clear();
    // Once per process, at DEBUG: every aterm process runs this loop.
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

/// The message an operator gets when this machine cannot read its release channel (the
/// evergreen pointer answered 404). Read months later out of `status.toml`, so it names the
/// consequence, every cause and the remedy. GitHub's web host renders "no published
/// release", "private" and "does not exist" alike, so all are named; the channel is read
/// with no credential, so none is offered. A repointed source adds its own cause.
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

/// A GitHub Release synthesized from the pointer's tag ([`web_release`]) with the derived
/// tag-specific URLs, so the roster chain and the staging tail consume one shape.
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

/// Classify the channel head's tag with [`aterm_update_core::tag::parse_release_tag`], the
/// grammar the publisher (`aterm-release/src/publish.rs`) uses too, so the two cannot
/// disagree. Only the wording is here: `vMAJOR.MINOR.PATCH` is a candidate, a
/// two-component tag is the retired scheme ([`TagKind::Legacy`]), anything else fails
/// closed.
fn parse_numeric_tag(tag: &str) -> Result<TagKind, String> {
    aterm_update_core::tag::parse_release_tag(tag).map_err(|error| match error {
        TagError::Malformed => format!("update candidate tag {tag:?} is not numeric dotted vN.N.N"),
        TagError::Overflow => {
            format!("update candidate tag {tag:?} has an out-of-range numeric component")
        }
    })
}

/// The tag contract: an installable release is spelled exactly `vMAJOR.MINOR.PATCH`
/// (`VERSIONING.md`). `numeric` is already three-component ([`parse_numeric_tag`]);
/// re-deriving the string pins the spelling too, so `v01.2.3` is never admitted beside
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
/// `Ok(None)`: no usable zip (no name, or no digest). `Err`: a declared zip this release
/// cannot resolve (noncanonical, missing, duplicated). Neither ends the check: staging falls
/// back to the DMG, which is proved separately.
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

/// Choose the container to stage from: the zip when the manifest carries a resolvable one,
/// else the DMG. The DMG identity is proved first and unconditionally, so the fallback
/// always exists. Preferring the zip is availability, not trust: both digests come from
/// the same manifest and both bundles pass the same codesign gate.
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

/// Everything the master-signed machine-roster tier needs, as one parameter. With
/// `master_pubkeys` empty (a fork with no master) the tier is absent; armed, as in this
/// tree, the roster is the sole authority over who may have signed the appcast.
pub(crate) struct RosterPolicy<'a> {
    /// The pinned paper master(s) — `pins::PAPER_MASTER_PUBKEYS` in production. Empty
    /// means the tier is absent.
    pub master_pubkeys: &'a [&'a str],
    /// The highest `roster_seq` this client has durably recorded: the replay defence for
    /// a client that has seen a newer roster (a fresh install relies on `valid_until`).
    pub floor_seq: u64,
    /// Injected wall clock (unix seconds), so the freshness gate stays pure and every
    /// expiry case is testable without waiting for one.
    pub now_unix: i64,
    /// Re-reads the durable floor just before admission: `floor_seq` is taken before any
    /// network I/O, and a concurrent instance may ratchet the floor past it meanwhile.
    /// Admission takes the max of the two, so this can only raise the floor; `None`
    /// (tests) leaves the snapshot to decide.
    pub floor_refresh: Option<&'a dyn Fn() -> u64>,
}

/// The marker a refusal carries when it is this build's trust anchor that cannot verify the
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
    /// The `appcast_fetch_error` was the manifest itself answering 404: a source-only
    /// release. Not a fault: the check records the head as having no app manifest yet.
    appcast_missing: bool,
    /// The `appcast_fetch_error` was a `github.com` 429 on an asset fetch, not a broken
    /// download: weather, and never a `pipeline`-class failure.
    asset_fetch_rate_limited: bool,
    manifest_rejected: bool,
    /// Why the manifest was rejected, in the log's words, so the ledger can tell a forged
    /// release, a revoked signer and a stranded client ([`STALE_ANCHOR_KEY`]) apart.
    rejection_reason: Option<String>,
    /// Which machine signed, when the roster tier is armed and the chain passed. `None`
    /// with an unpinned master (the tier is absent) and `None` on any rejection, because
    /// a rejection never produces a `selected` release either.
    attribution: Option<aterm_update_core::roster::Attribution>,
    /// The `roster_seq` of the master-verified roster that passed admission, for the
    /// caller to ratchet into the durable floor. Set on observation, not acceptance: a
    /// seq-10 roster that revokes the appcast's signer refuses the release, and a floor
    /// that ratcheted only on acceptance would let a replayed, still-fresh seq-9 roster
    /// re-authorize that machine.
    observed_roster_seq: Option<u64>,
    /// The machine ids the admitted roster revokes, set on the same observation as
    /// [`Self::observed_roster_seq`]. The caller retracts an already-staged build by them:
    /// revocation is a list inside the roster document, which only this lane reads.
    observed_revocations: Vec<String>,
}

impl AuthoritativeFetch {
    /// Refuse the candidate: warn the reason and carry it to the ledger. Every rejection
    /// arm goes through here, so `health.toml` records the reason the log printed.
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
        // After every signature has been verified and the version bound, and before
        // anything is staged, the signed manifest must name the very container URL this
        // client derived under the pointer's tag.
        if let Some((manifest, release, _)) = self.selected.as_ref()
            && let Err(error) = web_container_url_agrees(source, &release.tag_name, manifest)
        {
            let reason = format!("{error}; refusing authoritative {}", release.tag_name);
            self.selected = None;
            self.refuse(reason);
        }
        // Durable revocation set: a machine a roster this client
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

/// Why the roster chain produced no attribution, in the two classes the health ledger
/// must not confuse. Both refuse; they differ in what the operator is told. A
/// `pipeline`-class transport failure is retried, while a `manifest`-class refusal says
/// this Mac cannot install until the publisher fixes it, which a flaky fetch of
/// `aterm-machines.toml` must not claim.
enum RosterFailure {
    /// An asset could not be fetched, or what came back is not shaped like the asset (a
    /// signature that is not [`ED25519_SIGNATURE_LEN`] bytes). Nothing was judged; a retry
    /// may work.
    Transport(String),
    /// The chain ran and refused: no roster, a bad master signature, a stale or
    /// rolled-back generation, a revoked machine, an unauthorized signer.
    Refused(String),
}

/// Run the master-signed machine-roster chain for one candidate release and return which
/// machine signed its appcast. With the master armed this is the only authority. Cheapest
/// first, fail-closed throughout:
///
/// 1. The appcast signature and both roster assets are on the release (no roster, no
///    authority).
/// 2. Download them under tight caps (a roster is a few hundred bytes per machine; a
///    signature is 64 bytes).
/// 3. Verify the roster under the pinned paper master.
/// 4. Parse it, only from `VerifiedRoster`, which has no public constructor.
/// 5. `admit`: the durable `roster_seq` floor, then the freshness window.
/// 6. `authorize_appcast`: revoked and expired machines leave the candidate set before any
///    signature is checked; the survivors verify the appcast.
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

    // (2) Bounded transport. A failure here has judged nothing ([`RosterFailure`]).
    let roster_bytes = download(&candidate.release.assets[roster_index].url, 65_536)
        .map_err(|e| Transport(format!("fetch {ROSTER_ASSET}: {e}")))?;
    let roster_sig = download(&candidate.release.assets[roster_sig_index].url, 4096)
        .map_err(|e| Transport(format!("fetch {ROSTER_SIG_ASSET}: {e}")))?;
    let appcast_sig = download(&candidate.release.assets[signature_index].url, 4096)
        .map_err(|e| Transport(format!("fetch appcast signature: {e}")))?;
    // A body that is not 64 bytes is no failed signature but something else answering
    // with a 2xx (a TLS-inspecting proxy's block page, a captive portal): transport, which
    // a retry or another network can clear, where `Refused` would advise a reinstall. A
    // 64-byte signature that does not verify stays a verdict.
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
        // Not a rejection: a non-head master is a rotation in flight, logged (at INFO, the
        // roster verified) so a stalled rotation is visible.
        crate::log(&format!(
            "the machine roster was signed by master key #{}, not the current one — a \
             master rotation is in progress or incomplete",
            verified.master_index()
        ));
    }
    let roster = Roster::parse(&verified)
        .map_err(|e| Refused(format!("machine roster is unusable ({e:?})")))?;

    // (5) Replay floor, then freshness. The floor is re-read
    // ([`RosterPolicy::floor_refresh`]), since `floor_seq` predates the downloads above;
    // the max only raises it.
    let floor_seq = policy
        .floor_refresh
        .map_or(policy.floor_seq, |read| read().max(policy.floor_seq));
    roster
        .admit(floor_seq, policy.now_unix)
        .map_err(|e| Refused(format!("machine roster refused ({e:?})")))?;
    // Observed: master-verified and admitted. Recorded before the appcast authorization,
    // so a refusal below (a revoked signer, above all) still advances the floor.
    *observed_roster_seq = Some(roster.roster_seq);
    // The same event: a roster that refuses this release below still reports whom it
    // withdrew.
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
    // Who may have signed this appcast is decided by the paper master anchor alone:
    // unpinned (a fork with no master), the signature tier is absent; armed (this tree),
    // the master-signed roster decides, with no compiled-in key to fall back to. It runs
    // over the raw bytes, before the parse: the identity claims inside bind to nothing
    // until the bytes are verified (`bind` follows the parse). The observed sequence and
    // revocations land in `fetched` on every arm, refusals included
    // ([`AuthoritativeFetch::observed_roster_seq`]).
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
            // Fail closed: suppressing the roster assets must not downgrade an armed client.
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
            // The identity bind, over authenticated bytes: the machine id is inside the
            // signed bytes and the roster maps id to key, so a signature can be neither
            // relabelled nor claimed by another machine.
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
                // The durable record is the status file's, written by the check; the log
                // line is once per release per process.
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
/// staged build. A marker that names no machine is left alone: an unknown signer is no
/// evidence of withdrawal, and retiring it would cost a re-download on every check.
fn revocation_withdraws_stage(revocations: &[String], staged_machine: Option<&str>) -> bool {
    staged_machine.is_some_and(|machine| revocations.iter().any(|id| id == machine))
}

/// Whether the durable floor (`floor_now`, a fresh read) has superseded the roster
/// generation that authorized this release (`observed`) while the check ran. `None` (no
/// roster reached `admit`) is never superseded. This run's own ratchet makes
/// `floor_now >= observed`, so a strict `<` means a concurrent instance recorded a newer
/// generation. The refusal is transient: the next check runs under the new floor.
fn roster_authority_superseded(observed: Option<u64>, floor_now: u64) -> bool {
    observed.is_some_and(|seq| seq < floor_now)
}

/// Whether a published local stage makes this release's download redundant: a strictly
/// newer build, or the same build bound to the manifest's commit and DMG digest. The
/// pre-lock check and the under-lock re-check share it.
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

/// Record in `status.toml` that a verified stage already covers this candidate. The check's
/// attribution note precedes this decision, so without it a machine holding a pending
/// stage would show only who signed the release.
fn record_covered_stage_status(staging: &Staging, current_build: u64, manifest: &Manifest) {
    // Re-read the marker, the authority for what is on disk. If a concurrent retire removed
    // it since the predicate ran, still record a decision, naming the candidate.
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

/// The verdict for a stage the check found already in place, with the apply lane's
/// standing answer about that build, so a check never paints "ready" over "did not apply".
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

/// Reclaim container scratch a killed run leaked: the download path's counterpart to
/// `install::sweep_stale_mounts` / `install::sweep_stale_extracts`. Container names are
/// version-keyed, so a partial or abandoned container for a version the channel has moved
/// past is reachable by no other path, and `Staging::retire_published` never touches
/// `download/`. Every regular file goes (each download writes `{name}.part` and renames it,
/// so nothing reuses bytes here); directories are left alone.
///
/// Callers must hold `staging.stage_lock`: the staging critical section is this
/// directory's only writer.
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

/// An open stage-failure retry window, and what it may stop. The window in `failed.toml`
/// stops only re-downloading (up to 2 GiB of) bytes that refused to become a verified
/// bundle. It never holds back an already-published bundle, which costs no bandwidth and
/// has its own streak (`Health::apply_failures`): a re-published artifact (same build, new
/// digest, not covered by [`publishable_stage_covers`]) that fails to stage must not keep
/// the earlier verified one from applying.
#[derive(Debug)]
struct StageBackoff {
    /// Seconds until the candidate named by the manifest may be downloaded again.
    /// Meaningless when `quarantined` — that window never opens.
    retry_in_secs: u64,
    /// Consecutive stage failures recorded for that candidate (at least 1).
    attempts: u32,
    /// The candidate is quarantined (applied, crash-looped, reverted): not a timed backoff,
    /// and nothing on this machine retries it.
    quarantined: bool,
    /// The window is a moment's: staging could not finish, and the line must not say it
    /// "failed".
    moment: bool,
    /// A published local stage that is strictly newer than the running build, if
    /// one exists. The backoff never gates this: the check reports it so the apply
    /// lane runs on it this cycle.
    applicable: Option<Ready>,
}

impl StageBackoff {
    /// The operator-facing line for an open window: the re-stage it skips, then whether an
    /// apply is skipped too.
    fn status_line(&self, candidate_build: u64) -> String {
        let restage = if self.quarantined {
            // A quarantine has no clock: never "retrying automatically".
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

/// Read the stage-failure memo and decide what it may stop for this candidate; `None` when
/// no window is open for it (no memo, another artifact, or past the deadline).
///
/// A person's check passes a moment's memo: a moment (a full disk, a helper past its
/// deadline) says nothing about the bytes, and a person asking may be the news that it
/// passed. A verdict's memo and a quarantine still hold.
///
/// `applicable` is gated by [`Ready::read_publishable`], the local read the status and
/// apply surfaces share; full codesign/Team ID re-verification stays under the apply lock
/// on the apply path, so a backed-off check spawns no verification helper.
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

/// This check's hold on `stage.lock`, with the beat that tells a waiting sibling the
/// holder is alive. The beat is declared first so it stops before the lock is let go,
/// and never stamps this pid into a lock another process then holds.
struct StageHold {
    _beat: crate::HolderBeat,
    _lock: aterm_update_core::FileLock,
}

/// Take the machine-wide stage lock (download → extract → publish), waiting at most `wait`:
/// a blocking wait would park the checker behind any holder.
///
/// The holder stamps the lock while its check is alive ([`crate::stage_holder_alive`]:
/// checker generation current, heartbeat fresh), so a hung holder looks as stuck as a
/// stopped one. A waiter that times out reads the stamp: fresh is a sibling staging (stand
/// down), stale is a holder idle for half the wait. A build older than this one never
/// stamps, so a first stale reading stands down unbooked; only a second consecutive one
/// with the same stamp ([`StaleStageStamps`]) is booked, as the typed `pipeline` failure
/// ([`STOPPED_STAGE_HOLDER`]) that `update status` and the persistent notice report. A pid
/// the lock names is never reported as this process's own: an older holder leaves the last
/// writer's there. The lock cannot be taken from a live process, stopped or not.
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
            // No count of checks in the words: every later check that finds the same stamp
            // books this again.
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

/// The stage-lock stamps this process last found stale, per lock path: the memory behind
/// [`take_stage_lock`]'s two-reading rule. In process only: a relaunch costs one more
/// unbooked reading, never a false one. The readings must be consecutive: a check that
/// ends without reaching the stage lock ends the run ([`Self::check_ended`]), so a stale
/// reading cannot pair with one days later.
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
    /// Record the stale stamp `path` carries now, and say whether the previous stale
    /// reading of `path` found this same stamp in the same run of checks (nothing stamped
    /// in between, no check in between that did without the lock). An unreadable stamp is
    /// never a second reading.
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

/// Retire the published stage of `build`, which the floor withdraws (a revoked signer, a
/// yank), and forget its pending-update clock, which would otherwise announce a build that
/// can never apply. The caller holds the apply lock and has re-read that the stage is still
/// `build`'s; a clock re-keyed to another build is left alone
/// ([`crate::health::Health::forget_pending_update`]).
pub(crate) fn retire_revoked_stage(staging: &Staging, build: u64) {
    staging.retire_published();
    crate::health::Health::forget_pending_update(&staging.health(), build);
}

/// How [`book_stage_failure`] booked a stage that did not publish.
#[derive(Debug, PartialEq, Eq)]
enum StageBooked {
    /// Held, not failed: the release was withdrawn (a revoked signer, a yank) or
    /// quarantined while this check downloaded it. `status.toml` says why (`held: …`, the
    /// words carried here); the next check refuses it before any download. A healthy end.
    Held(String),
    /// A moment: staging could not finish; booked on the moment schedule.
    Moment,
    /// A verdict on the bytes; booked on the verdict schedule.
    Verdict,
}

/// Book a stage that did not publish after its container arrived (a digest, an unpack, a
/// verification, the publish), or a container this machine could not store
/// ([`book_download_failure`]), and say how it was booked: the one booking point past the
/// download, so one rule tells a moment from a verdict.
///
/// Only a verdict is booked against the build (`install::stage_failure_is_a_verdict`). A
/// helper past its deadline or one the kernel would not start, a copy past its launchd
/// budget, an `hdiutil attach` past its limit, the apply lock a sibling held through the
/// publish: each is a moment ([`crate::is_passing_refusal`]). A moment is memoized on
/// [`crate::manifest::MOMENT_BACKOFF_SECS`] (the first costs nothing; one that recurs widens
/// to four hours, shared by every aterm on the Mac; a person's check passes it,
/// [`stage_backoff`]) and advances the `stage` streak, so past `PERSISTENT_AFTER` the
/// persistent notice says staging could not finish, never that the build will not verify.
///
/// A stage refused because the artifact was quarantined or withdrawn meanwhile is
/// [`StageBooked::Held`], a healthy end.
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
    // Withdrawn while it downloaded (a sibling's check recorded its signer's revocation, or
    // a yank): held, with the floor as its standing record.
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

/// End the check for a stage that did not publish: booked by [`book_stage_failure`], a held
/// release is a healthy `Ok(None)` (a download's progress row comes down as withheld), and
/// anything else is the check's error.
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

/// Book a container download that failed with `error` (a rate limit defers instead) and
/// answer how the check ends: its error, unless the release was held meanwhile
/// ([`end_failed_stage`]).
///
/// A download this machine could not store is not the host's fault (design law L4): curl's
/// local write error ([`aterm_update_core::download_error_is_local_write`]) is a passing
/// moment of the stage lane ([`book_stage_failure`]), retried on the moment's schedule,
/// which a person's check passes ([`stage_backoff`]). It is any local write or open
/// failure, so the words say "is full" only when `free` (read before the partial was
/// removed) is under [`crate::install::STAGE_FREE_SPACE_HEADROOM`]; otherwise they keep
/// curl's cause. Every other failure is the host's (`pipeline`).
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

/// The asset names a release carries for the updater; the check derives their URLs
/// ([`web_release`]). The appcast's is shared with the dev build's channel look
/// ([`crate::dev_channel`]), so the two never HEAD different names.
const APPCAST_ASSET: &str = crate::dev_channel::APPCAST_ASSET;
const APPCAST_SIG_ASSET: &str = "aterm-appcast.toml.sig";

// The check

/// The injected pointer transport, `url → (status, Location)`:
/// [`aterm_update_core::head_no_redirect`] in production, a counting fake in tests.
type HeadFetch<'a> = &'a mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>;

/// What the evergreen pointer said the channel head is, relative to what this ledger
/// last authorized.
enum WebHead {
    /// The pointer names the tag this ledger last authorized: nothing to fetch.
    Unchanged { tag: String },
    /// A tag this ledger has not authorized (or nothing was recorded): the synthesized
    /// candidate whose every asset URL is the derived tag-specific one.
    New(AuthoritativeRelease),
    /// A non-failure end, already recorded: the channel has no published release
    /// (announced, loud), the host asked us to wait (deferred), or the head is not an app
    /// release.
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

/// The candidate for the pointer's `tag`: every asset's URL the derived
/// `…/releases/download/<tag>/<name>` (a pure function of the trusted source and the tag,
/// never a server string), and the canonical version the fetched manifest must equal.
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

/// Record that the channel head is not an app release (no `aterm-appcast.toml`, or a tag
/// this client does not install from) and end the check, healthy. The tag is not recorded
/// as authorized, so the next check reads the head again; nothing looks past it (see the
/// module docs).
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

/// The `why` of [`unconfirmable_stage_note`] when this build's anchor cannot verify the
/// roster.
const STALE_ANCHOR_STAGE_WAIT: &str = "this build can never confirm it";

/// What a check that admitted no roster says about a stage waiting for one: `Some` when the
/// publishable, newer stage waits for roster confirmation at `now` (gate 4d,
/// `Ready::roster_authorization_stale`), naming the wait and `why` this check cannot end it.
///
/// Gate 4d's refusal promises that a check confirms the stage, which only a check that
/// admits the head's roster can. Two channel states admit none: a head that is not an app
/// release ([`record_head_without_app`]), and a roster this build's anchor cannot verify
/// ([`STALE_ANCHOR_KEY`]), where only a reinstall helps (docs/RELEASE-KEYS.md, *Rotating
/// the master*).
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

/// A source-only head (a canonical app tag whose appcast answers 404), recorded as a head
/// with no app manifest yet, and latched as an app build in flight
/// ([`app_build_in_flight`]) when it outranks the last tag authorized for this source, so
/// the loop re-checks on the quick ladder.
fn record_source_only_head(staging: &Staging, current_build: u64, source: &Source, tag: &str) {
    let known = crate::status::authorized_tag(staging, source);
    if app_build_in_flight(tag, known.as_deref()) {
        set_head_in_flight(Some((tag.to_string(), 0)));
    }
    record_head_without_app(staging, current_build, tag);
}

/// One unmetered HEAD and its decision: a redirect to a canonical tag is the head,
/// unchanged when it equals `known_tag` (`status.toml`'s last authorized tag; both are
/// canonical, so string equality suffices); a 404 is the loud standing state; a 429 or 5xx
/// defers; a redirect to a tag this client does not install from is a head with no app
/// release; anything else is a `network`-class failure.
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
        // A refused redirect, an unexpected status, an unsafe source, a transport failure:
        // `network`-class. What the pointer named is never requested.
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
    // The pointer's `Location` and the derived appcast URL are equal by construction
    // (`parse_location`); asserted, because every later GET uses the derived one.
    debug_assert_eq!(candidate.release.assets[0].url, pointer.location);
    Ok(WebHead::New(candidate))
}

/// The URL cross-check: the signed manifest's container `url` must name the tag the pointer
/// chose and the DMG the manifest names. The field is never followed (the derived URL is
/// what is downloaded); this binds the signed bytes to the tag, so an appcast copied onto
/// another tag is refused.
///
/// The repository may be the source's or the compiled-in channel's (case-insensitively):
/// the publisher signs `url` under the public channel's slug, and a repointed mirror
/// carries those appcasts. `pointer::parse_location` already scopes every GET to the
/// source repository.
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

/// Background check and stage. `Some(version)` when a strictly newer verified build is
/// staged and applicable, whether or not this call staged it (the re-stage may be backed
/// off, [`StageBackoff`]): the caller arms the apply lane on it, so the answer is about
/// state, not activity. `None` when nothing newer is available. Errors are transient
/// (network, parse); the caller logs them.
///
/// The running build's version is not a parameter: selection is by release tag and the
/// downgrade gate compares build numbers. A dev build (`0.5.0+g<sha>`) and the release it
/// should install compare equal on the version triple, and build metadata is unordered
/// (`VERSIONING.md`).
pub fn check_and_stage(current_build: u64, source: &Source) -> Result<Option<String>, String> {
    check_and_stage_for(current_build, source, Asked::Schedule)
}

/// Who asked for a check: the background schedule, or a person (Check for Updates,
/// `aterm update check`, `aterm ctl update check`, all through [`crate::check_now`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    Schedule,
    Person,
}

/// [`check_and_stage`], asked for by `asked`. A person's check passes a moment's
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
    // Every exit writes status, failures included: else `status.toml` keeps the last
    // healthy outcome while every check fails.
    if let Err(error) = &result
        && let Some(staging) = Staging::resolve()
    {
        crate::status::record(
            &staging,
            current_build,
            &format!("update check failed: {error}"),
        );
    }
    // A check that began a download put a bar on the host's screen: a failure takes it
    // down here (the other ends report inline). A check that never downloaded owes nothing.
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

/// Stamp the check receipt when the check owes one ([`check_leaves_a_receipt`]), and answer
/// `found`, the head whose app build it found in flight, with the machine-wide count of
/// consecutive checks that found it so, this one included. A deferral counts none (it keeps
/// its own window, not the quick ladder); a failed check leaves the receipt untouched,
/// neither counting nor ending the run.
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

/// Whether the check that just ended owes the shared check receipt a stamp: only one that
/// reached the channel. Readers take the receipt for a completed check (`checker_skip_for`
/// skips the interval machine-wide on it; `last_check_at` and `checked_at` advertise it),
/// so a failure stamps nothing and is bounded by its own process's backoff
/// (`cadence::Cadence::failed`). A deferral (a 429 or 5xx) stamps, so every sibling's
/// window widens and the whole machine retreats.
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
    // The recorded tag is trusted only when the ledger still describes this build's
    // verdict on this source (`status::latest_tag`).
    let known_tag = crate::status::latest_tag(staging, current_build, source);
    match resolve_web_head(staging, current_build, source, known_tag.as_deref(), head)? {
        WebHead::Ended => Ok(Acquisition::Ended),
        // The head has not moved, but the staged build's roster confirmation is due (gate
        // 4d holds it past a day): judge the head again, roster and all, so the floor's
        // observation confirms or retires the stage. A few small GETs about hourly while
        // a newer stage waits.
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

/// Record what this check observed in the durable floor, withdraw a published stage the
/// floor no longer admits, and answer the floor the rest of the check judges by (the one
/// just written: a revocation may have lowered it). `Err` (passing) when the floor could
/// not be read back; a stage the revocations in hand withdraw is still retired first.
///
/// When this check admitted a roster (`observation.roster_seq` nonzero) and nothing on disk
/// is withdrawn, the stage's [`Ready::roster_confirmed_at`] moves to `roster_read_at`
/// (RFC3339, when this check began fetching that roster), which the swap's gate 4d waits
/// for ([`crate::install::confirm_roster_authorization`]).
///
/// The `Check` action of `NativeUpdateRosterFreshness`; Tier-1:
/// `tests::roster_freshness_conformance`.
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
    // Record the release's operator floor and roster sequence now, whether or not this
    // check stages: the ratchet follows observation, not acceptance (a roster that revokes
    // the release's signer is observed, then refuses). It also records what the roster
    // revokes, which takes back every floor the revoked machines raised (a stolen key's
    // huge `min_build` or staged build).
    crate::manifest::Floor::observe_and_write(&staging.floor(), observation);
    // Judge by the floor just written: a revocation may have lowered it.
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
    // A stage the floor no longer admits is withdrawn here: its signer is now revoked, or
    // it was yanked (`min_build`). Decided here and not only in the apply lane, because
    // revocation is a list in the roster document, which only this check holds; the apply
    // lane holds the floor and its durable revocation set, recorded just above. A
    // roster-generation comparison there would be wrong: a manifest attributed under an
    // older generation is the ordinary post-join state (`install`, gate 4c). The match is
    // on `machine_id`, inside the signed bytes. The stage is read without the floor
    // ([`Ready::read_published_bundle`]): the one the floor hides is the one to find.
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
        // Under the apply lock, like every retirement of the published stage: a launching
        // instance's `apply_staged_if_ready` renames the staged `.app` into place under it,
        // and a delete racing that rename would gut the live install. Nothing else is held
        // here (the stage lock comes later); a boot apply that wins the lock retires the
        // stage at its own gates (4b, 4c), and the re-read below finds nothing.
        // Bounded: this is the checker thread. The next check retries.
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
                    // Carried onto every later record of this check: `status.toml` is one
                    // overwritten line.
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
    // A roster this check admitted whose revocations (and the floor's) leave the stage
    // standing confirms it for another day. An unreadable floor, or a check that admitted
    // no roster, confirms nothing.
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

/// Project one observed stage onto the bounded `NativeUpdateRosterFreshness` machine: ticks
/// of half the window ([`crate::manifest::ROSTER_FRESHNESS_SECS`]), the tick its
/// confirmation was read at (`confirmed`), when its signer's revocation was published
/// (`revoked_at`, `None`: not yet), whether its marker and bundle are on disk, and whether a
/// lane installed it and how late (`swapped`: `None` not yet, `Some(late)` once).
///
/// Proof/test builds only. Every input is an observation of the real staging root and gate.
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

/// The check's snapshot of the floor, or the passing refusal that ends the check
/// ([`crate::manifest::Floor::read_for_gate`]), booked `pipeline` class: a floor that stays
/// unreadable (root-owned after a `sudo` run, a broken ACL, a failing disk) ends every check
/// here, and a streak of [`crate::PERSISTENT_AFTER`] raises the persistent notice.
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
    // Injected through `acquire` so a test can count the requests.
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
    // Reap abandoned copy attempts, which staging and cross-volume apply leave on their
    // destination volume.
    install::reap_abandoned_copy_attempts(&staging.staged_dir());
    if let Some(parent) = installed_parent {
        install::reap_abandoned_copy_attempts(parent);
    }
    crate::status::clear_check_note();
    // An apply streak recorded by a different build is stale (the machine moved by some
    // means), so every check expires it — except an uncommitted handoff candidate's, whose
    // check runs before it takes over and would erase the failures its own attempt adds.
    if !crate::is_uncommitted_handoff_candidate() {
        crate::health::Health::expire_stale_apply_streak(&staging.health(), current_build);
    }
    // The persisted recency floor (yank and rollback guard, F5/F6). An unreadable floor
    // ends the check ([`floor_snapshot`]): read as "no floor", a yanked or rolled-back
    // release would be staged.
    let floor = floor_snapshot(&staging)?;

    let authoritative = match acquire(&staging, current_build, source, head)? {
        Acquisition::Proceed(candidate) => candidate,
        Acquisition::Ended => return Ok(None),
        Acquisition::UpToDate { tag } => {
            // The steady state, one request: the pointer names the tag last authorized,
            // so nothing is fetched or re-judged. Healthy: the channel was read.
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
    // The roster tier's inputs. `floor_seq` is a snapshot from before any network I/O;
    // `floor_refresh` re-reads the durable floor at admission
    // ([`RosterPolicy::floor_refresh`]).
    let floor_path = staging.floor();
    // A re-read that failed keeps the snapshot: the max at admission only tightens.
    let floor_refresh = || crate::manifest::Floor::read(&floor_path).roster_seq;
    let roster_policy = RosterPolicy {
        master_pubkeys,
        floor_seq: floor.roster_seq,
        now_unix: aterm_update_core::roster::now_unix(),
        floor_refresh: Some(&floor_refresh),
    };
    // When this check read the roster, taken before the fetch: every stage it confirms or
    // publishes is stamped with this instant, never a later one, since a download can run
    // for hours of window a revocation could hide in. An unreadable clock is empty and
    // stamps nothing (read as unconfirmed).
    let roster_read_at = crate::install::now_rfc3339();
    let mut fetched =
        fetch_authoritative_release(Some(authoritative), &mut download, &roster_policy);
    // A source-only head (its appcast answers 404) is not an app release yet: a healthy
    // end, and the tag is not recorded as authorized, so the next check reads it again.
    if fetched.appcast_missing {
        record_source_only_head(&staging, current_build, source, &web_tag);
        return Ok(None);
    }
    fetched.refuse_unbound_or_revoked(source, &floor, &web_tag);
    // Attribution goes to the status file before the staging decision: who signed a
    // release this machine saw matters whether or not it installs it.
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
            // An asset GET met the host's throttle: a deferral, like a throttled HEAD, and
            // never a `pipeline` streak.
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
            // Fetched but rejected (unsigned, bad signature, unparseable, a version or URL
            // that does not bind to the tag): the release side or an attacker is the
            // problem, booked `manifest`. A stranded client (its anchor cannot verify the
            // channel) is told to reinstall, not to wait for the publisher.
            let reason = rejection_reason
                .as_deref()
                .unwrap_or("manifest(s) fetched but rejected (signature/parse)");
            let h = crate::health::Health::record_failure(&staging.health(), "manifest", reason);
            // A build whose anchor cannot verify the roster can never confirm a waiting
            // stage: say so, so the update row promises no check that cannot come.
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
            // The check ran fine (the head was read, nothing carries a manifest): clear
            // any stale streak.
            crate::health::Health::record_success(&staging.health());
            String::from("no release carries an update manifest")
        };
        crate::status::record(&staging, current_build, &msg);
        return Ok(None);
    };
    // No `record_success` yet: only a terminal healthy outcome records it, so a
    // download-only breakage accrues a streak instead of being reset by its own check.

    // Downgrade gate: never stage an older-or-equal build. Healthy, and it records
    // `latest_tag` so the next check stops at the HEAD.
    //
    // Not gated on the bundle at this path: its build is unverified, and a machine whose
    // newer bundle cannot be verified (mid-notarization, a broken seal, a yank) would stop
    // acquiring the release it can install.
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

    // Recency floors (F5/F6): refuse a build below the operator floor (a yank) or below the
    // high-water (a rollback to an older genuine build). Both holds are healthy ends:
    // everything the check exercised worked, and a machine parked under a yank for days
    // would otherwise accumulate blips into a false persistent notice. `latest_tag` is not
    // recorded: a floor can move under the same tag.
    if let Some(held) = floor_hold(manifest.build_number, effective_min_build, floor.high_water) {
        crate::health::Health::record_success(&staging.health());
        crate::status::record(&staging, current_build, &held);
        return Ok(None);
    }

    // A stage already covers this release: no download. Healthy (the head and manifest
    // were fetched and accepted), so the streaks clear rather than accumulate blips for
    // the life of a pending stage.
    if publishable_stage_covers(&staging, &manifest) {
        crate::health::Health::record_success(&staging.health());
        crate::status::set_latest_tag(&web_tag, source, current_build, manifest.build_number);
        record_covered_stage_status(&staging, current_build, &manifest);
        // `Some`, so a stage a sibling published arms this process's apply lane too: every
        // aterm process runs this checker.
        return Ok(Ready::read_publishable(&staging).map(|ready| ready.version));
    }

    // A build that already failed to stage is not re-downloaded (up to 2 GiB) every
    // interval; a re-publish under another sha256, or any newer build, clears the memo
    // (F17). The memo is keyed on the manifest's `sha256` for both containers. It gates
    // only the re-stage: a published newer stage is still reported ([`StageBackoff`]).
    if let Some(backoff) = stage_backoff(
        &staging,
        &manifest,
        current_build,
        crate::install::unix_now_secs(),
        asked,
    ) {
        // Healthy for the acquisition half: only the memo stopped it. Not `record_success`,
        // which zeroes `stage_failures`: failed checks interleave with backed-off ones, so
        // the stage streak could never reach PERSISTENT_AFTER. `record_acquisition_success`
        // clears the acquisition classes and keeps the stage streak.
        crate::health::Health::record_acquisition_success(&staging.health());
        crate::status::record(
            &staging,
            current_build,
            &backoff.status_line(manifest.build_number),
        );
        // `Some` drives the apply lane (the caller's `on_staged` hook), so a download
        // backoff cannot strand a build that only needs applying.
        return Ok(backoff.applicable.map(|ready| ready.version));
    }

    // Serialize download → extract → publish across processes, which share the download
    // and staged scratch. Separate from the apply lock, so a long download never blocks a
    // starting instance's apply. Bounded; a stopped holder is booked ([`take_stage_lock`]).
    let _stage_lock = take_stage_lock(&staging, crate::install::BACKGROUND_LOCK_WAIT)?;
    // Re-check under the lock: a sibling may have just staged this build. Healthy, and
    // `Some` so this process's apply lane arms too.
    if publishable_stage_covers(&staging, &manifest) {
        crate::health::Health::record_success(&staging.health());
        crate::status::set_latest_tag(&web_tag, source, current_build, manifest.build_number);
        record_covered_stage_status(&staging, current_build, &manifest);
        return Ok(Ready::read_publishable(&staging).map(|ready| ready.version));
    }
    // Re-read the roster floor under the lock: a concurrent instance may have observed a
    // newer generation since this check's admission ([`roster_authority_superseded`]).
    if roster_authority_superseded(
        observed_roster_seq,
        crate::manifest::Floor::read_for_gate(&staging.floor())?.roster_seq,
    ) {
        // Healthy: acquisition ran end to end and a concurrent instance ratcheted the
        // roster generation under us; the next check runs under the advanced floor.
        crate::health::Health::record_success(&staging.health());
        crate::status::record(
            &staging,
            current_build,
            "held: a newer machine-roster generation was recorded during this check; \
             re-checking under it next cycle",
        );
        return Ok(None);
    }

    // The container identity proved at acceptance (no asset lookup after this point), at
    // the URL derived for the pointer's tag; the manifest's own `url` is never followed.
    let asset = &release.assets[artifact.asset_index];
    let container = artifact.container.label();

    let part = staging.download.join(format!("{}.part", artifact.name));
    let container_path = staging.download.join(&artifact.name);
    sweep_download_scratch(&staging);
    // Live progress for a host that shows it: a poller stats the growing `.part`, and a
    // HEAD of the same URL, run beside the download and never before it, gives the total.
    // Joined on drop, so it cannot outlive the download.
    let download_watch = crate::progress::watch_download(&part, &manifest.version, &asset.url);
    // A failed download is `pipeline`-class (the release names the asset), unless this
    // machine could not store it (`book_download_failure`).
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
        // A 429 on the container defers, like one on the manifest.
        if aterm_update_core::download_error_is_rate_limit(&e) {
            let note = format!(
                "update check deferred: GitHub rate limit hit while downloading the \
                 {container} — backing off, will retry on the next check"
            );
            record_deferral(&staging, current_build, &note);
            // The bar opened on the first byte; a deferral closes it (the wrapper reports
            // only `Err`s).
            if crate::progress::take_download_began() {
                crate::progress::report(crate::progress::Progress::Deferred { detail: note });
            }
            return Ok(None);
        }
        return book_download_failure(&staging, current_build, &manifest, container, &e, free);
    }

    // Digest, extract, codesign/Gatekeeper and publish are one "verifying" phase on the
    // live channel.
    crate::progress::report(crate::progress::Progress::Verifying {
        version: manifest.version.clone(),
    });
    // From here failures are `stage`-class: the bytes arrived, so the artifact or the local
    // disk is the problem. (The sha256 below proves the size.)
    if let Err(e) = std::fs::rename(&part, &container_path) {
        let msg = format!("finalize download: {e}");
        crate::health::Health::record_failure(&staging.health(), "stage", &msg);
        return Err(msg);
    }

    // Integrity: SHA-256 must equal the manifest's digest for this container. Every
    // exit here is booked: a machine that can never hash a download must still reach
    // the persistent-failure notice.
    //
    // The hash has a deadline ([`container_sha256`]), and a hash past it or one whose
    // thread the system would not start just then is a moment, booked as one at the one
    // booking point.
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

    // Unpack, verify (codesign/Team ID/spctl), publish the ready marker. A failure reclaims
    // the container and is memoized under the manifest digest, so both containers
    // share one retry budget per build.
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
    // Reclaim the container and clear the backoff memo, never a quarantine verdict, which a
    // success of this build says nothing about.
    let _ = std::fs::remove_file(&container_path);
    crate::manifest::FailedMark::clear_backoff(&staging.failed());
    // The whole pipeline ran: clear every failure streak.
    crate::health::Health::record_success(&staging.health());
    // Raise the high-water to this build (F6), attributed to its signer so a revocation of
    // that machine takes it back (a stolen key's huge build would otherwise hold every
    // genuine release below high-water).
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

    // A record of what the stager did, not advice: the in-session apply lane decides when
    // it applies, and a refused apply overwrites it (`record_apply_refusal`).
    crate::status::record(
        &staging,
        current_build,
        &format!(
            "staged {} (build {}) — verified and ready to apply",
            manifest.version, manifest.build_number
        ),
    );
    // Only a check that downloaded reports its good end to the live channel.
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
