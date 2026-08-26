// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Post-publish verify (release spec §7 step 7; also the standalone
//! `ship verify [vX.Y.Z]`): replay the CLIENT's release-selection rule (the
//! greatest canonical `vMAJOR.MINOR.PATCH` non-draft release carrying the exact
//! appcast name) against the live API with no cache and require it to select our cut; download the
//! published manifest and assert BYTE-identity with the local artifact; `HEAD`
//! the DMG URL → 200. Prints PASS + release URL, or the exact remediation.
//! Absorbs the deleted tools/check-published.sh.
//!
//! This module also owns the OTHER read-side surfaces built on the same scan:
//! `ship status` (ledger tail vs releases API — dangling claims,
//! freshness), the remote-derived resume/recut decision of spec §5 (pure —
//! tests/resume.rs pins the table), `cut --abandon`, and `ship yank`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_update_core::Manifest;

use crate::ledger::{self, Error, Result};
use crate::manifest_out;
use crate::publish::{self, TagKind, gh_retry, step};

// ---------------------------------------------------------------------------
// the canonical client scan, replayed against the live API
// ---------------------------------------------------------------------------

/// One published (non-draft) release carrying a parseable manifest.
#[derive(Debug, Clone)]
pub struct Published {
    /// Release ID carried by the listing row. Production binds this to the
    /// complete `release` snapshot below before returning; pure fixtures may
    /// carry only this field.
    pub release_id: Option<u64>,
    /// Exact immutable GitHub release-object snapshot. Production scans always
    /// carry it; `None` is accepted only by pure fixtures and is never
    /// sufficient authority for a destructive yank. Historical releases may
    /// legitimately carry a branch-valued `target_commitish`; the annotated
    /// tag, not that creation hint, binds published code to the manifest.
    pub release: Option<publish::ReleaseObjectIdentity>,
    pub tag: String,
    pub build: u64,
    pub version: String,
    /// Exact asset selected for this release. Fast/client scans only admit
    /// `aterm-appcast.toml`; exhaustive status/yank scans fall back to the
    /// deterministic per-tag archive name.
    pub asset: String,
    /// Optional apply floor carried by this exact manifest. A release cut uses
    /// the canonical client candidate's value as the channel floor to inherit.
    pub min_build: Option<u64>,
    /// The manifest's exact downloaded bytes (strict UTF-8 — TOML is UTF-8 by
    /// definition, and a lossy decode would break the byte-identity check).
    pub text: String,
}

/// Scan all release metadata via the API (≤10 pages of 100 — the client's own
/// caps) before downloading any manifest. `stop_early` is the exact updater
/// replay: only `aterm-appcast.toml` is eligible, the greatest canonical numeric
/// `vMAJOR.MINOR.PATCH` tag is authoritative independent of REST row order
/// (retired two-component tags are skipped, exactly as the client skips them),
/// and exactly that one manifest is fetched. `stop_early: false` is the operator/history view:
/// each release falls back to `aterm-appcast-<tag>.toml`, preserving the complete
/// build set for status/yank after the single-head migration.
///
/// `gh api` performs a fresh REST call per page (gh only caches when asked
/// with `--cache`) — this is the "no cache" replay of spec §7 step 7.
pub fn scan_published(slug: &str, stop_early: bool) -> Result<Vec<Published>> {
    let found = scan_published_snapshot(slug, stop_early)?;
    for published in &found {
        validate_unbound_published_target(published)?;
    }
    Ok(found)
}

/// Production scan for a PUBLIC UPDATE CHANNEL — a repository that DISTRIBUTES
/// this cut but did not produce it.
///
/// Identical to [`scan_published`] with exactly one conjunct dropped: a mirrored
/// release object's `target_commitish` is NOT bound to the manifest's claim
/// commit. It cannot be. The channel is a different repository whose history does
/// not contain the claim commit — `publish::create_mirror_draft` deliberately
/// sends no `target_commitish` for that reason — so GitHub anchors the mirrored
/// release at the channel's default branch (`main`).
///
/// The channel-side identity invariant lives on
/// `publish::validate_mirror_release_capability`, which omits the field for the
/// same reason.
///
/// Everything else still holds, enforced inside [`scan_published_snapshot`]: the
/// immutable release ID, the listing-row-to-snapshot binding, exact tag identity,
/// `draft == false`, and the manifest version/build/commit identity. The bytes'
/// authenticity never came from the release target — it comes from the manifest
/// digest, the optional pinned signature, and codesign.
pub fn scan_published_channel(slug: &str, stop_early: bool) -> Result<Vec<Published>> {
    scan_published_snapshot(slug, stop_early)
}

/// A scratch/rehearsal scan has no trustworthy origin tag namespace. It must
/// therefore retain the current protocol's literal claim-SHA target invariant;
/// symbolic historical targets are admitted only by `scan_published_in_repo`.
pub(crate) fn validate_unbound_published_target(published: &Published) -> Result<()> {
    let release = published.release.as_ref().ok_or_else(|| {
        Error::new("production published identity has no immutable release-object snapshot")
    })?;
    if published.release_id != Some(release.id) {
        return Err(Error::new(
            "published listing ID differs from its immutable release-object snapshot",
        ));
    }
    let commit = validate_published_identity(published)?
        .commit
        .expect("validated published identity has commit");
    publish::validate_release_object_capability(
        Some(release),
        release.id,
        &published.tag,
        &commit,
        false,
    )
}

/// Does one published row fall under the CURRENT protocol's remote-tag
/// invariant — "`refs/tags/<tag>` resolves to exactly the commit this manifest
/// claims"?
///
/// Retired two-component rows are not canonical tags and were never in scope.
/// The pre-canonical dotted archive heads are out of scope too: several carry a
/// LIGHTWEIGHT tag that was later moved onto the post-release merge descendant
/// (`v0.5.10` → `0fbfb940`, `v0.5.11` → `3767f838`), so their manifest claim can
/// never equal their tag ref. They stay in the exhaustive `ship status` / `yank`
/// scan as accounting history; nothing can install them, because the client
/// elects on the exact `aterm-appcast.toml` asset name and theirs was renamed.
///
/// [`ledger::LEDGER_FLOOR`] is the era boundary, closed at both ends:
/// `ledger::next_build` refuses to MINT at or below it and
/// `manifest_out::v025_check` refuses to WRITE at or below it, so no release
/// this pipeline can cut is ever exempt. The row holding the client-facing exact
/// asset name is bound unconditionally, which is why every `stop_early` scan
/// keeps this check whatever build its manifest claims — those rows are built
/// with `asset: MANIFEST_ASSET`, so for them this predicate reduces to today's
/// condition exactly.
///
/// If a `stop_early == false` caller is ever added, this exemption travels with
/// it. Today `run_status` is the only one.
pub(crate) fn binds_remote_tag_identity(published: &Published) -> bool {
    parse_canonical_tag(&published.tag).is_ok()
        && (published.build > ledger::LEDGER_FLOOR
            || published.asset == manifest_out::MANIFEST_ASSET)
}

/// Production scan for the origin channel. In addition to the immutable
/// release-object closure, every release inside the current protocol's tag
/// invariant is bound to its exact remote tag (annotated or
/// legacy-lightweight) — see [`binds_remote_tag_identity`] for the two eras
/// deliberately outside it.
pub fn scan_published_in_repo(repo: &Path, slug: &str, stop_early: bool) -> Result<Vec<Published>> {
    let git = ledger::GitCli::new(repo);
    let found = scan_published_snapshot(slug, stop_early)?;
    let mut bindings = Vec::with_capacity(found.len());
    for published in &found {
        let commit = validate_published_identity(published)?
            .commit
            .expect("validated published identity has commit");
        if binds_remote_tag_identity(published) {
            bindings.push((published.tag.as_str(), commit));
        }
    }
    let borrowed = bindings
        .iter()
        .map(|(tag, commit)| (*tag, commit.as_str()))
        .collect::<Vec<_>>();
    publish::assert_remote_historical_tag_commits(&git, &borrowed)?;
    Ok(found)
}

