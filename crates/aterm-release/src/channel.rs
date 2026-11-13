// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release-channel policy and asset set validation ([`required_asset_names`],
//! [`validate_channel_asset_set`], [`provenance_asset_name`]).

use std::path::{Path, PathBuf};

use crate::ledger::{Error, Result};
use crate::manifest_out;
use crate::publish::{DurablePostDecision, durable_post_decision};

/// The workspace-manifest table holding release-channel policy.
pub const CHANNEL_TABLE: &str = "[workspace.metadata.aterm]";

/// The key inside [`CHANNEL_TABLE`] naming the release channel.
pub const CHANNEL_KEY: &str = "update_channel";

/// `OWNER/REPO` of the release channel.
pub fn update_channel_slug(cargo_toml: &str) -> Result<Option<String>> {
    let Some(raw) = table_string(cargo_toml, CHANNEL_TABLE, CHANNEL_KEY) else {
        return Ok(None);
    };
    validate_slug(&raw)?;
    Ok(Some(raw))
}

/// What the public channel's own source tree says its version is.
#[derive(Debug, PartialEq, Eq)]
pub enum ChannelVersion {
    /// The channel's `[workspace.package] version` is exactly the cut version.
    Agrees,
    /// The channel has no workspace manifest (an empty or source-less repo).
    NoManifest,
}

/// Refuse a cut whose version the public channel's source does not carry.
pub fn check_channel_version(
    cut_version: &str,
    channel_cargo_toml: &str,
) -> Result<ChannelVersion> {
    let trimmed = channel_cargo_toml.trim();
    if trimmed.is_empty() {
        return Ok(ChannelVersion::NoManifest);
    }
    let channel_version = crate::publish::workspace_version(channel_cargo_toml).map_err(|e| {
        Error::new(format!(
            "the public channel's Cargo.toml exists but its version is unreadable ({e}); \
             refusing to cut against a channel whose version cannot be established"
        ))
    })?;
    if channel_version == cut_version {
        return Ok(ChannelVersion::Agrees);
    }
    Err(Error::new(format!(
        "version disagreement: this cut is v{cut_version} but the public channel's source \
         tree carries {channel_version}. Publish the source FIRST, then cut:\n    \
         pub stage aterm && pub publish aterm\n\
         Cutting now would tag a public tree whose version is not the one in the binary \
         (that is exactly how v0.6.0's tag landed on 0.5.0 source)."
    )))
}

/// Read one `key = "value"` string out of one line-oriented `[table]`: only `key = "value"`
/// on its own line inside the header counts.
fn table_string(toml: &str, table: &str, key: &str) -> Option<String> {
    let mut in_table = false;
    for line in toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_table = line == table;
            continue;
        }
        if !in_table {
            continue;
        }
        let Some((name, rest)) = line.split_once('=') else {
            continue;
        };
        if name.trim() != key {
            continue;
        }
        let rest = rest.trim().strip_prefix('"')?;
        let (value, _) = rest.split_once('"')?;
        return Some(value.to_string());
    }
    None
}

/// Exactly two non-empty GitHub-name segments. Mirrors `aterm_update_core::is_valid_slug`,
/// which the client applies at runtime: a slug it would reject must never be published to.
fn validate_slug(slug: &str) -> Result<()> {
    let ok_segment = |segment: &str| {
        !segment.is_empty()
            && segment.len() <= 100
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    let mut parts = slug.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(Error::new(format!(
            "{CHANNEL_TABLE} {CHANNEL_KEY} = {slug:?} is not exactly one GitHub OWNER/REPO"
        )));
    };
    if !ok_segment(owner) || !ok_segment(repo) {
        return Err(Error::new(format!(
            "{CHANNEL_TABLE} {CHANNEL_KEY} = {slug:?} contains an invalid GitHub OWNER/REPO"
        )));
    }
    Ok(())
}

/// The canonical DMG asset name for a release version.
#[must_use]
pub fn dmg_asset_name(version: &str) -> String {
    format!("aterm-{version}.dmg")
}

/// The stable, version-independent twin of the DMG.
#[must_use]
pub fn stable_dmg_asset_name() -> String {
    "aterm.dmg".to_string()
}

/// The stable, version-independent twin of the updater zip.
#[must_use]
pub fn stable_zip_asset_name() -> String {
    "aterm-mac.zip".to_string()
}

