// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Post-publish verify (release spec §7 step 7; also the standalone
//! `ship verify [vX.Y]`): replay the CLIENT's release-selection rule (the
//! greatest canonical `vMAJOR.MINOR` non-draft release carrying the exact
//! appcast name) against the live API with no cache and require it to select our cut; download the
//! published manifest and assert BYTE-identity with the local artifact; `HEAD`
//! the DMG URL → 200. Prints PASS + release URL, or the exact remediation.
//! Absorbs the deleted tools/check-published.sh.
//!
//! This module also owns the OTHER read-side surfaces built on the same scan:
//! `ship status` (ledger tail vs releases API — dangling claims, cask-pin
//! freshness), the remote-derived resume/recut decision of spec §5 (pure —
//! tests/resume.rs pins the table), `cut --abandon`, and `ship yank`.

use std::fs;
use std::path::Path;
use std::process::Command;

use aterm_update_core::Manifest;

use crate::ledger::{self, Error, Result};
use crate::manifest_out;
use crate::publish::{self, gh_retry, step};

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
/// `vMAJOR.MINOR` tag is authoritative independent of REST row order, and exactly
/// that one manifest is fetched. `stop_early: false` is the operator/history view:
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

/// Production scan for the origin channel. In addition to the immutable
/// release-object closure, every canonical two-component protocol release is
/// bound to its exact remote tag (annotated or legacy-lightweight). Older
/// three-component archive rows predate that invariant and remain accounting
/// history only; several intentionally tag the post-release merge descendant.
pub fn scan_published_in_repo(repo: &Path, slug: &str, stop_early: bool) -> Result<Vec<Published>> {
    let git = ledger::GitCli::new(repo);
    let found = scan_published_snapshot(slug, stop_early)?;
    let mut bindings = Vec::with_capacity(found.len());
    for published in &found {
        let commit = validate_published_identity(published)?
            .commit
            .expect("validated published identity has commit");
        if parse_canonical_tag(&published.tag).is_ok() {
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
        let before = publish::release_object_by_id(slug, release_id)?;
        publish::validate_release_object_tag_state(before.as_ref(), release_id, tag, false)?;
        let before = before.expect("validated release object is present");
        if identities.insert(release_id, before.clone()).is_some() {
            return Err(Error::new(format!(
                "release ID {release_id} appeared more than once in the authoritative listing"
            )));
        }
        let bytes = publish::download_release_asset_for_release_id(slug, release_id, asset)?;
        let after = publish::release_object_by_id(slug, release_id)?;
        if after.as_ref() != Some(&before) {
            return Err(Error::new(format!(
                "release ID {release_id} identity changed during authoritative manifest download"
            )));
        }
        Ok(bytes)
    })?;
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
        let observed = publish::release_object_by_id(slug, release_id)?;
        if observed.as_ref() != Some(captured) {
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

/// Numeric order for both the canonical two-component protocol and the exact
/// appcast heads published before it. Every component participates in
/// lexicographic numeric ordering, so `v0.21.2607041853 < v0.54` while
/// `v0.54.1 > v0.54`.
fn parse_numeric_tag(tag: &str) -> Result<Vec<u64>> {
    let version = tag.strip_prefix('v').ok_or_else(|| {
        Error::new(format!(
            "published appcast tag {tag:?} is not numerically orderable"
        ))
    })?;
    let mut components = Vec::new();
    for component in version.split('.') {
        if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::new(format!(
                "published appcast tag {tag:?} is not numerically orderable"
            )));
        }
        components.push(component.parse::<u64>().map_err(|_| {
            Error::new(format!(
                "published appcast tag {tag:?} has an out-of-range numeric component"
            ))
        })?);
    }
    if components.len() < 2 {
        return Err(Error::new(format!(
            "published appcast tag {tag:?} is not numerically orderable"
        )));
    }
    Ok(components)
}

