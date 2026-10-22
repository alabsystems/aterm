// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release channel: policy and pure decisions for the `publish` pipeline step, which
//! puts each cut onto the one release object installed copies read.
//!
//! ## One release object per version
//!
//! The updater elects a non-draft release under a canonical `vMAJOR.MINOR.PATCH` tag
//! carrying exactly one `aterm-appcast.toml`, one `aterm-<version>.dmg` and one
//! `aterm-<version>-mac.zip`, both names derived from the tag and cross-checked against the
//! manifest (`aterm-update/src/github.rs`). Miss one and the release is live, plausible and
//! never elected: [`required_asset_names`] is that rule as data, and
//! [`validate_channel_asset_set`] holds the remote listing to it before the release is made
//! the head.
//!
//! Only the client set, the `.sha256` sidecars and declared native Linux executables go
//! public. The provenance record ([`provenance_asset_name`]) and the dSYM archive stay in
//! `dist/`: no client reads them, and the dSYM's DWARF names the build host's paths.

use std::path::{Path, PathBuf};

use crate::ledger::{Error, Result};
use crate::manifest_out;
use crate::publish::{DurablePostDecision, durable_post_decision};

/// The workspace-manifest table holding release-channel policy.
pub const CHANNEL_TABLE: &str = "[workspace.metadata.aterm]";

/// The key inside [`CHANNEL_TABLE`] naming the release channel.
pub const CHANNEL_KEY: &str = "update_channel";

/// `OWNER/REPO` of the release channel, from `[workspace.metadata.aterm] update_channel`
/// in the workspace manifest.
///
/// `Ok(None)` means the key is absent, which is legal: the cut then falls back to
/// `[workspace.package] repository` (`publish::channel_slug`). A key present but not a
/// clean `OWNER/REPO` is an error, never a fall-through, so it cannot drift.
pub fn update_channel_slug(cargo_toml: &str) -> Result<Option<String>> {
    let Some(raw) = table_string(cargo_toml, CHANNEL_TABLE, CHANNEL_KEY) else {
        return Ok(None);
    };
    validate_slug(&raw)?;
    Ok(Some(raw))
}

/// What the public channel's own source tree says its version is, relative to
/// the version being cut.
#[derive(Debug, PartialEq, Eq)]
pub enum ChannelVersion {
    /// The channel's `[workspace.package] version` is exactly the cut version.
    Agrees,
    /// The channel has no workspace manifest (an empty or source-less repo): nothing to
    /// disagree with, so the caller reports it and proceeds.
    NoManifest,
}