/// The canonical updater-container (zip) asset name for a release version.
#[must_use]
pub fn zip_asset_name(version: &str) -> String {
    format!("aterm-{version}-mac.zip")
}

/// The `.sha256` sidecar name for a container asset.
#[must_use]
pub fn sha256_sidecar_name(asset: &str) -> String {
    format!("{asset}.sha256")
}

/// The `.sha256` sidecar's entire content, `<hash> <filename>` (two spaces, newline).
#[must_use]
pub fn sha256_sidecar_contents(sha256: &str, asset: &str) -> String {
    format!("{sha256}  {asset}\n")
}

/// The exact client asset set a release must carry, sorted.
#[must_use]
pub fn required_asset_names(version: &str, signed: bool, rostered: bool) -> Vec<String> {
    let dmg = dmg_asset_name(version);
    let zip = zip_asset_name(version);
    let stable_dmg = stable_dmg_asset_name();
    let stable_zip = stable_zip_asset_name();
    let dmg_sha256 = sha256_sidecar_name(&dmg);
    let zip_sha256 = sha256_sidecar_name(&zip);
    let stable_dmg_sha256 = sha256_sidecar_name(&stable_dmg);
    let stable_zip_sha256 = sha256_sidecar_name(&stable_zip);
    let mut names = vec![
        manifest_out::MANIFEST_ASSET.to_string(),
        dmg,
        zip,
        stable_dmg,
        stable_zip,
        dmg_sha256,
        zip_sha256,
        stable_dmg_sha256,
        stable_zip_sha256,
    ];
    if signed {
        names.push(manifest_out::MANIFEST_SIG_ASSET.to_string());
    }
    if rostered {
        names.push(aterm_update_core::roster::ROSTER_ASSET.to_string());
        names.push(aterm_update_core::roster::ROSTER_SIG_ASSET.to_string());
    }
    names.sort_unstable();
    names
}

/// The source publish's attestation pair, allowed beside the binary asset set.
pub const SOURCE_ATTESTATION_ASSETS: [&str; 2] = ["SHA256SUMS", "SHA256SUMS.sig"];

