// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The read side of the release channel, and the verbs built on it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_update_core::Manifest;

use crate::gates;
use crate::ledger::{self, Error, Result};
use crate::manifest_out;
use crate::publish::{self, TagKind, step};

/// One published (non-draft) release carrying a parseable manifest.
#[derive(Debug, Clone)]
pub struct Published {
    /// Release ID carried by the listing row.
    pub release_id: Option<u64>,
    /// Exact immutable GitHub release-object snapshot.
    pub release: Option<publish::ReleaseObjectIdentity>,
    pub tag: String,
    pub build: u64,
    pub version: String,
    /// The exact asset the manifest was read from.
    pub asset: String,
    /// Optional apply floor carried by this exact manifest.
    pub min_build: Option<u64>,
    /// The manifest's exact downloaded bytes (strict UTF-8 — TOML is UTF-8 by definition, and a lossy decode would break the byte-identity check).
    pub text: String,
}

/// The channel's head before a claim.
pub fn scan_head_before_claim(slug: &str) -> Result<Vec<Published>> {
    let latest = latest_release_row(slug)?;
    let head = scan_published(slug, true)?;
    head_reaches_latest(
        latest.as_deref(),
        head.first().map(|published| published.tag.as_str()),
    )?;
    Ok(head)
}

/// The [`release_row`] of the release `releases/latest` names, or `None` when the channel has none (HTTP 404).
fn latest_release_row(slug: &str) -> Result<Option<String>> {
    let latest =
        crate::github::get_optional(&format!("repos/{slug}/releases/latest")).map_err(|error| {
            Error::new(format!(
                "read the channel's latest release failed: {error}; nothing was claimed"
            ))
        })?;
    Ok(latest.map(|release| crate::github::rows([release_row(&release)])))
}

/// [`scan_head_before_claim`]'s judgement, pure.
pub(crate) fn head_reaches_latest(latest: Option<&str>, head: Option<&str>) -> Result<()> {
    let Some(latest) = latest else {
        return Ok(());
    };
    let rows = parse_release_metadata(latest)?;
    let [row] = rows.as_slice() else {
        return Err(Error::new(format!(
            "the channel's latest release read as {} rows; nothing was claimed",
            rows.len()
        )));
    };
    if row.draft || row.prerelease || row.exact_count != 1 {
        return Ok(());
    }
    let TagKind::Candidate(latest_order) = publish::parse_release_tag(row.tag)? else {
        return Ok(());
    };
    let reached = match head {
        Some(tag) => matches!(
            publish::parse_release_tag(tag)?,
            TagKind::Candidate(order) if order >= latest_order
        ),
        None => false,
    };
    if reached {
        return Ok(());
    }
    Err(Error::new(format!(
        "the channel's release listing reaches no update candidate at or above {}, the release \
         its latest pointer names (the listing's newest: {}) — a torn read of the channel; \
         nothing was claimed — run the release again",
        row.tag,
        head.unwrap_or("none")
    )))
}