fn scan_published_snapshot(slug: &str, stop_early: bool) -> Result<Vec<Published>> {
    const PER_PAGE: usize = 100;
    const MAX_PAGES: u32 = 10;
    // Preserve COUNTS instead of collapsing matching assets through `[0]`.
    // The Rust boundary can therefore reject duplicate exact/archive names
    // before an immutable-ID, bounded asset download is allowed to run.
    const METADATA_JQ: &str = r#".[] | . as $r |
        ("aterm-appcast-" + $r.tag_name + ".toml") as $archive |
        ([$r.assets[]? | select(.name == "aterm-appcast.toml")] | length) as $exact_count |
        ([$r.assets[]? | select(.name == $archive)] | length) as $archive_count |
        [($r.id | tostring), $r.tag_name, ($r.draft | tostring),
         ($exact_count | tostring), ($archive_count | tostring)] | @tsv"#;
    let mut metadata = String::new();
    for page in 1..=MAX_PAGES {
        let path = format!("repos/{slug}/releases?per_page={PER_PAGE}&page={page}");
        // One lossless line per release in arbitrary API order: tag, draft flag,
        // exact-name count, deterministic archive-name count.
        let listing = gh_retry(&["api", &path, "--jq", METADATA_JQ])?;
        let text = listing.stdout_utf8();
        let page_len = text.lines().count();
        metadata.push_str(&text);
        if !text.is_empty() && !text.ends_with('\n') {
            metadata.push('\n');
        }
        if page_len < PER_PAGE {
            break;
        }
        if page == MAX_PAGES {
            return Err(Error::new(format!(
                "release listing reached the {MAX_PAGES}-page safety cap before exhaustion"
            )));
        }
    }
    let mut identities = std::collections::BTreeMap::new();
    let (_, mut found) = scan_release_page(&metadata, stop_early, |release_id, tag, asset| {
        let release_id = release_id.ok_or_else(|| {
            Error::new("production release scan row has no immutable GitHub release ID")
        })?;
        // Each END of the download bracket is ONE read of the release object:
        // the release identity and the exact-name asset binding come out of the
        // same JSON document, so asking for them separately paid two `gh` cold
        // starts and two round trips AND left the pair skewed in time. The
        // checks below are run in exactly the order the two separate reads
        // imposed, so error precedence is unchanged.
        let before = publish::release_object_and_asset_identity(slug, release_id, asset)?;
        publish::validate_release_object_tag_state(
            before.release.as_ref(),
            release_id,
            tag,
            false,
        )?;
        let before_asset = before.asset;
        let before = before.release.expect("validated release object is present");
        if identities.insert(release_id, before.clone()).is_some() {
            return Err(Error::new(format!(
                "release ID {release_id} appeared more than once in the authoritative listing"
            )));
        }
        let before_asset = before_asset.ok_or_else(|| missing_release_asset(release_id, asset))?;
        // The recheck runs after the transfer and BEFORE the asset identities
        // are compared, so `after` is the very snapshot the asset recheck saw:
        // asset drift is still reported before object drift, exactly as when
        // the object re-read was a separate call after the download.
        let mut after: Option<publish::ReleaseObjectIdentity> = None;
        let bytes = publish::download_release_asset_with_identity_and_recheck(
            slug,
            asset,
            before_asset,
            || {
                let observed = publish::release_object_and_asset_identity(slug, release_id, asset)?;
                after = observed.release;
                observed
                    .asset
                    .ok_or_else(|| missing_release_asset(release_id, asset))
            },
        )?;
        if after.as_ref() != Some(&before) {
            return Err(Error::new(format!(
                "release ID {release_id} identity changed during authoritative manifest download"
            )));
        }
        Ok(bytes)
    })?;
    // On an exhaustive scan every remaining release's fetch+download round ran
    // since any given release's bracketed before/after pair, so the whole
    // captured set is re-read once more: concurrent channel mutation during the
    // long scan must fail the scan, not return a torn view. ONE paginated
    // listing settles that for every captured ID, and it is STRICTLY STRONGER
    // than the per-ID re-reads it replaces — those were themselves skewed
    // across the minutes they took, so they never proved a single consistent
    // instant. It is also one `gh` process instead of one per release; the
    // codebase already batches this way for tag commits
    // (`publish::assert_remote_historical_tag_commits`). Nothing between here
    // and the comparison below touches the network, so the snapshot is still
    // taken strictly after the last download. The stop-early replay fetches
    // exactly one manifest and its own pair already brackets that only transfer.
    //
    // The batch is restricted to the IDs this scan captured — see
    // [`captured_identity_rows`]. The per-ID re-reads it replaces only ever
    // touched those IDs, and the listing carries releases the scan deliberately
    // skipped, under a parser that fails closed on shapes the scan tolerates.
    let live_identities = if stop_early {
        None
    } else {
        let captured_ids: std::collections::BTreeSet<u64> = identities.keys().copied().collect();
        Some(release_identity_listing(slug, &captured_ids)?)
    };
    for published in &mut found {
        let release_id = published.release_id.ok_or_else(|| {
            Error::new("production published identity has no immutable release ID")
        })?;
        validate_published_identity(published)?;
        let captured = identities.get(&release_id).ok_or_else(|| {
            Error::new(format!(
                "release ID {release_id} has no captured immutable identity"
            ))
        })?;
        // A missing entry is a release deleted mid-scan; a differing entry is a
        // release edited mid-scan. Both are the torn view this refuses to return.
        if let Some(live) = &live_identities
            && live.get(&release_id) != Some(captured)
        {
            return Err(Error::new(format!(
                "release ID {release_id} identity changed after manifest validation"
            )));
        }
        publish::validate_release_object_tag_state(
            Some(captured),
            release_id,
            &published.tag,
            false,
        )?;
        published.release = Some(captured.clone());
    }
    Ok(found)
}

/// The exact refusal [`publish::release_asset_identity_for_release_id`] raises,
/// kept verbatim now that the scan resolves the binding through the fused
/// release-object/asset read instead of that helper.
fn missing_release_asset(release_id: u64, name: &str) -> Error {
    Error::new(format!(
        "release ID {release_id} has 0 assets named {name:?}; expected exactly one"
    ))
}

/// The `wanted` release objects as the channel currently exposes them, by
/// immutable ID, from a single paginated listing.
///
/// `publish::release_identity_jq(true)` emits byte-identical
/// [`publish::ReleaseObjectIdentity`] rows to the exact-ID program — `tests/resume.rs`
/// pins that equivalence — so a row read here compares against a captured
/// snapshot on exactly the same four fields the per-ID read compared.
fn release_identity_listing(
    slug: &str,
    wanted: &std::collections::BTreeSet<u64>,
) -> Result<std::collections::BTreeMap<u64, publish::ReleaseObjectIdentity>> {
    const PER_PAGE: usize = 100;
    const MAX_PAGES: u32 = 10;
    let mut observed = std::collections::BTreeMap::new();
    for page in 1..=MAX_PAGES {
        let path = format!("repos/{slug}/releases?per_page={PER_PAGE}&page={page}");
        let out = gh_retry(&["api", &path, "--jq", publish::release_identity_jq(true)])?;
        let text = out.stdout_utf8();
        // Pagination is driven by what GitHub RETURNED, never by what survived
        // the ID filter below — otherwise one page of uninteresting releases
        // would end the walk before the captured ones were seen.
        let page_len = text.lines().count();
        let captured = captured_identity_rows(&text, wanted);
        let rows = publish::parse_release_object_identity_rows(&captured)?;
        for row in rows {
            observed.insert(row.id, row);
        }
        if page_len < PER_PAGE {
            break;
        }
        if page == MAX_PAGES {
            return Err(Error::new(format!(
                "release identity listing reached the {MAX_PAGES}-page safety cap before exhaustion"
            )));
        }
    }
    Ok(observed)
}

/// The `wanted` releases' rows, selected out of one page of the channel's full
/// identity listing before the strict parser sees them.
///
/// The listing endpoint returns EVERY release object in the repo, but only the
/// IDs this scan captured are ever compared, and
/// [`publish::parse_release_object_identity_rows`] fails closed on an empty tag
/// or target. Those two facts collide: [`scan_release_page`] deliberately
/// TOLERATES exactly that shape — a draft created in the GitHub web UI without
/// picking a tag comes back as `tag_name: ""`, and the scan skips it at the
/// `release.draft` guard, so `ship status`/`yank` succeed today. Handing the
/// whole page to the strict parser would turn that unrelated release into a
/// hard failure of a read-only command, so the strict parse is restricted to
/// the rows the scan actually captured — precisely the set the per-ID re-reads
/// this batch replaced used to touch.
///
/// Restricting the batch never loosens the check. For a captured ID the parse
/// stays exactly as strict as the per-ID read was; a row whose own ID field is
/// unreadable cannot be attributed to a captured release at all, so dropping it
/// leaves that ID ABSENT from the live map and the comparison in
/// [`scan_published_snapshot`] reports the scan as torn. Fail-closed either way
/// — the only thing that changes is whose malformation can fail us.
fn captured_identity_rows(page: &str, wanted: &std::collections::BTreeSet<u64>) -> String {
    page.lines()
        .filter(|line| {
            line.split('\t')
                .next()
                .and_then(|id| id.parse::<u64>().ok())
                .is_some_and(|id| wanted.contains(&id))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The canonical version a candidate tag names: `"v0.2.0"` → `"0.2.0"`.
/// Tag classification itself lives in [`publish::parse_release_tag`], so the
/// publisher's rule and the CLIENT's (`aterm-update/src/github.rs`) cannot
/// drift: canonical `vMAJOR.MINOR.PATCH` is a candidate, a retired
/// two-component `vMAJOR.MINOR` is [`TagKind::Legacy`] (skipped, never an
/// error), anything else fails closed.
fn parse_canonical_tag(tag: &str) -> Result<String> {
    publish::canonical_channel_tag_version(tag)
}

#[derive(Debug)]
struct ReleaseMetadata<'a> {
    release_id: Option<u64>,
    tag: &'a str,
    draft: bool,
    exact_count: usize,
    archive_count: usize,
}

fn parse_release_metadata(listing: &str) -> Result<Vec<ReleaseMetadata<'_>>> {
    listing
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let fields: Vec<&str> = line.split('\t').collect();
            let (release_id, tag, draft, exact_count, archive_count) = match fields.as_slice() {
                // Compatibility for pure pre-ID scan fixtures. Production's
                // jq always emits the five-field form above.
                [tag, draft, exact, archive] => (None, *tag, *draft, *exact, *archive),
                [id, tag, draft, exact, archive] => (
                    Some(id.parse::<u64>().map_err(|_| {
                        Error::new(format!(
                            "malformed release metadata row {}: invalid release ID {id:?}",
                            index + 1
                        ))
                    })?),
                    *tag,
                    *draft,
                    *exact,
                    *archive,
                ),
                _ => {
                return Err(Error::new(format!(
                    "malformed release metadata row {}: expected four fixture fields or five production fields",
                    index + 1
                )));
                }
            };
            let draft = match draft {
                "true" => true,
                "false" => false,
                _ => {
                    return Err(Error::new(format!(
                        "malformed release metadata row {}: invalid draft flag {draft:?}",
                        index + 1
                    )));
                }
            };
            let parse_count = |value: &str, field: &str| {
                value.parse::<usize>().map_err(|_| {
                    Error::new(format!(
                        "malformed release metadata row {}: invalid {field} {value:?}",
                        index + 1
                    ))
                })
            };
            Ok(ReleaseMetadata {
                release_id,
                tag,
                draft,
                exact_count: parse_count(exact_count, "exact asset count")?,
                archive_count: parse_count(archive_count, "archive asset count")?,
            })
        })
        .collect()
}

fn fetch_authoritative(
    release_id: Option<u64>,
    tag: &str,
    asset: &str,
    expected_version: &str,
    fetch_manifest: &mut impl FnMut(Option<u64>, &str, &str) -> Result<Vec<u8>>,
) -> Result<Published> {
    let bytes = fetch_manifest(release_id, tag, asset)?;
    let mtext = String::from_utf8(bytes)
        .map_err(|error| Error::new(format!("{tag}: manifest is not UTF-8 ({error})")))?;
    let manifest = Manifest::parse(&mtext)
        .map_err(|error| Error::new(format!("{tag}: unparseable manifest ({error})")))?;
    if manifest.version != expected_version {
        return Err(Error::new(format!(
            "authoritative {tag} carries manifest version {:?}, expected {expected_version:?}",
            manifest.version
        )));
    }
    Ok(Published {
        release_id,
        release: None,
        tag: tag.to_string(),
        build: manifest.build_number,
        version: manifest.version,
        asset: asset.to_string(),
        min_build: manifest.min_build,
        text: mtext,
    })
}

