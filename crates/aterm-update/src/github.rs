// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The DMG/.app-specific orchestration of a background update check: learn which
//! release is the channel head, and — if it is strictly newer than the running build —
//! download + verify its container and stage it. The portable GitHub plumbing it
//! drives (the redirect-refusing HEAD, `curl` GET/download on either host, the
//! per-machine token chain) lives in `aterm-update-core`.
//!
//! # Two lanes, chosen by the SOURCE
//!
//! **The web lane** — the compiled-in public channel, and any repointed source for
//! which no token resolves — never touches `api.github.com`. Not on a happy path, not on
//! a failure path: there is no budget on this lane, so there is nothing to hold, reserve
//! or fall back to. A check is:
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
//!    that moves mid-check cannot mix two releases — and meets exactly the checks the
//!    LIST-based election met: the Ed25519 appcast signature under the pinned keyset or
//!    the master-signed machine roster ([`authorize_by_roster`], fail-closed), the
//!    manifest `version` == the tag minus its `v`, the manifest's container `url` ==
//!    the DERIVED download URL (the manifest field is cross-checked, never followed),
//!    then the build-number, `min_build`, high-water and roster-floor gates, the
//!    sha256 and the codesign/Team-ID verification on the staged bundle.
//!
//! **The token lane** — a repointed source for which the token chain
//! ([`aterm_update_core::token`], dedicated rungs first, ambient rungs last) resolves a
//! credential — keeps the API path: the paginated releases LIST (draft and prerelease
//! filter, unique-appcast proof, max canonical tag) and the asset API URLs with the token on
//! stdin (never argv), and holds until the server's own `x-ratelimit-reset` when the
//! LIST is rate-limited. A token GitHub rejects drops the check onto the web lane, once,
//! with a throttled warning: the source then has no usable credential, which is the
//! web lane's definition.
//!
//! On the default channel every token rung is IGNORED — a credential buys nothing on
//! a host that reads none — and the status surfaces say so.
//!
//! Neither lane is a trust decision. Artifact trust is the pinned Ed25519 keyset / the
//! master-signed roster, the pinned Team ID and the manifest sha256 — none of which
//! either lane touches.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use serde::Deserialize;

use aterm_update_core::tag::{TagError, TagKind};
use aterm_update_core::{HeadAnswer, HttpError, token};

use crate::manifest::{Manifest, Ready};
use crate::{Source, bundle, install, paths::Staging, sig};

/// Which lane the last COMPLETED check ran on. Read by the background loop to pick a
/// check cadence (see `crate::spawn_background_check`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    /// No check has completed yet in this process.
    Unknown,
    /// The releases API, with a resolved token (a repointed source only).
    Token,
    /// The unmetered web host, with no credential at all.
    Web,
}

/// [`Lane`] of the last completed check, as a `u8` so it can live in an atomic.
static LANE: AtomicU8 = AtomicU8::new(0);

/// Whether the last check ended in a deferral — GitHub asked us to slow down (a token
/// lane rate limit, or a `github.com` 429/5xx). Read by the background loop to
/// LENGTHEN the next wait without recording a failure: weather, not a broken updater.
static RATE_LIMITED: AtomicBool = AtomicBool::new(false);

/// Whether this process has already logged which lane it is on. Once per process: it
/// is a standing condition, not an event.
static ANNOUNCED_LANE: AtomicBool = AtomicBool::new(false);

/// Whether the web lane was entered because a PROVISIONED token was rejected by
/// GitHub rather than because no token exists. The healthy-status lane note must tell
/// those apart: "provision a token" is exactly wrong advice for a machine whose
/// problem is a stale token that needs ROTATING. Cleared by a token-lane read (a
/// working token is the end of the story either way).
static TOKEN_REJECTED: AtomicBool = AtomicBool::new(false);

/// The lane the last completed check used.
#[must_use]
pub fn lane() -> Lane {
    match LANE.load(Ordering::Relaxed) {
        1 => Lane::Token,
        2 => Lane::Web,
        _ => Lane::Unknown,
    }
}

/// Whether the last check was cut short by a deferral.
#[must_use]
pub fn rate_limited() -> bool {
    RATE_LIMITED.load(Ordering::Relaxed)
}

/// The unix epoch until which the last rate-limited TOKEN-LANE check asked this
/// machine to HOLD — the server's own `x-ratelimit-reset` from this check's LIST
/// headers, jittered and clamped ([`hold_until_reset`]) — or 0 when the last check
/// recorded no hold. Read by the background loop beside [`rate_limited`]: a known
/// reset is honoured exactly, instead of the doubling ladder.
static RATE_LIMIT_RESET: AtomicU64 = AtomicU64::new(0);

/// The hold epoch of the last check, if it recorded one. See [`RATE_LIMIT_RESET`].
#[must_use]
pub fn rate_limit_reset() -> Option<u64> {
    match RATE_LIMIT_RESET.load(Ordering::Relaxed) {
        0 => None,
        until => Some(until),
    }
}

/// The writer's side of the hold bound the readers (`crate::ledger_hold`) also apply.
use crate::{HOLD_HORIZON_SECS, HOLD_JITTER_SECS};

/// Unix seconds now as `u64`, saturating the same way [`unix_now`] does (a clock we
/// cannot read is `i64::MAX`, which only makes every clamp below bind).
fn unix_now_secs() -> u64 {
    u64::try_from(unix_now()).unwrap_or(u64::MAX)
}

/// The epoch a rate-limited check holds until, derived from `reset` (the server's own
/// `x-ratelimit-reset`) at `now`: `min(reset, now + 1 h) + jitter(0..60 s)`. Pure, so
/// the clamp and the jitter band are testable without a clock or a header file.
///
/// `None` for a reset that is not ahead of `now`: a stale or skewed epoch names no
/// future to wait for, and jittering it forward would manufacture a short hold out of
/// nothing — the caller keeps the historical deferred record instead.
fn hold_epoch(reset: u64, now: u64, entropy: u8) -> Option<u64> {
    if reset <= now {
        return None;
    }
    let jitter = u64::from(entropy) * HOLD_JITTER_SECS / 256;
    Some(
        reset
            .min(now.saturating_add(HOLD_HORIZON_SECS))
            .saturating_add(jitter),
    )
}

/// Record — process-wide, for the loop — that this machine is to hold off GitHub
/// until the budget the LIST just measured renews, and return that epoch. `None` when
/// this check's `list.headers` carries no `x-ratelimit-reset` (a proxy stripped it, or
/// the failure was not the API's), in which case the caller keeps the historical
/// back-off. The reset comes from the server's OWN answer to this very check, so
/// waiting for it IS the oracle — no `/rate_limit` probe (itself a request), no
/// machine-wide accountant file. TOKEN LANE ONLY: the web host carries no such header
/// and no budget to renew.
fn hold_until_reset(staging: &Staging) -> Option<u64> {
    let reset = aterm_update_core::rate_limit_from_header_dump(&staging.list_headers())?.reset?;
    let until = hold_epoch(reset, unix_now_secs(), crate::cadence::entropy_byte())?;
    RATE_LIMIT_RESET.store(until, Ordering::Relaxed);
    Some(until)
}

/// Write a TOKEN-LANE rate-limit deferral to the status ledger: HELD until the
/// server's reset when this check's headers name one (siblings and the cadence then
/// release exactly there), else the plain deferred record every sibling widens its
/// window on. Neither arm touches `health.toml` — a rate limit is weather, not a fault.
fn record_deferral(staging: &Staging, current_build: u64, outcome: &str) {
    RATE_LIMITED.store(true, Ordering::Relaxed);
    crate::status::set_delivery_note("deferred");
    match hold_until_reset(staging) {
        Some(until) => crate::status::record_held(staging, current_build, until, outcome),
        None => crate::status::record(staging, current_build, outcome),
    }
}

/// Write a WEB-LANE deferral: the plain deferred record (the loop's back-off ladder
/// applies; there is no budget window to hold to), no `health.toml` entry.
fn record_web_deferral(staging: &Staging, current_build: u64, outcome: &str) {
    RATE_LIMITED.store(true, Ordering::Relaxed);
    crate::status::set_delivery_note("deferred");
    crate::status::record(staging, current_build, outcome);
}

/// Tell the status ledger which lane this check is on and — on the token lane — what
/// budget its LIST measured, so `aterm ctl update status` can say `lane=… budget=…`
/// from the file alone. Only what the headers actually carried is recorded: an absent
/// header is an absent field, never a guess.
///
/// The token lane is recorded as `token:<rung id>` — the rung's FIXED identifier
/// ([`token::rung_id`]: `env`, `keychain`, `file`, `github-env`, `gh-env`, `gh-cli`),
/// never its operator-facing label. Three labels contain spaces, and `lane=` is one
/// token of a space-separated status line; a label there split the line into three
/// (2026-09-04 audit).
fn note_delivery(staging: &Staging, lane: Lane, token_source: Option<&str>) {
    let (label, budget) = match lane {
        Lane::Token => {
            let mut label = String::from("token");
            if let Some(source) = token_source {
                label.push(':');
                label.push_str(token::rung_id(source));
            }
            (
                label,
                aterm_update_core::rate_limit_from_header_dump(&staging.list_headers()),
            )
        }
        Lane::Web | Lane::Unknown => (String::from("web"), None),
    };
    crate::status::set_delivery(
        label,
        budget.and_then(|b| b.remaining),
        budget.and_then(|b| b.limit),
        budget.and_then(|b| b.reset),
    );
}

/// Whether `source` is the compiled-in public channel — the one source on which a
/// token is ignored outright (see the module doc). GitHub slugs are case-insensitive,
/// so `ATERM_UPDATE_OWNER=Alabsystems` is the public channel too — not a "repointed"
/// source that walks the token chain every check and puts an ambient `gh auth token`
/// on the API lane (2026-09-04 review).
fn is_default_channel(source: &Source) -> bool {
    is_slug(source, crate::DEFAULT_OWNER, crate::DEFAULT_REPO)
}

/// Whether `source` names `owner/repo`, the way GitHub compares slugs (ASCII
/// case-insensitively).
fn is_slug(source: &Source, owner: &str, repo: &str) -> bool {
    source.owner.eq_ignore_ascii_case(owner) && source.repo.eq_ignore_ascii_case(repo)
}

/// Record that the channel head was READ on `lane`. This — not the token chain — is
/// what clears the "this machine cannot update" latch: reading the channel is the
/// property that matters, and on the web lane it holds with no credential.
fn note_readable(lane: Lane, source: &Source) {
    LANE.store(
        match lane {
            Lane::Token => 1,
            Lane::Web => 2,
            Lane::Unknown => 0,
        },
        Ordering::Relaxed,
    );
    RATE_LIMITED.store(false, Ordering::Relaxed);
    RATE_LIMIT_RESET.store(0, Ordering::Relaxed);
    if lane == Lane::Token {
        TOKEN_REJECTED.store(false, Ordering::Relaxed);
    }
    crate::no_token::clear();
    if lane == Lane::Web && !ANNOUNCED_LANE.swap(true, Ordering::Relaxed) {
        crate::log(&format!(
            "updating from github.com/{}/{} over its unmetered download host with no \
             credential — {}; nothing on this lane touches the GitHub API, and it checks \
             on a {}-minute interval",
            source.owner,
            source.repo,
            if is_default_channel(source) {
                "the public channel ignores every update-token rung"
            } else {
                "no update token resolved for this source"
            },
            crate::cadence::WEB_INTERVAL_SECS / 60
        ));
    }
}