/// Refuse a cut whose version the public channel's source does not carry.
///
/// Source is published by `pub` (staging -> `alabsystems/<repo>` main and annotated tag)
/// and binaries by `targo --unverified ship cut`; this is where the two are reconciled, so
/// a tag never rests on source that cannot have produced the binary beside it. Promote
/// the source first, then cut. Pre-claim, so a mismatch burns no ledger number.
///
/// Equality, not `>=`: a channel ahead of the cut has source for a version with no binary,
/// the state a half-finished publish leaves.
pub fn check_channel_version(
    cut_version: &str,
    channel_cargo_toml: &str,
) -> Result<ChannelVersion> {
    let trimmed = channel_cargo_toml.trim();
    if trimmed.is_empty() {
        return Ok(ChannelVersion::NoManifest);
    }
    // A manifest that exists without a readable `[workspace.package] version` is a real
    // disagreement, not a skip.
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

/// The canonical DMG asset name for a release version. The deployed client derives it from
/// the tag and refuses a manifest whose `dmg` field disagrees, so the cut cannot vary it.
/// The bytes are the lean app, the same signed bundle the updater zip carries.
#[must_use]
pub fn dmg_asset_name(version: &str) -> String {
    format!("aterm-{version}.dmg")
}

/// The stable, version-independent twin of the DMG, so
/// `releases/latest/download/aterm.dmg` is a permanent download URL. No client elects it
/// (install.sh and the updater bind the manifest's version-bound `dmg`). Byte-identical to
/// the [`dmg_asset_name`] asset, with its sidecar restating the digest under this name.
#[must_use]
pub fn stable_dmg_asset_name() -> String {
    "aterm.dmg".to_string()
}

/// The stable, version-independent twin of the updater zip, for bookmarked and
/// printed links to `releases/latest/download/aterm-mac.zip`, the lightweight
/// app-only container (the app installs its toolchain itself on first launch).
/// The alab.systems homepage button is NOT this link: `ship site` points it at
/// the versioned DMG of the newest release that carries one.
/// Byte-identical to the [`zip_asset_name`] asset of the same cut, exactly as
/// the DMG twin is to its canonical DMG, and like it elected by NO client —
/// the in-app updater stages from the manifest's version-bound `zip` field —
/// so it exists purely for the browser download lane.
#[must_use]
pub fn stable_zip_asset_name() -> String {
    "aterm-mac.zip".to_string()
}

/// The canonical updater-container (zip) asset name for a release version. Same contract as
/// [`dmg_asset_name`]: the client re-derives it from the tag and refuses a manifest whose
/// `zip` field disagrees.
#[must_use]
pub fn zip_asset_name(version: &str) -> String {
    format!("aterm-{version}-mac.zip")
}

/// The `.sha256` sidecar name for a container asset, the `<asset>.sha256` shape the Linux
/// tarball ships too, so one verification instruction covers every download.
#[must_use]
pub fn sha256_sidecar_name(asset: &str) -> String {
    format!("{asset}.sha256")
}

/// The `.sha256` sidecar's entire content, `<hash>  <filename>` (two spaces, newline): the
/// record `shasum -a 256 -c` accepts. The digest is the in-process one from packaging
/// (dmg.rs), so the sidecar never states anything the manifest does not.
#[must_use]
pub fn sha256_sidecar_contents(sha256: &str, asset: &str) -> String {
    format!("{sha256}  {asset}\n")
}

/// The exact client asset set a release must carry, sorted: the appcast, the version-bound
/// DMG and updater zip, the stable `aterm.dmg` and `aterm-mac.zip` download twins, a
/// `.sha256` sidecar for each of those, and, when `signed`, the appcast's detached
/// signature.
///
/// The zip is unconditional because every manifest this cutter emits names one,
/// and a manifest naming an asset the channel does not carry is exactly the
/// live-but-unelectable state this module exists to prevent. The stable twins
/// are unconditional for the inverse reason: every bookmarked or printed
/// `releases/latest/download/aterm-mac.zip` or `.../aterm.dmg` link 404s on any
/// release that drops them. (The homepage button links the versioned DMG, which
/// `ship site` picks.) The sidecars are unconditional for the humans, not
/// the updater: the containers are the manual downloads, their digests
/// otherwise live only inside the appcast TOML nobody opens, and a download
/// nobody can check is a funnel that trains people not to check. The TWIN
/// sidecars restate the same digests under the ALIAS filenames, because
/// `shasum -a 256 -c` matches on the embedded name — a versioned sidecar can
/// never verify the file a button-click actually saves.
///
/// A head still carrying a retired DMG name (`-x86_64`, `-lite`, `aterm-offline.dmg`) is
/// refused as an unexpected object.
#[must_use]
pub fn required_asset_names(version: &str, signed: bool, rostered: bool) -> Vec<String> {
    let mut names = vec![
        manifest_out::MANIFEST_ASSET.to_string(),
        dmg_asset_name(version),
        zip_asset_name(version),
        stable_dmg_asset_name(),
        stable_zip_asset_name(),
        sha256_sidecar_name(&dmg_asset_name(version)),
        sha256_sidecar_name(&zip_asset_name(version)),
        sha256_sidecar_name(&stable_dmg_asset_name()),
        sha256_sidecar_name(&stable_zip_asset_name()),
    ];
    if signed {
        names.push(manifest_out::MANIFEST_SIG_ASSET.to_string());
    }
    if rostered {
        names.push(aterm_update_core::roster::ROSTER_ASSET.to_string());
        names.push(aterm_update_core::roster::ROSTER_SIG_ASSET.to_string());
    }
    names.sort();
    names
}

/// The source publish's attestation pair, allowed beside the binary asset set: `pub
/// publish` runs before `ship cut`, so the channel's vX.Y.0 release already carries these.
/// They are not updater-elected, so they cannot change what a client resolves; each is
/// allowed at most once, and any other foreign name is refused.
pub const SOURCE_ATTESTATION_ASSETS: [&str; 2] = ["SHA256SUMS", "SHA256SUMS.sig"];

/// The provenance record's file name in `dist/` (`bundle::write_provenance`'s). Never
/// published (see the module doc).
#[must_use]
pub fn provenance_asset_name(version: &str) -> String {
    format!("aterm-{version}-build.txt")
}

/// Prove a release's real remote asset listing is exactly the set this cut publishes: no
/// missing name, no duplicate (the client's `unique_asset_index` refuses one), no extra
/// object. The set is the client set ([`required_asset_names`]) plus the native Linux
/// executables (`linux`, each canonical for `version`), beside at most one copy of each
/// source attestation asset. Run on a fresh listing before the release is made the head,
/// and again after.
pub fn validate_channel_asset_set(
    names: &[String],
    version: &str,
    signed: bool,
    rostered: bool,
    linux: &[String],
) -> Result<()> {
    let mut required = required_asset_names(version, signed, rostered);
    for name in linux {
        if !aterm_update_core::linux::LinuxTarget::X86_64
            .asset_name(version)
            .eq(name)
            && !aterm_update_core::linux::LinuxTarget::Aarch64
                .asset_name(version)
                .eq(name)
        {
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
    required.sort();
    // The source-attestation pair is allowed at most once each; what remains must match
    // the required set exactly.
    for src in SOURCE_ATTESTATION_ASSETS {
        if names.iter().filter(|n| n.as_str() == src).count() > 1 {
            return Err(Error::new(format!(
                "the release carries a duplicated source-attestation asset {src:?} — the \
                 client's unique_asset_index refuses duplicates"
            )));
        }
    }
    let mut observed: Vec<String> = names
        .iter()
        .filter(|n| !SOURCE_ATTESTATION_ASSETS.contains(&n.as_str()))
        .cloned()
        .collect();
    observed.sort();
    if observed == required {
        return Ok(());
    }
    let missing: Vec<&String> = required.iter().filter(|n| !observed.contains(n)).collect();
    let extra: Vec<&String> = observed.iter().filter(|n| !required.contains(n)).collect();
    let duplicated: Vec<&String> = required
        .iter()
        .filter(|n| observed.iter().filter(|o| o == n).count() > 1)
        .collect();
    Err(Error::new(format!(
        "the release's asset set does not match what this cut publishes \
         (required exactly {required:?}): missing {missing:?}, unexpected {extra:?}, \
         duplicated {duplicated:?}"
    )))
}

/// How `publish` binds this cut's release on the channel.
///
/// Every arm that is not provably ours refuses rather than mutating: the whole fleet reads
/// the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindPlan {
    /// No exact object and no durable intent: create the draft.
    CreateDraft,
    /// A draft with our tag exists — converge assets onto it, then make it the head.
    ConvergeDraft,
    /// A visible release under our tag: the engine's source release to adopt, or our
    /// own from a previous pass. Converge onto it, then make it the head.
    ConvergeVisible,
    /// A create POST was durably issued but nothing is visible yet. Never POST again: a
    /// duplicate draft is the ambiguity the one-shot protocol prevents.
    AwaitVisibility,
}

/// Decide the bind from the two facts that matter: whether this journal holds the
/// durable release intent, and what is visible now.
///
/// Reuses [`durable_post_decision`], the one-shot rule: a lost response can be discovered,
/// never re-POSTed.
#[must_use]
pub fn bind_plan(release_intent: bool, observed_draft: Option<bool>) -> BindPlan {
    match durable_post_decision(release_intent, observed_draft.is_some()) {
        DurablePostDecision::PersistIntentThenPost => BindPlan::CreateDraft,
        DurablePostDecision::AwaitVisibility => BindPlan::AwaitVisibility,
        DurablePostDecision::ConvergeVisible => match observed_draft {
            Some(true) => BindPlan::ConvergeDraft,
            // `ConvergeVisible` is only reachable with `Some(..)`.
            _ => BindPlan::ConvergeVisible,
        },
    }
}

/// Where an asset goes in the upload order onto the channel release: every other asset
/// first (rank 0), then the appcast's detached signature (1), then the appcast (2).
///
/// The appcast makes a release electable, so it lands after every byte it names, and its
/// signature before it, so no client reads an appcast whose signature is missing (a
/// pinned client refuses that head). On an adopted release this order is all that stands
/// between a client and a half-published head.
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

/// May this cut adopt a visible release under its tag that its journal holds no intent
/// for? Only as the engine's source release, carrying nothing but source shapes
/// ([`is_source_shape`]), which `pub publish` creates before every cut.
///
/// Anything else is another cut's release: every upload goes onto a release the journal
/// bound first under the durable intent, a lost journal's recovery reconstructs that
/// intent (or withdraws), and a recut claims a fresh build. Adopting it would publish
/// someone else's bytes as this cut.
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
/// The engine creates it as one (`BINARY_CUT_FOLLOWS_DEFAULT="true"` in the published
/// tree's `publish/config.sh`), and that alone hides the cut's uploads from every client
/// until its head PATCH: GitHub hands `latest` to a full release at creation, and the
/// client's replay skips a prerelease. On a full source release the appcast would be
/// electable the moment it lands, before the asset proof and the second floor read,
/// `--abandon` would refuse it (`verify::ReleaseState::Published`), and a resume would read
/// it as [`ChannelRelease::head_made`] and skip the proof. Refused before the intent is
/// persisted, so nothing is bound, uploaded or claimed.
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

/// The names the engine's source release carries: the attestation pair and the roster
/// pair it re-uploads. They never decide whose a release is, and a withdrawal never
/// takes them.
#[must_use]
pub fn is_source_shape(name: &str) -> bool {
    SOURCE_ATTESTATION_ASSETS.contains(&name)
        || name == aterm_update_core::roster::ROSTER_ASSET
        || name == aterm_update_core::roster::ROSTER_SIG_ASSET
}

/// Every name a cut of `version` can publish, whatever its signing, roster or Linux
/// shape: the widest reading of this version's release, for the one question asked
/// without a journal — is an object this version's?
#[must_use]
pub fn publishable_asset_names(version: &str) -> Vec<String> {
    let mut names = required_asset_names(version, true, true);
    for target in [
        aterm_update_core::linux::LinuxTarget::X86_64,
        aterm_update_core::linux::LinuxTarget::Aarch64,
    ] {
        names.push(target.asset_name(version));
    }
    names.sort();
    names
}

/// Does a release holding `names` hold nothing but what a cut of `version` publishes?
/// How a lost journal's recovery tells this claim's draft from someone else's: a
/// channel release carries no claim-commit target to bind by.
#[must_use]
pub fn binds_to_version(names: &[String], version: &str) -> bool {
    let publishable = publishable_asset_names(version);
    names.iter().all(|name| publishable.contains(name))
}

/// Which of `names` (the assets on the engine's source release this cut adopted) a
/// withdrawal takes, leaving the source release as the engine made it: with the journal,
/// exactly its uploads (`uploaded`); with a lost journal, every asset this version
/// publishes, refusing a name it does not publish. Source shapes are never taken.
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
            if let Some(foreign) = app.iter().find(|name| !publishable.contains(name)) {
                return Err(format!(
                    "carries {foreign}, which no cut of v{version} publishes"
                ));
            }
            Ok(app)
        }
    }
}