/// Scan a complete release-metadata listing through an injected appcast fetch.
///
/// This is the causal seam behind [`scan_published`]: the production closure
/// resolves an exact name to one immutable asset ID and performs a bounded
/// asset-ID download, while tests permute arbitrary REST rows and
/// make obsolete tags return HTTP 503. The client path resolves one canonical
/// authority before fetching. The exhaustive path deliberately keeps fetching
/// every candidate because `ship status`/`yank` need the complete published-build
/// set, including manifests renamed out of the client's exact-name channel.
pub(crate) fn scan_release_page(
    listing: &str,
    stop_early: bool,
    mut fetch_manifest: impl FnMut(Option<u64>, &str, &str) -> Result<Vec<u8>>,
) -> Result<(usize, Vec<Published>)> {
    let metadata = parse_release_metadata(listing)?;
    let page_len = metadata.len();
    if stop_early {
        let mut seen_tags = std::collections::BTreeSet::new();
        let mut selected: Option<(&ReleaseMetadata<'_>, Vec<u64>)> = None;
        for release in &metadata {
            if release.draft || release.exact_count == 0 {
                continue;
            }
            if release.exact_count != 1 {
                return Err(Error::new(format!(
                    "release {} has {} duplicate assets named {}; client authority is ambiguous",
                    release.tag,
                    release.exact_count,
                    manifest_out::MANIFEST_ASSET
                )));
            }
            // Retired-scheme releases stay published but are never installed,
            // so the publisher's replay must skip them exactly as the client
            // does — otherwise a still-published v0.61 would out-order every
            // current-scheme candidate and stall the whole check.
            let TagKind::Candidate(tag_order) = publish::parse_release_tag(release.tag)? else {
                continue;
            };
            if !seen_tags.insert(tag_order.clone()) {
                return Err(Error::new(format!(
                    "duplicate published update candidates have the same numeric tag order as {}",
                    release.tag
                )));
            }
            if selected
                .as_ref()
                .is_none_or(|(_, current)| tag_order > *current)
            {
                selected = Some((release, tag_order));
            }
        }
        let Some((release, _)) = selected else {
            return Ok((page_len, Vec::new()));
        };
        // Every selected candidate was already proved three-component; this
        // re-derives the canonical version string, pinning the tag's exact
        // spelling to the version the manifest must carry.
        let version = parse_canonical_tag(release.tag)?;
        let published = fetch_authoritative(
            release.release_id,
            release.tag,
            manifest_out::MANIFEST_ASSET,
            &version,
            &mut fetch_manifest,
        )?;
        return Ok((page_len, vec![published]));
    }

    let mut out = Vec::new();
    for release in metadata {
        if release.draft {
            continue;
        }
        if release.exact_count > 1 {
            return Err(Error::new(format!(
                "release {} has {} duplicate assets named {}; history is ambiguous",
                release.tag,
                release.exact_count,
                manifest_out::MANIFEST_ASSET
            )));
        }
        let archived = manifest_out::archived_manifest_asset(release.tag);
        if release.archive_count > 1 {
            return Err(Error::new(format!(
                "release {} has {} duplicate assets named {archived}; history is ambiguous",
                release.tag, release.archive_count
            )));
        }
        if release.exact_count == 1 && release.archive_count == 1 {
            return Err(Error::new(format!(
                "release {} carries both exact {} and archive {archived}; history source/target is ambiguous",
                release.tag,
                manifest_out::MANIFEST_ASSET
            )));
        }
        let asset = if release.exact_count == 1 {
            manifest_out::MANIFEST_ASSET.to_string()
        } else if release.archive_count == 1 {
            archived
        } else {
            continue;
        };
        // Preserve the operator/history scan's longstanding treatment of malformed
        // manifest bytes: warn and continue. Transport failures remain terminal.
        let bytes = fetch_manifest(release.release_id, release.tag, &asset)?;
        let Ok(mtext) = String::from_utf8(bytes) else {
            eprintln!(
                "    WARNING: {}: manifest is not UTF-8 — skipped",
                release.tag
            );
            continue;
        };
        match Manifest::parse(&mtext) {
            Ok(manifest) => out.push(Published {
                release_id: release.release_id,
                release: None,
                tag: release.tag.to_string(),
                build: manifest.build_number,
                version: manifest.version,
                asset,
                min_build: manifest.min_build,
                text: mtext,
            }),
            Err(error) => {
                eprintln!(
                    "    WARNING: {}: unparseable manifest skipped ({error})",
                    release.tag
                );
            }
        }
    }
    Ok((page_len, out))
}

/// Highest build among an ALREADY EXHAUSTIVE scan. This is status/ledger
/// accounting only: every `stop_early` caller must use its canonical scan's sole
/// candidate directly.
pub fn select_newest(scanned: &[Published]) -> Option<&Published> {
    let mut best: Option<&Published> = None;
    for p in scanned {
        if best.is_none_or(|b| p.build > b.build) {
            best = Some(p);
        }
    }
    best
}

/// What the remote knows about one release tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseState {
    Absent,
    Draft,
    Published,
}

/// Probe one release by tag. A missing release is a NORMAL answer here, so
/// this is a single un-retried call with the not-found exit distinguished
/// from real failures by gh's message.
pub fn release_state(slug: &str, tag: &str) -> Result<ReleaseState> {
    Ok(match publish::unique_release_object_by_tag(slug, tag)? {
        None => ReleaseState::Absent,
        Some(release) if release.draft => ReleaseState::Draft,
        Some(_) => ReleaseState::Published,
    })
}

// ---------------------------------------------------------------------------
// remote-derived cut mode (spec §5) — pure; tests/resume.rs pins the table
// ---------------------------------------------------------------------------

/// The three remote-derived facts the §5 decision reads. Gathered by the
/// caller (network) so the decision itself is pure and table-testable.
#[derive(Debug, Clone)]
pub struct RemoteState {
    /// The version this cut would publish: the `[workspace.package]`
    /// `MAJOR.MINOR.0` version, e.g. "0.5.0". There is
    /// no second lineage — the ledger supplies build numbers, not versions.
    pub current_version: String,
    /// Does CHANGELOG.md already carry `## [current_version]`?
    pub changelog_has_section: bool,
    /// Does a NON-DRAFT release `v<current_version>` exist? (A draft is not
    /// published — it is exactly the wedge recut exists to finish.)
    pub published: bool,
}

/// What kind of cut to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutMode {
    /// Normal: roll the changelog into `## [version]` and claim.
    Fresh { version: String },
    /// The §5 wedge signature (roll + claim landed on main, nothing published):
    /// skip the roll, reuse the rolled section, claim a FRESH n.
    Recut { version: String },
}

/// Derive the cut mode from remote-visible state (spec §5: "the workspace
/// version derives 0.2.0 + `## [0.2.0]` section present + no published v0.2.0
/// release ⇒ recut") — this is what lets ANY machine finish a wedged cut with
/// no journal.
///
/// Under the single-version scheme the default version is not a bump of
/// anything: it is [`RemoteState::current_version`] itself. So "cut twice
/// without bumping Cargo.toml" lands squarely on the already-published guard,
/// whose message names the exact fix.
pub fn derive_cut_mode(s: &RemoteState, set_version: Option<&str>) -> Result<CutMode> {
    let pending = s.changelog_has_section && !s.published;
    // An explicit DIFFERENT version is the operator's call — the tag +
    // cut-elsewhere gates still stand between it and a collision.
    if let Some(v) = set_version
        && v != s.current_version
    {
        return Ok(CutMode::Fresh {
            version: v.to_string(),
        });
    }
    let version = s.current_version.clone();
    if pending {
        return Ok(CutMode::Recut { version });
    }
    if s.published {
        return Err(Error::new(format!(
            "v{version} is already published — bump [workspace.package] version in \
             Cargo.toml (MINOR: the next release is v{}), or retire a bad build with \
             `cargo ship yank <build>`",
            publish::bump_minor_release(&version)?
        )));
    }
    Ok(CutMode::Fresh { version })
}

// ---------------------------------------------------------------------------
// post-publish verify (spec §7 step 7)
// ---------------------------------------------------------------------------

/// The full post-publish check. `expect_build` is Some during a cut (the
/// claimed n); the standalone re-check passes None and verifies whatever is
/// live. `api_dmg_check` swaps the raw-URL HEAD for an API asset probe — the
/// rehearsal's scratch repo is private, so its browser URL 404s by design.
pub struct PostPublishSignature<'a> {
    /// A live cut knows whether its journaled ratchet required a signature;
    /// standalone verification derives the requirement from remote history.
    pub expected: Option<bool>,
    pub pubkey: Option<&'a str>,
    pub local_signature: Option<&'a Path>,
}