fn parse_canonical_tag(tag: &str) -> Result<String> {
    let components = parse_numeric_tag(tag)?;
    let [major, minor] = components.as_slice() else {
        return Err(Error::new(format!(
            "authoritative appcast tag {tag:?} is not canonical vMAJOR.MINOR"
        )));
    };
    let canonical_version = format!("{major}.{minor}");
    if tag.strip_prefix('v') != Some(canonical_version.as_str()) {
        return Err(Error::new(format!(
            "authoritative appcast tag {tag:?} is not canonical vMAJOR.MINOR"
        )));
    }
    Ok(canonical_version)
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
            let tag_order = parse_numeric_tag(release.tag)?;
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
        // Lower numeric dotted legacy heads are safely orderable during the
        // one-time archive migration. Channel authority itself must use the
        // current canonical two-component protocol.
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
    /// `[workspace.package]` version with the `.0` stripped, e.g. "0.26".
    pub cargo_short: String,
    /// Does CHANGELOG.md already carry `## [cargo_short]`?
    pub changelog_has_section: bool,
    /// Does a NON-DRAFT release `v<cargo_short>` exist? (A draft is not
    /// published — it is exactly the wedge recut exists to finish.)
    pub published: bool,
}

/// What kind of cut to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutMode {
    /// Normal: bump + roll + claim.
    Fresh { version: String },
    /// The §5 wedge signature (bump + roll landed on main, nothing published):
    /// skip bump/roll, reuse the rolled section, claim a FRESH n.
    Recut { version: String },
}

