// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The DMG/.app-specific orchestration of a background update check: find the
//! newest release carrying an `aterm-appcast.toml`, and — if it is strictly newer
//! than the running build — download + verify its DMG and stage it. The portable
//! GitHub plumbing it drives (authenticated `curl` GET/download, the per-machine
//! token chain) lives in `aterm-update-core` (`api_get`/`download_bytes`/
//! `download_to`, [`aterm_update_core::token`]).
//!
//! Private repos require the API (the `releases/latest/download/…` browser
//! shortcut needs web auth), so every request is authenticated with the per-machine
//! token and asset bytes are downloaded via the asset API URL with
//! `Accept: application/octet-stream` (curl `-L` follows the 302 to storage and
//! drops the `Authorization` header on the cross-host redirect by default). The
//! token is fed to curl through STDIN (`curl --config -`), never on argv, so it is
//! not exposed to same-user processes via `ps`.

use serde::Deserialize;

use aterm_update_core::token;

use crate::manifest::{Manifest, Ready};
use crate::{PINNED_TEAM_ID, PINNED_UPDATE_PUBKEY, Source, bundle, install, paths::Staging, sig};

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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NumericTag(Vec<u64>);

/// Parse the ordering key for every exact-name candidate. Historical releases used
/// numeric multi-part tags; those remain orderable during the one-time archive
/// migration, but only the greatest key may become authority and it is subjected to
/// the stricter two-component canonical spelling below.
fn parse_numeric_tag(tag: &str) -> Result<NumericTag, String> {
    let version = tag
        .strip_prefix('v')
        .ok_or_else(|| format!("update candidate tag {tag:?} is not numeric dotted vN.N"))?;
    let components: Vec<&str> = version.split('.').collect();
    if components.len() < 2 {
        return Err(format!(
            "update candidate tag {tag:?} is not numeric dotted vN.N"
        ));
    }
    let components = components
        .into_iter()
        .map(|component| {
            if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(format!(
                    "update candidate tag {tag:?} is not numeric dotted vN.N"
                ));
            }
            component.parse::<u64>().map_err(|_| {
                format!("update candidate tag {tag:?} has an out-of-range numeric component")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NumericTag(components))
}

fn canonical_authority_version(tag: &str, numeric: &NumericTag) -> Result<String, String> {
    match numeric.0.as_slice() {
        [major, minor] if tag == format!("v{major}.{minor}") => Ok(format!("{major}.{minor}")),
        _ => Err(format!(
            "authoritative update tag {tag:?} is not canonical vMAJOR.MINOR"
        )),
    }
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

struct AuthoritativeRelease {
    tag: NumericTag,
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
        let tag = parse_numeric_tag(&release.tag_name)?;
        if !seen_tags.insert(tag.clone()) {
            return Err(format!(
                "duplicate published update candidates use numeric order {}",
                release.tag_name
            ));
        }
        let candidate = AuthoritativeRelease {
            tag,
            // Lower legacy candidates need no canonical version. The selected
            // numeric maximum is validated after the complete metadata pass.
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
    /// Manifest, its release, and the already-proved unique canonical DMG index.
    selected: Option<(Manifest, Release, usize)>,
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
            match authoritative_dmg_index(&candidate.release, &manifest, &candidate.version) {
                Ok(dmg_index) => {
                    fetched.selected = Some((manifest, candidate.release, dmg_index));
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

/// Background check + stage. Returns the staged version string on success, or
/// `None` when nothing newer is available / the updater is idle. Errors are
/// transient/operational (network, parse) and are logged by the caller.
pub fn check_and_stage(
    current_build: u64,
    _current_version: &str,
    source: &Source,
) -> Result<Option<String>, String> {
    // Only stage for a real installed bundle (a dev build has nothing to swap).
    if bundle::resolve().is_none() {
        return Ok(None);
    }
    let staging = Staging::resolve().ok_or("could not resolve Updates dir")?;
    // The Application Support dir is the Updates dir's parent.
    let support = staging.root.parent().ok_or("no support dir")?.to_path_buf();
    let Some(tok) = token::resolve(&support) else {
        // No token provisioned → stay idle (a private repo can't be read). Log it
        // ONCE per process so the periodic loop doesn't spam, and surface it in the
        // status file so an operator can see WHY the machine isn't updating.
        use std::sync::atomic::{AtomicBool, Ordering};
        static NO_TOKEN_LOGGED: AtomicBool = AtomicBool::new(false);
        if !NO_TOKEN_LOGGED.swap(true, Ordering::Relaxed) {
            crate::log("idle: no update token provisioned (see docs/RELEASING.md)");
        }
        crate::status::record(&staging, current_build, "idle: no update token provisioned");
        return Ok(None);
    };

    // Persisted monotonic recency floor (operator yank + rollback guard, F5/F6).
    let floor = crate::manifest::Floor::read(&staging.floor());

    // GitHub documents no ordering contract for List Releases. Enumerate the complete
    // bounded metadata set first, then choose the greatest canonical numeric
    // vMAJOR.MINOR tag carrying the exact appcast name. Only after that decision do we
    // fetch one manifest (+ one signature under Tier SIG), so row order cannot select
    // an older release and broken historical assets add no download latency.
    const PER_PAGE: u32 = 100;
    const MAX_PAGES: u32 = 10;
    let mut release_catalog = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases?per_page={PER_PAGE}&page={page}",
            source.owner, source.repo
        );
        // A failed releases LIST is `network`-class: GitHub unreachable / auth broken.
        // (The transient/persistent distinction the ledger needs lives in the CLASS
        // split — an asset that provably exists but can't be fetched is `pipeline`,
        // recorded below — so a broken download build can't hide behind "transient".)
        let body = match aterm_update_core::api_get(&url, &tok) {
            Ok(b) => b,
            Err(e) => {
                crate::health::Health::record_failure(&staging.health(), "network", &e);
                return Err(e);
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
    let mut download =
        |url: &str, max_bytes: u64| aterm_update_core::download_bytes(url, &tok, max_bytes);
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

    let Some((manifest, release, dmg_index)) = best else {
        let msg = if appcast_fetch_error {
            // Manifests exist but could not be downloaded while the releases list
            // succeeded — a `pipeline`-class failure. The ledger decides the honest
            // wording: a streak ≥ PERSISTENT_AFTER is no longer called "deferred"
            // (the build-826 incident hid behind "transient" for a whole release).
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
            "no release carries an update manifest".to_string()
        };
        crate::status::record(&staging, current_build, &msg);
        return Ok(None);
    };
    // NOTE: no `record_success` yet — the DMG download/verify/stage below is still
    // part of this check's pipeline. Success is recorded only at the terminal
    // healthy outcomes ("up to date" / "staged"), so a DMG-only breakage ACCRUES a
    // streak instead of being reset every cycle by its own check's manifest fetch.

    // Downgrade gate: never stage an older-or-equal build. A terminal healthy
    // outcome — the whole pipeline this check exercised worked.
    if manifest.build_number <= current_build {
        crate::health::Health::record_success(&staging.health());
        crate::status::record(
            &staging,
            current_build,
            &format!(
                "up to date (latest release build {})",
                manifest.build_number
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

    // If this exact build+DMG already failed to stage and nothing newer exists, don't
    // re-download the (up to 512 MB) DMG every interval; a re-publish under the same
    // build with a different sha256 (or any newer build) clears the memo (F17).
    if let Some(f) = crate::manifest::FailedMark::read(&staging.failed())
        && f.matches(manifest.build_number, &manifest.sha256)
    {
        crate::status::record(
            &staging,
            current_build,
            &format!(
                "skipping build {} (previously failed to stage; re-publish to retry)",
                manifest.build_number
            ),
        );
        return Ok(None);
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

    // Download the exact unique same-release DMG identity already proven while
    // accepting the authoritative manifest. No order-dependent asset lookup is
    // permitted after this point.
    let dmg_asset = &release.assets[dmg_index];

    let part = staging.download.join(format!("{}.part", manifest.dmg));
    let dmg = staging.download.join(&manifest.dmg);
    let _ = std::fs::remove_file(&part);
    // A failed DMG download is a `pipeline`-class ledger entry: the asset provably
    // exists (the release names it) but could not be fetched.
    if let Err(e) = aterm_update_core::download_to(&dmg_asset.url, &tok, &part, 536_870_912) {
        let _ = std::fs::remove_file(&part);
        crate::health::Health::record_failure(
            &staging.health(),
            "pipeline",
            &format!("DMG download failed: {e}"),
        );
        return Err(format!("DMG download failed: {e}"));
    }

    // Size sanity (when the API reported one), then atomically name it final. From
    // here failures are `stage`-class in the health ledger: the bytes ARRIVED; the
    // artifact (or local disk) is the problem, not the download pipeline.
    if dmg_asset.size != 0 {
        let got = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if got != dmg_asset.size {
            let _ = std::fs::remove_file(&part);
            let msg = format!(
                "DMG size mismatch: got {got} bytes, expected {}",
                dmg_asset.size
            );
            crate::health::Health::record_failure(&staging.health(), "stage", &msg);
            return Err(msg);
        }
    }
    if let Err(e) = std::fs::rename(&part, &dmg) {
        let msg = format!("finalize download: {e}");
        crate::health::Health::record_failure(&staging.health(), "stage", &msg);
        return Err(msg);
    }

    // Integrity: SHA-256 must equal the manifest.
    let got = aterm_update_core::sha256_file(&dmg)?;
    if !got.eq_ignore_ascii_case(&manifest.sha256) {
        let _ = std::fs::remove_file(&dmg);
        let msg = format!(
            "DMG sha256 mismatch: got {got}, manifest {}",
            manifest.sha256
        );
        crate::health::Health::record_failure(&staging.health(), "stage", &msg);
        return Err(msg);
    }

    // Mount, extract, verify (codesign/team-id/spctl), publish the ready marker. On a
    // post-download stage failure (verification etc.) memoize this build+sha so we
    // don't re-download it next cycle, and reclaim the DMG (F17).
    if let Err(e) = install::stage_from_dmg(&staging, &dmg, &manifest, PINNED_TEAM_ID) {
        crate::manifest::FailedMark::record(
            &staging.failed(),
            manifest.build_number,
            &manifest.sha256,
        );
        let _ = std::fs::remove_file(&dmg);
        crate::health::Health::record_failure(&staging.health(), "stage", &e);
        return Err(e);
    }
    // The verified bundle is the artifact now; reclaim the DMG and clear the memo.
    let _ = std::fs::remove_file(&dmg);
    crate::manifest::FailedMark::clear(&staging.failed());
    // Terminal healthy outcome: this check exercised the WHOLE pipeline (manifest,
    // DMG, verify, stage) successfully — clear every failure streak.
    crate::health::Health::record_success(&staging.health());
    // Raise the high-water to the build we just staged (never lowered): a later attempt
    // to roll us back below it is refused above (F6).
    crate::manifest::Floor::bump_and_write(&staging.floor(), 0, manifest.build_number);

    crate::status::record(
        &staging,
        current_build,
        &format!(
            "staged {} (build {}) — applies on next launch",
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
            version: "0.54.1".into(),
            build_number: 54,
            commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
            sha256: "ab".repeat(32),
            dmg: "aterm-0.54.1.dmg".into(),
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
            i64::try_from(selected.tag.0[1]).expect("bounded minor fits i64"),
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
            release_with_appcast("v0.8", "older-8-503"),
            release_with_appcast("v0.9", "older-9-503"),
            release_with_appcast("v0.10", "authoritative-10"),
        ];

        // Every concrete permutation must project onto the same numeric-max
        // CompleteMetadataArbitration transition.
        for (position, order) in orders.into_iter().enumerate() {
            let releases = order.map(|index| base[index].clone()).to_vec();
            let selected = select_authoritative_release(releases, "")
                .expect("canonical catalog")
                .expect("one authority");
            assert_eq!(selected.tag, NumericTag(vec![0, 10]));
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

        // The real migration catalog may include lower numeric multi-part tags.
        // They project to ObserveLowerLegacy and do not change the selected max.
        let selected = select_authoritative_release(
            vec![
                release_with_appcast("v0.5.14", "legacy-must-not-fetch"),
                release_with_appcast("v0.10", "authoritative-10"),
                release_with_appcast("v0.8", "older-8-503"),
                release_with_appcast("v0.9", "older-9-503"),
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

        // Conversely, the real selector refuses a noncanonical numeric maximum.
        let real_error = select_authoritative_release(
            vec![
                release_with_appcast("v0.10", "canonical-lower"),
                release_with_appcast("v0.10.1", "noncanonical-maximum"),
                release_with_appcast("v0.8", "older-8-503"),
                release_with_appcast("v0.9", "older-9-503"),
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
                "authoritative-10" => Ok(manifest_bytes("0.10", 10, 0)),
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
        let manifest = manifest_bytes("0.10", 10, 0);
        let signature = keypair.sign(&manifest).as_ref().to_vec();
        let signed_base = [
            release_with_signed_appcast("v0.8", "signed-old-8", "signed-old-8-sig"),
            release_with_signed_appcast("v0.9", "signed-old-9", "signed-old-9-sig"),
            release_with_signed_appcast("v0.10", "signed-high", "signed-high-sig"),
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
                Ok(manifest_bytes("0.9", 9, 0)),
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
            vec![release_with_appcast("v0.010", "must-not-fetch")],
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
        let mut missing_dmg = release_with_appcast("v0.10", "authoritative-10");
        missing_dmg
            .assets
            .retain(|asset| asset.name != "aterm-0.10.dmg");

        let mut duplicate_dmg = release_with_appcast("v0.10", "authoritative-10");
        duplicate_dmg.assets.push(Asset {
            name: "aterm-0.10.dmg".into(),
            url: "duplicate-dmg".into(),
            size: 0,
        });
        let mut duplicate_dmg_reversed = duplicate_dmg.clone();
        duplicate_dmg_reversed.assets.reverse();

        for (label, release, manifest) in [
            (
                "missing authoritative DMG",
                missing_dmg,
                manifest_bytes("0.10", 10, 0),
            ),
            (
                "duplicate authoritative DMG (forward order)",
                duplicate_dmg,
                manifest_bytes("0.10", 10, 0),
            ),
            (
                "duplicate authoritative DMG (reverse order)",
                duplicate_dmg_reversed,
                manifest_bytes("0.10", 10, 0),
            ),
            (
                "path-like authoritative DMG",
                release_with_appcast("v0.10", "authoritative-10"),
                manifest_bytes_with_dmg("0.10", 10, 0, "../aterm-0.10.dmg"),
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

        // NEGATIVE CONTROL: corrupting the real v0.10 selection into row-order
        // v0.9 cannot validate as CompleteMetadataArbitration.
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
        assert!(!admitted, "healthy model admitted v0.9 over v0.10: {why}");
        assert!(!model.check_invariant("SelectedAuthorityIsNumericMaximum", &corrupted));
    }

    #[test]
    fn canonical_numeric_selection_is_permutation_invariant_and_skips_older_503() {
        let base = [
            release_with_appcast("v0.9", "old-9-503"),
            release_with_appcast("v0.10", "authoritative-10"),
            release_with_appcast("v0.8", "old-8-503"),
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
            assert_eq!(authoritative.release.tag_name, "v0.10");
            let mut urls = Vec::new();
            let mut download = |url: &str, _max_bytes: u64| {
                urls.push(url.to_string());
                match url {
                    "authoritative-10" => Ok(manifest_bytes("0.10", 10, 0)),
                    "old-9-503" | "old-8-503" => {
                        Err("503 historical release asset unavailable".into())
                    }
                    unexpected => panic!("unexpected asset fetch: {unexpected}"),
                }
            };
            let fetched = fetch_authoritative_release(Some(authoritative), "", &mut download);
            assert_eq!(fetched.selected.unwrap().0.version, "0.10");
            assert_eq!(urls, ["authoritative-10"]);
            assert_eq!(fetched.manifest_fetch_attempts, 1);
            assert!(!fetched.appcast_fetch_error);
        }
    }

    #[test]
    fn signed_authority_fetches_one_manifest_and_one_signature_only() {
        let keypair = Ed25519KeyPair::from_seed_unchecked(&SIGNING_SEED).unwrap();
        let public_key = B64.encode(keypair.public_key().as_ref());
        let manifest = manifest_bytes("0.10", 10, 0);
        let signature = keypair.sign(&manifest).as_ref().to_vec();
        let releases = vec![
            release_with_signed_appcast("v0.9", "older-503", "older-signature"),
            release_with_signed_appcast("v0.10", "highest-manifest", "highest-signature"),
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
        assert_eq!(fetched.selected.unwrap().0.version, "0.10");
        assert_eq!(urls, ["highest-manifest", "highest-signature"]);
        assert_eq!(fetched.manifest_fetch_attempts, 1);
        assert!(!fetched.appcast_fetch_error);
    }

    #[test]
    fn lower_numeric_legacy_tags_are_tolerated_but_cannot_be_authority() {
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
        let catalog = std::iter::once(release_with_appcast("v0.54", "authoritative-54"))
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
            assert_eq!(selected.release.tag_name, "v0.54");
            let mut urls = Vec::new();
            let mut download = |url: &str, _max_bytes: u64| {
                urls.push(url.to_string());
                if url == "authoritative-54" {
                    Ok(manifest_bytes("0.54", 54, 0))
                } else {
                    Err("historical asset must not be fetched".into())
                }
            };
            let fetched = fetch_authoritative_release(Some(selected), "", &mut download);
            assert_eq!(fetched.selected.unwrap().0.version, "0.54");
            assert_eq!(urls, ["authoritative-54"]);
            assert_eq!(fetched.manifest_fetch_attempts, 1);
        }

        for same_or_newer_legacy in ["v0.54.1", "v0.55.1"] {
            let err = select_authoritative_release(
                vec![
                    release_with_appcast("v0.54", "canonical"),
                    release_with_appcast(same_or_newer_legacy, "must-not-fetch"),
                ],
                "",
            )
            .err()
            .expect("a noncanonical numeric maximum must fail closed");
            assert!(
                err.contains(same_or_newer_legacy) && err.contains("canonical"),
                "{err}"
            );
        }

        let err = select_authoritative_release(
            vec![
                release_with_appcast("v0.54", "canonical"),
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
        let lower = release_with_signed_appcast("v0.9", "lower-manifest", "lower-signature");

        let err = select_authoritative_release(
            vec![
                lower.clone(),
                release_with_appcast("v0.10", "unsigned-highest"),
            ],
            &public_key,
        )
        .err()
        .expect("unsigned highest must defer");
        assert!(err.contains("v0.10") && err.contains("unsigned"), "{err}");

        let mut duplicate_sig =
            release_with_signed_appcast("v0.10", "highest-manifest", "highest-signature-a");
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

    #[test]
    fn malformed_and_duplicate_candidates_fail_before_download() {
        for malformed in ["0.10", "V0.10", "v0", "v0.x"] {
            let err = select_authoritative_release(
                vec![release_with_appcast(malformed, "must-not-fetch")],
                "",
            )
            .err()
            .expect("nonnumeric exact-name candidate must fail closed");
            assert!(err.contains("numeric dotted"), "{malformed}: {err}");
        }
        for noncanonical_maximum in ["v0.10.0", "v00.10", "v0.010"] {
            let err = select_authoritative_release(
                vec![release_with_appcast(noncanonical_maximum, "must-not-fetch")],
                "",
            )
            .err()
            .expect("noncanonical numeric maximum must fail closed");
            assert!(
                err.contains("authoritative") && err.contains("canonical"),
                "{noncanonical_maximum}: {err}"
            );
        }

        let mut duplicate_asset = release_with_appcast("v0.10", "manifest-a");
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
                release_with_appcast("v0.10", "manifest-a"),
                release_with_appcast("v0.10", "manifest-b"),
            ],
            "",
        )
        .err()
        .expect("duplicate canonical candidates must fail closed");
        assert!(
            err.contains("duplicate published update candidates"),
            "{err}"
        );

        // Distinct spellings with the same numeric vector are an order collision,
        // not two candidates that may be resolved by response position.
        let err = select_authoritative_release(
            vec![
                release_with_appcast("v0.10", "manifest-a"),
                release_with_appcast("v00.010", "manifest-b"),
            ],
            "",
        )
        .err()
        .expect("duplicate numeric vector must fail closed");
        assert!(
            err.contains("duplicate published update candidates"),
            "{err}"
        );
    }

    #[test]
    fn authoritative_manifest_version_must_equal_canonical_tag() {
        let authoritative = select_authoritative_release(
            vec![
                release_with_appcast("v0.9", "older-must-not-fetch"),
                release_with_appcast("v0.10", "mismatched-highest"),
            ],
            "",
        )
        .unwrap()
        .unwrap();
        let mut urls = Vec::new();
        let mut download = |url: &str, _max_bytes: u64| {
            urls.push(url.to_string());
            match url {
                "mismatched-highest" => Ok(manifest_bytes("0.9", 10, 0)),
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
          {"tag_name": "v1.0", "draft": false, "assets": [
             {"name": "aterm-appcast.toml", "url": "https://api.github.com/repos/o/r/releases/assets/1", "size": 512},
             {"name": "aterm-1.0.0.dmg", "url": "https://api.github.com/repos/o/r/releases/assets/2", "size": 1000}
          ]},
          {"tag_name": "v1.1", "draft": true, "assets": [
             {"name": "aterm-appcast.toml", "url": "https://api.github.com/repos/o/r/releases/assets/3"}
          ]}
        ]"#;
        let rels: Vec<Release> = serde_json::from_str(json).unwrap();
        assert_eq!(rels.len(), 2);
        assert_eq!(rels[0].tag_name, "v1.0");
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

        // NEGATIVE CONTROL: the old build-only branch accepted this enormous
        // parseable marker and permanently bypassed the download, even though it
        // carries no canonical artifact identity.
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
}