pub fn post_publish(
    repo: &Path,
    slug: &str,
    version: &str,
    expect_build: Option<u64>,
    local_manifest: Option<&Path>,
    api_dmg_check: bool,
    signature: PostPublishSignature<'_>,
) -> Result<()> {
    let tag = format!("v{version}");
    let scanned = if api_dmg_check {
        scan_published(slug, true)?
    } else {
        scan_published_in_repo(repo, slug, true)?
    };
    let best = scanned.first().ok_or_else(|| {
        Error::new(format!(
            "no published release in {slug} carries {} — the fleet sees NOTHING; \
             if a draft exists, flip it (cargo ship cut --resume)",
            manifest_out::MANIFEST_ASSET
        ))
    })?;
    if best.tag != tag {
        return Err(Error::new(format!(
            "the client selection rule picks {} (build {}), NOT {tag} — the fleet \
             will not stage this version; a newer build is live or this one is \
             still a draft",
            best.tag, best.build
        )));
    }
    if let Some(n) = expect_build
        && best.build != n
    {
        return Err(Error::new(format!(
            "{tag} is selected but its live manifest carries build {}, not our {n} — \
             the published bytes are not this cut's bytes",
            best.build
        )));
    }

    // Byte-identity with the local artifact (when it is THIS build's).
    let mut byte_note = "local manifest absent — byte-compare skipped".to_string();
    if let Some(local) = local_manifest
        && local.is_file()
    {
        let local_bytes =
            fs::read(local).map_err(|e| Error::new(format!("read {}: {e}", local.display())))?;
        match Manifest::parse(&String::from_utf8_lossy(&local_bytes)) {
            Ok(lm) if lm.build_number == best.build => {
                if local_bytes != best.text.as_bytes() {
                    return Err(Error::new(format!(
                        "published {} is NOT byte-identical to the local {} — someone \
                         republished under this tag, or the upload was clobbered; \
                         investigate before trusting this release",
                        manifest_out::MANIFEST_ASSET,
                        local.display()
                    )));
                }
                byte_note = "manifest byte-identical to local".to_string();
            }
            _ => {
                byte_note = format!(
                    "local {} is a different build — byte-compare skipped",
                    manifest_out::MANIFEST_ASSET
                );
            }
        }
    }

    // Replay the updater's signature ratchet against exact live bytes.  Asset
    // metadata establishes uniqueness and forbids archive-name fallback; the
    // detached signature is then checked under the pinned public identity and,
    // for an in-flight cut, byte-compared with its local artifact.
    let local_signature = match signature.local_signature {
        Some(path) if path.is_file() => Some(fs::read(path).map_err(|error| {
            Error::new(format!("read local signature {}: {error}", path.display()))
        })?),
        _ => None,
    };
    let signed = publish::verify_live_channel_head_signature(
        repo,
        slug,
        &tag,
        best.text.as_bytes(),
        local_signature.as_deref(),
        signature.pubkey,
    )?;
    if let Some(expected) = signature.expected
        && signed != expected
    {
        return Err(Error::new(format!(
            "live signature policy ({signed}) differs from the journaled cut policy ({expected}); \
             refusing a downgrade or unjournaled key transition"
        )));
    }
    let signature_note = if signed {
        "Tier SIG exact signature + history verified"
    } else {
        "unsigned channel (no signature ratchet in published history)"
    };

    // The DMG must actually be fetchable where the manifest points. FIRST the
    // authenticated asset API — the path every installed client really uses
    // (github.rs downloads by asset API URL; the browser URL needs web auth on
    // a private repo). This one is a hard gate everywhere.
    let manifest = Manifest::parse(&best.text)
        .map_err(|e| Error::new(format!("published manifest re-parse: {e}")))?;
    if !api_dmg_check {
        let commit = manifest.commit.as_deref().ok_or_else(|| {
            Error::new(format!(
                "published release {tag} manifest has no immutable claim commit"
            ))
        })?;
        publish::assert_remote_annotated_tag_commit(&ledger::GitCli::new(repo), &tag, commit)?;
    }
    let release_id = best.release_id.ok_or_else(|| {
        Error::new("selected production release has no immutable GitHub release ID")
    })?;
    let verified_dmg = publish::verify_release_asset_digest_for_release_id(
        slug,
        release_id,
        &tag,
        &manifest.dmg,
        &manifest.sha256,
    )?;
    let size = verified_dmg.size;
    // The zip gets the SAME hard gate: it is the container the in-app updater
    // downloads and stages from (the DMG path needs `hdiutil`, which an orphaned
    // post-handoff process cannot use), so a release whose zip is unfetchable or
    // mis-digested is a release the fleet cannot install.
    let zip_note = match (manifest.zip.as_deref(), manifest.zip_sha256.as_deref()) {
        (Some(zip), Some(sha256)) => {
            let verified = publish::verify_release_asset_digest_for_release_id(
                slug, release_id, &tag, zip, sha256,
            )?;
            format!("zip via API ok ({} bytes)", verified.size)
        }
        _ => "no zip container (pre-zip release; clients stage from the DMG)".to_string(),
    };
    // THEN the raw browser URL (tools/install.sh's grep-and-curl path):
    // HEAD → 200 on a public repo; on a PRIVATE repo it 404s by GitHub design
    // (true of v0.25's live URL today too), so there it degrades to a note
    // instead of wedging every cut on its own final step. The rehearsal's
    // scratch repo is private by construction — skip the HEAD outright.
    let dmg_note = if api_dmg_check {
        format!("DMG via API ok ({size} bytes; scratch repo — HEAD skipped)")
    } else {
        let url = manifest.url.clone().unwrap_or_else(|| {
            format!(
                "https://github.com/{slug}/releases/download/{tag}/{}",
                manifest.dmg
            )
        });
        let code = head_status(&url)?;
        if code == "200" {
            format!("DMG via API ok ({size} bytes) · HEAD 200")
        } else if repo_is_private(slug)? {
            format!(
                "DMG via API ok ({size} bytes); browser URL {code} (private repo — \
                 the fleet fetches via the authenticated API; install.sh needs a token)"
            )
        } else {
            return Err(Error::new(format!(
                "HEAD {url} returned {code}, not 200 — installed clients (and \
                 tools/install.sh) cannot fetch the DMG from this public repo"
            )));
        }
    };

    step(
        "verify",
        &format!(
            "live scan selects {tag} build {} · {byte_note} · {signature_note} · {dmg_note} · \
             {zip_note}",
            best.build
        ),
    );
    step(
        "",
        &format!("PASS — https://github.com/{slug}/releases/tag/{tag}"),
    );
    Ok(())
}

/// Whether the repo is private — decides if a 404 on the browser download URL
/// is a failure (public repo: yes) or GitHub working as designed (private).
fn repo_is_private(slug: &str) -> Result<bool> {
    let out = gh_retry(&[
        "repo",
        "view",
        slug,
        "--json",
        "isPrivate",
        "--jq",
        ".isPrivate",
    ])?;
    Ok(out.stdout_utf8().trim() == "true")
}

