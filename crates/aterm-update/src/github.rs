// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The DMG/.app-specific orchestration of a background update check: find the
//! newest release carrying an `aterm-appcast.toml`, and — if it is strictly newer
//! than the running build — download + verify its DMG and stage it. The portable
//! GitHub plumbing it drives (token-optional `curl` GET/download, the per-machine
//! token chain) lives in `aterm-update-core` (`api_get_classified`/`download_bytes`/
//! `download_to`, [`aterm_update_core::token`]).
//!
//! Requests go through the API (the `releases/latest/download/…` browser shortcut
//! needs web auth even on a public repo) and asset bytes are downloaded via the asset
//! API URL with `Accept: application/octet-stream` (curl `-L` follows the 302 to
//! storage and drops the `Authorization` header on the cross-host redirect by
//! default). When a token IS available it is fed to curl through STDIN
//! (`curl --config -`), never on argv, so it is not exposed to same-user processes
//! via `ps`.
//!
//! # The credential ladder (token-first, anonymous fallback)
//!
//! The token is RESOLVED, never gated on, and the first releases-LIST response is
//! the sole classifier, because a network answer is the only real evidence about
//! whether this machine can read the channel:
//!
//! * a token resolved → use it (5000 requests/hour instead of the shared ~60/hour
//!   per-IP anonymous budget, and a private channel keeps working exactly as before);
//! * no token → ask anonymously anyway;
//! * 401/403 WITH a token → retry once anonymously, so a stale ambient `gh auth
//!   token` cannot brick a machine whose channel is public;
//! * 401/403/404 WITHOUT a token → [`unreadable_explanation`]: loud, actionable, and
//!   never a silent idle;
//! * 429 / rate-limited 403 → back off; not a failure, not a broken pipeline.
//!
//! Dropping auth is NOT a trust downgrade: artifact trust is the pinned Ed25519 key,
//! the pinned Team ID and the manifest sha256 — none of which this lane touches.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use serde::Deserialize;

use aterm_update_core::tag::{TagError, TagKind};
use aterm_update_core::{HttpError, token};

use crate::manifest::{Manifest, Ready};
use crate::{PINNED_UPDATE_PUBKEY, Source, bundle, install, paths::Staging, sig};

/// Which credential lane the last COMPLETED releases-LIST request used. Read by the
/// background loop to pick a check cadence the lane's rate-limit budget can afford
/// (see `crate::spawn_background_check`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// No check has completed yet in this process.
    Unknown,
    /// A token was accepted.
    Authenticated,
    /// The channel answered with no credential at all — a public repo.
    Anonymous,
}

/// [`Lane`] of the last completed check, as a `u8` so it can live in an atomic.
static LANE: AtomicU8 = AtomicU8::new(0);

/// Whether the last check ended in a GitHub rate limit. Read by the background loop
/// to LENGTHEN the next wait without recording a failure: a rate limit means "you
/// asked too often", which is a cadence problem, not a broken updater.
static RATE_LIMITED: AtomicBool = AtomicBool::new(false);

/// Whether this process has already logged that it is updating without a token. Once
/// per process: it is a standing condition, not an event.
static ANNOUNCED_ANONYMOUS: AtomicBool = AtomicBool::new(false);

/// The lane the last completed check used.
#[must_use]
pub fn lane() -> Lane {
    match LANE.load(Ordering::Relaxed) {
        1 => Lane::Authenticated,
        2 => Lane::Anonymous,
        _ => Lane::Unknown,
    }
}

/// Whether the last check was cut short by a GitHub rate limit.
#[must_use]
pub fn rate_limited() -> bool {
    RATE_LIMITED.load(Ordering::Relaxed)
}

/// Record that a releases-LIST request SUCCEEDED on `lane`. This — not the token
/// chain — is what clears the "this machine cannot update" latch: reading the channel
/// is the property that matters, and on a public repo it holds with no credential.
fn note_readable(authenticated: bool, source: &Source) {
    LANE.store(if authenticated { 1 } else { 2 }, Ordering::Relaxed);
    RATE_LIMITED.store(false, Ordering::Relaxed);
    crate::no_token::clear();
    if !authenticated && !ANNOUNCED_ANONYMOUS.swap(true, Ordering::Relaxed) {
        crate::log(&format!(
            "updating from github.com/{}/{} without a token (public channel) — \
             unauthenticated checks share ~60 GitHub requests/hour per IP address, so \
             this machine checks on a longer interval",
            source.owner, source.repo
        ));
    }
}

/// The lane annotation appended to a HEALTHY status outcome, so `status.toml` /
/// `aterm-ctl update status` answers "why is this machine slow to update?" on its own.
///
/// Empty on the authenticated lane (the default, and the one the 75-second cadence
/// documents), so no existing status wording changes for a provisioned machine.
fn lane_note() -> &'static str {
    if lane() == Lane::Anonymous {
        " — checking anonymously on a 15-minute interval (no update token provisioned; \
         the unauthenticated GitHub budget is ~60 requests/hour per IP)"
    } else {
        ""
    }
}

/// What the releases-LIST response says the check should do. Split out as a pure
/// function of the classified error so the whole ladder is unit-testable without a
/// network, a token, or an installed `.app`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListDecision {
    /// Re-issue the SAME request with no token (a resolved token was rejected).
    RetryAnonymous,
    /// This machine cannot read the channel and never will until an operator acts.
    /// Carries the full explanation; NOT a health failure — a configuration state is
    /// not a transient fault, and recording it as one would bury the real signal.
    Blocked(String),
    /// GitHub asked us to slow down. Back off; do not accrue a failure streak.
    RateLimited(String),
    /// A genuine failure: `network`-class ledger entry + `Err`.
    Failed(String),
}