/// One field of the head PATCH, typed the way `gh api` sends it: `-F` for a JSON
/// boolean, `-f` for a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadPatchValue {
    Bool(bool),
    Str(&'static str),
}

/// The head PATCH body, `PATCH /repos/{channel}/releases/{id}`: the one request that makes
/// this cut's release the channel head ([`ChannelRelease::make_head`]). Every field is
/// load-bearing:
///
/// * `draft=false` publishes a draft this cut created;
/// * `prerelease=false` turns the engine's source prerelease into a full release (a
///   prerelease never holds `latest`, so the pointer gate would refuse the adopt path);
/// * `make_latest="true"` moves `latest` here (a string enum on GitHub's API, hence `-f`).
///
/// The live PATCH ([`head_patch_argv`]) and the fake GitHub in `tests/it/channel_latest.rs`
/// both read this table.
pub const HEAD_PATCH: [(&str, HeadPatchValue); 3] = [
    ("draft", HeadPatchValue::Bool(false)),
    ("prerelease", HeadPatchValue::Bool(false)),
    ("make_latest", HeadPatchValue::Str("true")),
];

/// The `gh` argv that sends [`HEAD_PATCH`] to `endpoint` (`repos/{slug}/releases/{id}`).
#[must_use]
pub fn head_patch_argv(endpoint: &str) -> Vec<String> {
    let mut argv: Vec<String> = ["api", "--method", "PATCH", endpoint]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
    for (name, value) in HEAD_PATCH {
        let (flag, value) = match value {
            HeadPatchValue::Bool(value) => ("-F", value.to_string()),
            HeadPatchValue::Str(value) => ("-f", value.to_string()),
        };
        argv.push(flag.to_string());
        argv.push(format!("{name}={value}"));
    }
    argv
}