/// `curl -I -L` HEAD status of a URL (follows GitHub's 302 to storage).
fn head_status(url: &str) -> Result<String> {
    let out = Command::new("curl")
        .args(["-sS", "-o", "/dev/null", "-I", "-L", "-w", "%{http_code}"])
        .arg(url)
        .output()
        .map_err(|e| Error::new(format!("spawn curl: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "curl -I {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ---------------------------------------------------------------------------
// the standalone commands: verify / status / yank / abandon
// ---------------------------------------------------------------------------

/// `cargo ship verify [vX.Y.Z]` — re-run the post-publish check anytime.
pub fn run_verify(repo: &Path, version: Option<String>) -> Result<()> {
    let slug = slug_of(repo)?;
    publish::assert_origin_repo_binding(&ledger::GitCli::new(repo), &slug)?;
    println!("aterm-release · verify ({slug})");
    let version = match version {
        Some(v) => v,
        None => {
            // No argument: verify whatever the fleet would stage right now.
            let scanned = scan_published_in_repo(repo, &slug, true)?;
            let best = scanned.first().ok_or_else(|| {
                Error::new(format!(
                    "no published release in {slug} carries {}",
                    manifest_out::MANIFEST_ASSET
                ))
            })?;
            best.tag.trim_start_matches('v').to_string()
        }
    };
    let local = repo.join("dist").join(manifest_out::MANIFEST_ASSET);
    let local_signature = local.with_extension("toml.sig");
    let journal = publish::Journal::load(&repo.join("dist/cut-state.toml"))?;
    let matching = journal
        .as_ref()
        .filter(|journal| journal.version == version);
    // The VERIFICATION key, which is the release's own on a recovered cut and this
    // machine's on every other (see `Journal::verify_pubkey`).
    let pubkey = matching.and_then(|journal| {
        journal
            .verify_pubkey
            .as_deref()
            .or(journal.signature_pubkey.as_deref())
    });
    post_publish(
        repo,
        &slug,
        &version,
        None,
        Some(&local),
        false,
        PostPublishSignature {
            expected: matching.map(|journal| journal.signature_required),
            pubkey,
            local_signature: Some(&local_signature),
        },
    )
}

/// `cargo ship status` — version, ledger tail, dangling claims (ledger vs
/// releases API) and latest published build (spec §5).
pub fn run_status(repo: &Path) -> Result<()> {
    let slug = slug_of(repo)?;
    println!("aterm-release · status ({slug})");
    step(
        "signing",
        "Tier REPO channel: gh auth + SHA-256 + monotonic build number · update signing is optional (a configured key signs; no key is required to cut)",
    );

    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read Cargo.toml: {e}")))?;
    let full = publish::workspace_version(&cargo_text)?;
    let release_version = publish::release_version_from_workspace(&full)?;
    step(
        "source",
        &format!("{full} (Cargo.toml [workspace.package] MAJOR.MINOR.0 — the ONE version)"),
    );

    let ledger_text = fs::read_to_string(repo.join(ledger::LEDGER_FILE))
        .map_err(|e| Error::new(format!("read {}: {e}", ledger::LEDGER_FILE)))?;
    let records = ledger::parse(&ledger_text)?;
    // The ledger tail is deliberately NOT shape-checked: pre-cut-over lines
    // carry retired two-component versions and are real, append-only history.
    let tail = ledger::tail(&ledger_text)?;
    step(
        "app",
        &format!(
            "next cut v{release_version} (workspace {full}, DEV → 0) — the cut after that \
             needs [workspace.package] version bumped to publish v{}",
            publish::bump_minor_release(&release_version)?
        ),
    );
    step(
        "ledger",
        &format!(
            "tail {} ({}) · {} record{}",
            tail.build,
            tail.version,
            records.len(),
            if records.len() == 1 { "" } else { "s" }
        ),
    );

    // An unfinished journal is the most actionable fact on the machine —
    // surface it before the network facts.
    let journal_path = repo.join("dist/cut-state.toml");
    if let Some(j) = publish::Journal::load(&journal_path)? {
        match j.first_incomplete() {
            Some(next) => step(
                "cut",
                &format!(
                    "IN PROGRESS: v{} (build {}) at step \"{next}\" — `cargo ship cut --resume`",
                    j.version, j.build_number
                ),
            ),
            None => step(
                "cut",
                &format!("last journaled cut v{} completed", j.version),
            ),
        }
    }

    let scanned = scan_published_in_repo(repo, &slug, false)?;
    match select_newest(&scanned) {
        Some(best) => step(
            "published",
            &format!(
                "{} build {} (newest history manifest {})",
                best.tag, best.build, best.asset
            ),
        ),
        None => step(
            "published",
            &format!("NONE — no release carries {}", manifest_out::MANIFEST_ASSET),
        ),
    }

    // Dangling claims: ledger lines with no published release at that build —
    // the normal residue of crashed cuts (spec §2: gaps are expected). Derived
    // from the releases API, never a second ledger write.
    let live: Vec<u64> = scanned.iter().map(|p| p.build).collect();
    let dangling: Vec<String> = records
        .iter()
        .filter(|r| !live.contains(&r.build))
        .map(|r| format!("{} ({})", r.build, r.version))
        .collect();
    step(
        "dangling",
        &(if dangling.is_empty() {
            "none — every ledger claim is published".to_string()
        } else {
            format!(
                "{} (claimed, never published — harmless; numbers are single-use)",
                dangling.join(", ")
            )
        }),
    );

    Ok(())
}

fn validate_published_identity(published: &Published) -> Result<Manifest> {
    let expected_version = published
        .tag
        .strip_prefix('v')
        .ok_or_else(|| Error::new(format!("published tag {:?} has no v prefix", published.tag)))?;
    if published.version != expected_version {
        return Err(Error::new(format!(
            "release {} carries manifest version {:?}, not exact tag version {:?}",
            published.tag, published.version, expected_version
        )));
    }
    let manifest = Manifest::parse(&published.text).map_err(|error| {
        Error::new(format!(
            "{} manifest re-parse failed: {error}",
            published.tag
        ))
    })?;
    if manifest.build_number != published.build || manifest.version != published.version {
        return Err(Error::new(format!(
            "release {} scan identity changed during manifest re-parse",
            published.tag
        )));
    }
    let commit = manifest.commit.as_deref().ok_or_else(|| {
        Error::new(format!(
            "release {} manifest has no immutable claim commit",
            published.tag
        ))
    })?;
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::new(format!(
            "release {} manifest commit is not a full git object id",
            published.tag
        )));
    }
    Ok(manifest)
}

/// Pure successor-first yank decision. `false` means a new successor must be
/// published; malformed/non-orderable identity is an error, never a reason to
/// delete first.  The caller separately replays signature and DMG verification.
pub fn yank_successor_covers(bad: &Published, successor: &Published) -> Result<bool> {
    validate_published_identity(bad)?;
    validate_published_identity(successor)?;
    let required_floor = yank_required_floor(bad.build)?;
    // Both ends must be current-scheme candidates. A retired two-component
    // release is inert archive history no client will ever select, so it is
    // neither a yank target nor a successor — and "not orderable against the
    // current scheme" must fail, never license a deletion.
    let (TagKind::Candidate(bad_order), TagKind::Candidate(successor_order)) = (
        publish::parse_release_tag(&bad.tag)?,
        publish::parse_release_tag(&successor.tag)?,
    ) else {
        return Err(Error::new(format!(
            "yank needs two current-scheme vMAJOR.MINOR.PATCH releases; {} → {} includes a \
             retired two-component release, which is archive history no client selects",
            bad.tag, successor.tag
        )));
    };
    Ok(successor_order > bad_order
        && successor.build > bad.build
        && successor
            .min_build
            .is_some_and(|floor| floor >= required_floor))
}

fn unique_yank_target(scanned: &[Published], build: u64) -> Result<Published> {
    let matches: Vec<&Published> = scanned
        .iter()
        .filter(|published| published.build == build)
        .collect();
    match matches.as_slice() {
        [published] => {
            validate_published_identity(published)?;
            Ok((*published).clone())
        }
        [] => Err(Error::new(format!(
            "no published release carries build {build}"
        ))),
        _ => Err(Error::new(format!(
            "build {build} appears in {} published release manifests; refusing REST-order-dependent destruction",
            matches.len()
        ))),
    }
}

fn verification_pubkey_for(repo: &Path, version: &str) -> Result<Option<String>> {
    let journal = publish::Journal::load(&repo.join("dist/cut-state.toml"))?;
    if let Some(key) = journal
        .as_ref()
        .filter(|journal| journal.version == version)
        .and_then(|journal| {
            journal
                .verify_pubkey
                .clone()
                .or_else(|| journal.signature_pubkey.clone())
        })
    {
        return Ok(Some(key));
    }
    Ok(None)
}

/// The channel floor a successor must carry before build `build` may be
/// destroyed: yanking `u64::MAX` cannot be expressed and must fail, never wrap.
fn yank_required_floor(build: u64) -> Result<u64> {
    build
        .checked_add(1)
        .ok_or_else(|| Error::new("cannot yank u64::MAX: successor min_build would overflow"))
}

/// Shared skeleton of the yank convergence proofs: elect the channel head,
/// test it with the caller's coverage predicate, then fully re-verify it with
/// the post-publish replay before reporting it as the covering successor.
fn verified_channel_successor(
    repo: &Path,
    slug: &str,
    covers: impl Fn(&Published) -> Result<bool>,
) -> Result<Option<Published>> {
    let scanned = scan_published_in_repo(repo, slug, true)?;
    let Some(successor) = scanned.first() else {
        return Ok(None);
    };
    if !covers(successor)? {
        return Ok(None);
    }
    let pubkey = verification_pubkey_for(repo, &successor.version)?;
    post_publish(
        repo,
        slug,
        &successor.version,
        Some(successor.build),
        None,
        false,
        PostPublishSignature {
            expected: None,
            pubkey: pubkey.as_deref(),
            local_signature: None,
        },
    )?;
    Ok(Some(successor.clone()))
}

/// The yank's successor must be the head the FLEET installs from: when a public
/// update channel is configured, the channel's newest release must be this exact
/// build carrying at least this `min_build`. Without a channel, the origin is the
/// channel and the origin proof stands.
fn prove_successor_on_channel(repo: &Path, successor: &Published) -> Result<()> {
    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("cannot read workspace Cargo.toml: {e}")))?;
    let Some(mirror_slug) = crate::mirror::update_channel_slug(&cargo_text)? else {
        return Ok(());
    };
    let head = scan_published_channel(&mirror_slug, true)?;
    let head = head.first();
    let mirrored =
        head.is_some_and(|h| h.build == successor.build && h.min_build >= successor.min_build);
    if mirrored {
        return Ok(());
    }
    Err(Error::new(format!(
        "the yank successor v{} (build {}, min_build {:?}) is live on the origin but NOT the \
         head of the public channel {mirror_slug} (head: {:?}); the fleet installs from the \
         channel, so the bad build is not poisoned yet. Mirror the successor first (`cargo ship \
         cut --resume` if its journal is parked at mirror; a retired-unmirrored successor needs \
         a fresh cut under the current roster generation), then re-run yank",
        successor.version,
        successor.build,
        successor.min_build,
        head.map(|h| (h.version.clone(), h.build, h.min_build))
    )))
}

/// Re-prove that the bad release is already inert before every cleanup
/// mutation. The full post-publish replay covers canonical arbitration, exact
/// manifest bytes, current signature + all signed history, and DMG availability.
fn prove_yank_successor(repo: &Path, slug: &str, bad: &Published) -> Result<Option<Published>> {
    verified_channel_successor(repo, slug, |successor| {
        yank_successor_covers(bad, successor)
    })
}

/// Command-level convergence after tag-first cleanup and a release-delete
/// crash/response loss. The original manifest is gone, so only claim success
/// when no parsed release carries the build and the exact current authority is
/// newer, fully verified, and permanently poisons that build via min_build.
fn prove_absent_yank_converged(repo: &Path, slug: &str, build: u64) -> Result<Option<Published>> {
    let required_floor = yank_required_floor(build)?;
    verified_channel_successor(repo, slug, |successor| {
        validate_published_identity(successor)?;
        Ok(successor.build > build
            && successor
                .min_build
                .is_some_and(|floor| floor >= required_floor))
    })
}

fn published_commit(published: &Published) -> Result<String> {
    let manifest = validate_published_identity(published)?;
    Ok(manifest
        .commit
        .expect("validated published identity has an immutable commit")
        .to_ascii_lowercase())
}

/// A completed yank is not converged while a killed cleanup publisher still
/// owns the global release refs.  Returning success in that state would make a
/// later cut look mysteriously wedged, so a response-lost final unlock is
/// handled by the ordinary explicit stopped-publisher recovery lane.
fn ensure_yank_cleanup_session_absent(git: &ledger::GitCli, successor: &Published) -> Result<()> {
    let expected_owner = published_commit(successor)?;
    let owner = publish::release_lease_owner(git)?;
    let fence = publish::publisher_fence(git)?;
    match (owner.as_deref(), fence.as_ref()) {
        (None, None) => Ok(()),
        (Some(observed), None) if observed == expected_owner => Err(Error::new(format!(
            "yank cleanup is remotely converged but its release lease is still owned by the \
             verified successor claim {expected_owner}; after proving the old publisher is \
             stopped, run `cargo ship recover v{} {expected_owner} \
             --old-publisher-stopped`, then rerun yank",
            successor.version
        ))),
        (Some(observed), Some(current))
            if observed == expected_owner && current.owner == expected_owner =>
        {
            Err(Error::new(format!(
                "yank cleanup is remotely converged but its publisher session is still active \
                 for verified successor claim {expected_owner}; after proving the old publisher \
                 is stopped, run `cargo ship recover v{} {expected_owner} \
                 --old-publisher-stopped`, then rerun yank",
                successor.version
            )))
        }
        (Some(observed), Some(current)) if current.owner == observed => Err(Error::new(format!(
            "yank cleanup is remotely converged, but another coherent release publisher owns \
             claim {observed}; finish or explicitly recover that publisher before rerunning yank"
        ))),
        (Some(observed), None) => Err(Error::new(format!(
            "yank cleanup is remotely converged, but release claim {observed} is active; finish \
             or explicitly recover that publisher before rerunning yank"
        ))),
        (observed, Some(current)) => Err(Error::new(format!(
            "yank cleanup found incoherent release coordination refs: lease owner {}, fence \
             token {} peels to {}; refusing to declare convergence or delete either ref",
            observed.unwrap_or("absent"),
            current.token,
            current.owner
        ))),
    }
}

fn yank_cleanup_failure(error: Error, successor: &Published, owner: &str) -> Error {
    Error::new(format!(
        "{error}; yank cleanup coordination remains fail-closed for successor claim {owner}. \
         After proving this publisher is stopped, run `cargo ship recover v{} {owner} \
         --old-publisher-stopped`, then rerun yank",
        successor.version
    ))
}

/// Re-read the exact bad manifest before deletion.  A build-number match alone
/// is insufficient: tag/version/commit and manifest bytes are immutable yank
/// identity, and duplicate matches fail closed.
fn prove_yank_target_present(slug: &str, expected: &Published) -> Result<bool> {
    let expected_release = expected.release.as_ref().ok_or_else(|| {
        Error::new("yank target scan carries no immutable GitHub release object snapshot")
    })?;
    if expected.release_id != Some(expected_release.id) {
        return Err(Error::new(
            "yank target listing ID differs from its immutable release-object snapshot",
        ));
    }
    let release_id = expected_release.id;
    let exact = publish::release_object_by_id(slug, release_id)?;
    if exact.is_none() {
        return Ok(false);
    }
    publish::validate_release_object_snapshot(exact.as_ref(), expected_release)?;
    // The tag is intentionally allowed to be absent here: yank deletes it
    // first, then retains this exact release snapshot + manifest as its durable
    // crash-recovery receipt.
    let scanned = scan_published_snapshot(slug, false)?;
    let matches: Vec<&Published> = scanned
        .iter()
        .filter(|published| published.build == expected.build)
        .collect();
    let [observed] = matches.as_slice() else {
        return Err(Error::new(format!(
            "yank target build {} no longer has exactly one parseable published identity (found {})",
            expected.build,
            matches.len()
        )));
    };
    validate_published_identity(observed)?;
    if observed.tag != expected.tag
        || observed.release_id != Some(release_id)
        || observed.release.as_ref() != Some(expected_release)
        || observed.version != expected.version
        || observed.text.as_bytes() != expected.text.as_bytes()
    {
        return Err(Error::new(format!(
            "yank target build {} changed tag/version/manifest bytes; refusing destruction",
            expected.build
        )));
    }
    Ok(true)
}

fn delete_yank_release_convergently(
    repo: &Path,
    slug: &str,
    bad: &Published,
    git: &ledger::GitCli,
    lease: &publish::ReleaseLeaseGuard,
    fence: &publish::PublisherFenceGuard,
) -> Result<()> {
    prove_yank_successor(repo, slug, bad)?
        .ok_or_else(|| Error::new("ratcheted successor proof disappeared before yank cleanup"))?;
    if !prove_yank_target_present(slug, bad)? {
        return Ok(());
    }
    let expected = bad.release.as_ref().ok_or_else(|| {
        Error::new("yank target scan carries no immutable GitHub release object snapshot")
    })?;
    publish::delete_release_object_by_id_with_guard(
        slug,
        expected,
        true,
        || {
            prove_yank_successor(repo, slug, bad)?.ok_or_else(|| {
                Error::new("ratcheted successor proof disappeared before exact-ID release recheck")
            })?;
            Ok(())
        },
        || publish::assert_publisher_session(git, lease, fence),
    )?;
    Ok(())
}

/// What `cargo ship yank` must be told before it can publish anything: the
/// SIGNING inputs of the successor cut, and only those.
///
/// A yank's first act is a real, published cut — [`run_yank`] publishes the
/// ratcheted successor before it deletes one byte — so it is held to every rule
/// a cut is held to. It needed no inputs at all for as long as an unarmed tree
/// signed from an ambient key and asked nobody anything. Arming the paper
/// master (`aterm_update_core::pins::PAPER_MASTER_PUBKEYS`, 2026-08-15) changed
/// what a publish requires, in two ways that both land on this command:
///
/// * `publish::channel_signature_policy` refuses pre-claim when no signing
///   material resolves at all, and the only material that resolves without the
///   flag — the bare `~/.aterm/machine.key` fallback in
///   `sign::ReleaseCredentials::resolve` — names no notarytool credential, so
///   with `pins::APPLE_TEAM_ID` pinned `sign::resolve_apple_tier` refuses that
///   machine's artifact anyway. Either way the profile has to be nameable.
/// * When the rostered signing key is not the committed channel head — the
///   ordinary case the roster tier exists to enable — the cut refuses until the
///   operator answers the pre-roster stranding question out loud
///   ([`publish::PreRosterClients`]).
///
/// `yank` had no way to be told either answer, so the one command whose whole
/// purpose is to retire a bad published build could not run at all on the armed
/// tree: it refused inside its own successor cut, having published nothing and
/// deleted nothing.
///
/// The remaining cut flags are deliberately absent rather than merely
/// unforwarded: `--dry-run`/`--rehearse` would leave no published successor to
/// prove, `--resume` belongs to the journal (and [`run_yank`] already refuses an
/// unfinished journaled cut outright), and `--min-build`/`--set-version` are the
/// yank's own decision — the floor is `bad build + 1`, from
/// `yank_required_floor`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct YankOptions {
    /// Path to the ONE credentials profile, forwarded verbatim to the successor
    /// cut. `None` resolves exactly the way a bare `cargo ship cut` does — this
    /// machine's provisioned identity — so an unarmed or single-machine tree
    /// still needs no flag.
    pub release_credentials: Option<PathBuf>,
    /// The operator's `--strand-pre-roster-clients` acknowledgement, forwarded
    /// verbatim. It is never inferred here: only the operator knows whether a
    /// pre-roster client is left in the field, and "I am yanking" is not an
    /// answer to that question.
    pub strand_pre_roster_clients: bool,
}