/// Turn the token chain's outcome into the credential a check proceeds WITH.
///
/// TOTAL BY CONSTRUCTION, and that is the entire point: there is no "stop" value it
/// can return. The absence of a token is not evidence that this machine cannot
/// update — aterm's channel is public and readable anonymously — so only a network
/// response may decide that, in [`classify_list_error`].
///
/// The `Err` arm keeps the diagnosis rather than discarding it: it is what
/// `unreadable_explanation` uses to say "a token is present but was refused: …"
/// instead of the misleading "not configured", and what `note_unusable_token` reports
/// on a channel that turned out to be readable anyway.
///
/// Returning `(Option<token>, Option<diagnosis>)` — never a control-flow decision —
/// is what keeps the gate from creeping back in: reintroducing one means changing
/// this function's type, not quietly adding a branch.
fn plan_credential(
    resolved: Result<(String, &'static str), token::Diagnosis>,
) -> (Option<String>, Option<token::Diagnosis>) {
    match resolved {
        Ok((tok, _source)) => (Some(tok), None),
        Err(diagnosis) => (None, Some(diagnosis)),
    }
}

/// Classify a failed releases-LIST request.
///
/// `had_token` is whether the TOKEN CHAIN produced one at the start of this check —
/// deliberately not "did this particular request carry one", so the anonymous retry's
/// own failure is still reported as the auth problem it is, rather than being
/// mistaken for an unprovisioned machine.
pub(crate) fn classify_list_error(
    error: &HttpError,
    had_token: bool,
    already_retried: bool,
    source: &Source,
    diagnosis: Option<&token::Diagnosis>,
) -> ListDecision {
    match error {
        HttpError::RateLimited { .. } => ListDecision::RateLimited(error.to_string()),
        // GitHub answers 404 for a private repo an anonymous caller cannot see AND
        // for a repo that does not exist — `GET /repos/{o}/{r}` is 404 in both cases
        // too, so NO probe distinguishes them. Do not guess: name all three causes.
        HttpError::NotFound { .. } | HttpError::Unauthorized { .. } if !had_token => {
            ListDecision::Blocked(unreadable_explanation(error, source, diagnosis))
        }
        // A resolved token that GitHub rejects must not brick a machine whose channel
        // is public: the ambient `gh auth token` the chain may have picked up can be
        // stale, scoped elsewhere, or revoked. One anonymous retry, then give up.
        HttpError::Unauthorized { .. } if !already_retried => ListDecision::RetryAnonymous,
        // Everything else — including 404 WITH a token (a token that cannot see the
        // repo is a real, actionable auth problem) — is today's failure path.
        _ => ListDecision::Failed(error.to_string()),
    }
}

/// The one message an operator gets when this machine cannot read its release
/// channel and has no credential to try. It must survive being read months later out
/// of `status.toml`, so it names the consequence, every possible cause, and the exact
/// remedy for each.
fn unreadable_explanation(
    error: &HttpError,
    source: &Source,
    diagnosis: Option<&token::Diagnosis>,
) -> String {
    let code = match error {
        HttpError::Unauthorized { code } => *code,
        HttpError::NotFound { .. } => 404,
        _ => 0,
    };
    // A token that was PRESENT and refused by our own chain ("chmod 600 it") is the
    // actionable case and must never be collapsed into "not configured".
    let rejections = diagnosis
        .map(token::Diagnosis::rejections)
        .unwrap_or_default();
    let chain = if rejections.is_empty() {
        "no update token is provisioned".to_string()
    } else {
        format!(
            "a token is present but was refused: {}",
            rejections.join("; ")
        )
    };
    format!(
        "aterm cannot read its release channel github.com/{}/{} (HTTP {code}) and {chain}, \
         so this machine will NEVER receive an update until it is fixed. GitHub answers the \
         same way for every cause and cannot tell them apart, so check all three: (1) the \
         channel is PRIVATE — provision a token by running: {}; (2) the repository does not \
         exist; (3) the configured channel is wrong — check `[update] owner`/`repo` in \
         aterm's config, or $ATERM_UPDATE_OWNER / $ATERM_UPDATE_REPO.",
        source.owner,
        source.repo,
        token::PROVISION_COMMAND
    )
}

/// A GitHub Release (subset). Unknown fields are ignored.
#[derive(Clone, Debug, Deserialize)]
struct Release {
    /// Release-list order is not a GitHub REST contract. The canonical tag is
    /// therefore the updater's explicit ordering key.
    tag_name: String,
    /// Draft (unpublished) releases are visible to a write-capable token but must
    /// never be staged to the fleet — skip them (F12).
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// A release asset (subset).
#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    /// The asset's API URL (`…/releases/assets/<id>`), used for the octet download.
    url: String,
    #[serde(default)]
    size: u64,
}

/// Parse the ordering key for one exact-name candidate.
///
/// The grammar is [`aterm_update_core::tag::parse_release_tag`] — the SAME
/// function the publisher (`aterm-release/src/publish.rs`) classifies with, so
/// client and publisher cannot disagree about which releases are candidates.
/// Only the client's diagnostic wording is here: canonical three-component
/// `vMAJOR.MINOR.PATCH` is a candidate, two-component tags are the retired
/// scheme ([`TagKind::Legacy`]), anything else fails the check closed rather
/// than silently narrowing the candidate set.
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

struct AuthoritativeRelease {
    tag: Vec<u64>,
    version: String,
    release: Release,
    manifest_index: usize,
    signature_index: Option<usize>,
}

/// Select the authoritative exact-name appcast without trusting REST response
/// order. Every page must already have been collected before this runs.
fn select_authoritative_release(
    releases: Vec<Release>,
    pinned_update_pubkey: &str,
) -> Result<Option<AuthoritativeRelease>, String> {
    let mut seen_tags = std::collections::BTreeSet::new();
    let mut selected: Option<AuthoritativeRelease> = None;

    for release in releases {
        if release.draft {
            continue;
        }
        let Some(manifest_index) = unique_asset_index(&release, "aterm-appcast.toml")? else {
            continue;
        };
        // Retired-scheme releases stay published but are never installed. Skipping
        // (rather than erroring) is what lets the pre-cut-over archive coexist with
        // the current channel.
        let TagKind::Candidate(tag) = parse_numeric_tag(&release.tag_name)? else {
            continue;
        };
        if !seen_tags.insert(tag.clone()) {
            return Err(format!(
                "duplicate published update candidates use numeric order {}",
                release.tag_name
            ));
        }
        let candidate = AuthoritativeRelease {
            tag,
            // Losing candidates need no canonical version. The selected numeric
            // maximum is validated after the complete metadata pass.
            version: String::new(),
            release,
            manifest_index,
            signature_index: None,
        };
        if selected
            .as_ref()
            .is_none_or(|current| candidate.tag > current.tag)
        {
            selected = Some(candidate);
        }
    }

    if let Some(candidate) = &mut selected {
        candidate.version =
            canonical_authority_version(&candidate.release.tag_name, &candidate.tag)?;
        if pinned_update_pubkey.is_empty() {
            return Ok(selected);
        }
        candidate.signature_index = Some(
            unique_asset_index(&candidate.release, "aterm-appcast.toml.sig")?.ok_or_else(|| {
                format!(
                    "authoritative update {} is unsigned under the pinned channel",
                    candidate.release.tag_name
                )
            })?,
        );
    }
    Ok(selected)
}

#[derive(Default)]
struct AuthoritativeFetch {
    /// Manifest, its release, and the already-proved unique canonical container
    /// (zip when the manifest carries a resolvable one, else the DMG).
    selected: Option<(Manifest, Release, StageArtifact)>,
    appcast_fetch_error: bool,
    manifest_rejected: bool,
    /// Candidate-manifest fetches only. Detached-signature downloads are a
    /// subordinate verification step and intentionally do not increment this.
    #[cfg(test)]
    manifest_fetch_attempts: u32,
    /// Detached-signature transport attempts, exposed only for Tier-1
    /// projection onto the bounded channel-scan model.
    #[cfg(test)]
    signature_fetch_attempts: u32,
}

/// Fetch and validate exactly one candidate after the complete metadata pass.
/// Older appcasts are never downloaded, regardless of REST row order.
fn fetch_authoritative_release(
    candidate: Option<AuthoritativeRelease>,
    pinned_update_pubkey: &str,
    download: &mut impl FnMut(&str, u64) -> Result<Vec<u8>, String>,
) -> AuthoritativeFetch {
    let mut fetched = AuthoritativeFetch::default();
    let Some(candidate) = candidate else {
        return fetched;
    };
    let manifest_url = candidate.release.assets[candidate.manifest_index]
        .url
        .clone();
    let signature_url = candidate
        .signature_index
        .map(|index| candidate.release.assets[index].url.clone());

    #[cfg(test)]
    {
        fetched.manifest_fetch_attempts = 1;
    }
    let bytes = match download(&manifest_url, 5_000_000) {
        Ok(bytes) => bytes,
        Err(error) => {
            crate::warn(&format!("fetch appcast: {error}"));
            fetched.appcast_fetch_error = true;
            return fetched;
        }
    };
    if let Some(signature_url) = signature_url {
        #[cfg(test)]
        {
            fetched.signature_fetch_attempts = 1;
        }
        let sigbytes = match download(&signature_url, 4096) {
            Ok(bytes) => bytes,
            Err(error) => {
                crate::warn(&format!("fetch appcast signature: {error}"));
                fetched.appcast_fetch_error = true;
                return fetched;
            }
        };
        if let Err(error) = sig::verify_detached(pinned_update_pubkey, &bytes, &sigbytes) {
            crate::warn(&format!(
                "release manifest signature did not verify ({error:?}); refusing authoritative {}",
                candidate.release.tag_name
            ));
            fetched.manifest_rejected = true;
            return fetched;
        }
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            crate::warn(&format!(
                "authoritative {} appcast is not UTF-8: {error}",
                candidate.release.tag_name
            ));
            fetched.manifest_rejected = true;
            return fetched;
        }
    };
    match Manifest::parse(&text) {
        Ok(manifest) if manifest.version == candidate.version => {
            match select_stage_artifact(&candidate.release, &manifest, &candidate.version) {
                Ok(artifact) => {
                    fetched.selected = Some((manifest, candidate.release, artifact));
                }
                Err(error) => {
                    crate::warn(&error);
                    fetched.manifest_rejected = true;
                }
            }
        }
        Ok(manifest) => {
            crate::warn(&format!(
                "authoritative {} carries manifest version {:?}, expected {:?}",
                candidate.release.tag_name, manifest.version, candidate.version
            ));
            fetched.manifest_rejected = true;
        }
        Err(error) => {
            crate::warn(&format!(
                "parse authoritative {} appcast: {error}",
                candidate.release.tag_name
            ));
            fetched.manifest_rejected = true;
        }
    }
    fetched
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
    retry_in_secs: u64,
    /// Consecutive stage failures recorded for that candidate (at least 1).
    attempts: u32,
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
        let restage = format!(
            "skipping re-stage of build {candidate_build} for another {}m (failed to stage \
             {} time(s); retrying automatically, or re-publish to retry now)",
            self.retry_in_secs.div_ceil(60),
            self.attempts
        );
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
        applicable: Ready::read_publishable(staging)
            .filter(|ready| ready.build_number > current_build),
    })
}