/// Scans the channel's release metadata (the listing read whole) before downloading any manifest.
pub fn scan_published(slug: &str, stop_early: bool) -> Result<Vec<Published>> {
    let metadata = release_metadata_listing(slug)?;
    let mut identities = std::collections::BTreeMap::new();
    let (_, mut found) = scan_release_page(&metadata, stop_early, |release_id, tag, asset| {
        let release_id = release_id.ok_or_else(|| {
            Error::new("production release scan row has no immutable GitHub release ID")
        })?;
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

/// One release object as the five-field row [`parse_release_metadata`] reads: id, tag, draft flag, prerelease flag, exact appcast count.
pub(crate) fn release_row(release: &aterm_json::Value) -> String {
    let exact_count = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|asset| asset["name"].as_str() == Some("aterm-appcast.toml"))
        .count();
    crate::github::tsv(&[
        &crate::github::tostring(&release["id"]),
        &release["tag_name"],
        &crate::github::tostring(&release["draft"]),
        &crate::github::tostring(&release["prerelease"]),
        &aterm_json::Value::String(exact_count.to_string()),
    ])
}

/// [`release_row`] of every release on `slug`, the listing read whole (an incomplete or
/// inconsistent listing is refused).
fn release_metadata_listing(slug: &str) -> Result<String> {
    let listing = publish::release_listing(slug)?;
    Ok(crate::github::rows(listing.iter().map(release_row)))
}

/// The refusal [`publish::release_asset_identity_for_release_id`] raises, verbatim: the
/// scan reads the binding through the fused release-object/asset read instead.
fn missing_release_asset(release_id: u64, name: &str) -> Error {
    Error::new(format!(
        "release ID {release_id} has 0 assets named {name:?}; expected exactly one"
    ))
}

/// The `wanted` release objects as the channel exposes them now, by immutable ID, from one
/// paginated listing.
fn release_identity_listing(
    slug: &str,
    wanted: &std::collections::BTreeSet<u64>,
) -> Result<std::collections::BTreeMap<u64, publish::ReleaseObjectIdentity>> {
    let listing = publish::release_listing(slug)?;
    let text = publish::release_identity_rows(&listing);
    let captured = captured_identity_rows(&text, wanted);
    let rows = publish::parse_release_object_identity_rows(&captured)?;
    Ok(rows.into_iter().map(|row| (row.id, row)).collect())
}

/// The `wanted` releases' rows out of one page of the channel's identity listing, selected
/// before the strict parser sees them: [`publish::parse_release_object_identity_rows`]
/// fails closed on an empty tag or target, and a tagless draft made in the GitHub web UI
/// (which [`scan_release_page`] skips at its `release.draft` guard) must not fail a
/// read-only command.
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

/// The canonical version a candidate tag names: `"v0.2.0"` → `"0.2.0"`. The grammar is
/// `aterm_update_core::tag::parse_release_tag`, shared with the client so the two rules
/// cannot drift: canonical `vMAJOR.MINOR.PATCH` is a candidate, a retired two-component
/// `vMAJOR.MINOR` is [`TagKind::Legacy`] (skipped, never an error), anything else fails
/// closed.
fn parse_canonical_tag(tag: &str) -> Result<String> {
    publish::canonical_channel_tag_version(tag)
}

#[derive(Debug)]
struct ReleaseMetadata<'a> {
    release_id: Option<u64>,
    tag: &'a str,
    draft: bool,
    /// A prerelease is published but never the channel head: GitHub's `latest` cannot
    /// name one and the deployed client skips it, so neither replay counts it.
    prerelease: bool,
    exact_count: usize,
}

fn parse_release_metadata(listing: &str) -> Result<Vec<ReleaseMetadata<'_>>> {
    listing
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let fields: Vec<&str> = line.split('\t').collect();
            let parse_id = |id: &str| {
                id.parse::<u64>().map_err(|_| {
                    Error::new(format!(
                        "malformed release metadata row {}: invalid release ID {id:?}",
                        index + 1
                    ))
                })
            };
            let (release_id, tag, draft, prerelease, exact_count) = match fields.as_slice() {
                [tag, draft, exact] => (None, *tag, *draft, "false", *exact),
                [id, tag, draft, exact] => (Some(parse_id(id)?), *tag, *draft, "false", *exact),
                [id, tag, draft, prerelease, exact] => {
                    (Some(parse_id(id)?), *tag, *draft, *prerelease, *exact)
                }
                _ => {
                    return Err(Error::new(format!(
                        "malformed release metadata row {}: expected three or four fixture \
                         fields or five production fields",
                        index + 1
                    )));
                }
            };
            let flag = |value: &str, field: &str| match value {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(Error::new(format!(
                    "malformed release metadata row {}: invalid {field} flag {value:?}",
                    index + 1
                ))),
            };
            let draft = flag(draft, "draft")?;
            let prerelease = flag(prerelease, "prerelease")?;
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
                prerelease,
                exact_count: parse_count(exact_count, "exact asset count")?,
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

/// Scans a complete release-metadata listing through an injected appcast fetch.
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
            if release.draft || release.prerelease || release.exact_count == 0 {
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
        if release.draft || release.prerelease {
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
        if release.exact_count == 0 {
            continue;
        }
        let asset = manifest_out::MANIFEST_ASSET.to_string();
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

/// Highest build among an already exhaustive scan.
pub fn select_newest(scanned: &[Published]) -> Option<&Published> {
    let mut best: Option<&Published> = None;
    for p in scanned {
        if best.is_none_or(|b| p.build > b.build) {
            best = Some(p);
        }
    }
    best
}

/// What the channel holds under one release tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseState {
    /// No release object under the tag.
    Absent,
    /// Only a draft (or drafts).
    Draft,
    /// A visible release that is not an app release a client installs from.
    Unpublished,
    /// The app release.
    Published,
}

/// Classify one tag from the channel's metadata listing ([`release_metadata_listing`]'s rows).
///
/// # Errors
/// A malformed row, or two published app releases under one tag.
pub fn classify_release_state(listing: &str, tag: &str) -> Result<ReleaseState> {
    let rows = parse_release_metadata(listing)?;
    let mut state = ReleaseState::Absent;
    for row in rows.iter().filter(|row| row.tag == tag) {
        let this = if row.draft {
            ReleaseState::Draft
        } else if !row.prerelease && row.exact_count == 1 {
            ReleaseState::Published
        } else {
            ReleaseState::Unpublished
        };
        if this == ReleaseState::Published && state == ReleaseState::Published {
            return Err(Error::new(format!(
                "two published app releases carry the tag {tag}; channel authority is ambiguous"
            )));
        }
        let rank = |state: ReleaseState| match state {
            ReleaseState::Absent => 0,
            ReleaseState::Draft => 1,
            ReleaseState::Unpublished => 2,
            ReleaseState::Published => 3,
        };
        if rank(this) > rank(state) {
            state = this;
        }
    }
    Ok(state)
}

/// [`classify_release_state`] of `tag` on the channel `slug`, from one fresh listing.
pub fn release_state(slug: &str, tag: &str) -> Result<ReleaseState> {
    classify_release_state(&release_metadata_listing(slug)?, tag)
}

/// The three remote-derived facts [`derive_cut_mode`] reads, gathered by the caller (network) so the decision itself is pure and table-testable.
#[derive(Debug, Clone)]
pub struct RemoteState {
    /// The version this cut would publish.
    pub current_version: String,
    /// Does CHANGELOG.md already carry `## [current_version]`?.
    pub changelog_has_section: bool,
    /// Whether `v<current_version>` is a published app release on the channel ([`ReleaseState::Published`]).
    pub published: bool,
}

/// What kind of cut to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CutMode {
    /// Normal: roll the changelog into `## [version]` and claim.
    Fresh { version: String },
    /// The wedge signature (roll and claim landed on main, nothing published).
    Recut { version: String },
}

/// Derives the cut mode from remote-visible state.
pub fn derive_cut_mode(s: &RemoteState) -> Result<CutMode> {
    let pending = s.changelog_has_section && !s.published;
    let version = s.current_version.clone();
    if pending {
        return Ok(CutMode::Recut { version });
    }
    if s.published {
        return Err(Error::new(format!(
            "v{version} is already published, and it is the version the newest published \
             source declares — publish the next one first: `pub bump aterm --minor --write` \
             on main (the next release is v{}), commit and push, then `pub stage aterm` and \
             `pub publish aterm`, then cut; or retire a bad build with `{} yank <build> \
             --release-credentials <profile.toml> --mac-only|--linux-artifacts DIRECTORY`",
            publish::bump_minor_release(&version)?,
            publish::SHIP_COMMAND
        )));
    }
    Ok(CutMode::Fresh { version })
}

/// What the published head's signature must verify under, and against what.
pub struct PostPublishSignature<'a> {
    /// A journaled cut knows whether its ratchet required a signature.
    pub expected: Option<bool>,
    pub pubkey: Option<&'a str>,
    pub local_signature: Option<&'a Path>,
}