/// The successor cut a yank of a bad build must publish: the ratcheted floor
/// plus the operator's signing inputs, and nothing else.
///
/// Pure, and split out of [`run_yank`], because the defect it fixes was a
/// silently DROPPED field — a `..Default::default()` that quietly answered the
/// armed tree's two new questions with "nothing" — and no test of a
/// network-driving command would ever have caught that.
fn successor_cut_options(required_floor: u64, opts: &YankOptions) -> publish::CutOptions {
    publish::CutOptions {
        min_build: Some(required_floor),
        release_credentials: opts.release_credentials.clone(),
        strand_pre_roster_clients: opts.strand_pre_roster_clients,
        ..Default::default()
    }
}

/// `cargo ship yank <build>` (spec decision 21): FIRST publish/prove a
/// min_build-ratcheted successor under a fresh claim, THEN optionally remove
/// the now-inert bad release/tag. A crash at every cleanup edge leaves the
/// successor authoritative; delete-before-successor is structurally absent.
///
/// `opts` is the successor cut's signing input and nothing else; see
/// [`YankOptions`] for why a yank has to carry one at all.
pub fn run_yank(repo: &Path, build: u64, opts: &YankOptions) -> Result<()> {
    let slug = slug_of(repo)?;
    println!("aterm-release · yank build {build} ({slug})");
    let git = ledger::GitCli::new(repo);
    publish::assert_origin_repo_binding(&git, &slug)?;
    // Keep the release object + manifest discoverable after tag-first cleanup;
    // a resumed yank may legitimately find the bad tag already absent.
    let scanned = scan_published_snapshot(&slug, false)?;
    let matching = scanned
        .iter()
        .filter(|published| published.build == build)
        .count();
    if matching == 0
        && let Some(successor) = prove_absent_yank_converged(repo, &slug, build)?
    {
        ensure_yank_cleanup_session_absent(&git, &successor)?;
        step(
            "yank",
            &format!(
                "already converged: no release carries build {build}; {} build {} is verified with min_build {}",
                successor.tag,
                successor.build,
                successor.min_build.unwrap_or_default()
            ),
        );
        return Ok(());
    }
    let bad = unique_yank_target(&scanned, build).map_err(|error| {
        let live = scanned
            .iter()
            .map(|published| format!("{} ({})", published.build, published.tag))
            .collect::<Vec<_>>()
            .join(", ");
        Error::new(format!(
            "{error} — published identities: {}",
            if live.is_empty() { "none" } else { &live }
        ))
    })?;
    let required_floor = yank_required_floor(build)?;

    // An unfinished local cut remains actionable operator state. A completed
    // journal is harmless history and run_cut will replace it if needed.
    let journal_path = repo.join("dist/cut-state.toml");
    if let Some(j) = publish::Journal::load(&journal_path)?
        && j.first_incomplete().is_some()
    {
        return Err(Error::new(format!(
            "an unfinished cut is journaled: v{} (build {}) — finish it (`cargo ship cut \
             --resume`) or discard it (`cargo ship cut --abandon v{}`) before yanking; \
             nothing was deleted",
            j.version, j.build_number, j.version
        )));
    }

    if prove_yank_successor(repo, &slug, &bad)?.is_none() {
        step(
            "yank",
            &format!(
                "publishing successor FIRST with min_build = {required_floor}; bad release remains live until proof passes"
            ),
        );
        // The successor is an ORDINARY cut and is held to every rule one is, so
        // the operator's credentials profile and stranding acknowledgement ride
        // through unchanged. Building these options from `Default` alone was the
        // bug: on the armed tree the cut then refused pre-claim ("no signing
        // material was supplied" / "pass --strand-pre-roster-clients"), naming
        // flags `yank` had no way to accept — so the bad build stayed live and
        // un-poisoned, which is the exact outcome this command exists to end.
        //
        // Nothing is pre-validated here on purpose. A missing answer earns the
        // cut's own pre-claim refusal, which is free, burns no ledger number,
        // and states the remedy far better than a second copy of the rule here
        // could — a copy that would also drift the moment the tier changes again.
        publish::run_cut(repo, &successor_cut_options(required_floor, opts))?;
    } else {
        step(
            "yank",
            &format!("ratcheted successor already live above build {build} — resuming cleanup"),
        );
    }
    let successor = prove_yank_successor(repo, &slug, &bad)?.ok_or_else(|| {
        Error::new("successor cut returned without establishing the required yank proof")
    })?;
    // THE FLEET READS THE PUBLIC CHANNEL, NOT THE ORIGIN. A successor that is live on
    // the origin only (a cut retired unmirrored after a roster join, or a mirror
    // step that never ran) poisons nothing for any installed copy; deleting the bad
    // build here would leave the channel's head BELOW the bad build's floor with no
    // manifest carrying it (2026-08-19 round-3 audit). Prove the successor on the
    // channel before any cleanup mutation.
    prove_successor_on_channel(repo, &successor)?;
    let cleanup_owner = published_commit(&successor)?;

    // Cleanup is a release-channel mutation even though the bad build has
    // already been poisoned.  Claim the verified successor commit as the
    // durable recovery identity, then add a unique per-process token so two
    // simultaneous yank resumes can never both delete.
    let cleanup_lease = publish::acquire_release_lease(&git, &cleanup_owner)?;
    let cleanup_fence = publish::acquire_publisher_fence(&git, &cleanup_owner)
        .map_err(|error| yank_cleanup_failure(error, &successor, &cleanup_owner))?;

    step(
        "yank",
        &format!(
            "{} build {} is signed/verified, newer than {}, and carries min_build >= {}; cleaning inert {}",
            successor.tag, successor.build, build, required_floor, bad.tag
        ),
    );
    let bad_manifest = validate_published_identity(&bad)?;
    let bad_commit = bad_manifest
        .commit
        .as_deref()
        .expect("validated published identity has commit");
    publish::delete_release_tag_with_guard(&git, &bad.tag, bad_commit, || {
        prove_yank_successor(repo, &slug, &bad)?
            .map(|_| ())
            .ok_or_else(|| {
                Error::new("ratcheted successor proof disappeared before tag cleanup")
            })?;
        // Keep the unique-session assertion immediately adjacent to each
        // local/remote tag delete performed by the helper.
        publish::assert_publisher_session(&git, &cleanup_lease, &cleanup_fence)
    })
    .map_err(|error| yank_cleanup_failure(error, &successor, &cleanup_owner))?;
    // Tag first keeps the published manifest available as the durable cleanup
    // receipt. A crash can rediscover the exact build and retry. The release
    // remains updater-visible (but inert below the successor) until the final
    // convergent delete; deleting the release first would lose tag identity.
    delete_yank_release_convergently(repo, &slug, &bad, &git, &cleanup_lease, &cleanup_fence)
        .map_err(|error| yank_cleanup_failure(error, &successor, &cleanup_owner))?;

    // A successful REST response is not the final proof: replay the
    // absent-target + verified-floor condition before atomically releasing
    // both coordination refs.  Any ambiguity retains the refs for explicit
    // stopped-publisher recovery.
    prove_absent_yank_converged(repo, &slug, build)?
        .ok_or_else(|| Error::new("yank cleanup finished without a converged successor proof"))
        .map_err(|error| yank_cleanup_failure(error, &successor, &cleanup_owner))?;
    publish::assert_publisher_session(&git, &cleanup_lease, &cleanup_fence)
        .map_err(|error| yank_cleanup_failure(error, &successor, &cleanup_owner))?;
    match publish::release_completed_publisher_session(&git, &cleanup_owner, &cleanup_fence)
        .map_err(|error| yank_cleanup_failure(error, &successor, &cleanup_owner))?
    {
        publish::LeaseRelease::Released | publish::LeaseRelease::AlreadyAbsent => {}
        publish::LeaseRelease::AlreadySuperseded => {
            return Err(Error::new(
                "yank cleanup converged, but this publisher was superseded before final unlock; \
                 the winning release session was left untouched and must finish before yank can \
                 report success",
            ));
        }
    }
    step(
        "",
        &format!(
            "DONE — bad build {build} is poisoned by min_build {required_floor}; release/tag cleanup converged"
        ),
    );
    Ok(())
}