/// Why one asset of the source attestation pair does not stand on a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttestationFinding {
    /// Not listed: the source signing has not attached it yet.
    Missing(&'static str),
    /// Listed more than once.
    Duplicated(&'static str),
    /// Listed once, but GitHub's `state` is not `uploaded` (an upload cut short) or it holds no
    /// byte: nobody can download it, and the source signing cannot upload over it.
    Unfinished {
        name: &'static str,
        id: Option<u64>,
        state: String,
        size: u64,
    },
}

impl AttestationFinding {
    /// The finding as said of a release on `slug`. An unfinished asset names the `agh` DELETE
    /// that clears it, since the source signing cannot upload over it.
    pub(crate) fn words(&self, slug: &str) -> String {
        match self {
            Self::Missing(name) => {
                format!("{name} is missing — the signed source manifest `pub publish` attaches")
            }
            Self::Duplicated(name) => {
                format!("{name} is duplicated — the source manifest must be unambiguous")
            }
            Self::Unfinished {
                name,
                id,
                state,
                size,
            } => {
                let (asset, delete) = id.map_or_else(
                    || {
                        (
                            "with no id".to_string(),
                            format!("delete it on {slug}'s releases page"),
                        )
                    },
                    |id| {
                        let path = format!("repos/{slug}/releases/assets/{id}");
                        (
                            id.to_string(),
                            format!("delete it (agh api {path} -X DELETE --confirm {path})"),
                        )
                    },
                );
                format!(
                    "{name} (asset {asset}) is listed in state `{state}` with {size} bytes — an \
                     upload that never finished, which nobody can download; {delete} before the \
                     source signing attaches it again"
                )
            }
        }
    }
}

/// Each source attestation must occur exactly once on the release being published, as an upload
/// GitHub finished (`state` `uploaded`) that holds bytes. A name alone proves nothing: GitHub lists
/// an upload cut short under its name.
pub(crate) fn source_attestation_findings(release: &aterm_json::Value) -> Vec<AttestationFinding> {
    SOURCE_ATTESTATION_ASSETS
        .into_iter()
        .filter_map(|name| {
            let listed: Vec<&aterm_json::Value> = release
                .get("assets")
                .and_then(aterm_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter(|a| a.get("name").and_then(aterm_json::Value::as_str) == Some(name))
                .collect();
            match listed.as_slice() {
                [] => Some(AttestationFinding::Missing(name)),
                [asset] => {
                    let state = asset
                        .get("state")
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or("");
                    let size = asset
                        .get("size")
                        .and_then(aterm_json::Value::as_u64)
                        .unwrap_or(0);
                    (state != "uploaded" || size == 0).then(|| AttestationFinding::Unfinished {
                        name,
                        id: asset.get("id").and_then(aterm_json::Value::as_u64),
                        state: if state.is_empty() {
                            "<none>".to_string()
                        } else {
                            state.to_string()
                        },
                        size,
                    })
                }
                _ => Some(AttestationFinding::Duplicated(name)),
            }
        })
        .collect()
}

/// The findings of a release on `slug` as one line, each said once.
pub(crate) fn attestation_words(slug: &str, findings: &[AttestationFinding]) -> String {
    findings
        .iter()
        .map(|finding| finding.words(slug))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The provenance record's file name in `dist/` (`bundle::write_provenance`'s).
#[must_use]
pub fn provenance_asset_name(version: &str) -> String {
    format!("aterm-{version}-build.txt")
}

/// Prove a release's real remote asset listing is exactly the set this cut publishes.
pub fn validate_channel_asset_set(
    names: &[String],
    version: &str,
    signed: bool,
    rostered: bool,
    linux: &[String],
) -> Result<()> {
    let mut required = required_asset_names(version, signed, rostered);
    let canonical = linux_asset_names(version);
    for name in linux {
        if !canonical.contains(name) {
            return Err(Error::new(
                "noncanonical Linux asset requested for the channel",
            ));
        }
        if required.contains(name) {
            return Err(Error::new(
                "duplicate Linux asset requested for the channel",
            ));
        }
        required.push(name.clone());
    }
    required.sort_unstable();
    for src in SOURCE_ATTESTATION_ASSETS {
        if names.iter().filter(|n| n.as_str() == src).count() > 1 {
            return Err(Error::new(format!(
                "the release carries a duplicated source-attestation asset {src:?} — the \
                 client's unique_asset_index refuses duplicates"
            )));
        }
    }
    let mut observed: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| !SOURCE_ATTESTATION_ASSETS.contains(n))
        .collect();
    observed.sort_unstable();
    if observed.len() == required.len()
        && observed
            .iter()
            .zip(&required)
            .all(|(&obs, req)| obs == req.as_str())
    {
        return Ok(());
    }
    let missing: Vec<&String> = required
        .iter()
        .filter(|n| observed.binary_search(&n.as_str()).is_err())
        .collect();
    let extra: Vec<&&str> = observed
        .iter()
        .filter(|&&n| {
            required
                .binary_search_by(|req| req.as_str().cmp(n))
                .is_err()
        })
        .collect();
    let duplicated: Vec<&String> = required
        .iter()
        .filter(|n| observed.iter().filter(|&&o| o == n.as_str()).count() > 1)
        .collect();
    Err(Error::new(format!(
        "the release's asset set does not match what this cut publishes \
         (required exactly {required:?}): missing {missing:?}, unexpected {extra:?}, \
         duplicated {duplicated:?}"
    )))
}

/// How `publish` binds this cut's release on the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindPlan {
    /// No exact object and no durable intent.
    CreateDraft,
    /// A draft with our tag exists.
    ConvergeDraft,
    /// A visible release under our tag.
    ConvergeVisible,
    /// A create POST was durably issued but nothing is visible.
    AwaitVisibility,
}

/// Decide the bind from the two facts that matter.
#[must_use]
pub const fn bind_plan(release_intent: bool, observed_draft: Option<bool>) -> BindPlan {
    match durable_post_decision(release_intent, observed_draft.is_some()) {
        DurablePostDecision::PersistIntentThenPost => BindPlan::CreateDraft,
        DurablePostDecision::AwaitVisibility => BindPlan::AwaitVisibility,
        DurablePostDecision::ConvergeVisible => match observed_draft {
            Some(true) => BindPlan::ConvergeDraft,
            _ => BindPlan::ConvergeVisible,
        },
    }
}