/// The lane annotation appended to a HEALTHY status outcome, so `status.toml` /
/// `aterm-ctl update status` answers "why is this machine slow to update?" on its own.
///
/// Empty on the token lane (the 75-second cadence the crate docs describe), so no
/// existing status wording changes for a provisioned repointed machine.
///
/// The web-lane wording is per-channel: on the compiled-in PUBLIC channel a token is
/// ignored outright, so a status that said "no update token provisioned" NEXT TO a
/// file the installer wrote read as a contradiction, and the file-writing "fix" it
/// suggested is a no-op there. A REPOINTED channel walks the whole chain, so for it the
/// missing token stays the named, remediable cause. The note reports the interval
/// ACTUALLY in effect (an operator's `ATERM_UPDATE_INTERVAL_SECS` wins over the lane
/// default and must not be misreported as it), and a REJECTED provisioned token names
/// rotation — the opposite remedy from provisioning — via the `TOKEN_REJECTED` latch.
fn lane_note(source: &Source) -> String {
    if lane() != Lane::Web {
        return String::new();
    }
    let secs = std::env::var("ATERM_UPDATE_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(crate::cadence::WEB_INTERVAL_SECS);
    let every = if secs >= 120 && secs.is_multiple_of(60) {
        format!("{}-minute", secs / 60)
    } else {
        format!("{secs}-second")
    };
    let why = if TOKEN_REJECTED.load(Ordering::Relaxed) {
        "a provisioned update token was rejected by GitHub — rotate it"
    } else if is_default_channel(source) {
        "the public channel is read with no credential and ignores every update-token \
         rung, so a provisioned update-token file serves only a repointed updater"
    } else {
        "no update token provisioned"
    };
    format!(
        " — checking over the unmetered web lane on a {every} interval ({why}; no GitHub \
         API request is made)"
    )
}

/// What a failed token-lane releases-LIST response says the check should do. Split out
/// as a pure function of the classified error so the ladder is unit-testable without a
/// network, a token, or an installed `.app`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListDecision {
    /// GitHub rejected the resolved token: drop this check onto the web lane (the
    /// source now has no usable credential, which is that lane's definition).
    TokenRejected,
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
/// update — a source with no token is simply on the web lane — so only a network
/// response may decide that, in [`resolve_web_head`] and [`classify_list_error`].
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

/// Classify a failed TOKEN-LANE releases-LIST request. Every request on this lane
/// carried the resolved token, so a 401/403 is about that token and a 404 is a token
/// that cannot see the repo (a real, actionable auth problem — never the no-token
/// wording, which belongs to the web lane's pointer).
pub(crate) fn classify_list_error(error: &HttpError, source: &Source) -> ListDecision {
    match error {
        HttpError::RateLimited { .. } => ListDecision::RateLimited(error.to_string()),
        // A resolved token that GitHub rejects must not brick a machine whose channel
        // is public: the ambient `gh auth token` the chain may have picked up can be
        // stale, scoped elsewhere, or revoked. The web lane needs no credential.
        HttpError::Unauthorized { .. } => ListDecision::TokenRejected,
        // A renamed/transferred repository: the REST API answers a permanent
        // redirect on the OLD slug forever (git-level redirects keep clones
        // working, so nothing else on the machine breaks visibly), and api_get
        // follows no redirects on purpose — the URL embeds the trusted slug.
        // This is a STANDING configuration state, not weather: filing it as a
        // `network` failure parked it in the one class the health ledger
        // deliberately never escalates, so a whole fleet could stall silently
        // (round-11 audit). Blocked is the honest classification — announced
        // once, with the remedy.
        HttpError::Status {
            code: 301 | 308, ..
        } => ListDecision::Blocked(moved_explanation(source)),
        // Everything else — including 404 WITH a token — is today's failure path.
        _ => ListDecision::Failed(error.to_string()),
    }
}

/// The standing-state wording for a renamed or transferred repository.
fn moved_explanation(source: &Source) -> String {
    format!(
        "aterm's release channel github.com/{}/{} answers HTTP 301 (moved permanently): \
         the repository was renamed or transferred, and this machine will keep asking the \
         OLD name — and never receive an update — until an operator repoints it: set \
         `[update] owner`/`repo` in aterm's config, or $ATERM_UPDATE_OWNER / \
         $ATERM_UPDATE_REPO, to the repository's new location.",
        source.owner, source.repo
    )
}

/// The one message an operator gets when this machine cannot read its release
/// channel — the evergreen pointer answered 404 and there is no credential to try. It
/// must survive being read months later out of `status.toml`, so it names the
/// consequence, every possible cause, and the exact remedy for each.
///
/// GitHub renders "no published release", "private" and "does not exist" identically
/// on the web host, so all three are named. On the compiled-in public channel a token
/// is IGNORED, so the remedy for the private case is not offered there — a user who
/// followed it would watch nothing change and have no way to tell why (2026-08-19).
fn unreadable_explanation(
    code: u16,
    source: &Source,
    diagnosis: Option<&token::Diagnosis>,
) -> String {
    if is_default_channel(source) {
        return format!(
            "aterm cannot read its release channel github.com/{}/{} (HTTP {code}): the \
             channel has no published release, or the repository was made private, renamed \
             or removed. The public channel is read with no credential — every update-token \
             rung is ignored for it — so this machine will NEVER receive an update until \
             the channel is repaired at github.com/{}/{}, or the updater is repointed via \
             `[update] owner`/`repo` in aterm's config, or $ATERM_UPDATE_OWNER / \
             $ATERM_UPDATE_REPO.",
            source.owner, source.repo, source.owner, source.repo
        );
    }
    // A token that was PRESENT and refused by our own chain ("chmod 600 it") is the
    // actionable case and must never be collapsed into "not configured" — and neither
    // may a token the chain resolved and GITHUB rejected, which is why this check is on
    // the web lane at all (`Listing::TokenRejected`): the `TOKEN_REJECTED` latch, not
    // the diagnosis (the chain kept none: it succeeded), carries that fact, and the
    // remedy is rotation, the opposite of provisioning (2026-09-04 review).
    let rejections = diagnosis
        .map(token::Diagnosis::rejections)
        .unwrap_or_default();
    let remedy = token::provision_remedy(&source.owner, &source.repo);
    let (chain, private) = if TOKEN_REJECTED.load(Ordering::Relaxed) {
        (
            "a provisioned update token was rejected by GitHub".to_string(),
            format!("rotate the token, then re-provision it by running: {remedy}"),
        )
    } else if rejections.is_empty() {
        (
            "no update token is provisioned".to_string(),
            format!("provision a token by running: {remedy}"),
        )
    } else {
        (
            format!(
                "a token is present but was refused: {}",
                rejections.join("; ")
            ),
            format!("repair it, or provision a token by running: {remedy}"),
        )
    };
    format!(
        "aterm cannot read its release channel github.com/{}/{} (HTTP {code}) and {chain}, \
         so this machine will NEVER receive an update until it is fixed. GitHub answers the \
         same way for every cause and cannot tell them apart, so check all four: (1) the \
         channel is PRIVATE — {private}; (2) the channel has no published release yet; (3) \
         the repository does not exist; (4) the configured channel is wrong — check \
         `[update] owner`/`repo` in aterm's config, or $ATERM_UPDATE_OWNER / \
         $ATERM_UPDATE_REPO.",
        source.owner, source.repo,
    )
}

/// A GitHub Release (subset). Unknown fields are ignored. On the token lane it is
/// parsed out of the LIST; on the web lane it is SYNTHESIZED from the pointer's tag
/// ([`web_release`]) with the derived tag-specific URLs, so the election, the roster
/// chain and the staging tail consume one shape on both lanes.
#[derive(Clone, Debug, Deserialize)]
struct Release {
    /// Release-list order is not a GitHub REST contract. The canonical tag is
    /// therefore the updater's explicit ordering key.
    tag_name: String,
    /// Draft (unpublished) releases are visible to a write-capable token but must
    /// never be staged to the fleet — skip them (F12). (The web lane never sees one:
    /// GitHub's `latest` excludes drafts and prereleases by construction.)
    #[serde(default)]
    draft: bool,
    /// A prerelease is published but is not the channel head — GitHub's `latest`
    /// excludes it by construction, and the web lane's LIST fallback
    /// ([`web_head_fallback`]) must apply the same rule or the two lanes would
    /// disagree about what "newest published release" means. The election
    /// ([`select_authoritative_release`]) applies it too, so the token lane's raw
    /// LIST obeys it as well.
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// A release asset (subset).
#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
    /// Where the bytes come from: the asset's API URL (`…/releases/assets/<id>`) on
    /// the token lane, the derived `…/releases/download/<tag>/<name>` on the web lane.
    url: String,
    /// The API's reported size on the token lane; `0` ("unknown") on the web lane.
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

#[derive(Debug)]
struct AuthoritativeRelease {
    tag: Vec<u64>,
    version: String,
    release: Release,
    manifest_index: usize,
    signature_index: Option<usize>,
}

/// A release still in max arbitration: its (unambiguous, already-parsed) tag, and either
/// the proven-unique appcast asset index or the reason its update metadata is POISONED
/// (duplicate exact-name assets). Poisoning is carried rather than propagated because it
/// must only be fatal if this release WINS — see the loop comment in
/// [`select_authoritative_release`].
struct ArbitratedRelease {
    tag: Vec<u64>,
    release: Release,
    manifest_index: Result<usize, String>,
}

/// Select the authoritative exact-name appcast without trusting REST response
/// order. Every page must already have been collected before this runs.
fn select_authoritative_release(
    releases: Vec<Release>,
    pinned_update_pubkeys: &[&str],
) -> Result<Option<AuthoritativeRelease>, String> {
    let mut seen_tags = std::collections::BTreeSet::new();
    let mut selected: Option<ArbitratedRelease> = None;

    for release in releases {
        // Drafts and prereleases are never the channel head, on EITHER lane: the web
        // lane's inputs are already filtered, and applying the rule HERE is what keeps
        // the token lane's raw LIST from electing a prerelease — or, under an
        // `-rc` tag, failing every check closed on one (2026-09-12).
        if release.draft || release.prerelease {
            continue;
        }
        // A release with no exact-name appcast is not a candidate at all. A release whose
        // appcast asset name appears TWICE is a candidate with POISONED metadata: it still
        // competes in max arbitration under its unambiguous tag, carrying the error instead
        // of an index. The error becomes fatal only if the poisoned release is the selected
        // MAXIMUM (the winner-only gate after the loop). Erroring here — before arbitration
        // — let a duplicate asset on an old, strictly-lower release wedge the whole check
        // even though that release could never be elected; a losing release simply loses,
        // and failing closed over it defended nothing.
        let manifest_index = match unique_asset_index(&release, "aterm-appcast.toml") {
            Ok(Some(index)) => Ok(index),
            Ok(None) => continue,
            Err(error) => Err(error),
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
        let candidate = ArbitratedRelease {
            tag,
            release,
            manifest_index,
        };
        if selected
            .as_ref()
            .is_none_or(|current| candidate.tag > current.tag)
        {
            selected = Some(candidate);
        }
    }

    let Some(winner) = selected else {
        return Ok(None);
    };
    // THE WINNER-ONLY GATE. A poisoned maximum fails the whole check closed — never
    // elect a runner-up behind a broken winner (the same rationale as the noncanonical
    // -tag rule: the runner-up would be a silent downgrade candidate).
    let manifest_index = winner.manifest_index?;
    let mut candidate = AuthoritativeRelease {
        version: String::new(),
        tag: winner.tag,
        release: winner.release,
        manifest_index,
        signature_index: None,
    };
    candidate.version = canonical_authority_version(&candidate.release.tag_name, &candidate.tag)?;
    if pinned_update_pubkeys.is_empty() {
        return Ok(Some(candidate));
    }
    candidate.signature_index = Some(
        unique_asset_index(&candidate.release, "aterm-appcast.toml.sig")?.ok_or_else(|| {
            format!(
                "authoritative update {} is unsigned under the pinned channel",
                candidate.release.tag_name
            )
        })?,
    );
    Ok(Some(candidate))
}

/// Everything the master-signed machine roster tier needs to run, bundled so the client
/// path takes ONE parameter rather than three and so a caller cannot supply two of them
/// and forget the third.
///
/// [`RosterPolicy::INERT`] is the fail-closed default: no master pinned, which makes
/// `verify_roster` return `Disabled` for every input and authorizes nothing. With
/// `master_pubkeys` empty the tier is ABSENT and the compiled-in channel keyset is the
/// authority — the same shape as an empty `APPLE_TEAM_ID` removing the Developer-ID tier
/// without loosening anything beside it.
///
/// THAT IS NOT THIS TREE. This sentence used to end "exactly as it is in every shipped
/// build", which stopped being true on 2026-08-15 when `pins::PAPER_MASTER_PUBKEYS` was
/// armed (`atpkg-keys setup --id m3`). The empty-anchor shape is now the PRE-ROSTER /
/// FORK path — a checkout that has not committed a master of its own — and every build
/// cut from this tree takes the armed one, so the armed branch is production code and
/// must be read as such.
///
/// ARMED — the production path here — this tier does not sit BESIDE the keyset gate, it
/// REPLACES it: the roster is
/// the sole authority over who may have signed the appcast. See
/// [`fetch_authoritative_release`] for why an OR of the two would give up revocation,
/// which is the one thing a compiled-in keyset can never express.
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
    ///
    /// `#[cfg(test)]` because production never names it: the real path always builds a
    /// policy from `pins::PAPER_MASTER_PUBKEYS`, which has been ARMED since 2026-08-15 —
    /// so production never reaches the off state through this constant, and in this tree
    /// never reaches it at all. (This doc used to say the anchor "is empty today and
    /// therefore already inert", which described the pre-arming tree. A fork with no
    /// master of its own gets the off state from its own empty anchor, not from here.)
    /// Two ways to spell "off" would be one too many, and this is the one that is only a
    /// fixture.
    #[cfg(test)]
    pub(crate) const INERT: RosterPolicy<'static> = RosterPolicy {
        master_pubkeys: &[],
        floor_seq: 0,
        now_unix: 0,
        floor_refresh: None,
    };
}

#[derive(Default)]
struct AuthoritativeFetch {
    /// Manifest, its release, and the already-proved unique canonical container
    /// (zip when the manifest carries a resolvable one, else the DMG).
    selected: Option<(Manifest, Release, StageArtifact)>,
    appcast_fetch_error: bool,
    /// The `appcast_fetch_error` was the MANIFEST ITSELF answering 404 on the web
    /// host: the release the pointer names exists but carries no `aterm-appcast.toml`
    /// — the SOURCE-ONLY channel head every promote opens for ~19 minutes (and for as
    /// long as an app cut is late). Not a pipeline fault; the web lane answers it by
    /// electing the newest release that does carry one ([`web_head_fallback`]).
    appcast_missing: bool,
    /// The `appcast_fetch_error` was a GitHub RATE LIMIT on an asset fetch, not a
    /// broken download: a token's API window running out, or a `github.com` 429 —
    /// weather on either lane, and never a `pipeline`-class failure.
    asset_fetch_rate_limited: bool,
    manifest_rejected: bool,
    /// Candidate-manifest fetches only. Detached-signature downloads are a
    /// subordinate verification step and intentionally do not increment this.
    #[cfg(test)]
    manifest_fetch_attempts: u32,
    /// Detached-signature transport attempts, exposed only for Tier-1
    /// projection onto the bounded channel-scan model.
    ///
    /// Counted on the COMPILED-IN KEYSET path only. With the master armed the appcast
    /// signature is fetched inside [`authorize_by_roster`], which owns the whole armed
    /// transport sequence; the model this counter feeds is about the unarmed path.
    #[cfg(test)]
    signature_fetch_attempts: u32,
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
/// With the master ARMED this is the ONLY thing that authorizes a release: the
/// compiled-in channel keyset is not consulted, and cannot refuse what this accepts. See
/// [`fetch_authoritative_release`] for why that is the correct reading of the two tiers.
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

    // WHERE THE APPCAST SIGNATURE IS, resolved here rather than taken from the caller.
    //
    // `select_authoritative_release` records the index only when the compiled-in keyset
    // is pinned, because that is the gate it was written for. The roster tier must not
    // inherit that condition: a fork (or a fleet that has finished rolling over) may arm
    // the master and hold an EMPTY keyset, and requiring a signature index the keyset
    // gate happened to fill in would make the armed tier refuse every release for a
    // reason that has nothing to do with the roster. So the index is reused when it is
    // there and located here when it is not — the same unique-asset rule either way.
    let signature_index = match candidate.signature_index {
        Some(index) => index,
        None => unique_asset_index(&candidate.release, "aterm-appcast.toml.sig")
            .map_err(|e| Refused(format!("locate the appcast signature: {e}")))?
            .ok_or_else(|| {
                Refused(format!(
                    "the paper master is pinned but {} carries no appcast signature",
                    candidate.release.tag_name
                ))
            })?,
    };

    // (1) Structural, free.
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
            Refused(format!(
                "machine roster did not verify under the pinned master ({e:?})"
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

/// Fetch and validate exactly one candidate after the complete metadata pass.
/// Older appcasts are never downloaded, regardless of REST row order.
fn fetch_authoritative_release(
    candidate: Option<AuthoritativeRelease>,
    pinned_update_pubkeys: &[&str],
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
            fetched.asset_fetch_rate_limited =
                aterm_update_core::download_error_is_rate_limit(&error);
            fetched.appcast_missing = aterm_update_core::download_error_is_not_found(&error);
            return fetched;
        }
    };
    // ---------------------------------------------------------------------------
    // WHO IS ALLOWED TO HAVE SIGNED THIS APPCAST — in one of exactly two states.
    //
    // The states are chosen by the PAPER MASTER ANCHOR, and nothing else. They are not
    // two gates that both run; that arrangement is what this replaced, and it made the
    // roster unable to do the one job the owner asked of it.
    //
    // (A) NO MASTER PINNED — the compiled-in keyset decides, exactly as it always has.
    //     This was every build shipped BEFORE 2026-08-15; since the paper master was
    //     armed in `pins::PAPER_MASTER_PUBKEYS` this is the PRE-ROSTER / FORK branch —
    //     a checkout with no master of its own — and (B) is what every build cut from
    //     this tree takes. It is still deliberately unchanged code rather than a
    //     re-expression of it:
    //     `select_authoritative_release` yields exactly ONE candidate with no fallback
    //     to an older release, so a client that meets a release it cannot verify does
    //     not wait — it is WEDGED there permanently. Any behaviour change on this path
    //     is a fleet-bricking bug.
    //
    // (B) MASTER ARMED — THE PRODUCTION PATH IN THIS TREE, since 2026-08-15; (A) above
    //     is the compatibility shape a fork with no pinned master takes. The
    //     master-signed roster decides, and it decides ALONE. The
    //     keyset is not consulted, so it cannot refuse a machine the roster authorized;
    //     that is the whole point of the tier, because it is what makes adding a machine
    //     a LOCAL act (mint, roster, publish) instead of one that needs a release cut
    //     from a machine that can already sign.
    //
    // # So what is the keyset FOR once the master is armed?
    //
    // It is the PRE-ROSTER COMPATIBILITY ALLOWANCE, and nothing else. It is not dead —
    // it is the only thing a client that predates the roster has, and those clients are
    // real and in the field — but it is not a second allowance HERE either, and the
    // difference is not cosmetic. Accepting "keyset OR roster" would mean a machine the
    // owner had REVOKED could keep publishing to every client whose build happens to
    // carry its key, forever, because a compiled-in key cannot be un-shipped. Revocation
    // is precisely what the roster exists to provide, so an OR would buy compatibility
    // by giving up the tier's reason to exist. The obligation to old clients therefore
    // lives where it can actually be discharged — at the PRODUCER, which chooses the
    // signing key (`aterm_release::publish::channel_signature_policy`) — and this client
    // reports the mismatch rather than enforcing it (see the note after the chain).
    // ---------------------------------------------------------------------------
    let attribution = if roster.master_pubkeys.is_empty() {
        // (A) ------------------------------------------------------------------
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
                    fetched.asset_fetch_rate_limited =
                        aterm_update_core::download_error_is_rate_limit(&error);
                    return fetched;
                }
            };
            match sig::verify_detached_any(pinned_update_pubkeys, &bytes, &sigbytes) {
                // Index 0 is the key this build would sign with. Any other member is an
                // outgoing key inside its retirement window (or one pre-seeded ahead of a
                // rotation): still authoritative, but worth saying out loud so a stalled
                // rotation is visible in the log rather than silent until the key is
                // dropped. At INFO, not WARN: the signature VERIFIED against a pinned
                // key, so this is rotation visibility, not verification degradation —
                // the `Err` arm below is what degradation looks like.
                Ok(0) => {}
                Ok(index) => crate::log(&format!(
                    "release manifest for {} was signed by channel key #{index}, not the current \
                     one — a key rotation is in progress or incomplete",
                    candidate.release.tag_name
                )),
                Err(error) => {
                    crate::warn(&format!(
                        "release manifest signature did not verify ({error:?}); refusing authoritative {}",
                        candidate.release.tag_name
                    ));
                    fetched.manifest_rejected = true;
                    return fetched;
                }
            }
        }
        None
    } else {
        // (B) ------------------------------------------------------------------
        // The chain runs here, over the RAW bytes and before the parse below, which is
        // the only correct place for it: the identity claims INSIDE the appcast are not
        // bound to anything until something has verified the bytes that carry them, and
        // binding them requires a parse. So the crypto happens here and the cheap
        // identity cross-check happens immediately after the parse (`bind`, below).
        //
        // The observed sequence lands in `fetched` on EVERY arm, including the refusals:
        // it was set the moment a master-verified roster passed admission, and the
        // caller's ratchet must advance on that observation even when the release itself
        // is refused (see `AuthoritativeFetch::observed_roster_seq`).
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
            // Nothing is said about the signer HERE: the keyset comparison is only a
            // compatibility note, and it rides the attribution line after the parse
            // (one INFO line — see below). It used to be a separate WARN at this spot,
            // which stated the same fact twice and made every check of a release signed
            // after a routine key rotation open with a warning about a fully verified
            // signature.
            Ok(who) => Some(who),
            // FAIL CLOSED, and never back to the keyset. Falling back would make the
            // roster advisory: an attacker who could suppress the two roster assets
            // would downgrade every armed client to the tier the roster replaced, and a
            // revoked machine's key would start working again.
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
                crate::warn(&format!(
                    "{error}; refusing authoritative {}",
                    candidate.release.tag_name
                ));
                fetched.manifest_rejected = true;
                return fetched;
            }
        }
    };

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
            // THE IDENTITY BIND, over bytes that are authenticated by the time it runs.
            // A genuine signature by one machine cannot be relabelled as another (the id
            // is inside the signed bytes), and a machine cannot claim someone else's id
            // (the roster maps id to key). Both directions are string compares.
            if let Some(who) = &attribution
                && let Err(reject) = who.bind(manifest.machine_id.as_deref(), manifest.roster_seq)
            {
                crate::warn(&format!(
                    "authoritative {} verified under machine {} but its own attribution \
                     does not agree ({reject:?}); refusing",
                    candidate.release.tag_name, who.machine_id
                ));
                fetched.manifest_rejected = true;
                return fetched;
            }
            if let Some(who) = &attribution {
                // ATTRIBUTION, recorded where a human reads it. The owner asked to be able
                // to track which computer does what; this is that answer for the client
                // half, beside the release it applies to.
                //
                // The keyset comparison is a compatibility NOTE riding the same line — not
                // a gate, and not a warning. A signer outside this build's compiled-in
                // keyset is the ordinary state of every build installed after a key
                // rotation: the master-signed roster authorized it and verification fully
                // succeeded, and the note's one consequence (clients OLDER than the
                // roster cannot verify this release, and have no fallback) is a fleet
                // observation that keeps a split fleet diagnosable from a user's machine
                // — not a defect on THIS machine. It used to be a separate WARN ahead of
                // this line, so a pristine install's first check warned about its own
                // verified signature and then restated the fact at INFO — training people
                // to ignore exactly the warnings that matter. WARN on this path is
                // reserved for actual verification degradation: the roster refuses the
                // signer, a signature fails, or the roster itself cannot be verified
                // (all handled above).
                if !pinned_update_pubkeys.is_empty()
                    && !pinned_update_pubkeys.contains(&who.pubkey_b64.as_str())
                {
                    crate::log(&format!(
                        "authoritative {} was signed by machine {who}, whose key is not in \
                         this build's channel keyset — the master-signed roster authorizes \
                         it, but clients older than the roster cannot verify this release",
                        candidate.release.tag_name
                    ));
                } else {
                    crate::log(&format!(
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
        Some(ready) => format!(
            "staged {} (build {}) — verified and ready to apply; release build {} needs \
             no download",
            ready.version, ready.build_number, manifest.build_number
        ),
        None => format!(
            "a verified stage already covers release build {}",
            manifest.build_number
        ),
    };
    crate::status::record(staging, current_build, &msg);
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

/// One release-listing page (GitHub's maximum) and the page-walk safety cap, for the
/// TOKEN lane's LIST.
const PER_PAGE: u32 = 100;
// 30 pages = 3000 releases — parity with tools/install.sh's anonymous walk, whose
// comment calls a catalog past that "not a real state". The old cap of 10 was
// reachable years out (the channel repo also accumulates non-app releases, e.g.
// the atpkg index), and a cap-hit burned every list request per check while
// failing forever (round-11 audit).
const MAX_PAGES: u32 = 30;

/// The listing URL for one page — the exact string the walk has always built.
fn releases_page_url(source: &Source, page: u32) -> String {
    format!(
        "https://api.github.com/repos/{}/{}/releases?per_page={PER_PAGE}&page={page}",
        source.owner, source.repo
    )
}

/// The exact asset names a release carries for the updater. The election and the
/// roster chain look them up by name on both lanes; the web lane also DERIVES its
/// URLs from them ([`web_release`]).
const APPCAST_ASSET: &str = "aterm-appcast.toml";
const APPCAST_SIG_ASSET: &str = "aterm-appcast.toml.sig";

/// The injected TOKEN-LANE page fetcher: `(url, token) → body`. Production passes
/// [`aterm_update_core::api_get_with_headers`] with the header sink; tests pass a
/// counting fake.
type ListFetch<'a> = &'a mut dyn FnMut(&str, &str) -> Result<Vec<u8>, HttpError>;

/// Everything one listing-page GET needs beside the URL: the staging surfaces it
/// reports into, the credential, and the injected fetcher.
struct ListContext<'a> {
    staging: &'a Staging,
    current_build: u64,
    source: &'a Source,
    tok: &'a str,
    /// Which rung of the token chain produced `tok`, for the ledger's `lane=` — so a
    /// LIST that is rate-limited on the token lane records the same `token:<rung>`
    /// spelling a readable one does.
    token_source: Option<&'a str>,
    fetch: ListFetch<'a>,
}

/// How a token-lane LIST ended.
enum Listing {
    /// Every page, in listing order.
    Releases(Vec<Release>),
    /// A NON-failure end — channel unreadable (announced) or rate limited (status
    /// recorded). The check is over.
    Ended,
    /// GitHub rejected the token: the check continues on the web lane.
    TokenRejected,
}

/// What one page GET produced.
enum Page {
    Body(Vec<u8>),
    Ended,
    TokenRejected,
}

impl ListContext<'_> {
    /// GET one listing page with the token. `Err` is a real failure, already recorded
    /// in the health ledger.
    fn page(&mut self, url: &str) -> Result<Page, String> {
        // A failed releases LIST is `network`-class: GitHub unreachable / auth broken.
        // (The transient/persistent distinction the ledger needs lives in the CLASS
        // split — an asset that provably exists but can't be fetched is `pipeline`,
        // recorded by the caller — so a broken download build can't hide behind
        // "transient".) The two states that are NOT failures — "cannot read the
        // channel at all" and "rate limited" — leave the ledger alone and say so.
        let error = match (self.fetch)(url, self.tok) {
            Ok(body) => {
                note_readable(Lane::Token, self.source);
                return Ok(Page::Body(body));
            }
            Err(error) => error,
        };
        match classify_list_error(&error, self.source) {
            ListDecision::TokenRejected => {
                // The lane note must say "rotate the token", not "provision
                // one" — a credential exists; GitHub refused it.
                TOKEN_REJECTED.store(true, Ordering::Relaxed);
                // Throttled: this is a STANDING condition (a stale token stays
                // stale), re-observed on every check, so an unthrottled warning
                // would be ~48 identical lines an hour.
                crate::no_token::note_rejected_credential(&format!(
                    "the configured update token was rejected ({error}); continuing over \
                     the unmetered web lane against github.com/{}/{} — updates still work \
                     while the channel is public, but rotate the token with: {}",
                    self.source.owner,
                    self.source.repo,
                    token::provision_remedy(&self.source.owner, &self.source.repo)
                ));
                Ok(Page::TokenRejected)
            }
            ListDecision::Blocked(explanation) => {
                crate::no_token::announce_unreadable(
                    self.staging,
                    self.current_build,
                    &explanation,
                );
                Ok(Page::Ended)
            }
            ListDecision::RateLimited(message) => {
                // Deliberately no `record_failure`: a rate limit is not a broken
                // pipeline, and letting it accrue a streak would fire the "update
                // pipeline is likely broken" notification at a healthy machine
                // that simply checked too often. The latch lengthens the wait.
                // The 403/429 answer carries this window's headers too, so the
                // ledger can name the lane, the budget and — when the server said
                // when it renews — the exact epoch siblings and the cadence hold to.
                note_delivery(self.staging, Lane::Token, self.token_source);
                record_deferral(
                    self.staging,
                    self.current_build,
                    &format!("update check deferred: {message}"),
                );
                Ok(Page::Ended)
            }
            ListDecision::Failed(message) => {
                crate::health::Health::record_failure(&self.staging.health(), "network", &message);
                Err(message)
            }
        }
    }
}

/// Enumerate the complete bounded release-metadata set on the TOKEN lane — the
/// historical walk.
///
/// GitHub documents no ordering contract for List Releases, so the caller chooses the
/// greatest canonical numeric vMAJOR.MINOR.PATCH tag carrying the exact appcast name
/// (retired two-component tags are skipped, never ordered), and only after that decision
/// fetches one manifest (+ one signature under Tier SIG) — row order cannot select an
/// older release and broken historical assets add no download latency.
fn list_releases(ctx: &mut ListContext<'_>) -> Result<Listing, String> {
    let mut release_catalog = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = releases_page_url(ctx.source, page);
        let body = match ctx.page(&url)? {
            Page::Body(body) => body,
            Page::Ended => return Ok(Listing::Ended),
            Page::TokenRejected => return Ok(Listing::TokenRejected),
        };
        // Unparseable list JSON is the same `network` class (the LIST layer failed —
        // a proxy/portal mangling the response looks exactly like this).
        let releases: Vec<Release> = match aterm_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("parse releases JSON: {e}");
                crate::health::Health::record_failure(&ctx.staging.health(), "network", &msg);
                return Err(msg);
            }
        };
        let page_len = releases.len();
        release_catalog.extend(releases);
        if page_len < PER_PAGE as usize {
            return Ok(Listing::Releases(release_catalog));
        }
        if page == MAX_PAGES {
            let msg = format!(
                "release listing reached the {MAX_PAGES}-page safety cap before exhaustion"
            );
            // `pipeline`, not `network`: a catalog past the cap is a PERMANENT
            // publishing state — it will still be past the cap next check — so it
            // must accrue toward the persistent-failure escalation instead of
            // hiding in the one class the ledger treats as transient weather
            // (round-11 audit).
            crate::health::Health::record_failure(&ctx.staging.health(), "pipeline", &msg);
            return Err(msg);
        }
    }
    // Unreachable: the loop returns on the short page and errors at MAX_PAGES.
    Ok(Listing::Releases(release_catalog))
}