/// The full post-publish check of `version` on the channel `slug`.
pub fn post_publish(
    repo: &Path,
    slug: &str,
    version: &str,
    expect_build: Option<u64>,
    local_manifest: Option<&Path>,
    signature: PostPublishSignature<'_>,
) -> Result<()> {
    let tag = format!("v{version}");
    let scanned = scan_published(slug, true)?;
    let best = scanned.first().ok_or_else(|| {
        Error::new(format!(
            "no published release in {slug} carries {} — the fleet sees NOTHING; if a cut is \
             open, `{}` finishes it",
            manifest_out::MANIFEST_ASSET,
            publish::CUT_COMMAND
        ))
    })?;
    if best.tag != tag {
        return Err(Error::new(format!(
            "the client selection rule picks {} (build {}), NOT {tag} — the fleet \
             will not stage this version; a newer build is live or this one is \
             not published yet",
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

    let local_signature = match signature.local_signature {
        Some(path) if path.is_file() => Some(fs::read(path).map_err(|error| {
            Error::new(format!("read local signature {}: {error}", path.display()))
        })?),
        _ => None,
    };
    let signed = publish::verify_live_channel_head_signature(
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
        "Tier SIG exact signature verified"
    } else {
        "unsigned channel (no signing key journaled for this version)"
    };

    let manifest = Manifest::parse(&best.text)
        .map_err(|e| Error::new(format!("published manifest re-parse: {e}")))?;
    let commit = manifest.commit.as_deref().ok_or_else(|| {
        Error::new(format!(
            "published release {tag} manifest has no immutable claim commit"
        ))
    })?;
    publish::assert_remote_annotated_tag_commit(&ledger::GitCli::new(repo), &tag, commit)?;
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
    let zip_note = match (manifest.zip.as_deref(), manifest.zip_sha256.as_deref()) {
        (Some(zip), Some(sha256)) => {
            let verified = publish::verify_release_asset_digest_for_release_id(
                slug, release_id, &tag, zip, sha256,
            )?;
            format!("zip ok ({} bytes)", verified.size)
        }
        _ => "no zip container (pre-zip release; clients stage from the DMG)".to_string(),
    };
    let url = manifest.url.clone().unwrap_or_else(|| {
        format!(
            "https://github.com/{slug}/releases/download/{tag}/{}",
            manifest.dmg
        )
    });
    let code = head_status(&url)?;
    if code != "200" {
        return Err(Error::new(format!(
            "HEAD {url} returned {code}, not 200 — installed clients (and tools/install.sh) \
             cannot fetch the DMG from the channel"
        )));
    }

    match publish::probe_evergreen_pointer(
        slug,
        manifest_out::MANIFEST_ASSET,
        &aterm_update_core::pointer::canonical_app_tag,
    )? {
        publish::PointerProbe::Tag { tag: pointed, .. } if pointed == tag => {}
        publish::PointerProbe::Tag { tag: pointed, .. } => {
            return Err(Error::new(format!(
                "https://github.com/{slug}/releases/latest/download/{} names {pointed}, \
                 not {tag} — every credential-less install discovers the channel head from \
                 that pointer and would never see {tag}. The cut owns `latest`: `{cut}` while {tag}'s cut is open, which makes it the head behind the head \
                 ratchet; never re-point it by hand (docs/RELEASING.md, \"The cut owns \
                 `latest`\")",
                manifest_out::MANIFEST_ASSET,
                cut = publish::CUT_COMMAND
            )));
        }
        other => {
            return Err(Error::new(format!(
                "https://github.com/{slug}/releases/latest/download/{} does not resolve to a \
                 release this channel's clients would accept ({other:?}) — the updater would \
                 never see {tag}",
                manifest_out::MANIFEST_ASSET
            )));
        }
    }

    step(
        "verify",
        &format!(
            "live scan selects {tag} build {} · {byte_note} · {signature_note} · DMG ok \
             ({size} bytes) · HEAD 200 · {zip_note} · evergreen pointer → {tag}",
            best.build
        ),
    );
    step(
        "",
        &format!("PASS — https://github.com/{slug}/releases/tag/{tag}"),
    );
    Ok(())
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

/// `targo --unverified ship verify [vX.Y.Z]`.
pub fn run_verify(repo: &Path, version: Option<String>) -> Result<()> {
    publish::assert_origin_repo_binding(&ledger::GitCli::new(repo), &origin_slug_of(repo)?)?;
    let slug = publish::workspace_channel_slug(repo)?;
    println!("aterm-release · verify ({slug})");
    let version = match version {
        Some(v) => v,
        None => {
            let scanned = scan_published(&slug, true)?;
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
    let journal = publish::Journal::load_if_ours(&repo.join("dist/cut-state.toml"))?;
    let matching = journal
        .as_ref()
        .filter(|journal| journal.version == version);
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
        PostPublishSignature {
            expected: matching.map(|journal| journal.signature_required),
            pubkey,
            local_signature: Some(&local_signature),
        },
    )
}

/// `ship status`'s statement of what the next cut releases: the commit `pub publish`
/// recorded and the version it declares, not this checkout's, which a cut does not build.
fn next_cut_line(
    repo: &Path,
    published: Result<gates::PublishedSource>,
    checkout_version: &str,
) -> String {
    let source = match published {
        Ok(source) => source,
        Err(e) => {
            return format!(
                "next cut releases the commit `pub publish` records ({e}); this checkout \
                 declares v{checkout_version}"
            );
        }
    };
    let short = source.commit.get(..12).unwrap_or(&source.commit);
    let declared = ledger::git_ok(
        &ledger::GitCli::new(repo),
        &["show", &format!("{}:Cargo.toml", source.commit)],
    )
    .and_then(|out| publish::workspace_version(&out.stdout_utf8()))
    .and_then(|full| publish::release_version_from_workspace(&full));
    match declared {
        Ok(version) => format!(
            "next cut releases the published commit {short} (v{version}, verified {}), \
             whatever this checkout holds",
            source.verified_at
        ),
        Err(e) => format!("next cut releases the published commit {short} ({e})"),
    }
}

/// What a cut from this tree must be given to sign, read off the committed pins: the
/// paper master (the roster) and the Apple Team ID (notarization).
const fn signing_requirement() -> &'static str {
    let roster = aterm_update_core::pins::roster_tier_armed();
    let notary = !aterm_update_core::pins::APPLE_TEAM_ID.is_empty();
    match (roster, notary) {
        (true, true) => {
            "a cut needs --release-credentials <profile.toml>: a rostered machine key and a \
             notary credential"
        }
        (true, false) => {
            "a cut needs a rostered machine key (--release-credentials <profile.toml>, or \
             ~/.aterm/machine.key)"
        }
        (false, true) => {
            "a cut needs --release-credentials <profile.toml> with a notary credential; update \
             signing is optional (no paper master pinned)"
        }
        (false, false) => "update signing is optional (no paper master pinned)",
    }
}

/// `targo --unverified ship status`.
pub fn run_status(repo: &Path) -> Result<()> {
    let slug = publish::workspace_channel_slug(repo)?;
    println!("aterm-release · status ({slug})");
    step("signing", signing_requirement());

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
    let tail = ledger::tail(&ledger_text)?;
    step(
        "app",
        &next_cut_line(repo, gates::published_source(), &release_version),
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

    let journal_path = repo.join("dist/cut-state.toml");
    if let Some(j) = publish::JournalHeader::read(&journal_path)? {
        if j.finished() {
            step(
                "cut",
                &format!(
                    "last journaled cut v{} completed{}",
                    j.version,
                    if j.format == publish::JOURNAL_FORMAT {
                        String::new()
                    } else {
                        format!(
                            " (a format-{} journal: history, which the next cut clears)",
                            j.format
                        )
                    }
                ),
            );
        } else {
            step(
                "cut",
                &format!(
                    "IN PROGRESS: v{} (build {}) {} — `{}`",
                    j.version,
                    j.build_number,
                    j.position(),
                    publish::CUT_COMMAND
                ),
            );
        }
    }

    let scanned = scan_published(&slug, false)?;
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

/// Pure successor-first yank decision.
pub fn yank_successor_covers(bad: &Published, successor: &Published) -> Result<bool> {
    validate_published_identity(bad)?;
    validate_published_identity(successor)?;
    let required_floor = yank_required_floor(bad.build)?;
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
    let journal = publish::Journal::load_if_ours(&repo.join("dist/cut-state.toml"))?;
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
    let scanned = scan_published(slug, true)?;
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
        PostPublishSignature {
            expected: None,
            pubkey: pubkey.as_deref(),
            local_signature: None,
        },
    )?;
    Ok(Some(successor.clone()))
}

/// Re-proves the bad release inert before every cleanup mutation: a covering successor
/// that passes the full post-publish replay ([`post_publish`]).
fn prove_yank_successor(repo: &Path, slug: &str, bad: &Published) -> Result<Option<Published>> {
    verified_channel_successor(repo, slug, |successor| {
        yank_successor_covers(bad, successor)
    })
}

/// The ratcheted successor once the bad build has left the published set: the channel head
/// is newer than `build`, fully verified, and poisons it via `min_build`.
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

/// A completed yank is not converged while a killed cleanup publisher still owns the
/// global release refs: success then would leave a later cut wedged, so a response-lost
/// final unlock goes to the ordinary explicit stopped-publisher recovery.
fn ensure_yank_cleanup_session_absent(git: &ledger::GitCli, successor: &Published) -> Result<()> {
    let expected_owner = published_commit(successor)?;
    let owner = publish::release_lease_owner(git)?;
    let fence = publish::publisher_fence(git)?;
    match (owner.as_deref(), fence.as_ref()) {
        (None, None) => Ok(()),
        (Some(observed), None) if observed == expected_owner => Err(Error::new(format!(
            "yank cleanup is remotely converged but its release lease is still owned by the \
             verified successor claim {expected_owner}; after proving the old publisher is \
             stopped, run `{} recover v{} {expected_owner} \
             --old-publisher-stopped`, then rerun yank",
            publish::SHIP_COMMAND,
            successor.version
        ))),
        (Some(observed), Some(current))
            if observed == expected_owner && current.owner == expected_owner =>
        {
            Err(Error::new(format!(
                "yank cleanup is remotely converged but its publisher session is still active \
                 for verified successor claim {expected_owner}; after proving the old publisher \
                 is stopped, run `{} recover v{} {expected_owner} \
                 --old-publisher-stopped`, then rerun yank",
                publish::SHIP_COMMAND,
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
         After proving this publisher is stopped, run `{} recover v{} {owner} \
         --old-publisher-stopped`, then rerun yank",
        publish::SHIP_COMMAND,
        successor.version
    ))
}

/// Re-read the exact bad manifest before the demotion: a build-number match is not
/// identity, so tag, release, version and manifest bytes must all still match, and
/// duplicates fail closed.
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
    let scanned = scan_published(slug, false)?;
    let matches: Vec<&Published> = scanned
        .iter()
        .filter(|published| published.build == expected.build)
        .collect();
    let observed = match matches.as_slice() {
        [] => return Ok(false),
        [observed] => *observed,
        _ => {
            return Err(Error::new(format!(
                "yank target build {} has {} published identities; refusing an ambiguous \
                 demotion",
                expected.build,
                matches.len()
            )));
        }
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

/// Takes the yanked release out of the published set: demoted to a prerelease by its
/// immutable ID, with the successor re-proved and the exact owner+process token re-asserted
/// before every attempt.
fn demote_yank_release_convergently(
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
    let endpoint = format!("repos/{slug}/releases/{}", expected.id);
    crate::github::send(
        aterm_update_core::github_rest::Method::Patch,
        &endpoint,
        Some(&aterm_json::json!({"prerelease": true})),
        || {
            let observed = publish::release_object_by_id(slug, expected.id)?;
            publish::validate_release_object_snapshot(observed.as_ref(), expected)?;
            prove_yank_successor(repo, slug, bad)?.ok_or_else(|| {
                Error::new("ratcheted successor proof disappeared before the demotion")
            })?;
            publish::assert_publisher_session(git, lease, fence)
        },
    )?;
    Ok(())
}

/// What `targo --unverified ship yank` must be told before it can publish anything.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct YankOptions {
    /// The one credentials profile, forwarded verbatim to the successor cut.
    pub release_credentials: Option<PathBuf>,
    /// The successor cut's Linux declaration, forwarded verbatim.
    pub linux_artifacts: Option<PathBuf>,
    pub linux_targets: Vec<String>,
    pub linux_workers: Vec<String>,
    pub mac_only: bool,
}

/// The successor cut a yank of a bad build must publish: the ratcheted floor
/// plus the operator's signing inputs and Linux declaration, and nothing else.
fn successor_cut_options(required_floor: u64, opts: &YankOptions) -> publish::CutOptions {
    publish::CutOptions {
        min_build: Some(required_floor),
        release_credentials: opts.release_credentials.clone(),
        linux_artifacts: opts.linux_artifacts.clone(),
        linux_targets: opts.linux_targets.clone(),
        linux_workers: opts.linux_workers.clone(),
        mac_only: opts.mac_only,
        ..Default::default()
    }
}

/// Yank's local gates, before anything remote: a clean checkout whose own cutter this is
/// (`identity`: [`gates::current_own_cutter_gate`] in production), bound to the origin its
/// Cargo.toml names.
fn yank_local_gates(
    git: &dyn ledger::GitRunner,
    slug: &str,
    identity: &dyn Fn(&dyn ledger::GitRunner) -> Result<()>,
) -> Result<()> {
    gates::clean_tree(git)?;
    identity(git)?;
    publish::assert_origin_repo_binding(git, slug)
}

/// `targo --unverified ship yank <build>`.
pub fn run_yank(repo: &Path, build: u64, opts: &YankOptions) -> Result<()> {
    let slug = publish::workspace_channel_slug(repo)?;
    println!("aterm-release · yank build {build} ({slug})");
    let git = ledger::GitCli::new(repo);
    yank_local_gates(
        &git,
        &origin_slug_of(repo)?,
        &gates::current_own_cutter_gate,
    )?;
    let scanned = scan_published(&slug, false)?;
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

    let journal_path = repo.join("dist/cut-state.toml");
    if let Some(j) = publish::JournalHeader::read(&journal_path)?
        && !j.finished()
    {
        return Err(Error::new(format!(
            "an unfinished cut is journaled: v{} (build {}) — finish it (`{cut}`) or \
             discard it (`{cut} --abandon v{}`) before yanking; nothing was deleted",
            j.version,
            j.build_number,
            j.version,
            cut = publish::CUT_COMMAND
        )));
    }

    if prove_yank_successor(repo, &slug, &bad)?.is_none() {
        step(
            "yank",
            &format!(
                "publishing successor FIRST with min_build = {required_floor}; bad release remains live until proof passes"
            ),
        );
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
    let cleanup_owner = published_commit(&successor)?;

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
        publish::assert_publisher_session(&git, &cleanup_lease, &cleanup_fence)
    })
    .map_err(|error| yank_cleanup_failure(error, &successor, &cleanup_owner))?;
    demote_yank_release_convergently(repo, &slug, &bad, &git, &cleanup_lease, &cleanup_fence)
        .map_err(|error| yank_cleanup_failure(error, &successor, &cleanup_owner))?;

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
            "DONE — bad build {build} is poisoned by min_build {required_floor}; its tag is gone \
             and its release demoted to a prerelease on {slug}"
        ),
    );
    Ok(())
}

/// `targo --unverified ship release --abandon vX.Y.Z`.
pub fn run_abandon(repo: &Path, version: &str) -> Result<()> {
    let slug = publish::workspace_channel_slug(repo)?;
    let tag = format!("v{version}");
    println!("aterm-release · abandon {tag} ({slug})");
    let journal_path = repo.join("dist/cut-state.toml");
    let header = publish::JournalHeader::read(&journal_path)?.ok_or_else(|| {
        Error::new(format!(
            "there is no matching v{version} journal proving an owner; refusing destructive \
             abandon. If another machine was lost, prove its publisher stopped, then use \
             `{} recover v{version} <full-claim-sha> --old-publisher-stopped`",
            publish::SHIP_COMMAND
        ))
    })?;
    if header.version != version {
        return Err(Error::new(format!(
            "local journal is v{}, not requested abandon v{version}",
            header.version
        )));
    }
    if header.finished() {
        return Err(Error::new(format!(
            "release journal v{version} is already complete; abandon has no unfinished-cut authority"
        )));
    }
    let git = ledger::GitCli::new(repo);
    publish::assert_origin_repo_binding(&git, &origin_slug_of(repo)?)?;
    gates::own_cutter_gate(&git, &header.commit, false)?;
    let journal = publish::Journal::load(&journal_path)?.ok_or_else(|| {
        Error::new(format!(
            "{} vanished while abandoning",
            journal_path.display()
        ))
    })?;
    publish::ordinary_resume_claim_preflight(repo, &git, &journal)?;

    if release_state(&slug, &tag)? == ReleaseState::Published {
        return Err(Error::new(format!(
            "{tag} is the channel's PUBLISHED app release — this cut's head PATCH landed, so \
             it is finished, not abandoned: `{}` re-sends only that PATCH, proves \
             what a stranger sees and completes the journal (a bad build is then retired \
             with `{} yank <build> --release-credentials <profile.toml> --mac-only|--linux-artifacts \
             DIRECTORY`)",
            publish::CUT_COMMAND,
            publish::SHIP_COMMAND
        )));
    }

    let owner = journal.commit.clone();
    let lease = publish::acquire_release_lease(&git, &owner)?;
    let fence = publish::acquire_publisher_fence(&git, &owner)?;
    let action = (|| -> Result<()> {
        let withdrawn = publish::withdraw_unpublished_release(
            repo,
            &slug,
            &tag,
            version,
            publish::WithdrawKnowledge::of(&journal),
            &lease,
            &fence,
        )?;
        step("abandon", &withdrawn);
        let released = publish::withdraw_tag_then_unlock(&git, &tag, &owner, &lease, &fence)?;
        if released == publish::LeaseRelease::AlreadySuperseded {
            return Err(Error::new(
                "abandon was fenced out before final unlock; journal retained for the winner",
            ));
        }
        fs::remove_file(&journal_path)
            .map_err(|e| Error::new(format!("delete {}: {e}", journal_path.display())))?;
        step("", "release lock freed; journal deleted");
        step(
            "",
            &format!(
                "the claim commit stays (append-only ledger; the burned number is normal). A \
                 later `{}` of v{version} recuts it with a fresh number.",
                publish::CUT_COMMAND
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

/// The origin "owner/repo" — `[workspace.package] repository`, where the ledger, the
/// lease, the fence and the tags live.
fn origin_slug_of(repo: &Path) -> Result<String> {
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

    /// Identity rows as [`publish::release_identity_rows`] writes them: id, tag, draft, target, tab-separated.
    const PAGE_WITH_A_TAGLESS_DRAFT: &str = "9\t\ttrue\tmain\n12\tv0.3.0\tfalse\tabc123\n";

    /// A head scan below the release the latest pointer names is torn and refuses before
    /// the claim; a channel with no pointer, or a pointer the client would not elect,
    /// binds nothing.
    #[test]
    fn a_head_scan_below_the_latest_pointer_refuses_before_the_claim() {
        let latest = "420813912\tv0.112.0\tfalse\tfalse\t1\n";
        assert!(head_reaches_latest(Some(latest), None).is_err());
        assert!(head_reaches_latest(Some(latest), Some("v0.111.0")).is_err());
        assert!(head_reaches_latest(Some(latest), Some("v0.112.0")).is_ok());
        assert!(head_reaches_latest(Some(latest), Some("v0.113.0")).is_ok());
        assert!(head_reaches_latest(None, None).is_ok());
        assert!(head_reaches_latest(Some("7\tv0.112.0\tfalse\tfalse\t0\n"), None).is_ok());
        assert!(head_reaches_latest(Some("7\tv0.61\tfalse\tfalse\t1\n"), None).is_ok());
        assert!(head_reaches_latest(Some(""), None).is_err());
    }

    /// Both replays skip a prerelease on the production five-field row; the same row
    /// published is the client replay's head.
    #[test]
    fn a_prerelease_carrying_an_appcast_is_never_the_head() {
        let appcast = |version: &str, build: u64| -> Result<Vec<u8>> {
            Ok(format!(
                "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\n\
                 dmg = \"aterm-{version}.dmg\"\nsha256 = \"{}\"\n",
                "ab".repeat(32)
            )
            .into_bytes())
        };
        let fetch = |_: Option<u64>, tag: &str, _: &str| match tag {
            "v0.91.0" => appcast("0.91.0", 91),
            "v0.92.0" => appcast("0.92.0", 92),
            other => panic!("unexpected fetch of {other}"),
        };
        let listing = |prerelease: bool| {
            format!("11\tv0.91.0\tfalse\tfalse\t1\n12\tv0.92.0\tfalse\t{prerelease}\t1\n")
        };
        for stop_early in [true, false] {
            let (_, heads) = scan_release_page(&listing(true), stop_early, fetch).unwrap();
            assert_eq!(
                heads.iter().map(|p| p.tag.as_str()).collect::<Vec<_>>(),
                ["v0.91.0"],
                "stop_early={stop_early}: the prerelease is skipped"
            );
        }
        let (_, heads) = scan_release_page(&listing(false), true, fetch).unwrap();
        assert_eq!(
            heads[0].tag, "v0.92.0",
            "published, the same row is the head"
        );
        let malformed = "12\tv0.92.0\tfalse\tmaybe\t1\n";
        scan_release_page(malformed, true, fetch).expect_err("a bad flag");
    }

    /// The strict identity parse of the batched recheck sees only the captured rows.
    #[test]
    fn an_unrelated_tagless_draft_cannot_abort_the_batched_recheck() {
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

    /// Narrowing never softens the parse: a captured identity is authority for a yank.
    #[test]
    fn a_captured_row_that_is_malformed_still_fails_closed() {
        let wanted = std::collections::BTreeSet::from([9, 12]);
        let selected = captured_identity_rows(PAGE_WITH_A_TAGLESS_DRAFT, &wanted);
        publish::parse_release_object_identity_rows(&selected)
            .expect_err("an empty tag on a CAPTURED release is still fatal");
    }

    /// The captured ID is then absent from the live map, so [`scan_published`] reports the
    /// scan as torn.
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

    /// With the paper master armed a cut refuses pre-claim unless told which profile signs,
    /// so the yank's answer must reach its successor, which stays a plain real cut; the
    /// yank's floor and Linux declaration reach it too.
    #[test]
    fn the_successor_cut_carries_the_operators_signing_inputs_and_stays_a_real_cut() {
        let cut = successor_cut_options(
            1_783_918_102,
            &YankOptions {
                release_credentials: Some(PathBuf::from("/keys/m3.toml")),
                ..Default::default()
            },
        );
        assert_eq!(cut.min_build, Some(1_783_918_102));
        assert_eq!(
            cut.release_credentials.as_deref(),
            Some(Path::new("/keys/m3.toml"))
        );
        assert!(!cut.dry_run && !cut.resume);

        assert_eq!(
            successor_cut_options(7, &YankOptions::default()),
            publish::CutOptions {
                min_build: Some(7),
                ..Default::default()
            }
        );
        let successor = successor_cut_options(
            7,
            &YankOptions {
                linux_artifacts: Some(PathBuf::from("/handoff")),
                ..Default::default()
            },
        );
        assert_eq!(
            successor.linux_artifacts.as_deref(),
            Some(Path::new("/handoff"))
        );
        let mac_only = YankOptions {
            mac_only: true,
            ..Default::default()
        };
        assert!(successor_cut_options(7, &mac_only).mac_only);
    }
}

#[cfg(test)]
mod yank_local_gate_tests {
    //! Yank runs from any clean checkout whose own cutter it is.

    use super::*;

    fn scratch_repo(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("aterm-yank-gates-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        ledger::testing::init(
            &root,
            &[
                ("user.name", "t"),
                ("user.email", "t@example.invalid"),
                ("commit.gpgsign", "false"),
            ],
        );
        for args in [
            &["commit", "-q", "--allow-empty", "-m", "one"][..],
            &["commit", "-q", "--allow-empty", "-m", "two"],
            &["remote", "add", "origin", "https://github.com/o/r.git"],
        ] {
            let out = ledger::testing::git()
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        }
        root
    }

    #[test]
    fn a_detached_checkout_may_yank_and_a_dirty_one_may_not() {
        let root = scratch_repo("detached");
        let git = ledger::testing::Git::new(&root);
        ledger::git_ok(&git, &["checkout", "-q", "--detach", "HEAD~1"]).unwrap();
        let branch = ledger::git_ok(&git, &["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap()
            .stdout_utf8();
        assert_eq!(branch.trim(), "HEAD");
        let ours = |_: &dyn ledger::GitRunner| -> Result<()> { Ok(()) };
        yank_local_gates(&git, "o/r", &ours).expect("no branch rule");

        fs::write(root.join("stray"), "x").unwrap();
        assert!(yank_local_gates(&git, "o/r", &ours).is_err());
        fs::remove_file(root.join("stray")).unwrap();
        let stale = |_: &dyn ledger::GitRunner| -> Result<()> { Err(Error::new("stale cutter")) };
        assert!(yank_local_gates(&git, "o/r", &stale).is_err());
        assert!(yank_local_gates(&git, "someone/else", &ours).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