/// `cargo ship cut --abandon vX.Y.Z` (spec §5): delete any draft release, any
/// tag the failed cut minted (local AND origin — spec decision 5's "a failed
/// cut never leaves a public tag"), and the local journal; the claim commit
/// stays (the ledger is append-only). A later cut of the version recuts with
/// a fresh number.
pub fn run_abandon(repo: &Path, version: &str) -> Result<()> {
    let slug = slug_of(repo)?;
    let tag = format!("v{version}");
    println!("aterm-release · abandon {tag} ({slug})");
    let journal_path = repo.join("dist/cut-state.toml");
    let journal = publish::Journal::load(&journal_path)?.ok_or_else(|| {
        Error::new(format!(
            "there is no matching v{version} journal proving an owner; refusing destructive \
             abandon. If another machine was lost, prove its publisher stopped, then use \
             `cargo ship recover v{version} <full-claim-sha> --old-publisher-stopped`"
        ))
    })?;
    if journal.version != version {
        return Err(Error::new(format!(
            "local journal is v{}, not requested abandon v{version}",
            journal.version
        )));
    }
    journal.ensure_resumable()?;
    if journal.first_incomplete().is_none() {
        return Err(Error::new(format!(
            "release journal v{version} is already complete; abandon has no unfinished-cut authority"
        )));
    }
    let git = ledger::GitCli::new(repo);
    publish::assert_origin_repo_binding(&git, &slug)?;
    publish::ordinary_resume_claim_preflight(repo, &git, &journal)?;

    // Published releases are outside abandon's authority. Check this before
    // acquiring a previously absent lease so a mistaken command cannot leave
    // a new lock behind merely to report the published-state refusal.
    if release_state(&slug, &tag)? == ReleaseState::Published {
        return Err(Error::new(format!(
            "{tag} is PUBLISHED — abandon only covers drafts; retire a published \
             build with `cargo ship yank <build>`"
        )));
    }

    let owner = journal.commit.clone();
    let lease = publish::acquire_release_lease(&git, &owner)?;
    let fence = publish::acquire_publisher_fence(&git, &owner)?;
    let action = (|| -> Result<()> {
        let deleted = publish::delete_owned_draft_release(
            repo,
            &slug,
            &tag,
            journal.release_id,
            Some(journal.draft_create_issued),
            &lease,
            &fence,
        )?;
        let message = if deleted {
            format!("draft release {tag} deleted")
        } else {
            format!("no draft release {tag} — nothing remote to delete")
        };
        step("abandon", &message);
        publish::assert_publisher_session(&git, &lease, &fence)?;
        publish::delete_owned_release_tag(&git, &tag, &owner, &lease, &fence)?;
        publish::assert_publisher_session(&git, &lease, &fence)?;
        let released = publish::release_completed_publisher_session(&git, &owner, &fence)?;
        if released == publish::LeaseRelease::AlreadySuperseded {
            return Err(Error::new(
                "abandon was fenced out before final unlock; journal retained for the winner",
            ));
        }
        fs::remove_file(&journal_path)
            .map_err(|e| Error::new(format!("delete {}: {e}", journal_path.display())))?;
        step(
            "",
            "owner + unique publisher fence atomically released; local journal deleted",
        );
        step(
            "",
            &format!(
                "the claim commit stays (append-only ledger; the burned number is normal). A \
                 later `cargo ship cut` of v{version} recuts it with a fresh number."
            ),
        );
        Ok(())
    })();
    let cleanup = publish::release_publisher_fence(&git, &fence).map(|_| ());
    match (action, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(cleanup)) => Err(Error::new(format!(
            "abandon completed but exact fence cleanup failed: {cleanup}"
        ))),
        (Err(error), Err(cleanup)) => Err(Error::new(format!(
            "{error}; exact fence cleanup also failed: {cleanup}"
        ))),
    }
}