// ---------------------------------------------------------------------------------
// THE WEB LANE
// ---------------------------------------------------------------------------------

/// The injected WEB-LANE pointer transport: `url → (status, Location)`. Production
/// passes [`aterm_update_core::head_no_redirect`]; tests pass a counting fake, which
/// is what makes "zero `api.github.com` requests" a measurement rather than a claim.
type HeadFetch<'a> = &'a mut dyn FnMut(&str) -> Result<HeadAnswer, HttpError>;

/// What the evergreen pointer said the channel head is, relative to what this ledger
/// last authorized.
enum WebHead {
    /// The pointer names the tag this ledger last authorized: nothing to fetch.
    Unchanged { tag: String },
    /// A tag this ledger has not authorized (or nothing was recorded): the synthesized
    /// candidate whose every asset URL is the derived tag-specific one.
    New(AuthoritativeRelease),
    /// A NON-failure end — the channel has no published release (announced, loud) or
    /// the host asked us to wait (deferred). The check is over.
    Ended,
}

/// The names of every asset the web lane may fetch for release `tag`, in the index
/// order [`web_release`] fixes: appcast, its signature, the roster, its signature, the
/// DMG, the zip. Names, not URLs — the URLs are derived from these under the strict
/// charset, so a name is the only thing a fetch can be addressed by.
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

/// The candidate the web lane elects for the pointer's `tag`: the same
/// [`AuthoritativeRelease`] shape the LIST election produces, with every asset's URL
/// the DERIVED `…/releases/download/<tag>/<name>` (a pure function of the trusted
/// source and the pointer's tag — never a server string) and the canonical version the
/// tag names, which the fetched manifest's `version` must then equal.
///
/// The signature index is recorded exactly when the LIST election would record it
/// (a pinned keyset), so branch (A) of [`fetch_authoritative_release`] behaves
/// identically on both lanes; the roster tier locates its assets by name either way.
fn web_release(
    source: &Source,
    tag: &str,
    pinned_update_pubkeys: &[&str],
) -> Result<AuthoritativeRelease, String> {
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
        assets.push(Asset { name, url, size: 0 });
    }
    Ok(AuthoritativeRelease {
        tag: numeric,
        version,
        release: Release {
            tag_name: tag.to_string(),
            draft: false,
            prerelease: false,
            assets,
        },
        manifest_index: 0,
        signature_index: (!pinned_update_pubkeys.is_empty()).then_some(1),
    })
}

/// ONE unmetered HEAD, and the decision it yields. Runs the web lane's whole
/// classification: a redirect to a canonical tag is the head; the ledger's `latest_tag`
/// decides whether anything else is fetched; a 404 is the loud standing state; a 429 or
/// 5xx is a deferral; a transport failure is the historical `network`-class failure.
///
/// `known_tag` is what `status.toml` recorded as last authorized. The comparison is
/// string equality on tags this function already proved canonical, so `v0.74.0`
/// recorded and `v0.74.0` pointed at is "unchanged", and nothing else is.
fn resolve_web_head(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    known_tag: Option<&str>,
    diagnosis: Option<&token::Diagnosis>,
    pinned_update_pubkeys: &[&str],
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
            crate::no_token::announce_unreadable(
                staging,
                current_build,
                &unreadable_explanation(404, source, diagnosis),
            );
            return Ok(WebHead::Ended);
        }
        Err(error @ PointerError::Transient { .. }) => {
            record_web_deferral(
                staging,
                current_build,
                &format!("update check deferred: {error}"),
            );
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
    note_readable(Lane::Web, source);
    if known_tag == Some(pointer.tag.as_str()) {
        return Ok(WebHead::Unchanged { tag: pointer.tag });
    }
    let candidate = web_release(source, &pointer.tag, pinned_update_pubkeys)?;
    // The pointer's own `Location` and the candidate's derived appcast URL are the same
    // string by construction (`parse_location` proved it); assert the construction
    // rather than trust it, because every later GET is addressed by the derived one.
    debug_assert_eq!(candidate.release.assets[0].url, pointer.location);
    Ok(WebHead::New(candidate))
}

/// What the web lane does about a channel head that carries no appcast.
#[derive(Debug)]
enum HeadFallback {
    /// The head's appcast was fetched, or failed for some other reason: the check
    /// proceeds on the head exactly as before, and the LIST was never asked for.
    NotNeeded,
    /// The newest published (non-draft, non-prerelease) release that DOES carry an
    /// appcast, synthesized under the derived tag-specific URLs like any web-lane
    /// candidate — the check proceeds on it.
    Candidate(AuthoritativeRelease),
    /// No published release carries an appcast at all: nothing to install from.
    Nothing,
    /// The LIST could not answer this check (rate-limited, or an election that failed
    /// closed); the outcome is already recorded and the check ends.
    Ended,
}

/// THE SOURCE-ONLY HEAD (2026-09-10, m21). The publication train mints the shared
/// `vX.Y.0` GitHub release from the SOURCE side first — with `SHA256SUMS` and a roster
/// copy, and no app assets — and the app cut attaches `aterm-appcast.toml` under the
/// same tag later: ~19 minutes on an ordinary promote, and indefinitely when no cut
/// follows (v0.80.0 sat source-only for a night). GitHub's `latest` pointer moves to
/// that release the moment it is published, so every credential-less client derived
/// `…/download/v0.80.0/aterm-appcast.toml`, met a 404, booked a `pipeline` failure and
/// told its owner "this build's download pipeline is likely broken" — while v0.79.0,
/// one release down, carried a perfectly good app build that the same client would
/// have installed from.
///
/// The answer is the rule `tools/install.sh`'s `select_authoritative_tag` and the
/// website's download button already apply: the newest non-draft, non-prerelease
/// release that carries the manifest, skipping releases with zero manifests. It costs
/// ONE anonymous LIST of the releases API — the only request the web lane ever makes
/// there, and only after the head's appcast has answered 404 — because the web host
/// serves no listing. At the web lane's cadence that is at most two metered requests
/// an hour against the ~60/hour anonymous budget, for as long as the head stays
/// source-only; a rate-limited LIST is a deferral, never a failure.
///
/// `head_tag` is excluded from the election by fact rather than by name (it carries
/// no appcast, so it cannot be elected), but the candidate is still checked against
/// it so the head can never be re-elected through a listing that disagrees with the
/// download host. The elected release is rebuilt through [`web_release`]: every later
/// GET is addressed by the DERIVED tag-specific URL, never by an asset URL the listing
/// handed back — the listing is consulted for tag and asset NAMES only.
///
/// Recorded outcome wording: the distinct sentence `channel head <tag> has no app
/// manifest` rides the check note onto every later record of this check, so
/// `aterm ctl update status` and the update screen say what is actually going on.
fn web_head_fallback(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    fetched: &AuthoritativeFetch,
    head_tag: &str,
    list: ListFetch<'_>,
    pinned_update_pubkeys: &[&str],
) -> Result<HeadFallback, String> {
    if !fetched.appcast_missing {
        return Ok(HeadFallback::NotNeeded);
    }
    crate::warn(&format!(
        "channel head {head_tag} has no app manifest (a source-only release); electing the \
         newest published release that carries one"
    ));
    let mut catalog: Vec<Release> = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = releases_page_url(source, page);
        // Anonymous: the empty credential is the web lane's, and the transport's own
        // host gate keeps it that way.
        let body = match list(&url, "") {
            Ok(body) => body,
            Err(error @ HttpError::RateLimited { .. }) => {
                record_web_deferral(
                    staging,
                    current_build,
                    &format!(
                        "update check deferred: channel head {head_tag} has no app manifest, \
                         and the release listing that would name the newest app release is \
                         rate-limited ({error}) — will retry on the next check"
                    ),
                );
                return Ok(HeadFallback::Ended);
            }
            Err(error) => {
                let message = format!(
                    "channel head {head_tag} has no app manifest, and the release listing \
                     could not be read: {error}"
                );
                crate::health::Health::record_failure(&staging.health(), "network", &message);
                return Err(message);
            }
        };
        let releases: Vec<Release> = match aterm_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                let message = format!(
                    "channel head {head_tag} has no app manifest, and the release listing \
                     did not parse: {e}"
                );
                crate::health::Health::record_failure(&staging.health(), "network", &message);
                return Err(message);
            }
        };
        let page_len = releases.len();
        catalog.extend(
            releases
                .into_iter()
                .filter(|r| !r.draft && !r.prerelease && r.tag_name != head_tag),
        );
        if page_len < PER_PAGE as usize {
            break;
        }
    }
    let elected = match select_authoritative_release(catalog, pinned_update_pubkeys) {
        Ok(elected) => elected,
        Err(error) => {
            record_untrustworthy_election(staging, current_build, &error);
            return Ok(HeadFallback::Ended);
        }
    };
    let Some(elected) = elected else {
        return Ok(HeadFallback::Nothing);
    };
    // Rebuilt under the derived URLs: the listing chose the TAG, and nothing else.
    let candidate = web_release(source, &elected.release.tag_name, pinned_update_pubkeys)?;
    Ok(HeadFallback::Candidate(candidate))
}