/// Derive the cut mode from remote-visible state (spec §5: "Cargo.toml
/// already 0.26 + `## [0.26]` section present + no published v0.26 release ⇒
/// recut") — this is what lets ANY machine finish a wedged cut with no
/// journal.
pub fn derive_cut_mode(s: &RemoteState, set_version: Option<&str>) -> Result<CutMode> {
    let pending = s.changelog_has_section && !s.published;
    match set_version {
        Some(v) if v == s.cargo_short => {
            if pending {
                Ok(CutMode::Recut {
                    version: v.to_string(),
                })
            } else if s.changelog_has_section && s.published {
                Err(Error::new(format!(
                    "v{v} is already published — cut the next version, or retire a bad \
                     build with `cargo ship yank <build>`"
                )))
            } else {
                Ok(CutMode::Fresh {
                    version: v.to_string(),
                })
            }
        }
        // An explicit different version is the operator's call — the tag +
        // cut-elsewhere gates still stand between it and a collision.
        Some(v) => Ok(CutMode::Fresh {
            version: v.to_string(),
        }),
        None => {
            if pending {
                Ok(CutMode::Recut {
                    version: s.cargo_short.clone(),
                })
            } else {
                Ok(CutMode::Fresh {
                    version: publish::bump_minor(&s.cargo_short)?,
                })
            }
        }
    }
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
            "live scan selects {tag} build {} · {byte_note} · {signature_note} · {dmg_note}",
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

/// `cargo ship verify [vX.Y]` — re-run the post-publish check anytime.
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
    let pubkey = matching.and_then(|journal| journal.signature_pubkey.as_deref());
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
/// releases API), latest published build, cask-pin freshness (spec §5).
pub fn run_status(repo: &Path) -> Result<()> {
    let slug = slug_of(repo)?;
    println!("aterm-release · status ({slug})");
    let authority = publish::load_channel_signing_authority(repo)?;
    step(
        "signing",
        &format!(
            "epoch {} activates at v{} · current key {}… · retired key {}…",
            authority.epoch,
            authority.activation_version,
            &authority.current_fingerprint_sha256[..12],
            &authority.retired_fingerprint_sha256[..12]
        ),
    );
    step(
        "",
        "lost epoch-1 secret: old-key-pinned v0.26 requires manual/bootstrap installation; unsigned-pin v0.27–v0.54 can migrate to signed v0.55",
    );

    let cargo_text = fs::read_to_string(repo.join("Cargo.toml"))
        .map_err(|e| Error::new(format!("read Cargo.toml: {e}")))?;
    let full = publish::workspace_version(&cargo_text)?;
    let short = publish::short_version(&full);
    step(
        "version",
        &format!(
            "{full} (Cargo.toml) — next default cut v{}",
            publish::bump_minor(&short)?
        ),
    );

    let ledger_text = fs::read_to_string(repo.join(ledger::LEDGER_FILE))
        .map_err(|e| Error::new(format!("read {}: {e}", ledger::LEDGER_FILE)))?;
    let records = ledger::parse(&ledger_text)?;
    let tail = ledger::tail(&ledger_text)?;
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

    // Cask pin freshness (spec §7 step 6 keeps it re-pinned per cut).
    let cask_path = repo.join("packaging/homebrew/aterm.rb");
    match fs::read_to_string(&cask_path) {
        Ok(text) => {
            let pin = cask_version(&text).unwrap_or_else(|| "?".to_string());
            let fresh = select_newest(&scanned).map(|b| b.tag.trim_start_matches('v') == pin);
            step(
                "cask",
                &match fresh {
                    Some(true) => format!("aterm.rb pins {pin} — fresh"),
                    Some(false) => format!("aterm.rb pins {pin} — STALE (next cut re-pins it)"),
                    None => format!("aterm.rb pins {pin} (nothing published to compare)"),
                },
            );
        }
        Err(e) => step(
            "cask",
            &format!("packaging/homebrew/aterm.rb unreadable: {e}"),
        ),
    }
    Ok(())
}

/// The cask's pinned `version "…"` (pure — tests/resume.rs).
pub fn cask_version(cask: &str) -> Option<String> {
    cask.lines()
        .map(str::trim_start)
        .find_map(|l| l.strip_prefix("version \""))
        .and_then(|rest| rest.split('"').next())
        .map(str::to_string)
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
    let required_floor = bad
        .build
        .checked_add(1)
        .ok_or_else(|| Error::new("cannot yank u64::MAX: min_build successor would overflow"))?;
    let bad_order = parse_numeric_tag(&bad.tag)?;
    let successor_order = parse_numeric_tag(&successor.tag)?;
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
        .and_then(|journal| journal.signature_pubkey.clone())
    {
        return Ok(Some(key));
    }
    Ok(None)
}

/// Re-prove that the bad release is already inert before every cleanup
/// mutation. The full post-publish replay covers canonical arbitration, exact
/// manifest bytes, current signature + all signed history, and DMG availability.
fn prove_yank_successor(repo: &Path, slug: &str, bad: &Published) -> Result<Option<Published>> {
    let scanned = scan_published_in_repo(repo, slug, true)?;
    let Some(successor) = scanned.first() else {
        return Ok(None);
    };
    if !yank_successor_covers(bad, successor)? {
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

/// Command-level convergence after tag-first cleanup and a release-delete
/// crash/response loss. The original manifest is gone, so only claim success
/// when no parsed release carries the build and the exact current authority is
/// newer, fully verified, and permanently poisons that build via min_build.
fn prove_absent_yank_converged(repo: &Path, slug: &str, build: u64) -> Result<Option<Published>> {
    let required_floor = build
        .checked_add(1)
        .ok_or_else(|| Error::new("cannot yank u64::MAX: successor min_build would overflow"))?;
    let scanned = scan_published_in_repo(repo, slug, true)?;
    let Some(successor) = scanned.first() else {
        return Ok(None);
    };
    validate_published_identity(successor)?;
    if successor.build <= build
        || !successor
            .min_build
            .is_some_and(|floor| floor >= required_floor)
    {
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

/// `cargo ship yank <build>` (spec decision 21): FIRST publish/prove a
/// min_build-ratcheted successor under a fresh claim, THEN optionally remove
/// the now-inert bad release/tag. A crash at every cleanup edge leaves the
/// successor authoritative; delete-before-successor is structurally absent.
pub fn run_yank(repo: &Path, build: u64) -> Result<()> {
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
    let required_floor = build
        .checked_add(1)
        .ok_or_else(|| Error::new("cannot yank u64::MAX: successor min_build would overflow"))?;

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
        publish::run_cut(
            repo,
            &publish::CutOptions {
                min_build: Some(required_floor),
                ..Default::default()
            },
        )?;
    } else {
        step(
            "yank",
            &format!("ratcheted successor already live above build {build} — resuming cleanup"),
        );
    }
    let successor = prove_yank_successor(repo, &slug, &bad)?.ok_or_else(|| {
        Error::new("successor cut returned without establishing the required yank proof")
    })?;
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

/// `cargo ship cut --abandon vX.Y` (spec §5): delete any draft release, any
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