/// THE SUPPORTED EXIT FOR A FLIPPED-BUT-UNMIRRORED CUT THE FLEET HAS MOVED PAST.
///
/// A cut that flipped on the origin and then stopped before `mirror` (probe
/// timeout, GitHub 5xx, Ctrl-C) is resumable — but a roster join is not
/// lease-gated, and one landing before the resume re-dresses the public head under
/// a newer generation. `step_mirror` then refuses, correctly: mirroring this cut's
/// older-generation roster would strand every ratcheted client. Before this verb
/// that refusal had no terminal move — `--resume`/`recover` re-enter the same
/// refusal, `--abandon` refuses a published release, `yank` refuses an unfinished
/// journal, and the held lease blocks every fresh cut — so the operator was left
/// with the ref surgery the docs forbid (2026-08-19 review).
///
/// What it does: proves the journal is this version's, published on the origin,
/// stopped at or before `mirror`, and that the public channel's roster generation
/// is strictly AHEAD of the generation this cut carries (the one condition under
/// which the mirror can never legitimately proceed); then, as the journal's owner,
/// releases the lease + publisher fence and deletes the journal. The origin
/// release stays exactly as it is (it is live privately and tells the truth), the
/// claim commit stays (the number is burned, as always), and the public channel —
/// which never saw this cut — is superseded by the next cut, attributed under the
/// current generation. Nothing older is ever mirrored.
pub fn run_retire_unmirrored(repo: &Path, version: &str) -> Result<()> {
    let slug = slug_of(repo)?;
    let tag = format!("v{version}");
    println!("aterm-release · retire-unmirrored {tag} ({slug})");
    let journal_path = repo.join("dist/cut-state.toml");
    let journal = publish::Journal::load(&journal_path)?.ok_or_else(|| {
        Error::new(format!(
            "there is no v{version} journal on this machine; retire-unmirrored acts only for the \
             cut's own publisher. A lost publisher is `cargo ship recover …`'s case"
        ))
    })?;
    if journal.version != version {
        return Err(Error::new(format!(
            "local journal is v{}, not requested v{version}",
            journal.version
        )));
    }
    journal.ensure_resumable()?;
    match journal.first_incomplete() {
        Some("mirror") => {}
        Some("unlock") => {
            return Err(Error::new(format!(
                "v{version} already flipped on the public channel (only `unlock` is pending); \
                 there is nothing unmirrored to retire — finish it with `cargo ship cut --resume`"
            )));
        }
        Some(step) => {
            return Err(Error::new(format!(
                "v{version} stopped at step {step:?}, before the origin flip; that is \
                 `--resume`'s or `--abandon`'s case, not a retire"
            )));
        }
        None => {
            return Err(Error::new(format!(
                "release journal v{version} is already complete; nothing to retire"
            )));
        }
    }
    let git = ledger::GitCli::new(repo);
    publish::assert_origin_repo_binding(&git, &slug)?;
    // The journal is never authority on its own: prove its claim commit is a real
    // claim on origin/main whose ledger tail names this version/build, exactly as
    // `--abandon` does before it acts on anything.
    publish::ordinary_resume_claim_preflight(repo, &git, &journal)?;
    if release_state(&slug, &tag)? != ReleaseState::Published {
        return Err(Error::new(format!(
            "{tag} is not published on the origin; a draft is `--abandon`'s case"
        )));
    }
    // The ONE condition: the public channel's roster generation is strictly ahead of
    // the generation this cut carries, so the mirror can never proceed.
    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("cannot read workspace Cargo.toml: {e}")))?;
    let mirror_slug = crate::mirror::update_channel_slug(&cargo_text)?.ok_or_else(|| {
        Error::new("no public update channel is configured; there is no mirror to retire from")
    })?;
    let carried = {
        let manifest_path = repo.join("dist").join("aterm-appcast.toml");
        let text = fs::read_to_string(&manifest_path).map_err(|e| {
            Error::new(format!(
                "read {} to learn which roster generation v{version} carries: {e}",
                manifest_path.display()
            ))
        })?;
        aterm_update_core::Manifest::parse(&text)
            .map_err(|e| Error::new(format!("staged manifest re-parse failed: {e}")))?
            .roster_seq
    };
    let fleet = crate::machines::channel_roster_document(&mirror_slug).map_err(|e| {
        Error::new(format!(
            "cannot read the public channel {mirror_slug}'s roster ({e}); refusing to retire a \
             cut whose mirror might still be able to proceed"
        ))
    })?;
    // The two refusals `step_mirror` can issue for good, and only those: the fleet's
    // generation is strictly AHEAD of this cut's, or EQUAL with a different document
    // (a lineage fork). Anything else is `--resume`'s case.
    // The roster THIS CUT SHIPPED is the asset on its origin release — not dist/,
    // which the fork remedy tells the operator to overwrite with the channel's
    // document (which would then make the fork invisible from here and wedge).
    let shipped_roster = || -> Result<Vec<u8>> {
        let release_id = journal.release_id.ok_or_else(|| {
            Error::new(format!(
                "journal v{version} records no origin release ID; cannot read the roster this \
                 cut shipped"
            ))
        })?;
        publish::download_release_asset_for_release_id(
            &slug,
            release_id,
            aterm_update_core::roster::ROSTER_ASSET,
        )
    };
    match (carried, fleet.as_ref()) {
        (Some(carried), Some((fleet, _))) if *fleet > carried => step(
            "retire",
            &format!(
                "v{version} carries roster generation {carried}; the public channel's head is at \
                 {fleet} — the mirror can never proceed, so this cut retires unmirrored"
            ),
        ),
        (Some(carried), Some((fleet, bytes)))
            if *fleet == carried && shipped_roster()? != *bytes =>
        {
            step(
                "retire",
                &format!(
                    "v{version} and the public channel's head both carry roster generation \
                     {carried} with DIFFERENT documents (a lineage fork) — the mirror can never \
                     proceed, so this cut retires unmirrored; re-join from the machine holding \
                     the channel's document before the next cut"
                ),
            );
        }
        (carried, fleet) => {
            return Err(Error::new(format!(
                "v{version} carries roster generation {carried:?} and the public channel's head \
                 is at {:?}: the mirror is not refused by the fleet's floor or a lineage fork, \
                 so finish it with `cargo ship cut --resume` instead of retiring",
                fleet.map(|(seq, _)| *seq)
            )));
        }
    }
    let owner = journal.commit.clone();
    let lease = publish::acquire_release_lease(&git, &owner)?;
    let fence = publish::acquire_publisher_fence(&git, &owner)?;
    let action = (|| -> Result<()> {
        publish::assert_publisher_session(&git, &lease, &fence)?;
        // A public DRAFT this cut already created (assets uploaded, refused at the
        // pre-flip ratchet) must not stay behind as an unlisted release carrying the
        // older-generation roster with no journal pointing at it. Delete it by its
        // immutable ID, only if it is still a draft under our tag.
        if let Some(id) = journal.mirror_release_id {
            // Under the CHANNEL credential: the dev account cannot see a draft on
            // the public channel (404), and "not found" here would otherwise read as
            // "already gone". A missing channel token is a refusal, not a skip.
            if publish::channel_token().is_none() {
                return Err(Error::new(format!(
                    "this cut created public draft {tag} (ID {id}) on {mirror_slug}, and deleting \
                     it needs the release-org token ({}); provide it before retiring",
                    publish::channel_token_path()
                        .map_or_else(|| "channel token".to_string(), |p| p.display().to_string())
                )));
            }
            publish::with_channel_cred(|| {
                match publish::release_object_by_id(&mirror_slug, id)? {
                    Some(draft) if draft.draft && draft.tag == tag => {
                        let endpoint = format!("repos/{mirror_slug}/releases/{id}");
                        let out = publish::gh_raw(&["api", "--method", "DELETE", &endpoint])?;
                        if publish::release_object_by_id(&mirror_slug, id)?.is_some() {
                            return Err(Error::new(format!(
                                "could not delete the orphaned public draft {tag} (ID {id}) on \
                                 {mirror_slug}: {}",
                                out.stderr_utf8().trim()
                            )));
                        }
                        step(
                            "retire",
                            &format!("orphaned public draft {tag} (ID {id}) deleted"),
                        );
                    }
                    Some(other) => {
                        return Err(Error::new(format!(
                            "the public release ID {id} on {mirror_slug} is {} under tag {:?}; \
                             refusing to touch it",
                            if other.draft { "a draft" } else { "LIVE" },
                            other.tag
                        )));
                    }
                    None => {}
                }
                Ok(())
            })?;
        }
        let released = publish::release_completed_publisher_session(&git, &owner, &fence)?;
        if released == publish::LeaseRelease::AlreadySuperseded {
            return Err(Error::new(
                "retire was fenced out before final unlock; journal retained for the winner",
            ));
        }
        fs::remove_file(&journal_path)
            .map_err(|e| Error::new(format!("delete {}: {e}", journal_path.display())))?;
        step(
            "",
            "owner + unique publisher fence atomically released; local journal deleted",
        );
        step(
            "",
            &format!(
                "{tag} stays live on the origin exactly as it is; the public channel never saw it \
                 and the next cut (attributed under the current roster generation) supersedes \
                 it. The claim commit stays (append-only ledger; the burned number is normal)."
            ),
        );
        Ok(())
    })();
    let cleanup = publish::release_publisher_fence(&git, &fence).map(|_| ());
    match (action, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(cleanup)) => Err(Error::new(format!(
            "retire completed but exact fence cleanup failed: {cleanup}"
        ))),
        (Err(error), Err(cleanup)) => Err(Error::new(format!(
            "{error}; exact fence cleanup also failed: {cleanup}"
        ))),
    }
}

/// "owner/repo" from the workspace manifest — the single source the client's
/// compiled-in default also uses.
fn slug_of(repo: &Path) -> Result<String> {
    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read Cargo.toml: {e}")))?;
    publish::repo_slug(&cargo_text).ok_or_else(|| {
        Error::new(
            "Cargo.toml [workspace.package] repository is not an exact GitHub OWNER/REPO URL",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One page of `release_identity_jq(true)` output, exactly as `gh` hands it
    /// over: id, tag, draft, target, tab-separated. Release 9 is the shape the
    /// GitHub web UI produces when a draft is created without picking a tag.
    const PAGE_WITH_A_TAGLESS_DRAFT: &str = "9\t\ttrue\tmain\n12\tv0.3.0\tfalse\tabc123\n";

    /// The regression the row filter exists to stop. The exhaustive scan skips
    /// that tagless draft outright (`release.draft` guard in
    /// [`scan_release_page`], over the tolerant [`parse_release_metadata`]), so
    /// `ship status`/`yank` succeed with it in the repo. Feeding the whole page
    /// to the strict identity parser would have made that unrelated release
    /// abort the batched recheck of the releases the scan DID capture.
    #[test]
    fn an_unrelated_tagless_draft_cannot_abort_the_batched_recheck() {
        // Unfiltered the page really is fatal — this is what makes the filter
        // load-bearing rather than decorative.
        assert!(publish::parse_release_object_identity_rows(PAGE_WITH_A_TAGLESS_DRAFT).is_err());
        let wanted = std::collections::BTreeSet::from([12]);
        let selected = captured_identity_rows(PAGE_WITH_A_TAGLESS_DRAFT, &wanted);
        let rows = publish::parse_release_object_identity_rows(&selected)
            .expect("the captured rows alone must parse");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 12);
        assert_eq!(rows[0].tag, "v0.3.0");
        assert!(!rows[0].draft);
        assert_eq!(rows[0].target_commitish, "abc123");
    }

    /// Narrowing the batch must not SOFTEN it. When the malformed row belongs to
    /// a release this scan captured, the strict parse still refuses — that ID's
    /// identity is authority for a destructive yank.
    #[test]
    fn a_captured_row_that_is_malformed_still_fails_closed() {
        let wanted = std::collections::BTreeSet::from([9, 12]);
        let selected = captured_identity_rows(PAGE_WITH_A_TAGLESS_DRAFT, &wanted);
        let err = publish::parse_release_object_identity_rows(&selected)
            .expect_err("an empty tag on a CAPTURED release is still fatal");
        assert!(
            err.to_string().contains("empty/zero identity field"),
            "{err}"
        );
    }

    /// A row we cannot even attribute is dropped rather than parsed: it cannot
    /// be one of ours, because ours are identified by numeric ID. The captured
    /// ID is then simply absent from the live map and
    /// [`scan_published_snapshot`] reports the scan as torn — the same refusal a
    /// mid-scan deletion earns. An empty selection is not itself an error.
    #[test]
    fn a_row_with_an_unreadable_id_is_dropped_not_parsed() {
        let page = "not-a-number\tv0.3.0\tfalse\tabc123\n";
        let wanted = std::collections::BTreeSet::from([12]);
        assert_eq!(captured_identity_rows(page, &wanted), "");
        assert!(
            publish::parse_release_object_identity_rows("")
                .expect("an empty selection parses")
                .is_empty()
        );
    }

    /// The successor a yank publishes is a real cut, and since the paper master
    /// was armed a real cut refuses pre-claim unless it is told WHICH profile
    /// signs and whether stranding pre-roster clients is acceptable. Those two
    /// answers used to be dropped on the floor here, which made `cargo ship
    /// yank` unable to publish anything at all on the armed tree. Nothing else
    /// may leak in either: a yank's successor is always a real, published,
    /// floor-ratcheted cut, never a dry run, a rehearsal, or a resume.
    #[test]
    fn the_successor_cut_carries_the_operators_signing_inputs_and_stays_a_real_cut() {
        let cut = successor_cut_options(
            1_783_918_102,
            &YankOptions {
                release_credentials: Some(PathBuf::from("/keys/m3.toml")),
                strand_pre_roster_clients: true,
            },
        );
        assert_eq!(cut.min_build, Some(1_783_918_102));
        assert_eq!(
            cut.release_credentials.as_deref(),
            Some(Path::new("/keys/m3.toml"))
        );
        assert!(cut.strand_pre_roster_clients);
        assert!(!cut.dry_run && !cut.resume && !cut.gate && !cut.arm64_only);
        assert!(cut.rehearse.is_none() && cut.set_version.is_none());

        // A flagless yank must still ask for exactly the cut it always asked
        // for, so an unarmed tree (and every fork with no master pinned) sees no
        // behaviour change from the fix.
        assert_eq!(
            successor_cut_options(7, &YankOptions::default()),
            publish::CutOptions {
                min_build: Some(7),
                ..Default::default()
            }
        );
    }
}