/// THE URL CROSS-CHECK, web lane only: the signed manifest names its container's
/// download URL, and that URL must name THE TAG the pointer chose and THE CONTAINER
/// the manifest itself names. The manifest field is never followed — the derived URL
/// is what is downloaded — so this binds the signed bytes to the tag (invariant (d):
/// tag == `v<version>` == the tag in the manifest's URL), and a re-published appcast
/// copied onto another tag cannot be elected under it.
///
/// What is bound is the TAG and the DMG name, not the repository: the publisher signs
/// `url` under the compiled-in public channel's slug (`manifest_out.rs` `repo_slug` =
/// `[workspace.metadata.aterm] update_channel`, the key `build.rs` stamps into
/// `DEFAULT_OWNER`/`DEFAULT_REPO`), so a credential-less updater REPOINTED at a mirror
/// that carries the upstream-signed appcast would otherwise refuse every release as a
/// `manifest`-class failure blamed on a publisher who did nothing wrong (2026-09-04
/// review). The repository half adds nothing to (d): `pointer::parse_location` already
/// scopes every GET after the HEAD to the source repository. The URL's slug must be the
/// source's or the compiled-in channel's (GitHub-style case-insensitively); anything
/// else is a manifest signed for a channel this updater does not read.
fn web_container_url_agrees(source: &Source, tag: &str, manifest: &Manifest) -> Result<(), String> {
    let Some(url) = manifest.url.as_deref() else {
        return Err(format!(
            "authoritative {tag} carries no container `url`; the web lane requires the \
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

/// The acquisition half of a check, on whichever lane the source put it: the elected
/// candidate (or none), and the lane it was elected on.
#[derive(Debug)]
struct Acquired {
    lane: Lane,
    /// The credential every asset fetch on this check rides with — `Some` on the token
    /// lane only. The transport refuses to pair it with a non-API host, so the web
    /// lane's `None` is structural, not policy.
    tok: Option<String>,
    candidate: Option<AuthoritativeRelease>,
    /// The pointer's tag, on the web lane: recorded as `latest_tag` on the terminal
    /// healthy outcomes so the next check can stop at the HEAD.
    web_tag: Option<String>,
}

/// How a check's acquisition ended.
#[derive(Debug)]
enum Acquisition {
    /// Proceed to authorization and staging.
    Proceed(Acquired),
    /// The web lane's steady state: the pointer names the last authorized tag.
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
    result
}

/// The acquisition half of [`check_and_stage_inner`]: pick the lane from the source and
/// the token chain, learn the channel head on it, and elect the candidate.
///
/// Injected transports, so the whole lane choice is measurable without a network: the
/// token lane's page fetcher and the web lane's HEAD. The asset fetches that follow use
/// the credential this returns, through the transport's own host gate.
fn acquire(
    staging: &Staging,
    current_build: u64,
    source: &Source,
    support: &Path,
    list: ListFetch<'_>,
    head: HeadFetch<'_>,
) -> Result<Acquisition, String> {
    // THE LANE CHOICE. On the compiled-in public channel the token chain is not even
    // consulted: a credential buys nothing on a host that reads none, and walking the
    // chain would re-spawn `security`/`gh` every check to gather a secret nothing uses.
    // A repointed source runs ONE walk of the chain — the token, or the diagnosis
    // explaining why there isn't one — and the outcome decides the lane.
    //
    // RESOLVE, DO NOT GATE: the absence of a token never ends a check here — a source
    // with no token is on the web lane, and only a network response may declare this
    // machine unable to update (`resolve_web_head`, `classify_list_error`).
    let (tok, token_source, diagnosis) = if is_default_channel(source) {
        (None, None, None)
    } else {
        let resolved = token::resolve_or_diagnose(support, &source.owner, &source.repo);
        let token_source = resolved.as_ref().ok().map(|(_, source)| *source);
        let (tok, diagnosis) = plan_credential(resolved);
        (tok, token_source, diagnosis)
    };

    if let Some(tok) = tok.as_deref() {
        // THE TOKEN LANE: today's LIST, byte for byte, with the token.
        let mut ctx = ListContext {
            staging,
            current_build,
            source,
            tok,
            token_source,
            fetch: list,
        };
        match list_releases(&mut ctx)? {
            Listing::Releases(releases) => {
                // The LIST answered on the token lane, and its headers are this check's
                // budget measurement.
                note_delivery(staging, Lane::Token, token_source);
                let candidate =
                    match select_authoritative_release(releases, crate::PINNED_UPDATE_PUBKEYS) {
                        Ok(candidate) => candidate,
                        Err(error) => {
                            record_untrustworthy_election(staging, current_build, &error);
                            return Ok(Acquisition::Ended);
                        }
                    };
                return Ok(Acquisition::Proceed(Acquired {
                    lane: Lane::Token,
                    tok: Some(tok.to_string()),
                    web_tag: candidate.as_ref().map(|c| c.release.tag_name.clone()),
                    candidate,
                }));
            }
            Listing::Ended => return Ok(Acquisition::Ended),
            // GitHub refused the credential: the source has no usable token, which is
            // the web lane's definition. Fall through with the diagnosis the rejection
            // implies (the chain did resolve something; it just does not work).
            Listing::TokenRejected => {}
        }
    } else if let Some(diagnosis) = diagnosis.as_ref() {
        // A token that our own chain refused (a chmod 644 file, a mangled paste)
        // still costs this repointed machine the token lane even though the web lane
        // may work. Say so, throttled, once in a while.
        crate::no_token::note_unusable_token(source, diagnosis);
    }

    // THE WEB LANE. The recorded tag is trusted only when the ledger still describes
    // THIS build's verdict on THIS source (`status::latest_tag`).
    let known_tag = crate::status::latest_tag(staging, current_build, source);
    match resolve_web_head(
        staging,
        current_build,
        source,
        known_tag.as_deref(),
        diagnosis.as_ref(),
        crate::PINNED_UPDATE_PUBKEYS,
        head,
    )? {
        WebHead::Ended => Ok(Acquisition::Ended),
        WebHead::Unchanged { tag } => {
            note_delivery(staging, Lane::Web, None);
            Ok(Acquisition::UpToDate { tag })
        }
        WebHead::New(candidate) => {
            note_delivery(staging, Lane::Web, None);
            Ok(Acquisition::Proceed(Acquired {
                lane: Lane::Web,
                tok: None,
                web_tag: Some(candidate.release.tag_name.clone()),
                candidate: Some(candidate),
            }))
        }
    }
}

/// The token-lane election failed closed (a duplicate candidate, a malformed tag, an
/// unsigned maximum): `manifest`-class, with the two-tier wording.
fn record_untrustworthy_election(staging: &Staging, current_build: u64, error: &str) {
    crate::warn(error);
    let h = crate::health::Health::record_failure(&staging.health(), "manifest", error);
    // Two-tier wording, exactly like the pipeline branch below. "deferred"
    // means postponed-and-will-retry, which is a lie for this class: an
    // untrustworthy authoritative release stays untrustworthy until the
    // PUBLISHER republishes, so retrying changes nothing. A machine sat at
    // failure 597 still being told its check was "deferred".
    let msg = if h.manifest_failures >= crate::PERSISTENT_AFTER {
        format!(
            "FAILING ({} consecutive checks since {}): {error} — this machine \
             cannot install any release until that is fixed at the publisher",
            h.manifest_failures,
            h.class_since("manifest")
        )
    } else {
        format!(
            "update check deferred: {error} (attempt {})",
            h.manifest_failures
        )
    };
    crate::status::record(staging, current_build, &msg);
}

/// Write a deferral for a rate-limit-shaped ASSET failure on `lane`: held to the
/// LIST's `x-ratelimit-reset` on the token lane (the API window is what ran out), the
/// plain deferred record on the web lane (a `github.com` 429 holds to nothing).
fn record_asset_deferral(staging: &Staging, lane: Lane, current_build: u64, outcome: &str) {
    match lane {
        Lane::Token => record_deferral(staging, current_build, outcome),
        Lane::Web | Lane::Unknown => record_web_deferral(staging, current_build, outcome),
    }
}

/// Where `lane`'s release assets come from, for the wording of a fetch failure, and
/// the `delivery=` note such a failure books — BY LANE, because the two lanes fetch
/// from different hosts and a message naming the wrong one sends an operator to
/// debug a host the check never spoke to (2026-09-04 audit). The web lane fetches
/// every asset from the unmetered download host (`github.com/…/releases/download/…`)
/// and books `blocked` — the host did not serve an asset the release names (a
/// filtering proxy, or a publish missing an asset); the token lane fetches through
/// the releases API and books `api-failed`. Neither note contains whitespace: it is
/// one token of the space-separated status line.
fn asset_source(lane: Lane) -> (&'static str, &'static str) {
    match lane {
        Lane::Token => ("the releases API", "api-failed"),
        Lane::Web | Lane::Unknown => ("the download host", "blocked"),
    }
}

/// A publishable stage strictly newer than the running build, as the check loop's
/// `Some` answer: "a build is staged and can be applied" — reported so the apply lane
/// arms even on a check that fetched nothing.
fn applicable_stage(staging: &Staging, current_build: u64) -> Option<String> {
    Ready::read_publishable(staging)
        .filter(|ready| ready.build_number > current_build)
        .map(|ready| ready.version)
}

/// Remove the state files a PREVIOUS design of this crate wrote into the `0700`
/// Updates root and nothing reads any more: `catalog.json` / `catalog.headers`, the
/// conditional-request memo (ETag + cached LIST body) of the API-lane builds up to
/// d15e9ff47 (v0.74.0). Without this every upgraded machine keeps a stale ETag memo
/// forever — `sweep_download_scratch` sweeps only `download/`.
fn reclaim_retired_state(staging: &Staging) {
    for name in ["catalog.json", "catalog.headers"] {
        let _ = std::fs::remove_file(staging.root.join(name));
    }
}

fn check_and_stage_inner(current_build: u64, source: &Source) -> Result<Option<String>, String> {
    // Only stage for a real installed bundle (a dev build has nothing to swap).
    if bundle::resolve().is_none() {
        return Ok(None);
    }
    let staging = Staging::resolve().ok_or("could not resolve Updates dir")?;
    reclaim_retired_state(&staging);
    crate::status::clear_check_note();
    // The hold epoch is THIS check's to set or not: a previous check's reset must never
    // be read back by the loop as this one's (a rate-limited LIST whose headers carry
    // no reset would otherwise "hold" to an epoch already in the past).
    RATE_LIMIT_RESET.store(0, Ordering::Relaxed);
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
    // The Application Support dir is the Updates dir's parent.
    let support = staging.root.parent().ok_or("no support dir")?.to_path_buf();

    // Persisted monotonic recency floor (operator yank + rollback guard, F5/F6).
    let floor = crate::manifest::Floor::read(&staging.floor());

    // The two transports, INJECTED rather than called directly, so the lane choice and
    // its request cost are measurable in a test. The token lane's header sink is a file
    // in the same `0700` Updates directory as the rest of the staging state; curl dumps
    // the LIST's response headers there so the `x-ratelimit-*` block can be read back
    // without corrupting the body it also writes to stdout. The web lane's HEAD needs
    // no sink: the headers ARE its answer.
    let header_sink = staging.list_headers();
    let mut list = |url: &str, token: &str| {
        // An EMPTY credential is the web lane's anonymous LIST (`web_head_fallback`,
        // after a source-only head): no `Authorization` header at all, rather than a
        // bearer token of nothing.
        let token = (!token.is_empty()).then_some(token);
        aterm_update_core::api_get_with_headers(url, token, Some(header_sink.as_path()))
    };
    let mut head = aterm_update_core::head_no_redirect;
    let acquired = match acquire(
        &staging,
        current_build,
        source,
        &support,
        &mut list,
        &mut head,
    )? {
        Acquisition::Proceed(acquired) => acquired,
        Acquisition::Ended => return Ok(None),
        Acquisition::UpToDate { tag } => {
            // THE STEADY STATE, on one request. The pointer names the tag this ledger
            // last authorized, so nothing is fetched and nothing is re-judged: the
            // release was accepted or declined on a previous check under the same
            // gates, and only a MOVED pointer can change that answer. A terminal
            // healthy outcome — the channel was read.
            crate::health::Health::record_success(&staging.health());
            crate::status::set_latest_tag(&tag, source);
            crate::status::record(
                &staging,
                current_build,
                &format!("up to date (channel head {tag}){}", lane_note(source)),
            );
            // …still answering `Some` for a stage a sibling won the race to publish,
            // so this process's apply lane arms too (see the covered arms below).
            return Ok(applicable_stage(&staging, current_build));
        }
    };
    let Acquired {
        lane,
        tok,
        candidate: authoritative,
        web_tag,
    } = acquired;
    // The asset fetches ride the credential the lane settled on: `Some(token)` to the
    // asset API on the token lane, `None` to the derived web URLs on the web lane. The
    // transport's own host gate (`refuse_credential_off_api`) makes the pairing
    // structural — a credential can never reach `github.com` from here.
    let mut download = |url: &str, max_bytes: u64| {
        aterm_update_core::download_bytes(url, tok.as_deref(), max_bytes)
    };
    // The roster tier's inputs, resolved from the anchor and this client's durable state.
    // `PAPER_MASTER_PUBKEYS` is ARMED (2026-08-15, `atpkg-keys setup --id m3`), so this
    // policy is LIVE: the master-signed roster — not the compiled-in channel keyset —
    // decides who may have signed the appcast this check accepts, and every field below
    // is load-bearing. This comment used to say the anchor was empty and the whole tier
    // a no-op, which is how a production path came to be read as dead code; the only
    // build still taking the unarmed keyset path is a fork that has not committed a
    // master of its own (branch (A) of `fetch_authoritative_release`).
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
    let mut fetched = fetch_authoritative_release(
        authoritative,
        crate::PINNED_UPDATE_PUBKEYS,
        &mut download,
        &roster_policy,
    );
    // THE SOURCE-ONLY HEAD, web lane only: the pointer named a release whose appcast
    // answers 404. Elect the newest published release that carries one and proceed
    // on it — `web_tag` follows, so the tag recorded as authorized is the release
    // actually judged, and the head itself is re-fetched on the next check (the cut
    // that attaches its app assets later must not be missed by a ledger that
    // "authorized" the tag while it carried nothing).
    let mut web_tag = web_tag;
    if lane == Lane::Web
        && let Some(head) = web_tag.clone()
    {
        match web_head_fallback(
            &staging,
            current_build,
            source,
            &fetched,
            &head,
            &mut list,
            crate::PINNED_UPDATE_PUBKEYS,
        )? {
            HeadFallback::NotNeeded => {}
            HeadFallback::Candidate(candidate) => {
                let tag = candidate.release.tag_name.clone();
                crate::status::set_check_note(format!(
                    "channel head {head} has no app manifest; the newest app release is {tag}"
                ));
                web_tag = Some(tag);
                fetched = fetch_authoritative_release(
                    Some(candidate),
                    crate::PINNED_UPDATE_PUBKEYS,
                    &mut download,
                    &roster_policy,
                );
            }
            HeadFallback::Nothing => {
                // The check itself ran fine — the head was read, the listing answered,
                // and nothing carries a manifest — so health reflects THIS check.
                crate::health::Health::record_success(&staging.health());
                crate::status::record(
                    &staging,
                    current_build,
                    &format!(
                        "channel head {head} has no app manifest, and no published release \
                         carries one{}",
                        lane_note(source)
                    ),
                );
                return Ok(None);
            }
            HeadFallback::Ended => return Ok(None),
        }
    }
    // THE URL CROSS-CHECK, after every signature has been verified and the version
    // bound, and before anything is staged: on the web lane the signed manifest must
    // name the very container URL this client derived under the pointer's tag.
    if lane == Lane::Web
        && let Some((manifest, release, _)) = fetched.selected.as_ref()
        && let Err(error) = web_container_url_agrees(source, &release.tag_name, manifest)
    {
        crate::warn(&format!(
            "{error}; refusing authoritative {}",
            release.tag_name
        ));
        fetched.selected = None;
        fetched.manifest_rejected = true;
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
            // The manifest/roster/signature GET met GitHub's throttle — the same
            // verdict a rate-limited LIST gets one request earlier, and for the same
            // reason no `record_failure`: a saturated check must not book a
            // persistent `pipeline` streak and fire the "download pipeline is likely
            // broken" notice at a perfectly healthy machine (2026-08-19 audit).
            // "deferred" is the substring every sibling's ledger gate reads.
            record_asset_deferral(
                &staging,
                lane,
                current_build,
                "update check deferred: GitHub rate limit hit while fetching a release \
                 asset — backing off, will retry on the next check",
            );
            return Ok(None);
        }
        let msg = if appcast_fetch_error {
            // Manifests exist but could not be downloaded while the channel head was
            // readable — a `pipeline`-class failure. The ledger decides the honest
            // wording: a streak ≥ PERSISTENT_AFTER is not called "deferred". Worded
            // and noted BY LANE: the host that did not answer is the one this lane
            // actually asked (`asset_source`).
            let (host, note) = asset_source(lane);
            crate::status::set_delivery_note(note);
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
            // own class — it must not clear a streak.
            let h = crate::health::Health::record_failure(
                &staging.health(),
                "manifest",
                "manifest(s) fetched but rejected (signature/parse)",
            );
            if h.manifest_failures >= crate::PERSISTENT_AFTER {
                format!(
                    "FAILING ({} consecutive checks since {}): manifest(s) fetched but \
                     rejected (signature/parse) — this machine cannot install any release \
                     until that is fixed at the publisher",
                    h.manifest_failures,
                    h.class_since("manifest")
                )
            } else {
                "no stageable release: manifest(s) fetched but rejected (signature/parse)"
                    .to_string()
            }
        } else {
            // The check itself ran fine (the head was read, nothing carries a
            // manifest): clear any stale failure streak so health reflects THIS check.
            crate::health::Health::record_success(&staging.health());
            format!("no release carries an update manifest{}", lane_note(source))
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
    // records `latest_tag`, so the next web-lane check stops at the HEAD.
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
        if let Some(tag) = web_tag.as_deref() {
            crate::status::set_latest_tag(tag, source);
        }
        crate::status::record(
            &staging,
            current_build,
            &format!(
                "up to date (latest release build {}){}",
                manifest.build_number,
                lane_note(source)
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
        if let Some(tag) = web_tag.as_deref() {
            crate::status::set_latest_tag(tag, source);
        }
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
        if let Some(tag) = web_tag.as_deref() {
            crate::status::set_latest_tag(tag, source);
        }
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
    // after this point. On the web lane the URL is the DERIVED one for the pointer's
    // tag (the manifest's own `url` was cross-checked above and is never followed).
    let asset = &release.assets[artifact.asset_index];
    let container = artifact.container.label();

    let part = staging.download.join(format!("{}.part", artifact.name));
    let container_path = staging.download.join(&artifact.name);
    sweep_download_scratch(&staging);
    // LIVE PROGRESS for a host that shows it (the aterm window's status bar): a
    // sibling poller stats the growing `.part` against the asset's declared size
    // — the API's `size` on the token lane, `0` on the web lane (and when the API
    // did not report one), which the host renders as "unknown". Joined on drop, so
    // it cannot outlive the download it watches.
    let download_watch = crate::progress::watch_download(&part, &manifest.version, asset.size);
    // A failed download is a `pipeline`-class ledger entry: the asset provably
    // exists (the release names it) but could not be fetched.
    let downloaded = aterm_update_core::download_to(
        &asset.url,
        tok.as_deref(),
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
            record_asset_deferral(&staging, lane, current_build, &note);
            // Answer the live channel too: the bar opened on the first byte and
            // a deferral is its honest end (the wrapper reports only `Err`s).
            if crate::progress::take_download_began() {
                crate::progress::report(crate::progress::Progress::Deferred { detail: note });
            }
            return Ok(None);
        }
        // The same by-lane note as the manifest leg: the container came from the
        // host this lane fetches from, and no other was asked.
        crate::status::set_delivery_note(asset_source(lane).1);
        crate::health::Health::record_failure(
            &staging.health(),
            "pipeline",
            &format!("{container} download failed: {e}"),
        );
        return Err(format!("{container} download failed: {e}"));
    }

    // The container arrived; everything from here to the publish is one "verifying"
    // phase on the live channel: size, digest, extract, codesign/Gatekeeper, the
    // atomic stage.
    crate::progress::report(crate::progress::Progress::Verifying {
        version: manifest.version.clone(),
    });
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
            // MEMOIZE, exactly as the post-stage failure below does. Without this the
            // bytes-arrived-but-wrong path had no retry budget at all: `stage_backoff`
            // consults only this memo, so an asset re-uploaded after its manifest was
            // signed (or a stale CDN object) was re-downloaded IN FULL every cycle —
            // ~3 GB/hour/machine, forever, while the sibling container whose identity
            // was already proven was never tried.
            crate::manifest::FailedMark::record_stage_failure(
                &staging.failed(),
                manifest.build_number,
                &manifest.sha256,
                crate::install::unix_now_secs(),
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
    // …and remember the tag, so the next web-lane check is one HEAD.
    if let Some(tag) = web_tag.as_deref() {
        crate::status::set_latest_tag(tag, source);
    }

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

    /// The web lane's standing "cannot read the channel" wording must survive being read
    /// months later out of `status.toml`: the consequence, every cause GitHub renders
    /// identically, and the remedy for each — never a bare "idle", which an operator
    /// cannot tell apart from "no updates available".
    #[test]
    fn an_unreadable_channel_is_loud_and_actionable_not_idle() {
        let source = test_source();
        let text = unreadable_explanation(404, &source, Some(&unprovisioned()));
        assert!(text.contains("NEVER receive an update"), "{text}");
        assert!(text.contains("github.com/alabsystems/aterm"), "{text}");
        assert!(text.contains("HTTP 404"), "{text}");
        // Every cause the web host cannot distinguish.
        assert!(
            text.contains("no published release"),
            "cause 1 missing: {text}"
        );
        assert!(text.contains("private"), "cause 2 missing: {text}");
        assert!(
            text.contains("ATERM_UPDATE_OWNER"),
            "cause 3 missing: {text}"
        );
        // On the compiled-in public channel a token is IGNORED, so no token remedy is
        // offered there: a user who followed it would watch nothing change.
        assert!(text.contains("ignored"), "{text}");
        assert!(
            !text.contains(token::PROVISION_COMMAND) && !text.contains("ATERM_UPDATE_TOKEN"),
            "a token remedy on the public channel is a no-op and must not be printed: {text}"
        );

        // A REPOINTED source walks the token chain, so there the private case has a
        // remedy and the refused-token sub-case is named rather than folded into "not
        // configured".
        let overridden = Source {
            owner: "someone-else".to_string(),
            repo: "private-aterm".to_string(),
        };
        let text = unreadable_explanation(404, &overridden, Some(&unprovisioned()));
        assert!(text.contains("PRIVATE"), "{text}");
        assert!(text.contains("does not exist"), "{text}");
        assert!(text.contains("no update token is provisioned"), "{text}");
        assert!(
            text.contains(token::provision_remedy(&overridden.owner, &overridden.repo)),
            "{text}"
        );
        use aterm_update_core::token::{ProbeOutcome, SourceProbe};
        let refused = token::Diagnosis {
            resolved: None,
            probes: vec![SourceProbe {
                source: "0600 update-token file",
                outcome: ProbeOutcome::Rejected("chmod 600 it"),
            }],
        };
        let text = unreadable_explanation(404, &overridden, Some(&refused));
        assert!(
            text.contains("0600 update-token file (chmod 600 it)"),
            "the refused source must be named: {text}"
        );
    }

    /// A missing token must not stop a check before the network. `plan_credential` is
    /// deliberately total — it has no value that means "stop" — so the only way to
    /// reintroduce a gate is to change its type, which this test pins. A source with no
    /// token is simply on the web lane, and only a network answer (the pointer's) may
    /// declare a machine unable to update.
    #[test]
    fn a_missing_token_never_stops_a_check_before_the_network() {
        let (tok, carried) = plan_credential(Err(unprovisioned()));
        assert!(tok.is_none(), "no token was resolvable");
        assert!(
            carried.is_some(),
            "the diagnosis must survive so the channel-unreadable explanation can name \
             WHY there is no token, instead of the misleading 'not configured'"
        );
        let (tok, carried) = plan_credential(Ok(("ghp_x".to_string(), "$ATERM_UPDATE_TOKEN")));
        assert_eq!(tok.as_deref(), Some("ghp_x"));
        assert!(carried.is_none());
    }

    /// The TOKEN lane's LIST ladder: a rate limit backs off without a health streak or
    /// an auth verdict; a rejected credential drops the check onto the web lane (never
    /// "unprovisioned", never a strand); a renamed repository is the standing Blocked
    /// state; everything else — including 404 WITH a token — is today's failure path.
    #[test]
    fn the_token_lane_list_ladder_classifies_every_answer() {
        let source = test_source();
        let url = "https://api.github.com/repos/alabsystems/aterm/releases".to_string();
        let ListDecision::RateLimited(text) = classify_list_error(
            &HttpError::RateLimited {
                code: 429,
                url: url.clone(),
                authenticated: true,
            },
            &source,
        ) else {
            panic!("a rate limit must classify as RateLimited");
        };
        assert!(
            !text.contains("rotate") && !text.contains("NEVER receive an update"),
            "a rate limit must not read as a revoked token or a stranded machine: {text}"
        );
        assert!(text.contains("backing off"), "{text}");
        for code in [401u16, 403] {
            assert_eq!(
                classify_list_error(&HttpError::Unauthorized { code }, &source),
                ListDecision::TokenRejected,
                "HTTP {code} with a token continues on the web lane"
            );
        }
        for code in [301u16, 308] {
            let ListDecision::Blocked(text) = classify_list_error(
                &HttpError::Status {
                    code,
                    url: url.clone(),
                },
                &source,
            ) else {
                panic!("a moved repository is a standing state, not weather");
            };
            assert!(text.contains("renamed or transferred"), "{text}");
            assert!(text.contains("ATERM_UPDATE_OWNER"), "{text}");
        }
        let ListDecision::Failed(text) = classify_list_error(&not_found(), &source) else {
            panic!("404 with a token is a failure, not a strand");
        };
        assert!(text.contains("404"), "{text}");
        for error in [
            HttpError::Status {
                code: 500,
                url: url.clone(),
            },
            HttpError::Transport("curl GET x failed (exit 6): dns".into()),
            HttpError::Malformed("GitHub API returned HTTP <html> for x".into()),
        ] {
            assert!(
                matches!(
                    classify_list_error(&error, &source),
                    ListDecision::Failed(_)
                ),
                "{error:?} must stay a failure"
            );
        }
    }

    /// The lane latch is what the background loop reads to choose a cadence, and what
    /// clears the stranded state. Reading the channel on the web lane — with no
    /// credential at all — clears it exactly as a token-lane read does, and the healthy
    /// status names the lane, the interval and (per channel) WHY a token is absent.
    #[test]
    fn a_readable_channel_establishes_the_lane_and_clears_the_strand() {
        let _serialized = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let source = test_source();
        RATE_LIMITED.store(true, Ordering::Relaxed);
        RATE_LIMIT_RESET.store(7, Ordering::Relaxed);
        TOKEN_REJECTED.store(false, Ordering::Relaxed);
        note_readable(Lane::Web, &source);
        assert_eq!(lane(), Lane::Web);
        assert!(
            !rate_limited(),
            "a completed read clears the rate-limit backoff latch"
        );
        assert_eq!(rate_limit_reset(), None, "…and any hold");
        assert!(!crate::no_token::is_stranded());
        let note = lane_note(&source);
        assert!(
            note.contains("unmetered web lane")
                && note.contains("30-minute")
                && note.contains("no GitHub API request is made"),
            "{note}"
        );
        // On the PUBLIC channel the status must NOT claim "no update token
        // provisioned" — the installer may have written the update-token file, and
        // this channel ignores every rung. Name the channel and the file's irrelevance.
        assert!(
            note.contains("ignores every update-token rung") && note.contains("repointed"),
            "{note}"
        );
        assert!(!note.contains("no update token provisioned"), "{note}");
        // A REPOINTED channel walks the whole chain, so for it the missing token stays
        // the named, remediable cause.
        let repointed = Source {
            owner: "example".into(),
            repo: "mirror".into(),
        };
        assert!(
            lane_note(&repointed).contains("no update token provisioned"),
            "{}",
            lane_note(&repointed)
        );
        // A rejected PROVISIONED token asks for the opposite remedy: rotation.
        TOKEN_REJECTED.store(true, Ordering::Relaxed);
        let rejected_note = lane_note(&source);
        assert!(
            rejected_note.contains("rejected by GitHub") && rejected_note.contains("rotate"),
            "{rejected_note}"
        );
        // A token-lane read ends the rejected story and keeps the historical wording.
        note_readable(Lane::Token, &source);
        assert_eq!(lane(), Lane::Token);
        assert!(!TOKEN_REJECTED.load(Ordering::Relaxed));
        assert_eq!(
            lane_note(&source),
            "",
            "a provisioned machine's existing status wording must not change"
        );
        note_readable(Lane::Web, &source);
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
            draft: false,
            prerelease: false,
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
            prerelease: false,
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
            let selected = select_authoritative_release(releases, &[])
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
            &[],
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
            &[],
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
        let selected = select_authoritative_release(base.to_vec(), &[])
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
        let fetched =
            fetch_authoritative_release(Some(selected), &[], &mut download, &RosterPolicy::INERT);
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
        let public_key = b64(keypair.public_key().as_ref());
        let manifest = manifest_bytes("0.10.0", 10, 0);
        let signature = keypair.sign(&manifest).as_ref().to_vec();
        let signed_base = [
            release_with_signed_appcast("v0.8.0", "signed-old-8", "signed-old-8-sig"),
            release_with_signed_appcast("v0.9.0", "signed-old-9", "signed-old-9-sig"),
            release_with_signed_appcast("v0.10.0", "signed-high", "signed-high-sig"),
        ];
        let selected = select_authoritative_release(signed_base.to_vec(), &[public_key.as_str()])
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
        let fetched = fetch_authoritative_release(
            Some(selected),
            &[public_key.as_str()],
            &mut download,
            &RosterPolicy::INERT,
        );
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
        let selected = select_authoritative_release(signed_base.to_vec(), &[public_key.as_str()])
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
        let fetched = fetch_authoritative_release(
            Some(selected),
            &[public_key.as_str()],
            &mut download,
            &RosterPolicy::INERT,
        );
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
        let selected = select_authoritative_release(signed_base.to_vec(), &[public_key.as_str()])
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
        let fetched = fetch_authoritative_release(
            Some(selected),
            &[public_key.as_str()],
            &mut download,
            &RosterPolicy::INERT,
        );
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
            let selected = select_authoritative_release(base.to_vec(), &[])
                .unwrap()
                .unwrap();
            let selection_before = catalog_model_state(&model, orders[2], false);
            let selected_state = project_authority_selection(selection_before, &selected);
            let mut calls = 0usize;
            let mut download = |_url: &str, _max_bytes: u64| {
                calls += 1;
                manifest_result.clone()
            };
            let fetched = fetch_authoritative_release(
                Some(selected),
                &[],
                &mut download,
                &RosterPolicy::INERT,
            );
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
            &[],
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
            let selected = select_authoritative_release(vec![release], &[])
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
            let fetched = fetch_authoritative_release(
                Some(selected),
                &[],
                &mut download,
                &RosterPolicy::INERT,
            );
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
        let selected = select_authoritative_release(base.to_vec(), &[])
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
            let authoritative = select_authoritative_release(releases, &[])
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
            let fetched = fetch_authoritative_release(
                Some(authoritative),
                &[],
                &mut download,
                &RosterPolicy::INERT,
            );
            assert_eq!(fetched.selected.unwrap().0.version, "0.10.0");
            assert_eq!(urls, ["authoritative-10"]);
            assert_eq!(fetched.manifest_fetch_attempts, 1);
            assert!(!fetched.appcast_fetch_error);
        }
    }

    #[test]
    fn signed_authority_fetches_one_manifest_and_one_signature_only() {
        let keypair = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let public_key = b64(keypair.public_key().as_ref());
        let manifest = manifest_bytes("0.10.0", 10, 0);
        let signature = keypair.sign(&manifest).as_ref().to_vec();
        let releases = vec![
            release_with_signed_appcast("v0.9.0", "older-503", "older-signature"),
            release_with_signed_appcast("v0.10.0", "highest-manifest", "highest-signature"),
        ];
        let authoritative = select_authoritative_release(releases, &[public_key.as_str()])
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
        let fetched = fetch_authoritative_release(
            Some(authoritative),
            &[public_key.as_str()],
            &mut download,
            &RosterPolicy::INERT,
        );
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
            let selected = select_authoritative_release(releases, &[])
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
            let fetched = fetch_authoritative_release(
                Some(selected),
                &[],
                &mut download,
                &RosterPolicy::INERT,
            );
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
                &[],
            )
            .expect_err("a noncanonical numeric maximum must fail closed");
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
            &[],
        )
        .expect_err("an unorderable historical exact-name tag must fail closed");
        assert!(err.contains("numeric dotted"), "{err}");
    }

    /// A PRERELEASE IS NOT A CANDIDATE ON EITHER LANE (2026-09-12). The web lane's
    /// LIST fallback filtered prereleases before this election; the token lane
    /// passed the raw LIST in, so a hand-published `v0.81.0-rc.1` prerelease with
    /// an appcast failed every token-lane check closed ("not numeric dotted")
    /// even though `v0.80.0` stood valid beneath it.
    #[test]
    fn token_lane_election_skips_prerelease_with_noncanonical_tag() {
        let mut prerelease = release_with_appcast("v0.81.0-rc.1", "u1");
        prerelease.prerelease = true;
        let elected = select_authoritative_release(
            vec![prerelease, release_with_appcast("v0.80.0", "u2")],
            &[],
        )
        .expect("a prerelease must not fail the election")
        .expect("stable elected");
        assert_eq!(elected.release.tag_name, "v0.80.0");
        assert_eq!(elected.version, "0.80.0");
    }

    /// …and a prerelease under a canonical tag never wins it: token-lane machines
    /// must not stage a build the web lane and `tools/install.sh` never select.
    #[test]
    fn token_lane_election_never_elects_a_canonical_prerelease() {
        let mut prerelease = release_with_appcast("v0.81.0", "u1");
        prerelease.prerelease = true;
        let elected = select_authoritative_release(
            vec![prerelease, release_with_appcast("v0.80.0", "u2")],
            &[],
        )
        .expect("canonical catalog")
        .expect("stable elected");
        assert_eq!(elected.release.tag_name, "v0.80.0");
    }

    #[test]
    fn unsigned_or_duplicate_signature_on_highest_never_falls_back() {
        let keypair = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let public_key = b64(keypair.public_key().as_ref());
        let lower = release_with_signed_appcast("v0.9.0", "lower-manifest", "lower-signature");

        let err = select_authoritative_release(
            vec![
                lower.clone(),
                release_with_appcast("v0.10.0", "unsigned-highest"),
            ],
            &[public_key.as_str()],
        )
        .expect_err("unsigned highest must defer");
        assert!(err.contains("v0.10.0") && err.contains("unsigned"), "{err}");

        let mut duplicate_sig =
            release_with_signed_appcast("v0.10.0", "highest-manifest", "highest-signature-a");
        duplicate_sig.assets.push(Asset {
            name: "aterm-appcast.toml.sig".into(),
            url: "highest-signature-b".into(),
            size: 0,
        });
        let err = select_authoritative_release(vec![duplicate_sig, lower], &[public_key.as_str()])
            .expect_err("duplicate highest signature must defer");
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
            &[],
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
        let fetched =
            fetch_authoritative_release(Some(selected), &[], &mut download, &RosterPolicy::INERT);
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
                &[],
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
                prerelease: false,
                assets: vec![Asset {
                    name: format!("aterm-appcast-{historical}.toml"),
                    url: "archive-must-not-be-fetched".into(),
                    size: 0,
                }],
            };
            let selected = select_authoritative_release(
                vec![renamed, release_with_appcast("v0.5.0", "current-head")],
                &[],
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
            &[],
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
        let selected = select_authoritative_release(only_legacy, &[])
            .expect("retired releases are skipped, never an error");
        assert!(
            selected.is_none(),
            "an archive-only catalog has no installable candidate"
        );

        // A pinned channel reaches the same conclusion without ever demanding a
        // signature from a retired release (it is skipped before the signature
        // policy applies).
        let keypair = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let public_key = b64(keypair.public_key().as_ref());
        let selected = select_authoritative_release(
            vec![release_with_appcast("v0.61", "retired-unsigned")],
            &[public_key.as_str()],
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
                    &[],
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
                &[],
            )
            .expect_err("nonnumeric exact-name candidate must fail closed");
            assert!(err.contains("numeric dotted"), "{malformed}: {err}");
        }
        // Numeric but noncanonical: a leading zero gives one release two
        // spellings, so it is refused outright rather than admitted alongside its
        // canonical twin.
        for noncanonical_maximum in ["v00.10.0", "v0.010.0", "v0.10.00", "v0.10.0000000"] {
            let err = select_authoritative_release(
                vec![release_with_appcast(noncanonical_maximum, "must-not-fetch")],
                &[],
            )
            .expect_err("noncanonical numeric maximum must fail closed");
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
        let err = select_authoritative_release(vec![duplicate_asset], &[])
            .expect_err("duplicate exact assets must fail closed");
        assert!(err.contains("duplicate assets"), "{err}");

        let err = select_authoritative_release(
            vec![
                release_with_appcast("v0.10.0", "manifest-a"),
                release_with_appcast("v0.10.0", "manifest-b"),
            ],
            &[],
        )
        .expect_err("duplicate canonical candidates must fail closed");
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
            &[],
        )
        .expect_err("an aliasing spelling must fail closed");
        assert!(err.contains("numeric dotted"), "{err}");
    }

    /// A DUPLICATE-ASSET RELEASE THAT LOSES SELECTION SIMPLY LOSES. Its tag is
    /// unambiguous and strictly lower, so it cannot be elected no matter which duplicate
    /// were chosen — failing the whole check closed over it would wedge every client on
    /// a defect of a release that was never going to be installed. Both catalog orders,
    /// so the outcome provably does not ride on REST response position.
    ///
    /// MUTATION: restore the pre-arbitration `?` on `unique_asset_index` (the old code)
    /// and both iterations fail — selection errors out before the maximum is chosen.
    #[test]
    fn a_poisoned_release_that_loses_selection_cannot_wedge_the_check() {
        for newest_first in [true, false] {
            let winner = release_with_appcast("v0.10.0", "manifest-good");
            let mut poisoned_loser = release_with_appcast("v0.9.0", "old-manifest-a");
            poisoned_loser.assets.push(Asset {
                name: "aterm-appcast.toml".into(),
                url: "old-manifest-b".into(),
                size: 0,
            });
            let catalog = if newest_first {
                vec![winner, poisoned_loser]
            } else {
                vec![poisoned_loser, winner]
            };
            let selected = select_authoritative_release(catalog, &[])
                .expect("a poisoned loser must not poison the check")
                .expect("the clean maximum is elected");
            assert_eq!(selected.release.tag_name, "v0.10.0");
            assert_eq!(selected.version, "0.10.0");
            assert_eq!(
                selected.release.assets[selected.manifest_index].url, "manifest-good",
                "the elected appcast is the winner's own, never the old release's"
            );
        }
    }

    /// ...AND A DUPLICATE-ASSET WINNER STILL FAILS THE WHOLE CHECK CLOSED, even with a
    /// clean runner-up behind it: electing the runner-up would be a silent downgrade
    /// behind a broken maximum — the same rule as the noncanonical-tag case. This is the
    /// gate that proves the loser-tolerance above is winner-only.
    #[test]
    fn a_poisoned_winner_still_fails_the_whole_check_closed() {
        let mut poisoned_winner = release_with_appcast("v0.10.0", "manifest-a");
        poisoned_winner.assets.push(Asset {
            name: "aterm-appcast.toml".into(),
            url: "manifest-b".into(),
            size: 0,
        });
        let err = select_authoritative_release(
            vec![
                poisoned_winner,
                release_with_appcast("v0.9.0", "runner-up-must-not-fetch"),
            ],
            &[],
        )
        .expect_err("a poisoned maximum must fail closed, not elect the runner-up");
        assert!(
            err.contains("duplicate assets") && err.contains("v0.10.0"),
            "{err}"
        );
    }

    #[test]
    fn authoritative_manifest_version_must_equal_canonical_tag() {
        let authoritative = select_authoritative_release(
            vec![
                release_with_appcast("v0.9.0", "older-must-not-fetch"),
                release_with_appcast("v0.10.0", "mismatched-highest"),
            ],
            &[],
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
        let fetched = fetch_authoritative_release(
            Some(authoritative),
            &[],
            &mut download,
            &RosterPolicy::INERT,
        );
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
        let rels: Vec<Release> = aterm_json::from_str(json).unwrap();
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

    /// ROTATION, end to end through the real selection + fetch path: a client whose
    /// keyset holds BOTH the incoming and the outgoing key installs a release signed
    /// by EITHER. This is the whole point of the keyset — without it, the release
    /// that would tell a client about the new key is itself unverifiable.
    #[test]
    fn a_release_signed_by_either_keyset_member_is_authoritative() {
        let outgoing = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let incoming = Ed25519KeyPair::from_seed_unchecked(&[42u8; 32]).unwrap();
        let k_out = b64(outgoing.public_key().as_ref());
        let k_in = b64(incoming.public_key().as_ref());
        let manifest = manifest_bytes("0.10.0", 10, 0);
        // Head = incoming (post-promotion), outgoing still inside its window.
        let keyset = [k_in.as_str(), k_out.as_str()];

        for (who, signer) in [("incoming", &incoming), ("outgoing", &outgoing)] {
            let signature = signer.sign(&manifest).as_ref().to_vec();
            let releases = vec![release_with_signed_appcast("v0.10.0", "m-url", "sig-url")];
            let selected = select_authoritative_release(releases, &keyset)
                .unwrap()
                .expect("a signed candidate is selected");
            let mut download = |url: &str, _max: u64| match url {
                "m-url" => Ok(manifest.clone()),
                "sig-url" => Ok(signature.clone()),
                other => Err(format!("unexpected fetch {other}")),
            };
            let fetched = fetch_authoritative_release(
                Some(selected),
                &keyset,
                &mut download,
                &RosterPolicy::INERT,
            );
            assert!(
                !fetched.manifest_rejected,
                "{who} key is in the keyset and must be accepted"
            );
        }
    }

    /// The non-vacuity control for the test above: once a key is DROPPED from the
    /// keyset it stops working. Without this, an implementation that accepted any
    /// signature at all would pass the rotation test.
    #[test]
    fn a_retired_key_is_refused_once_dropped_from_the_keyset() {
        let retired = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let current = Ed25519KeyPair::from_seed_unchecked(&[42u8; 32]).unwrap();
        let k_current = b64(current.public_key().as_ref());
        let manifest = manifest_bytes("0.10.0", 10, 0);
        let signature = retired.sign(&manifest).as_ref().to_vec();

        let releases = vec![release_with_signed_appcast("v0.10.0", "m-url", "sig-url")];
        let keyset = [k_current.as_str()];
        let selected = select_authoritative_release(releases, &keyset)
            .unwrap()
            .expect("a signed candidate is selected");
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(manifest.clone()),
            "sig-url" => Ok(signature.clone()),
            other => Err(format!("unexpected fetch {other}")),
        };
        let fetched = fetch_authoritative_release(
            Some(selected),
            &keyset,
            &mut download,
            &RosterPolicy::INERT,
        );
        assert!(
            fetched.manifest_rejected,
            "a key no longer in the keyset must not authenticate a release"
        );
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
            size: 0,
        });
        release.assets.push(Asset {
            name: "aterm-machines.toml.sig".into(),
            url: "roster-sig-url".into(),
            size: 0,
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
        // The channel keyset is the MACHINE's key — which is exactly the bridge shape:
        // the existing pinned key is declared to be a machine key, so old clients verify
        // the appcast unchanged while roster-aware clients verify the same signature
        // through the roster.
        let keyset = [f.machine_pub.as_str()];
        let selected = select_authoritative_release(vec![release_with_roster("v0.10.0")], &keyset)
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
            &keyset,
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
        // The LOG contract of the fully-in-keyset accept: one INFO attribution line,
        // no rotation note (the signer's key IS compiled in), and nothing at WARN.
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
        assert!(
            !attributed[0].1.contains("channel keyset"),
            "the signer is in the keyset, so no compatibility note applies: {lines:?}"
        );
    }

    /// A RELEASE WITH NO ROSTER is refused under an armed master — structurally, before
    /// any roster crypto. An armed anchor never degrades to "unsigned is fine".
    #[test]
    fn an_armed_master_refuses_a_release_that_carries_no_roster() {
        let f = roster_fixture(false);
        let keyset = [f.machine_pub.as_str()];
        // The plain signed release: appcast + signature, no roster assets.
        let selected = select_authoritative_release(
            vec![release_with_signed_appcast("v0.10.0", "m-url", "sig-url")],
            &keyset,
        )
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
            &keyset,
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

    /// A REVOKED MACHINE is refused on the real path, though its signature is genuine and
    /// the channel keyset still accepts it. This is the whole point of the tier.
    #[test]
    fn a_revoked_machine_is_refused_on_the_client_transport_path() {
        let f = roster_fixture(true);
        let keyset = [f.machine_pub.as_str()];
        let selected = select_authoritative_release(vec![release_with_roster("v0.10.0")], &keyset)
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
            &keyset,
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
        let keyset = [f.machine_pub.as_str()];
        let master = [f.master_pub.as_str()];
        let refuse_with = |floor_seq: u64, now_unix: i64| {
            let selected =
                select_authoritative_release(vec![release_with_roster("v0.10.0")], &keyset)
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
                &keyset,
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

        let keyset = [machine_pub.as_str()];
        let master_pub = b64(master.public_key().as_ref());
        let masters = [master_pub.as_str()];
        let selected = select_authoritative_release(vec![release_with_roster("v0.10.0")], &keyset)
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
            &keyset,
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

    /// AN UNPINNED MASTER LEAVES THE PATH EXACTLY AS IT WAS: the roster assets are never
    /// fetched, no attribution is produced, and the release is accepted on the channel
    /// keyset alone. This is the pre-v0.21.0 (pre-arming) behaviour, exercised here with
    /// a synthetic empty anchor — the shipped tree has been armed since 2026-08-15.
    #[test]
    fn an_unpinned_master_never_touches_the_roster_assets() {
        let f = roster_fixture(false);
        let keyset = [f.machine_pub.as_str()];
        let selected = select_authoritative_release(vec![release_with_roster("v0.10.0")], &keyset)
            .unwrap()
            .unwrap();
        let mut urls = Vec::new();
        let mut download = |url: &str, _max: u64| {
            urls.push(url.to_string());
            match url {
                "m-url" => Ok(f.manifest.clone()),
                "sig-url" => Ok(f.manifest_sig.clone()),
                other => panic!("an inert tier must fetch nothing extra, got {other}"),
            }
        };
        let fetched = fetch_authoritative_release(
            Some(selected),
            &keyset,
            &mut download,
            &RosterPolicy::INERT,
        );
        assert!(fetched.selected.is_some());
        assert!(fetched.attribution.is_none());
        assert_eq!(fetched.observed_roster_seq, None);
        assert_eq!(urls, ["m-url", "sig-url"]);
    }

    // -----------------------------------------------------------------------
    // THE ROSTER IS THE AUTHORITY — the two-state gate, both states.
    //
    // Everything below drives the REAL transport path, because the change these
    // exercise is not in the chain (the chain's gates are unchanged and proved in
    // `aterm_update_core::roster`) — it is in WHICH gate gets to refuse.
    // -----------------------------------------------------------------------

    /// A second machine, and a second master, both obviously synthetic and distinct from
    /// everything above so a mix-up cannot pass by coincidence.
    const M11_SEED_FIXTURE: [u8; 32] = [0xB8; 32];
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

    /// Drive the real path over a [`Chain`] with an explicit keyset and policy. Returns
    /// both the verdict and the URLs that were actually fetched.
    fn run_chain(
        c: &Chain,
        keyset: &[&str],
        masters: &[&str],
        floor_seq: u64,
        now_unix: i64,
    ) -> (AuthoritativeFetch, Vec<String>) {
        let selected = select_authoritative_release(vec![release_with_roster("v0.10.0")], keyset)
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
            keyset,
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

    /// **THE WHOLE POINT.** With the master ARMED, a machine the master-signed roster
    /// names is accepted even though this build's compiled-in keyset has never heard of
    /// it. That is what makes adding a machine a LOCAL act: mint on the new machine, put
    /// it on the roster, publish — no release cut from a machine that can already sign.
    ///
    /// Kills the mutation this test was written against: restore the keyset gate ahead of
    /// the roster chain and m11's release is refused here, exactly as it was before.
    #[test]
    fn an_armed_master_accepts_a_machine_the_compiled_in_keyset_does_not_carry() {
        let c = chain(
            &[("m3", M3_SEED_FIXTURE), ("m11", M11_SEED_FIXTURE)],
            &[],
            ("m11", M11_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        // The keyset is the OLD world: it holds m3 alone, which is exactly the state of
        // every build shipped before m11 existed.
        let m3_pub = pub_b64(&M3_SEED_FIXTURE);
        let keyset = [m3_pub.as_str()];
        assert!(
            !keyset.contains(&pub_b64(&M11_SEED_FIXTURE).as_str()),
            "precondition: the keyset must NOT carry the signing machine, or this test \
             proves nothing"
        );
        let masters = [c.master_pub.as_str()];
        let (fetched, urls) = run_chain(&c, &keyset, &masters, 0, ROSTER_NOW);

        assert!(
            fetched.selected.is_some(),
            "the roster authorized m11; the compiled-in keyset must not be able to refuse it"
        );
        let who = fetched.attribution.expect("attributed");
        assert_eq!(who.machine_id, "m11");
        assert_eq!(who.pubkey_b64, pub_b64(&M11_SEED_FIXTURE));
        assert_eq!(fetched.observed_roster_seq, Some(4));
        assert!(
            urls.contains(&"roster-url".to_string()),
            "the roster must actually be fetched: {urls:?}"
        );
    }

    /// THE ROTATION NOTE'S LOG CONTRACT, pinned by LEVEL. A signer outside the
    /// compiled-in keyset that the master-signed roster authorizes is the ordinary state
    /// of every pristine install made after a key rotation — the very first check such
    /// an install runs lands exactly here. It must be told so ONCE, at INFO, on the
    /// attribution line. It used to be told twice — first as a WARN, then the same fact
    /// as INFO — which presented routine rotation as a security problem on a release the
    /// user had just installed and verified, and trained people to ignore the signature
    /// warnings that DO matter. WARN stays reserved for actual verification degradation
    /// (see the revoked-machine test above, which pins that a refusal still warns).
    #[test]
    fn a_roster_authorized_rotation_is_one_info_note_and_never_a_warn() {
        let c = chain(
            &[("m3", M3_SEED_FIXTURE), ("m11", M11_SEED_FIXTURE)],
            &[],
            ("m11", M11_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        // The old world's keyset holds m3 alone, so m11 — the roster-authorized signer —
        // is outside it and the compatibility note applies.
        let m3_pub = pub_b64(&M3_SEED_FIXTURE);
        let keyset = [m3_pub.as_str()];
        let masters = [c.master_pub.as_str()];
        let _ = crate::log_capture::take();
        let (fetched, _) = run_chain(&c, &keyset, &masters, 0, ROSTER_NOW);
        assert!(
            fetched.selected.is_some(),
            "precondition: the roster accepts this release, or the note under test never fires"
        );

        let lines = crate::log_capture::take();
        assert!(
            lines
                .iter()
                .all(|(level, _)| *level != aterm_log::Level::Warn),
            "a roster-authorized rotation is not a warning: {lines:?}"
        );
        let noted: Vec<_> = lines
            .iter()
            .filter(|(_, msg)| msg.contains("was signed by machine"))
            .collect();
        assert_eq!(
            noted.len(),
            1,
            "the fact is stated exactly once, not WARN-then-INFO: {lines:?}"
        );
        let (level, note) = noted[0];
        assert_eq!(*level, aterm_log::Level::Info, "the note is INFO: {note}");
        assert!(
            note.contains("m11 (roster seq 4)") && note.contains("channel keyset"),
            "one line carries both the attribution and the compatibility note: {note}"
        );
    }

    /// THE CONVERSE, and it is what stops the change from being a loosening: a key that
    /// IS in the compiled-in keyset — the very key every shipped client accepts — is
    /// refused when the roster does not name it. Membership grants nothing under an armed
    /// anchor.
    ///
    /// Kills the mutation "accept if EITHER the keyset or the roster authorizes": under an
    /// OR this release is accepted, and revocation stops meaning anything.
    #[test]
    fn an_armed_master_refuses_a_keyset_member_the_roster_does_not_name() {
        // The roster names only m3; m11 signs. m11's key is in the keyset.
        let c = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m11", M11_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        let m11_pub = pub_b64(&M11_SEED_FIXTURE);
        let keyset = [m11_pub.as_str()];
        let masters = [c.master_pub.as_str()];
        // Precondition: with NO master pinned this very release is ACCEPTED — so the
        // refusal below is the roster's doing and not a broken fixture.
        let (unarmed, _) = run_chain(&c, &keyset, &[], 0, ROSTER_NOW);
        assert!(
            unarmed.selected.is_some(),
            "precondition: the keyset accepts this signature when the tier is absent"
        );

        let (armed, _) = run_chain(&c, &keyset, &masters, 0, ROSTER_NOW);
        assert!(
            armed.selected.is_none() && armed.manifest_rejected,
            "keyset membership must not authorize a machine the roster does not name"
        );
        assert!(armed.attribution.is_none());
    }

    /// A REVOKED MACHINE is refused even though its key is in the keyset AND its
    /// signature is genuine — and no crypto is ever run against it, because revocation
    /// empties the candidate set before `authorize_appcast` reaches a verifier at all.
    ///
    /// The ordering property itself is proved by construction in
    /// `aterm_update_core::roster` (`live()` filters, then the loop verifies). What this
    /// adds is the seam: the same bytes, the same keyset, the same master — only the
    /// deny-list differs, and the verdict flips.
    #[test]
    fn an_armed_master_refuses_a_revoked_machine_whose_key_the_keyset_still_carries() {
        let m3_pub = pub_b64(&M3_SEED_FIXTURE);
        let keyset = [m3_pub.as_str()];
        let live = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        let masters = [live.master_pub.as_str()];
        // The negative control FIRST, so the refusal below cannot be a broken fixture.
        let (ok, _) = run_chain(&live, &keyset, &masters, 0, ROSTER_NOW);
        assert!(ok.selected.is_some(), "precondition: m3 is live and signs");

        let revoked = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &["m3"],
            ("m3", M3_SEED_FIXTURE),
            5,
            &MASTER_SEED_FIXTURE,
            5,
        );
        let (refused, _) = run_chain(&revoked, &keyset, &masters, 0, ROSTER_NOW);
        assert!(
            refused.selected.is_none() && refused.manifest_rejected,
            "a revoked machine may not publish, whatever the keyset says"
        );
        assert!(refused.attribution.is_none());
        assert_eq!(
            refused.observed_roster_seq,
            Some(5),
            "the revoking generation was master-verified and ADMITTED, so it was OBSERVED \
             — the ratchet must learn seq 5 from this refusal, or a replayed seq-4 roster \
             re-authorizes the machine the owner just revoked"
        );
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
        let m3_pub = pub_b64(&M3_SEED_FIXTURE);
        let keyset = [m3_pub.as_str()];

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
        let (saw_revocation, _) = run_chain(&revoking, &keyset, &masters, 9, ROSTER_NOW);
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
            &keyset,
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
        let (accepted, _) = run_chain(&pre_revocation, &keyset, &masters, 9, ROSTER_NOW);
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
        let m3_pub = pub_b64(&M3_SEED_FIXTURE);
        let keyset = [m3_pub.as_str()];
        let masters = [c.master_pub.as_str()];
        let run_with_refresh = |refreshed_floor: u64| {
            let selected =
                select_authoritative_release(vec![release_with_roster("v0.10.0")], &keyset)
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
                &keyset,
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

    /// FAIL CLOSED, FOUR WAYS, AND NEVER BACK TO THE KEYSET.
    ///
    /// Every case here is arranged so that a keyset fallback would be VISIBLE: the
    /// appcast is signed by a key the keyset holds, so "refuse the roster, then accept on
    /// the keyset" would accept. All four must refuse.
    ///
    /// This is the case an attacker reaches for — suppressing or downgrading the roster
    /// assets is far easier than forging a master signature — and a fallback would hand
    /// them every armed client back at the old tier.
    #[test]
    fn an_armed_master_never_falls_back_to_the_keyset_when_the_roster_chain_fails() {
        let m3_pub = pub_b64(&M3_SEED_FIXTURE);
        let keyset = [m3_pub.as_str()];
        let good = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        let masters = [good.master_pub.as_str()];
        // Precondition: the keyset alone WOULD accept these exact bytes.
        let (unarmed, _) = run_chain(&good, &keyset, &[], 0, ROSTER_NOW);
        assert!(
            unarmed.selected.is_some(),
            "precondition: a fallback would be observable"
        );

        // (1) THE ROSTER ASSETS ARE MISSING from the release.
        let plain = select_authoritative_release(
            vec![release_with_signed_appcast("v0.10.0", "m-url", "sig-url")],
            &keyset,
        )
        .unwrap()
        .unwrap();
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(good.manifest.clone()),
            "sig-url" => Ok(good.manifest_sig.clone()),
            other => Err(format!("unexpected fetch {other}")),
        };
        let missing = fetch_authoritative_release(
            Some(plain),
            &keyset,
            &mut download,
            &RosterPolicy {
                master_pubkeys: &masters,
                floor_seq: 0,
                now_unix: ROSTER_NOW,
                floor_refresh: None,
            },
        );
        assert!(missing.selected.is_none() && missing.manifest_rejected);

        // (2) THE ROSTER IS UNVERIFIABLE — signed by a master this build does not pin.
        let wrong_master = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &OTHER_MASTER_FIXTURE,
            4,
        );
        let (forged, _) = run_chain(&wrong_master, &keyset, &masters, 0, ROSTER_NOW);
        assert!(forged.selected.is_none() && forged.manifest_rejected);

        // (3) THE ROSTER IS STALE — past its own `valid_until`, the only defence a fresh
        //     install has.
        let (lapsed, _) = run_chain(&good, &keyset, &masters, 0, 1_900_000_000);
        assert!(lapsed.selected.is_none() && lapsed.manifest_rejected);

        // (4) THE ROSTER IS ROLLED BACK — below the generation this client has already
        //     durably seen.
        let (replayed, _) = run_chain(&good, &keyset, &masters, 5, ROSTER_NOW);
        assert!(replayed.selected.is_none() && replayed.manifest_rejected);
    }

    /// A ROSTER ASSET THAT WILL NOT DOWNLOAD is a TRANSPORT failure, not a publisher
    /// error — and it still refuses.
    ///
    /// The distinction is what the operator is told. `manifest_rejected` escalates to
    /// "this Mac cannot install any release until that is fixed at the publisher", which
    /// is a false accusation for a flaky network — and now that the roster is the sole
    /// authority, every armed client's fetch of it is on that path.
    ///
    /// # This is also the ATTACKER'S branch, and that is why the fixture is built the way
    /// it is
    ///
    /// Transport failure is the roster failure an adversary controls most directly: drop,
    /// 404, reset or simply time out the `aterm-machines.toml` fetch while serving a
    /// perfectly good, keyset-signed appcast. If this arm could be talked into "the
    /// roster was unreachable, fall back to the keyset", one suppressed asset would
    /// downgrade every armed client to the tier the roster replaced — and a revoked
    /// machine's key would start working again.
    ///
    /// So the download closure below SERVES `sig-url` with a signature that verifies
    /// under the keyset, and the keyset is the signing machine's own key. Everything a
    /// fallback would need is present and correct; the only thing missing is the roster.
    /// A closure that returned an error for `sig-url` would make this arm untestable —
    /// the rescue would fail for an unrelated reason and every assertion below would
    /// still hold, which is exactly how this branch went unguarded.
    ///
    /// Kills the mutation "rescue a Transport failure with `verify_detached_any` over the
    /// pinned keyset".
    #[test]
    fn a_roster_that_cannot_be_fetched_is_reported_as_transport_and_still_refuses() {
        let m3_pub = pub_b64(&M3_SEED_FIXTURE);
        let keyset = [m3_pub.as_str()];
        let c = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        // THE PRECONDITION THIS TEST WOULD BE VACUOUS WITHOUT: the appcast signature the
        // closure is about to serve really does verify under the pinned keyset. A
        // fallback would therefore succeed if one existed, and the refusal below is a
        // decision rather than an accident.
        assert!(
            sig::verify_detached_any(&keyset, &c.manifest, &c.manifest_sig).is_ok(),
            "precondition: the keyset-signed appcast is exactly what a fallback would take"
        );
        let masters = [c.master_pub.as_str()];
        let selected = select_authoritative_release(vec![release_with_roster("v0.10.0")], &keyset)
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
            &keyset,
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
        let selected = select_authoritative_release(vec![release_with_roster("v0.10.0")], &keyset)
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
            &keyset,
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
            "a roster with no master signature authorizes nothing, keyset or no keyset"
        );
        assert!(fetched.appcast_fetch_error);
        assert!(!fetched.manifest_rejected);
    }

    /// AN ARMED MASTER WITH AN EMPTY KEYSET works — the configuration a fork has, and the
    /// one the owner reaches once no pre-roster client is left to protect.
    ///
    /// It is worth its own test because `select_authoritative_release` records the appcast
    /// signature's asset index only when the KEYSET is pinned. The roster tier locates it
    /// for itself, so arming the master does not depend on a keyset the build may not
    /// have. Kills the mutation "take the index from the candidate and refuse if absent".
    #[test]
    fn an_armed_master_authorizes_with_no_channel_keyset_at_all() {
        let c = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        let masters = [c.master_pub.as_str()];
        let (fetched, _) = run_chain(&c, &[], &masters, 0, ROSTER_NOW);
        assert!(
            fetched.selected.is_some(),
            "an empty keyset removes the OLD tier, it does not disarm the roster"
        );
        assert_eq!(
            fetched.attribution.expect("attributed").machine_id,
            "m3",
            "attribution still follows the key that signed"
        );
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
        let m3_pub = pub_b64(&M3_SEED_FIXTURE);
        let keyset = [m3_pub.as_str()];
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
        let (refused, _) = run_chain(&lying, &keyset, &masters, 0, ROSTER_NOW);
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
        let (admitted, _) = run_chain(&redressed, &keyset, &masters, 0, ROSTER_NOW);
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
        let (ok, _) = run_chain(&honest, &keyset, &masters, 0, ROSTER_NOW);
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
        let (rolled, _) = run_chain(&older, &keyset, &masters, 6, ROSTER_NOW);
        assert!(rolled.selected.is_none() && rolled.manifest_rejected);
    }

    /// **THE FLEET-BRICKING CASE, TESTED HARDEST.** With the anchor EMPTY the decision is
    /// the compiled-in keyset and nothing else, across the whole table — including when
    /// the release is carrying perfectly good roster assets, which the client must not so
    /// much as fetch.
    ///
    /// This is every build already in the field. `select_authoritative_release` yields
    /// exactly ONE candidate with no fallback to an older release, so a client that meets
    /// a release it cannot verify is not delayed, it is WEDGED permanently. Kills the
    /// mutation "run the roster chain whenever the release carries a roster".
    #[test]
    fn an_empty_master_anchor_leaves_the_keyset_decision_exactly_as_it_was() {
        let good = chain(
            &[("m3", M3_SEED_FIXTURE)],
            &[],
            ("m3", M3_SEED_FIXTURE),
            4,
            &MASTER_SEED_FIXTURE,
            4,
        );
        let m3_pub = pub_b64(&M3_SEED_FIXTURE);
        let m11_pub = pub_b64(&M11_SEED_FIXTURE);

        // (1) THE KEY IS THE HEAD: accepted, nothing attributed, and the roster assets on
        //     the release are never touched.
        let head_only = [m3_pub.as_str()];
        let (fetched, urls) = run_chain(&good, &head_only, &[], 0, ROSTER_NOW);
        assert!(fetched.selected.is_some());
        assert!(fetched.attribution.is_none());
        assert_eq!(fetched.observed_roster_seq, None);
        assert_eq!(
            urls,
            ["m-url", "sig-url"],
            "an absent tier must fetch nothing extra, even when the roster is right there"
        );

        // (2) THE KEY IS A NON-HEAD MEMBER (a rotation in flight): still accepted.
        let rotating = [m11_pub.as_str(), m3_pub.as_str()];
        let (fetched, urls) = run_chain(&good, &rotating, &[], 0, ROSTER_NOW);
        assert!(
            fetched.selected.is_some(),
            "any keyset member is authoritative"
        );
        assert_eq!(urls, ["m-url", "sig-url"]);

        // (3) THE KEY IS IN NO KEYSET: refused, as a manifest rejection.
        let stranger = [m11_pub.as_str()];
        let (fetched, _) = run_chain(&good, &stranger, &[], 0, ROSTER_NOW);
        assert!(fetched.selected.is_none() && fetched.manifest_rejected);

        // (4) THE SIGNATURE WILL NOT DOWNLOAD: a transport failure, unchanged.
        let selected =
            select_authoritative_release(vec![release_with_roster("v0.10.0")], &head_only)
                .unwrap()
                .unwrap();
        let mut download = |url: &str, _max: u64| match url {
            "m-url" => Ok(good.manifest.clone()),
            "sig-url" => Err("connection reset".to_string()),
            other => panic!("an absent tier must fetch nothing extra, got {other}"),
        };
        let fetched = fetch_authoritative_release(
            Some(selected),
            &head_only,
            &mut download,
            &RosterPolicy::INERT,
        );
        assert!(fetched.appcast_fetch_error && !fetched.manifest_rejected);

        // (5) NO KEYSET AT ALL and no master: unauthenticated channel, accepted, and the
        //     signature is not even fetched.
        let (fetched, urls) = run_chain(&good, &[], &[], 0, ROSTER_NOW);
        assert!(fetched.selected.is_some());
        assert_eq!(urls, ["m-url"]);
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

    /// The LIST transport on the web lane: any call is the bug these tests exist to
    /// catch.
    fn no_api_ever(url: &str, _token: &str) -> Result<Vec<u8>, HttpError> {
        panic!("the web lane made an api.github.com request: {url}")
    }

    /// The owner side of one web-lane release: a master-signed roster naming m3, and an
    /// appcast for [`WEB_TAG`] signed by m3 whose `url` field is `container_url` — the
    /// derived DMG URL by default, or whatever a test wants to bind against.
    struct WebChannel {
        master_pub: String,
        machine_pub: String,
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
                pubkey: machine_pub.clone(),
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
            machine_pub,
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

    fn support_dir(staging: &Staging) -> std::path::PathBuf {
        staging.root.parent().unwrap().to_path_buf()
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
                 latest_source = {source:?}\n"
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
        let mut list = no_api_ever;
        let outcome = acquire(
            staging,
            WEB_BUILD,
            source,
            &support_dir(staging),
            &mut list,
            &mut head,
        );
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
        let Ok(Acquisition::Proceed(acquired)) = outcome else {
            panic!("a moved pointer proceeds to acquisition: {outcome:?}");
        };
        assert_eq!(acquired.lane, Lane::Web);
        assert_eq!(acquired.tok, None, "no credential exists on this lane");
        assert_eq!(acquired.web_tag.as_deref(), Some(WEB_TAG));
        let candidate = acquired
            .candidate
            .expect("the pointer's tag is the candidate");
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
        let fetched = fetch_authoritative_release(
            Some(candidate),
            crate::PINNED_UPDATE_PUBKEYS,
            &mut download,
            &channel.policy(&masters),
        );
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
        assert_eq!(lane(), Lane::Web);
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
        crate::no_token::clear();
        let (outcome, heads) = acquire_web(&staging, None, || {
            Ok(HeadAnswer {
                code: 404,
                location: None,
            })
        });
        assert_eq!(heads.len(), 1);
        assert!(matches!(outcome, Ok(Acquisition::Ended)), "{outcome:?}");
        assert!(crate::no_token::is_stranded());
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(text.contains("cannot read its release channel"), "{text}");
        assert!(text.contains("HTTP 404"), "{text}");
        assert!(
            !staging.health().exists(),
            "a standing state is not a fault"
        );
        crate::no_token::clear();

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
            assert_eq!(rate_limit_reset(), None, "the web host names no reset");
            let text = std::fs::read_to_string(&staging.status).unwrap();
            assert!(text.contains("deferred"), "{code}: {text}");
            assert!(!text.contains("held_until"), "{code}: {text}");
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
        note_readable(Lane::Web, &test_source());
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
        let candidate = web_release(&source, "v0.11.0", crate::PINNED_UPDATE_PUBKEYS).unwrap();
        let mut gets = Vec::new();
        let mut download = |url: &str, _max: u64| {
            gets.push(url.to_string());
            // Serve WEB_TAG's bytes under v0.11.0's derived names.
            let name = url.rsplit('/').next().unwrap().to_string();
            channel.serve(&tag_url(WEB_TAG, &name))
        };
        let fetched = fetch_authoritative_release(
            Some(candidate),
            crate::PINNED_UPDATE_PUBKEYS,
            &mut download,
            &channel.policy(&masters),
        );
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
        let Ok(Acquisition::Proceed(acquired)) = outcome else {
            panic!("{outcome:?}");
        };
        let mut gets = Vec::new();
        let mut download = |url: &str, _max: u64| {
            gets.push(url.to_string());
            channel.serve(url)
        };
        let fetched = fetch_authoritative_release(
            Some(acquired.candidate.unwrap()),
            crate::PINNED_UPDATE_PUBKEYS,
            &mut download,
            &channel.policy(&masters),
        );
        assert!(fetched.selected.is_some());
        assert!(
            gets.iter().all(|u| u.starts_with(&tag_prefix(WEB_TAG))),
            "{gets:?}"
        );
        assert!(gets.iter().all(|u| !u.contains("v0.11.0")), "{gets:?}");
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// Invariant (c) on the web lane: a roster that cannot be FETCHED refuses the
    /// release as a transport failure and never falls back to the compiled-in keyset —
    /// even when that keyset carries the very machine key that signed the appcast.
    #[test]
    fn a_roster_that_cannot_be_fetched_on_the_web_lane_refuses_and_never_falls_to_the_keyset() {
        let source = test_source();
        let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
        let masters = [channel.master_pub.as_str()];
        let keyset = [channel.machine_pub.as_str()];
        let candidate = web_release(&source, WEB_TAG, &keyset).unwrap();
        let mut gets = Vec::new();
        let mut download = |url: &str, _max: u64| {
            gets.push(url.to_string());
            if url.ends_with(aterm_update_core::roster::ROSTER_ASSET) {
                return Err("HTTP 404: the release does not carry this asset".to_string());
            }
            channel.serve(url)
        };
        let fetched = fetch_authoritative_release(
            Some(candidate),
            &keyset,
            &mut download,
            &channel.policy(&masters),
        );
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
        let candidate = web_release(&source, WEB_TAG, &keyset).unwrap();
        let fetched = fetch_authoritative_release(
            Some(candidate),
            &keyset,
            &mut download,
            &channel.policy(&masters),
        );
        assert!(fetched.selected.is_none() && fetched.appcast_fetch_error);
        assert!(fetched.asset_fetch_rate_limited);
    }

    /// The listing GitHub's API answers while the channel head is SOURCE-ONLY (the
    /// measured 2026-09-10 shape): the head carries only the attestation pair, the
    /// release below it carries the app set, an older one too; a prerelease and a
    /// draft above them both carry appcasts and must not be elected.
    fn source_only_head_listing() -> Vec<u8> {
        br#"[
          {"tag_name":"v0.82.0","draft":true,"prerelease":false,"assets":[
            {"name":"aterm-appcast.toml","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/90","size":512},
            {"name":"aterm-appcast.toml.sig","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/91","size":64}]},
          {"tag_name":"v0.81.0","draft":false,"prerelease":true,"assets":[
            {"name":"aterm-appcast.toml","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/80","size":512},
            {"name":"aterm-appcast.toml.sig","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/81","size":64}]},
          {"tag_name":"v0.80.0","draft":false,"prerelease":false,"assets":[
            {"name":"aterm-machines.toml","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/70","size":427},
            {"name":"aterm-machines.toml.sig","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/71","size":64},
            {"name":"SHA256SUMS","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/72","size":9000},
            {"name":"SHA256SUMS.sig","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/73","size":64}]},
          {"tag_name":"v0.79.0","draft":false,"prerelease":false,"assets":[
            {"name":"aterm-appcast.toml","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/60","size":512},
            {"name":"aterm-appcast.toml.sig","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/61","size":64},
            {"name":"aterm-machines.toml","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/62","size":541},
            {"name":"aterm-0.79.0.dmg","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/63","size":1}]},
          {"tag_name":"v0.78.0","draft":false,"prerelease":false,"assets":[
            {"name":"aterm-appcast.toml","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/50","size":512},
            {"name":"aterm-appcast.toml.sig","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/51","size":64}]}
        ]"#
        .to_vec()
    }

    /// A fetch whose manifest answered the web host's 404 — the source-only head.
    fn head_without_appcast() -> AuthoritativeFetch {
        AuthoritativeFetch {
            appcast_fetch_error: true,
            appcast_missing: true,
            ..AuthoritativeFetch::default()
        }
    }

    /// THE SOURCE-ONLY HEAD (m21, 2026-09-10): the pointer names v0.80.0, whose
    /// appcast 404s; the fallback elects v0.79.0 — the newest non-draft,
    /// non-prerelease release carrying an appcast — from ONE anonymous LIST, and
    /// hands back a candidate whose every asset URL is the DERIVED tag-specific one
    /// (never the listing's API asset URLs). The prerelease and the draft above it
    /// carry appcasts and are skipped, exactly as `install.sh` and the pointer do.
    #[test]
    fn a_source_only_channel_head_falls_back_to_the_newest_release_with_an_appcast() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("source-only-head");
        let source = test_source();
        let mut requests: Vec<(String, String)> = Vec::new();
        let mut list = |url: &str, token: &str| {
            requests.push((url.to_string(), token.to_string()));
            Ok(source_only_head_listing())
        };
        let outcome = web_head_fallback(
            &staging,
            WEB_BUILD,
            &source,
            &head_without_appcast(),
            "v0.80.0",
            &mut list,
            crate::PINNED_UPDATE_PUBKEYS,
        );
        let Ok(HeadFallback::Candidate(candidate)) = outcome else {
            panic!("the newest release with an appcast is elected: {outcome:?}");
        };
        assert_eq!(candidate.release.tag_name, "v0.79.0");
        assert_eq!(candidate.version, "0.79.0");
        for asset in &candidate.release.assets {
            assert_eq!(
                asset.url,
                tag_url("v0.79.0", &asset.name),
                "every asset URL is the derived tag-specific one, not the listing's"
            );
            assert!(!aterm_update_core::cdn::is_api_host(&asset.url));
        }
        assert_eq!(
            requests.len(),
            1,
            "one anonymous LIST, one page: {requests:?}"
        );
        let (url, token) = &requests[0];
        assert!(aterm_update_core::cdn::is_api_host(url), "{url}");
        assert!(
            token.is_empty(),
            "no credential ever rides the web lane's LIST"
        );
        assert!(
            !staging.health().exists(),
            "a source-only head is not a pipeline fault"
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// THE FAST PATH: when the head's appcast was fetched (or failed for any reason
    /// other than the web host's 404) the LIST is never consulted — the LIST fake
    /// panics on any call, so "zero `api.github.com` requests on a healthy check" stays
    /// a measured fact.
    #[test]
    fn a_head_that_carries_its_appcast_never_consults_the_listing() {
        let staging = Staging::scratch("head-has-appcast");
        let source = test_source();
        let mut list = no_api_ever;
        for fetched in [
            AuthoritativeFetch::default(),
            AuthoritativeFetch {
                appcast_fetch_error: true,
                asset_fetch_rate_limited: true,
                ..AuthoritativeFetch::default()
            },
            AuthoritativeFetch {
                appcast_fetch_error: true,
                ..AuthoritativeFetch::default()
            },
            AuthoritativeFetch {
                manifest_rejected: true,
                ..AuthoritativeFetch::default()
            },
        ] {
            assert!(matches!(
                web_head_fallback(
                    &staging,
                    WEB_BUILD,
                    &source,
                    &fetched,
                    WEB_TAG,
                    &mut list,
                    crate::PINNED_UPDATE_PUBKEYS,
                ),
                Ok(HeadFallback::NotNeeded)
            ));
        }
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// A listing in which NOTHING carries an appcast (every release source-only, or
    /// empty) ends the check as "nothing to install from"; a rate-limited listing is a
    /// deferral with no health entry; a listing that cannot be read is `network`-class.
    #[test]
    fn the_fallback_listing_s_empty_deferred_and_failed_answers_are_classified() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("fallback-classes");
        let source = test_source();
        crate::status::clear_check_note();

        let mut empty = |_url: &str, _token: &str| Ok(b"[]".to_vec());
        assert!(matches!(
            web_head_fallback(
                &staging,
                WEB_BUILD,
                &source,
                &head_without_appcast(),
                "v0.80.0",
                &mut empty,
                crate::PINNED_UPDATE_PUBKEYS,
            ),
            Ok(HeadFallback::Nothing)
        ));
        let mut only_source = |_url: &str, _token: &str| {
            Ok(br#"[{"tag_name":"v0.80.0","draft":false,"assets":[{"name":"SHA256SUMS","url":"u","size":1}]},
                    {"tag_name":"v0.79.0","draft":false,"assets":[{"name":"SHA256SUMS","url":"u","size":1}]}]"#
                .to_vec())
        };
        assert!(matches!(
            web_head_fallback(
                &staging,
                WEB_BUILD,
                &source,
                &head_without_appcast(),
                "v0.80.0",
                &mut only_source,
                crate::PINNED_UPDATE_PUBKEYS,
            ),
            Ok(HeadFallback::Nothing)
        ));

        let mut limited = |url: &str, _token: &str| {
            Err(HttpError::RateLimited {
                code: 403,
                url: url.to_string(),
                authenticated: false,
            })
        };
        assert!(matches!(
            web_head_fallback(
                &staging,
                WEB_BUILD,
                &source,
                &head_without_appcast(),
                "v0.80.0",
                &mut limited,
                crate::PINNED_UPDATE_PUBKEYS,
            ),
            Ok(HeadFallback::Ended)
        ));
        assert!(
            !staging.health().exists(),
            "a rate-limited listing is weather, not a fault"
        );
        let recorded = std::fs::read_to_string(&staging.status).unwrap_or_default();
        assert!(
            recorded.contains("deferred") && recorded.contains("has no app manifest"),
            "{recorded}"
        );

        let mut broken = |_url: &str, _token: &str| Err(HttpError::Transport("dns".into()));
        let error = web_head_fallback(
            &staging,
            WEB_BUILD,
            &source,
            &head_without_appcast(),
            "v0.80.0",
            &mut broken,
            crate::PINNED_UPDATE_PUBKEYS,
        )
        .expect_err("an unreadable listing is a failure");
        assert!(
            error.contains("channel head v0.80.0 has no app manifest"),
            "{error}"
        );
        let h = crate::health::Health::read(&staging.health());
        assert_eq!((h.network_failures, h.pipeline_failures), (1, 0));
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// END TO END on the fixture channel: the pointer names a tag above [`WEB_TAG`]
    /// whose appcast the download host does not carry; the fallback elects
    /// [`WEB_TAG`] from the listing and the armed roster chain accepts it over the
    /// derived URLs — with no request to the API beyond the one LIST, and no GET
    /// through `latest`.
    #[test]
    fn a_source_only_head_is_bypassed_and_the_release_below_it_is_authorized() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("source-only-e2e");
        let source = test_source();
        let channel = web_channel(Some(&tag_url(WEB_TAG, "aterm-0.10.0.dmg")));
        let masters = [channel.master_pub.as_str()];
        let head = "v0.11.0";
        let mut gets: Vec<String> = Vec::new();
        let mut download = |url: &str, _max: u64| {
            gets.push(url.to_string());
            channel.serve(url)
        };
        // The head as the web lane synthesizes it, and its fetch: the appcast 404s.
        let candidate = web_release(&source, head, crate::PINNED_UPDATE_PUBKEYS).unwrap();
        let fetched = fetch_authoritative_release(
            Some(candidate),
            crate::PINNED_UPDATE_PUBKEYS,
            &mut download,
            &channel.policy(&masters),
        );
        assert!(fetched.selected.is_none() && fetched.appcast_missing);
        let mut lists = 0;
        let mut list = |_url: &str, _token: &str| {
            lists += 1;
            Ok(format!(
                r#"[{{"tag_name":"{head}","draft":false,"assets":[{{"name":"SHA256SUMS","url":"u","size":1}}]}},
                    {{"tag_name":"{WEB_TAG}","draft":false,"assets":[
                      {{"name":"aterm-appcast.toml","url":"https://api.github.com/x/1","size":1}},
                      {{"name":"aterm-appcast.toml.sig","url":"https://api.github.com/x/2","size":1}}]}}]"#
            )
            .into_bytes())
        };
        let Ok(HeadFallback::Candidate(below)) = web_head_fallback(
            &staging,
            WEB_BUILD,
            &source,
            &fetched,
            head,
            &mut list,
            crate::PINNED_UPDATE_PUBKEYS,
        ) else {
            panic!("the release below the head is elected");
        };
        assert_eq!(below.release.tag_name, WEB_TAG);
        let fetched = fetch_authoritative_release(
            Some(below),
            crate::PINNED_UPDATE_PUBKEYS,
            &mut download,
            &channel.policy(&masters),
        );
        let (manifest, release, _) = fetched
            .selected
            .as_ref()
            .expect("the armed chain accepts the release below the source-only head");
        assert_eq!(release.tag_name, WEB_TAG);
        web_container_url_agrees(&source, &release.tag_name, manifest)
            .expect("the manifest binds to the elected tag, not the head");
        assert_eq!(lists, 1, "one LIST");
        assert!(
            gets.iter()
                .all(|u| !aterm_update_core::cdn::is_api_host(u) && !u.contains("/latest/")),
            "{gets:?}"
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// The TOKEN lane is today's LIST, byte for byte: every page is an `api.github.com`
    /// request carrying the resolved token, a rejected token hands the check to the web
    /// lane (and the status names rotation), and a rate limit ends the check as a
    /// deferral with no health entry — and none of it ever touches `github.com`.
    #[test]
    fn the_token_lane_keeps_the_api_list_and_never_requests_from_github_com() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("token-lane");
        let source = test_source();
        crate::status::clear_check_note();
        let page = br#"[{"tag_name":"v0.9.0","draft":false,"assets":[
            {"name":"aterm-appcast.toml","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/1","size":512},
            {"name":"aterm-appcast.toml.sig","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/2","size":64},
            {"name":"aterm-0.9.0.dmg","url":"https://api.github.com/repos/alabsystems/aterm/releases/assets/3","size":1}]}]"#
            .to_vec();
        let mut requests: Vec<(String, String)> = Vec::new();
        let mut fetch = |url: &str, token: &str| {
            requests.push((url.to_string(), token.to_string()));
            Ok(page.clone())
        };
        let mut ctx = ListContext {
            staging: &staging,
            current_build: 5,
            source: &source,
            tok: "ghp_test",
            token_source: Some("$ATERM_UPDATE_TOKEN"),
            fetch: &mut fetch,
        };
        let Ok(Listing::Releases(releases)) = list_releases(&mut ctx) else {
            panic!("one short page is the whole listing");
        };
        assert_eq!(releases.len(), 1);
        assert_eq!(requests.len(), 1, "one page, one request");
        for (url, token) in &requests {
            assert!(aterm_update_core::cdn::is_api_host(url), "{url}");
            assert_eq!(token, "ghp_test", "the token rides every LIST request");
            assert!(!url.starts_with("https://github.com/"), "{url}");
        }
        assert_eq!(lane(), Lane::Token);
        assert_eq!(lane_note(&source), "");
        // Every asset URL the token lane elects from is the API URL — the credential
        // pairs only with that host, by the transport's own gate.
        let elected = select_authoritative_release(releases, &[])
            .unwrap()
            .unwrap();
        for asset in &elected.release.assets {
            assert!(aterm_update_core::cdn::is_api_host(&asset.url));
        }

        // A rejected token: the check continues on the web lane, and says "rotate".
        TOKEN_REJECTED.store(false, Ordering::Relaxed);
        let mut rejecting = |_url: &str, _token: &str| Err(HttpError::Unauthorized { code: 401 });
        let mut ctx = ListContext {
            staging: &staging,
            current_build: 5,
            source: &source,
            tok: "ghp_stale",
            token_source: Some("$ATERM_UPDATE_TOKEN"),
            fetch: &mut rejecting,
        };
        assert!(matches!(
            list_releases(&mut ctx),
            Ok(Listing::TokenRejected)
        ));
        assert!(TOKEN_REJECTED.load(Ordering::Relaxed));
        assert!(!staging.health().exists(), "a stale token is not a fault");

        // A rate-limited LIST: deferred, held to the server's reset, no health entry.
        let reset = unix_now_secs() + 20 * 60;
        std::fs::write(
            staging.list_headers(),
            format!(
                "HTTP/2 403 \r\nx-ratelimit-limit: 5000\r\nx-ratelimit-remaining: 0\r\n\
                 x-ratelimit-reset: {reset}\r\n\r\n"
            ),
        )
        .unwrap();
        let mut limited = |url: &str, _token: &str| {
            Err(HttpError::RateLimited {
                code: 403,
                url: url.to_string(),
                authenticated: true,
            })
        };
        let mut ctx = ListContext {
            staging: &staging,
            current_build: 5,
            source: &source,
            tok: "ghp_test",
            token_source: Some("$ATERM_UPDATE_TOKEN"),
            fetch: &mut limited,
        };
        assert!(matches!(list_releases(&mut ctx), Ok(Listing::Ended)));
        assert!(rate_limited());
        assert!(
            rate_limit_reset().is_some(),
            "the token lane holds to the reset"
        );
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(
            text.contains("deferred") && text.contains("held_until"),
            "{text}"
        );
        assert!(
            text.contains("lane = \"token:env\""),
            "the rung's ID, not its label: {text}"
        );
        assert!(!staging.health().exists(), "weather is not a fault");
        TOKEN_REJECTED.store(false, Ordering::Relaxed);
        note_readable(Lane::Web, &source);
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// The compiled-in public channel never consults the token chain: `acquire` goes
    /// straight to the web lane even when a token would resolve, because a credential
    /// buys nothing on a host that reads none — and honouring one would make draft
    /// releases visible to the selector.
    #[test]
    fn the_public_channel_ignores_every_token_rung() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(is_default_channel(&test_source()));
        assert!(!is_default_channel(&Source {
            owner: "example".into(),
            repo: "mirror".into(),
        }));
        let staging = Staging::scratch("public-ignores-token");
        // The LIST fake panics, so a check that consulted the chain and found a token
        // (this developer's shell may well carry `gh auth token`) would fail here.
        let (outcome, heads) = acquire_web(&staging, None, || redirect_to(WEB_TAG));
        assert_eq!(heads.len(), 1);
        let Ok(Acquisition::Proceed(acquired)) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(acquired.lane, Lane::Web);
        assert!(acquired.tok.is_none());
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// GitHub slugs are case-insensitive, so a case variant of the public channel IS
    /// the public channel: no chain walk (the LIST fake would panic on a resolved
    /// token), the web lane, and the "ignores every rung" wording — never the
    /// repointed-source treatment.
    #[test]
    fn a_case_variant_of_the_public_channel_is_still_the_web_lane_with_no_chain_walk() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let variant = Source {
            owner: "Alabsystems".into(),
            repo: "ATERM".into(),
        };
        assert!(is_default_channel(&variant));
        let staging = Staging::scratch("public-case-variant");
        let _ = std::fs::remove_file(&staging.status);
        let (outcome, heads) = acquire_web_from(&staging, &variant, || {
            Ok(HeadAnswer {
                code: 302,
                location: Some(
                    aterm_update_core::cdn::release_download_url(
                        "Alabsystems",
                        "ATERM",
                        WEB_TAG,
                        APPCAST_ASSET,
                    )
                    .unwrap(),
                ),
            })
        });
        assert_eq!(heads.len(), 1);
        let Ok(Acquisition::Proceed(acquired)) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(acquired.lane, Lane::Web);
        assert!(acquired.tok.is_none());
        assert!(
            lane_note(&variant).contains("ignores every update-token rung"),
            "{}",
            lane_note(&variant)
        );
        let _ = std::fs::remove_dir_all(&staging.root);
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
        crate::status::set_latest_tag(WEB_TAG, &source);
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
        let Ok(Acquisition::Proceed(acquired)) = outcome else {
            panic!("a retired stage forces a re-fetch: {outcome:?}");
        };
        assert_eq!(acquired.web_tag.as_deref(), Some(WEB_TAG));
        for asset in &acquired.candidate.unwrap().release.assets {
            assert!(!aterm_update_core::cdn::is_api_host(&asset.url));
        }
        crate::status::clear_check_note();
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// THE URL BIND ON A REPOINTED SOURCE. The publisher signs `url` under the
    /// compiled-in channel's slug, so a credential-less updater repointed at a mirror
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

    /// The standing "cannot read the channel" wording on a REPOINTED source names the
    /// remedy for the token the machine actually has: a token GitHub REJECTED (the
    /// check fell onto the web lane through `Listing::TokenRejected`, and the chain
    /// kept no diagnosis because it succeeded) says rotate, never "no update token is
    /// provisioned" — the two status surfaces agree.
    #[test]
    fn a_rejected_token_is_named_as_such_when_the_pointer_then_answers_404() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mirror = Source {
            owner: "example".into(),
            repo: "private-mirror".into(),
        };
        TOKEN_REJECTED.store(true, Ordering::Relaxed);
        let text = unreadable_explanation(404, &mirror, None);
        assert!(
            text.contains("rejected by GitHub") && text.contains("rotate"),
            "{text}"
        );
        assert!(!text.contains("no update token is provisioned"), "{text}");
        TOKEN_REJECTED.store(false, Ordering::Relaxed);
        let text = unreadable_explanation(404, &mirror, None);
        assert!(text.contains("no update token is provisioned"), "{text}");
        assert!(!text.contains("rotate"), "{text}");
        let text = unreadable_explanation(404, &mirror, Some(&unprovisioned()));
        assert!(text.contains("no update token is provisioned"), "{text}");
    }

    /// The memo files of the retired conditional-request design are reclaimed from the
    /// Updates root on every check; nothing else there is touched.
    #[test]
    fn the_retired_catalog_memo_is_reclaimed() {
        let staging = Staging::scratch("reclaim");
        for name in ["catalog.json", "catalog.headers", "floor.toml"] {
            std::fs::write(staging.root.join(name), "x").unwrap();
        }
        reclaim_retired_state(&staging);
        assert!(!staging.root.join("catalog.json").exists());
        assert!(!staging.root.join("catalog.headers").exists());
        assert!(
            staging.floor().exists(),
            "only the retired memo is reclaimed"
        );
        reclaim_retired_state(&staging);
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// The ledger's `lane` is one token of a space-separated status line, so the
    /// token lane records the rung's fixed ID and never its label: for EVERY rung the
    /// chain can report — three labels carry spaces — the recorded value has no
    /// whitespace, and `status_line_suffix` renders exactly the expected tokens.
    #[test]
    fn every_token_rung_records_a_whitespace_free_lane() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("lane-ids");
        let _ = std::fs::remove_file(staging.list_headers());
        for rung in token::RUNGS {
            crate::status::clear_check_note();
            note_delivery(&staging, Lane::Token, Some(rung.label));
            crate::status::record(&staging, 5, "up to date");
            let text = std::fs::read_to_string(&staging.status).unwrap();
            let delivery = crate::Delivery::from_ledger_text(&text);
            let lane = delivery.lane.clone().expect("the lane was recorded");
            assert!(
                !lane.chars().any(char::is_whitespace),
                "{:?} recorded lane {lane:?}",
                rung.label
            );
            assert_eq!(lane, format!("token:{}", rung.id));
            assert_eq!(
                delivery.status_line_suffix(),
                format!(" lane=token:{} delivery=ok", rung.id),
                "{:?}",
                rung.label
            );
            let suffix = delivery.status_line_suffix();
            let tokens: Vec<&str> = suffix.split(' ').filter(|t| !t.is_empty()).collect();
            assert_eq!(tokens.len(), 2, "{tokens:?}");
        }
        // The web lane is the bare word, and a lane-less ledger reports nothing.
        crate::status::clear_check_note();
        note_delivery(&staging, Lane::Web, None);
        crate::status::record(&staging, 5, "up to date");
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert_eq!(
            crate::Delivery::from_ledger_text(&text).lane.as_deref(),
            Some("web")
        );
        crate::status::clear_check_note();
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// A fetch failure is worded — and its `delivery=` note booked — for the host the
    /// lane actually asked: the download host on the web lane (`blocked`), the
    /// releases API on the token lane (`api-failed`). Neither note carries whitespace.
    #[test]
    fn a_fetch_failure_names_the_host_its_lane_asked() {
        assert_eq!(asset_source(Lane::Web), ("the download host", "blocked"));
        assert_eq!(
            asset_source(Lane::Token),
            ("the releases API", "api-failed")
        );
        for lane in [Lane::Unknown, Lane::Token, Lane::Web] {
            let (host, note) = asset_source(lane);
            assert!(!note.chars().any(char::is_whitespace), "{note:?}");
            assert!(!host.is_empty());
        }
        // The web lane never says "API" and the token lane never says "download host".
        assert!(!asset_source(Lane::Web).0.contains("API"));
        assert!(!asset_source(Lane::Token).0.contains("download host"));
    }

    // ------------------------------------------------------------------------------
    // HOLD-UNTIL-RESET: the epoch a rate-limited check hands its siblings and the loop.
    // ------------------------------------------------------------------------------

    /// `min(reset, now + 1 h) + jitter(0..60 s)`: an honest reset is waited out exactly
    /// (plus scatter), a reset past the horizon is a lie and is clamped to it.
    #[test]
    fn a_list_rate_limit_records_held_until_from_the_servers_reset_clamped_to_an_hour() {
        let now = 1_788_390_000;
        let reset = now + 13 * 60;
        assert_eq!(
            hold_epoch(reset, now, 0),
            Some(reset),
            "no entropy, no scatter"
        );
        assert_eq!(
            hold_epoch(reset, now, 255),
            Some(reset + 59),
            "the scatter is under a minute"
        );
        for entropy in 0..=255u8 {
            let until = hold_epoch(reset, now, entropy).expect("a future reset holds");
            assert!(
                (reset..reset + HOLD_JITTER_SECS).contains(&until),
                "{entropy}: {until}"
            );
        }
        assert_eq!(
            hold_epoch(now + 48 * 3600, now, 0),
            Some(now + HOLD_HORIZON_SECS),
            "a reset two days out is clamped to the hour GitHub's window actually is"
        );
        for stale in [0, now - 1, now] {
            assert_eq!(
                hold_epoch(stale, now, 255),
                None,
                "a reset not ahead of now is no hold at all — never jittered forward"
            );
        }

        // End to end from the header file: the LIST's own `x-ratelimit-reset` becomes
        // the ledger's `held_until` and the loop's `rate_limit_reset()`, and a readable
        // LIST clears it again.
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("held");
        let live_now = unix_now_secs();
        let reset = live_now + 20 * 60;
        std::fs::write(
            staging.list_headers(),
            format!(
                "HTTP/2 403 \r\nx-ratelimit-limit: 60\r\nx-ratelimit-remaining: 0\r\n\
                 x-ratelimit-used: 60\r\nx-ratelimit-reset: {reset}\r\n\r\n"
            ),
        )
        .unwrap();
        crate::status::clear_check_note();
        // (`RATE_LIMIT_RESET` is process-wide and every check-path test that reads its
        // channel zeroes it, so the epoch is asserted on the function's own return and
        // the ledger it wrote, not on a global another thread may have cleared.)
        let until =
            hold_until_reset(&staging).expect("the reset was read from this check's headers");
        assert!(
            (reset..reset + HOLD_JITTER_SECS).contains(&until),
            "{until} vs {reset}"
        );
        record_deferral(&staging, 42, "update check deferred: GitHub rate limit hit");
        let text = std::fs::read_to_string(&staging.status).unwrap();
        let v: aterm_toml::Value = text.parse().unwrap();
        let held = v
            .get("held_until")
            .and_then(aterm_toml::Value::as_str)
            .and_then(crate::rfc3339_to_unix)
            .expect("held_until is written as the ledger's RFC3339 shape");
        assert!(
            (reset..reset + HOLD_JITTER_SECS).contains(&held),
            "{held} vs {reset}: {text}"
        );
        assert!(
            v.get("outcome")
                .and_then(aterm_toml::Value::as_str)
                .is_some_and(|o| o.contains("deferred")),
            "the 'deferred' substring stays on the sentence: {text}"
        );
        RATE_LIMIT_RESET.store(until, Ordering::Relaxed);
        note_readable(Lane::Token, &test_source());
        assert_eq!(rate_limit_reset(), None, "a readable LIST clears the hold");

        // Headers without a reset: the plain deferred record, no hold, ladder as before.
        std::fs::write(
            staging.list_headers(),
            "HTTP/2 403 \r\ncontent-type: x\r\n\r\n",
        )
        .unwrap();
        assert_eq!(hold_until_reset(&staging), None);
        record_deferral(&staging, 42, "update check deferred: GitHub rate limit hit");
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(!text.contains("held_until"), "{text}");
        assert!(text.contains("deferred"), "{text}");
        let _ = std::fs::remove_dir_all(&staging.root);
    }

    /// A hold is a fact about GitHub's clock, not about this machine: it writes no
    /// `health.toml`, books no streak, and the asset-lane variant holds only when the
    /// API window was actually involved — a `github.com` 429 keeps the plain record.
    #[test]
    fn a_hold_never_writes_health_or_changes_a_verdict() {
        let _guard = crate::STRANDED_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let staging = Staging::scratch("held-health");
        let reset = unix_now_secs() + 600;
        std::fs::write(
            staging.list_headers(),
            format!(
                "HTTP/2 403 \r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: {reset}\r\n\r\n"
            ),
        )
        .unwrap();
        crate::status::clear_check_note();
        record_deferral(&staging, 42, "update check deferred: GitHub rate limit hit");
        assert!(
            !staging.health().exists(),
            "a hold never writes health.toml"
        );
        let h = crate::health::Health::read(&staging.health());
        assert_eq!(h.acquisition_failures(), 0);
        assert!(!h.is_persistent());

        // The asset legs: a deferral on the TOKEN lane holds to the LIST's reset (the
        // API window is what ran out); one on the WEB lane holds to nothing — the
        // download host carries no budget — and books the plain deferred record.
        record_web_deferral(&staging, 42, "update check deferred: web 429");
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(
            !text.contains("held_until"),
            "a github.com 429 holds to nothing: {text}"
        );
        assert!(text.contains("deferred"), "{text}");
        assert!(rate_limited(), "the loop still lengthens its wait");
        record_asset_deferral(&staging, Lane::Web, 42, "update check deferred: web asset");
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(!text.contains("held_until"), "{text}");
        record_asset_deferral(
            &staging,
            Lane::Token,
            42,
            "update check deferred: api asset",
        );
        let text = std::fs::read_to_string(&staging.status).unwrap();
        assert!(
            text.contains("held_until"),
            "a token-lane asset deferral holds to the reset: {text}"
        );
        assert!(!staging.health().exists(), "still no health.toml");
        note_readable(Lane::Web, &test_source());
        let _ = std::fs::remove_dir_all(&staging.root);
    }
}