/// Where an asset goes in the upload order onto the channel release.
#[must_use]
pub fn channel_upload_rank(name: &str) -> u8 {
    if name == manifest_out::MANIFEST_SIG_ASSET {
        1
    } else if name == manifest_out::MANIFEST_ASSET {
        2
    } else {
        0
    }
}

/// Whether this cut may adopt a visible release under its tag with no journal intent.
///
/// # Errors
/// The first app asset on it, completing "the release … ".
pub fn unclaimed_is_source_release(names: &[String]) -> std::result::Result<(), String> {
    match names.iter().find(|name| !is_source_shape(name)) {
        None => Ok(()),
        Some(app) => Err(format!(
            "carries {app}, which the engine's source release never does"
        )),
    }
}

/// Is the unclaimed source release this cut would adopt still a prerelease?
///
/// # Errors
/// A full release, completing "the release … ".
pub fn unclaimed_source_is_prerelease(prerelease: bool) -> std::result::Result<(), String> {
    if prerelease {
        Ok(())
    } else {
        Err(
            "is a FULL release, not the engine's source PRERELEASE — the engine that \
             created it did not read `BINARY_CUT_FOLLOWS_DEFAULT=\"true\"` (an older \
             engine, config drift, or a run from another commit), so it holds or can take \
             `latest` with no appcast on it"
                .to_string(),
        )
    }
}

/// The names the engine's source release carries.
#[must_use]
pub fn is_source_shape(name: &str) -> bool {
    SOURCE_ATTESTATION_ASSETS.contains(&name)
        || name == aterm_update_core::roster::ROSTER_ASSET
        || name == aterm_update_core::roster::ROSTER_SIG_ASSET
}

/// Every name a cut of `version` can publish, whatever its signing, roster or Linux shape.
#[must_use]
pub fn publishable_asset_names(version: &str) -> Vec<String> {
    let mut names = required_asset_names(version, true, true);
    names.push(crate::service_qualification::REPORT_ASSET.to_string());
    names.extend(linux_asset_names(version));
    names.sort_unstable();
    names
}

/// The Linux assets a cut of `version` can publish: each architecture's executable and
/// its static bootstrap verifier (`aterm_bootstrap_verify::pin::asset_name`).
#[must_use]
pub fn linux_asset_names(version: &str) -> Vec<String> {
    let mut names = Vec::new();
    for target in [
        aterm_update_core::linux::LinuxTarget::X86_64,
        aterm_update_core::linux::LinuxTarget::Aarch64,
    ] {
        names.push(target.asset_name(version));
        names.push(aterm_bootstrap_verify::pin::asset_name(
            version,
            target.arch(),
        ));
    }
    names
}

/// Whether a release holding `names` holds only what a cut of `version` publishes.
#[must_use]
pub fn binds_to_version(names: &[String], version: &str) -> bool {
    let publishable = publishable_asset_names(version);
    names
        .iter()
        .all(|name| publishable.binary_search(name).is_ok())
}

/// Which of `names` on an adopted source release a withdrawal removes.
///
/// # Errors
/// The foreign name, completing "the source release … ".
pub fn withdrawable_assets(
    names: &[String],
    version: &str,
    uploaded: Option<&[String]>,
) -> std::result::Result<Vec<String>, String> {
    let app = names.iter().filter(|name| !is_source_shape(name));
    match uploaded {
        Some(uploaded) => Ok(app
            .filter(|name| uploaded.contains(name))
            .cloned()
            .collect()),
        None => {
            let publishable = publishable_asset_names(version);
            let app: Vec<String> = app.cloned().collect();
            if let Some(foreign) = app
                .iter()
                .find(|name| publishable.binary_search(name).is_err())
            {
                return Err(format!(
                    "carries {foreign}, which no cut of v{version} publishes"
                ));
            }
            Ok(app)
        }
    }
}

/// One field of the head PATCH, typed as its JSON body carries it: a boolean, or a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadPatchValue {
    Bool(bool),
    Str(&'static str),
}

/// The head PATCH body, `PATCH /repos/{channel}/releases/{id}`.
pub const HEAD_PATCH: [(&str, HeadPatchValue); 3] = [
    ("draft", HeadPatchValue::Bool(false)),
    ("prerelease", HeadPatchValue::Bool(false)),
    ("make_latest", HeadPatchValue::Str("true")),
];