/// Both halves of putting one cut onto its channel release (the local proof of `dist/` and
/// every GitHub call), so [`publish_on_channel`] fixes their order in one place and a fake
/// GitHub can prove it (`tests/it/channel_latest.rs`).
pub trait ChannelRelease {
    /// Has this cut's head PATCH already landed? Asked first, read-only: the bound release
    /// is the channel's published app release (not a draft, not a prerelease, carrying its
    /// appcast: the state only the head PATCH produces, which `--abandon` refuses). A yes
    /// has bound that release.
    fn head_made(&mut self) -> Result<bool>;
    /// The local half: `dist/` re-proved against the signed appcast (and the roster pair
    /// against the master signature), and the path of every asset this cut publishes.
    fn artifacts(&mut self) -> Result<Vec<PathBuf>>;
    /// Converge one asset onto the release under a durable one-shot upload intent,
    /// re-downloaded and proved byte-identical to the local file.
    fn upload(&mut self, file: &Path) -> Result<()>;
    /// From a fresh listing: the release carries exactly the asset set the deployed
    /// updater elects, each byte-identical to this cut's artifact.
    fn prove_assets(&mut self) -> Result<()>;
    /// The head half of the floors alone, from what the channel serves now: the channel
    /// head (the release the evergreen pointer names) is this cut or older. A cut never
    /// takes `latest` from a newer release.
    fn ratchet_head(&mut self) -> Result<()>;
    /// Every floor: [`Self::ratchet_head`], then the fleet's — the carried apply floor,
    /// the monotonic build, and a machine-roster generation this cut's roster covers. A
    /// cut never makes a head of a roster the fleet has moved past.
    fn ratchet(&mut self) -> Result<()>;
    /// Bind the release object this cut publishes onto — the engine's source release,
    /// adopted, or a draft created under a durable one-shot intent — and record it in
    /// the journal. Called only after the floors passed (see [`publish_on_channel`]).
    fn bind(&mut self) -> Result<()>;
    /// The one PATCH that makes the release the channel head: [`HEAD_PATCH`], guarded by
    /// the release capability and lease. Idempotent, and sent on every path.
    fn make_head(&mut self) -> Result<()>;
    /// What a credential-less client now sees: the release elected by the client's
    /// replay, the evergreen pointer naming it and serving its appcast, and every
    /// required asset downloading.
    fn prove_head(&mut self) -> Result<()>;
}