/// Enumerate the complete bounded release-metadata set. GitHub documents no
/// ordering contract for List Releases, so the caller chooses the greatest
/// canonical numeric vMAJOR.MINOR.PATCH tag carrying the exact appcast name
/// (retired two-component tags are skipped, never ordered), and only after that
/// decision fetches one manifest (+ one signature under Tier SIG) — row order
/// cannot select an older release and broken historical assets add no download
/// latency.
///
/// Runs the credential ladder: `tok` is cleared in place when a rejected token
/// falls back to anonymous, so the caller's later asset fetches ride the same
/// lane. Returns `Ok(None)` for the two non-failure ends of a check — channel
/// unreadable (announced) or rate limited (status recorded); `Err` is a real
/// failure, already recorded in the health ledger.
fn fetch_release_catalog(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    tok: &mut Option<String>,
    diagnosis: Option<token::Diagnosis>,
) -> Result<Option<Vec<Release>>, String> {
    let had_token = tok.is_some();
    const PER_PAGE: u32 = 100;
    const MAX_PAGES: u32 = 10;
    let mut release_catalog = Vec::new();
    // At most one anonymous retry per check, and only after a token was REJECTED.
    let mut already_retried = false;
    for page in 1..=MAX_PAGES {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases?per_page={PER_PAGE}&page={page}",
            source.owner, source.repo
        );
        // A failed releases LIST is `network`-class: GitHub unreachable / auth broken.
        // (The transient/persistent distinction the ledger needs lives in the CLASS
        // split — an asset that provably exists but can't be fetched is `pipeline`,
        // recorded below — so a broken download build can't hide behind "transient".)
        // The two states that are NOT failures — "cannot read the channel at all" and
        // "rate limited" — leave the ledger alone and say so instead.
        //
        // The inner loop runs at most twice: `already_retried` latches, so
        // `RetryAnonymous` can be taken only once for the whole check.
        let body = loop {
            let error = match aterm_update_core::api_get_classified(&url, tok.as_deref()) {
                Ok(body) => {
                    note_readable(tok.is_some(), source);
                    // A token that our own chain refused (a chmod 644 file, a mangled
                    // paste) still costs this machine the 5000/hour budget even though
                    // the public channel works. Say so, throttled, once in a while.
                    if let Some(diagnosis) = diagnosis.as_ref() {
                        crate::no_token::note_unusable_token(diagnosis);
                    }
                    break body;
                }
                Err(error) => error,
            };
            let decision = classify_list_error(
                &error,
                had_token,
                already_retried,
                source,
                diagnosis.as_ref(),
            );
            match decision {
                ListDecision::RetryAnonymous => {
                    already_retried = true;
                    *tok = None;
                    // Throttled: this is a STANDING condition (a stale token stays
                    // stale), re-observed on every check, so an unthrottled warning
                    // would be ~48 identical lines an hour.
                    crate::no_token::note_rejected_credential(&format!(
                        "the configured update token was rejected ({error}); continuing \
                         unauthenticated against github.com/{}/{} — updates still work \
                         while the channel is public, but rotate the token with: {}",
                        source.owner,
                        source.repo,
                        token::PROVISION_COMMAND
                    ));
                }
                ListDecision::Blocked(explanation) => {
                    crate::no_token::announce_unreadable(staging, current_build, &explanation);
                    return Ok(None);
                }
                ListDecision::RateLimited(message) => {
                    // Deliberately no `record_failure`: a rate limit is not a broken
                    // pipeline, and letting it accrue a streak would fire the "update
                    // pipeline is likely broken" notification at a healthy machine
                    // that simply checked too often. The latch lengthens the wait.
                    RATE_LIMITED.store(true, Ordering::Relaxed);
                    crate::status::record(
                        staging,
                        current_build,
                        &format!("update check deferred: {message}"),
                    );
                    return Ok(None);
                }
                ListDecision::Failed(message) => {
                    crate::health::Health::record_failure(&staging.health(), "network", &message);
                    return Err(message);
                }
            }
        };
        // Unparseable list JSON is the same `network` class (the LIST layer failed —
        // a proxy/portal mangling the response looks exactly like this).
        let releases: Vec<Release> = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("parse releases JSON: {e}");
                crate::health::Health::record_failure(&staging.health(), "network", &msg);
                return Err(msg);
            }
        };
        let page_len = releases.len();
        release_catalog.extend(releases);
        if page_len < PER_PAGE as usize {
            break;
        }
        if page == MAX_PAGES {
            let msg = format!(
                "release listing reached the {MAX_PAGES}-page safety cap before exhaustion"
            );
            crate::health::Health::record_failure(&staging.health(), "network", &msg);
            return Err(msg);
        }
    }
    Ok(Some(release_catalog))
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
    // Only stage for a real installed bundle (a dev build has nothing to swap).
    if bundle::resolve().is_none() {
        return Ok(None);
    }
    let staging = Staging::resolve().ok_or("could not resolve Updates dir")?;
    // The Application Support dir is the Updates dir's parent.
    let support = staging.root.parent().ok_or("no support dir")?.to_path_buf();
    // ONE walk of the token chain: the token, or the diagnosis explaining why there
    // isn't one. Resolving and then separately diagnosing would re-spawn `security`
    // and `gh` on every check of an unprovisioned machine.
    //
    // RESOLVE, DO NOT GATE: the absence of a token may never end a check here — only
    // a network response may declare this machine unable to update (`plan_credential`,
    // `classify_list_error`).
    let (mut tok, diagnosis) = plan_credential(token::resolve_or_diagnose(&support, &source.owner, &source.repo));

    // Persisted monotonic recency floor (operator yank + rollback guard, F5/F6).
    let floor = crate::manifest::Floor::read(&staging.floor());

    // List first, decide after: [`fetch_release_catalog`] documents the ordering
    // contract and the credential ladder (it may clear `tok` in place).
    let Some(release_catalog) =
        fetch_release_catalog(&staging, current_build, source, &mut tok, diagnosis)?
    else {
        return Ok(None);
    };

    let authoritative = match select_authoritative_release(release_catalog, PINNED_UPDATE_PUBKEY) {
        Ok(candidate) => candidate,
        Err(error) => {
            crate::warn(&error);
            crate::health::Health::record_failure(&staging.health(), "manifest", &error);
            crate::status::record(
                &staging,
                current_build,
                &format!("update check deferred: {error}"),
            );
            return Ok(None);
        }
    };
    // The asset fetches ride the SAME lane the list request settled on: if the token
    // was rejected above, `tok` is already `None` and these go anonymous too.
    let mut download = |url: &str, max_bytes: u64| {
        aterm_update_core::download_bytes(url, tok.as_deref(), max_bytes)
    };
    let fetched = fetch_authoritative_release(authoritative, PINNED_UPDATE_PUBKEY, &mut download);
    let appcast_fetch_error = fetched.appcast_fetch_error;
    let manifest_rejected = fetched.manifest_rejected;
    let best = fetched.selected;
    let seen_min_build = best
        .as_ref()
        .and_then(|(manifest, _, _)| manifest.min_build)
        .unwrap_or(0);

    // Remember the authoritative release's operator floor immediately (even if we do
    // not stage). The persisted floor remains monotonic across checks.
    crate::manifest::Floor::bump_and_write(&staging.floor(), seen_min_build, 0);
    let effective_min_build = floor.min_build.max(seen_min_build);

    let Some((manifest, release, artifact)) = best else {
        let msg = if appcast_fetch_error {
            // Manifests exist but could not be downloaded while the releases list
            // succeeded — a `pipeline`-class failure. The ledger decides the honest
            // wording: a streak ≥ PERSISTENT_AFTER is not called "deferred".
            let h = crate::health::Health::record_failure(
                &staging.health(),
                "pipeline",
                "release manifests exist but could not be fetched",
            );
            if h.is_persistent() {
                format!(
                    "FAILING ({} consecutive checks since {}): release manifests exist \
                     but cannot be downloaded — this build's download pipeline is \
                     likely broken",
                    h.pipeline_failures, h.failing_since
                )
            } else {
                format!(
                    "update check deferred: a release manifest could not be fetched \
                     (attempt {} — will retry)",
                    h.pipeline_failures
                )
            }
        } else if manifest_rejected {
            // Manifests were FETCHED but rejected (unsigned / bad signature /
            // unparseable): the pipeline works; the release side (or an attacker)
            // is the problem. Its own class — it must not clear a streak.
            crate::health::Health::record_failure(
                &staging.health(),
                "manifest",
                "manifest(s) fetched but rejected (signature/parse)",
            );
            "no stageable release: manifest(s) fetched but rejected (signature/parse)".to_string()
        } else {
            // The check itself ran fine (list fetched, nothing carries a manifest):
            // clear any stale failure streak so health reflects THIS check.
            crate::health::Health::record_success(&staging.health());
            format!("no release carries an update manifest{}", lane_note())
        };
        crate::status::record(&staging, current_build, &msg);
        return Ok(None);
    };
    // NOTE: no `record_success` yet — the container download/verify/stage below is
    // still part of this check's pipeline. Success is recorded only at the terminal
    // healthy outcomes ("up to date" / "staged"), so a download-only breakage ACCRUES
    // a streak instead of being reset every cycle by its own check's manifest fetch.

    // Downgrade gate: never stage an older-or-equal build. A terminal healthy
    // outcome — the whole pipeline this check exercised worked.
    if manifest.build_number <= current_build {
        crate::health::Health::record_success(&staging.health());
        crate::status::record(
            &staging,
            current_build,
            &format!(
                "up to date (latest release build {}){}",
                manifest.build_number,
                lane_note()
            ),
        );
        return Ok(None);
    }

    // Recency floors (F5/F6): refuse a genuine build below the operator floor (yank),
    // or below our high-water (an attacker re-pointing the newest release at an older
    // genuine build cannot roll a client that has already advanced back down).
    if manifest.build_number < effective_min_build {
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

    // If a newer build is already staged, don't re-download it.
    if publishable_stage_covers(&staging, &manifest) {
        return Ok(None);
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
    if publishable_stage_covers(&staging, &manifest) {
        return Ok(None);
    }

    // Download the exact unique same-release container identity already proven
    // while accepting the authoritative manifest — the zip when the release
    // carries one, else the DMG. No order-dependent asset lookup is permitted
    // after this point.
    let asset = &release.assets[artifact.asset_index];
    let container = artifact.container.label();

    let part = staging.download.join(format!("{}.part", artifact.name));
    let container_path = staging.download.join(&artifact.name);
    let _ = std::fs::remove_file(&part);
    // A failed download is a `pipeline`-class ledger entry: the asset provably
    // exists (the release names it) but could not be fetched.
    if let Err(e) = aterm_update_core::download_to(&asset.url, tok.as_deref(), &part, 536_870_912) {
        let _ = std::fs::remove_file(&part);
        crate::health::Health::record_failure(
            &staging.health(),
            "pipeline",
            &format!("{container} download failed: {e}"),
        );
        return Err(format!("{container} download failed: {e}"));
    }

    // Size sanity (when the API reported one), then atomically name it final. From
    // here failures are `stage`-class in the health ledger: the bytes ARRIVED; the
    // artifact (or local disk) is the problem, not the download pipeline.
    if asset.size != 0 {
        let got = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if got != asset.size {
            let _ = std::fs::remove_file(&part);
            let msg = format!(
                "{container} size mismatch: got {got} bytes, expected {}",
                asset.size
            );
            crate::health::Health::record_failure(&staging.health(), "stage", &msg);
            return Err(msg);
        }
    }
    if let Err(e) = std::fs::rename(&part, &container_path) {
        let msg = format!("finalize download: {e}");
        crate::health::Health::record_failure(&staging.health(), "stage", &msg);
        return Err(msg);
    }

    // Integrity: SHA-256 must equal the manifest's digest FOR THIS CONTAINER.
    let got = aterm_update_core::sha256_file(&container_path)?;
    if !got.eq_ignore_ascii_case(&artifact.sha256) {
        let _ = std::fs::remove_file(&container_path);
        let msg = format!(
            "{container} sha256 mismatch: got {got}, manifest {}",
            artifact.sha256
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
    // DMG, verify, stage) successfully — clear every failure streak.
    crate::health::Health::record_success(&staging.health());
    // Raise the high-water to the build we just staged (never lowered): a later attempt
    // to roll us back below it is refused above (F6).
    crate::manifest::Floor::bump_and_write(&staging.floor(), 0, manifest.build_number);

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
    Ok(Some(manifest.version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const SIGNING_SEED: [u8; 32] = [19u8; 32];

    fn test_source() -> Source {
        Source {
            owner: "alabsystems".into(),
            repo: "aterm".into(),
        }
    }

    fn unprovisioned() -> token::Diagnosis {
        use aterm_update_core::token::{ProbeOutcome, SourceProbe};
        token::Diagnosis {
            resolved: None,
            probes: vec![SourceProbe {
                source: "$ATERM_UPDATE_TOKEN",
                outcome: ProbeOutcome::Absent,
            }],
        }
    }

    fn not_found() -> HttpError {
        HttpError::NotFound {
            url: "https://api.github.com/repos/alabsystems/aterm/releases".into(),
        }
    }

    /// A machine with no token, against a channel it cannot read, must produce a
    /// Blocked decision whose text names the consequence, all three indistinguishable
    /// causes, and the exact remedy for each — never a bare "idle", which an operator
    /// cannot tell apart from "no updates available".
    #[test]
    fn an_unreadable_channel_without_a_token_is_loud_and_actionable_not_idle() {
        let source = test_source();
        for error in [
            not_found(),
            HttpError::Unauthorized { code: 401 },
            HttpError::Unauthorized { code: 403 },
        ] {
            let ListDecision::Blocked(text) =
                classify_list_error(&error, false, false, &source, Some(&unprovisioned()))
            else {
                panic!("{error:?} with no token must block, not idle or fail");
            };
            // The consequence, in the words the status/notification surfaces assert.
            assert!(text.contains("NEVER receive an update"), "{text}");
            // The channel, so an operator can see WHICH repo was unreadable.
            assert!(text.contains("github.com/alabsystems/aterm"), "{text}");
            // All three causes: GitHub answers identically for every one of them, so
            // guessing (and naming only "private repo") sends operators down the
            // wrong path when the real fault is a typo'd owner/repo.
            assert!(text.contains("PRIVATE"), "cause 1 missing: {text}");
            assert!(text.contains("does not exist"), "cause 2 missing: {text}");
            assert!(
                text.contains("ATERM_UPDATE_OWNER"),
                "cause 3 missing: {text}"
            );
            // The copy-pasteable fix for cause 1.
            assert!(text.contains(token::PROVISION_COMMAND), "{text}");
        }

        // A token the CHAIN refused (chmod 644) is the actionable sub-case and must
        // be named rather than folded into "not configured".
        use aterm_update_core::token::{ProbeOutcome, SourceProbe};
        let refused = token::Diagnosis {
            resolved: None,
            probes: vec![SourceProbe {
                source: "0600 update-token file",
                outcome: ProbeOutcome::Rejected("chmod 600 it"),
            }],
        };
        let ListDecision::Blocked(text) =
            classify_list_error(&not_found(), false, false, &source, Some(&refused))
        else {
            panic!("must block");
        };
        assert!(
            text.contains("0600 update-token file (chmod 600 it)"),
            "the refused source must be named: {text}"
        );
    }

    /// A missing token must not stop a check before the network. `plan_credential` is
    /// deliberately total — it has no value that means "stop" — so the only way to
    /// reintroduce a gate is to change its type, which this test pins.
    ///
    /// The residual gap this test does NOT close: a hand-written `return` added
    /// directly inside `check_and_stage` still slips past every automated test here,
    /// because `check_and_stage` resolves its own staging dir and network and cannot
    /// be driven from a unit test. Closing that needs the fetch/staging seams to be
    /// injectable.
    #[test]
    fn a_missing_token_never_stops_a_check_before_the_network() {
        // The unprovisioned machine: the chain found nothing.
        let diagnosis = unprovisioned();
        let (tok, carried) = plan_credential(Err(diagnosis));
        assert!(tok.is_none(), "no token was resolvable");
        assert!(
            carried.is_some(),
            "the diagnosis must survive so the channel-unreadable explanation can name \
             WHY there is no token, instead of the misleading 'not configured'"
        );
        // …and the check proceeds: the ONLY thing that may now declare this machine
        // unable to update is a network response.
        assert_eq!(
            classify_list_error(
                &HttpError::RateLimited {
                    code: 429,
                    url: "u".into(),
                    authenticated: false
                },
                false,
                false,
                &test_source(),
                carried.as_ref(),
            ),
            ListDecision::RateLimited(
                "GitHub rate limit hit (HTTP 429) for u; the unauthenticated API allows ~60 \
                 requests/hour per IP address — backing off, will retry on the next check"
                    .to_string()
            ),
            "an anonymous check must reach — and be judged by — the network"
        );

        // A resolved token still flows through unchanged, with no diagnosis.
        let (tok, carried) = plan_credential(Ok(("ghp_x".to_string(), "$ATERM_UPDATE_TOKEN")));
        assert_eq!(tok.as_deref(), Some("ghp_x"));
        assert!(carried.is_none());
    }

    /// A rate limit is not a broken pipeline and not an auth failure. It must reach
    /// the caller as its own decision so the check backs off WITHOUT recording a
    /// health failure — a streak there fires the "your update pipeline is likely
    /// broken" notification at a machine that is merely checking too often.
    #[test]
    fn a_rate_limit_backs_off_and_is_never_an_auth_or_pipeline_failure() {
        let source = test_source();
        let url = "https://api.github.com/repos/alabsystems/aterm/releases".to_string();
        for (had_token, authenticated) in [(true, true), (false, false)] {
            let error = HttpError::RateLimited {
                code: 429,
                url: url.clone(),
                authenticated,
            };
            let decision =
                classify_list_error(&error, had_token, false, &source, Some(&unprovisioned()));
            let ListDecision::RateLimited(text) = decision else {
                panic!("a rate limit must classify as RateLimited, got {decision:?}");
            };
            assert!(
                !text.contains("rotate") && !text.contains("NEVER receive an update"),
                "a rate limit must not read as a revoked token or a stranded machine: {text}"
            );
            assert!(text.contains("backing off"), "{text}");
        }
        // The anonymous lane's advice has to name the budget that was actually hit —
        // ~60/hour per IP, shared by every machine behind one NAT.
        let anon = HttpError::RateLimited {
            code: 403,
            url,
            authenticated: false,
        };
        let ListDecision::RateLimited(text) =
            classify_list_error(&anon, false, false, &source, None)
        else {
            panic!("must be RateLimited");
        };
        assert!(text.contains("~60 requests/hour per IP"), "{text}");
    }

    /// A resolved token that GitHub rejects gets exactly ONE anonymous retry: a
    /// stale ambient `gh auth token` must not brick a machine whose channel is
    /// public. The retry is bounded — after it, the failure is reported as the auth
    /// problem it is, and is NEVER mistaken for an unprovisioned machine (which would
    /// tell the operator to provision a token they already have).
    #[test]
    fn a_rejected_token_is_retried_anonymously_exactly_once() {
        let source = test_source();
        for code in [401u16, 403] {
            assert_eq!(
                classify_list_error(
                    &HttpError::Unauthorized { code },
                    true,
                    false,
                    &source,
                    None
                ),
                ListDecision::RetryAnonymous,
                "HTTP {code} with a token must be retried without it"
            );
            // …and after the retry, it is a plain failure with today's wording.
            let ListDecision::Failed(text) =
                classify_list_error(&HttpError::Unauthorized { code }, true, true, &source, None)
            else {
                panic!("the retry must not loop");
            };
            assert!(text.contains("rotate it"), "{text}");
        }
        // A 404 WITH a token is a real, actionable auth problem (the token cannot see
        // the repo) — never a retry, and never the no-token "blocked" wording.
        let ListDecision::Failed(text) =
            classify_list_error(&not_found(), true, false, &source, None)
        else {
            panic!("404 with a token is a failure, not a retry or a strand");
        };
        assert!(text.contains("404"), "{text}");
    }

    /// Nothing about the ladder may turn a machine that HAS a working token into a
    /// blocked one: `Blocked` is reachable only when the chain produced no token at
    /// all, and transport/other statuses stay on today's `network`-class failure path
    /// regardless of lane.
    #[test]
    fn a_machine_with_a_token_is_never_classified_as_unprovisioned() {
        let source = test_source();
        let every_error = [
            not_found(),
            HttpError::Unauthorized { code: 401 },
            HttpError::Unauthorized { code: 403 },
            HttpError::Status {
                code: 500,
                url: "https://api.github.com/x".into(),
            },
            HttpError::Transport("curl GET x failed (exit 6): dns".into()),
            HttpError::Malformed("GitHub API returned HTTP <html> for x".into()),
        ];
        for error in &every_error {
            for already_retried in [false, true] {
                let decision = classify_list_error(error, true, already_retried, &source, None);
                assert!(
                    !matches!(decision, ListDecision::Blocked(_)),
                    "{error:?} (retried={already_retried}) must not read as an \
                     unprovisioned machine: {decision:?}"
                );
            }
        }
        // Transport and unexpected statuses are unchanged on BOTH lanes: they are
        // genuine transient faults, so they keep the `network`-class Err path.
        for had_token in [true, false] {
            for error in [
                HttpError::Transport("curl GET x failed (exit 6): dns".into()),
                HttpError::Status {
                    code: 500,
                    url: "https://api.github.com/x".into(),
                },
            ] {
                assert!(
                    matches!(
                        classify_list_error(&error, had_token, false, &source, None),
                        ListDecision::Failed(_)
                    ),
                    "{error:?} must stay a failure (had_token={had_token})"
                );
            }
        }
    }

    /// The lane latch is what the background loop reads to choose a cadence its
    /// rate-limit budget can afford, and what clears the stranded state. A successful
    /// anonymous read must clear the latch just like an authenticated one: on a
    /// public channel, "we can read it" is the whole property.
    #[test]
    fn a_successful_anonymous_read_establishes_the_lane_and_clears_the_strand() {
        let _serialized = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let source = test_source();
        RATE_LIMITED.store(true, Ordering::Relaxed);
        note_readable(false, &source);
        assert_eq!(lane(), Lane::Anonymous);
        assert!(
            !rate_limited(),
            "a completed read clears the rate-limit backoff latch"
        );
        assert!(!crate::no_token::is_stranded());
        // …and the healthy status says WHICH lane, so `aterm-ctl update status` can
        // answer "why is this Mac slow to update?" without anyone reading the log.
        let note = lane_note();
        assert!(
            note.contains("anonymously") && note.contains("15-minute"),
            "{note}"
        );
        assert!(
            note.contains("no update token provisioned"),
            "the healthy-but-slow status must name the remediable cause: {note}"
        );

        note_readable(true, &source);
        assert_eq!(lane(), Lane::Authenticated);
        assert_eq!(
            lane_note(),
            "",
            "a provisioned machine's existing status wording must not change"
        );
    }

    fn test_staging(label: &str) -> Staging {
        static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aterm-github-stage-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("download")).unwrap();
        Staging {
            apply_lock: root.join("apply.lock"),
            stage_lock: root.join("stage.lock"),
            download: root.join("download"),
            staged_app: root.join("staged/aterm.app"),
            ready: root.join("ready.toml"),
            status: root.join("status.toml"),
            root,
        }
    }

    fn candidate_manifest() -> Manifest {
        Manifest {
            schema: 1,
            version: "0.54.0".into(),
            build_number: 54,
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            sha256: "ab".repeat(32),
            dmg: "aterm-0.54.0.dmg".into(),
            zip: None,
            zip_sha256: None,
            min_build: None,
            changelog: None,
        }
    }

    fn release_with_appcast(tag: &str, url: &str) -> Release {
        let version = tag.strip_prefix('v').unwrap_or(tag);
        Release {
            tag_name: tag.into(),
            draft: false,
            assets: vec![
                Asset {
                    name: "aterm-appcast.toml".into(),
                    url: url.into(),
                    size: 0,
                },
                Asset {
                    name: format!("aterm-{version}.dmg"),
                    url: format!("{url}-dmg"),
                    size: 0,
                },
            ],
        }
    }

    fn release_with_signed_appcast(tag: &str, manifest_url: &str, signature_url: &str) -> Release {
        let version = tag.strip_prefix('v').unwrap_or(tag);
        Release {
            tag_name: tag.into(),
            draft: false,
            assets: vec![
                Asset {
                    name: "aterm-appcast.toml".into(),
                    url: manifest_url.into(),
                    size: 0,
                },
                Asset {
                    name: "aterm-appcast.toml.sig".into(),
                    url: signature_url.into(),
                    size: 0,
                },
                Asset {
                    name: format!("aterm-{version}.dmg"),
                    url: format!("{manifest_url}-dmg"),
                    size: 0,
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

    fn catalog_model_state(
        model: &aterm_spec::derive::Model,
        order: [usize; 3],
        signed: bool,
    ) -> aterm_spec::interp::State {
        let mut state = model.init_state();
        if signed {
            assert!(model.fire("ConfigureSignatures", &mut state));
        }
        let actions = ["ObserveMinor8", "ObserveMinor9", "ObserveMinor10"];
        for index in order {
            assert!(model.fire(actions[index], &mut state));
        }
        state
    }

    fn project_authority_selection(
        mut before: aterm_spec::interp::State,
        selected: &AuthoritativeRelease,
    ) -> aterm_spec::interp::State {
        before.insert("phase", 1);
        before.insert(
            "selected_minor",
            i64::try_from(selected.tag[1]).expect("bounded minor fits i64"),
        );
        before.insert("metadata_complete", 1);
        before
    }

    fn assert_tiered_transition(
        model: &aterm_spec::derive::Model,
        before: &aterm_spec::interp::State,
        after: &aterm_spec::interp::State,
        action: &str,
        label: &str,
    ) {
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            model,
            &[],
            before,
            after,
            Some(action),
            label,
        );
        assert!(admitted, "{label}: model rejected real transition: {why}");
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, after),
                "{label}: real transition violated {}: {after:?}",
                invariant.name
            );
        }
    }

    /// Tier-1 binds the real metadata arbiter and authoritative transport seam to
    /// `NativeUpdateChannelScan`. The real functions determine every projected
    /// output; the model supplies only the bounded input representation.
    #[test]
    fn authoritative_selection_and_fetch_refine_channel_scan_model() {
        let model = aterm_spec::derive::native_update_channel_scan_model();
        let orders = [
            [0usize, 1usize, 2usize],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        let base = [
            release_with_appcast("v0.8.0", "older-8-503"),
            release_with_appcast("v0.9.0", "older-9-503"),
            release_with_appcast("v0.10.0", "authoritative-10"),
        ];

        // Every concrete permutation must project onto the same numeric-max
        // CompleteMetadataArbitration transition.
        for (position, order) in orders.into_iter().enumerate() {
            let releases = order.map(|index| base[index].clone()).to_vec();
            let selected = select_authoritative_release(releases, "")
                .expect("canonical catalog")
                .expect("one authority");
            assert_eq!(selected.tag, vec![0, 10, 0]);
            let before = catalog_model_state(&model, order, false);
            let after = project_authority_selection(before.clone(), &selected);
            assert!(
                model
                    .successors("CompleteMetadataArbitration", &before)
                    .contains(&after),
                "permutation {order:?} did not conform"
            );
            if position == 0 {
                assert_tiered_transition(
                    &model,
                    &before,
                    &after,
                    "CompleteMetadataArbitration",
                    "updater catalog numeric-max arbitration",
                );
            }
        }

        // The real migration catalog may include strictly-lower current-scheme
        // tags carrying a large PATCH component. They project to
        // ObserveLowerLegacy and do not change the selected max: ordering is by
        // numeric vector, so `[0, 5, 14]` stays below `[0, 10, 0]`.
        let selected = select_authoritative_release(
            vec![
                release_with_appcast("v0.5.14", "legacy-must-not-fetch"),
                release_with_appcast("v0.10.0", "authoritative-10"),
                release_with_appcast("v0.8.0", "older-8-503"),
                release_with_appcast("v0.9.0", "older-9-503"),
            ],
            "",
        )
        .unwrap()
        .unwrap();
        let mut before = catalog_model_state(&model, orders[4], false);
        assert!(model.fire("ObserveLowerLegacy", &mut before));
        let after = project_authority_selection(before.clone(), &selected);
        assert_tiered_transition(
            &model,
            &before,
            &after,
            "CompleteMetadataArbitration",
            "updater lower legacy migration arbitration",
        );

        // Conversely, the real selector refuses a noncanonical numeric maximum:
        // `v0.10.0.1` orders above the canonical `v0.10.0` but is not a
        // `vMAJOR.MINOR.PATCH` identity, so the whole check fails closed rather
        // than quietly electing the canonical runner-up.
        let real_error = select_authoritative_release(
            vec![
                release_with_appcast("v0.10.0", "canonical-lower"),
                release_with_appcast("v0.10.0.1", "noncanonical-maximum"),
                release_with_appcast("v0.8.0", "older-8-503"),
                release_with_appcast("v0.9.0", "older-9-503"),
            ],
            "",
        );
        assert!(real_error.is_err());
        let mut before = catalog_model_state(&model, orders[0], false);
        assert!(model.fire("ObserveNewerNoncanonical", &mut before));
        let mut refused = before.clone();
        refused.insert("phase", 3);
        refused.insert("metadata_complete", 1);
        refused.insert("deferred", 1);
        assert_tiered_transition(
            &model,
            &before,
            &refused,
            "RefuseNoncanonicalAuthority",
            "updater noncanonical numeric maximum refusal",
        );

        // Drive the real unsigned fetch. The historical 503 URLs are present but
        // invocation-counted and must never be called.
        let selected = select_authoritative_release(base.to_vec(), "")
            .unwrap()
            .unwrap();
        let selection_before = catalog_model_state(&model, orders[0], false);
        let mut selected_state = project_authority_selection(selection_before, &selected);
        assert!(model.fire("ExposeOlderUnreadable", &mut selected_state));
        let mut urls = Vec::new();
        let mut download = |url: &str, _max_bytes: u64| {
            urls.push(url.to_string());
            match url {
                "authoritative-10" => Ok(manifest_bytes("0.10.0", 10, 0)),
                "older-8-503" | "older-9-503" => {
                    Err("503 historical release asset unavailable".into())
                }
                unexpected => panic!("unexpected asset fetch: {unexpected}"),
            }
        };
        let fetched = fetch_authoritative_release(Some(selected), "", &mut download);
        assert!(fetched.selected.is_some());
        assert_eq!(urls, ["authoritative-10"]);
        let mut verified = selected_state.clone();
        verified.insert("phase", 2);
        verified.insert(
            "manifest_fetch_count",
            i64::from(fetched.manifest_fetch_attempts),
        );
        verified.insert("signature_fetch_count", 0);
        verified.insert("fetched_minor", 10);
        assert_tiered_transition(
            &model,
            &selected_state,
            &verified,
            "FetchAuthoritativeVerified",
            "updater one authoritative manifest fetch",
        );

        // A pinned channel projects the same action with exactly one subordinate
        // signature fetch, and still does not invoke either older release.
        let keypair = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let public_key = B64.encode(keypair.public_key().as_ref());
        let manifest = manifest_bytes("0.10.0", 10, 0);
        let signature = keypair.sign(&manifest).as_ref().to_vec();
        let signed_base = [
            release_with_signed_appcast("v0.8.0", "signed-old-8", "signed-old-8-sig"),
            release_with_signed_appcast("v0.9.0", "signed-old-9", "signed-old-9-sig"),
            release_with_signed_appcast("v0.10.0", "signed-high", "signed-high-sig"),
        ];
        let selected = select_authoritative_release(signed_base.to_vec(), &public_key)
            .unwrap()
            .unwrap();
        let selection_before = catalog_model_state(&model, orders[5], true);
        let selected_state = project_authority_selection(selection_before, &selected);
        let mut urls = Vec::new();
        let mut download = |url: &str, _max_bytes: u64| {
            urls.push(url.to_string());
            match url {
                "signed-high" => Ok(manifest.clone()),
                "signed-high-sig" => Ok(signature.clone()),
                "signed-old-8" | "signed-old-9" => Err("older 503".into()),
                "signed-old-8-sig" | "signed-old-9-sig" => {
                    panic!("older signature must not be fetched")
                }
                unexpected => panic!("unexpected asset fetch: {unexpected}"),
            }
        };
        let fetched = fetch_authoritative_release(Some(selected), &public_key, &mut download);
        assert!(fetched.selected.is_some());
        assert_eq!(urls, ["signed-high", "signed-high-sig"]);
        let mut verified = selected_state.clone();
        verified.insert("phase", 2);
        verified.insert(
            "manifest_fetch_count",
            i64::from(fetched.manifest_fetch_attempts),
        );
        verified.insert("signature_fetch_count", 1);
        verified.insert("fetched_minor", 10);
        assert_tiered_transition(
            &model,
            &selected_state,
            &verified,
            "FetchAuthoritativeVerified",
            "updater signed authoritative fetch",
        );

        // Once the authoritative manifest succeeds under a pinned policy, a
        // signature transport failure is terminal with one manifest attempt and
        // one signature attempt. It cannot be projected as an unsigned failure.
        let selected = select_authoritative_release(signed_base.to_vec(), &public_key)
            .unwrap()
            .unwrap();
        let selection_before = catalog_model_state(&model, orders[1], true);
        let selected_state = project_authority_selection(selection_before, &selected);
        let mut urls = Vec::new();
        let mut download = |url: &str, _max_bytes: u64| {
            urls.push(url.to_string());
            match url {
                "signed-high" => Ok(manifest.clone()),
                "signed-high-sig" => Err("authoritative signature 503".into()),
                unexpected => panic!("unexpected asset fetch: {unexpected}"),
            }
        };
        let fetched = fetch_authoritative_release(Some(selected), &public_key, &mut download);
        assert!(fetched.selected.is_none());
        assert!(fetched.appcast_fetch_error);
        assert_eq!(urls, ["signed-high", "signed-high-sig"]);
        let mut refused = selected_state.clone();
        refused.insert("phase", 3);
        refused.insert(
            "manifest_fetch_count",
            i64::from(fetched.manifest_fetch_attempts),
        );
        refused.insert(
            "signature_fetch_count",
            i64::from(fetched.signature_fetch_attempts),
        );
        refused.insert("fetched_minor", 10);
        refused.insert("authoritative_fetch_failed", 1);
        refused.insert("deferred", 1);
        assert_tiered_transition(
            &model,
            &selected_state,
            &refused,
            "FetchAuthoritativeSignatureUnreadable",
            "updater authoritative signature transport failure",
        );

        // A fetched but invalid authoritative signature is a rejection, also
        // terminal after exactly the same two bounded transport attempts.
        let selected = select_authoritative_release(signed_base.to_vec(), &public_key)
            .unwrap()
            .unwrap();
        let selection_before = catalog_model_state(&model, orders[3], true);
        let selected_state = project_authority_selection(selection_before, &selected);
        let mut urls = Vec::new();
        let mut download = |url: &str, _max_bytes: u64| {
            urls.push(url.to_string());
            match url {
                "signed-high" => Ok(manifest.clone()),
                "signed-high-sig" => Ok(vec![0_u8; 64]),
                unexpected => panic!("unexpected asset fetch: {unexpected}"),
            }
        };
        let fetched = fetch_authoritative_release(Some(selected), &public_key, &mut download);
        assert!(fetched.selected.is_none());
        assert!(fetched.manifest_rejected);
        assert_eq!(urls, ["signed-high", "signed-high-sig"]);
        let mut refused = selected_state.clone();
        refused.insert("phase", 3);
        refused.insert(
            "manifest_fetch_count",
            i64::from(fetched.manifest_fetch_attempts),
        );
        refused.insert(
            "signature_fetch_count",
            i64::from(fetched.signature_fetch_attempts),
        );
        refused.insert("fetched_minor", 10);
        refused.insert("authoritative_manifest_rejected", 1);
        refused.insert("deferred", 1);
        assert_tiered_transition(
            &model,
            &selected_state,
            &refused,
            "RejectAuthoritativeManifest",
            "updater invalid authoritative signature rejection",
        );

        // Authoritative transport failure and tag/manifest mismatch are distinct
        // real outputs, but both project to terminal no-fallback refusal actions.
        for (manifest_result, action) in [
            (
                Err("authoritative 503".to_string()),
                "FetchAuthoritativeUnreadable",
            ),
            (
                Ok(manifest_bytes("0.9.0", 9, 0)),
                "RejectAuthoritativeManifest",
            ),
        ] {
            let selected = select_authoritative_release(base.to_vec(), "")
                .unwrap()
                .unwrap();
            let selection_before = catalog_model_state(&model, orders[2], false);
            let selected_state = project_authority_selection(selection_before, &selected);
            let mut calls = 0usize;
            let mut download = |_url: &str, _max_bytes: u64| {
                calls += 1;
                manifest_result.clone()
            };
            let fetched = fetch_authoritative_release(Some(selected), "", &mut download);
            assert_eq!(calls, 1);
            assert!(fetched.selected.is_none());
            let mut refused = selected_state.clone();
            refused.insert("phase", 3);
            refused.insert(
                "manifest_fetch_count",
                i64::from(fetched.manifest_fetch_attempts),
            );
            refused.insert("fetched_minor", 10);
            refused.insert("deferred", 1);
            refused.insert(
                "authoritative_fetch_failed",
                i64::from(fetched.appcast_fetch_error),
            );
            refused.insert(
                "authoritative_manifest_rejected",
                i64::from(fetched.manifest_rejected),
            );
            assert_tiered_transition(
                &model,
                &selected_state,
                &refused,
                action,
                "updater authoritative failure is terminal",
            );
        }

        // Malformed metadata is refused by the real selector before the transport
        // closure exists, and projects to RefuseMetadata with zero fetches.
        let real_error = select_authoritative_release(
            vec![release_with_appcast("v0.010.0", "must-not-fetch")],
            "",
        );
        assert!(real_error.is_err());
        let before = model.successors("ObserveMalformedCandidate", &model.init_state())[0].clone();
        let mut refused = before.clone();
        refused.insert("phase", 3);
        refused.insert("metadata_complete", 1);
        refused.insert("deferred", 1);
        assert_tiered_transition(
            &model,
            &before,
            &refused,
            "RefuseMetadata",
            "updater malformed metadata refuses before fetch",
        );

        // The signed/parsed authority still cannot select a DMG by first match.
        // Missing, duplicate (in either REST order), and path-like/noncanonical
        // names are manifest-class rejections after exactly one authoritative
        // manifest fetch, with no older fallback and no DMG transport.
        let mut missing_dmg = release_with_appcast("v0.10.0", "authoritative-10");
        missing_dmg
            .assets
            .retain(|asset| asset.name != "aterm-0.10.0.dmg");

        let mut duplicate_dmg = release_with_appcast("v0.10.0", "authoritative-10");
        duplicate_dmg.assets.push(Asset {
            name: "aterm-0.10.0.dmg".into(),
            url: "duplicate-dmg".into(),
            size: 0,
        });
        let mut duplicate_dmg_reversed = duplicate_dmg.clone();
        duplicate_dmg_reversed.assets.reverse();

        for (label, release, manifest) in [
            (
                "missing authoritative DMG",
                missing_dmg,
                manifest_bytes("0.10.0", 10, 0),
            ),
            (
                "duplicate authoritative DMG (forward order)",
                duplicate_dmg,
                manifest_bytes("0.10.0", 10, 0),
            ),
            (
                "duplicate authoritative DMG (reverse order)",
                duplicate_dmg_reversed,
                manifest_bytes("0.10.0", 10, 0),
            ),
            (
                "path-like authoritative DMG",
                release_with_appcast("v0.10.0", "authoritative-10"),
                manifest_bytes_with_dmg("0.10.0", 10, 0, "../aterm-0.10.0.dmg"),
            ),
        ] {
            let selected = select_authoritative_release(vec![release], "")
                .unwrap()
                .unwrap();
            let selection_before = catalog_model_state(&model, orders[0], false);
            let selected_state = project_authority_selection(selection_before, &selected);
            let mut urls = Vec::new();
            let mut download = |url: &str, _max_bytes: u64| {
                urls.push(url.to_string());
                assert_eq!(url, "authoritative-10", "{label}");
                Ok(manifest.clone())
            };
            let fetched = fetch_authoritative_release(Some(selected), "", &mut download);
            assert!(fetched.selected.is_none(), "{label}");
            assert!(fetched.manifest_rejected, "{label}");
            assert!(!fetched.appcast_fetch_error, "{label}");
            assert_eq!(urls, ["authoritative-10"], "{label}");
            assert_eq!(fetched.manifest_fetch_attempts, 1, "{label}");

            let mut refused = selected_state.clone();
            refused.insert("phase", 3);
            refused.insert("manifest_fetch_count", 1);
            refused.insert("fetched_minor", 10);
            refused.insert("authoritative_manifest_rejected", 1);
            refused.insert("deferred", 1);
            assert_tiered_transition(
                &model,
                &selected_state,
                &refused,
                "RejectAuthoritativeManifest",
                label,
            );
        }

        // NEGATIVE CONTROL: corrupting the real v0.10.0 selection into row-order
        // v0.9.0 cannot validate as CompleteMetadataArbitration.
        let selected = select_authoritative_release(base.to_vec(), "")
            .unwrap()
            .unwrap();
        let before = catalog_model_state(&model, orders[0], false);
        let mut corrupted = project_authority_selection(before.clone(), &selected);
        corrupted.insert("selected_minor", 9);
        let (admitted, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &corrupted,
            Some("CompleteMetadataArbitration"),
            "updater row-order selection negative control",
        );
        assert!(
            !admitted,
            "healthy model admitted v0.9.0 over v0.10.0: {why}"
        );
        assert!(!model.check_invariant("SelectedAuthorityIsNumericMaximum", &corrupted));
    }

    #[test]
    fn canonical_numeric_selection_is_permutation_invariant_and_skips_older_503() {
        let base = [
            release_with_appcast("v0.9.0", "old-9-503"),
            release_with_appcast("v0.10.0", "authoritative-10"),
            release_with_appcast("v0.8.0", "old-8-503"),
        ];
        for order in [
            [0usize, 1usize, 2usize],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let releases = order.map(|index| base[index].clone()).to_vec();
            let authoritative = select_authoritative_release(releases, "")
                .unwrap()
                .expect("one authoritative release");
            assert_eq!(authoritative.release.tag_name, "v0.10.0");
            let mut urls = Vec::new();
            let mut download = |url: &str, _max_bytes: u64| {
                urls.push(url.to_string());
                match url {
                    "authoritative-10" => Ok(manifest_bytes("0.10.0", 10, 0)),
                    "old-9-503" | "old-8-503" => {
                        Err("503 historical release asset unavailable".into())
                    }
                    unexpected => panic!("unexpected asset fetch: {unexpected}"),
                }
            };
            let fetched = fetch_authoritative_release(Some(authoritative), "", &mut download);
            assert_eq!(fetched.selected.unwrap().0.version, "0.10.0");
            assert_eq!(urls, ["authoritative-10"]);
            assert_eq!(fetched.manifest_fetch_attempts, 1);
            assert!(!fetched.appcast_fetch_error);
        }
    }

    #[test]
    fn signed_authority_fetches_one_manifest_and_one_signature_only() {
        let keypair = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let public_key = B64.encode(keypair.public_key().as_ref());
        let manifest = manifest_bytes("0.10.0", 10, 0);
        let signature = keypair.sign(&manifest).as_ref().to_vec();
        let releases = vec![
            release_with_signed_appcast("v0.9.0", "older-503", "older-signature"),
            release_with_signed_appcast("v0.10.0", "highest-manifest", "highest-signature"),
        ];
        let authoritative = select_authoritative_release(releases, &public_key)
            .unwrap()
            .unwrap();
        let mut urls = Vec::new();
        let mut download = |url: &str, _max_bytes: u64| {
            urls.push(url.to_string());
            match url {
                "highest-manifest" => Ok(manifest.clone()),
                "highest-signature" => Ok(signature.clone()),
                "older-503" => Err("503 historical release asset unavailable".into()),
                "older-signature" => panic!("older signature must not be fetched"),
                unexpected => panic!("unexpected asset fetch: {unexpected}"),
            }
        };
        let fetched = fetch_authoritative_release(Some(authoritative), &public_key, &mut download);
        assert_eq!(fetched.selected.unwrap().0.version, "0.10.0");
        assert_eq!(urls, ["highest-manifest", "highest-signature"]);
        assert_eq!(fetched.manifest_fetch_attempts, 1);
        assert!(!fetched.appcast_fetch_error);
    }

    /// The archive carries current-scheme tags whose PATCH component is a huge
    /// historical timestamp. They are perfectly orderable candidates — they just
    /// order strictly below a higher MINOR, and none of them may be fetched.
    #[test]
    fn lower_numeric_candidates_are_tolerated_but_cannot_be_authority() {
        let historical = [
            "v0.21.2607041853",
            "v0.20.2607041751",
            "v0.19.2607040807",
            "v0.18.2607040011",
            "v0.17.2607032327",
            "v0.15.2607031838",
            "v0.15.2607021856",
            "v0.5.14",
            "v0.5.13",
            "v0.5.12",
            "v0.5.11",
            "v0.5.10",
            "v0.5.9",
        ];
        let catalog = std::iter::once(release_with_appcast("v0.54.0", "authoritative-54"))
            .chain(
                historical
                    .iter()
                    .enumerate()
                    .map(|(index, tag)| release_with_appcast(tag, &format!("legacy-{index}"))),
            )
            .collect::<Vec<_>>();

        for releases in [catalog.clone(), catalog.iter().rev().cloned().collect()] {
            let selected = select_authoritative_release(releases, "")
                .unwrap()
                .expect("canonical maximum exists");
            assert_eq!(selected.release.tag_name, "v0.54.0");
            let mut urls = Vec::new();
            let mut download = |url: &str, _max_bytes: u64| {
                urls.push(url.to_string());
                if url == "authoritative-54" {
                    Ok(manifest_bytes("0.54.0", 54, 0))
                } else {
                    Err("historical asset must not be fetched".into())
                }
            };
            let fetched = fetch_authoritative_release(Some(selected), "", &mut download);
            assert_eq!(fetched.selected.unwrap().0.version, "0.54.0");
            assert_eq!(urls, ["authoritative-54"]);
            assert_eq!(fetched.manifest_fetch_attempts, 1);
        }

        // A same-or-newer tag with too many components orders ABOVE the canonical
        // maximum but has no `vMAJOR.MINOR.PATCH` identity. It must fail the whole
        // check closed rather than let the runner-up be elected behind it.
        for same_or_newer_noncanonical in ["v0.54.0.1", "v0.55.0.0"] {
            let err = select_authoritative_release(
                vec![
                    release_with_appcast("v0.54.0", "canonical"),
                    release_with_appcast(same_or_newer_noncanonical, "must-not-fetch"),
                ],
                "",
            )
            .err()
            .expect("a noncanonical numeric maximum must fail closed");
            assert!(
                err.contains(same_or_newer_noncanonical) && err.contains("numeric dotted"),
                "{err}"
            );
        }

        let err = select_authoritative_release(
            vec![
                release_with_appcast("v0.54.0", "canonical"),
                release_with_appcast("v0.legacy.1", "must-not-fetch"),
            ],
            "",
        )
        .err()
        .expect("an unorderable historical exact-name tag must fail closed");
        assert!(err.contains("numeric dotted"), "{err}");
    }

    #[test]
    fn unsigned_or_duplicate_signature_on_highest_never_falls_back() {
        let keypair = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let public_key = B64.encode(keypair.public_key().as_ref());
        let lower = release_with_signed_appcast("v0.9.0", "lower-manifest", "lower-signature");

        let err = select_authoritative_release(
            vec![
                lower.clone(),
                release_with_appcast("v0.10.0", "unsigned-highest"),
            ],
            &public_key,
        )
        .err()
        .expect("unsigned highest must defer");
        assert!(err.contains("v0.10.0") && err.contains("unsigned"), "{err}");

        let mut duplicate_sig =
            release_with_signed_appcast("v0.10.0", "highest-manifest", "highest-signature-a");
        duplicate_sig.assets.push(Asset {
            name: "aterm-appcast.toml.sig".into(),
            url: "highest-signature-b".into(),
            size: 0,
        });
        let err = select_authoritative_release(vec![duplicate_sig, lower], &public_key)
            .err()
            .expect("duplicate highest signature must defer");
        assert!(
            err.contains("duplicate assets") && err.contains(".sig"),
            "{err}"
        );
    }

    /// The cut-over contract (`VERSIONING.md`): there is now exactly ONE version
    /// — the workspace `MAJOR.MINOR.0` — and a release is that number with DEV
    /// reset to 0, tagged `vMAJOR.MINOR.PATCH`. The pre-cut-over `vMAJOR.MINOR`
    /// app-channel releases were NOT carried forward. They stay published in the
    /// archive, so the selector must treat them as inert: never an error (that
    /// would stall the channel behind history nobody can delete) and never a
    /// candidate (that would strand the fleet on a retired release, because the
    /// retired numbers are far LARGER than the ones the new scheme starts from).
    #[test]
    fn legacy_two_component_tags_are_never_installed() {
        // The whole point of the cut-over: v0.61 is numerically enormous next to
        // the first current-scheme release v0.5.0, and still loses — it is not
        // ordered against the candidate at all, it is skipped.
        let selected = select_authoritative_release(
            vec![
                release_with_appcast("v0.61", "retired-app-channel-head"),
                release_with_appcast("v0.5.0", "current-head"),
            ],
            "",
        )
        .expect("a retired two-component release is skipped, not an error")
        .expect("the current-scheme candidate is elected");
        assert_eq!(
            selected.release.tag_name, "v0.5.0",
            "a retired two-component tag must never win selection"
        );
        assert_eq!(selected.version, "0.5.0");

        // …and it is not merely unselected: the retired release's assets are
        // never fetched, so no legacy manifest can reach the install path.
        let mut urls = Vec::new();
        let mut download = |url: &str, _max_bytes: u64| {
            urls.push(url.to_string());
            match url {
                "current-head" => Ok(manifest_bytes("0.5.0", 2, 0)),
                "retired-app-channel-head" => panic!("a retired release must never be fetched"),
                unexpected => panic!("unexpected asset fetch: {unexpected}"),
            }
        };
        let fetched = fetch_authoritative_release(Some(selected), "", &mut download);
        assert_eq!(fetched.selected.unwrap().0.version, "0.5.0");
        assert_eq!(urls, ["current-head"]);
        assert_eq!(fetched.manifest_fetch_attempts, 1);

        // Every ordering position is the same decision — the retired tag is
        // invisible to arbitration, not a runner-up.
        for legacy in ["v0.25", "v0.54", "v0.61", "v9.99"] {
            let selected = select_authoritative_release(
                vec![
                    release_with_appcast(legacy, "retired-must-not-fetch"),
                    release_with_appcast("v0.5.0", "current-head"),
                ],
                "",
            )
            .expect("retired two-component releases are inert archive history")
            .expect("the current-scheme candidate is elected");
            assert_eq!(selected.release.tag_name, "v0.5.0", "lost to {legacy}");
        }
    }

    /// A release that does not carry the EXACT `aterm-appcast.toml` asset is not
    /// a candidate at all, whatever its tag says. That asset requirement — not a
    /// version floor — is what keeps the pre-cut-over archive inert now that the
    /// lineage counts from the PUBLIC series (`v0.1.0`; `VERSIONING.md`).
    ///
    /// This matters because the archive is NOT all two-component/Legacy: the
    /// pre-0.23 releases carry three-component *timestamp* tags
    /// (`v0.15.2607021856` … `v0.21.2607041853`) plus a `v0.5.x` line, which
    /// parse as canonical `vMAJOR.MINOR.PATCH` and therefore compete on numeric
    /// order. `[0, 21, 2607041853]` outranks every MINOR below 21, so at the
    /// public `0.x` lineage the archive WOULD win a numeric contest — see
    /// [`a_low_lineage_really_does_lose_a_numeric_contest_to_the_archive`].
    /// It never gets to have that contest: when the archive was retired every
    /// appcast was renamed to `aterm-appcast-<tag>.toml`, so `select`'s
    /// `unique_asset_index(&release, "aterm-appcast.toml")` skips those releases
    /// before their tags are ever parsed.
    ///
    /// The retired lineage also lives in the PRIVATE staging repo, while shipped
    /// clients read the public channel (`[workspace.metadata.aterm]
    /// update_channel`), whose namespace carries only the current series. This
    /// test pins the asset rule so the archive stays inert even for a machine
    /// pointed back at the staging repo by `ATERM_UPDATE_OWNER`/`_REPO`.
    #[test]
    fn archive_releases_are_not_candidates_even_when_their_tags_outrank_us() {
        // Real tags from this repo's history, in their real spellings, carrying
        // the renamed asset the archive migration actually left behind.
        let archive = [
            "v0.21.2607041853",
            "v0.20.2607041751",
            "v0.15.2607021856",
            "v0.5.14",
            "v0.4.1",
            "v0.3.0",
        ];
        for historical in archive {
            let renamed = Release {
                tag_name: historical.into(),
                draft: false,
                assets: vec![Asset {
                    name: format!("aterm-appcast-{historical}.toml"),
                    url: "archive-must-not-be-fetched".into(),
                    size: 0,
                }],
            };
            let selected = select_authoritative_release(
                vec![renamed, release_with_appcast("v0.5.0", "current-head")],
                "",
            )
            .expect("an archive release without the exact appcast asset is inert, not an error")
            .expect("the current-series candidate is elected");
            assert_eq!(
                selected.release.tag_name, "v0.5.0",
                "{historical} carries no aterm-appcast.toml and must never be elected"
            );
        }
    }

    /// The ordering hazard the asset rule is protecting, asserted head-on so it
    /// can never be mistaken for impossible: give an archive timestamp tag the
    /// canonical appcast asset and it BURIES the current public lineage.
    ///
    /// Nothing in the shipped channel does that — the archive's assets are
    /// renamed and clients read the public repo — but if a future change ever
    /// re-attached `aterm-appcast.toml` to an archive release, or published the
    /// archive into the public channel, the fleet would elect a July-2026 build
    /// and then sit at "up to date" forever (selection picks it; the
    /// strictly-greater `build_number` apply gate then refuses it). This test is
    /// the tripwire for that, and the reason the lineage must never depend on
    /// out-numbering the archive.
    #[test]
    fn a_low_lineage_really_does_lose_a_numeric_contest_to_the_archive() {
        let inverted = select_authoritative_release(
            vec![
                release_with_appcast("v0.21.2607041853", "archive"),
                release_with_appcast("v0.5.0", "current-public-lineage"),
            ],
            "",
        )
        .expect("orderable")
        .expect("a candidate is elected");
        assert_eq!(
            inverted.release.tag_name, "v0.21.2607041853",
            "the archive outranks the public 0.x lineage numerically — the appcast \
             asset rule, not a version floor, is what keeps it out of the channel"
        );
    }

    /// A catalog that carries ONLY the retired archive is "nothing to install",
    /// not a failure: the client stays on its build and the check is healthy.
    #[test]
    fn a_catalog_of_only_legacy_releases_selects_nothing() {
        let only_legacy = ["v0.25", "v0.54", "v0.60", "v0.61"]
            .iter()
            .enumerate()
            .map(|(index, tag)| release_with_appcast(tag, &format!("retired-{index}")))
            .collect::<Vec<_>>();
        let selected = select_authoritative_release(only_legacy, "")
            .expect("retired releases are skipped, never an error");
        assert!(
            selected.is_none(),
            "an archive-only catalog has no installable candidate"
        );

        // A pinned channel reaches the same conclusion without ever demanding a
        // signature from a retired release (it is skipped before the signature
        // policy applies).
        let keypair = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let public_key = B64.encode(keypair.public_key().as_ref());
        let selected = select_authoritative_release(
            vec![release_with_appcast("v0.61", "retired-unsigned")],
            &public_key,
        )
        .expect("a retired unsigned release is skipped, not a signature failure");
        assert!(selected.is_none());
    }

    /// Tag order is by numeric component, never lexicographic — now that the
    /// PATCH component exists, `0.2.9 < 0.2.10` has to hold there too.
    #[test]
    fn numeric_order_beats_lexicographic_order_in_every_component() {
        for (lower, higher) in [
            ("v0.2.9", "v0.2.10"),
            ("v0.9.0", "v0.10.0"),
            ("v9.0.0", "v10.0.0"),
        ] {
            for catalog in [[lower, higher], [higher, lower]] {
                let selected = select_authoritative_release(
                    catalog
                        .iter()
                        .map(|tag| release_with_appcast(tag, tag))
                        .collect(),
                    "",
                )
                .unwrap()
                .expect("a numeric maximum exists");
                assert_eq!(
                    selected.release.tag_name, higher,
                    "{lower} must sort below {higher} in {catalog:?}"
                );
            }
        }
    }

    #[test]
    fn malformed_and_duplicate_candidates_fail_before_download() {
        // Structurally unparseable: no `v`, wrong case, too few or too many
        // components, empty or nonnumeric components. Note that exactly TWO
        // numeric components is NOT here — that is the retired scheme, which is
        // skipped rather than refused (`legacy_two_component_tags_are_never_installed`).
        for malformed in [
            "0.10.0",
            "V0.10.0",
            "v",
            "v0",
            "v0.x.0",
            "v0.1.2.3",
            "v0..10",
            "v0.10.",
            "v.10.0",
            "v0.10.0-rc1",
        ] {
            let err = select_authoritative_release(
                vec![release_with_appcast(malformed, "must-not-fetch")],
                "",
            )
            .err()
            .expect("nonnumeric exact-name candidate must fail closed");
            assert!(err.contains("numeric dotted"), "{malformed}: {err}");
        }
        // Numeric but noncanonical: a leading zero gives one release two
        // spellings, so it is refused outright rather than admitted alongside its
        // canonical twin.
        for noncanonical_maximum in ["v00.10.0", "v0.010.0", "v0.10.00", "v0.10.0000000"] {
            let err = select_authoritative_release(
                vec![release_with_appcast(noncanonical_maximum, "must-not-fetch")],
                "",
            )
            .err()
            .expect("noncanonical numeric maximum must fail closed");
            assert!(
                err.contains(noncanonical_maximum) && err.contains("numeric dotted"),
                "{noncanonical_maximum}: {err}"
            );
        }

        let mut duplicate_asset = release_with_appcast("v0.10.0", "manifest-a");
        duplicate_asset.assets.push(Asset {
            name: "aterm-appcast.toml".into(),
            url: "manifest-b".into(),
            size: 0,
        });
        let err = select_authoritative_release(vec![duplicate_asset], "")
            .err()
            .expect("duplicate exact assets must fail closed");
        assert!(err.contains("duplicate assets"), "{err}");

        let err = select_authoritative_release(
            vec![
                release_with_appcast("v0.10.0", "manifest-a"),
                release_with_appcast("v0.10.0", "manifest-b"),
            ],
            "",
        )
        .err()
        .expect("duplicate canonical candidates must fail closed");
        assert!(
            err.contains("duplicate published update candidates"),
            "{err}"
        );

        // Two spellings of one numeric vector can never both be candidates: the
        // canonicality rule refuses the noncanonical twin outright, so a numeric
        // order collision cannot be resolved by response position.
        let err = select_authoritative_release(
            vec![
                release_with_appcast("v0.10.0", "manifest-a"),
                release_with_appcast("v00.010.00", "manifest-b"),
            ],
            "",
        )
        .err()
        .expect("an aliasing spelling must fail closed");
        assert!(err.contains("numeric dotted"), "{err}");
    }

    #[test]
    fn authoritative_manifest_version_must_equal_canonical_tag() {
        let authoritative = select_authoritative_release(
            vec![
                release_with_appcast("v0.9.0", "older-must-not-fetch"),
                release_with_appcast("v0.10.0", "mismatched-highest"),
            ],
            "",
        )
        .unwrap()
        .unwrap();
        let mut urls = Vec::new();
        let mut download = |url: &str, _max_bytes: u64| {
            urls.push(url.to_string());
            match url {
                "mismatched-highest" => Ok(manifest_bytes("0.9.0", 10, 0)),
                "older-must-not-fetch" => panic!("older fallback must not be fetched"),
                unexpected => panic!("unexpected asset fetch: {unexpected}"),
            }
        };
        let fetched = fetch_authoritative_release(Some(authoritative), "", &mut download);
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
                size: 0,
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

    /// Lock the GitHub Releases JSON contract the selection loop depends on: the
    /// `draft` flag deserializes (drafts are skipped, F12), assets are found by name,
    /// the asset API `url` + `size` are captured, and a missing `size` defaults to 0.
    #[test]
    fn parses_release_json_flags_drafts_and_finds_assets() {
        let json = r#"[
          {"tag_name": "v1.0.0", "draft": false, "assets": [
             {"name": "aterm-appcast.toml", "url": "https://api.github.com/repos/o/r/releases/assets/1", "size": 512},
             {"name": "aterm-1.0.0.dmg", "url": "https://api.github.com/repos/o/r/releases/assets/2", "size": 1000}
          ]},
          {"tag_name": "v1.1.0", "draft": true, "assets": [
             {"name": "aterm-appcast.toml", "url": "https://api.github.com/repos/o/r/releases/assets/3"}
          ]}
        ]"#;
        let rels: Vec<Release> = serde_json::from_str(json).unwrap();
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].tag_name, "v1.0.0");
        assert!(!rels[0].draft);
        assert!(
            rels[1].draft,
            "draft flag must deserialize so the loop can skip it"
        );
        let dmg_index = unique_asset_index(&rels[0], "aterm-1.0.0.dmg")
            .unwrap()
            .expect("dmg asset present");
        let dmg = &rels[0].assets[dmg_index];
        assert_eq!(dmg.size, 1000);
        assert!(dmg.url.ends_with("/assets/2"), "asset API url captured");
        assert_eq!(
            unique_asset_index(&rels[0], "nope.dmg").unwrap(),
            None,
            "absent asset ⇒ None"
        );
        let appcast_index = unique_asset_index(&rels[1], "aterm-appcast.toml")
            .unwrap()
            .unwrap();
        assert_eq!(
            rels[1].assets[appcast_index].size, 0,
            "missing size defaults to 0"
        );
    }

    #[test]
    fn corrupt_high_ready_and_deleted_stage_cannot_suppress_restage() {
        let staging = test_staging("publishable");
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
    #[test]
    fn a_stage_backoff_throttles_the_restage_never_an_already_staged_newer_build() {
        use crate::manifest::{FailedMark, RETRY_BACKOFF_SECS};

        let staging = test_staging("stage-backoff-vs-apply");
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
}