/// The JSON body that sends [`HEAD_PATCH`].
#[must_use]
pub fn head_patch_body() -> aterm_json::Value {
    let mut body = aterm_json::Map::new();
    for (name, value) in HEAD_PATCH {
        let value = match value {
            HeadPatchValue::Bool(value) => aterm_json::Value::Bool(value),
            HeadPatchValue::Str(value) => aterm_json::Value::String(value.to_string()),
        };
        body.insert(name.to_string(), value);
    }
    aterm_json::Value::Object(body)
}

/// Both halves of putting one cut onto its channel release.
pub trait ChannelRelease {
    /// Has this cut's head PATCH already landed? Asked first, read-only: the bound release
    /// is the channel's published app release (not a draft, not a prerelease, carrying its
    /// appcast: the state only the head PATCH produces, which `--abandon` refuses).
    fn head_made(&mut self) -> Result<bool>;
    /// The local half: `dist/` re-proved against the signed appcast (and the roster pair
    /// against the master signature), and the path of every asset this cut publishes.
    fn artifacts(&mut self) -> Result<Vec<PathBuf>>;
    /// Converge one asset onto the release under a durable one-shot upload intent,
    /// re-downloaded and proved byte-identical to the local file.
    fn upload(&mut self, file: &Path) -> Result<()>;
    /// Upload independent payloads, joining every started upload before returning.
    fn upload_payloads(&mut self, files: &[PathBuf]) -> Result<()>;
    /// From a fresh listing: the release carries exactly the asset set the deployed
    /// updater elects, each byte-identical to this cut's artifact.
    fn prove_assets(&mut self) -> Result<()>;
    /// The head half of the floors alone, from what the channel serves now: the channel
    /// head (the release the evergreen pointer names) is this cut or older.
    fn ratchet_head(&mut self) -> Result<()>;
    /// Every floor: [`Self::ratchet_head`], then the fleet's — the carried apply floor,
    /// the monotonic build, and a machine-roster generation this cut's roster covers.
    fn ratchet(&mut self) -> Result<()>;
    /// Bind the release object this cut publishes onto — the engine's source release,
    /// adopted, or a draft created under a durable one-shot intent — and record it in
    /// the journal.
    fn bind(&mut self) -> Result<()>;
    /// The one PATCH that makes the release the channel head: [`HEAD_PATCH`], guarded by
    /// the release capability and lease.
    fn make_head(&mut self) -> Result<()>;
    /// What a credential-less client now sees: the release elected by the client's
    /// replay, the evergreen pointer naming it and serving its appcast, and every
    /// required asset downloading.
    fn prove_head(&mut self) -> Result<()>;
}

/// The channel publish sequence, one order for every path.
///
/// # Errors
/// The first failing call.
pub fn publish_staged_on_channel(release: &mut dyn ChannelRelease) -> Result<()> {
    if release.head_made()? {
        release.ratchet_head()?;
        release.make_head()?;
        return release.prove_head();
    }
    release.artifacts()?;
    release.ratchet()?;
    release.bind()?;
    release.prove_assets()?;
    release.ratchet()?;
    release.make_head()?;
    release.prove_head()
}

/// Populate a private draft. This path has no call that can publish a release.
pub fn stage_on_channel(release: &mut dyn ChannelRelease) -> Result<()> {
    if release.head_made()? {
        return Err(Error::new("a published release is never re-cut"));
    }
    let mut files = release.artifacts()?;
    files.sort_by_key(|file| {
        channel_upload_rank(
            file.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        )
    });
    release.ratchet()?;
    release.bind()?;
    let payloads = files.partition_point(|file| {
        channel_upload_rank(
            file.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default(),
        ) == 0
    });
    // Invariant: every named payload is confirmed before the signature, and the
    // signature is confirmed before the appcast that clients can elect.
    release.upload_payloads(&files[..payloads])?;
    for file in &files[payloads..] {
        release.upload(file)?;
    }
    // Each upload has proved its own bytes. The source engine may still be
    // attaching its attestation; the claimed cut checks the complete inventory
    // after both branches join, rather than racing that expected addition here.
    Ok(())
}

#[cfg(test)]
#[path = "channel_tests.rs"]
mod tests;