/// The channel publish sequence, one order for every path: prove `dist/`, check the
/// fleet's floors, bind the release, upload everything with the appcast pair last
/// ([`channel_upload_rank`]), prove the asset set, check the floors again, make the
/// release the head, prove what a stranger sees.
///
/// A head the cut made is finished, never refused ([`ChannelRelease::head_made`], the
/// predicate `--abandon` refuses on). The step can fail after its PATCH landed (the
/// pointer gate's five-minute bound); re-entering the write half then would let a roster
/// join since the PATCH make the floors refuse a release every client already runs, with
/// no exit, and an upload would put the older roster pair back over the join's. So the
/// resume runs only what cannot refuse a running head: the head half of the ratchet, the
/// idempotent PATCH (re-sent because GitHub hands `latest` to any full release published
/// in between, an `atpkg-index-<n>` included) and the read side.
///
/// The floors are read BEFORE THE BIND. A cut they refuse there has touched nothing
/// and recorded nothing: no draft created, no adoption intent persisted, no channel
/// release ID in the journal — so `--abandon` finds nothing on the channel to withdraw.
///
/// The PATCH is on every path because the cut owns GitHub's `latest` pointer: the
/// engine's source prerelease never holds it, so without the PATCH on the adopt path the
/// pointer would stay on the previous release and the gate would refuse every cut.
///
/// # Errors
/// The first failing call; nothing after it runs.
pub fn publish_on_channel(release: &mut dyn ChannelRelease) -> Result<()> {
    if release.head_made()? {
        release.ratchet_head()?;
        release.make_head()?;
        return release.prove_head();
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
    for file in &files {
        release.upload(file)?;
    }
    release.prove_assets()?;
    release.ratchet()?;
    release.make_head()?;
    release.prove_head()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_MANIFEST: &str = include_str!("../../../Cargo.toml");

    /// The source publish and the binary cut share one vX.Y.0 release, so the source
    /// attestation pair rides beside the elected set at most once each; any other
    /// foreign name, or a duplicate, is refused.
    #[test]
    fn the_source_attestation_pair_may_share_the_release_and_nothing_else_may() {
        let mut with_pair = required_asset_names("0.65.0", true, true);
        with_pair.push("SHA256SUMS".to_string());
        with_pair.push("SHA256SUMS.sig".to_string());
        validate_channel_asset_set(&with_pair, "0.65.0", true, true, &[])
            .expect("the pub pair shares the tag by design");

        let mut dup = with_pair.clone();
        dup.push("SHA256SUMS".to_string());
        validate_channel_asset_set(&dup, "0.65.0", true, true, &[])
            .expect_err("a duplicated attestation asset is still refused");

        let mut foreign = with_pair.clone();
        foreign.push("aterm-offline.dmg".to_string());
        validate_channel_asset_set(&foreign, "0.65.0", true, true, &[])
            .expect_err("a retired container beside the pair is still foreign");

        let only_pair: Vec<String> = SOURCE_ATTESTATION_ASSETS
            .iter()
            .map(|s| s.to_string())
            .collect();
        validate_channel_asset_set(&only_pair, "0.65.0", true, true, &[])
            .expect_err("the pair alone is a source release, not a published cut");
    }

    /// The channel's workspace version must EQUAL the cut's: behind (the observed
    /// `v0.6.0` tag on a `0.5.0` tree) and ahead both refuse; an empty body is the
    /// only skip, and a manifest with no version is unknown, never absent.
    #[test]
    fn the_channel_version_must_equal_the_cut() {
        let at = |v: &str| format!("[workspace.package]\nversion = \"{v}\"\nedition = \"2024\"\n");
        assert!(check_channel_version("0.6.0", &at("0.5.0")).is_err());
        assert!(check_channel_version("0.8.0", &at("0.9.0")).is_err());
        assert_eq!(
            check_channel_version("0.8.0", &at("0.8.0")).unwrap(),
            ChannelVersion::Agrees
        );
        assert_eq!(
            check_channel_version("0.8.0", "   \n\n").unwrap(),
            ChannelVersion::NoManifest
        );
        assert!(check_channel_version("0.8.0", "[workspace]\nmembers = []\n").is_err());
    }

    #[test]
    fn the_channel_is_exactly_the_one_clients_compiled_in() {
        // The cutter publishes to the channel every shipped binary compiles in
        // (`aterm_update_core::DEFAULT_OWNER`/`DEFAULT_REPO`); if they disagree, releases
        // go where no client reads.
        let slug = update_channel_slug(REAL_MANIFEST)
            .expect("workspace manifest parses")
            .expect("the workspace declares a public update channel");
        assert_eq!(
            slug,
            format!(
                "{}/{}",
                aterm_update_core::DEFAULT_OWNER,
                aterm_update_core::DEFAULT_REPO
            )
        );
    }

    #[test]
    fn absent_key_means_the_repository_but_a_present_bad_key_is_an_error() {
        assert_eq!(
            update_channel_slug("[workspace]\nmembers = []\n").unwrap(),
            None
        );
        // Right key, wrong table — must not be picked up.
        assert_eq!(
            update_channel_slug("[workspace.metadata.atpkg]\nupdate_channel = \"a/b\"\n").unwrap(),
            None
        );
        for bad in [
            "alabsystems",                          // no repo segment
            "alabsystems/aterm/extra",              // three segments
            "alabsystems/",                         // empty repo
            "/aterm",                               // empty owner
            "alab systems/aterm",                   // space
            "alabsystems/../aterm",                 // traversal shape
            "https://github.com/alabsystems/aterm", // a URL, not a slug
        ] {
            let doc = format!("[workspace.metadata.aterm]\nupdate_channel = \"{bad}\"\n");
            assert!(
                update_channel_slug(&doc).is_err(),
                "{bad:?} must be refused, never silently ignored"
            );
        }
    }

    #[test]
    fn channel_key_is_read_only_inside_its_own_table() {
        let doc = "\
[workspace.metadata.aterm]
update_channel = \"alabsystems/aterm\"  # trailing comment is ignored

[workspace.package]
update_channel = \"someone/else\"
";
        assert_eq!(
            update_channel_slug(doc).unwrap(),
            Some("alabsystems/aterm".to_string())
        );
    }

    #[test]
    fn required_asset_set_is_exactly_what_the_updater_elects() {
        // Unsigned (Tier REPO): appcast, version-bound DMG and zip, the two stable
        // download twins, and a `.sha256` sidecar for each (the twin sidecars embed the
        // alias names, so `shasum -c` accepts them against what a download saves).
        assert_eq!(
            required_asset_names("0.5.0", false, false),
            vec![
                "aterm-0.5.0-mac.zip".to_string(),
                "aterm-0.5.0-mac.zip.sha256".to_string(),
                "aterm-0.5.0.dmg".to_string(),
                "aterm-0.5.0.dmg.sha256".to_string(),
                "aterm-appcast.toml".to_string(),
                "aterm-mac.zip".to_string(),
                "aterm-mac.zip.sha256".to_string(),
                "aterm.dmg".to_string(),
                "aterm.dmg.sha256".to_string(),
            ]
        );
        // Signed (Tier SIG): a pinned client refuses a head with no .sig.
        assert_eq!(
            required_asset_names("0.5.0", true, false),
            vec![
                "aterm-0.5.0-mac.zip".to_string(),
                "aterm-0.5.0-mac.zip.sha256".to_string(),
                "aterm-0.5.0.dmg".to_string(),
                "aterm-0.5.0.dmg.sha256".to_string(),
                "aterm-appcast.toml".to_string(),
                "aterm-appcast.toml.sig".to_string(),
                "aterm-mac.zip".to_string(),
                "aterm-mac.zip.sha256".to_string(),
                "aterm.dmg".to_string(),
                "aterm.dmg.sha256".to_string(),
            ]
        );
    }

    /// The sidecar is only worth publishing if `shasum -a 256 -c` accepts it: hex digest,
    /// two spaces, exact asset name, one trailing newline.
    #[test]
    fn sidecar_contents_are_shasum_check_records() {
        let hash = "c6".repeat(32);
        assert_eq!(
            sha256_sidecar_contents(&hash, "aterm-0.5.0.dmg"),
            format!("{hash}  aterm-0.5.0.dmg\n")
        );
    }

    #[test]
    fn the_release_asset_set_must_match_the_client_rules_exactly() {
        let ok = vec![
            "aterm-appcast.toml".to_string(),
            "aterm-0.5.0.dmg".to_string(),
            "aterm-0.5.0.dmg.sha256".to_string(),
            "aterm-0.5.0-mac.zip".to_string(),
            "aterm-0.5.0-mac.zip.sha256".to_string(),
            "aterm.dmg".to_string(),
            "aterm.dmg.sha256".to_string(),
            "aterm-mac.zip".to_string(),
            "aterm-mac.zip.sha256".to_string(),
        ];
        validate_channel_asset_set(&ok, "0.5.0", false, false, &[]).unwrap();
        // Order is irrelevant — GitHub does not promise listing order.
        let reordered = vec![
            "aterm-0.5.0-mac.zip".to_string(),
            "aterm-mac.zip.sha256".to_string(),
            "aterm-0.5.0-mac.zip.sha256".to_string(),
            "aterm-0.5.0.dmg.sha256".to_string(),
            "aterm.dmg".to_string(),
            "aterm-mac.zip".to_string(),
            "aterm-0.5.0.dmg".to_string(),
            "aterm.dmg.sha256".to_string(),
            "aterm-appcast.toml".to_string(),
        ];
        validate_channel_asset_set(&reordered, "0.5.0", false, false, &[]).unwrap();

        // Every way a plausible-looking release silently never updates, each
        // refusal naming the asset it is about.
        let without =
            |name: &str| -> Vec<String> { ok.iter().filter(|n| *n != name).cloned().collect() };
        let with = |extra: &str| -> Vec<String> {
            let mut set = ok.clone();
            set.push(extra.to_string());
            set
        };
        let renamed = |from: &str, to: &str| -> Vec<String> {
            ok.iter()
                .map(|n| if n == from { to.to_string() } else { n.clone() })
                .collect()
        };
        let cases: Vec<(&str, Vec<String>, bool, &str)> = vec![
            (
                "no appcast",
                without("aterm-appcast.toml"),
                false,
                "aterm-appcast.toml",
            ),
            (
                "two appcasts",
                with("aterm-appcast.toml"),
                false,
                "aterm-appcast.toml",
            ),
            (
                "the DMG named for another version",
                renamed("aterm-0.5.0.dmg", "aterm-0.61.0.dmg"),
                false,
                "aterm-0.5.0.dmg",
            ),
            (
                "no evergreen twin",
                without("aterm.dmg"),
                false,
                "aterm.dmg",
            ),
            (
                "no sidecar",
                without("aterm-0.5.0.dmg.sha256"),
                false,
                "aterm-0.5.0.dmg.sha256",
            ),
            (
                "a signed cut without its signature",
                ok.clone(),
                true,
                "aterm-appcast.toml.sig",
            ),
            (
                "a private artifact",
                with("aterm-0.5.0-dSYM.zip"),
                false,
                "aterm-0.5.0-dSYM.zip",
            ),
        ];
        for (case, names, signed, needle) in cases {
            let err =
                validate_channel_asset_set(&names, "0.5.0", signed, false, &[]).expect_err(case);
            assert!(err.to_string().contains(needle), "{case}: {err}");
        }
    }

    #[test]
    fn bind_plan_never_reissues_a_durable_post() {
        assert_eq!(bind_plan(false, None), BindPlan::CreateDraft);
        assert_eq!(bind_plan(false, Some(true)), BindPlan::ConvergeDraft);
        assert_eq!(bind_plan(false, Some(false)), BindPlan::ConvergeVisible);
        // Intent issued and nothing visible is not a retry.
        assert_eq!(bind_plan(true, None), BindPlan::AwaitVisibility);
        assert_eq!(bind_plan(true, Some(true)), BindPlan::ConvergeDraft);
        assert_eq!(bind_plan(true, Some(false)), BindPlan::ConvergeVisible);
    }

    /// The provenance record and the dSYM archive are refused on the channel, and no
    /// reading of the version (not even a lost journal's widest) calls either publishable.
    #[test]
    fn the_debugging_aids_never_join_the_channel_set() {
        let aids = [
            provenance_asset_name("0.5.0"),
            "aterm-0.5.0-dSYM.zip".to_string(),
        ];
        let publishable = publishable_asset_names("0.5.0");
        for aid in &aids {
            assert!(!publishable.contains(aid), "{aid} is publishable");
            let mut set = required_asset_names("0.5.0", true, true);
            set.push(aid.clone());
            validate_channel_asset_set(&set, "0.5.0", true, true, &[])
                .expect_err("an aid on the channel is an unexpected object");
        }
    }

    /// A release is this version's when it holds nothing a cut of this version does not
    /// publish, and a withdrawal takes the journal's uploads (or, with a lost journal,
    /// every app asset of this version), never a source shape.
    #[test]
    fn a_withdrawal_takes_this_cuts_assets_and_never_the_source_shapes() {
        let v = "0.95.0";
        let source: Vec<String> = [
            "SHA256SUMS",
            "SHA256SUMS.sig",
            aterm_update_core::roster::ROSTER_ASSET,
            aterm_update_core::roster::ROSTER_SIG_ASSET,
        ]
        .iter()
        .map(|name| (*name).to_string())
        .collect();
        let dmg = dmg_asset_name(v);
        let zip = zip_asset_name(v);
        let mut adopted = source.clone();
        adopted.push(dmg.clone());
        adopted.push(zip.clone());

        // With the journal: exactly its uploads, minus source shapes (the source release
        // keeps a roster pair).
        let uploaded = vec![
            dmg.clone(),
            aterm_update_core::roster::ROSTER_ASSET.to_string(),
        ];
        assert_eq!(
            withdrawable_assets(&adopted, v, Some(&uploaded)).unwrap(),
            vec![dmg.clone()]
        );
        // With a lost journal: every app asset of this version.
        assert_eq!(
            withdrawable_assets(&adopted, v, None).unwrap(),
            vec![dmg.clone(), zip.clone()]
        );
        // …and a foreign name refuses instead of guessing.
        let mut foreign = adopted.clone();
        foreign.push("aterm-0.94.0.dmg".to_string());
        withdrawable_assets(&foreign, v, None).unwrap_err();
        // The source release alone has nothing to withdraw.
        assert!(withdrawable_assets(&source, v, None).unwrap().is_empty());

        assert!(binds_to_version(&[], v), "an empty draft is nobody else's");
        assert!(binds_to_version(&[dmg.clone(), zip], v));
        assert!(
            !binds_to_version(&[dmg.clone(), format!("aterm-{v}-dSYM.zip")], v),
            "a debugging aid is no cut's to publish"
        );
        assert!(!binds_to_version(&[dmg, "aterm-0.94.0.dmg".to_string()], v));
        assert!(!binds_to_version(&["SHA256SUMS".to_string()], v));
    }
}
